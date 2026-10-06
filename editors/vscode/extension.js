const {
    EventEmitter,
    Uri,
    ViewColumn,
    commands,
    languages,
    window,
    workspace,
} = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

const LIR_SCHEME = "lilac-lir";

let client;

const lirChange = new EventEmitter();
const sourceEmitters = new Map();
const openLirDocs = new Map();

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
        const result = await client.sendRequest("lilac/lir", {
            uri: lirSource(uri),
            stage: lirStage(uri),
        });
        return result ? result.text : "// IR unavailable";
    } catch {
        return "// IR unavailable";
    }
}

async function showLir() {
    const editor = window.activeTextEditor;
    if (!editor || editor.document.languageId !== "lilac") {
        window.showWarningMessage("Open a Lilac source file to show its IR.");
        return;
    }
    const sourceUri = editor.document.uri;
    const sourceKey = sourceUri.toString();
    const uri = lirUriFor(sourceUri, "final");
    openLirDocs.set(uri.toString(), { sourceUri: sourceKey, uri });
    sourceEmitter(sourceKey);
    const document = await workspace.openTextDocument(uri);
    await window.showTextDocument(document, {
        viewColumn: ViewColumn.Beside,
        preview: false,
    });
    await languages.setTextDocumentLanguage(document, "lilac-ir");
}

function refreshLir() {
    for (const { sourceUri, uri } of openLirDocs.values()) {
        const emitter = sourceEmitters.get(sourceUri);
        if (emitter) {
            emitter.fire(uri);
        }
    }
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
    context.subscriptions.push(lirChange);
    context.subscriptions.push(
        workspace.onDidCloseTextDocument((document) => {
            if (document.uri.scheme === LIR_SCHEME) {
                openLirDocs.delete(document.uri.toString());
            }
        })
    );

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
