# Lilac Semantic Tokens

> Scope: add LSP semantic highlighting — `textDocument/semanticTokens/full` —
> that colors names by kind, type names, and builtins from the retained
> analysis, and wire the VSCode extension so themes render them. Sub-project 2
> of the semantic-highlighting work, built on SP1's error-tolerant analysis.
> Branch `ai`. LLVM-only.

## 1. Outcome / constraints / success

**Outcome**

- The LSP advertises `semanticTokensProvider` and answers
  `textDocument/semanticTokens/full` with tokens derived from the retained
  `Analysis`: item names (functions, hooks, objects, enums, fields, consts)
  from the existing index, plus locals, parameters, type names, variants, and
  builtins from a new analysis field.
- Tokens are produced from the retained analysis, so they survive lex/parse/
  type errors (SP1) — colors do not vanish while you type.
- The VSCode extension maps each Lilac token type to standard scopes so
  existing themes color them.
- Keywords and operators stay the TextMate grammar's job (unchanged).

**Constraints**

- `lilac-lsp` remains the only crate permitted `lsp-server`/`lsp-types`/
  `serde_json`; compiler crates stay Rust std-only. No new dependencies.
  `unsafe_code = "forbid"` and `clippy::pedantic = deny` hold.
- No diagnostic changes; the CLI and all committed snapshots are unchanged.
- Semantic tokens are read-only editor data: no lowering/codegen effect.
- The token source is derived from the existing analysis passes; no new
  compiler pass and no re-parse in the LSP.

**Success**

- `semanticTokens/full` on a fixture returns tokens with the expected
  `(range, tokenType, modifiers)` for a function name, an object type name, a
  field, a const, a local, a parameter, a user type reference, a builtin type
  name, and a builtin call name.
- On the same fixture with a syntax error introduced, the surviving tokens are
  still returned (not an empty set), per SP1's error-tolerant analysis.
- `initialize` advertises `semanticTokensProvider` with the legend below.
- `editors/vscode`: `npm test` and `npm run package` succeed.
- `cargo test --workspace` is green.

**Non-goals**

- Keywords/operators as semantic tokens (the grammar owns them).
- Expression-type coloring (only type *names* are colored).
- `semanticTokens/range`, `semanticTokens/delta`, incremental sync.
- Completion/rename (separate future sub-projects; this source may feed them).
- Coloring `.lir` files (grammar only).

### 1.1 Decisions locked during brainstorming

1. **Token source lives in the compiler** (`Analysis`), not an LSP re-parse
   (Approach A): one source of truth, error-tolerant, spans share the driver's
   `FileId`s.
2. **Keywords/operators stay with the TextMate grammar.**
3. **Type names only** — type references and builtin type names, not inferred
   expression types.
4. **Standard token types + a `defaultLibrary` modifier for builtins**, so
   themes color them without a bespoke theme.

## 2. Problem

The LSP advertises only sync/hover/definition (`lilac-lsp/src/lib.rs:28-33`)
and the extension has only the TextMate grammar. Names and types are therefore
uncolored (or colored only by lexical heuristics), and there is no
semantic-aware highlighting that reflects what the compiler resolved. The
retained `AnalysisIndex` already carries item uses (`uses`) and declarations
(`defs`), but not locals, parameters, type references, or builtins — so a new
compiler-side token source is required.

## 3. Architecture / components

```
crates/lilac-ast/src/lib.rs        # Type.name_span (the type's identifier span)
crates/lilac-parse/src/lib.rs      # record name_span in parse_type
crates/lilac-typeck/src/check.rs   # record SemanticSite (locals, params, body types, builtins)
crates/lilac-typeck/src/sigs.rs    # record signature type-reference sites
crates/lilac-hir/src/lib.rs        # SemanticSite/SemanticKind; HirProgram.semantics
crates/lilac-driver/src/lib.rs     # Analysis.semantics assembled from both passes
crates/lilac-lsp/src/lib.rs        # advertise semanticTokensProvider (legend)
crates/lilac-lsp/src/handlers.rs   # derive + encode tokens
crates/lilac-lsp/src/server.rs     # route textDocument/semanticTokens/full
crates/lilac-lsp/src/index.rs      # carry Analysis.semantics into the index
editors/vscode/package.json        # semanticTokenScopes contribution
editors/vscode/CHANGELOG.md        # entry
```

**Untouched:** `lilac-lower`, `lilac-ir`, `lilac-codegen-llvm`, `lilac-const`,
`lilac-module`, `lilac-resolve`, the TextMate grammars.

## 4. The semantic-token source (compiler)

**Shape.** A new `SemanticSite { span: Span, kind: SemanticKind }`, where
`SemanticKind` is one of:

- `Local` — a local binding (declaration name and each use);
- `Param` — a parameter (declaration name and each use);
- `UserType` — a user object/enum name in type position;
- `Variant` — an enum variant (declaration name and each `.Variant` use);
- `BuiltinType` — a builtin type name (`@bit8`…`@fbit128`, `@bit_native`,
  `@byte_buffer`, `@format_args`, `unknown`);
- `BuiltinFunc` — a compiler-known intrinsic call name (`@add`, `@truncate`,
  `@load_u8`, …).

Item names (functions, hooks, objects, enums, fields, consts) are **not** in
this source: the index already maps them via `uses` (uses) and `defs`
(declarations + kind). Enum variants are the exception: a variant is not an item
with its own `DefInfo`, so its declaration and each `.Variant` use are recorded
here as `Variant` sites. A `.Variant` use also produces an item `use` resolving
to the owning enum; the LSP merge drops that item use in favor of the more
specific `Variant` site.

**Where recorded.** During typeck, where each site is resolved:

- local declarations: at the `let`/binding name span in `check_bodies`;
- local uses: in `check_expr`'s `Ident` arm, where `ident_type` already
  determines whether the name is a local (item uses there go through
  `record_use`; locals currently record nothing);
- parameter names: at the parameter name span when a function body is set up;
- variant declarations: at the variant name token when the enum's items are
  collected, and `.Variant` uses: in `check_expr`'s `Tag` arm at the tag token
  (not the leading dot);
- type references: in `resolve_type` (called for both signature types during
  `collect_sigs` and body types during `check_bodies`), classified
  `BuiltinType` when the name resolves via `types.builtin` (so `@bit8` and
  `unknown` are builtins) and `UserType` otherwise;
- builtin calls: in the intrinsic-call path when `lookup_intrinsic` matches, at
  the callee identifier's span.

To color only the type *identifier* (not a leading `ref`), the AST `Type` gains
a `name_span: Span` set by `parse_type`; the token source uses it.

**Storage.** Signature type references are recorded during `collect_sigs` and
body sites during `check_bodies`; the driver assembles both into a new
`Analysis.semantics: Vec<SemanticSite>`. Sites are deduped by `(span, kind)`
and sorted by `(file, lo)` so the LSP receives a deterministic list. The list
is populated best-effort, exactly like `analysis` today, so it survives errors.

## 5. LSP capability + derivation

- `initialize` adds `semantic_tokens_provider: Some(SemanticTokensServerFull)`
  carrying the legend (`token_types` + `token_modifiers` below).
- `handle_request` routes `textDocument/semanticTokens/full`: build the token
  list, encode, reply.
- `Retained`/`AnalysisIndex` carries `Analysis.semantics` so both the project
  and standalone compile paths serve tokens (the index is already rebuilt per
  compile).
- **Derivation:** merge (a) the index's item uses (`uses` → `defs.kind`) and
  declaration names (`defs.span`), and (b) `Analysis.semantics`, into a list of
  `(span, token_type, modifiers)`. Map each `span` to a `Range` via the
  existing `convert::span_range`; skip dummy spans and spans whose file is not
  an open/known URI. Sort by `(line, col)` and drop overlaps (later tokens at
  the same start are dropped) so the encoding is valid.
- **Encoding:** LSP `SemanticTokens` delta encoding (`deltaLine`,
  `deltaStart`, `length`, `tokenType`, `tokenModifiers`), lengths in UTF-16
  code units (the existing `convert::position` already counts UTF-16).
- **Error tolerance:** because the source is the retained analysis (SP1), a
  broken buffer still yields tokens for whatever resolved; a wholly
  unparseable file yields the tokens from any recovered items.

## 6. Legend and kind mapping

Legend `token_types` (indices are positional):

| index | token type | Lilac kind(s) |
|-------|-----------|---------------|
| 0 | `function` | function, hook |
| 1 | `type` | object, user type reference, builtin type name |
| 2 | `enum` | enum |
| 3 | `property` | field |
| 4 | `enumMember` | variant (declaration and `.Variant` use) |
| 5 | `variable` | const, local |
| 6 | `parameter` | parameter |

Legend `token_modifiers`: `defaultLibrary` (index 0) — set for `BuiltinType`
(including `unknown`) and `BuiltinFunc` (intrinsic call names).

Item kinds come from `DefInfo.kind` (`function`/`hook`/`object`/`enum`/`const`/
`field`), and variant declarations/uses from `SemanticKind::Variant`; the mapping
table above is the single source of truth, shared by the LSP handler and
documented for the VSCode scopes.

## 7. VSCode

- `package.json` contributes `semanticTokenScopes` mapping each Lilac token
  type to standard TextMate scopes (`function` → `entity.name.function`,
  `type` → `entity.name.type`, `enum` → `entity.name.type.enum`, `property` →
  `variable.other.property`, `enumMember` → `variable.other.enummember`,
  `variable` → `variable.other`, `parameter` → `variable.parameter`), so common
  themes color them without a custom theme.
- No legend declaration is needed in the extension: VSCode takes the legend
  from the LSP client's `semanticTokensProvider`.
- `npm test` and `npm run package` must stay green; add a CHANGELOG entry.

## 8. Testing

- **`lilac-ast`/`lilac-parse`:** `Type.name_span` is the identifier's span for
  `@bit32`, `ref @bit32`, and `GenObj[@bit32]`.
- **`lilac-typeck`:** `HirProgram.semantics` (and the `Sigs` type-reference
  list) record the expected `(span, kind)` for a fixture covering a local
  declaration + use, a parameter + use, a user type reference, a builtin type
  name, and a builtin call name; item names are absent.
- **`lilac-driver`:** `Analysis.semantics` is populated after a successful
  compile and non-empty after a body type error.
- **`lilac-lsp`:** `initialize` advertises `semanticTokensProvider` with the
  legend; `semanticTokens/full` returns the expected tokens (ranges, types,
  modifiers) for a fixture; the same fixture with a syntax error still returns
  the surviving tokens; a clean file with nothing to color returns an empty
  data array.
- **`editors/vscode`:** the `semanticTokenScopes` contribution is present;
  `npm test` and `npm run package` succeed.

## 9. Gates

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `editors/vscode`: `npm test` and `npm run package`

## 10. Maintenance / source of truth

- `Analysis.semantics` is the single non-item token source; the legend mapping
  table (§6) is the single kind→token-type mapping, shared by the LSP handler
  and the VSCode scopes.
- Item names come only from `uses`/`defs`; the source never duplicates them.
  The one deliberate exception is `Variant` (enum variants are not items), whose
  `.Variant` item use is dropped in favor of the `Variant` site.
- The token source is populated best-effort (like `analysis`), so editor
  features never depend on a clean compile.

## 11. Follow-ups

- `semanticTokens/range` and `semanticTokens/delta` (incremental).
- Completion and rename/references, which can reuse `Analysis.semantics`.
- Coloring `.lir` (grammar-only today).
