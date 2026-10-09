# Language Server Plan

> Status: in progress (2026-10-09). Phases 1 and 1b done. Phase 1 has one
> documented limitation; `E051` keeps its single-chain note (a recursion
> depth error has one chain worth showing). Phase 1b step 5 needed no
> change: `Diags` already drops identical diagnostics.
>
> Goal: finish `lilac-lsp` as a daily editor: errors show up where the user
> caused them, navigation and completion work, and edits stay responsive.
> `SPEC.md` stays normative; language rule changes (1b, `E037`) need spec
> edits.

## Decisions recorded

| Topic | Decision |
|---|---|
| Trace locations in the editor | **Every frame** of a diagnostic's trace gets an Error at its call site, not just the origin or a "user code" boundary (a `@compile_error` in the user's own library would otherwise only show at its definition). One diagnostic per (site, origin); each links back to the origin via `relatedInformation`. |
| Which call sites | **Every** call site that reaches the error, not only the shortest chain, in the **CLI too**. Still one diagnostic per error source. |
| CLI trace shape | An inverted caller tree under `note: reached from:`; each function's callers are expanded once and a repeat prints `[see above]` (linear in call edges, terminates on recursion). Hidden `.lilac.*` frames are spliced out. |
| Constant out-of-bounds `@array_at` | **Hard error `E037`, always**: reported whether or not the function is reachable (no silent bugs in unused code). A non-constant index stays a runtime panic. |
| `@compile_error` (`E031`) | Stays **live-only** (it is deliberate, e.g. unsupported-target arms). |
| Unused generics | Type-checked in every compile with abstract `T`s; diagnostics that depend on `T` are dropped, the rest are kept (1b). |
| Future | A `release-safe` mode that keeps runtime bounds checks in release while `release` drops them. Not part of this plan. |

## Phase 1: Trace-aware diagnostics

1. **Structured trace.** `lilac_error::Diagnostic` gains a `trace` (a tree
   of frames: function display name, call-site span, address-taken flag).
   The human emitter renders it as the `reached from:` caller tree; the JSON
   emitter as a nested `trace` array.
2. **All incoming edges.** `lilac_ir::Reach` keeps every incoming edge of a
   reached function, not only the first. `reach_chain_text` becomes a
   trace builder walking callers from the error function. Used by `E031`
   and `E035`.
3. **Generic instance origins.** Typeck records every origin of an instance
   (`Instance::origin` becomes a list), so `E042` and `E051` carry the same
   tree back to user code. `no_cascade_single_e042` stays one diagnostic,
   now listing both sites.
4. **`E037` constant out-of-bounds index.** `lower_array_at` emits a marker
   when the index folds to a constant `>= N`. It is checked right after
   lowering, *before* DCE, so every lowered function reports it; the trace
   comes from the full call graph (unreachable callers included). Constant
   conditions still lower only the live arm, so `if N > 3 { @array_at(ref
   a, 3) }` is fine. Add `docs/errors/E037.md` and a `SPEC.md` rule.
5. **LSP fan-out** (`convert.rs`, `server::retain`). The origin diagnostic
   gets a `relatedInformation` entry per frame; each frame's call site gets
   a diagnostic in its own file (`E031: @compile_error reached through this
   call: <msg>`) linking back to the origin. Deduplicated per (site,
   origin); the `covered` bookkeeping applies per target file.
6. **Tests.** Update CLI chain expectations (`m2.rs`); UI tests for several
   call sites, recursion, and `E037` in a function nothing calls; a run test
   showing a constant-guarded index still builds; LSP tests that the
   caller's file receives the frame diagnostic.

Known limitation (documented, not fixed here): `E031`/`E035`/`E028` run
after lowering, which only runs when the unit type-checks, so any type error
hides them.

## Phase 1b: Best-effort checking of unused generics

`check_templates` already checks every uninstantiated generic with abstract
`TypeKind::Param` arguments, but only for the editor, and discards every
diagnostic.

1. **Filter, don't discard.** In template mode `emit_requirement` drops a
   diagnostic only when a type it involves `contains_param`; everything else
   is kept (a concrete `@bit8` mismatch inside a generic is a real error).
2. **Run it in every compile** (`check`, `build`, LSP). Template HIR stays
   out of lowering, so generated code is unchanged. A generic that is
   instantiated skips the template check, so nothing is reported twice.
3. **Find the leaks.** Abstract `T` reaching a non-requirement error path
   (`@size_of(T)`, the array element hook check `E010`, literal inference
   into `T`, calls to other generics with `T`) is tagged as a requirement.
   Gate: `lib/Standard` and every run test produce zero template
   diagnostics.
4. **`E037` in templates.** The constant folder is in lowering, which never
   sees templates, so typeck checks a literal or `const` index against the
   array's literal length (known even when the element is `T`).
5. Check whether a non-dependent error in a generic instantiated twice is
   reported once per instance; deduplicate if so.

This is a language change: the CLI rejects programs with broken unused
generics that it accepts today. Record it in `SPEC.md`.

## Phase 2: Diagnostics robustness

- Spanless diagnostics (`E025`, ...) go to `lilac.json` or the root
  `main.lilac`, not "the first open document" in `HashMap` order.
- Publish the document `version` with diagnostics.
- Deadline-based debounce: `recv_timeout` restarts on every message, so a
  stream of hover/token requests can delay recompiles indefinitely.
- `workspace/didChangeWatchedFiles` for `.lilac` and `lilac.json` (new or
  deleted files, `git checkout`); `workspace/didChangeConfiguration` for
  `lilac.libraries` without a restart.

## Phase 3: Navigation

- Find references and document highlight (index `uses` + `defs`).
- Document symbols (outline, breadcrumbs) and workspace symbols.
- Definition and references for locals and parameters: typeck records a
  local's declaration span (`SemanticSite` has no link today).
- Rename: items, then locals; refuse library and builtin definitions
  (`prepareRename`).
- Go to type definition.
- Richer hover: object fields, enum variants, const values, `@` intrinsic
  signatures.

## Phase 4: Editing assistance

- Completion: names in scope (`FileScope`), namespace members after `A.`,
  fields and methods after `x.` (expression types), intrinsics after `@`,
  keywords. Depends on parser recovery mid-edit; the largest item.
- Signature help from `sigs`.
- Quick fixes from `help:` suggestions (did-you-mean). Needs structured
  suggestions (span + replacement) on `Diagnostic`.

## Phase 5: Performance

- Compile on a worker thread; answer requests from the last retained
  compile meanwhile and drop stale compiles by generation.
- Then cache unchanged files' parses and mounted libraries' analysis.

## Optional

- A VS Code `problemMatcher` that turns a runtime `panic:` trace from a
  `lilac build` + run task into diagnostics on each `at fn (file:line:col)`.
