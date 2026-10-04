# M3 Implementation Plan — strings, literals, const call-eval

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Typecheck and lower string literals, decimal number literals, and f-strings; fold `@as_int`/`@as_float` at compile time; add best-effort bounded call-eval in `lilac-const`; ship `E060`/`E061`.

**Architecture:** No new surviving IR instruction shapes. Builtins (`@byte_buffer_*`, `@format_args_*`, `@load_u8`, `@store_u8`, `@memcpy`) reuse `call` via a new `LirCallee::Builtin`; literals reuse `const` with a data-symbol literal (`const !c0:#byte_buffer`); fold-required intrinsics (`@as_int`/`@as_float`/`@size_of`) are pre-fold markers consumed by the folder. Literal classes (hex/bin vs decimal) and f-string desugaring live in typeck; string/number materialization in lower; folding in `lilac-const`.

**Tech Stack:** Rust (std only), existing workspace crates (`lilac-lex/ast/parse/hir/ty/typeck/ir/const/lower/driver/cli`).

**Spec:** `docs/superpowers/specs/2026-10-04-lilac-rust-rewrite-m3-design.md` — the plan argues from the spec; executors read both.

## Global Constraints

- Gates after every task: `cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace` all green.
- Frontend crates never import `inkwell`/`llvm-sys`.
- No `unwrap`/`expect`/`panic!` on user input; `bug!()` for internal invariants only.
- std only; no new crates beyond the already-scaffolded workspace members.
- TDD: write the failing test first, watch it fail, implement, watch it pass.
- One commit per task; an `docs/errors/E0xx.md` doc for every new code before its task commits.
- Work stays on branch `rust-rewrite-m0`.

## Review Focus

Input classes the spec implies but no task's happy path exercises, most likely to bite first:

1. **Hex/bin literal wider than its target** (`a: @bit8 = 0x1FF`) → `E060`, not silent truncation/wrap.
2. **Decimal literal assigned to a bit type** (`a: @bit8 = 5`) → `E010` naming `@byte_buffer`, not `E024` or a silent fold.
3. **`@as_int`/`@as_float` on a non-constant buffer** (a variable, not a literal) → `E060`, never an ICE or a runtime call.
4. **F-string hole without `@conversion(T -> @byte_buffer)`** → `E043` at the hole span, not a dump of format internals.
5. **More than 16 f-string pieces** → `E061`, not a panic or index overflow.

Each is pinned by a test in the task that owns it (Task 2, Task 2, Task 5, Task 3, Task 3 respectively). Additional value-error coverage (bad digits `@as_int("hello")`, string-const dedup) rides in Tasks 5 and 4.

---

### Task 1: IR builtins + data consts (`lilac-ir`)

**Files:**
- Modify: `crates/lilac-ir/src/lib.rs` (types, printer, verifier)
- Test: `crates/lilac-ir/src/lib.rs` (`mod tests`)

**Interfaces:**
- Consumes: existing `LirCallee`, `LirInst`, `LirData`/`LirDataKind`, `Verifier`.
- Produces (used by Tasks 4–6):
  - `LirCallee::Builtin(String)` (name without `@`, e.g. `"@format_args_new"`).
  - `LirInst::FoldIntrinsic { name: String, arg: LirOperand, dst: String, ty: String, span: Span }` — pre-fold marker for `@as_int`/`@as_float`/`@size_of` (name keeps the `@`). Verifier treats a survivor like `CompileError` (internal error).
  - `LirInst::Const { lit }` may be a data symbol `!c0` (used only when `ty` is a builtin aggregate `#byte_buffer`/`#format_args` or `ref #bit8`); the verifier resolves the name against `module.data`.
  - `pub fn builtin_sig(name: &str) -> Option<BuiltinSig>` where `BuiltinSig { ins: Vec<Option<&'static str>>, outs: Vec<Option<&'static str>> }` (type strings in `#`-spellings; `None` = width-parameterized slot). Table rows (name → ins, outs): `@byte_buffer_from` `[Some("ref #bit8"), Some("#bitNative")] → [Some("#byte_buffer")]`; `@byte_buffer_ptr` `[Some("#byte_buffer")] → [Some("ref #bit8")]`; `@byte_buffer_len` `[Some("#byte_buffer")] → [Some("#bitNative")]`; `@format_args_new` `[] → [Some("#format_args")]`; `@format_args_push` `[Some("ref #format_args"), Some("#byte_buffer")] → []`; `@format_args_count` `[Some("#format_args")] → [Some("#bitNative")]`; `@format_args_piece` `[Some("#format_args"), Some("#bitNative")] → [Some("#byte_buffer")]`; `@load_u8` `[Some("ref #bit8")] → [Some("#bit8")]`; `@store_u8` `[Some("ref #bit8"), Some("#bit8")] → []`; `@memcpy` `[Some("ref #bit8"), Some("ref #bit8"), Some("#bitNative")] → []`.
  - Printer: `print_callee` renders `Builtin` as `@name`; `print_inst` renders `FoldIntrinsic` as `&d: #t = @as_int &arg`; `Const` with a `!`-literal renders the symbol verbatim.

- [ ] **Step 1: Write failing tests.** In `mod tests`, add (a) `builtin_call_prints_and_verifies`: a module whose one func's entry block has `alloca #byte_buffer %buf : zeroed` + a `Call { func: Builtin("@format_args_new".into()), ins: vec![], outs: vec!["buf".into()], span }` + `Return`; assert `verify` is `Ok` and `to_text()` contains `call @format_args_new`. (b) `fold_intrinsic_prints`: a func with `FoldIntrinsic { name: "@as_int", arg: Reg("1"), dst: "2", ty: "#bit8", span }`; assert `to_text()` contains `@as_int` and that `verify` errors (`E070`). (c) `data_symbol_const_verifies`: a module with `data = [LirData { name: "c0", kind: Str("hi"), span }]` and a func with `Const { dst: "1", lit: "!c0", ty: "#byte_buffer", span }`; assert `verify` is `Ok`, and `unknown_data_symbol_rejected` errors.

- [ ] **Step 2: Run tests to verify they fail.** `cargo test -p lilac-ir` — compile errors (`Builtin`/`FoldIntrinsic` variants missing).

- [ ] **Step 3: Implement.** Add `Builtin` to `LirCallee`; add `FoldIntrinsic` to `LirInst`; add `BuiltinSig`/`builtin_sig`. In `check_call_target`, add a `LirCallee::Builtin(name)` arm that looks up `builtin_sig` (unknown → error; arity mismatch → error; where a slot is `Some(t)`, check the operand/place type equals `t`). In `verify_inst`, add the `FoldIntrinsic` arm emitting `E070` (never survives). In `verify_inst`'s `Const` arm, when `lit` starts with `!`, resolve the name in `module.data` (error if absent) and require `ty` to be one of `#byte_buffer`, `#format_args`, or `ref #bit8`. Update `print_callee`/`print_inst` and any `match` exhaustiveness sites.

- [ ] **Step 4: Run tests to verify they pass.** `cargo test -p lilac-ir` — all green; then `cargo clippy -p lilac-ir --all-targets -- -D warnings`.

- [ ] **Step 5: Commit.** `git add crates/lilac-ir/src/lib.rs && git commit -m "M3: IR builtin calls, fold markers, data consts"`

---

### Task 2: Literal classification + `E060` range check (`lilac-typeck`)

**Files:**
- Modify: `crates/lilac-typeck/src/check.rs` (check_literal, unify_width)
- Test: `crates/lilac-typeck/src/check.rs` (`mod tests`)

**Interfaces:**
- Consumes: `check_literal(expr, expected)`, `unify_width(fctx, args, expected, span)`, `is_bit`, `builtin_ty`.
- Produces (used by Task 3–4): decimal `Expr::Number` now types as `@byte_buffer` (always, ignoring `expected`) and produces `HirExprKind::Number { bytes }`; hex/bin types as the expected/unified bit type and produces `HirExprKind::Number { bytes }`. Free helpers:
  - `fn is_lit_literal(bytes: &str) -> bool` — true for `0x`/`0X`/`0b`/`0B` prefixes.
  - `fn lit_value_bits(bytes: &str) -> Option<(u64, u32)>` — parse hex/bin (with `_` separators) into value + significant-bit count.

- [ ] **Step 1: Write failing tests.** In `mod tests`:
  - `hex_literal_takes_expected_bit_type`: `check_src("main: function () nothing {\n    x: @bit8 = 0x1\n}\n")` has no errors.
  - `decimal_literal_is_byte_buffer_not_bit`: `check_src("main: function () nothing {\n    x: @bit8 = 5\n}\n")` has `E010` and a message containing `@byte_buffer`.
  - `as_int_of_decimal_checks`: `check_src("main: function () nothing {\n    x: @bit8 = @as_int(5)\n}\n")` has no errors.
  - `hex_literal_overflow_is_e060`: `check_src("main: function () nothing {\n    x: @bit8 = 0x1FF\n}\n")` has `E060`.
  - `decimal_in_arith_is_e010`: `check_src("main: function () nothing {\n    x: @bit32 = @iadd(5, 6)\n}\n")` has `E010`.

- [ ] **Step 2: Run tests to verify they fail.** `cargo test -p lilac-typeck` — the decimal cases currently `E024` (or pass), so the `E010`/`E060` assertions fail.

- [ ] **Step 3: Implement.** Add `is_lit_literal`/`lit_value_bits`. Rewrite the `Expr::Number` arm of `check_literal`: if `is_lit_literal(bytes)`, keep today's expected-bit behavior and additionally, once a concrete bit width is known, parse `lit_value_bits` and emit `E060` (message `value does not fit in {ty} (max 0x{max:x})`) when the value exceeds it; otherwise (decimal) return `@byte_buffer` with `HirExprKind::Number { bytes }`, ignoring `expected`. In `unify_width`, change the `matches!(arg, Expr::Number(..))` skip to `matches!(arg, Expr::Number(bytes, _) if is_lit_literal(bytes))` so decimals flow through normal checking (and fail as non-bit with `E010`).

- [ ] **Step 4: Run tests to verify they pass.** `cargo test -p lilac-typeck` — new tests green; run the full suite to confirm no existing test regressed (the old `overflowing_literal_waits_for_m3` test now expects `E060`: update its name/assertion to `hex_literal_overflow_is_e060` if it overlaps).

- [ ] **Step 5: Commit.** `git add crates/lilac-typeck/src/check.rs && git commit -m "M3: classify literals; E060 range check"`

---

### Task 3: F-string desugar + `E061` (`lilac-typeck`)

**Files:**
- Modify: `crates/lilac-typeck/src/check.rs` (check_expr FString arm, new `check_fstring`)
- Create: `docs/errors/E061.md`
- Test: `crates/lilac-typeck/src/check.rs` (`mod tests`)

**Interfaces:**
- Consumes: `check_expr`, `convert_if_needed`, `fresh_local`, `builtin_ty`, `push_stmt`, `HirExprKind::{Call, Ident, RefOf, Str}`, `HirCallee::Intrinsic`, `Intrinsic::{FormatArgsNew, FormatArgsPush}`.
- Produces (used by Task 4): a desugared `HirExpr` whose `kind` is `Ident { name: "__f", def: DefId::ERROR }`, typed `@format_args`, with the block prelude carrying a `Let` (`@format_args_new`) and one `ExprStmt` per piece (`@format_args_push`). Hole values are already converted to `@byte_buffer` (E043 on failure). New signature:
  - `fn check_fstring(&mut self, fctx: &mut FuncCtx, parts: &[lilac_ast::FStrPart], span: Span) -> (TypeRef, HirExpr)`

- [ ] **Step 1: Write failing tests.**
  - `fstring_desugars_to_format_args`: `check_src("main: function () nothing {\n    x: @format_args = $\"a{0x1}\"\n}\n")` has no errors. (Hole `0x1` is a hex literal; the ordinary `@conversion(@bit32 -> @byte_buffer)` search is expected to fail → but for this test declare a conversion, or use a hole that needs none.) Use a hole-free form for the clean case: `x: @format_args = $"a"`.
  - `fstring_hole_missing_conversion_is_e043`: source with `name: @bit32` local and `x: @format_args = $"{name}"` (no conversion declared) → `E043`.
  - `fstring_piece_cap_is_e061`: an f-string with 17 literal chunks (`$"0123456789abcdefg"` split is not needed — write 17 `{name}` holes or 17 literal chunks via concatenated `""` pieces is not possible; instead write 17 holes) → `E061`. Use `$"{a}{b}…"` with 17 distinct holes; simpler: one hole repeated is fine syntactically. Assert `E061`.

- [ ] **Step 2: Run tests to verify they fail.** `cargo test -p lilac-typeck` — f-strings currently `E030`.

- [ ] **Step 3: Implement.** Replace the `Expr::FString` arm in `check_expr` with a call to `check_fstring`. In `check_fstring`: emit `E061` (new code, label `span`, message `too many format pieces (max 16)`) when `parts.len() > 16`. Otherwise: allocate `let fname = fresh_local(fctx, "f")`; insert `fctx.locals.insert(fname.clone(), TypeRef { id: builtin_ty("@format_args"), is_ref: false })`; `push_stmt` a `HirStmt::Let` whose value is `HirExprKind::Call { callee: HirCallee::Intrinsic { name: "@format_args_new".into() }, args: vec![] }` typed `@format_args`. For each part: build the piece `HirExpr` — a `FStrPart::Lit(chunk)` becomes `HirExprKind::Str { value: chunk }` (ty `@byte_buffer`); a `FStrPart::Hole(expr)` is `check_expr(fctx, expr, None)` then `convert_if_needed(got, builtin_ty("@byte_buffer"), hir)` (the conversion search's failure already emits `E043`). Then `push_stmt` an `ExprStmt` of `HirExprKind::Call { callee: Intrinsic("@format_args_push"), args: vec![RefOf{ value: Ident{fname} }, piece] }`. Return `(format_args TypeRef, HirExpr { kind: Ident { name: fname, def: DefId::ERROR }, .. })`. Write `docs/errors/E061.md` (good/bad examples).

- [ ] **Step 4: Run tests to verify they pass.** `cargo test -p lilac-typeck` — new tests green; `E030` no longer fires for f-strings.

- [ ] **Step 5: Commit.** `git add crates/lilac-typeck/src/check.rs docs/errors/E061.md && git commit -m "M3: f-string desugar to @format_args; E061"`

---

### Task 4: String/number lowering + builtin emission (`lilac-lower`)

**Files:**
- Modify: `crates/lilac-lower/src/lib.rs` (eval Str/Number arms, lower_intrinsic)
- Test: `crates/lilac-lower/src/lib.rs` (`mod tests`)

**Interfaces:**
- Consumes: `LirCallee::Builtin`, `LirInst::{Const, FoldIntrinsic}`, `LirData/LirDataKind` (Task 1); decimal `HirExprKind::Number` with ty `@byte_buffer` (Task 2); f-string HIR calls (Task 3).
- Produces (used by Tasks 5–6): literals materialize as `data` consts + a `const !cN:#byte_buffer` register; `@as_int`/`@as_float`/`@size_of` emit `FoldIntrinsic`; `@byte_buffer_*`/`@format_args_*`/`@load_u8`/`@store_u8`/`@memcpy` emit `Call` to `Builtin`. New signatures:
  - `fn intern_data(&mut self, kind: LirDataKind, payload: &str) -> String` — dedup by `(kind, payload)`, assign `c0`, `c1`, … in first-use order, push into `self.module.data`, return the name (without `!`).
  - `fn escape_data(payload: &str) -> String` — UTF-8 encode; `"`→`\22`, `\`→`\5C`, `\n`→`\0A` (the canonical `data` escaping already used by the printer for `LirDataKind::Str`).

- [ ] **Step 1: Write failing tests.**
  - `lower_str_emits_data_and_buffer`: `lower_src("main: function () nothing {\n    s: @byte_buffer = \"hi\"\n}\n")` — assert `diags` clean, `text.contains("str = \"hi\"")`, `text.contains("#byte_buffer")`.
  - `lower_decimal_emits_numlit_data`: `lower_src("main: function () nothing {\n    x: @bit8 = @as_int(5)\n}\n")` — assert `text.contains("numlit = \"5\"")` and `text.contains("@as_int")`.
  - `lower_hex_stays_const`: `lower_src("main: function () nothing {\n    x: @bit8 = 0x1\n}\n")` — assert `text.contains("const 0x1:#bit8")` and no `data {`.
  - `lower_builtin_calls`: `lower_src` with an `@format_args_new()` in an `@format_args` binding — assert `text.contains("call @format_args_new")`.
  - `identical_strings_share_one_const`: `lower_src` with two `"hi"` bindings — assert `text` contains exactly one `str = "hi"`.

- [ ] **Step 2: Run tests to verify they fail.** `cargo test -p lilac-lower` — Str/decimal currently `E030` or `const`.

- [ ] **Step 3: Implement.** In `eval`, replace the `HirExprKind::Str` `E030` arm: call `intern_data(LirDataKind::Str(v), v)`, emit `Const { dst, lit: format!("!{name}"), ty: "#byte_buffer", span }`. In `lower_number`, when `ty` is `@byte_buffer` (decimal), do the same with `LirDataKind::NumLit`; hex/bin keep the direct `Const`. In `lower_intrinsic`, add arms: `"@as_int" | "@as_float" | "@size_of"` → emit `FoldIntrinsic` (one arg via `eval_reg`); `"@byte_buffer_from" | "@byte_buffer_ptr" | "@byte_buffer_len" | "@format_args_new" | "@format_args_push" | "@format_args_count" | "@format_args_piece" | "@load_u8" | "@store_u8" | "@memcpy"` → emit a `Call` to `LirCallee::Builtin` into a temp place (single-result builtins then `CopyReg` place→reg for value position). Remove the `E030` fallthrough for these names.

- [ ] **Step 4: Run tests to verify they pass.** `cargo test -p lilac-lower` — new tests green; full suite confirms no regression.

- [ ] **Step 5: Commit.** `git add crates/lilac-lower/src/lib.rs && git commit -m "M3: lower strings/decimals to data+buffer; builtin calls"`

---

### Task 5: Fold `@as_*`/`@size_of` + `E060` (`lilac-const`)

**Files:**
- Modify: `crates/lilac-const/src/lib.rs` (ConstValue, fold, eval)
- Create: `docs/errors/E060.md`
- Test: `crates/lilac-const/src/lib.rs` (`mod tests`)

**Interfaces:**
- Consumes: `FoldIntrinsic`, `Builtin` calls, data-symbol `Const` (Tasks 1, 4); existing `fold`/`Folder`.
- Produces (used by Task 6): extended `ConstValue` and an intrinsic evaluator.
  - `pub enum ConstValue { Unknown, Int(u64, u32), Float(f64, u32), Bytes(Vec<u8>), Bool(bool) }` (replace the M2 three-variant form; update M2 call sites).
  - `fn eval_intrinsic(name: &str, args: &[ConstValue], width: u32) -> Result<ConstValue, FoldValueError>` where `FoldValueError` carries a message; `@as_int` parses `Bytes` digits (`0x`/`0b`/decimal int) into `Int`, overflow → error; `@as_float` parses into `Float`; `@size_of` returns `Int(width/8, 64)`.
  - `Const { lit: "!cN" }` becomes `ConstValue::Bytes` by resolving `module.data`.

- [ ] **Step 1: Write failing tests.** In `mod tests`:
  - `as_int_folds_to_const`: a module whose func has `Const !c0:#byte_buffer` (data `numlit "5"`), then `FoldIntrinsic @as_int (in arg reg, out `#bit8`)`; assert `fold` is `Ok`, `to_text()` contains `const 0x5:#bit8` and no `@as_int`.
  - `as_int_bad_digits_is_e060`: data `numlit "hello"` → `FoldIntrinsic @as_int`; assert `fold` is `Err` and `diags` has `E060`.
  - `as_int_overflow_is_e060`: data `numlit "0x1FF"` → `@as_int` into `#bit8`; assert `E060`.
  - `as_int_nonconstant_is_e060`: `FoldIntrinsic @as_int` whose arg is a register with no const (`Unknown`); assert `E060`.

- [ ] **Step 2: Run tests to verify they fail.** `cargo test -p lilac-const` — `FoldIntrinsic`/`Bytes` don't exist yet.

- [ ] **Step 3: Implement.** Extend `ConstValue`. In `fold_inst`, handle `Const` with a `!`-literal by looking up `module.data` and producing `Bytes`; handle `FoldIntrinsic` by `eval_intrinsic` on the tracked arg (missing/`Unknown`/`ValueError` → emit `E060`, set failed, and poison the dst to `Unknown`); on success, rewrite the instruction to `Const { dst, lit: <hex/int form>, ty, span }`. Thread `&module` (for data lookup) into the folder as needed. Write `docs/errors/E060.md`.

- [ ] **Step 4: Run tests to verify they pass.** `cargo test -p lilac-const` — new tests green; M2 fold tests still pass after the `ConstValue` shape change.

- [ ] **Step 5: Commit.** `git add crates/lilac-const/src/lib.rs docs/errors/E060.md && git commit -m "M3: fold @as_int/@as_float/@size_of; E060"`

---

### Task 6: Bounded call-eval (`lilac-const`)

**Files:**
- Modify: `crates/lilac-const/src/lib.rs` (cross-block consts, `eval_call`, memory model)
- Test: `crates/lilac-const/src/lib.rs` (`mod tests`)

**Interfaces:**
- Consumes: `ConstValue`, `eval_intrinsic`, `Builtin` calls (Tasks 1, 5).
- Produces: none beyond the extended `fold` (Task 7 drives it through the pipeline).
  - `fn eval_builtin(name: &str, ins: &[ConstValue], out_ty: &str) -> Result<ConstValue, FoldValueError>` — evaluates `@byte_buffer_from` (data ptr + len → `Bytes` from the const-bytes memory), `@load_u8` (offset 0 from a `Bytes` pointee), `@store_u8`/`@memcpy` (mutate local bytes), `@byte_buffer_ptr`/`@byte_buffer_len` (decompose a `Bytes`); anything else → `Err` (miss).
  - `fn fold` now iterates functions with a worklist over blocks, joining a block's entry `Reg → ConstValue` map from its predecessors (non-equal join → `Unknown`); when it sees `Call { func: Builtin(..), .. }` or `Call { func: Func(..), .. }` with all-const `in` places, it attempts `eval_call` under `MAX_FOLD_STEPS`/`MAX_CALL_DEPTH`; success rewrites the call's out-place stores to `Const`, miss leaves the call.

- [ ] **Step 1: Write failing tests.**
  - `block_wrapper_parser_folds`: a two-func module where `@parse` (pure, loop over `@load_u8` from its `@byte_buffer` param accumulating an int) is `call`ed with a const buffer; assert `fold` produces a `const 0x...` and no surviving `call @parse`.
  - `call_eval_miss_stays_call`: a `call @f` whose arg is `Unknown`; assert the `call @f` survives (miss-is-ok, `fold` is `Ok`).
  - `fuel_exhaustion_stays_call`: a `@f` with a loop whose trip count exceeds a tiny fuel (use a small injected `MAX_FOLD_STEPS` override via a `fold_with_fuel(module, diags, fuel, depth)` test-only entry); assert the call survives and `fold` is `Ok`.

- [ ] **Step 2: Run tests to verify they fail.** `cargo test -p lilac-const` — no `eval_call`/worklist yet.

- [ ] **Step 3: Implement.** Add `eval_builtin` and a `fn eval_call` (interpret `const`/`copy`/`arith`/`convert`/`branch`/`branch.if`/`return` over a `ConstValue` environment and a `HashMap<place, Vec<u8>>` const-bytes memory, decrementing `steps`, capping `depth`, stopping on `panic`/impure builtins). Replace the M2 single-pass block fold with a worklist that first computes per-block entry const maps to a fixpoint, then folds instructions and calls in block order. Refactor the M2 `fold_func` accordingly. Keep `prune_dead`/`check_live_errors`.

- [ ] **Step 4: Run tests to verify they pass.** `cargo test -p lilac-const` — new tests green; M2 fold tests still pass.

- [ ] **Step 5: Commit.** `git add crates/lilac-const/src/lib.rs && git commit -m "M3: bounded call-eval in const folder"`

---

### Task 7: Driver/CLI ports, snapshots, spec patches (`driver`, `cli`, `tests`, docs)

**Files:**
- Modify: `crates/lilac-driver/src/lib.rs` (none expected; verify pipeline already runs fold before verify), `crates/lilac-cli/tests/m3.rs` (new), `docs/errors/E030.md`, `SPEC.md`, `IR_SPEC.md`
- Create: `crates/lilac-cli/tests/m3.rs`; `tests/ui/{as_int_8,as_int_16,as_int_32,as_int_64,as_float_16,as_float_32,as_float_64,string,fstring,cfold_call_eval,cfold_miss_is_ok}.lilac` + `.lir`; `tests/FAILS_fstring_missing_conversion.lilac`; `tests/FAILS_as_int_overflow.lilac`; `tests/FAILS_fstring_piece_cap.lilac`

**Interfaces:**
- Consumes: `emit-ir`/`check` CLI, `run_pipeline` (fold → verify), the M2 `m2.rs` harness pattern (`lilac()`, `ui()`, `legacy()`, `fails_with`).
- Produces: byte-stable `.lir` snapshots and green UI suite.

- [ ] **Step 1: Port and write fixtures.** Port `tests/as_int_8.lilac` etc. to `tests/ui/` using `@as_int(5)` / `@as_float(5.25)` (expected-width `@bit8/16/32/64`). Write the `string`, `fstring`, `cfold_call_eval`, `cfold_miss_is_ok` sources, and the three `FAILS_*` sources (hole without conversion, `@as_int(0x1FF)` into `@bit8`, 17-piece f-string).

- [ ] **Step 2: Generate snapshots.** Build `lilac`, then for each `tests/ui/*.lilac` run `emit-ir` and write `to_text()` to the matching `.lir` via `[IO.File]::WriteAllText` (no BOM). Confirm `@as_int`/`@as_float` never appear in the `as_*` snapshots (folded to `const`).

- [ ] **Step 3: Write the `m3.rs` harness.** Mirror `m2.rs`: a `PORTS` list (the `as_*` + `string` + `fstring` + `cfold_*` names) with `check_ports_clean` and `lir_snapshots_stable` (`emit-ir --check`), plus `fails_with` tests for the three `FAILS_*` codes (`E043`, `E060`, `E061`). Add `mod m3;`-style registration per the existing test layout.

- [ ] **Step 4: Run the suite.** `cargo test --workspace` — m0/m1/m2/m3 all green; then `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings`.

- [ ] **Step 5: Patch docs.** Trim `E030.md` (remove string/f-string triggers); update `SPEC.md` §2 (LitLiteral/NumberLiteral classes, `@as_*` must be constant, `E060`/`E061`) and §3 table notes; update `IR_SPEC.md` (builtin calls, `const !cN` data form, fold-only markers never survive); bump the `SPEC.md` version header; add `E060.md`/`E061.md` good/bad examples. Commit.

- [ ] **Step 6: Final gates.** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; `grep -r inkwell crates/lilac-{lex,ast,parse,resolve,ty,hir,typeck,ir,const,lower}` empty.

---

## Self-review

- **Spec coverage:** §3.1 literals → Task 2; §3.2 strings → Task 4; §3.3 f-strings → Tasks 3+4; §3.4 LIR builtins → Task 1; §3.5 call-eval → Tasks 5+6; §4 spec patches → Task 7; §5/§6 tests+exit → Task 7. All sections have an owner.
- **Step scan:** every step names the exact signature/assertion or a concrete command; no "handle edge cases"/"add validation" filler.
- **Type consistency:** `Builtin`/`FoldIntrinsic`/`builtin_sig`/`ConstValue` shapes are identical across Tasks 1→5→6; `HirExprKind::Number`/`Str` and `@byte_buffer` typing are used consistently in Tasks 2→4.
- **Review Focus:** each of the five lines is pinned by a named test in the owning task (Task 2: `hex_literal_overflow_is_e060`, `decimal_literal_is_byte_buffer_not_bit`; Task 5: `as_int_nonconstant_is_e060`; Task 3: `fstring_hole_missing_conversion_is_e043`, `fstring_piece_cap_is_e061`).
- **Proportion:** ~7 tasks for a 7-section spec; interfaces carry the decisions, no function bodies are transcribed except the `builtin_sig` table and exact error messages the spec fixes.

---

## Deferred follow-ups (post-implementation, not in M3 scope)

- **Dead code elimination.** M3 prunes only unreachable *blocks* within a live function (`lilac-const::prune_dead`) and removes dead `@compile_error`s. It does **not** remove: dead *functions* (an uncalled function whose call sites all folded, e.g. `@parse` in `cfold_call_eval.lir`, remains in the module), dead *data* consts (e.g. the `!cN: numlit` backing a folded `@as_int` literal), or unused instructions/allocas inside live blocks. This is not a correctness issue — the IR verifies and LLVM drops unused internal functions — but it is a size/cleanliness gap. A post-fold reachability sweep (roots = `@main`; follow `Call` and `AddrFunc`; then sweep unreferenced `data`) would address it. Deferred to M5 (LLVM/optimization) per the milestone plan. Caveats if added: indirect calls via `AddrFunc` must be treated as live roots, and a dead function containing an unfoldable `@as_int` currently reports `E060` (the `E060`-before-prune ordering).
- **Parked minor findings** from task/final reviews (all non-blocking): `FoldIntrinsic` survivor emits `E070` rather than `bug!`; duplicate ins/outs type-check loops in `check_call_target` and the indirect-call diagnostic lost the callee name; `E060` message casing differs between typeck (`@bitN`) and folder (`#bitN`); `fold_intrinsic` `Ok(value)` arm does not rewrite the marker (unreachable today); `@size_of` folds only from a constant operand width (best-effort, M5 surface); `materialize_call` returns `None` for `Float`/`Bool` (missed fold); `intern_data` uses `Span::dummy()`; f-string literal chunks reuse the whole-f-string span; a mutating callee is conservatively never folded; no positive `@conversion(@byte_buffer -> String)` UI case (only the negative `E043` path is exercised).
