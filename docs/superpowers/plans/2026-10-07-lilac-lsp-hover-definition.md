# LSP Hover & Go-to-Definition Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `textDocument/hover` and `textDocument/definition` to `lilac-lsp`, backed by a checker-emitted name-site index and an analysis bundle retained by the driver.

**Architecture:** The type checker records every resolved item use (bare identifier, call callee, member field, enum variant) as a `NameSite { span, def }` on `HirProgram.uses`. The driver retains the last compile's `ResolvedProject`/`Types`/`Sigs`/`HirProgram` as a `Session.analysis`. The LSP keeps the last `Session` and a derived `AnalysisIndex` (def table, uses, expression types) and answers hover/definition from it, best-effort on broken projects.

**Tech Stack:** Rust; `lsp-server`/`lsp-types` (only in `lilac-lsp`); std-only compiler crates.

**Spec:** `docs/superpowers/specs/2026-10-07-lilac-lsp-hover-definition-design.md`

## Global Constraints

- Compiler crates (`lilac-hir`, `lilac-typeck`, `lilac-ty`, `lilac-driver`) stay std-only — no serde.
- `lilac-lsp` is the only crate permitted `lsp-server`/`lsp-types`/serde.
- No `unsafe` (`unsafe_code = "forbid"`); `clippy::pedantic = deny` holds.
- No `unwrap`/`expect`/`panic!` on user input in production code (tests may).
- Do not change existing driver entry-point return types, `render_diagnostics`, or any snapshot.
- Full document sync stays; no incremental sync.
- Run `cargo fmt` before every commit.

## Review Focus

These are the input classes/failure modes the spec implies but no happy-path test covers; each is pinned by a test in the task that owns the code.

1. Cursor on whitespace or between tokens → hover and definition return `null`, never panic (Task 9, Task 10).
2. A position past the end of a line (stale text) → clamped to line end; no panic (Task 7).
3. Definition whose target is in another file → the reply carries the target file's URI, not the request's (Task 10).
4. A UTF-16 offset on a multibyte line → correct byte offset (Task 7).
5. A project with a type error → definition still resolves; hover degrades without panicking (Task 5, Task 10).

---

### Task 1: `NameSite` type and bare-identifier uses

**Files:**
- Modify: `crates/lilac-hir/src/lib.rs`
- Modify: `crates/lilac-typeck/src/check.rs`
- Test: `crates/lilac-typeck/src/check.rs` (the existing `mod tests` at line 5361)

**Interfaces:**
- Produces: `lilac_hir::NameSite { pub span: lilac_span::Span, pub def: lilac_resolve::DefId }` (`#[derive(Clone, Copy, Debug)]`).
- Produces: `HirProgram.uses: Vec<NameSite>` (additive field).
- Produces: `Checker::record_use(&mut self, span: Span, def: DefId)` — a no-op when `def == DefId::ERROR`.

- [ ] **Step 1: Write the failing test**

In `mod tests` (near `check_hir`), add:

```rust
#[test]
fn bare_ident_use_is_recorded() {
    let src = "pi: @bit32 = 0x2A\nmain: function () nothing {\n    x: @bit32 = pi\n}\n";
    let (diags, hir) = check_hir(src);
    assert!(!diags.has_errors(), "{diags:?}");
    let lo = src.rfind("pi").expect("use site") as u32;
    assert!(
        hir.uses
            .iter()
            .any(|s| s.span.lo.0 == lo && s.def != lilac_resolve::DefId::ERROR),
        "{:?}",
        hir.uses
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-typeck bare_ident_use_is_recorded`
Expected: FAIL — no field `uses` on `HirProgram`.

- [ ] **Step 3: Implement**

In `crates/lilac-hir/src/lib.rs`, add the struct and the field:

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

Add `pub uses: Vec<NameSite>,` to `HirProgram` (the single construction site at `check.rs:146` must add `uses: checker.uses`).

In `crates/lilac-typeck/src/check.rs`:
- Add `uses: Vec<NameSite>,` to `struct Checker` and `uses: Vec::new(),` to the `Checker { .. }` literal in `check_bodies` (line 101).
- Add the helper:

```rust
fn record_use(&mut self, span: Span, def: DefId) {
    if def != DefId::ERROR {
        self.uses.push(NameSite { span, def });
    }
}
```

- In the `Expr::Ident(name, span)` arm (line 1918), after `ident_type` returns `(ty, def)`, call `self.record_use(*span, def)` (before building the `HirExpr`).
- At the `HirProgram { .. }` construction (line 146), add `uses: checker.uses,`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lilac-typeck bare_ident_use_is_recorded`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-hir/src/lib.rs crates/lilac-typeck/src/check.rs
git commit -m "feat(typeck): record bare-identifier name sites on HIR"
```

---

### Task 2: Call-callee uses

**Files:**
- Modify: `crates/lilac-typeck/src/check.rs`
- Test: `crates/lilac-typeck/src/check.rs`

**Interfaces:**
- Consumes: `Checker::record_use` (Task 1).
- Produces: `NameSite` entries for function/hook calls (not intrinsics or function-valued locals).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn callee_use_is_recorded() {
    let src = "foo: function () nothing {\n}\nmain: function () nothing {\n    foo()\n}\n";
    let (diags, hir) = check_hir(src);
    assert!(!diags.has_errors(), "{diags:?}");
    let lo = src.rfind("foo").expect("call") as u32;
    assert!(
        hir.uses
            .iter()
            .any(|s| s.span.lo.0 == lo && s.def != lilac_resolve::DefId::ERROR),
        "{:?}",
        hir.uses
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-typeck callee_use_is_recorded`
Expected: FAIL — no site at the call.

- [ ] **Step 3: Implement**

In `check_call` (line 2609), in the `Expr::Ident(name, name_span)` branch, at the `Ok(Some(item))` arm (line 2644) before dispatching to `check_named_call`, record the callee when the item is callable:

```rust
if matches!(item.kind, ResKind::Func { .. } | ResKind::Hook { .. }) {
    self.record_use(*name_span, item.id);
}
```

This one site covers calls in statement, value, and multi-return positions. `Local` and intrinsic callees are not recorded.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lilac-typeck callee_use_is_recorded`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-typeck/src/check.rs
git commit -m "feat(typeck): record call-callee name sites"
```

---

### Task 3: Member-field uses

**Files:**
- Modify: `crates/lilac-typeck/src/check.rs`
- Test: `crates/lilac-typeck/src/check.rs`

**Interfaces:**
- Consumes: `Checker::record_use` (Task 1).
- Produces: `NameSite` entries for object member reads, writes, and method calls.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn member_use_is_recorded() {
    let src = "Object: object {\n    member: @bit8\n}\nmain: function () nothing {\n    v: Object = Object {\n        member = 0x1\n    }\n    x: @bit8 = v.member\n}\n";
    let (diags, hir) = check_hir(src);
    assert!(!diags.has_errors(), "{diags:?}");
    assert!(
        hir.uses.iter().any(|s| {
            &src[s.span.lo.0 as usize..s.span.hi.0 as usize] == "member"
                && s.def != lilac_resolve::DefId::ERROR
        }),
        "{:?}",
        hir.uses
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-typeck member_use_is_recorded`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add a helper that re-resolves a member's `DefId` from a receiver type (mirrors the owner lookup in `field_type`, lines 1297–1304):

```rust
fn member_def(&self, obj: TypeId, field: &str) -> Option<DefId> {
    let owner = match self.types.kind(obj) {
        lilac_ty::TypeKind::Object(def) => Some(*def),
        lilac_ty::TypeKind::Instance { base, .. } => Some(*base),
        _ => None,
    }?;
    if let Some(ResKind::Object { members, .. }) = self.find_by_def(owner) {
        members.get(field).map(|m| m.id)
    } else {
        None
    }
}
```

Add a span helper (the AST keeps only the whole-access span):

```rust
fn field_span(span: Span, field: &str) -> Span {
    Span::new(span.hi.0 - field.len() as u32, span.hi.0, span.file)
}
```

Record after a successful member resolution:
- in `check_member_read` (line 1667), after `field_type` returns `Ok(ty)`:

```rust
if let Some(def) = self.member_def(obj_ty.id, field) {
    self.record_use(field_span(span, field), def);
}
```

- in the member write path (line 1260), after `field_type(...)` returns `Ok(ty)`, the same two lines using `obj_ty.id`.
- in `check_member_call` (line 3015), after the receiver type is known, the same two lines.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lilac-typeck member_use_is_recorded`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-typeck/src/check.rs
git commit -m "feat(typeck): record member-field name sites"
```

---

### Task 4: Enum-variant uses

**Files:**
- Modify: `crates/lilac-typeck/src/check.rs`
- Test: `crates/lilac-typeck/src/check.rs`

**Interfaces:**
- Consumes: `Checker::record_use` (Task 1).
- Produces: `NameSite` entries for variant constructions (`.Some(..)`) and payload-less tags (`.None`), pointing at the owning enum's `DefId`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn variant_use_is_recorded() {
    let src = "OptionalBit32: enum {\n    Some: @bit32\n    None\n}\nmain: function () nothing {\n    v: OptionalBit32 = .Some(0x0)\n    w: OptionalBit32 = .None\n}\n";
    let (diags, hir) = check_hir(src);
    assert!(!diags.has_errors(), "{diags:?}");
    assert!(
        hir.uses.iter().any(|s| {
            &src[s.span.lo.0 as usize..s.span.hi.0 as usize] == ".Some"
                && s.def != lilac_resolve::DefId::ERROR
        }),
        "{:?}",
        hir.uses
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-typeck variant_use_is_recorded`
Expected: FAIL.

- [ ] **Step 3: Implement**

The tag token `.Some` starts the variant span and is `1 + name.len()` bytes long. In `check_variant` (line 4842), before returning each `HirExprKind::Variant` (lines 4893 and 4916), record:

```rust
let tag = Span::new(span.lo.0, span.lo.0 + 1 + name.len() as u32, span.file);
self.record_use(tag, def);
```

For the payload-less `Expr::Tag` arm (line 1941), the enum's `DefId` is only known through `tag_type`; if it cannot be surfaced without widening that function, leave bare tags unrecorded (the `.Some` construction covers the spec's example). Do not change `tag_type`'s signature in this task.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lilac-typeck variant_use_is_recorded`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-typeck/src/check.rs
git commit -m "feat(typeck): record enum-variant name sites"
```

---

### Task 5: `Types: Clone` and driver analysis retention

**Files:**
- Modify: `crates/lilac-ty/src/lib.rs`
- Modify: `crates/lilac-driver/src/lib.rs`
- Test: `crates/lilac-driver/src/lib.rs` (the existing `mod tests` at line 326)

**Interfaces:**
- Produces: `lilac_ty::Types` is `Clone`.
- Produces: `lilac_driver::Analysis { pub project: ResolvedProject, pub types: Types, pub sigs: Sigs, pub hir: HirProgram }` (`#[derive(Clone, Debug)]`).
- Produces: `Session.analysis: Option<Analysis>` and `Session::analysis(&self) -> Option<&Analysis>`.

- [ ] **Step 1: Write the failing test**

In the driver's `mod tests`:

```rust
#[test]
fn analysis_is_retained_on_a_type_error() {
    let mut session = Session::new();
    let result = session.compile_lir(
        "bad.lilac",
        "pi: @bit32 = 0x2A\nmain: function () nothing {\n    y: @bit8 = pi\n}\n",
    );
    assert!(result.is_err(), "the body has a type error");
    let analysis = session.analysis().expect("partial analysis retained");
    assert!(
        !analysis.hir.uses.is_empty(),
        "uses must survive the error: {:?}",
        analysis.hir.uses
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-driver analysis_is_retained_on_a_type_error`
Expected: FAIL — no method `analysis`.

- [ ] **Step 3: Implement**

In `crates/lilac-ty/src/lib.rs`, add `Clone` to the `Types` derive (line 82).

In `crates/lilac-driver/src/lib.rs`:
- Add the `Analysis` struct (imports: `lilac_resolve::ResolvedProject`, `lilac_ty::Types`, `lilac_typeck::Sigs`, `lilac_hir::HirProgram`).
- Add `pub analysis: Option<Analysis>,` to `Session`, `analysis: None,` to `Default`, and:

```rust
/// Retained analysis from the last compile (populated best-effort).
#[must_use]
pub fn analysis(&self) -> Option<&Analysis> {
    self.analysis.as_ref()
}
```

- In `finish_pipeline`, after `collect_sigs` (line 259) set:

```rust
self.analysis = Some(Analysis {
    project: project.clone(),
    types: types.clone(),
    sigs: sigs.clone(),
    hir: lilac_hir::HirProgram::default(),
});
```

- After `check_bodies` (line 267) refresh it with the checked HIR:

```rust
self.analysis = Some(Analysis {
    project: project.clone(),
    types: types.clone(),
    sigs: sigs.clone(),
    hir: hir.clone(),
});
```

These two points are hit on the error paths (the early returns follow them), so a broken project still retains resolution + uses.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lilac-driver analysis_is_retained_on_a_type_error`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-ty/src/lib.rs crates/lilac-driver/src/lib.rs
git commit -m "feat(driver): retain the last compile's analysis"
```

---

### Task 6: `convert::byte_offset`

**Files:**
- Modify: `crates/lilac-lsp/src/convert.rs`
- Test: `crates/lilac-lsp/src/convert.rs` (the existing `mod tests`)

**Interfaces:**
- Produces: `convert::byte_offset(src: &str, position: Position) -> Option<usize>` — the inverse of `position`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn byte_offset_inverts_position() {
    let src = "abc\ndef\n";
    for byte in 0..=src.len() {
        if !src.is_char_boundary(byte) {
            continue;
        }
        assert_eq!(byte_offset(src, position(src, byte)), Some(byte), "byte {byte}");
    }
}

#[test]
fn byte_offset_clamps_and_rejects_unknown_lines() {
    assert_eq!(byte_offset("é x", Position::new(0, 2)), Some(3));
    assert_eq!(byte_offset("😀x", Position::new(0, 2)), Some(4));
    assert_eq!(byte_offset("abc\n", Position::new(0, 99)), Some(3));
    assert_eq!(byte_offset("abc\n", Position::new(5, 0)), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-lsp --lib byte_offset`
Expected: FAIL — `byte_offset` not found.

- [ ] **Step 3: Implement**

In `convert.rs`:

```rust
/// Byte offset for a 0-based line and UTF-16-code-unit column.
///
/// Returns `None` when the line does not exist; a column past the line end
/// clamps to the line end.
#[must_use]
pub fn byte_offset(src: &str, position: Position) -> Option<usize> {
    let mut line_start = 0usize;
    let mut line = 0u32;
    while line < position.line {
        let newline = src[line_start..].find('\n')?;
        line_start += newline + 1;
        line += 1;
    }
    let line_end = src[line_start..]
        .find('\n')
        .map_or(src.len(), |offset| line_start + offset);
    let mut units = 0u32;
    for (offset, ch) in src[line_start..line_end].char_indices() {
        if units >= position.character {
            return Some(line_start + offset);
        }
        units += u32::try_from(ch.len_utf16()).unwrap_or(1);
    }
    Some(line_end)
}
```

(Import `Position` is already present.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lilac-lsp --lib byte_offset`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-lsp/src/convert.rs
git commit -m "feat(lsp): add byte_offset position conversion"
```

---

### Task 7: LSP analysis index

**Files:**
- Create: `crates/lilac-lsp/src/index.rs`
- Modify: `crates/lilac-lsp/src/lib.rs` (add `pub mod index;`)
- Test: `crates/lilac-lsp/src/index.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Session::analysis()` (Task 5), `NameSite`/`HirProgram` (Task 1).
- Produces: `index::DefInfo { pub name: String, pub qualified: String, pub kind: &'static str, pub span: Span }`; `index::AnalysisIndex { pub defs: HashMap<DefId, DefInfo>, pub uses: Vec<NameSite>, pub exprs: Vec<(Span, TypeRef)> }`; `index::Retained { pub session: Session, pub index: AnalysisIndex, pub file_uris: HashMap<FileId, Url>, pub uri_files: HashMap<Url, FileId> }`; `index::Retained::build(session: Session, file_uris: HashMap<FileId, Url>, uri_files: HashMap<Url, FileId>) -> Retained`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn retained(src: &str) -> Retained {
        let mut session = lilac_driver::Session::new();
        let _ = session.compile_lir("test.lilac", src);
        let uri = Url::parse("file:///c:/test.lilac").expect("uri");
        Retained::build(
            session,
            HashMap::from([(FileId(0), uri.clone())]),
            HashMap::from([(uri, FileId(0))]),
        )
    }

    #[test]
    fn index_has_defs_uses_and_exprs() {
        let retained = retained(
            "pi: @bit32 = 0x2A\nmain: function () nothing {\n    x: @bit32 = pi\n}\n",
        );
        assert!(retained.index.defs.values().any(|d| d.name == "pi" && d.kind == "const"));
        assert!(retained.index.defs.values().any(|d| d.name == "main" && d.kind == "function"));
        assert!(!retained.index.uses.is_empty());
        assert!(!retained.index.exprs.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-lsp --lib index_has_defs_uses_and_exprs`
Expected: FAIL — module `index` does not exist.

- [ ] **Step 3: Implement**

Create `crates/lilac-lsp/src/index.rs`:
- `DefInfo`/`AnalysisIndex`/`Retained` as above.
- `Retained::build`: take `session`; if `session.analysis()` is `Some(analysis)`, build:
  - `defs` by iterating `analysis.project.all_items()` and recursing into each `ResKind::Object { members, .. }` (each member is a `ResItem`), plus every `ResItem` in `analysis.project.hooks`. `kind` maps `ResKind` to `"function"`/`"hook"`/`"object"`/`"enum"`/`"const"`/`"field"`; `qualified` is the item's namespace path (`analysis.project.namespace_path(item.ns)`) joined with the name (root namespace adds no prefix); `span` is `item.span`.
  - `uses` from `analysis.hir.uses`, deduped by `(span.lo.0, span.hi.0, span.file, def.0)` and sorted by `(span.file.0, span.lo.0)`.
  - `exprs` by recursively walking every `HirExpr` in `analysis.hir.funcs[*].body` and `analysis.hir.consts[*].value`, collecting `(expr.span, expr.ty)`.
- If `analysis()` is `None`, all three collections are empty.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lilac-lsp --lib index_has_defs_uses_and_exprs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-lsp/src/index.rs crates/lilac-lsp/src/lib.rs
git commit -m "feat(lsp): build an analysis index from the retained session"
```

---

### Task 8: Retain the compile and serve hover

**Files:**
- Modify: `crates/lilac-lsp/src/server.rs`
- Modify: `crates/lilac-lsp/src/lib.rs` (capability)
- Create: `crates/lilac-lsp/src/handlers.rs`
- Modify: `crates/lilac-lsp/src/lib.rs` (add `pub mod handlers;`)
- Test: `crates/lilac-lsp/tests/hover.rs`

**Interfaces:**
- Consumes: `index::Retained` (Task 7), `convert::byte_offset` (Task 6).
- Produces: `handlers::hover(retained: &Retained, uri: &Url, position: Position) -> Option<Hover>`.

- [ ] **Step 1: Write the failing test**

Create `crates/lilac-lsp/tests/hover.rs` reusing the harness shape from `tests/diagnostics.rs` (`start_server`, `initialize`, `did_open`, `shutdown`, `publish_for`). Add a request helper and the test:

```rust
fn hover_at(client: &Connection, uri: &Url, line: u32, character: u32) -> Option<Hover> {
    let id = RequestId::from(50);
    let params = serde_json::json!({
        "textDocument": { "uri": uri.as_str() },
        "position": { "line": line, "character": character },
    });
    client.sender.send(Message::Request(Request {
        id: id.clone(), method: "textDocument/hover".to_owned(), params,
    })).expect("hover");
    let response = read_response(&client, &id);
    response.response_result.ok().flatten().map(|value| serde_json::from_value(value).expect("hover"))
}

#[test]
fn hover_on_a_call_shows_the_signature() {
    let (client, handle) = start_server();
    initialize(&client, None);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "foo: function (value: @bit8) result: @bit32 {\n    result = 0x1\n    return\n}\n\nmain: function () nothing {\n    x: @bit32 = foo(0x1)\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    let line = 6; // "    x: @bit32 = foo(0x1)"
    let character = 17; // inside "foo"
    let hover = hover_at(&client, &uri, line, character).expect("hover present");
    let contents = format!("{:?}", hover.contents);
    assert!(contents.contains("foo"), "{contents}");
    assert!(contents.contains("@bit32"), "{contents}");

    shutdown(&client);
    handle.join().expect("server thread");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-lsp --test hover hover_on_a_call_shows_the_signature`
Expected: FAIL — no hover response.

- [ ] **Step 3: Implement**

In `lib.rs`, add to the capabilities literal (line 25):
```rust
hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
```

In `server.rs`:
- Add fields `project: Option<Retained>` and `per_file: HashMap<Url, Retained>`; initialize them empty in `new`.
- In `publish`, build `Retained::build(session, file_uris, uri_files)` for the project (moving the `Session` into it after diagnostics are sent) and store it in `self.project`; in single-file mode build one per open document and store in `self.per_file`.
- In `handle_request`, add a branch for `"textDocument/hover"`:

```rust
} else if request.method == "textDocument/hover" {
    let params: lsp_types::HoverParams =
        serde_json::from_value(request.params).unwrap_or_else(|_| /* reply null */);
    // look up retained by params.text_document_position_params.text_document.uri,
    // compute position -> byte_offset, call handlers::hover, reply with the value
}
```

On any parse/lookup failure, reply `Response::new_ok(request.id, serde_json::Value::Null)`.

Create `crates/lilac-lsp/src/handlers.rs` with:

```rust
pub fn hover(retained: &Retained, uri: &Url, position: Position) -> Option<Hover>
```

Implementation: map `uri` to a `FileId` via `retained.uri_files`; get the file text from `retained.session.source_map`; compute the byte via `convert::byte_offset`; find the smallest `NameSite` (by length, tie-break earliest `lo`) whose span contains the byte; build Markdown:
- function/hook → `function <qualified>(<params>) -> <returns>` from `retained.session.analysis()`'s `sigs` rendered with `Types::display_ref`;
- object/enum → `object <qualified>` / `enum <qualified>`;
- otherwise → `<kind> <qualified>` plus a type line from the smallest `exprs` span containing the byte.
If no name site, use the smallest `exprs` span → its type. Return `Hover { contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value }), range: Some(convert::span_range(...)) }`; `None` when nothing matches. Make `convert::span_range` `pub(crate)`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lilac-lsp --test hover`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-lsp/src/server.rs crates/lilac-lsp/src/lib.rs crates/lilac-lsp/src/handlers.rs crates/lilac-lsp/tests/hover.rs
git commit -m "feat(lsp): serve textDocument/hover from the retained analysis"
```

---

### Task 9: Whitespace hover and the parse-failure path

**Files:**
- Test: `crates/lilac-lsp/tests/hover.rs`

**Interfaces:**
- Consumes: `handlers::hover` (Task 8).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn hover_on_whitespace_is_null() {
    let (client, handle) = start_server();
    initialize(&client, None);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "main: function () nothing {\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    assert!(hover_at(&client, &uri, 1, 0).is_none());

    shutdown(&client);
    handle.join().expect("server thread");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-lsp --test hover hover_on_whitespace_is_null`
Expected: PASS is acceptable here (this is a guard); if it already passes, keep it as the regression guard and proceed. If it panics or returns `Some`, fix `handlers::hover` to return `None`.

- [ ] **Step 3: Implement**

Only if Step 2 failed: ensure `handlers::hover` returns `None` when no name site or expression span contains the byte, and that `byte_offset` returning `None` short-circuits to `None`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lilac-lsp --test hover`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-lsp/tests/hover.rs
git commit -m "test(lsp): hover on whitespace returns null"
```

---

### Task 10: Serve go-to-definition

**Files:**
- Modify: `crates/lilac-lsp/src/handlers.rs`
- Modify: `crates/lilac-lsp/src/server.rs`
- Modify: `crates/lilac-lsp/src/lib.rs` (capability)
- Test: `crates/lilac-lsp/tests/definition.rs`

**Interfaces:**
- Consumes: `index::Retained` (Task 7), `convert::byte_offset`/`span_range` (Task 6, Task 8).
- Produces: `handlers::definition(retained: &Retained, uri: &Url, position: Position) -> Option<GotoDefinitionResponse>`.

- [ ] **Step 1: Write the failing test**

Create `crates/lilac-lsp/tests/definition.rs` with the same harness. Add:

```rust
fn definition_at(client: &Connection, uri: &Url, line: u32, character: u32) -> Option<Location> {
    let id = RequestId::from(60);
    let params = serde_json::json!({
        "textDocument": { "uri": uri.as_str() },
        "position": { "line": line, "character": character },
    });
    client.sender.send(Message::Request(Request {
        id: id.clone(), method: "textDocument/definition".to_owned(), params,
    })).expect("definition");
    let response = read_response(client, &id);
    let value = response.response_result.ok().flatten()?;
    serde_json::from_value::<GotoDefinitionResponse>(value).ok().and_then(|r| match r {
        GotoDefinitionResponse::Scalar(location) => Some(location),
        GotoDefinitionResponse::Array(mut locations) => locations.pop(),
        GotoDefinitionResponse::Link(_) => None,
    })
}

#[test]
fn definition_jumps_to_the_callee_declaration() {
    let (client, handle) = start_server();
    initialize(&client, None);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "foo: function () nothing {\n}\nmain: function () nothing {\n    foo()\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    let location = definition_at(&client, &uri, 3, 5).expect("definition");
    assert_eq!(location.uri, uri);
    assert_eq!(location.range.start.line, 0);

    shutdown(&client);
    handle.join().expect("server thread");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-lsp --test definition definition_jumps_to_the_callee_declaration`
Expected: FAIL — no definition response.

- [ ] **Step 3: Implement**

In `lib.rs`, add `definition_provider: Some(lsp_types::OneOf::Left(true)),` to the capabilities.

In `server.rs`, add a `"textDocument/definition"` branch mirroring hover.

In `handlers.rs`, add:

```rust
pub fn definition(retained: &Retained, uri: &Url, position: Position) -> Option<GotoDefinitionResponse>
```

Implementation: map uri→`FileId`, compute the byte, find the smallest `NameSite` containing it; look up `def` in `retained.index.defs`; return `GotoDefinitionResponse::Scalar(Location { uri: retained.file_uris[&def.span.file], range: convert::span_range(&retained.session.source_map, def.span)? })`. Return `None` when no site, `def == DefId::ERROR`, or the target file has no URI.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lilac-lsp --test definition definition_jumps_to_the_callee_declaration`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-lsp/src/handlers.rs crates/lilac-lsp/src/server.rs crates/lilac-lsp/src/lib.rs crates/lilac-lsp/tests/definition.rs
git commit -m "feat(lsp): serve textDocument/definition"
```

---

### Task 11: Cross-file definition and best-effort on errors

**Files:**
- Test: `crates/lilac-lsp/tests/definition.rs`

**Interfaces:**
- Consumes: `handlers::definition` (Task 10).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn definition_crosses_files() {
    // project root with two files; main calls a function declared in lib
    let dir = std::env::temp_dir().join(format!("lilac-lsp-def-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("lib.lilac"), "helper: function () nothing {\n}\n").expect("write lib");
    let main = dir.join("main.lilac");
    let main_text = "main: function () nothing {\n    helper()\n}\n";
    std::fs::write(&main, main_text).expect("write main");
    let root = Url::from_directory_path(&dir).expect("root uri");
    let main_uri = Url::from_file_path(&main).expect("uri");

    let (client, handle) = start_server();
    initialize(&client, Some(&root));
    did_open(&client, &main_uri, main_text);
    let _ = publish_for(&client, &main_uri);

    let location = definition_at(&client, &main_uri, 1, 5).expect("definition");
    assert!(location.uri.path().ends_with("lib.lilac"), "{}", location.uri);
    assert_eq!(location.range.start.line, 0);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn definition_survives_a_type_error() {
    let (client, handle) = start_server();
    initialize(&client, None);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "pi: @bit32 = 0x2A\nmain: function () nothing {\n    y: @bit8 = pi\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    let location = definition_at(&client, &uri, 2, 15).expect("definition");
    assert_eq!(location.range.start.line, 0);

    shutdown(&client);
    handle.join().expect("server thread");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lilac-lsp --test definition`
Expected: FAIL for at least `definition_crosses_files` (the URI/range must come from the other file).

- [ ] **Step 3: Implement**

Only if a test failed: ensure the project-mode `Retained` maps every project file to a URI (from `session.project_files()`), and that `definition` uses `retained.file_uris[&def.span.file]` rather than the request URI.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p lilac-lsp --test definition`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-lsp/tests/definition.rs
git commit -m "test(lsp): cross-file and error-resilient definition"
```

---

### Task 12: Full gates

**Files:** none (verification only).

- [ ] **Step 1: Run the gates**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo clippy --release -p lilac-vm --all-targets --features bench-baseline -- -D warnings
cargo test --release -p lilac-vm --features bench-baseline
```

Expected: all exit 0. Snapshots unchanged.

- [ ] **Step 2: Run the editor gates**

Run: `npm --prefix editors/vscode test; npm --prefix editors/vscode run package`
Expected: both succeed.

- [ ] **Step 3: Commit any formatting fixes**

```bash
git add -A
git commit -m "chore: formatting and lint fixes for hover/definition"
```

(If there is nothing to commit, skip.)

---

## Self-Review

- **Spec coverage:** §3.1 `NameSite` (Task 1); §3.2 checker recording (Tasks 1–4); §3.3 `Types: Clone` (Task 5); §3.4 `Analysis`/`Session.analysis` (Task 5); §4 analysis index (Task 7); §5 hover/definition semantics (Tasks 8, 10); §6 error handling (Tasks 5, 9, 11); §7 capabilities + `byte_offset` (Tasks 6, 8, 10); §8 testing (each task); §9 gates (Task 12).
- **Type consistency:** `NameSite` (Tasks 1–4, 7), `Analysis`/`analysis()` (Tasks 5, 7), `Retained`/`AnalysisIndex`/`DefInfo` (Tasks 7–11), `byte_offset` (Tasks 6, 8, 10), `handlers::hover`/`handlers::definition` (Tasks 8, 10).
- **Review Focus:** whitespace/between tokens (Tasks 9, 10), past-line-end (Task 6), cross-file (Task 11), UTF-16 offset (Task 6), broken project (Tasks 5, 11).
