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
diagnostics, hover, go-to-definition and semantic highlighting. It reads
the workspace folder's `lilac.toml`, the project manifest `lilac build`
reads (`lilac init` writes one), so the editor checks the program the
CLI builds:

```toml
[project]
root = "src"

[build]
target = "host"

[libraries]
Standard = { bundled = true }
```

Its root, libraries, target, mode and `link.libc` apply to every check
(the main project's README lists every setting). Mistakes in it are
shown on the file. The server reads it from disk, so a change applies
when the file is saved. A file that is not TOML keeps the last settings
that were.

The `lilac.libraries` setting mounts libraries as `{ "Name": "dir" }`
(a relative `dir` is taken from the workspace folder); each replaces the
manifest's library of that name, as `--lib=` does on the command line.

`lilac.json` is no longer read: move its `root` and `libraries` into
`lilac.toml`.

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
