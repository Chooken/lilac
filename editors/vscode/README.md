# Lilac for Visual Studio Code

Syntax highlighting for the [Lilac](../../) programming language and Lilac IR.

## Languages

| Language | Extension | Scope |
|---|---|---|
| Lilac | `.lilac` | `source.lilac` |
| Lilac IR | `.lir` | `source.lilac-ir` |

Highlighting is a pure TextMate grammar — no language server, no runtime code.
It works with any color theme.

## Install

Build the extension and install the VSIX:

```sh
cd editors/vscode
npm install
npm run package
code --install-extension lilac-0.1.0.vsix
```

Or, for development, open this folder in VS Code and press `F5` to launch an
Extension Development Host.

## Development

```sh
npm install     # dev dependencies
npm test        # run the TextMate grammar assertions
npm run package # build lilac-<version>.vsix
```

Grammar behavior is pinned by the `*.test` assertion files in `test/`; when the
language or IR grammar changes, update the grammar and its assertions together.
