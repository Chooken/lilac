# Lilac `@compile_error` Reachability Trace Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** when `E031` fires, show *why* the `@compile_error` is live: the
call chain from a root (`main`, an `--export`) down to the function holding
the call, in the same `at <fn> (<file:line:col>)` format a `@panic` trace
prints.

```text
error[E031]: used hook
  --> FAILS_hook_used_compile_error.lilac:4:24
   |
 4 |         @compile_error("used hook")
   |         ---------------^^^^^^^^^^^-
   |         |              |
   |         |              message here
   |         evaluated here
   |
   = note: reached from:
             at Object.@on_drop (FAILS_hook_used_compile_error.lilac:4:9)
             at main (FAILS_hook_used_compile_error.lilac:9:5)
```

**Why:** `E031` is now a reachability rule. The message says *what* fired
but not *which path* keeps it alive. For a hook (`Box[T].@on_copy`) or a
generic body, the path is the whole answer: it names the instantiation
(`Box[Foo].@on_copy`) and the user line that copied the value. Without it the
user has to rebuild the call graph by hand.

**Architecture:** this is a compile-time trace over the LIR call graph. It
does not use the runtime shadow stack. `prune_unreachable` already does a
graph walk from the roots. We make that walk record, for each reached
function, the first caller and the call span (a BFS parent pointer). Then
`check_live_compile_errors` walks those pointers back from the failing
function and renders one note.

**Tech stack:** Rust workspace, no new dependencies.

## Design decisions

1. **Shortest path, one chain.** BFS from the roots gives each function a
   single parent edge `(caller, span)` and a shortest path. That path never
   repeats a function, so recursion cannot make it loop or blow up, and the
   output is deterministic. If there are several paths we show only one; we
   do not list them all.
2. **Frame format is `trace::site_text`.** Same line text as a panic trace
   (`  at Holder[@bit32].make (main.lilac:10:9)`). Names come from
   `lilac_lower::Lowered::names` and locations from `SourceMap::location`.
   The first frame is the `@compile_error` call itself, like the panic site.
   Each later frame is the call in that function that leads to the frame
   above it.
3. **Hide `.lilac.*` frames.** Drop/copy glue (`.lilac.drop.T`,
   `.lilac.copy.T`) and `.lilac.start` are skipped, the same set
   `trace::instrument` skips. The caller of a hidden frame keeps its own call
   span. So a hook reached through drop glue shows the user line where the
   drop happens, not the glue.
4. **Address-taken edges** (`addr.func`) count as edges, as they do in DCE.
   The frame gets the `addr.func` span. Optional: a suffix such as
   `(address taken)` so users don't look for a call that isn't there (see
   Open questions).
5. **Export roots:** the chain ends at the exported function, the same as
   it ends at `main`.
6. **Run the check before instrumentation.** `trace::instrument` adds only
   `@trace_head` builtin calls, so it does not change the graph. Running
   `check_live_compile_errors` right after prune is clearer and avoids
   instrumenting a module that is about to fail.

## Tasks

### Task 1: reachability with parent edges (`lilac-ir/src/dce.rs`)

- [ ] Add `pub struct Reach { parent: HashMap<String, Option<(String, Span)>> }`
      (`None` marks a root) and `pub fn reach(module, extra_roots) -> Option<Reach>`.
      Return `None` when there are no roots, matching today's no-op.
- [ ] Switch the walk from a `Vec` stack to a `VecDeque` (BFS). Make
      `callees` return `(target, span)` pairs from `Call`/`AddrFunc` spans.
- [ ] Add `Reach::chain(&self, func) -> Vec<(String, Option<Span>)>`: from the
      function up to its root, each entry being the frame's function and the
      span of its call toward the previous entry. The innermost frame's span
      is filled in by the caller (the `@compile_error` site).
- [ ] `prune_unreachable` becomes `reach` + `retain` + data sweep. Return the
      `Reach` so the driver can reuse it without walking the graph twice.
- [ ] Unit tests: existing DCE tests unchanged; new tests check that
      `chain` gives the shortest path when a deeper path also exists, ends at
      an extra root, and goes through an `addr.func` edge.

### Task 2: render the chain in `check_live_compile_errors` (`lilac-ir/src/lib.rs`)

- [ ] New signature:
      `check_live_compile_errors(module, reach: Option<&Reach>, sources: &SourceMap, names: &HashMap<String, String>, diags)`.
- [ ] For each `CompileError` in function `f`: build the frames from
      `reach.chain(f)`, set the innermost span to `call_span`, drop
      `.lilac.*` frames, format each with `trace::site_text`, and attach one
      `note("reached from:\n" + lines)`. With `reach == None` (no roots,
      e.g. `check` on a library with no `main`/export) add no note.
- [ ] Factor out the `.lilac.*` / extern skip predicate so `trace::instrument`
      and this function share it (`trace::is_traced(func)`).
- [ ] Check that `human.rs` renders a multi-line note with aligned
      continuation lines. `draw_footer_line` already handles continuation
      lines; trim the two-space indent from `site_text` if it doubles up.

### Task 3: driver wiring (`lilac-driver/src/lib.rs`)

- [ ] In `Session::run`: `let reach = lilac_ir::prune_unreachable(&mut lir, &roots);`
      then call `check_live_compile_errors(&lir, reach.as_ref(), &self.source_map, &lowered.names, &mut self.diags)?`
      **before** `trace::instrument`. Update the stage doc comment and the
      module doc (prune → `@compile_error` check → trace → verify).
- [ ] Driver unit tests: extend `reachable_compile_error_is_e031` to check
      that the rendered text has `at boom (` and `at main (`.
      `exported_compile_error_is_e031` must end at `at api (`.

### Task 4: LSP

- [ ] `lilac-lsp/src/convert.rs` already maps notes into the message. Check
      that the multi-line note reads well in VS Code hover. Optional
      follow-up: emit each frame as `relatedInformation` with a `Location`
      so the frames are clickable. That needs structured frames, not one
      string; see Open questions.

### Task 5: tests

- [ ] `crates/lilac-cli/tests/m2.rs`: `FAILS_hook_used_compile_error` also
      asserts `at Object.@on_drop (` and `at main (`. This covers the
      hidden-glue rule. Pin the exact `main` line/col after checking which
      span lowering gives the scope-end drop.
- [ ] New UI case `tests/ui/FAILS_compile_error_generic_chain.lilac`: a
      generic `Holder[T].make` that hits `@compile_error`, reached through
      a helper from `main`. Assert the instantiated name
      `Holder[@bit32].make` and three frames.
- [ ] New UI case for the `addr.func` edge (function taken by address, never
      called).

### Task 6: docs

- [ ] `docs/errors/E031.md`: update the example output with the note and
      explain the chain (shortest path, glue hidden).
- [ ] SPEC "Constant conditions and `@compile_error`": one sentence on the
      trace.
- [ ] AGENTS.md "Panics and stack traces": the `E031` chain reuses
      `trace::site_text` and DCE's parent edges. `site_text` stays the only
      `at` formatter.

### Gates

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Status (2026-10-09)

Implemented with the default answers below: `[address taken]` suffix, no
elision, one string note. Follow-up done: drops now carry real spans (the
closing `}`, the `return`/`break`, or the statement for temporaries), so a
hook reached through a drop prints its caller's location in both `E031`
chains and `@panic` traces (`tests/run/panic_drop_*`).

## Open questions

1. **`(address taken)` suffix** on `addr.func` frames: recommended, small,
   needs `Reach` to record the edge kind.
2. **Elision:** shortest paths are short in practice, so no elision for
   now. If they get long, reuse the human emitter's `MAX_FRAMES` /
   `--verbose` rule.
3. **Structured notes:** rendering frames into one string is simplest.
   Clickable LSP frames would need a `Diagnostic` field for spanned notes
   (`note_at(span, text)`). Do that as its own change if wanted.
