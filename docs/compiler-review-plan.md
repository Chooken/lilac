# Compiler Review Fix Plan

> Status: in progress (Phases 0–8 done). Source: the codebase review of
> 2026-10-09.
> Goal: the compiler never crashes or silently hides an error, its internal
> state is easy to follow, and the editor gets unobtrusive hints, without
> adding noise to the build log.
>
> Decisions made with the owner:
> - **No console warnings.** Lint-like feedback is editor-only: faded hints
>   (LSP `Hint` severity, `Unnecessary` tag) that never reach the build log
>   and never fail a build. Discarded return values are never flagged.
> - **No compile cache for now.** It returns when compile times get slow;
>   until then the LIR codec and `--cache-dir` are deleted.
> - **`lilac.json` stays LSP workspace settings.** A Zig-style build system is
>   planned later; nothing here makes the CLI read `lilac.json` or auto-mount
>   `Standard`.

## Ground rules

- Every phase ends with the `AGENTS.md` gates green.
- Every bug fix lands with a test that reproduces it first.
- No phase changes generated code: the `.lir`/`.ll` snapshots must stay
  byte-identical throughout. A snapshot change is a bug, not something to
  re-bless.
- `AGENTS.md` is updated in the phase that changes what it describes.

Order: 0 → 1 → 2 → 3 → 4 → 5, then 6, 7 and 8 in any order. Phase 1 is the
most urgent; 2 must precede 3 (both touch instance state).

---

## Phase 0: Cleanup

Small, and it shrinks what later phases have to touch.

- **0.1 Delete the LIR codec.** Remove `crates/lilac-ir/src/codec.rs`, the
  `CodecError`/`LIR_CODEC_VERSION` re-exports, and the round-trip test in
  `crates/lilac-cli/tests/m5a.rs`. Git keeps it for when a cache is designed.
- **0.2 Delete `--cache-dir`.** Remove `Session::cache_dir`
  (`crates/lilac-driver/src/lib.rs`), its parsing and usage text in
  `crates/lilac-cli/src/main.rs`, and the two CLI tests for it.
- **0.3 Delete unused legacy fixtures.** The 21 `tests/*.lilac-ir` files and
  the `tests/FAILS_assignment`, `tests/FAILS_assignment_2` folders are
  referenced by no test. Re-check with a grep before deleting; keep every
  `tests/*.lilac` that `legacy()` or `ui()` names.
- **0.4 Archive finished plans.** Move `RUST_REWRITE_PLAN.md`,
  `docs/build-command-plan.md`, `docs/compiler-fix-plan.md`,
  `docs/lsp-plan.md` and `docs/superpowers/` to `docs/history/`, and fix the
  references (`IR_SPEC.md` points at `RUST_REWRITE_PLAN.md`). `SPEC.md`,
  `IR_SPEC.md`, `STDLIB.md` and `AGENTS.md` remain the source of truth.

**Exit:** gates green; `rg -i "codec|cache_dir|cache-dir" crates` finds
nothing.

---

## Phase 1: Never overflow the stack

**Why:** 30–40 nested parentheses overflow the stack in a debug build (about
300 in release); nested `if`s overflow at about 100 (debug) and 1,000
(release). The parser alone overflows: each parenthesis level is 11
recursive calls (`parse_expr` → `parse_or` → … → `parse_primary`,
`crates/lilac-parse/src/lib.rs:1460`) on a 208-byte `Expr`. A stack overflow
aborts the process, so it also kills the language server; `catch_unwind`
cannot catch it.

- **1.1 Big compile stacks.** Run the compile on a thread with a 64 MiB stack:
  in the CLI (`main` spawns one thread running the command and joins it) and
  in the LSP worker (`.stack_size(...)` on the builder in
  `crates/lilac-lsp/src/compile.rs`). The stack is reserved virtual memory,
  so this costs nothing until used.
- **1.2 Nesting limit.** The parser counts nesting depth (expressions,
  blocks, statements that open blocks, types) and past a limit emits `E001`
  "expression nested too deeply (limit N)" at the opening token, then skips
  to the matching close. Pick N (e.g. 256) so that typeck and lowering, which
  also recurse, handle N comfortably in a **debug** build on the 1.1 stack.
- **1.3 Shrink the AST.** `Type` (168 bytes) sits inline in
  `Expr::StructLit`, `Expr::Cast` and `Expr::TypeApp`; box it there, and box
  the large `Stmt` variants (`Stmt` is 480 bytes). Remove the workspace-wide
  `large_enum_variant = "allow"` if nothing else needs it.
- **1.4 Precedence climbing (recommended).** Replace the seven binary
  precedence functions (`parse_or` … `parse_multiplicative`) with one loop
  over a binding-power table. This cuts the frames per level from 11 to
  about 4, and the operator table becomes one readable list.

**Tests:** a CLI test generating 1,000 nested parentheses and 1,000 nested
`if`s expects `E001` and no crash; a test at the limit minus one expects a
clean check (run in the debug test build); an LSP test opens the 1,000-deep
file and the server still answers a hover afterwards.

---

## Phase 2: Typeck diagnostics bookkeeping

**Why:** the checker uses the diagnostic sink as its own state. It decides
that an instance failed by slicing `diags.diagnostics()[mark..]`, keeps
`requirements` and `dependent` as side tables keyed by diagnostic index,
swaps in a scratch sink while it checks templates (`check_templates`), and
the 10-error cap is applied as diagnostics are emitted, so an instance whose
errors were capped looks clean. It also reads message text
(`diagnostic.msg.contains("expected ref ")`, `generics.rs:294`) and compares
types by display string (`Requirement::Conversion { from: String }`).

- **2.1 Per-instance capture.** While an instance body is checked, typeck
  diagnostics go to a checker-owned buffer of
  `Pending { diagnostic, requirement: Option<Requirement>, dependent: bool }`
  instead of `Diags`. When the body is done, one function decides what
  happens to the buffer: fold the requirements into `E042`, drop the
  dependent ones (templates), and forward the rest to `Diags`. "Did this
  instance fail" is whether the buffer holds an error. `emit_requirement` and
  `emit_involving` set the fields; `requirements`, `dependent`, the index
  arithmetic in `instance_failure`/`template_failure` and the sink swap all
  go away.
- **2.2 Typed requirements.** `Requirement::Conversion { from: TypeId }`;
  `Requirement::Mismatch { expected_ref: bool }` replaces the message-text
  check.
- **2.3 Cap errors when rendering, not when emitting.** `Diags` keeps every
  diagnostic; the human emitter and `render_summary` apply `max_errors` and
  count what they skipped. `Diags::take_since`'s cap bookkeeping disappears.
  The LSP keeps showing everything.

**Tests:** the existing `E042`/template/`E051` fixtures pass unchanged; a new
test with more than 10 unrelated errors followed by a failing generic
instance still reports the `E042` in JSON output and counts it in the
summary.

---

## Phase 3: One error must not hide others

**Why:** a function whose body has an error is marked `State::Failed`
(`crates/lilac-typeck/src/body.rs:146`), and every later `request` of it
returns `Err` silently (`lib.rs:404`). So for `broken(vlaue2, 0x2)`, where
`broken` takes one argument and has an unrelated error in its body,
neither the arity error nor the unresolved `vlaue2` is reported. Calling an
unresolved function (`undefined_fn(vlaue)`) also skips its arguments.

- **3.1 Split the failure states.** `State::{Queued, Done, BodyFailed,
  Unusable}`. `Unusable` covers what really makes a call meaningless: a
  signature type that failed (`ensure_type`), the `E051` depth cap, and the
  dropped `@entry`. `request` returns the signature for everything except
  `Unusable`. A body failure is the body's problem; callers are checked
  against the declared signature. Audit every `State::Failed` read (three
  sites today).
- **3.2 Always check arguments.** When a callee cannot be resolved (unknown
  name, unresolved receiver, failed static receiver, unusable instance),
  still check each argument with no expected type, so errors inside the
  arguments are reported. The call itself stays an error type, so nothing
  cascades from its result.
- **3.3 Audit the other silent `Err`s in `calls.rs`.** Each of the ~15
  `Err(ErrorGuaranteed::new())` returns gets a one-line comment naming the
  diagnostic that already covers it, or is fixed to emit one.

**Tests:** a UI fixture with the `broken(...)`/`undefined_fn(...)` program
expects the body error, the arity error and both unresolved names;
`FAILS_generic_no_cascade` still has exactly one `E042` with both call sites
traced.

---

## Phase 4: Make the error types mean what they say

- **4.1 Earn `ErrorGuaranteed`.** Make `ErrorGuaranteed::new` private to
  `lilac-error`. Construction goes through `Diags::emit`, or
  `Diags::already_reported(&self) -> ErrorGuaranteed`, which
  `debug_assert!`s that an error was recorded (or capped). Typeck returns
  that for poison it propagates. The `@entry` silent drop gets its own
  outcome instead of a fake error. Templates drop dependent errors on
  purpose, so the assertion is relaxed only inside template checking (after
  Phase 2 that is one explicit flag, not a sink swap).
- **4.2 Error-code registry.** `lilac_error::codes` lists every code once:
  `pub const E010: Code = Code { id: "E010", doc: include_str!(".../E010.md") }`.
  `Diagnostic::error` takes a `Code`, so a misspelled code or a missing doc
  page is a compile error. Add `docs/errors/E000.md` (lex errors) and
  `E001.md` (parse errors, including the new nesting limit).

**Tests:** the existing suite in debug (the assertion is the test); a unit
test that every registered code's doc starts with its own `# EXXX` heading.

---

## Phase 5: Language server resilience and faded hints

- **5.1 Report internal errors.** When the worker's compile panics, show
  `window/showMessage` (error) once per failing generation: "the Lilac
  compiler hit an internal error; diagnostics may be stale", including the
  panic message (captured with a panic hook into the job's result). Wrap
  `handle_request` in `catch_unwind` and answer a panicking request with an
  `InternalError` response instead of dying.
- **5.2 Faded hints (editor only).** Computed in the LSP from the retained
  analysis and published with the file's diagnostics as `Hint` +
  `DiagnosticTag::UNNECESSARY`, never by the compiler, so the CLI output is
  unchanged:
  - **Unused locals and parameters:** a `LocalSite` declaration with no
    other site. `self` is never faded.
  - **Unused aliases:** a `using … as` declaration with no `AliasSite` use.
  - **Unused private items:** a top-level `private` function, constant or
    type with no use in the index.
  - **Unreachable statements:** typeck records the span of statements that
    follow a `return`, `break`, `continue`, `@panic` or `@trap` in the same
    block (`HirProgram::unreachable`). Arms ruled out by a constant
    condition are deliberately not faded (they are platform-dependent).
  - Discarded return values are never flagged.

  An unused plain `using` needs import provenance in the resolver; it is
  left for later.

**Tests:** LSP integration tests for each hint kind, and one asserting a
compile with hints prints nothing extra from the CLI.

---

## Phase 6: Readability refactors

No behavior change; snapshots byte-identical.

- **6.1 Split large files** along the lines typeck already uses:
  - `lilac-lower/src/lib.rs` (2,800 lines) into `symbols.rs` (names, mangling,
    externs), `ownership.rs` (drops, copies, glue), `control.rs` (if, match,
    loops), `calls.rs` (calls and the extern ABI), `intrinsics.rs`. Break up
    `value` (312 lines) by expression kind.
  - `lilac-ir/src/lib.rs` into the types (`lib.rs`), `print.rs` and
    `verify.rs`.
  - `lilac-parse/src/lib.rs` into `items.rs`, `types.rs`, `stmt.rs`,
    `expr.rs`.
- **6.2 Rename lowering's `Fs`** (per-function emission state) to
  `FnState`, matching `lilac-codegen-llvm`, so it no longer reads like the
  module loader's `Fs` trait.
- **6.3 One `Using` record.** Replace `FileUnit::usings`, `using_spans` and
  `using_aliases` (parallel vectors) with `Vec<Using { path, span, alias }>`;
  the private `ScannedUsing` becomes that type.
- **6.4 Typed builtin types.** A `Builtin` enum in `lilac-ty`, interned in
  `Types::new`, so `types.builtin(Builtin::Bit8)` cannot fail and the ~50
  `builtin("@bit8")` string lookups (and their `bug!`) go away.
- **6.5 AST type shapes.** Replace the empty-name poison `Type` and the
  `name == "function"`/`"self"` checks with an enum of type shapes (named,
  function, array, `self`, error). This was item 1.9 of the previous plan.

---

## Phase 7: Typed LIR

**Why:** registers, places, labels and symbols are all `String`, so one can be
passed as another; constants are text the backend re-parses (`parse_int`,
`const_value` in `crates/lilac-codegen-llvm/src/lower.rs:1088`), data symbols
are marked by a `!` prefix, and the verifier never checks numeric literal
text, so a malformed one reaches the backend as an internal error.

- **7.1 Newtypes:** `Reg`, `PlaceName`, `Label`, `Symbol` in `lilac-ir`, used
  by `LirInst`, `LirOperand`, `LirCallee` and `LirTerm`. Each displays with
  its sigil, so the printer's text is unchanged.
- **7.2 Typed constants:** `enum LirConst { Int(u64), Float(f64 bits),
  Data(Symbol) }` replaces `Const { lit: String }`. Lowering builds the value
  once; the backend's `parse_int` and float parsing go away; the verifier
  checks the constant against its type.

**Tests:** all `.lir`/`.ll` snapshots byte-identical; verifier unit tests for
a constant of the wrong type.

**Done.** The snapshots are byte-identical except five `const` lines that
now print canonically (`0x2A` → `0x2a`, `2E10` → `20000000000.0`).

---

## Phase 8: Small comforts

- **8.1 Parse errors say what they found:** "expected `:` after declaration
  name, found `function`" (a `TokenKind` description helper; end of file
  reads "found end of file").
- **8.2 Unclosed blocks** point at the opening `{` ("this `{` is never
  closed") with the end of file as a secondary label.
- **8.3 The `E042` method note uses Lilac syntax:** `name: function (self:
  ref T, a: @bit32) @bit64`, not `fn name(...) -> R`.
- **8.4 Operators on raw bit types:** `E043` "no @add for (@bit32, @bit32)"
  gets a help line naming `Standard`'s `Int32` and the matching intrinsic
  (`@i_add_32`).
- **8.5 CLI did-you-mean:** move `closest`/`levenshtein` from
  `lilac-typeck/src/suggest.rs` into `lilac-error`, so an unknown command or
  flag says "did you mean `check`?" / "did you mean `--release`?". Print a
  one-line hint ("run `lilac help` for all options") instead of the full
  usage dump.
- **8.6 `lilac explain E0xx`** prints the doc page from the Phase 4 registry.
- **8.7 `--timings` and `-q`** are accepted by `check`, `emit-ir` and
  `emit-llvm`, not only `build`.

**Tests:** UI fixtures for 8.1–8.4; CLI tests for 8.5–8.7.

**Done.** Tests are in `crates/lilac-cli/tests/comforts.rs`. The method note
writes `self` bare, as a declaration does, and names the other parameters
`a`, `b`, ...: `other: function (self, a: @bit8) T`.

---

## Phase 9 (optional): Test layout

Rename the milestone test files (`crates/lilac-cli/tests/m0.rs` … `m6.rs`,
`m4_regressions.rs`) by area (`parse.rs`, `modules.rs`, `generics.rs`,
`externs.rs`, …), moving tests without changing them, so a test can be found
by feature.
