# Lilac Language Server — Foundation (Diagnostics)

> Scope: sub-project 1 of the editor LSP effort. A new `crates/lilac-lsp`
> binary (`lilac-lsp`) that speaks the Language Server Protocol over stdio and
> publishes compiler diagnostics for `.lilac` projects, plus the VSCode client
> wiring to launch it. Branch `ai`. This is editor tooling; the compiler crates
> gain only additive APIs. Hover, go-to-definition, completion, references, and
> rename are deliberately deferred to later specs.

## 1. Outcome / constraints / success

**Outcome**

- Opening a `.lilac` file in VSCode (extension installed, `lilac-lsp` on PATH)
  shows compiler errors and warnings inline as you type.
- The server starts on `onLanguage:lilac`, tracks open documents, recompiles
  the project after edits, and publishes diagnostics per file.
- `cargo test -p lilac-lsp` exercises the protocol in-process; the full
  workspace gates stay green.

**Constraints**

- New crate `crates/lilac-lsp` only. Compiler crates stay std-only (no serde);
  `lilac-lsp` alone may depend on `lsp-server`, `lsp-types`, and their
  transitive serde.
- No change to existing driver entry points, rendered diagnostics, or
  snapshots. Driver/module additions are additive.
- The server writes only LSP frames to stdout; all logging goes to stderr.
- No panics on user input; `unsafe_code = "forbid"` and `clippy::pedantic`
  hold for the new crate.
- Full document sync in v1 (whole-text replacement); incremental sync is
  future work.

**Success**

- `initialize` advertises `textDocumentSync = Full` and a `serverInfo`.
- A clean project publishes empty diagnostic sets; an `E001`/`E021`/`E025`
  source publishes the matching diagnostic at the correct range.
- Editing from broken to clean clears the previously published diagnostics.
- `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo test --workspace` pass; `npm run package` in `editors/vscode`
  succeeds and ships the runtime dependency.

**Non-goals**

- Hover, go-to-definition, completion, references, rename.
- Semantic tokens, formatting, code actions, inlay hints.
- Incremental text sync, multi-root workspaces.
- Any `.lir` (IR) language-server features.
- Bundling the `lilac-lsp` binary inside the VSIX.

### 1.1 Decisions locked during brainstorming

1. Full-featured LSP is the long-term goal, built as four sub-projects:
   (1) foundation + diagnostics, (2) hover + go-to-definition,
   (3) completion, (4) references + rename. This spec is sub-project 1.
2. Protocol layer: `lsp-server` (transport) + `lsp-types` (schema).
3. Server ships as a new `crates/lilac-lsp` binary `lilac-lsp`; the CLI is
   untouched. The extension finds it via `lilac.serverPath` (default
   `lilac-lsp` on PATH).
4. Client: `vscode-languageclient` in a plain-JS `extension.js` (no TypeScript
   or bundler build step).
5. Diagnostics only in v1.

## 2. Architecture / components

```
crates/lilac-lsp/
  Cargo.toml
  src/main.rs        # stdio entry: Connection::stdio(), run(), exit code
  src/lib.rs         # run(connection): lifecycle + message loop
  src/server.rs      # Server state + compile orchestration
  src/documents.rs   # open-document store
  src/fs.rs          # overlay Fs over open docs + disk
  src/convert.rs     # Diagnostic -> lsp_types::Diagnostic; positions
  src/roots.rs       # workspace root, path/URI/FileId mapping
```

- `Cargo.toml` declares `[[bin]] name = "lilac-lsp"` and depends on
  `lilac-driver`, `lsp-server`, `lsp-types`.
- `main.rs` opens a stdio `Connection`, calls `lib::run`, and returns a
  process exit code.
- `lib.rs` exposes `pub fn run(connection: Connection) -> Result<(), Box<dyn std::error::Error>>`
  so tests can drive it with `lsp_server::Connection::memory()`.

Data flow:

```
initialize  -> capture root (first workspace folder or rootUri)
didOpen/didChange/didClose -> documents store -> mark dirty
debounce elapses -> compile -> Diags -> convert -> publishDiagnostics per URI
```

## 3. Document model & roots

- `documents: HashMap<Url, Document>` where
  `Document { text: String, version: i32, path: PathBuf }`.
- `didOpen` inserts; `didChange` (Full) replaces `text` and `version`;
  `didClose` removes (overlay falls back to disk); `didSave` is ignored.
- Root selection at `initialize`: first `workspaceFolders` entry, else
  `rootUri`, else none. Only the first folder is used (multi-root deferred).
- **Project mode** when a root directory is known:
  `Session::compile_project(root, overlay_fs)`.
- **Single-file mode** when no root is known: compile the changed document
  with `Session::compile_lir(name, text)`; its `FileId(0)` maps to that URI.
- Roots and document paths are canonicalized so the loader's joined paths
  match the overlay keys.

## 4. Overlay Fs

`fs.rs` implements `lilac_module::Fs`:

- `read(path)`: return the open document's in-memory text when `path` matches
  a document path; otherwise `std::fs::read_to_string(path)`.
- `list_dir(dir)`: `std::fs::read_dir(dir)`.
- v1 limitation: a new unsaved file that is not yet on disk is not listed by
  `list_dir`, so it is invisible to the project until saved.

## 5. Compile loop

- Recompile the whole project after any open/change/close.
- Debounce ~200 ms: the loop uses `Connection`'s crossbeam
  `receiver.recv_timeout(DEBOUNCE)`; on timeout with a dirty flag set, compile.
- A `generation: u64` increments on every document change; a compile captures
  the generation and discards its result if the generation moved.
- Compilation is synchronous and whole-project; incremental analysis is future
  work.
- After a compile, diagnostics are grouped by file and published; files that
  were dirty previously but are now clean receive an empty array (tracked in a
  `published: HashSet<Url>`).

## 6. Diagnostics mapping & positions

`convert.rs`:

- `Diagnostic` -> `lsp_types::Diagnostic`:
  - `severity`: `Error` / `Warning` from `Level`.
  - `code`: the stable code string.
  - `source`: `"lilac"`.
  - `message`: the headline; `notes` and `helps` appended as trailing lines
    (`note: ...` / `help: ...`).
  - `related_information`: every secondary (`primary == false`) label with a
    resolvable location.
  - `range`: the primary label's span; if none, the first label; if the span
    is `Span::dummy`, `0:0`.
- **Byte -> UTF-16 positions**: a `LineIndex` (line-start byte offsets) is
  built per file from the source text; a byte offset maps to
  `Position { line, character }` with 0-based line and UTF-16-code-unit
  column. (`SourceFile::line_col` is byte-based and only used for the line.)
- Diagnostics are grouped by `FileId`; each `FileId` is mapped to a URI via the
  driver-provided `FileId -> PathBuf` map (section 7).

## 7. Driver / module additions (additive)

- `lilac-module`: add `pub path: PathBuf` to `FileUnit`, set from the path the
  loader read. No existing field or ordering changes.
- `lilac-driver`: populate a `FileId -> PathBuf` map on the `Session` during
  `compile_project*` and expose it via
  `pub fn project_files(&self) -> &[(FileId, PathBuf)]`. The map is filled
  from the loader's `FileUnit`s. `compile_project`'s return type,
  `render_diagnostics`, and every snapshot stay identical.
- These are the only compiler-crate changes in this sub-project.

## 8. LSP capabilities & lifecycle

- `initialize` result: `capabilities.text_document_sync = Full`;
  `server_info = { name: "lilac-lsp", version: env!("CARGO_PKG_VERSION") }`; no
  dynamic registration.
- `initialized`, `didOpen`, `didChange`, `didClose`, `didSave` handled;
  `shutdown` replies `null`; `exit` ends the loop (exit code 0 iff `shutdown`
  was seen, else 1).
- Unknown requests -> `MethodNotFound`; unknown notifications ignored.
- `$/cancelRequest` is accepted and ignored in v1.
- No non-protocol output on stdout; all diagnostics/logs to stderr.

## 9. VSCode client

- `editors/vscode/extension.js` (CommonJS): `activate(context)` reads
  `lilac.serverPath`, builds a `LanguageClient` with
  `serverOptions: { command: serverPath, args: [], transport: TransportKind.stdio }`
  and `documentSelector: [{ language: "lilac" }]`, calls `start()`, and
  registers disposal. A change to `lilac.serverPath` restarts the client.
- `package.json`: add `"main": "./extension.js"`,
  `"activationEvents": ["onLanguage:lilac"]`,
  `contributes.configuration` (`lilac.serverPath`, string, default
  `"lilac-lsp"`), and `dependencies.vscode-languageclient`. Bump version to
  `0.2.0`.
- `CHANGELOG.md`: add a `0.2.0` entry.
- `.vscodeignore`: keep excluding `test/`, `*.vsix`, and editor cruft; ensure
  the runtime dependency is packaged (verified with `vsce ls`). If `vsce`
  cannot separate production from dev deps, bundle `extension.js` with esbuild
  as a fallback (decided during implementation, not assumed).

## 10. Testing

- `convert.rs` unit tests: ASCII and multibyte byte->UTF-16 positions;
  `Diagnostic`->LSP severity/code/range/related-information.
- Protocol tests in `crates/lilac-lsp/tests/` using
  `lsp_server::Connection::memory()`: run the server on a thread, drive
  `initialize` + `didOpen`, and assert the `publishDiagnostics` payloads for a
  clean file, `E001`, `E021`, and a `using` `E025`; assert stale diagnostics
  are cleared after an edit to clean.
- One integration test in `crates/lilac-lsp/tests/` spawning the built binary
  and exchanging LSP frames over stdio.
- Grammar tests under `editors/vscode/` are unchanged.

## 11. Packaging / gates

- Gates: `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, and the release VM bench gates
  (`cargo clippy --release -p lilac-vm --all-targets --features bench-baseline`
  / `cargo test --release -p lilac-vm --features bench-baseline`).
- `editors/vscode`: `npm test` (grammar) and `npm run package` both succeed.

## 12. Maintenance / source of truth

- The diagnostic schema is pinned by `lilac-error`; `convert.rs` is the single
  translation point.
- The `FileId -> PathBuf` map is pinned by `lilac-module`'s `FileUnit::path`.
- `AGENTS.md` gains a note: `lilac-lsp` is the sole crate permitted
  `lsp-server`/`lsp-types`/serde; the compiler crates remain std-only.

## 13. Follow-ups (separate specs)

- Sub-project 2: hover + go-to-definition.
- Sub-project 3: completion.
- Sub-project 4: references + rename.
- Later: incremental sync, multi-root, semantic tokens, `.lir` support,
  bundling the server binary.
