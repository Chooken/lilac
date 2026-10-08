# Lilac Reachability `@compile_error` and Constant Conditions Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `lilac-const` (the LIR dataflow folder and bounded call
evaluator) with two rules that fit in a spec paragraph:

1. **Reachability.** A `@compile_error` is `E031` exactly when its function
   is reachable from the roots (`main`, `--export`, `.lilac.start`) after
   monomorphization and `prune_unreachable`.
2. **Constant conditions.** An `if` (statement or value), an `and`/`or`
   operand, or a `match` whose condition or scrutinee is a *constant
   expression* (defined below) has only its live arm lowered. The dead arm never reaches LIR, so its calls
   never enter the call graph and its `@compile_error` never exists.

Everything else that `lilac-const` did (constant arithmetic, call evaluation)
is optimization. LLVM does it at O1 and above.

**Why:** today `E031` means "survived a best-effort optimizer". Fuel and depth
limits, address-taken places, unsupported operations and changes to lowering
can all change which programs compile. The two rules above don't depend on
optimizer quality or opt level, and the LSP gets the same answer as `build`.
Both stdlib uses (`Box[T].@on_copy`, `Const[T].@on_override`) only need rule
1.

**Architecture:** the constant-expression evaluator is a small structural
interpreter over **HIR** in `lilac-lower` (`constant.rs`). It runs when
lowering reaches a condition. It has no memory model, no places, no loops and
no fuel. `check_live_compile_errors` moves to `lilac-ir`. The `lilac-const`
crate and the `fold` pipeline stage are deleted.

**Tech stack:** Rust workspace, no new dependencies; `inkwell` stays confined
to `lilac-codegen-llvm`.

## `true` / `false` as inline constants

`lib/Standard/bool.lilac` declares

```lilac
true: Bool => Bool.from_int8(0x1)
from_int8: function (from: @bit8) Bool => Bool { data = from }
@conversion: function (from: Bool) to: @bit8 => from.data
```

`if true { … }` checks to the HIR condition
`Call(@conversion Bool→@bit8, [Const(true)])`. Under the rules below:

- `Const(true)` is a module constant, so evaluate its initializer;
- `Bool.from_int8(0x1)` is a `=>` call with a constant argument, so evaluate
  `Bool { data = 0x1 }` with `from = 0x1`, which is a constant aggregate;
- `@conversion(…)` is a `=>` call, so evaluate `from.data`, which is `0x1`.

The condition is constant and only the `then` arm is lowered. **Inline
constants built from `=>` functions over hook-free types fold**, so `true` and
`false` can live in the standard library.

What does *not* fold: `t: Bool = true; if t { … }`. A local is never a
constant expression. LLVM still removes the branch at O1 and above, but a
`@compile_error` in its dead arm is reachable and reports `E031`. That is the
price of a rule users can predict. See *Follow-ups*.

## Constant expressions (normative)

A HIR expression is constant when it is one of:

| Form | Value |
| --- | --- |
| `Number` | its bits at the expression's type width |
| `SizeOf(T)` | the instance's concrete size (already concrete per instance) |
| `Const(def)` | the value of its initializer, which must be constant |
| `Struct(fields)` | an aggregate, if every field is constant |
| `Variant { index, payload }` | a variant, if the payload is absent or constant |
| `Field { base, index }` | field `index` of a constant aggregate `base` |
| `Eq(a, b)` | bit equality of two constant values (`@bit8` 0/1) |
| `If { cond, then, els }` | the constant arm chosen by a constant `cond` (the other arm is not evaluated) |
| `Logic { and, lhs, rhs }` | short-circuit over constant `@bit8`s (`rhs` is evaluated only if needed) |
| `Match { scrutinee, arms, els }` | for a constant `scrutinee`, the first arm whose pattern matches (`Variant(i)` compares the variant index and binds `capture` to the payload; `Const(lit)` compares bits), else `els`. Only the chosen arm is evaluated. |
| `Call(Intrinsic)` | integer/float arithmetic, comparisons, `@negate`, `@truncate`, `@extend_zero`, bitcasts, with constant arguments |
| `Call(Func(k))` | `k` is a `=>` function (`HirFunc.inline`), every argument is constant, and `k`'s definition is not already being expanded; the value is its `out = expr` body with parameters bound to the arguments |
| `Local(p)` | **only** a parameter of a `=>` function being expanded, or a `match` capture bound during evaluation |

Anything else is not constant: locals, `RefOf`, `Deref`, `Retype`, `Str`,
`FormatArgs`, calls to block-bodied functions, function values, `@load_u8`
and other memory intrinsics, and `CompileError`.

**Why HIR, not LIR.** A `=>` body is one expression, but that expression can
contain value `if`, value `match` and `and`/`or`. In LIR those become several
blocks, branches and temporaries, which is why `=>` is an ordinary
always-inline function and LLVM does the inlining (compiler-fix-plan 3.1:
source substitution needed temp renaming, a recursion guard and
single-evaluation fixes). In HIR the same body is still an expression *tree*:
value `match` arms are expressions only (`E010` otherwise), and nothing in
the tree is a statement. Evaluating a pure tree needs none of the
substitution machinery. It walks the tree with an environment. Inlining for
code generation stays with LLVM.

Extra rules:

- **Hook-free types only.** Every value produced while evaluating (each
  aggregate, variant, argument and result) must have a type that
  `needs_drop` and `needs_copy` both reject. Otherwise skipping lowering would
  skip an observable `@on_drop`/`@on_copy`, or hide a `Box` copy's
  `@compile_error`.
- **No recursion depth or fuel.** A `=>` call whose definition is already on
  the expansion stack is not constant (the spec already says a recursive `=>`
  call is not inlined). Without loops, evaluation always terminates.
- **Arithmetic** has the same semantics as LIR (wrapping, unsigned variants,
  IEEE floats). Integer division or modulo by zero is not constant: the
  branch stays a runtime branch, and it is not an error.
- The evaluator returns `Option<Value>`. `None` means "lower as an ordinary
  runtime branch". Evaluation never reports a diagnostic.

## Decisions

1. **The evaluator lives in lowering, not typeck.** Lowering already has the
   whole `HirProgram`, `needs_drop`/`needs_copy` and layouts. Typeck still
   checks (and instantiates generics in) both arms. A duck-typed generic call
   in a dead arm therefore still has to type-check. Skipping that is a
   follow-up.
2. **The condition is not lowered at all when it is constant.** It is pure by
   construction (no locals, hook-free, no memory), so dropping it is
   unobservable.
3. **No LIR peephole.** `-O0` output keeps runtime arithmetic on constants.
   Add a trivial LIR peephole later only if debug builds measurably suffer.
   It would be purely an optimization.
4. **`E031` loses its fold-limit note.** No limit exists any more.

## Global constraints

- `unsafe_code = "forbid"` and `clippy::pedantic = deny` stay in force.
- Ownership: lowering a constant `if` must behave exactly like lowering the
  live arm as a nested block. Scopes, temps and drops are those of the live
  arm, and the dead arm's moves never happen.
- Gates before each commit: `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`.

## File map

| File | Change |
| --- | --- |
| `crates/lilac-lower/src/constant.rs` | **new**: `Value`, `ConstEval`, arithmetic helpers |
| `crates/lilac-lower/src/lib.rs` | constant `if`/value `if`/`Logic` lowering; `FuncKey → &HirFunc` map; factor the `NumOp → ArithOp` table out of `lower_intrinsic` |
| `crates/lilac-ir/src/lib.rs` (or `dce.rs`) | `check_live_compile_errors(module, diags)` (moved, no `FoldReport`) |
| `crates/lilac-driver/src/lib.rs`, `Cargo.toml` | drop the fold stage and the `lilac-const` dependency; call `lilac_ir::check_live_compile_errors` |
| `crates/lilac-cli/src/{main.rs,progress.rs}` | stage list `["lower", "runtime", "final"]`; remove `Stage::Fold` |
| `crates/lilac-lsp/src/index.rs` | stage-name test |
| `crates/lilac-lower/{Cargo.toml,src/tests.rs}` | drop the `lilac-const` dev-dependency and the `fold` call |
| `crates/lilac-const/` | **delete**; remove from the workspace `Cargo.toml` |
| `tests/ui/cfold_*`, `tests/run/*`, `crates/lilac-cli/tests/{m2,m3}.rs` | fixtures (Task 5) |
| `SPEC.md`, `IR_SPEC.md`, `docs/errors/E031.md`, `AGENTS.md` | docs (Task 6) |

## Tasks

### Task 1: Arithmetic helpers move to `lilac-lower`

- [x] Move `eval_arith`, `eval_convert`, `mask_of`, `sign_extend`,
      `float_bits`, `bits_float` from `lilac-const` into
      `crates/lilac-lower/src/constant.rs`, over a new
      `enum Value { Int(u64, u32), Float(f64, u32), Aggregate(Vec<Value>), Variant(u32, Option<Box<Value>>) }`.
      `Bool`/`Bytes`/`Unknown` go away: comparisons yield `Int(0|1, 8)`,
      and "unknown" is `None`.
- [x] Make division or modulo by zero return `None`.
- [x] Factor the `NumOp → ArithOp` closure in `lower_intrinsic` into a free
      `fn arith_op(op, float) -> Option<ArithOp>`, used by both lowering and
      the evaluator, so their semantics cannot drift.
- [x] Port the arithmetic unit tests (`eval_arith_unsigned_semantics`,
      `bitcast_reinterprets_bits_directly`).
- [x] Commit: `lower: constant-value arithmetic helpers`.

### Task 2: The constant-expression evaluator

- [x] `struct ConstEval<'a> { lower: &'a Lower<'a>, stack: Vec<DefId>, env: Vec<HashMap<LocalId, Value>> }`
      with `fn eval(&mut self, expr: &HirExpr) -> Option<Value>`,
      implementing the table above.
- [x] Build `funcs: HashMap<FuncKey, &HirFunc>` once in `lower()`. A `=>`
      body is `{ out = expr }`: extract `expr` from the single assignment to
      `returns[0]`. Any other body shape is `None`.
- [x] Apply the hook-free check to every produced value's `TypeRef`
      (`needs_drop || needs_copy` ⇒ `None`).
- [x] Unit tests (HIR built by checking small sources, as in
      `lilac-lower/src/tests.rs`):
  - literal, `@size_of`, intrinsic arithmetic and comparison;
  - module constant → `=>` call → struct → field (the `true` chain above);
  - nested `=>` calls with parameters;
  - a recursive `=>` returns `None`;
  - a local returns `None`;
  - a block-bodied call returns `None`;
  - a type with `@on_drop` returns `None`, and so does one with `@on_copy`;
  - divide by zero returns `None`;
  - a value `if` and `and`/`or` with a constant condition do not evaluate
    the dead side (a dead side that is non-constant still gives `Some`);
  - a `=>` body that is a value `match` on an enum argument, with a payload
    capture, picks the right arm; a literal-pattern `match` falls through to
    `els`; a non-constant non-chosen arm still gives `Some`.
- [x] Commit: `lower: constant-expression evaluator over HIR`.

### Task 3: Lower constant conditions

- [x] `HirStmt::If`: if `eval(cond)` is `Some(v)`, lower the live block via
      the same path as `HirStmt::Block` (so it gets its own scope) and emit
      no branch. Truthiness is non-zero, matching runtime `@bit8` semantics.
- [x] `HirExprKind::If`: lower only the live arm into the result place
      (`region`). Keep `own_temp` on the result.
- [x] `HirExprKind::Logic`: with a constant `lhs`, either emit the
      short-circuit constant or lower only `rhs`.
- [x] `HirExprKind::Match` and `HirStmt::Match`: with a constant scrutinee,
      lower only the chosen arm (binding its capture as lowering does now)
      or `els`.
- [x] Check that a live arm ending in `return`/`break` leaves lowering in the
      same state it does today when the arm is followed by a join. The block
      after it must be unreachable-but-valid for `verify`.
- [x] Lowering tests: `if 0x0 { boom() }` has no call to `boom` in the LIR;
      `if true { … } else { … }` has no `branch.if`.
- [x] Commit: `lower: lower only the live arm of a constant condition`.

### Task 4: Remove `lilac-const`

- [x] Move `check_live_compile_errors` to `lilac-ir`, without `FoldReport`
      or the note.
- [x] Driver: delete the fold stage (`Stage::Fold`, `on_stage("fold")`); call
      `lilac_ir::check_live_compile_errors` where it is called now (after
      prune and trace).
- [x] CLI `main.rs:598`, `progress.rs:187`, the driver test at `lib.rs:1019`
      and the LSP `index.rs:183`: stage list `lower, runtime, final`.
- [x] Delete `crates/lilac-const` and its workspace and dependency entries
      (driver, the lower dev-dependency); update `lilac-lower/src/tests.rs:25`.
- [x] Update the `lilac-ir` doc comments that mention folding (`dce.rs:4`,
      `lib.rs:393`, `lib.rs:747`, verify's `bug!` text: "`@compile_error`
      reached verify").
- [x] Commit: `driver: drop lilac-const; @compile_error is reachability-only`.

### Task 5: Fixtures

- [x] `tests/ui/cfold_call_eval.lilac` and `cfold_miss_is_ok.lilac` test
      behaviour that no longer exists. Keep them as plain "checks clean"
      ports in `m3.rs` (they still compile), and regenerate or delete their
      `.lir`/`.ll` siblings. First confirm whether any test compares those
      snapshots: `emit-ir --check` only verifies.
- [x] Keep `cfold_dead_compile_error.lilac` (`if 0x0` / `if 0x1 … else`); it
      must still be silent.
- [x] New UI and run tests (add a `tests/run` case for each that changes
      generated code):
  - `const_cond_inline_bool`: with `// lib: Standard=lib/Standard`,
    `if Standard.false { @compile_error("x") }` is silent, and
    `if Standard.true { exit(0x1) }` exits 1;
  - `const_cond_dead_box_copy`: a `Box` copy inside `if false` is silent;
  - `FAILS_const_cond_local`: `t: Bool = false; if t { @compile_error("x") }`
    is `E031` (pins the rule);
  - `const_cond_size_of`: generic `if @size_of(T) == 0x0 { @compile_error }`
    is silent for `@bit32`;
  - `FAILS_const_cond_hooked`: a constant over a type with `@on_drop` does
    not fold, so the guarded `@compile_error` is `E031`.
- [x] Commit: `tests: constant conditions and reachability E031`.

### Task 6: Docs

- [x] `SPEC.md` §"Compile-time folding" → "Constant conditions": the two
      rules, the constant-expression table, and the note that locals never
      fold. Remove `MAX_FOLD_STEPS`/`MAX_CALL_DEPTH`. Update the §7 `=>`
      paragraph ("constant arguments fold like any other call" → "a `=>` call
      with constant arguments is a constant expression").
- [x] `IR_SPEC.md` line 884: `branch.if` on a *constant-expression* condition
      never reaches LIR.
- [x] `docs/errors/E031.md`: replace the folding and budget text with the
      reachability and constant-condition rules.
- [x] `AGENTS.md` pipeline: `lilac-lower` (… including constant conditions) →
      runtime injection → DCE → … → live `@compile_error` check.
- [x] Commit: `docs: reachability @compile_error and constant conditions`.

## Implementation notes

- `Value` has no `Float`: float literals and float arithmetic are never
  constant (the old folder never folded float arithmetic either), so
  `float_bits`/`bits_float` and the bitcast test were dropped rather than
  ported.
- The evaluator is `impl Lower` methods in `constant.rs` (`constant`,
  `constant_choice`) with an `Eval { stack, frames }` state, rather than a
  separate `ConstEval` struct holding `&Lower` (it needs `&mut` for the
  memoized `needs_drop`/`needs_copy`).
- Evaluator coverage is tested through lowering (`assert_arm` in
  `lilac-lower/src/tests.rs`), which pins both the evaluator and the
  live-arm lowering.
- `emit-ir --check`/`emit-llvm --check` *do* compare snapshots; every
  `tests/ui` `.lir`/`.ll` was regenerated. The diffs are only constant
  conditions losing their branch and constant arithmetic/calls staying as
  runtime code.
- The new fixtures live in `tests/run` (`const_cond_*`, `FAILS_const_cond_*`
  via `// expect-error: E031`).
- The verifier still rejects `branch.if` on a `const` register: lowering
  never emits one, since every condition that lowers to a bare `const` is a
  constant expression.

## Follow-ups (not in this plan)

- **Typeck-time constant conditions**, so a dead arm in a generic instance is
  never instantiated (duck-typed `T.method()` behind `@size_of`/type tests).
- **Constant locals**, if the language gains immutable bindings: a local
  whose initializer is constant and that is never assigned could count. This
  is the only way `t: Bool = true; if t` would fold, and it should be an
  explicit language rule rather than dataflow.
- **An `-O0` LIR peephole**, only if debug-build size or speed regresses.
