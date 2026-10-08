# Lilac Semantic Tokens Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add LSP semantic highlighting — `textDocument/semanticTokens/full` coloring names by kind, type names, and builtins from the retained analysis — and wire the VSCode extension so themes render them.

**Architecture:** The compiler records a small semantic-token source (`Analysis.semantics`) during typeck; the LSP merges it with the existing index (`uses`/`defs`) into delta-encoded LSP tokens and advertises a legend; the VSCode extension maps the legend's token types to standard scopes.

**Tech Stack:** Rust workspace (compiler crates std-only; `lilac-lsp` uses `lsp-server`/`lsp-types`/`serde_json`); TypeScript/JSON for the VSCode extension.

**Spec:** `docs/superpowers/specs/2026-10-08-lilac-semantic-tokens-design.md`

## Global Constraints

- `lilac-lsp` remains the only crate permitted `lsp-server`/`lsp-types`/`serde_json`; compiler crates stay Rust std-only. No new dependencies. `unsafe_code = "forbid"` and `clippy::pedantic = deny` hold; no `unwrap`/`expect`/`panic!` on user input in production code (tests may).
- No diagnostic changes; the CLI and all committed snapshots are unchanged. Semantic tokens have no lowering/codegen effect.
- Legend `token_types` in order: `function`, `type`, `enum`, `property`, `enumMember`, `variable`, `parameter`. `token_modifiers`: `defaultLibrary` (index 0), set for `@`-builtin type names and intrinsic call names.
- Item names (function/hook/object/enum/field/const) come only from the index (`uses`/`defs`). Enum variants are the exception: they are not items, so `Analysis.semantics` records their declarations and `.Variant` uses as `SemanticKind::Variant`; the LSP drops the `.Variant` item use (which resolves to the owning enum) in favor of that more specific site.
- Gates: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; and in `editors/vscode`: `npm test` and `npm run package`.

## Review Focus

- A `ref T` type reference must color only `T`, never `ref` — pinned by the Task 1 `name_span` test and the Task 3 token test.
- Builtin type names and intrinsic call names must carry the `defaultLibrary` modifier — pinned by the Task 3 token test.
- A file with a syntax error must still return tokens (not an empty set) — pinned by the Task 3 syntax-error test.
- `Analysis.semantics` must contain only non-item kinds (no function/object/field names) — pinned by the Task 2 recording test.
- The encoded token list must be sorted by `(line, col)`, non-overlapping, with UTF-16 lengths — pinned by the Task 3 encoding test.

---

### Task 1: `Type.name_span`

**Files:**
- Modify: `crates/lilac-ast/src/lib.rs` (`Type`)
- Modify: `crates/lilac-parse/src/lib.rs` (`parse_type` and the `Type { … }` constructors it drives)
- Test: `crates/lilac-parse/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `Type.name_span: Span` — the span of the type's identifier token (`@bit32`, `Foo`), excluding a leading `ref` and any generic arguments.

- [ ] **Step 1: Write the failing test**

Add a parser test asserting `name_span` equals the identifier's span for `x: @bit32` (name `@bit32`), `x: ref @bit32` (name span covers `@bit32`, not `ref`), and `x: GenObj[@bit32]` (name span covers `GenObj`, not `[@bit32]`). Compare against the identifier's byte span computed from the source.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p lilac-parse type_name_span`
Expected: FAIL (no `name_span` field).

- [ ] **Step 3: Add the field and populate it**

Add `name_span: Span` to `ast::Type`; in `parse_type` record the identifier token's span (after consuming an optional `ref`), and set it in every `Type { … }` literal (including the poison type from Task 1 of the error-tolerant sub-project). Update the AST visitor if it touches `Type` fields.

- [ ] **Step 4: Run the test to verify it passes, then the full gates**

Run: `cargo test -p lilac-parse type_name_span` then `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "ast: record a type reference's name span"
```

---

### Task 2: The semantic-token source

**Files:**
- Modify: `crates/lilac-hir/src/lib.rs` (`SemanticKind`, `SemanticSite`, `HirProgram.semantics`)
- Modify: `crates/lilac-typeck/src/check.rs` (record locals, params, body types, builtins)
- Modify: `crates/lilac-typeck/src/sigs.rs` (record signature type references)
- Modify: `crates/lilac-driver/src/lib.rs` (`Analysis.semantics`)
- Test: `crates/lilac-typeck/src/check.rs`, `crates/lilac-driver/src/lib.rs`

**Interfaces:**
- Consumes: Task 1 (`Type.name_span`).
- Produces: `enum SemanticKind { Local, Param, UserType, BuiltinType, BuiltinFunc }`; `struct SemanticSite { pub span: Span, pub kind: SemanticKind }`; `HirProgram.semantics: Vec<SemanticSite>`; `Analysis.semantics: Vec<SemanticSite>` (deduped by `(span, kind)`, sorted by `(file, lo)`).

- [ ] **Step 1: Write the failing tests**

Add typeck tests: for a fixture with a local declaration + use, a parameter + use, a user type reference, a builtin type name, and a builtin call name, `HirProgram.semantics` contains the expected `(span, kind)` for each and **no** item kinds (no function/object/field entries). Add a driver test: `Analysis.semantics` is non-empty after a successful compile and after a body type error.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p lilac-typeck semantic_sites` and `cargo test -p lilac-driver semantic_analysis`
Expected: FAIL (`SemanticSite`/`Analysis.semantics` do not exist).

- [ ] **Step 3: Add the types and record the sites**

Add `SemanticKind`/`SemanticSite` to `lilac-hir` and `semantics: Vec<SemanticSite>` to `HirProgram`. In `check_bodies`, record `Local` at a `let` binding's name span and at each local-ident use, `Param` at each parameter name span, `BuiltinFunc` at an intrinsic call's callee span, and `UserType`/`BuiltinType` at each body type reference's `name_span` (classify by whether the name starts with `@`). In `sigs.rs`, record signature type references the same way into a list carried on `Sigs`. Assemble both into `Analysis.semantics` in the driver, dedup by `(span, kind)`, sort by `(file, lo)`.

- [ ] **Step 4: Run the tests to verify they pass, then the full gates**

Run: `cargo test -p lilac-typeck semantic_sites && cargo test -p lilac-driver semantic_analysis` then `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "typeck: record a semantic-token source on the analysis"
```

---

### Task 3: LSP semantic tokens

**Files:**
- Modify: `crates/lilac-lsp/src/lib.rs` (advertise `semanticTokensProvider` + legend)
- Modify: `crates/lilac-lsp/src/index.rs` (carry `Analysis.semantics`)
- Modify: `crates/lilac-lsp/src/handlers.rs` (derive + encode tokens)
- Modify: `crates/lilac-lsp/src/server.rs` (route `textDocument/semanticTokens/full`)
- Test: `crates/lilac-lsp/tests/` (a new `semantic_tokens.rs`)

**Interfaces:**
- Consumes: Task 2 (`Analysis.semantics`, `SemanticKind`); the existing index (`defs`, `uses`) and `convert::span_range`/`convert::position`.
- Produces: `initialize` advertises `semanticTokensProvider` (legend per Global Constraints); `textDocument/semanticTokens/full` returns delta-encoded tokens.

- [ ] **Step 1: Write the failing tests**

Add LSP tests (memory connection): `initialize` advertises `semanticTokensProvider` with the 7 token types and 1 modifier; `semanticTokens/full` on a fixture returns tokens covering a function name, an object type name, a field, a const, a local, a parameter, a user type reference, a builtin type name, and a builtin call name — asserting ranges, `tokenType`, and `tokenModifiers` (builtins carry `defaultLibrary`); a `ref T` reference's token covers only `T`; the same fixture with a syntax error still returns the surviving tokens; a clean file with nothing to color returns an empty `data` array.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p lilac-lsp semantic_tokens`
Expected: FAIL (method not advertised/handled).

- [ ] **Step 3: Implement the capability and handler**

Advertise `semanticTokensProvider` with the legend in `lib.rs`. Carry `Analysis.semantics` on `Retained`/`AnalysisIndex` in `index.rs`. In `handlers.rs`, add `semantic_tokens(retained, uri) -> SemanticTokens` that merges item uses/declarations (from `defs`/`uses`) with `semantics`, maps each kind to `(token_type_index, modifier_bits)` per the mapping table, converts spans via `convert::span_range`, drops dummy/unknown-file spans and overlaps, sorts by `(line, col)`, and delta-encodes with UTF-16 lengths. Route the request in `server.rs`.

- [ ] **Step 4: Run the tests to verify they pass, then the full gates**

Run: `cargo test -p lilac-lsp semantic_tokens` then `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-lsp
git commit -m "lsp: serve semantic tokens from the analysis"
```

---

### Task 4: VSCode semantic-token scopes

**Files:**
- Modify: `editors/vscode/package.json` (`contributes.semanticTokenScopes`)
- Modify: `editors/vscode/CHANGELOG.md`
- Test: `editors/vscode/test/` (a small assertion in the existing test runner)

**Interfaces:**
- Consumes: Task 3 (the legend's token types).
- Produces: a `semanticTokenScopes` contribution mapping `function`→`entity.name.function`, `type`→`entity.name.type`, `enum`→`entity.name.type.enum`, `property`→`variable.other.property`, `enumMember`→`variable.other.enummember`, `variable`→`variable.other`, `parameter`→`variable.parameter`.

- [ ] **Step 1: Write the failing test**

Add a test that reads `package.json` and asserts `contributes.semanticTokenScopes` maps every legend token type to a non-empty scope list.

- [ ] **Step 2: Run it to verify it fails**

Run: `npm test` (in `editors/vscode`)
Expected: FAIL (no `semanticTokenScopes`).

- [ ] **Step 3: Add the contribution and CHANGELOG entry**

Add `contributes.semanticTokenScopes` to `package.json` with the mapping above, and a CHANGELOG entry describing semantic highlighting.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npm test` then `npm run package`
Expected: both succeed.

- [ ] **Step 5: Commit**

```bash
git add editors/vscode
git commit -m "vscode: map semantic token types to standard scopes"
```

---

## Self-Review

**Spec coverage:** §4 source → Tasks 1–2; §5 capability/derivation → Task 3; §6 legend → Task 3 (and Task 4 scopes); §7 VSCode → Task 4; §8 testing → each task; §9 gates → all; §10 maintenance → the legend table is shared by Tasks 3–4. No section unmapped.

**Ordering:** Task 1 adds `name_span` (green alone). Task 2 records the source (needs Task 1). Task 3 consumes it (needs Task 2). Task 4 maps the legend (needs Task 3's token types). Each task ends green and independently testable.

**Type/name consistency:** `Type.name_span`, `SemanticKind { Local, Param, UserType, BuiltinType, BuiltinFunc }`, `SemanticSite { span, kind }`, `HirProgram.semantics`, `Analysis.semantics`, the 7 token types + `defaultLibrary` are spelled identically wherever they appear.
