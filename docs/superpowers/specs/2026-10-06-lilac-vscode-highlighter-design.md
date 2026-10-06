# Lilac VSCode Syntax Highlighter

> Scope: a self-contained VSCode extension under `editors/vscode/` that
> syntax-highlights Lilac source (`.lilac`) and the authoritative Lilac IR
> (`.lir`, `lilac_ir = "0.3"`) using hand-written TextMate grammars, with
> grammar tests and `vsce` packaging. Branch `rust-rewrite-m0`. This is an
> editor-tooling artifact, not a compiler change; the cargo workspace is
> untouched. The language server is deliberately deferred to a separate spec.

## 1. Outcome / constraints / success

**Outcome**

- Installing the extension makes `.lilac` and `.lir` files render with correct
  TextMate scopes in any VSCode theme: keywords, builtin types, hooks,
  numbers, strings/chars/f-strings, comments, declaration names, IR sigils, and
  IR instructions/operators.
- `npm test` runs `vscode-tmgrammar-test` assertions against sample snippets
  for both languages.
- `npm run package` runs `vsce package` and produces a `.vsix`.

**Constraints**

- The extension lives entirely under `editors/vscode/`; it is **not** a cargo
  crate and is ignored by `members = ["crates/*"]`. No Rust workspace file
  changes.
- Highlighting is **declarative only**: TextMate grammars plus
  `language-configuration.json`. No extension-host runtime code, no `vscode`
  API dependency, no activation events (a pure grammar extension is always
  available and needs none).
- Scopes use **standard TextMate scope names** so behavior is theme-independent.
- No file under `crates/` is modified. Existing snapshots are unaffected.

**Success**

- Both grammars load (`vsce package` succeeds and the extension activates on
  opening a `.lilac`/`.lir` file).
- Every row of the scope tables in §4 and §6 is asserted by at least one
  `.test` assertion that passes.
- The grammar keyword/builtin lists exactly match `crates/lilac-lex` and
  `IR_SPEC.md` at the time of writing (see §8).

**Non-goals**

- The language server (diagnostics, hover, completion, go-to-definition).
- Semantic tokens / a TypeScript extension host.
- `.lbc` (binary) and the legacy Zig-era `.lilac-ir` fixture format.
- Snippets, file icons, themes, custom configuration settings.
- Auto-detecting new keywords at build time (grammars are hand-maintained).

### 1.1 Decisions locked during brainstorming

1. **Scope now is the highlighter only.** The LSP is a separate follow-up spec.
2. **File types:** `.lilac` (language id `lilac`) and `.lir` (language id
   `lilac-ir`). Legacy `.lilac-ir` and `.lbc` are excluded.
3. **Location:** `editors/vscode/` inside this repo, committed on
   `rust-rewrite-m0`.
4. **Tooling:** grammar tests via `vscode-tmgrammar-test` and packaging via
   `@vscode/vsce`, both driven from `editors/vscode/package.json`.
5. **Approach A:** hand-written TextMate JSON grammars (not YAML-compiled, not
   semantic tokens).

## 2. Extension structure

```
editors/vscode/
  package.json                      # manifest, npm scripts, devDependencies
  language-configuration.json       # shared by both languages
  syntaxes/
    lilac.tmLanguage.json           # scopeName source.lilac      (files *.lilac)
    lilac-ir.tmLanguage.json        # scopeName source.lilac-ir   (files *.lir)
  test/
    lilac.test                      # vscode-tmgrammar-test assertions
    lilac-ir.test
  .vscodeignore                     # drop test/, node_modules/, *.vsix
  .gitignore                        # node_modules/, *.vsix
  README.md
  CHANGELOG.md
```

`package.json` (key fields):

- `"name": "lilac"`, `"displayName": "Lilac"`, `"version": "0.1.0"`,
  `"publisher": "lilac-lang"` (placeholder), `"engines": { "vscode": "^1.75.0" }`,
  `"categories": ["Programming Languages"]`, `"main"` omitted (no runtime code).
- `"contributes.languages"`: two entries —
  `{ "id": "lilac", "extensions": [".lilac"], "configuration": "./language-configuration.json" }`
  and the `lilac-ir` / `.lir` analogue.
- `"contributes.grammars"`: two entries —
  `{ "language": "lilac", "scopeName": "source.lilac", "path": "./syntaxes/lilac.tmLanguage.json" }`
  and the `lilac-ir` analogue.
- `"scripts"`: `"test"` runs `vscode-tmgrammar-test` over `test/*.test`;
  `"package"` runs `vsce package`.
- `"devDependencies"`: `vscode-tmgrammar-test`, `@vscode/vsce`.

## 3. `.lilac` grammar

- `scopeName`: `source.lilac`; `fileTypes`: `["lilac"]`; `patterns` include
  `#comments`, `#decl-names`, `#keywords`, `#builtin-types`, `#hooks`,
  `#numbers`, `#strings`, `#fstrings`, `#chars`, `#operators`.
- Ordering matters: `#comments` first; `#decl-names` before `#keywords` so a
  declaration name is captured; `#builtin-types` before `#hooks` so `@bit32`
  is a type, not a generic hook; `#fstrings` before `#strings` so `$"…"` wins.
- The keyword and builtin lists are pinned by `crates/lilac-lex/src/lib.rs`
  (`lex_word`, `@`-name handling) and `SPEC.md` §1–§2.

## 4. `.lilac` scope mapping

| Construct | TextMate scope |
|---|---|
| `//…` (to end of line) | `comment.line.double-slash.lilac` |
| `object` `function` `enum` `extern` | `storage.type.lilac` |
| `using` | `keyword.control.import.lilac` |
| `private` | `storage.modifier.lilac` |
| `ref` `unknown` `nothing` `self` | `storage.type.lilac` |
| `if` `else` `loop` `match` `break` `return` | `keyword.control.lilac` |
| `and` `or` | `keyword.operator.lilac` |
| `@bit8` `@bit16` `@bit32` `@bit64` `@bit_native` `@byte_buffer` `@format_args` | `support.type.builtin.lilac` |
| any other `@name` (hook / intrinsic) | `support.function.builtin.lilac` |
| `0x…` | `constant.numeric.hex.lilac` |
| `0b…` | `constant.numeric.binary.lilac` |
| `10.5` | `constant.numeric.float.lilac` |
| `5` | `constant.numeric.integer.lilac` |
| `"…"` and its escapes `\" \\ \n \xHH` | `string.quoted.double.lilac` + `constant.character.escape.lilac` |
| `'…'` | `string.quoted.single.lilac` |
| `$"…"` (f-string) | `string.interpolated.lilac` |
| `{expr}` hole inside a f-string | `punctuation.section.interpolation.begin/end.lilac` + the expression patterns re-included |
| `{{` / `}}` f-string escapes | `constant.character.escape.lilac` |
| `:` `,` `=` `==` `=>` `->` `.` `+` `-` `*` `/` `%` `<` `>` `<=` `>=` `!` | `keyword.operator.lilac` |
| name before `function` (`name: function …`) | `entity.name.function.lilac` |
| name before `object` / `enum` (`name: object …`) | `entity.name.type.lilac` |
| `@name:` hook declaration name | `entity.name.function.lilac` |

## 5. `.lir` grammar

- `scopeName`: `source.lilac-ir`; `fileTypes`: `["lir"]`; `patterns` include
  `#comments`, `#header`, `#sigils`, `#keywords`, `#instructions`, `#operators`,
  `#numbers`, `#strings`.
- Ordering matters: `#comments` first; `#header` before `#keywords`; sigil
  patterns (`#sigils`) are matched on their leading `!`/`@`/`#`/`%`/`$`/`&`
  characters, so `#bit32` (type) is not confused with `#` comments (Lilac IR
  has no `#` comments).
- The instruction/operator/type lists are pinned by `IR_SPEC.md` and
  `crates/lilac-ir/src/lib.rs` (`print_inst`, `print_op`, `print_callee`).

## 6. `.lir` scope mapping

| Construct | TextMate scope |
|---|---|
| `//…` (incl. `// idx N` annotations) | `comment.line.double-slash.lilac-ir` |
| `lilac_ir` / `target` header keys | `keyword.other.lilac-ir` |
| header value string | `string.quoted.double.lilac-ir` |
| `!name` data const | `variable.other.constant.lilac-ir` |
| `@name` function (incl. `.lilac.` names) | `entity.name.function.lilac-ir` |
| `#name` type (incl. `#bit32`, `#byte_buffer`, user types) | `entity.name.type.lilac-ir` |
| `%place` | `variable.other.lilac-ir` |
| `$label` | `entity.name.label.lilac-ir` |
| `&reg` | `variable.other.ssa.lilac-ir` |
| `function` `extern` `type` | `storage.type.lilac-ir` |
| `in` `out` | `storage.modifier.lilac-ir` |
| `str` `numlit` (data kind) | `storage.type.lilac-ir` |
| `zeroed` (`alloca … : zeroed`) | `storage.modifier.lilac-ir` |
| `call` `copy` `const` `load` `addr` `addr.func` `field` `field.addr` `drop` `alloca` `branch` `branch.if` `return` `panic` `unreachable` | `keyword.other.instruction.lilac-ir` |
| `trunc` `extend.zero` | `keyword.operator.lilac-ir` |
| `i.add` `i.sub` `i.mul` `i.div` `i.div.u` `i.mod` `i.mod.u` `f.add` `f.sub` `f.mul` `f.div` `f.mod` `cmp.eql` `i.cmp.lt` `i.cmp.lt.u` `i.cmp.gt` `i.cmp.gt.u` `f.cmp.lt` `f.cmp.gt` | `keyword.operator.lilac-ir` |
| `0x…` / `0b…` / decimal / float literals | `constant.numeric.lilac-ir` |
| `"…"` string literal | `string.quoted.double.lilac-ir` |

## 7. Language configuration

One shared `language-configuration.json` referenced by both languages:

- `comments.lineComment: "//"`.
- `brackets`: `{}`, `()`, `[]`.
- `autoClosingPairs` and `surroundingPairs` for the same three pairs.
- `wordPattern` that keeps sigils attached to the following identifier so
  double-click selects `@hook`, `#bit32`, `%place`, `!c0`, `$entry`, `&reg`
  (i.e. allow a leading `[@#%!$&]` in the word).

## 8. Testing

- `test/lilac.test` and `test/lilac-ir.test` use the
  `vscode-tmgrammar-test` format: a header line
  `// SYNTAX TEST "source.lilac" "optional label"`, a snippet, and `^`/`~`
  scope assertions beneath the relevant characters.
- Coverage must assert **every row** of the §4 and §6 tables, including the
  disambiguations: `@bit32` → type vs `@add` → hook; `#bit32` → type vs a
  `#`-comment (must not appear); `$"…{x}…"` hole re-tokenization; `{{`/`}}`
  escapes; `//` vs `!c0` vs `%place` in `.lir`; declaration-name captures.
- `npm test` runs both files; a failing assertion exits non-zero.

## 9. Packaging

- `.vscodeignore` excludes `test/`, `node_modules/`, `*.vsix`, and editor cruft.
- `npm run package` → `vsce package` → `lilac-0.1.0.vsix` at
  `editors/vscode/`.
- `README.md` documents install (VSIX), the two language ids, and how to run
  the tests/package scripts. `CHANGELOG.md` starts at `0.1.0`.

## 10. Maintenance / source of truth

- The `.lilac` keyword and builtin lists are pinned by
  `crates/lilac-lex/src/lib.rs`; the `.lir` lists by `IR_SPEC.md` and
  `crates/lilac-ir/src/lib.rs`.
- When the language or IR gains a keyword/instruction/type, the grammar and its
  `.test` assertions must be updated in the same change. The `.test` files are
  the regression guard; §8 requires them to cover the whole mapping.
- This spec is intentionally the only editor artifact for now; the deferred LSP
  spec will build on the same language ids and scope names.
