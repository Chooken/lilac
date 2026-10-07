const {
    DocumentLink,
    EventEmitter,
    Hover,
    MarkdownString,
    Range,
    TextEditorRevealType,
    ThemeColor,
    Uri,
    ViewColumn,
    commands,
    languages,
    window,
    workspace,
} = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");
const { forward, byteToPosition, positionToByte, reverse } = require("./lirMap");

const LIR_SCHEME = "lilac-lir";
const HIGHLIGHT_MS = 2500;
const GENERATED_HOVER = "compiler-generated (no source)";
const OPEN_SOURCE_COMMAND = "lilac.openLirSource";
const LIR_STAGES = ["lower", "fold", "runtime", "final"];

let client;

const lirChange = new EventEmitter();
const sourceEmitters = new Map();
const openLirDocs = new Map();
const lirPayloads = new Map();

let sourceHighlightType;
let highlightedEditor;
let highlightTimer;

function sourceEmitter(sourceUri) {
    let emitter = sourceEmitters.get(sourceUri);
    if (!emitter) {
        emitter = new EventEmitter();
        emitter.event((uri) => lirChange.fire(uri));
        sourceEmitters.set(sourceUri, emitter);
    }
    return emitter;
}

function lirUriFor(sourceUri, stage) {
    return Uri.from({
        scheme: LIR_SCHEME,
        path: sourceUri.path,
        query: `stage=${stage}`,
    });
}

function lirStage(uri) {
    return new URLSearchParams(uri.query).get("stage") ?? "final";
}

function lirSource(uri) {
    const entry = openLirDocs.get(uri.toString());
    if (entry) {
        return entry.sourceUri;
    }
    return Uri.from({ scheme: "file", path: uri.path }).toString();
}

async function provideLirText(uri) {
    if (!client) {
        return "// IR unavailable";
    }
    try {
        const sourceUri = lirSource(uri);
        const stage = lirStage(uri);
        const payload = await client.sendRequest("lilac/lir", {
            uri: sourceUri,
            stage,
        });
        lirPayloads.set(uri.toString(), { sourceUri, stage, payload: payload ?? null });
        return payload ? payload.text : "// IR unavailable";
    } catch {
        lirPayloads.delete(uri.toString());
        return "// IR unavailable";
    }
}

async function openLirDocument(sourceUri, stage) {
    const sourceKey = sourceUri.toString();
    const uri = lirUriFor(sourceUri, stage);
    openLirDocs.set(uri.toString(), { sourceUri: sourceKey, uri });
    sourceEmitter(sourceKey);
    const document = await workspace.openTextDocument(uri);
    await window.showTextDocument(document, {
        viewColumn: ViewColumn.Beside,
        preview: false,
    });
    await languages.setTextDocumentLanguage(document, "lilac-ir");
}

async function showLir() {
    const editor = window.activeTextEditor;
    if (!editor || editor.document.languageId !== "lilac") {
        window.showWarningMessage("Open a Lilac source file to show its IR.");
        return;
    }
    try {
        await openLirDocument(editor.document.uri, "final");
    } catch {
    }
}

async function lirStageAvailable(sourceKey, stage) {
    if (!client) {
        return false;
    }
    try {
        const payload = await client.sendRequest("lilac/lir", {
            uri: sourceKey,
            stage,
        });
        return payload !== null && payload !== undefined;
    } catch {
        return false;
    }
}

async function selectLirStage() {
    const editor = window.activeTextEditor;
    if (!editor || editor.document.languageId !== "lilac") {
        window.showWarningMessage("Open a Lilac source file to select its IR stage.");
        return;
    }
    const sourceUri = editor.document.uri;
    const sourceKey = sourceUri.toString();
    let available;
    try {
        available = await Promise.all(
            LIR_STAGES.map((stage) => lirStageAvailable(sourceKey, stage))
        );
    } catch {
        return;
    }
    const items = LIR_STAGES.map((stage, index) => {
        const item = { label: stage, stage, available: available[index] };
        if (!available[index]) {
            item.description = "unavailable";
        }
        return item;
    });
    const picked = await window.showQuickPick(items, {
        placeHolder: "Select the IR stage to show",
    });
    if (!picked || !picked.available) {
        return;
    }
    try {
        await openLirDocument(sourceUri, picked.stage);
    } catch {
    }
}

function visibleEditorFor(uri) {
    const key = uri.toString();
    return window.visibleTextEditors.find(
        (editor) => editor.document.uri.toString() === key
    );
}

function restoreTopLine(editor, line) {
    try {
        if (editor.document.isClosed) {
            return;
        }
        const range = new Range(line, 0, line, 0);
        editor.revealRange(range, TextEditorRevealType.AtTop);
    } catch {
    }
}

function refreshLir() {
    for (const { sourceUri, uri } of openLirDocs.values()) {
        const emitter = sourceEmitters.get(sourceUri);
        if (!emitter) {
            continue;
        }
        const editor = visibleEditorFor(uri);
        const topLine = editor?.visibleRanges?.[0]?.start?.line;
        emitter.fire(uri);
        if (editor && typeof topLine === "number") {
            setTimeout(() => restoreTopLine(editor, topLine), 0);
        }
    }
}

function sourceDocumentFor(uriString) {
    return workspace.textDocuments.find(
        (document) => document.uri.toString() === uriString
    );
}

function sourceEditorFor(uriString) {
    return window.visibleTextEditors.find(
        (editor) => editor.document.uri.toString() === uriString
    );
}

function basenameOf(uriString) {
    try {
        const parts = Uri.parse(uriString).path.split("/");
        return parts[parts.length - 1] || uriString;
    } catch {
        return uriString;
    }
}

async function sourceDocument(uriString) {
    const open = sourceDocumentFor(uriString);
    if (open) {
        return open;
    }
    try {
        return await workspace.openTextDocument(Uri.parse(uriString));
    } catch {
        return undefined;
    }
}

function rangeForBytes(document, lo, hi) {
    const text = document.getText();
    const start = byteToPosition(text, lo);
    const end = byteToPosition(text, hi);
    return new Range(start.line, start.character, end.line, end.character);
}

function clearHighlight() {
    if (highlightTimer !== undefined) {
        clearTimeout(highlightTimer);
        highlightTimer = undefined;
    }
    if (highlightedEditor) {
        try {
            highlightedEditor.setDecorations(sourceHighlightType, []);
        } catch {
        }
        highlightedEditor = undefined;
    }
}

function highlightSource(uriString, range) {
    const editor = sourceEditorFor(uriString);
    if (!editor || !sourceHighlightType) {
        return;
    }
    clearHighlight();
    highlightedEditor = editor;
    try {
        editor.setDecorations(sourceHighlightType, [range]);
    } catch {
    }
    highlightTimer = setTimeout(clearHighlight, HIGHLIGHT_MS);
}

function generatedHover() {
    return new Hover(new MarkdownString(GENERATED_HOVER));
}

function lineRange(document, line) {
    try {
        return document.lineAt(line).range;
    } catch {
        return undefined;
    }
}

function registerHoverProvider(context) {
    const provider = {
        async provideHover(document, position) {
            try {
                const entry = lirPayloads.get(document.uri.toString());
                if (!entry || !entry.payload) {
                    return undefined;
                }
                const span = forward(entry.payload.spans || [], position.line);
                if (!span) {
                    return generatedHover();
                }
                const files = entry.payload.files || {};
                const fileUri = files[span.file];
                if (typeof fileUri !== "string") {
                    return generatedHover();
                }
                const source = await sourceDocument(fileUri);
                if (!source) {
                    return generatedHover();
                }
                const range = rangeForBytes(source, span.lo, span.hi);
                highlightSource(fileUri, range);
                const markdown = new MarkdownString();
                markdown.appendCodeblock(source.getText(range), "lilac");
                markdown.appendMarkdown(
                    `${basenameOf(fileUri)}:${range.start.line + 1}`
                );
                return new Hover(markdown, lineRange(document, position.line));
            } catch {
                return undefined;
            }
        },
    };
    context.subscriptions.push(
        languages.registerHoverProvider({ scheme: LIR_SCHEME }, provider)
    );
}

function cachedLirEntry(sourceKey) {
    let fallback;
    for (const entry of lirPayloads.values()) {
        if (entry.sourceUri !== sourceKey || !entry.payload) {
            continue;
        }
        if (entry.stage === "final") {
            return entry;
        }
        if (!fallback) {
            fallback = entry;
        }
    }
    return fallback;
}

async function lirPayloadForSource(sourceKey) {
    const cached = cachedLirEntry(sourceKey);
    if (cached) {
        return cached.payload;
    }
    if (!client) {
        return null;
    }
    const uri = lirUriFor(Uri.parse(sourceKey), "final");
    try {
        const payload = await client.sendRequest("lilac/lir", {
            uri: sourceKey,
            stage: "final",
        });
        if (payload) {
            lirPayloads.set(uri.toString(), {
                sourceUri: sourceKey,
                stage: "final",
                payload,
            });
            return payload;
        }
    } catch {
    }
    return null;
}

function fileIdFor(files, documentUri) {
    const target = documentUri.toString();
    let fallback;
    for (const [id, uri] of Object.entries(files)) {
        if (typeof uri !== "string") {
            continue;
        }
        if (uri === target) {
            return Number(id);
        }
        if (fallback === undefined && uri.toLowerCase() === target.toLowerCase()) {
            fallback = Number(id);
        }
    }
    return fallback;
}

function lirLinesFor(payload, lines) {
    const all = (payload.text || "").split("\n");
    return lines
        .filter((line) => line >= 0 && line < all.length)
        .map((line) => all[line]);
}

function registerSourceHoverProvider(context) {
    const provider = {
        async provideHover(document, position) {
            try {
                if (!document || document.languageId !== "lilac") {
                    return undefined;
                }
                const sourceKey = document.uri.toString();
                const payload = await lirPayloadForSource(sourceKey);
                if (!payload) {
                    return undefined;
                }
                const fileId = fileIdFor(payload.files || {}, document.uri);
                if (fileId === undefined) {
                    return undefined;
                }
                const byte = positionToByte(document.getText(), position);
                const lines = reverse(payload.spans || [], fileId, byte);
                if (lines.length === 0) {
                    return undefined;
                }
                const lirLines = lirLinesFor(payload, lines);
                if (lirLines.length === 0) {
                    return undefined;
                }
                const markdown = new MarkdownString();
                markdown.appendCodeblock(lirLines.join("\n"), "lilac");
                return new Hover(markdown, lineRange(document, position.line));
            } catch {
                return undefined;
            }
        },
    };
    context.subscriptions.push(
        languages.registerHoverProvider({ language: "lilac" }, provider)
    );
}

function linkCommand(fileUri, lo, hi) {
    return {
        command: OPEN_SOURCE_COMMAND,
        title: "Go to source",
        arguments: [fileUri, lo, hi],
    };
}

function registerLinkProvider(context) {
    const provider = {
        provideDocumentLinks(document) {
            const links = [];
            try {
                const entry = lirPayloads.get(document.uri.toString());
                if (!entry || !entry.payload) {
                    return links;
                }
                const files = entry.payload.files || {};
                for (const span of entry.payload.spans || []) {
                    const line = span[0];
                    const fileUri = files[span[1]];
                    if (typeof fileUri !== "string") {
                        continue;
                    }
                    if (line < 0 || line >= document.lineCount) {
                        continue;
                    }
                    try {
                        const link = new DocumentLink(
                            document.lineAt(line).range,
                            Uri.parse(fileUri)
                        );
                        link.command = linkCommand(fileUri, span[2], span[3]);
                        link.tooltip = `${basenameOf(fileUri)} (go to source)`;
                        links.push(link);
                    } catch {
                    }
                }
            } catch {
            }
            return links;
        },
        resolveDocumentLink(link) {
            return link;
        },
    };
    context.subscriptions.push(
        languages.registerDocumentLinkProvider({ scheme: LIR_SCHEME }, provider)
    );
}

async function openLirSource(uriString, lo, hi) {
    try {
        const document = await workspace.openTextDocument(Uri.parse(uriString));
        const range = rangeForBytes(document, lo, hi);
        const editor = await window.showTextDocument(document, { selection: range });
        editor.revealRange(range, TextEditorRevealType.InCenter);
    } catch {
    }
}

function registerLirProviders(context) {
    sourceHighlightType = window.createTextEditorDecorationType({
        backgroundColor: new ThemeColor("editor.findMatchHighlightBackground"),
        border: "1px solid",
        borderColor: new ThemeColor("editor.findMatchBorder"),
    });
    context.subscriptions.push(sourceHighlightType);
    registerHoverProvider(context);
    registerSourceHoverProvider(context);
    registerLinkProvider(context);
    context.subscriptions.push(
        commands.registerCommand(OPEN_SOURCE_COMMAND, openLirSource)
    );
    context.subscriptions.push(
        window.onDidChangeTextEditorSelection(() => clearHighlight())
    );
    context.subscriptions.push(
        window.onDidChangeActiveTextEditor(() => clearHighlight())
    );
}

function startClient(context, serverPath) {
    const next = new LanguageClient(
        "lilac",
        "Lilac",
        { command: serverPath, args: [], transport: TransportKind.stdio },
        { documentSelector: [{ language: "lilac" }] }
    );
    next.onNotification("lilac/lirInvalidated", refreshLir);
    next.start();
    context.subscriptions.push(next);
    return next;
}

function serverPath() {
    return workspace.getConfiguration("lilac").get("serverPath", "lilac-lsp");
}

async function activate(context) {
    context.subscriptions.push(
        workspace.registerTextDocumentContentProvider(LIR_SCHEME, {
            onDidChange: lirChange.event,
            provideTextDocumentContent: provideLirText,
        })
    );
    context.subscriptions.push(commands.registerCommand("lilac.showLir", showLir));
    context.subscriptions.push(
        commands.registerCommand("lilac.selectLirStage", selectLirStage)
    );
    context.subscriptions.push(lirChange);
    context.subscriptions.push(
        workspace.onDidCloseTextDocument((document) => {
            if (document.uri.scheme === LIR_SCHEME) {
                openLirDocs.delete(document.uri.toString());
                lirPayloads.delete(document.uri.toString());
            }
        })
    );

    registerLirProviders(context);

    client = startClient(context, serverPath());

    context.subscriptions.push(
        workspace.onDidChangeConfiguration(async (event) => {
            if (!event.affectsConfiguration("lilac.serverPath")) {
                return;
            }
            if (client) {
                await client.stop();
            }
            client = startClient(context, serverPath());
        })
    );
}

async function deactivate() {
    if (client) {
        await client.stop();
    }
}

module.exports = { activate, deactivate };
