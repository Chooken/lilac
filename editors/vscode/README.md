# Lilac for Visual Studio Code

Syntax highlighting for the [Lilac](../../) programming language and Lilac IR.

## Languages

| Language | Extension | Scope |
|---|---|---|
| Lilac | `.lilac` | `source.lilac` |
| Lilac IR | `.lir` | `source.lilac-ir` |

Base highlighting is a TextMate grammar and works with any color theme; the
language server adds semantic highlighting on top.

## Language server and project settings

The extension starts `lilac-lsp` (the `lilac.serverPath` setting) for
diagnostics, hover, go-to-definition and semantic highlighting. Put a
`lilac.json` in the workspace folder to describe the project:

```json
{
  "root": "src",
  "libraries": { "Standard": "lib/Standard" }
}
```

- `root`: the project's root folder (default: the workspace folder).
- `libraries`: libraries to mount, as the CLI's `--lib=Name=dir`. A relative
  `dir` is taken from the workspace folder.

The `lilac.libraries` setting mounts libraries the same way and wins on a name
both define. Restart the server (reload the window) after changing either.

An open file the project does not reach is checked as the library namespace
it lies in, else as its folder (when the folder has a `main.lilac` or the file
has no `main`), else on its own.

## Install

From the repository root, build the extension and install the VSIX:

```sh
scripts/package-vscode.sh
code --install-extension package/lilac-0.5.0.vsix
```

`scripts/package.sh` builds `lilac-lsp` into `package/bin`; put that folder
on your `PATH` or point `lilac.serverPath` at the executable.

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
