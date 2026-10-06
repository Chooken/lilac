# Lilac Language Server — Live LIR View

> Scope: a new LSP sub-project (parallel to the completion/references track) that
> adds a live, whole-module view of the Lilac IR with bidirectional
> source↔LIR provenance. A retained compile exposes each IR stage through a
> `lilac/lir` request; a read-only virtual document in VSCode renders it, hover
> highlights the originating source, click jumps to it, and hovering source
> peeks the LIR it lowered to. Branch `ai`. This is editor tooling; the compiler
> crates gain only additive APIs. Stepping/breakpoints, LIR editing, and
> incremental sync are deliberately out of scope.

## 1. Outcome / constraints / success

**Outcome**

- A command opens a read-only "Lilac IR" document beside the source, showing
  the **whole module** for a selectable pipeline stage (`lower` / `fold` /
  `runtime` / `final`, default `final`).
- Hovering an LIR line highlights the source range it came from and shows a
  tooltip with the source snippet and `file:line`; clicking jumps to it.
- Hovering a source expression peeks the LIR lines it lowered to.
- The view refreshes as the project is edited, without jumping the scroll.

**Constraints**

- Compiler crates gain only additive APIs. The canonical `LirModule::to_text()`
  output stays **byte-identical** (snapshots are pinned).
- `lilac-lsp` remains the only crate permitted `lsp-server`/`lsp-types`/serde;
  the extension gains no new runtime dependency.
- No panics on user input; `unsafe_code = "forbid"` and `clippy::pedantic`
  hold.
- Full document sync stays; the whole-project recompile/debounce loop is
  unchanged.

**Success**

- `lilac-ir::LirModule::to_text_spanned().text == to_text()` for every existing
  snapshot module, and the per-line spans land on the expected lines.
- With a project open, `lilac/lir` returns the module text and a span map for
  each stage; `final` is present after a clean compile, and a project that
  fails after lowering returns `lower` with later stages `null`.
- In the extension, hovering an LIR instruction highlights the correct source
  range and shows `file:line`; clicking selects it; hovering a source name
  shows the LIR lines it produced.
- The full gates pass (section 10).

**Non-goals**

- Debugging (stepping, breakpoints, variable inspection), execution.
- Editing or formatting the LIR view.
- Filtering the injected `.lilac.*` runtime, search, folding, or per-function
  scoping in v1.
- Semantic tokens or diagnostics for the LIR document.
- Incremental text sync, multi-root, and any change to `to_text()`.

### 1.1 Decisions locked during brainstorming

1. **Scope: the whole module**, including the injected `.lilac.*` runtime.
2. **Stage: selectable** across `lower`/`fold`/`runtime`/`final`, default
   `final`.
3. **Provenance UX: highlight on hover, jump on click.** Hovering an LIR line
   highlights (does not move focus to) the source range; an explicit click
   reveals and selects it.
4. **Both directions in v1**: LIR→source and source→LIR.
5. **Delivery: extension-driven virtual document (Approach A).** The server
   computes text + spans; the extension owns the cross-editor gestures because
   the highlight/reveal APIs live client-side.

## 2. Architecture / components

```
crates/lilac-ir/src/lib.rs          # + SpannedText; + LirModule::to_text_spanned;
                                    #   printer refactored to one line/span traversal
crates/lilac-lsp/src/index.rs       # + Retained.stages (captured LirModules)
crates/lilac-lsp/src/server.rs      # capture stages; + lilac/lir; + lilac/lirInvalidated
crates/lilac-lsp/src/lir.rs         # NEW: stage rendering -> protocol payload
editors/vscode/extension.js         # + lilac-lir virtual doc, hover/link/decor, peek
editors/vscode/lirMap.js            # NEW: pure line<->span<->position mapping
```

Data flow:

```
edit -> debounce -> recompile (existing loop, now compile_*_with_stages)
     -> Retained.stages captured; publishDiagnostics (existing)
     -> lilac/lirInvalidated (new)                         [server -> client]

extension opens lilac-lir:<source>?stage=<stage>
     -> request lilac/lir { uri, stage } -> { stage, text, spans, files }
     -> line<->span map drives highlight/jump (forward) and peek (reverse)
```

**Untouched:** `lilac-lower`, `lilac-typeck`, `lilac-const`, codegen, VM. The
only compiler change is in `lilac-ir`.

## 3. Compiler-side additions (`lilac-ir`)

The printer is refactored so a single traversal emits one entry per physical
output line:

```rust
/// One rendered module: canonical text plus a span for each line that has one.
#[derive(Clone, Debug)]
pub struct SpannedText {
    /// Canonical module text (byte-identical to `to_text`).
    pub text: String,
    /// One entry per line of `text`; `Some` when the line originates in source.
    pub lines: Vec<Option<Span>>,
}

impl LirModule {
    /// Like [`LirModule::to_text`], but also reports each line's source span.
    #[must_use]
    pub fn to_text_spanned(&self) -> SpannedText { /* … */ }
}
```

- Internally, `print_data`, `print_layout`, `print_func`, and the header are
  reworked to return `Vec<(String, Option<Span>)>` (one entry per line, no
  newline). `to_text()` joins those lines with `\n` and appends a trailing
  `\n`; `to_text_spanned()` returns the same join plus the span column. Because
  both come from one traversal, the text and the span map cannot drift, and the
  existing `to_text()` snapshots prove the refactor is output-preserving.
- Span assignment: instruction and terminator lines carry that node's `Span`;
  block labels carry the block's span; module/type/function headers and `in`/
  `out` lines are `None` (no origin). Runtime-injected nodes keep
  `Span::dummy()`, which the LSP treats as "no source" (section 5).
- `text.lines().count() == lines.len()` is the invariant.

## 4. LSP additions (`lilac-lsp`)

### 4.1 Retaining stages

`Retained` (`index.rs`) gains:

```rust
/// Captured IR stages from the last compile (a stage is absent if the compile
/// stopped before it).
#[derive(Clone, Debug, Default)]
pub struct LirStages {
    /// Stage name -> module, in pipeline order.
    pub modules: Vec<(&'static str, LirModule)>,
}
```

and a field `pub stages: LirStages`. A new `Retained::build_with_stages(
session, file_uris, uri_files, stages)` constructs it; the existing
`Retained::build` delegates with `LirStages::default()` so existing callers and
tests are unchanged.

`Server::publish` switches from `compile_project` to
`compile_project_with_stages` (and, in single-file mode,
`compile_lir_with_stages`), passing a closure that clones each fired stage into
a local `LirStages`. Stages are captured only when `generation` still matches,
consistent with the existing stale-compile guard. The four stage names are the
driver's existing `lower`/`fold`/`runtime`/`final`.

### 4.2 `lilac/lir` request

A new module `lir.rs` renders a stage to a protocol payload:

```rust
/// Builds the `lilac/lir` result for `uri` at `stage`, or `None` if unavailable.
pub fn lir_payload(retained: &Retained, uri: &Url, stage: &str)
    -> Option<serde_json::Value>;
```

Result JSON:

```jsonc
{
  "stage": "final",
  "text": "lilac_ir = \"0.3\"\n…",
  "spans": [[12, 0, 40, 44], …],   // [line, fileId, lo, hi], 0-based line
  "files": { "0": "file:///…/main.lilac" } // FileId -> URI (from file_uris)
}
```

- Only lines with a non-dummy span appear in `spans`; the client treats any
  other line as compiler-generated.
- `files` lets the client jump across project files.
- Unknown stage, no retained compile, or a stage the compile never reached →
  `null`.

`Server::handle_request` gains the `lilac/lir` arm, looking the `Retained` up
exactly as hover/definition do (`self.project` or `self.per_file.get(uri)`).

### 4.3 `lilac/lirInvalidated` notification

After each successful publish, the server sends a `lilac/lirInvalidated`
notification (no params). The extension refreshes the virtual document on
receipt. A dedicated notification keeps refresh decoupled from diagnostics
semantics and is trivial to assert in tests.

## 5. Semantics: line↔span mapping

**Forward (LIR→source).** Given a hovered LIR line `L`, `spans[L]` (if present)
is `(file, lo, hi)`. The extension resolves `file` to a URI via `files`, turns
`(lo, hi)` into a `Range` in that document, highlights it, and shows a tooltip
with the source snippet and `file:line`. A line with no span (or `Span::dummy`)
reports "compiler-generated (no source)" and offers no jump.

**Reverse (source→LIR).** Given a cursor byte offset in a source document, the
extension selects every entry whose `(file, lo, hi)` contains the byte and
renders those lines in the hover (or opens the view scrolled to the first).
Because the source document may be in another file of the module, the reverse
lookup filters by the source document's `FileId`.

**Position math.** Lilac spans are UTF-8 byte offsets; VSCode positions are
UTF-16. The extension converts with `TextEncoder`/`TextDecoder` (via
`lirMap.js`), matching `lilac-lsp`'s own `convert` logic.

## 6. Extension (`editors/vscode/`)

- **Virtual document.** Scheme `lilac-lir`; URI `lilac-lir:/<source-uri>?stage=
  <stage>`. A `TextDocumentContentProvider` calls `client.sendRequest('lilac/
  lir', { uri, stage })` and returns `text`. It fires `onDidChange` when
  `lilac/lirInvalidated` arrives. The document is read-only (never saved).
- **Open / stage.** Command `lilac.showLir` opens it beside the source with
  `{ viewColumn: ViewColumn.Beside, preview: false }`. A second command
  (`lilac.selectLirStage`) offers a QuickPick of the four stages, disabling
  those the last `lilac/lir` result reported absent, and re-opens with the new
  query.
- **Forward link.** A `HoverProvider` registered for `{ scheme: 'lilac-lir' }`
  returns the source snippet + `file:line` and, as a side effect, applies a
  `TextEditorDecorationType` to the source range in the adjacent editor. A
  `DocumentLinkProvider` for the same scheme makes each spanned line a link;
  following it calls `window.showTextDocument(source, { selection })` then
  `revealRange(range, InCenter)`.
- **Reverse peek.** A `HoverProvider` for `{ language: 'lilac' }` computes the
  reverse lookup and renders the emitted LIR lines (fenced `lilac` block) in
  the hover.
- **Decoration lifecycle.** VSCode has no hover-end event, so the highlight is
  cleared on `onDidChangeTextEditorSelection`, on active-editor change, and by
  a short timer after the last hover.
- **Live refresh.** On `lilac/lirInvalidated`, capture the editor's
  `visibleRanges`, update the provider, and restore the top visible line so
  typing does not scroll the view.
- **Pure mapping module.** `lirMap.js` holds the position↔byte and
  forward/reverse span logic as dependency-free functions so it is unit
  testable without a running editor.

## 7. Error handling / limitations

- `Span::dummy()` (runtime functions, some lowered nodes) → no source; the
  tooltip says so and no highlight/link is offered.
- **Partial stages.** `compile_*_with_stages` stops at the first failing pass,
  so a broken project may have `lower` but not `final`. The view shows the
  stages that exist; `lilac/lir` returns `null` for the rest and the stage
  picker disables them.
- Project mode uses the retained `file_uris`; single-file mode uses `FileId(0)`.
- A request between an edit and the debounce uses the previous compile; a line
  with no span returns no action (never panics).
- Whole-module documents can be large; the view is read-only and unpaginated.
  Filtering the runtime is deferred.

## 8. Capabilities & lifecycle

- `initialize` advertises nothing new; `lilac/lir` is a non-standard method the
  extension calls directly, and `lilac/lirInvalidated` is a server notification.
- Unknown requests stay `MethodNotFound`; unknown notifications are ignored.
- `shutdown`/`exit` behavior is unchanged.

## 9. Testing

- **`lilac-ir`:** `to_text_spanned().text` is byte-identical to `to_text()` for
  representative modules (extern, layouts, injected runtime, numeric literals,
  all terminator kinds); spans land on the expected lines; header/type/function
  lines are `None`; `lines.len()` equals the line count.
- **`lilac-lsp` (memory connection):** `lilac/lir` returns text + spans + files
  for a project at each stage; a project failing after `lower` returns `lower`
  and `null` for later stages; an unknown stage returns `null`; spans resolve to
  the correct URI and byte range; `lilac/lirInvalidated` is sent after an edit.
- **`lilac-lsp` unit:** `lir_payload` maps `FileId`s to the retained URIs and
  drops dummy spans.
- **Extension:** `lirMap.js` unit tests (both directions, ASCII and multibyte,
  clamping) run under `npm test`; hover/highlight/jump/peek are verified by
  manual smoke.

## 10. Gates

- `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, and the release VM bench gates
  (`cargo clippy --release -p lilac-vm --all-targets --features bench-baseline`
  / `cargo test --release -p lilac-vm --features bench-baseline`).
- `editors/vscode`: `npm test` and `npm run package` both succeed.

## 11. Maintenance / source of truth

- `to_text_spanned` is the single line↔span producer; `to_text` and the spanned
  text come from one traversal, so they cannot drift.
- Stage names are the driver's `lower`/`fold`/`runtime`/`final`; the LSP and the
  extension share that vocabulary.
- The protocol payload shape lives in `lilac-lsp/src/lir.rs`; the extension
  treats it as opaque data plus `lirMap.js`.
- Compiler crates stay std-only; `lilac-lsp` remains the sole crate permitted
  `lsp-server`/`lsp-types`/serde (see `AGENTS.md`).

## 12. Follow-ups (separate specs)

- Filtering the injected runtime and per-function scoping.
- A source→LIR inline "peek" panel (versus the hover peek) and jump-to-LIR.
- Semantic tokens / folding for the LIR document.
- Integrating the view with the completion/references track's UI.
