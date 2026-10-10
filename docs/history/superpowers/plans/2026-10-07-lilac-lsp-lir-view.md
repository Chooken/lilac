# Live LIR View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a live, whole-module Lilac IR view to the editor with bidirectional source↔LIR provenance.

**Architecture:** A single `lilac-ir` traversal renders canonical module text plus a per-line span map; the LSP retains every pipeline stage and serves them over a `lilac/lir` request plus a `lilac/lirInvalidated` notification; a VSCode virtual document renders the text and uses native APIs for hover-highlight, click-jump, and source→LIR peek.

**Tech Stack:** Rust (std-only compiler crates), `lsp-server`/`lsp-types`/`serde_json` in `lilac-lsp`, VSCode extension in plain JS (`vscode-languageclient`).

**Spec:** `docs/superpowers/specs/2026-10-07-lilac-lsp-lir-view-design.md`

## Global Constraints

- Compiler crates stay std-only; `lilac-lsp` remains the **only** crate permitted `lsp-server`/`lsp-types`/serde (see `AGENTS.md`).
- `LirModule::to_text()` output stays **byte-identical**; all existing `to_text()` snapshots must pass unchanged.
- `unsafe_code = "forbid"` and `clippy::pedantic = deny` hold for all our crates; no `unwrap`/`expect`/`panic!` on user input in production code.
- Snapshot/source files are LF, no BOM.
- Run before each commit: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`.
- Extension: `npm test` and `npm run package` run in `editors/vscode/`.
- Stage vocabulary is exactly `lower`, `fold`, `runtime`, `final` (the driver's existing stage names).

## Review Focus

Inputs/conditions the spec implies but that are easy to get wrong — each gets a test in the owning task:

1. A module file **other than** the one the view was opened from — cross-file jump needs the `files` map to carry every `FileId`.
2. **Multibyte UTF-8** source — position↔byte conversion must be exact (Lilac spans are UTF-8 bytes, VSCode positions are UTF-16).
3. A project whose compile **stopped after `lower`** — `lilac/lir` must return `null` for unreached stages, never panic.
4. `Span::dummy()` lines (injected runtime) — must produce no highlight and no link.
5. Header/label/layout lines with **no span** — hover must report "compiler-generated", not crash or mis-map.

---

### Task 1: Spanned IR renderer (`lilac-ir`)

**Files:**
- Modify: `crates/lilac-ir/src/lib.rs` (the `to_text` path ~407–666 and its unit tests)

**Interfaces:**
- Consumes: `LirModule`, `LirData`, `LirFunc`, `LirBlock`, `LirInst`, `LirTerm`, `Span`.
- Produces:
  - `pub struct SpannedText { pub text: String, pub lines: Vec<Option<Span>> }`
  - `impl LirModule { pub fn to_text_spanned(&self) -> SpannedText }`
  - private `fn inst_span(inst: &LirInst) -> Span`, `fn term_span(term: &LirTerm) -> Span`

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` in `crates/lilac-ir/src/lib.rs`:

```rust
#[test]
fn spanned_text_matches_to_text_byte_for_byte() {
    let mut module = canonical_module();
    module.layouts.insert(
        "#Number".to_owned(),
        vec![("tag".to_owned(), "#bit8".to_owned())],
    );
    module.data.push(LirData {
        name: "c0".to_owned(),
        kind: LirDataKind::Str("hi".to_owned()),
        span: test_span(),
    });
    let spanned = module.to_text_spanned();
    assert_eq!(spanned.text, module.to_text());
    assert_eq!(spanned.lines.len(), spanned.text.lines().count());
}

#[test]
fn spanned_instruction_and_terminator_lines_carry_spans() {
    let module = canonical_module();
    let spanned = module.to_text_spanned();
    let lines: Vec<&str> = spanned.text.lines().collect();
    let inst_line = lines
        .iter()
        .position(|l| l.contains("= copy %in"))
        .expect("instruction line");
    let term_line = lines
        .iter()
        .position(|l| l.trim() == "return")
        .expect("terminator line");
    assert_eq!(spanned.lines[inst_line], Some(test_span()));
    assert_eq!(spanned.lines[term_line], Some(test_span()));
}

#[test]
fn spanned_header_and_layout_lines_have_no_span() {
    let mut module = canonical_module();
    module
        .layouts
        .insert("#Number".to_owned(), vec![("tag".to_owned(), "#bit8".to_owned())]);
    let spanned = module.to_text_spanned();
    let lines: Vec<&str> = spanned.text.lines().collect();
    for needle in ["lilac_ir =", "target =", "function @", "  in ", "type #Number", "{", "}"] {
        if let Some(i) = lines.iter().position(|l| l.contains(needle)) {
            assert_eq!(spanned.lines[i], None, "line `{needle}` should have no span");
        }
    }
}

#[test]
fn spanned_extern_only_module_is_consistent() {
    let mut module = LirModule::default();
    module.funcs.push(LirFunc {
        name: "puts".to_owned(),
        ins: vec![("s".to_owned(), "#bit8".to_owned())],
        outs: vec![],
        is_extern: true,
        blocks: vec![],
    });
    let spanned = module.to_text_spanned();
    assert_eq!(spanned.text, module.to_text());
    assert_eq!(spanned.lines.len(), spanned.text.lines().count());
    assert!(spanned.lines.iter().all(Option::is_none));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lilac-ir spanned -- --nocapture`
Expected: FAIL — `to_text_spanned` not found.

- [ ] **Step 3: Refactor the printer to one line/span traversal**

Change the private printers to emit `Vec<(String, Option<Span>)>` (one entry per physical line, no trailing newline), and add the public API:

- `print_data(data) -> Vec<(String, Option<Span>)>` (one line, `Some(data.span)`).
- `print_layout(name, members) -> Vec<(String, Option<Span>)>` (all `None`).
- `print_func(func) -> Vec<(String, Option<Span>)>`; header/`in`/`out`/`{`/`}` lines `None`; block-label line `Some(block.span)`; instruction line `Some(inst_span(inst))`; terminator line `Some(term_span(term))`.
- `fn inst_span(inst: &LirInst) -> Span` and `fn term_span(term: &LirTerm) -> Span`: match each variant and return its `span` field; for `LirInst::CompileError { call_span, .. }` return `call_span`.
- `to_text()` becomes: collect `module_lines()` (header as two `None` lines, then data, layouts sorted by name, funcs), join the texts with `"\n"`, append a trailing `"\n"`. Output must be byte-identical to today.
- `to_text_spanned()` returns `SpannedText { text: <same join>, lines }`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lilac-ir`
Expected: PASS, including every pre-existing `to_text` snapshot test.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-ir/src/lib.rs
git commit -m "feat(ir): render module text with per-line source spans"
```

---

### Task 2: Retain IR stages in the LSP (`lilac-lsp`)

**Files:**
- Modify: `crates/lilac-lsp/Cargo.toml`
- Modify: `crates/lilac-lsp/src/index.rs`

**Interfaces:**
- Consumes: `lilac_ir::LirModule`.
- Produces:
  - `pub struct LirStages { pub modules: Vec<(&'static str, LirModule)> }` (derive `Clone, Debug, Default`)
  - `pub struct Retained { … , pub stages: LirStages }`
  - `pub fn Retained::build_with_stages(session, file_uris, uri_files, stages: LirStages) -> Self`
  - `Retained::build(...)` unchanged in signature; delegates with `LirStages::default()`.

- [ ] **Step 1: Add the dependency**

In `crates/lilac-lsp/Cargo.toml` under `[dependencies]`, add:

```toml
lilac-ir = { path = "../lilac-ir" }
```

- [ ] **Step 2: Write the failing test**

Add to `index.rs`'s `#[cfg(test)] mod tests`:

```rust
#[test]
fn retained_carries_captured_stages() {
    let mut session = lilac_driver::Session::new();
    let mut stages = LirStages::default();
    let _ = session.compile_lir_with_stages("t.lilac", "main: function () nothing {\n}\n", &mut |name, lir| {
        stages.modules.push((name, lir.clone()));
    });
    let uri = Url::parse("file:///c:/t.lilac").expect("uri");
    let retained = Retained::build_with_stages(
        session,
        HashMap::from([(FileId(0), uri.clone())]),
        HashMap::from([(uri, FileId(0))]),
        stages,
    );
    let names: Vec<&str> = retained.stages.modules.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, vec!["lower", "fold", "runtime", "final"]);
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p lilac-lsp retained_carries_captured_stages`
Expected: FAIL — `LirStages`/`build_with_stages` not found.

- [ ] **Step 4: Implement**

Add `use lilac_ir::LirModule;`, the `LirStages` struct, the `stages` field on `Retained`, and `build_with_stages`. Reimplement `build` as `Self::build_with_stages(session, file_uris, uri_files, LirStages::default())`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p lilac-lsp`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/lilac-lsp/Cargo.toml crates/lilac-lsp/src/index.rs
git commit -m "feat(lsp): retain captured IR stages per compile"
```

---

### Task 3: `lilac/lir` payload builder (`lilac-lsp`)

**Files:**
- Create: `crates/lilac-lsp/src/lir.rs`
- Modify: `crates/lilac-lsp/src/lib.rs` (add `pub mod lir;`)

**Interfaces:**
- Consumes: `Retained` (Task 2), `LirModule::to_text_spanned` (Task 1), `Retained::file_uris`.
- Produces: `pub fn lir_payload(retained: &Retained, uri: &Url, stage: &str) -> Option<serde_json::Value>`; result keys `stage`, `text`, `spans` (`[[line, fileId, lo, hi], …]`, only non-dummy spans), `files` (`{ "<fileId>": "<uri>" }`).

- [ ] **Step 1: Write the failing test**

In `crates/lilac-lsp/src/lir.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{LirStages, Retained};
    use lilac_span::FileId;
    use lsp_types::Url;
    use std::collections::HashMap;

    #[test]
    fn payload_reports_stage_text_spans_and_files() {
        let mut session = lilac_driver::Session::new();
        let mut stages = LirStages::default();
        let _ = session.compile_lir_with_stages(
            "t.lilac",
            "main: function () nothing {\n}\n",
            &mut |name, lir| stages.modules.push((name, lir.clone())),
        );
        let uri = Url::parse("file:///c:/t.lilac").expect("uri");
        let other = Url::parse("file:///c:/other.lilac").expect("uri");
        // Two files: `files` must carry every FileId, not just the opened one.
        let retained = Retained::build_with_stages(
            session,
            HashMap::from([(FileId(0), uri.clone()), (FileId(1), other.clone())]),
            HashMap::from([(uri.clone(), FileId(0)), (other.clone(), FileId(1))]),
            stages,
        );

        let value = lir_payload(&retained, &uri, "final").expect("payload");
        assert_eq!(value["stage"], "final");
        assert!(value["text"].as_str().expect("text").contains("function @main"));
        assert_eq!(value["files"]["0"], uri.as_str());
        assert_eq!(value["files"]["1"], other.as_str());
        let spans = value["spans"].as_array().expect("spans");
        assert!(!spans.is_empty());
        assert_eq!(spans[0].as_array().expect("row").len(), 4);
    }

    #[test]
    fn payload_is_none_for_an_unknown_stage() {
        let retained = Retained::build(
            lilac_driver::Session::new(),
            HashMap::new(),
            HashMap::new(),
        );
        let uri = Url::parse("file:///c:/t.lilac").expect("uri");
        assert!(lir_payload(&retained, &uri, "nope").is_none());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p lilac-lsp lir::`
Expected: FAIL — `lir` module/`lir_payload` not found.

- [ ] **Step 3: Implement**

`lir_payload`: find `(stage, module)` in `retained.stages.modules` by name; if absent return `None`. Call `module.to_text_spanned()`. Build `spans` from `spanned.lines.iter().enumerate()` where `Some(span)` and `!span.is_dummy()` (a dummy span has `file == FileId(u32::MAX)`), pushing `[line, span.file.0, span.lo.0, span.hi.0]`. Build `files` from `retained.file_uris` as `{ id.0.to_string(): uri.as_str() }`. Return `serde_json::json!({ "stage": stage, "text": spanned.text, "spans": spans, "files": files })`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lilac-lsp lir::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-lsp/src/lir.rs crates/lilac-lsp/src/lib.rs
git commit -m "feat(lsp): build lilac/lir stage payloads"
```

---

### Task 4: Capture stages and serve `lilac/lir` (`lilac-lsp`)

**Files:**
- Modify: `crates/lilac-lsp/src/server.rs`
- Create: `crates/lilac-lsp/tests/lir.rs`

**Interfaces:**
- Consumes: `lir::lir_payload` (Task 3), `Retained::build_with_stages` (Task 2), driver `compile_project_with_stages` / `compile_lir_with_stages`.
- Produces: the `lilac/lir` request arm in `Server::handle_request`; `Server::publish` now populates `Retained.stages`.

- [ ] **Step 1: Write the failing test**

Create `crates/lilac-lsp/tests/lir.rs` (copy the `start_server`/`initialize`/`shutdown`/`did_open`/`publish_for`/`read_response` helpers from `tests/hover.rs`, plus this):

```rust
fn lir_request(client: &Connection, uri: &Url, stage: &str) -> serde_json::Value {
    let id = RequestId::from(60);
    let params = serde_json::json!({ "uri": uri.as_str(), "stage": stage });
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: "lilac/lir".to_owned(),
            params,
        }))
        .expect("lilac/lir");
    read_response(client, &id).response_result.expect("result")
}

#[test]
fn lir_request_returns_the_final_module() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    did_open(
        &client,
        &uri,
        "main: function () nothing {\n    x: @bit_native = @i_add(0x1, 0x2)\n}\n",
    );
    let _ = publish_for(&client, &uri);

    let value = lir_request(&client, &uri, "final");
    assert_eq!(value["stage"], "final");
    assert!(value["text"].as_str().expect("text").contains("function @main"));
    let spans = value["spans"].as_array().expect("spans");
    assert!(!spans.is_empty());
    assert!(
        spans.iter().all(|row| row[1].as_u64() != Some(u32::MAX as u64)),
        "dummy (compiler-generated runtime) spans must be omitted"
    );

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn lir_request_is_null_for_an_unreached_stage() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "boom: function () nothing {\n    @compile_error(\"live\")\n}\n\nmain: function () nothing {\n    boom()\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    assert!(lir_request(&client, &uri, "final").is_null());
    assert!(!lir_request(&client, &uri, "lower").is_null());

    shutdown(&client);
    handle.join().expect("server thread");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lilac-lsp --test lir`
Expected: FAIL — `lilac/lir` returns a `MethodNotFound` error (result is null / error).

- [ ] **Step 3: Implement**

In `Server::publish`, switch the two compile calls to capture stages:

```rust
let mut stages = LirStages::default();
let _ = session.compile_project_with_stages(&root, &mut fs, &mut |name, lir| {
    stages.modules.push((name, lir.clone()));
});
```
(and, in single-file mode, `compile_lir_with_stages(&name, &document.text, &mut |name, lir| { stages.modules.push((name, lir.clone())); })`). Build `Retained` with `Retained::build_with_stages(session, file_uris, uri_files, stages)`.

Add a `lilac/lir` arm to `handle_request` and a `fn lir_response(&self, request: &Request) -> serde_json::Value` that deserializes `{ uri: Url, stage: String }`, looks up `Retained` (`self.project` or `self.per_file.get(&uri)`), and returns `lir::lir_payload(retained, &uri, &stage).unwrap_or(serde_json::Value::Null)`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lilac-lsp --test lir`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-lsp/src/server.rs crates/lilac-lsp/tests/lir.rs
git commit -m "feat(lsp): serve lilac/lir from captured stages"
```

---

### Task 5: `lilac/lirInvalidated` notification (`lilac-lsp`)

**Files:**
- Modify: `crates/lilac-lsp/src/server.rs`
- Modify: `crates/lilac-lsp/tests/lir.rs`

**Interfaces:**
- Produces: a `lilac/lirInvalidated` notification (no params) sent after each successful publish.

- [ ] **Step 1: Write the failing test**

Add to `tests/lir.rs`:

```rust
fn next_invalidation(client: &Connection) {
    loop {
        match client.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Message::Notification(n)) if n.method == "lilac/lirInvalidated" => return,
            Ok(_) => {}
            Err(err) => panic!("no lilac/lirInvalidated: {err}"),
        }
    }
}

#[test]
fn an_edit_sends_lir_invalidated() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    did_open(&client, &uri, "main: function () nothing {\n}\n");
    let _ = publish_for(&client, &uri);

    did_change(&client, &uri, "main: function () nothing {\n    x: @bit32 = 0x1\n}\n");
    next_invalidation(&client);

    shutdown(&client);
    handle.join().expect("server thread");
}
```

Add a `did_change` helper (full sync) modeled on `did_open` using `DidChangeTextDocumentParams`.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p lilac-lsp --test lir an_edit_sends_lir_invalidated`
Expected: FAIL — times out waiting for `lilac/lirInvalidated`.

- [ ] **Step 3: Implement**

At the end of `Server::publish` (after diagnostics are sent, inside the `generation == self.generation` guard), send a notification with method `"lilac/lirInvalidated"` and `params: serde_json::Value::Null` via `self.connection.sender`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lilac-lsp --test lir`
Expected: PASS (all three).

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-lsp/src/server.rs crates/lilac-lsp/tests/lir.rs
git commit -m "feat(lsp): notify clients when the IR view is stale"
```

---

### Task 6: Pure LIR mapping module (`editors/vscode`)

**Files:**
- Create: `editors/vscode/lirMap.js`
- Create: `editors/vscode/test/lirMap.test.js`
- Modify: `editors/vscode/package.json` (test script)

**Interfaces:**
- Produces (CommonJS): `positionToByte(text, position) -> number`, `byteToPosition(text, byte) -> { line, character }`, `forward(spans, line) -> { file, lo, hi } | null`, `reverse(spans, file, byte) -> number[]` (ascending lines). `spans` rows are `[line, fileId, lo, hi]`.

- [ ] **Step 1: Write the failing tests**

In `editors/vscode/test/lirMap.test.js`:

```js
const test = require("node:test");
const assert = require("node:assert");
const { positionToByte, byteToPosition, forward, reverse } = require("../lirMap.js");

test("positionToByte counts UTF-8 bytes, not UTF-16 units", () => {
  const text = "ab\né!\n";
  assert.strictEqual(positionToByte(text, { line: 1, character: 0 }), 3);
  assert.strictEqual(positionToByte(text, { line: 1, character: 1 }), 5); // é is 2 bytes
});

test("byteToPosition inverts positionToByte", () => {
  const text = "ab\né!\n";
  for (const p of [{ line: 0, character: 0 }, { line: 1, character: 1 }, { line: 2, character: 0 }]) {
    const byte = positionToByte(text, p);
    assert.deepStrictEqual(byteToPosition(text, byte), p);
  }
});

test("forward finds the span for a line", () => {
  const spans = [[12, 0, 40, 44], [13, 1, 5, 9]];
  assert.deepStrictEqual(forward(spans, 12), { file: 0, lo: 40, hi: 44 });
  assert.strictEqual(forward(spans, 99), null);
});

test("reverse returns all lines containing a byte", () => {
  const spans = [[12, 0, 40, 44], [13, 0, 40, 50], [14, 1, 0, 3]];
  assert.deepStrictEqual(reverse(spans, 0, 41), [12, 13]);
  assert.deepStrictEqual(reverse(spans, 0, 3), []);
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run (in `editors/vscode/`): `node --test test/lirMap.test.js`
Expected: FAIL — cannot find module `../lirMap.js`.

- [ ] **Step 3: Implement `lirMap.js`**

Use `TextEncoder` for UTF-8 byte counts. `positionToByte`: sum the UTF-8 byte length of each preceding line plus one newline per line, then add the UTF-8 byte length of the target line's first `character` UTF-16 units. `byteToPosition`: walk lines accumulating byte lengths until the target byte falls in a line, then decode that line prefix up to the byte. `forward`/`reverse` filter the span rows as specified.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `node --test test/lirMap.test.js`
Expected: PASS (all four).

- [ ] **Step 5: Wire into `npm test`**

In `editors/vscode/package.json`, set:

```json
"test": "node --test test/lirMap.test.js && node scripts/run-grammar-tests.mjs \"test/*.test\""
```

- [ ] **Step 6: Run the full extension tests**

Run: `npm test`
Expected: PASS (JS unit tests then grammar tests).

- [ ] **Step 7: Commit**

```bash
git add editors/vscode/lirMap.js editors/vscode/test/lirMap.test.js editors/vscode/package.json
git commit -m "feat(vscode): add pure LIR line/span mapping helpers"
```

---

### Task 7: LIR virtual document + open command (`editors/vscode`)

**Files:**
- Modify: `editors/vscode/extension.js`
- Modify: `editors/vscode/package.json` (activation events + commands)

**Interfaces:**
- Consumes: `client.sendRequest('lilac/lir', { uri, stage })`, `client.onNotification('lilac/lirInvalidated', …)`.
- Produces: scheme `lilac-lir`; commands `lilac.showLir`, `lilac.selectLirStage` (the latter implemented in Task 9); the virtual doc refreshes on invalidation.

- [ ] **Step 1: Implement the content provider and command**

- Register a `TextDocumentContentProvider` for `lilac-lir` whose `provideTextDocumentContent(uri)` parses `stage` from the query and calls `client.sendRequest("lilac/lir", { uri: <source>, stage })`, returning `result ? result.text : "// IR unavailable"`.
- Register command `lilac.showLir`: for the active `.lilac` editor, build `lilac-lir:/<source-path>?stage=final`, `openTextDocument`, then `showTextDocument(doc, { viewColumn: ViewColumn.Beside, preview: false })`, and `languages.setTextDocumentLanguage(doc, "lilac-ir")`.
- Keep a map `sourceUri -> EventEmitter`; on `lilac/lirInvalidated`, fire `onDidChange` for each open `lilac-lir` doc.

- [ ] **Step 2: Update `package.json`**

Add to `activationEvents`: `"onCommand:lilac.showLir"`, `"onCommand:lilac.selectLirStage"`. Add `contributes.commands` with titles `Lilac: Show IR` and `Lilac: Select IR Stage`.

- [ ] **Step 3: Verify manually**

Run `npm run package` (must succeed). Then, in an Extension Development Host with `lilac-lsp` on PATH: open a `.lilac` project, run `Lilac: Show IR`, and confirm a read-only `.lir` document opens beside the source showing `function @main`. Edit the source and confirm the IR text updates after the debounce.

- [ ] **Step 4: Commit**

```bash
git add editors/vscode/extension.js editors/vscode/package.json
git commit -m "feat(vscode): open a live LIR virtual document"
```

---

### Task 8: Forward link — hover highlight + click jump (`editors/vscode`)

**Files:**
- Modify: `editors/vscode/extension.js`

**Interfaces:**
- Consumes: `forward`/`byteToPosition` (Task 6), the `lilac/lir` payload's `spans`/`files` (Task 4).
- Produces: a hover provider + document link provider for scheme `lilac-lir`; a decoration type; decoration lifecycle.

- [ ] **Step 1: Implement**

- Cache the latest `lilac/lir` payload per open LIR document (from the provider fetch).
- `languages.registerHoverProvider({ scheme: "lilac-lir" }, …)`: map `position.line` via `forward(spans, line)`; if absent, return a hover saying "compiler-generated (no source)"; otherwise resolve `files[file]`, compute the range from `lo`/`hi` via `byteToPosition` on that document's text, return a `Hover` with the snippet + `file:line`, and apply `decorationType` to that range in the source editor.
- `languages.registerDocumentLinkProvider({ scheme: "lilac-lir" }, …)`: for each spanned line, a `DocumentLink` whose target is the source URI; `resolveDocumentLink`/command opens the source with `{ selection: range }` and `revealRange(range, InCenter)`.
- Clear the decoration on `window.onDidChangeTextEditorSelection`, `window.onDidChangeActiveTextEditor`, and a short `setTimeout` after each hover.

- [ ] **Step 2: Verify manually**

In the dev host: hovering an LIR instruction highlights the matching source range and shows `file:line`; Ctrl+Click jumps to it; moving the cursor clears the highlight. Hovering a runtime line shows "compiler-generated".

- [ ] **Step 3: Commit**

```bash
git add editors/vscode/extension.js
git commit -m "feat(vscode): highlight and jump to source from an IR line"
```

---

### Task 9: Stage selector (`editors/vscode`)

**Files:**
- Modify: `editors/vscode/extension.js`

**Interfaces:**
- Consumes: `lilac/lir` returning `null` for absent stages.
- Produces: command `lilac.selectLirStage` (registered in Task 7).

- [ ] **Step 1: Implement**

On `lilac.selectLirStage`, show a `QuickPick` of `lower`/`fold`/`runtime`/`final`; for the current source, probe each stage with `lilac/lir` and mark `null` results as `{ description: "unavailable" }` and skip them on pick. On pick, re-open the `lilac-lir` URI with the new `stage` query.

- [ ] **Step 2: Verify manually**

In the dev host: the picker lists four stages; with a clean project all are selectable; with a project broken by a live `@compile_error`, `final` is shown unavailable and switching to `lower` shows pre-fold IR.

- [ ] **Step 3: Commit**

```bash
git add editors/vscode/extension.js
git commit -m "feat(vscode): select the IR pipeline stage"
```

---

### Task 10: Reverse link — source→LIR peek (`editors/vscode`)

**Files:**
- Modify: `editors/vscode/extension.js`

**Interfaces:**
- Consumes: `reverse`/`positionToByte` (Task 6), the cached `lilac/lir` payload (Task 8).

- [ ] **Step 1: Implement**

`languages.registerHoverProvider({ language: "lilac" }, …)`: if an LIR payload is cached for this source (or fetch one at the selected stage), compute the cursor byte via `positionToByte(document.getText(), position)`, run `reverse(spans, fileId, byte)` for this document's `FileId` (resolve via the payload's `files` map), and if any lines match, return a hover with a fenced `lilac` block of those LIR lines (read from the payload `text`). Return `undefined` when nothing matches.

- [ ] **Step 2: Verify manually**

In the dev host: hovering a call in the source shows the LIR lines it lowered to; hovering whitespace shows nothing.

- [ ] **Step 3: Commit**

```bash
git add editors/vscode/extension.js
git commit -m "feat(vscode): peek emitted IR from a source expression"
```

---

### Task 11: Docs, version, and full gates

**Files:**
- Modify: `editors/vscode/CHANGELOG.md`
- Modify: `editors/vscode/package.json` (version)
- Modify: `AGENTS.md` (one line)

- [ ] **Step 1: Update docs**

Bump `editors/vscode/package.json` `version` to `0.3.0` and add a `0.3.0` entry to `CHANGELOG.md` describing the live IR view, stage selection, and hover/peek provenance. Add to `AGENTS.md` under the language-server section: the `lilac/lir` request and `lilac/lirInvalidated` notification are the IR-view protocol; `lilac-ir` is now also a `lilac-lsp` dependency.

- [ ] **Step 2: Run the full gates**

Run: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; `cargo clippy --release -p lilac-vm --all-targets --features bench-baseline -- -D warnings`; `cargo test --release -p lilac-vm --features bench-baseline`.
Expected: all pass.

- [ ] **Step 3: Run the extension gates**

Run (in `editors/vscode/`): `npm test`; `npm run package`.
Expected: both succeed; a `lilac-0.3.0.vsix` is produced.

- [ ] **Step 4: Commit**

```bash
git add editors/vscode/CHANGELOG.md editors/vscode/package.json AGENTS.md
git commit -m "docs(lsp): document the live IR view and bump the extension"
```
