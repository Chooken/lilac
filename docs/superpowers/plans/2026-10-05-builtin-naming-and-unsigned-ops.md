# Builtin Naming and Unsigned Ops Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every builtin snake_case, add the remaining unsigned integer ops, and cull unused leaf builtins from the emitted module.

**Architecture:** All three are mechanical extensions of existing machinery: a rename sweep across the builtin vocabulary (`@bitNative` type, integer/float arithmetic builtins, the `@compilerError` alias), three new `ArithOp` variants plus four new builtin hooks, and an emit-time reachability scan in the LLVM backend that emits only the leaf intrinsics a surviving LIR function calls.

**Tech Stack:** Rust (std only), LLVM 18.1.8 via `inkwell` (confined to `lilac-codegen-llvm`), hand-rolled binary LIR codec.

**Spec:** none — bounded follow-up; the agreed design is the Design section below. Background: `docs/superpowers/specs/2026-10-05-lilac-runtime-in-lir-design.md`.

## Global Constraints

- LLVM dev tree at `C:\Users\joshu\llvm-18.1.8`; `LLVM_SYS_181_PREFIX` set via `.cargo/config.toml`.
- Rust std only; no new third-party crates. `inkwell`/`llvm-sys` only in `lilac-codegen-llvm`; workspace `unsafe_code = "forbid"`.
- No `unwrap`/`expect`/`panic!` on user input; use `lilac_error::bug!` for ICEs.
- Gates before each commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Snapshots are LF, no BOM: write with `[IO.File]::WriteAllText($p, $t, (New-Object Text.UTF8Encoding $false))`, never shell redirect. `.gitattributes` pins `*.lir`/`*.ll` to `text eol=lf`.
- One commit per task.
- Reserved `.` naming and `.lilac.*` compiler symbols (from the runtime-in-LIR plan) stay in force.

## Design

- **Snake sweep:** `@bitNative` -> `@bit_native` (the builtin type; the LIR spelling `#bitNative` -> `#bit_native`; the mangle fragment `bitNative` -> `bit_native`); integer arithmetic `@iadd`/`@isub`/`@imul`/`@idiv`/`@imod` -> `@i_add`/`@i_sub`/`@i_mul`/`@i_div`/`@i_mod`; float arithmetic `@fadd`/`@fsub`/`@fmul`/`@fdiv`/`@fmod` -> `@f_add`/`@f_sub`/`@f_mul`/`@f_div`/`@f_mod`; drop the `@compilerError` alias (keep `@compile_error`).
- **Unsigned ops:** `ArithOp::IGtU`/`IDivU`/`IModU` (`ILtU` exists) + builtins `@i_less_than_u`/`@i_greater_than_u`/`@i_div_u`/`@i_mod_u`.
- **Leaf culling:** emit only the leaf intrinsics a surviving LIR function calls.

## Review Focus

- A source program using `@bit_native` (the renamed type) still type-checks, emits `#bit_native`, and runs; a generic over it mangles to `.bit_native`.
- A source program using `@i_add`/`@f_add` still type-checks and emits the same `ArithOp` as before.
- `@compilerError` is gone: `lookup_intrinsic("@compilerError")` is `None` and no fixture uses it.
- Each unsigned builtin lowers to its exact LLVM op (`icmp ugt`, `udiv`, `urem`), distinct from the signed op.
- A module using no `@memcpy`/`@ptr_add`/`@lilac_abort` emits no corresponding leaf definition; one that uses them does.

---

### Task 1: Snake-case builtin sweep

**Files:**
- Modify: `crates/lilac-ty/src/lib.rs` (builtin type name, `mangle`; tests)
- Modify: `crates/lilac-typeck/src/sigs.rs` (`lookup_intrinsic`; tests)
- Modify: `crates/lilac-lower/src/lib.rs` (`lir_ty`, hook->`ArithOp` map; tests)
- Modify: `crates/lilac-ir/src/lib.rs` (`valid_type`, `builtin_sig`, `check_arith_width`; tests)
- Modify: `crates/lilac-const/src/lib.rs` (`width_of` / `#bitNative` handling)
- Modify: `crates/lilac-codegen-llvm/src/lower.rs` (`lir_type` arm)
- Modify: `crates/lilac-codegen-llvm/src/runtime.rs` (any `#bitNative`/`bitNative`)
- Modify: `tests/ui/cfold_call_eval.lilac`, `tests/ui/string.lilac`, `tests/ui/format_args_use.lilac` (source spellings)
- Modify: `IR_SPEC.md`, `docs/` where the old spellings appear
- Regenerate: all `tests/ui/*.lir`, `tests/ui/*.ll`

**Interfaces:**
- Produces: builtin type `@bit_native` (LIR `#bit_native`, mangle `.bit_native`); builtins `@i_add`/`@i_sub`/`@i_mul`/`@i_div`/`@i_mod`, `@f_add`/`@f_sub`/`@f_mul`/`@f_div`/`@f_mod`; no `@compilerError`.

- [ ] **Step 1: Update the tests to the new spellings**

In `lilac-ty` tests: a generic over the native type mangles with `.bit_native`. In `lilac-typeck` `sigs.rs` tests: `lookup_intrinsic("@i_add") == Some(Intrinsic::Arith)`, `lookup_intrinsic("@f_add") == Some(Intrinsic::Arith)`, `lookup_intrinsic("@i_less_than")` unchanged, and `lookup_intrinsic("@compilerError") == None`. In `lilac-ir` tests: `builtin_sig` and `check_arith_width` fixtures use `#bit_native`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p lilac-ty; cargo test -p lilac-typeck; cargo test -p lilac-ir`
Expected: FAIL (old spellings).

- [ ] **Step 3: Rename the type `@bitNative` -> `@bit_native`**

Update every site: the source-facing builtin name, the LIR type string `#bitNative` -> `#bit_native`, the `mangle` fragment `"bitNative"` -> `"bit_native"`, and the source fixtures. Search the workspace for `bitNative` and replace consistently (do not touch unrelated identifiers).

- [ ] **Step 4: Rename the arithmetic builtins and drop the alias**

In `lookup_intrinsic` (`sigs.rs`) and the `lilac-lower` hook map: `@iadd`->`@i_add`, `@isub`->`@i_sub`, `@imul`->`@i_mul`, `@idiv`->`@i_div`, `@imod`->`@i_mod`, `@fadd`->`@f_add`, `@fsub`->`@f_sub`, `@fmul`->`@f_mul`, `@fdiv`->`@f_div`, `@fmod`->`@f_mod`. Remove `"@compilerError"` from `lookup_intrinsic` and its test. Update any tests/fixtures/specs referencing the old names.

- [ ] **Step 5: Regenerate snapshots**

Rebuild `cargo build -p lilac-cli`, then regenerate every `tests/ui/<name>.lir` and `.ll` (LF/no-BOM). Inspect the diff: only the renamed tokens should change.

- [ ] **Step 6: Run gates and commit**

Run: `cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace`
```bash
git add -A
git commit -m "Snake-case the builtin vocabulary"
```

---

### Task 2: Unsigned integer ops

**Files:**
- Modify: `crates/lilac-ir/src/lib.rs` (`ArithOp`, `print_op`, `check_arith_width`; tests)
- Modify: `crates/lilac-ir/src/codec.rs` (`ArithOp` tag; tests)
- Modify: `crates/lilac-codegen-llvm/src/lower.rs` (`lower_arith`)
- Modify: `crates/lilac-const/src/lib.rs` (`eval_arith`)
- Modify: `crates/lilac-typeck/src/sigs.rs` (`lookup_intrinsic`)
- Modify: `crates/lilac-lower/src/lib.rs` (hook->`ArithOp` map)

**Interfaces:**
- Consumes: the snake spellings from Task 1.
- Produces: `ArithOp::IGtU` (`i.cmp.gt.u`, `icmp ugt`), `IDivU` (`i.div.u`, `udiv`), `IModU` (`i.mod.u`, `urem`); builtins `@i_less_than_u` (`Intrinsic::Cmp`), `@i_greater_than_u` (`Cmp`), `@i_div_u` (`Arith`), `@i_mod_u` (`Arith`).

- [ ] **Step 1: Write failing tests**

- `lilac-ir`: `print_op` renders the three new ops; codec round-trips each; `check_arith_width` accepts them on bit containers.
- `lilac-codegen-llvm`: `IGtU` -> `icmp ugt`, `IDivU` -> `udiv`, `IModU` -> `urem`, and the signed counterparts stay `sgt`/`sdiv`/`srem`.
- `lilac-typeck`: `lookup_intrinsic("@i_less_than_u") == Some(Intrinsic::Cmp)`, `@i_greater_than_u` `Cmp`, `@i_div_u` `Arith`, `@i_mod_u` `Arith`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p lilac-ir; cargo test -p lilac-codegen-llvm; cargo test -p lilac-typeck`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add the three `ArithOp` variants and wire printer, `check_arith_width` (bit containers, non-float path), codec tags, LLVM lowering (`IntPredicate::UGT`, `UDiv`, `URem`), const-eval (masked unsigned semantics), `lookup_intrinsic`, and the lower hook map.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Gates and commit**

```bash
git add -A
git commit -m "Add unsigned integer comparison and division ops"
```

---

### Task 3: Cull unused leaf builtins

**Files:**
- Modify: `crates/lilac-codegen-llvm/src/runtime.rs` (`emit_runtime` / call site)
- Test: `crates/lilac-codegen-llvm/src/lib.rs` (unit tests)
- Regenerate: all `tests/ui/*.ll`

**Interfaces:**
- Consumes: the runtime-in-LIR prelude (nine `.lilac.*` funcs) and the leaves `@ptr_add`/`@memcpy`/`@lilac_abort`.

- [ ] **Step 1: Write failing tests**

- A module with no builtin call emits no `@.lilac.ptr_add`/`@.lilac.memcpy`/`@.lilac.abort` definition and no `declare void @llvm.trap`.
- A module calling `@memcpy` emits `@.lilac.memcpy` but not `.lilac.ptr_add`.
- A module calling `@ptr_add` emits `@.lilac.ptr_add`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p lilac-codegen-llvm`
Expected: FAIL (leaves are always emitted today).

- [ ] **Step 3: Implement the reachability scan**

Before emitting, scan `lower.lir.funcs` for `LirInst::Call { func: LirCallee::Builtin(name), .. }` and collect the used names. In `emit_runtime`, emit `.lilac.ptr_add` iff `@ptr_add` is used, `.lilac.memcpy` iff `@memcpy` is used, and `.lilac.abort` + `declare void @llvm.trap` iff `@lilac_abort` is used. The definition still precedes `lower_func`, so `lower_builtin_call` finds it.

- [ ] **Step 4: Regenerate `.ll` snapshots**

Rebuild `lilac-cli` and regenerate every `tests/ui/<name>.ll` (LF/no-BOM). Modules not using a leaf lose its definition.

- [ ] **Step 5: Run gates and commit**

Run: `cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace`
```bash
git add -A
git commit -m "Emit only the leaf builtins a module calls"
```
