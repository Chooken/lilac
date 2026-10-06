# Lilac Language Server — Hover & Go-to-Definition

> Scope: sub-project 2 of the editor LSP effort. Extends `crates/lilac-lsp` with
> `textDocument/hover` and `textDocument/definition`, backed by a name-site
> index emitted by the type checker and an analysis bundle retained by the
> driver. Branch `ai`. This is editor tooling; the compiler crates gain only
> additive APIs. Completion (sub-project 3) and references/rename (sub-project
> 4) are deliberately deferred.

## 1. Outcome / constraints / success

**Outcome**

- With a `.lilac` file open, hovering a name shows its kind and signature (or
  an expression's type), and `Go to Definition` on a name jumps to the item's
  declaration — across files in a project.
- Both work while the project has compiler errors (best-effort).

**Constraints**

- No change to existing driver entry-point return types, `render_diagnostics`,
  or any snapshot. Compiler additions are additive.
- Compiler crates stay std-only; `lilac-lsp` remains the only crate permitted
  `lsp-server`/`lsp-types`/serde.
- No panics on user input; `unsafe_code = "forbid"` and `clippy::pedantic`
  hold.
- Full document sync stays; incremental sync is future work.

**Success**

- `initialize` advertises `hover_provider` and `definition_provider`.
- Hover on a call shows `function foo(@bit8) -> @bit32`; hover on a const use
  shows `const PI` plus its type; hover on a number literal shows its type.
- Definition on a call/ident/member/variant jumps to the declaration's
  whole-declaration range; cross-file in project mode.
- On a project with a type error, definition on a resolvable name still
  returns its declaration.
- The full gates pass (section 9).

**Non-goals**

- Locals (parameters, `let` bindings).
- Hover/definition on declaration name tokens; type-annotation hover.
- Signature help, semantic tokens, code actions, inlay hints.
- Incremental sync, multi-root workspaces, `.lir` features.

### 1.1 Decisions locked during brainstorming

1. **Symbol scope: items only** — functions, objects, enums, consts, hooks, and
   object members. Locals carry no `DefId` today and are out of scope.
2. **Definition range: the whole declaration** (`ResItem.span`); no
   parser/AST name-token span is added.
3. **Hover content: name + kind + type/signature.**
4. **Best-effort on broken projects**: retain partial analysis; definition
   works whenever name resolution succeeded, hover degrades to name/kind when
   types are absent.
5. **Name-site data is emitted by the checker** (Approach A): additive, with no
   HIR-shape, lowering, or snapshot churn.

## 2. Architecture / components

```
crates/lilac-hir/src/lib.rs        # + NameSite; + HirProgram.uses
crates/lilac-typeck/src/check.rs   # record a NameSite at each item resolution
crates/lilac-ty/src/lib.rs         # + #[derive(Clone)] on Types
crates/lilac-driver/src/lib.rs     # + Analysis; + Session.analysis / analysis()

crates/lilac-lsp/src/lib.rs        # + hover/definition capabilities
crates/lilac-lsp/src/convert.rs    # + byte_offset (the inverse of position)
crates/lilac-lsp/src/index.rs      # NEW: AnalysisIndex (defs, uses, exprs, uris)
crates/lilac-lsp/src/handlers.rs   # NEW: hover/definition request handlers
crates/lilac-lsp/src/server.rs     # retain Session + index; dispatch requests
```

Data flow:

```
compile -> Session.analysis { project, types, sigs, hir(uses) }   [retained]
        -> AnalysisIndex { defs, uses, exprs, file_uris }
request -> uri -> FileId -> position -> byte -> query index -> reply
```

## 3. Compiler-side additions

### 3.1 `lilac-hir`

```rust
/// One resolved item use: the name token's span and what it resolved to.
#[derive(Clone, Copy, Debug)]
pub struct NameSite {
    /// The name token (callee, member field, tag, or bare identifier).
    pub span: Span,
    /// The item it resolved to (`DefId::ERROR` when not an item).
    pub def: DefId,
}
```

`HirProgram` gains one additive field, `pub uses: Vec<NameSite>`. `HirProgram`
is constructed in exactly one place (`lilac-typeck` `check.rs`), and its
`Default`/`Clone` derives are preserved (`Vec` defaults to empty).

### 3.2 `lilac-typeck`

`Checker` gains `uses: Vec<NameSite>`, pushed during the source-AST walk at
each item resolution:

- bare identifier `Expr::Ident(name, span)` → the ident span + the resolved
  item `DefId`; locals/intrinsics are skipped (`DefId::ERROR` is not recorded).
- call callee `Expr::Call { callee: Expr::Ident(_, span), .. }` → the callee
  token's span + the function/hook `DefId`; intrinsic and function-valued-local
  callees are skipped.
- member access `Expr::Member { field, span, .. }` → the field token's span +
  the member's `DefId`. The AST keeps only the whole-access span, so the field
  token is `span.hi - field.len()`; if that does not land on the field text,
  the whole access span is used.
- enum tag/variant (`Expr::Tag(name, span)` and variant construction) → the tag
  span + the owning enum's `DefId` (variants have no `DefId` of their own).

`check_bodies` moves `checker.uses` onto the returned `HirProgram`. Duplicate
sites produced by generic monomorphization carry the same span and def and are
deduped when the LSP builds its index.

### 3.3 `lilac-ty`

Add `#[derive(Clone)]` to `Types` (every field is already `Clone`).

### 3.4 `lilac-driver`

```rust
/// Retained analysis artifacts from the last compile.
#[derive(Clone, Debug)]
pub struct Analysis {
    /// Resolved namespaces and per-file scopes.
    pub project: ResolvedProject,
    /// Type table (for `display_ref`).
    pub types: Types,
    /// Collected function/hook signatures.
    pub sigs: Sigs,
    /// Checked HIR (carries `uses`).
    pub hir: HirProgram,
}
```

`Session` gains `pub analysis: Option<Analysis>` (initialized `None` in
`Default`) and a `pub fn analysis(&self) -> Option<&Analysis>` accessor.

`finish_pipeline` populates `self.analysis` at each pipeline boundary so a
project that fails later still exposes resolution and uses:

- after `collect_sigs`: `project` + `types` + `sigs` (no `hir`);
- after `check_bodies`: also `hir`;
- on the success path after `lower`: the moved analysis.

Every early return stores the partial analysis before returning. Existing
entry-point return types, `render_diagnostics`, and all snapshots are
unchanged.

**Untouched:** `lilac-lower`, `lilac-ir`, codegen, VM.

## 4. Analysis index (LSP)

Built once per compile and stored on `Server`:

- `defs: HashMap<DefId, DefInfo>` where
  `DefInfo { name: String, qualified: String, kind: &'static str, span: Span }`.
  Built from `project.all_items()`, recursing into `ResKind::Object.members`,
  plus every overload in `project.hooks`. `kind` is `function`/`hook`/`object`/
  `enum`/`const`/`field`; `qualified` is the namespace-qualified name.
- `uses: Vec<NameSite>` from `hir.uses`, deduped by `(span, def)` and sorted by
  `(span.file, span.lo)`.
- `exprs: Vec<(Span, TypeRef)>` from a single HIR walk, for type hover.
- `file_uris: HashMap<FileId, Url>` and `uri_files: HashMap<Url, FileId>` from
  `session.project_files()`; in single-file mode, `FileId(0)` ↔ the document
  URI.

## 5. Hover & definition semantics

**Definition**

- Find the smallest name site whose span contains the cursor byte (same file),
  smallest by span length and ties broken by the earliest `lo`.
- `def` → `defs.get(def)` → `Location { uri: file_uris[span.file], range:
  span_range(span) }` (the whole-declaration range).
- Variant uses (`.Some`) resolve to the owning enum's declaration.
- No site under the cursor, or `DefId::ERROR`, or a missing `DefInfo` → `null`.

**Hover**

- If a name site contains the cursor byte:
  - function/hook → `function <qualified>(<params>) -> <returns>`, with resolved
    types from `sigs` rendered by `Types::display_ref`;
  - object/enum → `object <qualified>` / `enum <qualified>`;
  - const/field/other → `<kind> <qualified>` plus a type line taken from the
    smallest containing HIR expression.
- Otherwise, if a HIR expression contains the cursor byte, show just its type.
- The reported range is the name-site span when present, else the expression
  span. Content is Markdown with a fenced `lilac` block; no range → `null`.

## 6. Error handling / limitations

- Best-effort: the index is built from whatever `analysis()` holds.
- Stale window: a request between an edit and the ~200 ms debounce uses the
  previous compile; a position that lands on nothing returns `null` (never
  panics).
- The member field token span is derived from the access span (assumes no
  trailing trivia), falling back to the whole span.
- Monomorphization duplicates are deduped by span.

## 7. Capabilities & lifecycle

- `initialize` result adds `hover_provider: Some(HoverProviderCapability::
  Simple(true))` and `definition_provider: Some(OneOf::Left(true))` alongside
  the existing `text_document_sync = Full` and `serverInfo`.
- `handle_request` handles `textDocument/hover` and `textDocument/definition`;
  every other request stays `MethodNotFound`. Notifications are unchanged.
- A new `convert::byte_offset(src: &str, position: &Position) ->
  Option<usize>` is the inverse of `convert::position`: it walks the target
  line counting UTF-16 code units to the byte index, clamping a position past
  the line end to the line end. The existing private `convert::span_range` is
  reused (made `pub(crate)`) to turn a `Span` into a `Range`.

## 8. Testing

- `lilac-typeck`: `hir.uses` records callee, member-field, tag, and bare-ident
  sites with the expected spans and defs; locals and intrinsics are skipped.
- `lilac-driver`: `analysis()` is `Some` after a successful `compile_project`
  (with a non-empty `hir.uses`), and after a project whose body has a type
  error it still carries `project` + `hir.uses` (best-effort).
- `lilac-lsp` `convert`: `byte_offset` inverts `position` for ASCII and
  multibyte text, and clamps past the line end.
- `lilac-lsp` protocol tests (memory connection): `initialize` advertises
  hover + definition; hover on a call returns its signature; definition on a
  call jumps to the declaration (cross-file in a project); hover on a const
  use returns its type; definition on a member field; `null` on whitespace;
  definition still resolves on a project with a type error.

## 9. Gates

- `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, and the release VM bench gates
  (`cargo clippy --release -p lilac-vm --all-targets --features bench-baseline`
  / `cargo test --release -p lilac-vm --features bench-baseline`).
- `editors/vscode`: `npm test` and `npm run package` both succeed (unchanged).

## 10. Maintenance / source of truth

- `NameSite` is the single use-site schema; `HirProgram.uses` is its only
  source.
- `DefId → declaration site` comes from `ResItem.span`; the LSP `defs` map is
  the only DefId→site index.
- Type rendering is pinned by `Types::display_ref`.
- Compiler crates stay std-only; `lilac-lsp` remains the sole crate permitted
  `lsp-server`/`lsp-types`/serde (see `AGENTS.md`).

## 11. Follow-ups (separate specs)

- Sub-project 3: completion.
- Sub-project 4: references + rename.
- Later: locals, declaration-name hover, signature help, incremental sync,
  multi-root, semantic tokens, `.lir` support.
