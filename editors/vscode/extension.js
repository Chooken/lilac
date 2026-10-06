const { workspace } = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let client;

function startClient(context, serverPath) {
    const next = new LanguageClient(
        "lilac",
        "Lilac",
        { command: serverPath, args: [], transport: TransportKind.stdio },
        { documentSelector: [{ language: "lilac" }] }
    );
    next.start();
    context.subscriptions.push(next);
    return next;
}

function serverPath() {
    return workspace.getConfiguration("lilac").get("serverPath", "lilac-lsp");
}

async function activate(context) {
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
