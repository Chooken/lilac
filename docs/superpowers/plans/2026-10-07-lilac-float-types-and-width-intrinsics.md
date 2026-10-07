# Lilac Float Types & Width-Suffixed Intrinsics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `@fbit16/32/64/128` real builtin types, replace the width-polymorphic numeric intrinsics with per-width names, add per-width fbit↔bit bitcasts, and extend the C ABI to the float widths.

**Architecture:** Add the float types through every layer (ty → IR → LLVM), switch float arithmetic to operate on real float values, then atomically rename the numeric intrinsic surface and update every source that uses it. The rename is one task because there are no aliases.

**Tech Stack:** Rust workspace; compiler crates std-only; LLVM confined to `lilac-codegen-llvm`; `inkwell` for codegen.

**Spec:** `docs/superpowers/specs/2026-10-07-lilac-float-types-and-width-intrinsics-design.md`

## Global Constraints

- Compiler crates stay Rust std-only; add no dependencies. `unsafe_code = "forbid"` and `clippy::pedantic = deny` hold.
- Operators stay hook-based; no operator behavior change.
- `@truncate`/`@extend_zero` and all non-numeric intrinsics keep their names.
- No aliases for the renamed intrinsics: the old names are removed.
- `@fbit128` constant folding is deferred; runtime 128-bit arithmetic is in scope.
- Gates: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`.

## Review Focus

- A builtin type is added to `Types::builtin` but missed in `mangle`/`display`/`valid_type`/`lir_type`, so it resolves but ICEs in codegen — pinned by the Task 1 end-to-end test.
- A renamed intrinsic is missed in one of the cooperating tables (typeck `lookup_intrinsic`, lowering `arith_op_for`, or a fixture source), leaving a live call to a dead name — pinned by the Task 2 sweep (`grep -rn '@i_add\b\|@f_add\b' …` must be empty) and the stdlib test.
- Float `Arith`/`Cmp` still carry the old bit-container width contract after the switch, so a real `#fbit32` op trips the IR verifier — pinned by the Task 2 float fixtures, which now compile to `#fbit` ops.
- An `extern` float signature passes `is_c_abi_type` but the backend has no `extern_c_type` arm, so it ICEs — pinned by the Task 4 extern test.
- The stdlib float files still wrap `@bit32`/`@bit64`, so `@f_add_32` rejects their `.data` — pinned by the Task 2 stdlib test.

---

### Task 1: Float builtin types and real-fbit lowering

**Files:**
- Modify: `crates/lilac-ty/src/lib.rs` (`TypeKind`, `Types::new`, `Types::builtin`, `Types::mangle`, `Types::display`)
- Modify: `crates/lilac-ir/src/lib.rs` (`valid_type`, `check_arith_width`, `ConvertOp`)
- Modify: `crates/lilac-lower/src/lib.rs` (`lir_ty`, `lower_convert`)
- Modify: `crates/lilac-codegen-llvm/src/lower.rs` (`lir_type`, `arith`, `float_type`, `lower_convert`)
- Test: unit tests in each of the above crates

**Interfaces:**
- Consumes: nothing.
- Produces: `TypeKind::FBit16/32/64/128`; `Types::builtin("@fbit16"|"@fbit32"|"@fbit64"|"@fbit128")`; LIR spellings `#fbit16/32/64/128`; `ConvertOp::Bitcast`.

- [ ] **Step 1: Write the failing `lilac-ty` test**

Add to `crates/lilac-ty/src/lib.rs` tests:

```rust
#[test]
fn float_builtins_intern_parse_mangle_and_display() {
    let mut types = Types::new();
    for (name, mangle, display) in [
        ("@fbit16", ".fbit16", "@fbit16"),
        ("@fbit32", ".fbit32", "@fbit32"),
        ("@fbit64", ".fbit64", "@fbit64"),
        ("@fbit128", ".fbit128", "@fbit128"),
    ] {
        let id = types.builtin(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(types.mangle(id), mangle);
        assert_eq!(types.display(id), display);
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p lilac-ty float_builtins_intern_parse_mangle_and_display`
Expected: FAIL (`builtin("@fbit16")` returns `None`).

- [ ] **Step 3: Add the float kinds**

In `crates/lilac-ty/src/lib.rs`, add `FBit16`, `FBit32`, `FBit64`, `FBit128` to `TypeKind`; intern them in `Types::new` (fixed order, after the integer builtins); accept the four `@fbitN` names in `Types::builtin`; map them in `Types::mangle` and `Types::display`.

- [ ] **Step 4: Run the `lilac-ty` test to verify it passes**

Run: `cargo test -p lilac-ty float_builtins_intern_parse_mangle_and_display`
Expected: PASS.

- [ ] **Step 5: Write the failing IR-verifier and LLVM tests**

In `crates/lilac-ir/src/lib.rs` tests, assert `valid_type("#fbit16"|"#fbit32"|"#fbit64"|"#fbit128")` is true. In `crates/lilac-codegen-llvm/src/lower.rs` tests, assert `lir_type` maps each fbit to `half`/`float`/`double`/`fp128`, and that a hand-built `Convert { op: Bitcast }` between `#bit32` and `#fbit32` emits an LLVM `bitcast` (follow the existing `lir_type`/`lower_convert` test style). This task is **purely additive**: it does not change float arithmetic, so the legacy `@f_add` bit-pattern path still works.

- [ ] **Step 6: Run them to verify they fail**

Run: `cargo test -p lilac-ir -p lilac-codegen-llvm fbit`
Expected: FAIL (`#fbit32` is not a valid type / `lir_type` ICEs / `ConvertOp::Bitcast` does not exist).

- [ ] **Step 7: Add `#fbit*` to IR and LLVM, and the `Bitcast` convert op**

- `lilac-ir`: `valid_type` accepts `#fbit16/32/64/128`; add `ConvertOp::Bitcast`. (Do **not** change `check_arith_width` or the float `Arith` contract in this task.)
- `lilac-lower`: `lir_ty` maps `FBitN` → `#fbitN`; `lower_convert` handles `ConvertOp::Bitcast`.
- `lilac-codegen-llvm`: `lir_type` maps `#fbit16`→`f16_type`, `#fbit32`→`f32_type`, `#fbit64`→`f64_type`, `#fbit128`→`f128_type`; `lower_convert` emits `build_bit_cast` for `Bitcast`. Leave the float `arith`/`cmp` arms (which still reinterpret bit containers) unchanged.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p lilac-ir -p lilac-codegen-llvm fbit`
Expected: PASS.

- [ ] **Step 9: Run the full suite and gates**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green with no behavior change (float fixtures still use `@bit32`/`@bit64` bit-patterns with the old `@f_add`; the new fbit path is covered by unit tests only, so no snapshot changes yet).

- [ ] **Step 10: Commit**

```bash
git add crates/lilac-ty crates/lilac-ir crates/lilac-lower crates/lilac-codegen-llvm
git commit -m "types: add @fbit16/32/64/128 builtin float types"
```

---

### Task 2: Width-suffixed numeric intrinsics

**Files:**
- Modify: `crates/lilac-typeck/src/sigs.rs` (`Intrinsic`, `lookup_intrinsic`)
- Modify: `crates/lilac-typeck/src/check.rs` (`check_intrinsic`, remove `arith_out`/`unify_width`)
- Modify: `crates/lilac-lower/src/lib.rs` (`arith_op_for`)
- Modify: `crates/lilac-const/src/lib.rs` (any intrinsic-name match)
- Modify: `lib/Standard/int.lilac`, `int8/16/32/64.lilac`, `float32/64.lilac`, `bool.lilac`
- Modify: every source using the old names — `tests/ui/*.lilac` and unit-test source strings under `crates/*/src/`
- Modify: regenerate `tests/ui/*.lir`, `tests/ui/*.ll` for affected fixtures

**Interfaces:**
- Consumes: Task 1 (`@fbitN`, `#fbitN`).
- Produces: the exact-signature intrinsic set from spec §5 (`@i_add_8`…`@i_add_native`, `@f_add_16`…`@f_add_128`, `@i_less_than_8`, `@f_negate_32`, …); `@i_add`/`@f_add` and all unsuffixed numeric names are gone.

- [ ] **Step 1: Find every old-name user**

Run: `git grep -n -E "@i_(add|sub|mul|div|div_u|mod|mod_u|negate|less_than|less_than_u|greater_than|greater_than_u)\b|@f_(add|sub|mul|div|mod|negate|less_than|greater_than)\b" -- crates tests lib`
Expected: the list of files to update (compiler tables, stdlib, fixtures, unit-test sources).

- [ ] **Step 2: Write the failing typeck test**

Add to `crates/lilac-typeck/src/check.rs` tests a case that `@i_add_8(0x1, 0x2)` typechecks to `@bit8` and `@i_add_8(0x1, 0x2)` on `@bit16` operands is `E010`, plus `@f_add_32(0x1, 0x2)` requires `@fbit32` operands.

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test -p lilac-typeck width_suffixed`
Expected: FAIL (`@i_add_8` is unresolved).

- [ ] **Step 4: Replace the intrinsic table and exact-signature checking, and switch float ops to real fbits**

- `sigs.rs`: replace the `Arith`/`Cmp`/`Negate` `Intrinsic` variants and `lookup_intrinsic` names with the width-suffixed set (spec §5).
- `check.rs`: `check_intrinsic` checks each numeric argument against its exact parameter type and returns the exact result type; delete `arith_out` and `unify_width`.
- `lilac-ir`: `check_arith_width` now requires **fbit** widths for `FAdd/FSub/FMul/FDiv/FMod/FLt/FGt` (the bit-container acceptance for float ops is removed).
- `lilac-codegen-llvm`: the float `arith`/`cmp` arms read their operands as float values directly (`build_float_add`, …; comparisons `OLT`/`OGT` widened to `@bit8`) and drop the legacy `bit_as_float`/`build_bit_cast` round-trip.
- `lilac-lower`: `arith_op_for` maps the suffixed names to the existing `ArithOp`s.
- `lilac-const`: update any intrinsic-name match to the suffixed names.

- [ ] **Step 5: Run the typeck test to verify it passes**

Run: `cargo test -p lilac-typeck width_suffixed`
Expected: PASS.

- [ ] **Step 6: Update `lib/Standard` and every fixture/unit-test source**

Rewrite the old names to the suffixed ones with the correct width: `int*.lilac` → `@i_*_8/16/32/64` (`int.lilac` → `_native`); `float32/64.lilac` → `@f_*_32/64` and wrap `@fbit32`/`@fbit64` instead of `@bit32`/`@bit64`; `bool.lilac` and comparison hooks → the suffixed comparison names. Update `tests/ui/*.lilac` and unit-test source strings.

- [ ] **Step 7: Regenerate affected snapshots**

Run: `cargo test --workspace` and, for each failing `.lir`/`.ll` snapshot, regenerate it by running the fixture through `lilac emit-ir`/`emit-llvm` without `--check` (as the existing snapshot tests instruct), then re-run.

- [ ] **Step 8: Verify no old name survives and the suite is green**

Run: `git grep -n -E "@i_(add|sub|mul|div|div_u|mod|mod_u|negate|less_than|less_than_u|greater_than|greater_than_u)\b|@f_(add|sub|mul|div|mod|negate|less_than|greater_than)\b" -- crates tests lib`
Expected: no matches.

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green.

- [ ] **Step 9: Commit**

```bash
git add crates lib tests
git commit -m "typeck: width-suffix the numeric intrinsics"
```

---

### Task 3: fbit↔bit reinterpret bitcasts

**Files:**
- Modify: `crates/lilac-typeck/src/sigs.rs` (`Intrinsic`, `lookup_intrinsic`)
- Modify: `crates/lilac-typeck/src/check.rs` (`check_intrinsic` / `converting_out`)
- Modify: `crates/lilac-lower/src/lib.rs` (lowering to `ConvertOp::Bitcast`)
- Modify: `crates/lilac-const/src/lib.rs` (fold a constant operand)
- Test: unit tests in `lilac-typeck`, `lilac-const`, `lilac-codegen-llvm`

**Interfaces:**
- Consumes: Task 1 (`ConvertOp::Bitcast`), Task 2 (the suffixed surface).
- Produces: `@fbit_to_bits_N(@fbitN): @bitN` and `@bits_to_fbit_N(@bitN): @fbitN` for N ∈ 16/32/64/128.

- [ ] **Step 1: Write the failing test**

Add a `lilac-typeck` test that `@fbit_to_bits_32(@bits_to_fbit_32(x))` typechecks when `x: @bit32`, and a `lilac-const` test that `@fbit_to_bits_32` of a constant folds to the reinterpreted bits.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p lilac-typeck -p lilac-const bitcast`
Expected: FAIL (unresolved intrinsic).

- [ ] **Step 3: Add the cast intrinsics**

Register the eight names in `sigs.rs`/`lookup_intrinsic`; in `check.rs` check the operand against the exact source type and return the exact destination type; in `lilac-lower` lower to `ConvertOp::Bitcast`; in `lilac-const` fold a constant operand by reinterpreting the bits.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lilac-typeck -p lilac-const bitcast`
Expected: PASS.

- [ ] **Step 5: Run the full suite and gates**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates
git commit -m "typeck: add fbit<->bit reinterpret bitcasts"
```

---

### Task 4: C ABI float types

**Files:**
- Modify: `crates/lilac-ir/src/lib.rs` (`is_c_abi_type`)
- Modify: `crates/lilac-codegen-llvm/src/lower.rs` (`extern_c_type`)
- Modify: `docs/errors/E028.md` (accepted-type list)
- Test: `crates/lilac-codegen-llvm` / `crates/lilac-cli` extern test

**Interfaces:**
- Consumes: Task 1 (`#fbitN`, `lir_type`).
- Produces: `is_c_abi_type` accepts `#fbit16/32/64/128`; `extern_c_type` maps them to `half`/`float`/`double`/`fp128`.

- [ ] **Step 1: Write the failing test**

Add a codegen test that an `extern` declared with a `@fbit64` parameter and a `@fbit32` return lowers without `E028` and emits `double`/`float`; add the inverse assertion that a `@byte_buffer` parameter is still `E028`.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p lilac-codegen-llvm extern_float`
Expected: FAIL (`E028` / missing `extern_c_type` arm).

- [ ] **Step 3: Extend the accept-set and mapping**

Add `#fbit16/32/64/128` to `is_c_abi_type`; add the four arms to `extern_c_type` (`f16`/`f32`/`f64`/`fp128`); update `docs/errors/E028.md`'s accepted list to include the float types.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p lilac-codegen-llvm extern_float`
Expected: PASS.

- [ ] **Step 5: Run the full suite and gates**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates docs/errors/E028.md
git commit -m "codegen: accept float types in the C ABI"
```

---

## Self-Review

**Spec coverage:** §4 types → Task 1; §5 width intrinsics → Task 2; §6 casts → Task 3; §7 typeck → Tasks 2–3; §8 lowering/IR → Tasks 1–3; §9 LLVM → Tasks 1–4; §10 C ABI → Task 4; §11 const → Tasks 1,3; §12 stdlib → Task 2; §13 testing → all tasks; §14 gates → all tasks. No section is unmapped.

**Ordering:** Task 1 adds types/lowering with no behavior change (float fixtures still use the old bit-pattern path), so it is green. Task 2 is the atomic rename (compiler + stdlib + fixtures + snapshots together). Tasks 3–4 build on it.

**Type/name consistency:** `@fbitN`/`#fbitN`, `ConvertOp::Bitcast`, and the suffixed intrinsic names are spelled identically wherever they appear across tasks.
