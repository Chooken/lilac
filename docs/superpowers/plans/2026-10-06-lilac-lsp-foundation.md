# Lilac Language Server — Foundation (Diagnostics) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `lilac-lsp` language server that publishes compiler diagnostics over stdio for `.lilac` projects, and wire the VSCode extension to launch it.

**Architecture:** A new `crates/lilac-lsp` binary uses `lsp-server` for transport and `lsp-types` for the protocol. It tracks open documents in memory, overlays them on disk via a `lilac_module::Fs` implementation, recompiles the project with `lilac_driver::Session`, converts `lilac_error::Diagnostic`s to LSP diagnostics, and publishes them per file. Two additive compiler changes expose a `FileId -> PathBuf` map.

**Tech Stack:** Rust 2021, `lsp-server` 0.10, `lsp-types` 0.97, `serde_json`; the existing `lilac-driver` pipeline. VSCode client in plain CommonJS with `vscode-languageclient`.

**Spec:** `docs/superpowers/specs/2026-10-06-lilac-lsp-foundation-design.md`

## Global Constraints

- Only `crates/lilac-lsp` is new. Compiler crates stay std-only (no serde); `lilac-lsp` alone may depend on `lsp-server`/`lsp-types`/`serde_json` and their transitive serde.
- Compiler changes are additive only: no existing entry point, rendered diagnostic, or snapshot may change. `lilac-module` gains one struct field; `lilac-driver` gains one field + one accessor.
- The server writes only LSP frames to stdout; all logging goes to stderr.
- No `unwrap`/`expect`/`panic!` on user input in `lilac-lsp`; `unsafe_code = "forbid"` and `clippy::pedantic` (deny) hold for the new crate.
- Full document sync in v1 (`TextDocumentSyncKind::FULL`).
- Pin `lsp-server = "0.10"`, `lsp-types = "0.97"`, `serde_json = "1"`.
- Fixed names: binary `lilac-lsp`; diagnostic `source` is `"lilac"`; config key `lilac.serverPath`; extension version `0.2.0`; server info name `"lilac-lsp"`.
- Branch `ai`. Gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo clippy --release -p lilac-vm --all-targets --features bench-baseline -- -D warnings`, `cargo test --release -p lilac-vm --features bench-baseline`; plus `npm test` and `npm run package` in `editors/vscode`.

## Review Focus

Failure modes the spec implies that a reasonable user would expect to work; each is pinned by a test in the task that owns the code (the test is named after this list):

1. **Non-ASCII text before a diagnostic span** — LSP positions are UTF-16 code units, but spans are UTF-8 byte offsets; a naive byte column misplaces every diagnostic on such a line. (Task 5.)
2. **Windows / percent-encoded URIs** — `file:///c:/My%20Folder/main.lilac` must round-trip to the loader's joined path, or the overlay never matches and edits are ignored. (Task 4.)
3. **CRLF line endings** — the line index must not drift across `\r\n`. (Task 5.)
4. **Diagnostic with no primary label / dummy span** — must not panic and must not be silently dropped. (Task 5.)
5. **A file that had diagnostics and then becomes clean** — the previously published set must be cleared. (Task 6.)

---

### Task 1: Carry the read path on `FileUnit`

**Files:**
- Modify: `crates/lilac-module/src/lib.rs` (struct `FileUnit` ~line 38; construction ~line 131)
- Modify: `crates/lilac-resolve/src/lib.rs` (test construction ~line 1072)
- Test: `crates/lilac-module/src/lib.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `lilac_module::load`.
- Produces: `lilac_module::FileUnit` gains `pub path: PathBuf` — the exact path passed to `Fs::read`.

- [ ] **Step 1: Write the failing test**

Add to `crates/lilac-module/src/lib.rs` tests:

```rust
#[test]
fn file_unit_carries_read_path() {
    let root = PathBuf::from("proj");
    let mut fs = MemFs::new();
    fs.add(&root, "main.lilac", "using util\n");
    fs.add(&root, "util/helper.lilac", "helper: function () nothing {}\n");

    let mut diags = Diags::new();
    let project = load(&root, &mut fs, &mut diags).expect("load succeeds");

    let helper = project
        .files
        .iter()
        .find(|file| file.name == "helper.lilac")
        .expect("helper loaded");
    assert_eq!(helper.path, join_rel(&root, "util/helper.lilac"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-module file_unit_carries_read_path`
Expected: FAIL — `no field 'path' on type 'FileUnit'`.

- [ ] **Step 3: Add the field**

In `FileUnit`, add `pub path: PathBuf` (document it as "path the loader read"). In `load`, set `path: path.clone()` on the constructed `FileUnit`. In the `lilac-resolve` test's `FileUnit { .. }` literal (~line 1072), add `path: PathBuf::from(spec.name)`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p lilac-module -p lilac-resolve`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-module/src/lib.rs crates/lilac-resolve/src/lib.rs
git commit -m "feat(module): carry the read path on FileUnit"
```

---

### Task 2: Expose the project `FileId -> PathBuf` map from the driver

**Files:**
- Modify: `crates/lilac-driver/src/lib.rs` (`Session` ~line 70; `Default` ~line 96; `compile_project_with_stages` ~line 167; tests module)
- Test: `crates/lilac-driver/src/lib.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `FileUnit::path` (Task 1).
- Produces: `pub fn project_files(&self) -> &[(FileId, PathBuf)]` on `Session`, populated by `compile_project*`.

- [ ] **Step 1: Write the failing test**

Add a test-local `Fs` (mirrors `lilac-module`'s `MemFs`) and the test:

```rust
struct TestFs {
    files: std::collections::HashMap<PathBuf, String>,
    dirs: std::collections::HashMap<PathBuf, Vec<PathBuf>>,
}
impl lilac_module::Fs for TestFs {
    fn read(&mut self, path: &Path) -> std::io::Result<String> {
        self.files.get(path).cloned().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no such file")
        })
    }
    fn list_dir(&mut self, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        self.dirs.get(dir).cloned().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no such directory")
        })
    }
}

#[test]
fn project_files_maps_ids_to_paths() {
    let root = PathBuf::from("proj");
    let main = root.join("main.lilac");
    let mut fs = TestFs {
        files: [(main.clone(), "main: function () nothing {\n}\n".to_owned())]
            .into_iter()
            .collect(),
        dirs: [(root.clone(), vec![main.clone()])].into_iter().collect(),
    };
    let mut session = Session::new();
    let _ = session.compile_project(&root, &mut fs);
    assert_eq!(session.project_files(), [(FileId(0), main)]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-driver project_files_maps_ids_to_paths`
Expected: FAIL — no method `project_files`.

- [ ] **Step 3: Implement**

Add `pub project_files: Vec<(FileId, PathBuf)>` to `Session`, initialise it to `Vec::new()` in the `Default` impl, clear it at the start of `compile_project_with_stages`, and after `lilac_module::load` populate it with `project.files.iter().map(|f| (f.file, f.path.clone())).collect()`. Add:

```rust
pub fn project_files(&self) -> &[(FileId, PathBuf)] {
    &self.project_files
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p lilac-driver`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-driver/src/lib.rs
git commit -m "feat(driver): expose project FileId to path map"
```

---

### Task 3: `lilac-lsp` crate, lifecycle, and initialize handshake

**Files:**
- Create: `crates/lilac-lsp/Cargo.toml`
- Create: `crates/lilac-lsp/src/main.rs`
- Create: `crates/lilac-lsp/src/lib.rs`
- Test: `crates/lilac-lsp/tests/lifecycle.rs`

**Interfaces:**
- Produces: `pub fn run(connection: lsp_server::Connection) -> Result<(), Box<dyn std::error::Error>>`.
- Produces: initialize result with `capabilities.text_document_sync == Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL))` and `server_info.name == "lilac-lsp"`.

- [ ] **Step 1: Write the crate manifest**

`crates/lilac-lsp/Cargo.toml`:

```toml
[package]
name = "lilac-lsp"
version.workspace = true
edition.workspace = true

[[bin]]
name = "lilac-lsp"
path = "src/main.rs"

[dependencies]
lilac-driver = { path = "../lilac-driver" }
lilac-error = { path = "../lilac-error" }
lilac-span = { path = "../lilac-span" }
lilac-module = { path = "../lilac-module" }
lsp-server = "0.10"
lsp-types = "0.97"
serde_json = "1"

[lints]
workspace = true
```

- [ ] **Step 2: Write the failing lifecycle test**

`crates/lilac-lsp/tests/lifecycle.rs`:

```rust
use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    InitializeParams, InitializeResult, TextDocumentSyncCapability, TextDocumentSyncKind,
};

fn read_response(client: &Connection, id: &RequestId) -> lsp_server::Response {
    loop {
        match client.receiver.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Message::Response(response)) if &response.id == id => return response,
            Ok(_) => continue,
            Err(err) => panic!("no response for {id:?}: {err}"),
        }
    }
}

#[test]
fn initializes_shuts_down_and_exits() {
    let (server, client) = Connection::memory();
    let handle = std::thread::spawn(move || lilac_lsp::run(server).expect("server runs"));

    let params = serde_json::from_value::<InitializeParams>(
        serde_json::json!({ "capabilities": {} }),
    )
    .expect("params");
    let init_id = RequestId::from(1);
    client
        .sender
        .send(Message::Request(Request {
            id: init_id.clone(),
            method: "initialize".to_owned(),
            params: serde_json::to_value(params).expect("serialize"),
        }))
        .expect("send initialize");

    let response = read_response(&client, &init_id);
    let result: InitializeResult =
        serde_json::from_value(response.result.expect("initialize result")).expect("decode");
    assert_eq!(
        result.capabilities.text_document_sync,
        Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL))
    );
    assert_eq!(result.server_info.expect("server info").name, "lilac-lsp");

    let shutdown_id = RequestId::from(2);
    client
        .sender
        .send(Message::Request(Request {
            id: shutdown_id.clone(),
            method: "shutdown".to_owned(),
            params: serde_json::Value::Null,
        }))
        .expect("send shutdown");
    assert!(read_response(&client, &shutdown_id).error.is_none());

    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_owned(),
            params: serde_json::Value::Null,
        }))
        .expect("send exit");
    handle.join().expect("server thread joins");
}

#[test]
fn exit_without_shutdown_is_an_error() {
    let (server, client) = Connection::memory();
    let handle = std::thread::spawn(move || lilac_lsp::run(server));

    let init_id = RequestId::from(1);
    client
        .sender
        .send(Message::Request(Request {
            id: init_id.clone(),
            method: "initialize".to_owned(),
            params: serde_json::json!({ "capabilities": {} }),
        }))
        .expect("send initialize");
    let _ = read_response(&client, &init_id);

    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_owned(),
            params: serde_json::Value::Null,
        }))
        .expect("send exit");
    assert!(handle.join().expect("server thread joins").is_err());
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p lilac-lsp --test lifecycle`
Expected: FAIL to compile — `lilac_lsp::run` does not exist.

- [ ] **Step 4: Implement `main.rs` and `lib.rs`**

`main.rs`: open `Connection::stdio()`, call `lilac_lsp::run(connection)`, and `std::process::exit` with 0 on `Ok`, printing the error to stderr and exiting 1 on `Err`.

`lib.rs`: `pub fn run(connection) -> Result<(), Box<dyn std::error::Error>>` that:
1. calls `connection.initialize_start()` to get `(id, params)`, decodes `InitializeParams`;
2. builds `ServerCapabilities { text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)), ..Default::default() }` and `ServerInfo { name: "lilac-lsp".into(), version: Some(env!("CARGO_PKG_VERSION").into()) }`, then `connection.initialize_finish(id, serde_json::json!({ "capabilities": capabilities, "serverInfo": server_info }))`;
3. runs a message loop: a `Request` with method `shutdown` replies `null` and records shutdown; a `Notification` with method `exit` ends the loop (returning `Ok` if `shutdown` was seen, else `Err`); any other request replies with a `MethodNotFound` error; other notifications are ignored.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p lilac-lsp --test lifecycle`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/lilac-lsp
git commit -m "feat(lsp): add the lilac-lsp binary and lifecycle"
```

---

### Task 4: Documents, roots, and the overlay filesystem

**Files:**
- Create: `crates/lilac-lsp/src/documents.rs`
- Create: `crates/lilac-lsp/src/roots.rs`
- Create: `crates/lilac-lsp/src/fs.rs`
- Modify: `crates/lilac-lsp/src/lib.rs` (add `mod documents; mod roots; mod fs;`)
- Test: `#[cfg(test)] mod tests` in each file

**Interfaces:**
- Produces: `pub struct Document { pub text: String, pub version: i32, pub path: PathBuf }`.
- Produces: `pub struct Documents` with `open(&mut self, &TextDocumentItem)`, `change(&mut self, &DidChangeTextDocumentParams)`, `close(&mut self, &Url)`, `overlay(&self) -> HashMap<PathBuf, String>`, `path_of(&self, &Url) -> Option<&Path>`.
- Produces: `pub fn root_from_params(params: &InitializeParams) -> Option<PathBuf>`.
- Produces: `pub struct OverlayFs<'a> { pub overlay: &'a HashMap<PathBuf, String> }` implementing `lilac_module::Fs`.

- [ ] **Step 1: Write the failing tests**

`documents.rs` tests: `open` a `TextDocumentItem` (`file:///proj/main.lilac`); assert `overlay()` maps its path to its text. `change` with a Full `TextDocumentContentChangeEvent` updates `overlay()` and `version`. `close` removes it.

`roots.rs` tests:
```rust
#[test]
fn workspace_folder_beats_root_uri() {
    // workspaceFolders = [file:///c:/My%20Folder], rootUri = file:///other
    // assert root_from_params == Some(PathBuf::from(r"c:\My Folder"))  (platform-normalized)
}

#[test]
fn percent_encoded_uri_round_trips() {
    // assert a file:///c:/My%20Folder/main.lilac document's path_of ends with "My Folder/main.lilac"
}
```

`fs.rs` tests: with `overlay` containing a path → `read` returns the overlay text even if a different file exists on disk; a path not in the overlay → `read` returns `Err` (NotFound) for a nonexistent path; a path that exists on disk (use `concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")`) → `read` returns its contents; `list_dir` on the crate dir returns entries including `Cargo.toml`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lilac-lsp --lib`
Expected: FAIL to compile — modules/types do not exist.

- [ ] **Step 3: Implement**

- `documents.rs`: `Documents { map: HashMap<Url, Document> }`; `open`/`change`/`close` update the map, deriving `path` with `Url::to_file_path` (skip the document if the URI has no file path); `change` uses the last `content_changes` entry's `text` (Full sync); `overlay` builds `HashMap<PathBuf, String>` from the map.
- `roots.rs`: `root_from_params` prefers the first `workspace_folders` entry, else `root_uri`, converting with `Url::to_file_path`. Reuse a `pub(crate) fn url_to_path(url: &Url) -> Option<PathBuf>`.
- `fs.rs`: `impl lilac_module::Fs for OverlayFs<'_>` — `read` checks the overlay first, else `std::fs::read_to_string`; `list_dir` uses `std::fs::read_dir`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p lilac-lsp --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-lsp/src
git commit -m "feat(lsp): track documents and overlay them on disk"
```

---

### Task 5: Diagnostic and position conversion

**Files:**
- Create: `crates/lilac-lsp/src/convert.rs`
- Modify: `crates/lilac-lsp/src/lib.rs` (add `pub mod convert;`)
- Test: `#[cfg(test)] mod tests` in `convert.rs`

**Interfaces:**
- Produces: `pub fn position(src: &str, byte: usize) -> lsp_types::Position` — 0-based line, UTF-16-code-unit column.
- Produces: `pub fn to_lsp_diagnostic(diag: &lilac_error::Diagnostic, sources: &lilac_span::SourceMap, uri_of: &dyn Fn(lilac_span::FileId) -> Option<lsp_types::Url>) -> Option<lsp_types::Diagnostic>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn position_is_utf16_on_multibyte_lines() {
    assert_eq!(position("abc\ndef\n", 4), Position::new(1, 0));
    assert_eq!(position("é x", 3), Position::new(0, 2));   // 'é' is 1 UTF-16 unit
    assert_eq!(position("😀x", 4), Position::new(0, 2));    // emoji is 2 UTF-16 units
}

#[test]
fn position_survives_crlf() {
    assert_eq!(position("a\r\nb", 3), Position::new(1, 0));
}

#[test]
fn diagnostic_maps_code_range_and_source() {
    let mut sources = SourceMap::new();
    let file = sources.add_file("main.lilac", "abc\n");
    let diag = Diagnostic::error("E001", "boom")
        .label(Span::new(0, 3, file), true, "here");
    let uri = Url::parse("file:///main.lilac").expect("uri");
    let lsp = to_lsp_diagnostic(&diag, &sources, &|id| (id == file).then(|| uri.clone()))
        .expect("mapped");
    assert_eq!(lsp.severity, Some(DiagnosticSeverity::ERROR));
    assert_eq!(lsp.code, Some(NumberOrString::String("E001".to_owned())));
    assert_eq!(lsp.source.as_deref(), Some("lilac"));
    assert_eq!(lsp.range, Range::new(Position::new(0, 0), Position::new(0, 3)));
    assert!(lsp.message.contains("boom"));
}

#[test]
fn diagnostic_without_primary_label_does_not_panic() {
    let mut sources = SourceMap::new();
    let _ = sources.add_file("main.lilac", "abc\n");
    let diag = Diagnostic::error("E000", "no location");
    let lsp = to_lsp_diagnostic(&diag, &sources, &|_| None).expect("mapped");
    assert_eq!(lsp.range, Range::new(Position::new(0, 0), Position::new(0, 0)));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lilac-lsp --lib convert`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

- `position`: walk `src` from the last `\n` before `byte` to compute the line, then count UTF-16 code units (`char::len_utf16`) in the slice up to `byte`. Clamp `byte` to `src.len()`.
- `to_lsp_diagnostic`: pick the primary label (else the first); its span's file must resolve in `sources` or return `None`. `range` = that label's span via `position` on the file's `src`, or `0:0` when the span is dummy. `message` = headline plus `\nnote: ..` / `\nhelp: ..` lines. `severity` from `Level`. `code` = `NumberOrString::String(code)`. `source = "lilac"`. `related_information` = each secondary label whose file resolves and whose `uri_of` is `Some`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p lilac-lsp --lib convert`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-lsp/src
git commit -m "feat(lsp): convert diagnostics and byte offsets to LSP"
```

---

### Task 6: Compile loop and diagnostics publishing

**Files:**
- Create: `crates/lilac-lsp/src/server.rs`
- Modify: `crates/lilac-lsp/src/lib.rs` (own a `Server`, drive the loop from it)
- Test: `crates/lilac-lsp/tests/diagnostics.rs`

**Interfaces:**
- Consumes: `Documents`, `OverlayFs`, `root_from_params`, `to_lsp_diagnostic`, `Session::compile_project`, `Session::compile_lir`, `Session::project_files`.
- Produces: `textDocument/publishDiagnostics` notifications; a file with no diagnostics is published with an empty array at least once after it was previously dirty.

- [ ] **Step 1: Write the failing tests**

`tests/diagnostics.rs` — a helper that runs a server via `Connection::memory()`, sends `initialize` (no `rootUri`/`workspaceFolders` → single-file mode) + `initialized` + `didOpen`, and collects the first `publishDiagnostics` for the URI:

```rust
fn publish_for(client: &Connection, uri: &Url) -> PublishDiagnosticsParams {
    loop {
        match client.receiver.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Message::Notification(n)) if n.method == "textDocument/publishDiagnostics" => {
                let params: PublishDiagnosticsParams = serde_json::from_value(n.params).expect("decode");
                if &params.uri == uri { return params; }
            }
            Ok(_) => continue,
            Err(err) => panic!("no diagnostics for {uri}: {err}"),
        }
    }
}
```

Tests (single-file mode; `uri = file:///main.lilac`):
- `clean_file_publishes_empty` — open `main: function () nothing {\n}\n`; assert `diagnostics.is_empty()`.
- `parse_error_is_e001` — open `main: function ( {\n}\n`; assert a diagnostic with code `"E001"`.
- `unknown_name_is_e021` — open `main: function () nothing {\n    x: @bit8 = nope\n}\n`; assert code `"E021"`.
- `stale_diagnostics_are_cleared` — open the clean text, then `didChange` to `main: function ( {\n}\n` (assert `E001`), then `didChange` back to clean (assert the next publish is empty).

Project mode test:
- `missing_using_is_e025` — create a temp dir with `main.lilac` = `"using missing\n"`, `initialize` with that dir as a workspace folder, `didOpen` it, and assert an `E025` diagnostic for its URI. Remove the temp dir at the end.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lilac-lsp --test diagnostics`
Expected: FAIL — no diagnostics are published yet.

- [ ] **Step 3: Implement `server.rs`**

`Server { connection: Connection, documents: Documents, root: Option<PathBuf>, dirty: bool, generation: u64, published: HashSet<Url>, shutdown: bool }`.
- `main_loop`: `connection.receiver.recv_timeout(Duration::from_millis(200))`. On a message, handle it (open/change/close set `dirty = true` and bump `generation`; `exit` returns; requests are answered as in Task 3). On `Timeout` with `dirty`, call `publish`.
- `publish`: build a `Session`, and for the current generation compile — project mode (`compile_project(root, &mut OverlayFs { overlay: &documents.overlay() })`) or single-file mode (`compile_lir(path, text)` for each open document). Group `session.diags.diagnostics()` by primary-file `FileId`; map each `FileId` to a URI via `session.project_files()` (project mode) or the open-document URI (single-file). Send `publishDiagnostics` per file; send an empty array for any URI in `published` that now has none; update `published`.

`lib.rs` builds `Server` after the initialize handshake (storing `root_from_params`) and calls `main_loop`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p lilac-lsp --test diagnostics`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-lsp
git commit -m "feat(lsp): compile on change and publish diagnostics"
```

---

### Task 7: Stdio integration smoke test

**Files:**
- Create: `crates/lilac-lsp/tests/stdio.rs`

**Interfaces:**
- Consumes: the `lilac-lsp` binary (`env!("CARGO_BIN_EXE_lilac-lsp")`).

- [ ] **Step 1: Write the failing test**

A helper writes a frame (`Content-Length: <n>\r\n\r\n<body>`) to the child's stdin and reads one framed message from stdout. The test spawns the binary, sends `initialize` (`{"capabilities":{}}`), asserts the response has a `result.capabilities`, sends `shutdown`, reads its response, sends `exit`, and asserts the child exits with code 0.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-lsp --test stdio`
Expected: FAIL — the binary does not frame responses yet (or the test compiles against a not-yet-working server).

- [ ] **Step 3: No production change expected**

This task should pass once Task 3 is correct. If framing is wrong, fix `lilac-lsp` (not the test): `lsp-server`'s stdio transport already emits `Content-Length` frames.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lilac-lsp --test stdio`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-lsp/tests/stdio.rs
git commit -m "test(lsp): smoke-test the stdio binary"
```

---

### Task 8: VSCode client

**Files:**
- Create: `editors/vscode/extension.js`
- Modify: `editors/vscode/package.json`
- Modify: `editors/vscode/CHANGELOG.md`
- Modify: `editors/vscode/.vscodeignore` (only if `vsce ls` shows the runtime dep missing)

**Interfaces:**
- Consumes: the `lilac-lsp` binary via `lilac.serverPath`.
- Produces: a `LanguageClient` on `documentSelector [{ language: "lilac" }]`.

- [ ] **Step 1: Write `extension.js`**

CommonJS exporting `activate(context)` and `deactivate()`. `activate` reads `vscode.workspace.getConfiguration("lilac").get("serverPath", "lilac-lsp")`, constructs `new LanguageClient("lilac", "Lilac", { command: serverPath, args: [], transport: TransportKind.stdio }, { documentSelector: [{ language: "lilac" }] })`, calls `client.start()`, pushes `client` to `context.subscriptions`, and registers `vscode.workspace.onDidChangeConfiguration` to restart the client when `lilac.serverPath` changes. `deactivate` stops the client.

- [ ] **Step 2: Update `package.json`**

Add `"main": "./extension.js"`, `"activationEvents": ["onLanguage:lilac"]`, `"contributes.configuration": { "title": "Lilac", "properties": { "lilac.serverPath": { "type": "string", "default": "lilac-lsp", "description": "Path to the lilac-lsp executable." } } }`, `"dependencies": { "vscode-languageclient": "^9.0.1" }`, and bump `"version"` to `"0.2.0"`.

- [ ] **Step 3: Update `CHANGELOG.md`**

Add a `## 0.2.0` entry: "Add a language server client that launches `lilac-lsp` and shows compiler diagnostics."

- [ ] **Step 4: Install and verify packaging**

Run: `npm install` then `npm test` then `npm run package` then `npx vsce ls`.
Expected: grammar tests pass; `vsce ls` lists `extension.js` **and** `node_modules/vscode-languageclient/...`. If the runtime dependency is absent from `vsce ls`, edit `.vscodeignore` so only dev tooling is excluded (e.g. replace `node_modules/**` with explicit `node_modules/vscode-tmgrammar-test/**` and `node_modules/@vscode/**` lines) and re-run until it is listed.

- [ ] **Step 5: Commit**

```bash
git add editors/vscode
git commit -m "feat(vscode): launch lilac-lsp and show diagnostics"
```

---

### Task 9: Document the dependency exception and run the gates

**Files:**
- Modify: `AGENTS.md`

- [ ] **Step 1: Update `AGENTS.md`**

Add a section noting that `lilac-lsp` is the only crate permitted to depend on `lsp-server`/`lsp-types`/`serde_json` (and thus serde); the compiler crates remain std-only. Note the binary name and the `lilac.serverPath` config key.

- [ ] **Step 2: Run the full gates**

Run each and confirm success:
- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo clippy --release -p lilac-vm --all-targets --features bench-baseline -- -D warnings`
- `cargo test --release -p lilac-vm --features bench-baseline`
- `npm test` and `npm run package` in `editors/vscode`

- [ ] **Step 3: Commit**

```bash
git add AGENTS.md
git commit -m "docs: note the lilac-lsp dependency exception"
```
