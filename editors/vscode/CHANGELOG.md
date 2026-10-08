# Changelog

## 0.4.0

- Add the `lilac.libraries` setting and `lilac.json` `libraries` field: mount
  libraries (the CLI's `--lib=Name=dir`) for the language server.
- Library files are checked as their namespace, so opening `lib/Standard`
  files no longer reports every sibling type as unknown.
- Generic bodies nothing instantiates are highlighted too, and tokens are
  refreshed after each compile (they were missing until the first edit).
- Add semantic highlighting: map the language server's semantic token types to
  standard TextMate scopes so themes color functions, types, enums, fields,
  variants, variables, and parameters.

## 0.3.0

- Add a live LIR view: `lilac.showLir` opens the current project's LIR in a
  read-only `lilac-lir:` virtual document that refreshes on edit.
- Add stage selection: `lilac.selectLirStage` chooses which pipeline stage the
  live IR view renders, and the view keeps its scroll position across refreshes.
- Add provenance navigation: hover-highlight and click-jump between an IR line
  and its source, plus a peek of the emitted IR for a source expression.

## 0.2.0

- Add a language server client that launches `lilac-lsp` and shows compiler
  diagnostics.

## 0.1.0

- Initial release: syntax highlighting for Lilac (`.lilac`) and Lilac IR
  (`.lir`).
