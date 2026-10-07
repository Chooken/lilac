# Lilac Real Literals Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make numeric literals real typed values — integer literals smallest-fit grow-only, negative literals folded to two's-complement constants, float literals defaulting to `@fbit64`, and literal-only arithmetic folded at 64-bit — and remove the `@as_int`/`@as_float` intrinsics and the `lilac-decimal` crate.

**Architecture:** Add exponent lexing, then make literal typing/folding real in `lilac-typeck` (which knows the consumed width). The literal change is coupled to retiring the `@byte_buffer`-numeric path, so typing + the `@as_int`/`@as_float` removal + the stdlib conversions land together. Remove the pre-typeck `lilac-decimal` fold last.

**Tech Stack:** Rust workspace; compiler crates std-only; LLVM confined to `lilac-codegen-llvm`.

**Spec:** `docs/superpowers/specs/2026-10-07-lilac-real-literals-design.md`

## Global Constraints

- Compiler crates stay Rust std-only; add no dependencies. `unsafe_code = "forbid"` and `clippy::pedantic = deny` hold.
- Operators on non-literal operands stay hook-based; only all-literal `+ - * / %` folds.
- `@truncate`/`@extend_zero` and every other intrinsic are unchanged; the C ABI and the sub-project-1 type/intrinsic surface are unchanged.
- No `as` operator (sub-project 3); no string→number parsing; no `@bit128`.
- Gates: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`.

## Review Focus

- A decimal literal still reaching the old `@byte_buffer` path, so `x: Int8 = 5` or the stdlib still depends on a removed conversion — pinned by the Task 2 stdlib test and the `@as_int`/`@as_float` fixture deletion.
- A literal width chosen from the signed range for a positive value (or vice versa), so `0xFF` is `@bit16` or `-128` is `@bit16` — pinned by the Task 2 width tests.
- Float literals silently typed `@bit` or vice versa — pinned by the Task 2 float tests.
- Literal folding not range-checked, so `x: @bit8 = 200 + 100` compiles — pinned by the Task 4 `E060` test.
- `lilac-decimal` removed but still referenced by the driver or a CLI flag — pinned by Task 5's build and the `DecimalPolicy` grep.

---

### Task 1: Lexer exponent forms

**Files:**
- Modify: `crates/lilac-lex/src/lib.rs` (`lex_number`)
- Test: `crates/lilac-lex/src/lib.rs` tests

**Interfaces:**
- Consumes: nothing.
- Produces: a `Number` token for `3.14`, `1e3`, `1.5e-4`, `2E10` (verbatim source text).

- [ ] **Step 1: Write the failing lexer test**

Add a test asserting `lex("1e3")`, `lex("1.5e-4")`, `lex("2E10")`, `lex("3.14")` each yield one `Number` token with the exact source text, and that `lex("1.")` and `lex("1e")` are lex errors.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p lilac-lex exponent`
Expected: FAIL (`1e3` lexes as `Number("1")` + ident, or errors).

- [ ] **Step 3: Add exponent handling**

In `lex_number`, after the integer/fraction part, accept an optional exponent: `e`/`E`, an optional `+`/`-`, then at least one digit; if `e`/`E` is present without following digits, or a `.` is not followed by a digit, produce the existing malformed-number error.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p lilac-lex exponent`
Expected: PASS.

- [ ] **Step 5: Full gates and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green (typeck still treats these tokens as before; no behavior change yet).

```bash
git add crates/lilac-lex
git commit -m "lex: accept exponent float literals"
```

---

### Task 2: Real literal typing; retire the byte-buffer numeric path

**Files:**
- Modify: `crates/lilac-typeck/src/check.rs` (`check_number_literal`; the literal helpers)
- Modify: `crates/lilac-typeck/src/sigs.rs` (remove `Intrinsic::AsInt`/`AsFloat`; `lookup_intrinsic`)
- Modify: `crates/lilac-typeck/src/check.rs` (`converting_out` / `check_intrinsic` arms)
- Modify: `crates/lilac-lower/src/lib.rs` (remove the `@as_int`/`@as_float` lowering)
- Modify: `crates/lilac-const/src/lib.rs` (remove `eval_as_int`/`eval_as_float`)
- Modify: `lib/Standard/int.lilac`, `int8/16/32/64.lilac`, `float32.lilac`, `float64.lilac`
- Delete: `tests/ui/as_int_*`, `tests/ui/as_float_*`, `tests/as_int_*`, `tests/as_float_*`, `tests/FAILS_as_int_overflow.lilac`
- Modify: regenerate affected `tests/ui/*.lir`/`*.ll`; delete their `as_int`/`as_float` snapshots

**Interfaces:**
- Consumes: Task 1 (exponent tokens).
- Produces: an integer literal is the smallest `@bit8/16/32/64` holding its magnitude, grown to an expected bit type (`E060` on overflow); a float literal is `@fbit64` or the expected `@fbitN` (nearest-even; `E060` past max finite); `@as_int`/`@as_float` are gone.

- [ ] **Step 1: Write the failing typeck tests**

Add tests asserting: `0`→`@bit8`, `0xFF`→`@bit8`, `0x1FF`→`@bit16`, `0x1_0000_0000`→`@bit64`, `0x1_0000_0000_0000_0000`→`E060`; `x: @bit32 = 0x1` and `x: @bit32 = 1` are `@bit32`; `x: @bit8 = 0x1FF` is `E060`; `3.14`→`@fbit64`; `x: @fbit32 = 3.14`→`@fbit32`; a float literal in a `@bit` slot is `E010`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p lilac-typeck literal`
Expected: FAIL (decimal is `@byte_buffer`; hex/bin needs an expected type).

- [ ] **Step 3: Implement real literal typing**

In `check_number_literal`, classify the source bytes: hex/bin/decimal-without-`.`/`e` → integer (smallest-fit by unsigned magnitude over `@bit8/16/32/64`, grow-only to the expected bit type, `E060` on overflow); a literal containing `.` or `e`/`E` → float (`@fbit64` default, expected `@fbitN` pins, nearest-even rounding, `E060` past max finite). Remove the `E024` "needs an expected bit type" path. Keep `@bit_native` growing the literal to native width.

- [ ] **Step 4: Run the typeck tests to verify they pass**

Run: `cargo test -p lilac-typeck literal`
Expected: PASS.

- [ ] **Step 5: Remove `@as_int`/`@as_float` and the byte-buffer conversions**

Remove `Intrinsic::AsInt`/`AsFloat` from `sigs.rs`/`lookup_intrinsic` and their `check_intrinsic`/`converting_out` handling; remove the `@as_int`/`@as_float` arms from `lilac-lower` and `eval_as_int`/`eval_as_float` from `lilac-const`. In `lib/Standard`: replace `int*.lilac`'s `@conversion(from: @byte_buffer)` with `@conversion(from: @bitN)` (`@bit_native` for `int.lilac`), and `float32/64.lilac`'s with `@conversion(from: @fbit32/64)`.

- [ ] **Step 6: Delete the fixtures and regenerate snapshots**

Delete the `as_int`/`as_float` fixtures and snapshots (above). Regenerate affected `tests/ui/*.lir`/`*.ll` by running each fixture through `lilac emit-ir`/`emit-llvm` without `--check`.

- [ ] **Step 7: Verify and run the full gates**

Run: `git grep -n -E "@as_int|@as_float" -- crates tests lib`
Expected: no matches.

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green.

- [ ] **Step 8: Commit**

```bash
git add crates lib tests
git commit -m "typeck: real literal typing; remove @as_int/@as_float"
```

---

### Task 3: Negative literal folding

**Files:**
- Modify: `crates/lilac-typeck/src/check.rs` (the `@negate`-with-literal case)
- Test: `crates/lilac-typeck/src/check.rs` tests

**Interfaces:**
- Consumes: Task 2 (integer literal typing).
- Produces: `-<integer-literal>` is a two's-complement constant, signed smallest-fit, grow-only; `@negate` on a non-literal is unchanged.

- [ ] **Step 1: Write the failing test**

Add tests: `-1`→`@bit8` value `0xFF`; `-128`→`@bit8`; `-129`→`@bit16`; `x: @bit16 = -1`→`@bit16`; `@i_less_than_8(-1, 0)`-style signed comparison is `true`.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p lilac-typeck negative_literal`
Expected: FAIL (`-1` is a `@negate` hook lookup).

- [ ] **Step 3: Implement the fold**

When checking a `@negate` whose single argument is an integer literal, produce a two's-complement constant typed by the smallest width whose **signed** range holds `-magnitude`, grown to an expected bit type (never shrunk; `E060` on overflow), instead of resolving the `@negate` hook. Leave `@negate` on non-literals unchanged.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p lilac-typeck negative_literal`
Expected: PASS.

- [ ] **Step 5: Full gates and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green.

```bash
git add crates
git commit -m "typeck: fold negative integer literals"
```

---

### Task 4: Literal-only arithmetic folding

**Files:**
- Modify: `crates/lilac-typeck/src/check.rs` (the binop/hook-call path)
- Test: `crates/lilac-typeck/src/check.rs` tests

**Interfaces:**
- Consumes: Tasks 2–3.
- Produces: an all-literal `+ - * / %` folds at i64/f64 then fits to the consumed width; integer results range-checked (`E060`); divide/modulo by zero is `E060`; mixed int/float is `E010`.

- [ ] **Step 1: Write the failing test**

Add tests: `2 + 3`→`5: @bit8`; `200 + 100`→`300: @bit16`; `x: @bit8 = 200 + 100`→`E060`; `2 + 3.0`→`E010`; `5 / 0`→`E060`; `2 + x` (x a variable) still resolves the `@add` hook.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p lilac-typeck literal_fold`
Expected: FAIL (`2 + 3` is an `@add` hook lookup).

- [ ] **Step 3: Implement folding**

When a `+ - * / %` operator call has both operands as literals of the same kind, fold at i64/f64 and type the result by Task 2's rules (grow to the consumed width; `E060` on integer overflow or divide/modulo by zero). A mixed int/float literal pair is `E010`. Any non-literal operand keeps today's hook resolution.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p lilac-typeck literal_fold`
Expected: PASS.

- [ ] **Step 5: Full gates and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green.

```bash
git add crates
git commit -m "typeck: fold literal-only arithmetic"
```

---

### Task 5: Remove the `lilac-decimal` crate

**Files:**
- Delete: `crates/lilac-decimal/`
- Modify: `crates/lilac-driver/src/lib.rs` (remove the `lilac_decimal::fold` calls and the dependency)
- Modify: `crates/lilac-driver/Cargo.toml` (drop the `lilac-decimal` dep)
- Modify: `crates/lilac-driver/src/lib.rs`, `crates/lilac-cli/src/main.rs` (remove `DecimalPolicy`/`DecimalRound` and `--decimal-scale=`/`--decimal-round=` if they have no other consumer)

**Interfaces:**
- Consumes: Task 4 (folding now in typeck).
- Produces: no pre-typeck numeric fold pass.

- [ ] **Step 1: Confirm the knobs' consumers**

Run: `git grep -n -E "DecimalPolicy|DecimalRound|decimal_scale|decimal-round|decimal-scale|session\.decimal"`
Expected: only the driver/CLI fold wiring (and the `lilac-decimal` crate). If a consumer outside that exists, keep the type and remove only the fold and the CLI flags.

- [ ] **Step 2: Delete the crate and its wiring**

Delete `crates/lilac-decimal/`; remove the driver's `lilac_decimal::fold(...)` calls (project and single-file) and the dependency; remove the decimal session options and CLI flags if Step 1 showed no other consumer.

- [ ] **Step 3: Verify no dangling reference and gates**

Run: `git grep -n -E "lilac-decimal|lilac_decimal" -- crates`
Expected: no matches.

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green (the typeck fold from Task 4 replaces the AST fold; decimal `2 + 3` still yields `5`).

- [ ] **Step 4: Commit**

```bash
git add -A Cargo.lock
git commit -m "driver: remove the lilac-decimal fold"
```

---

## Self-Review

**Spec coverage:** §4 int literals → Task 2; §5 negative → Task 3; §6 float → Task 1 (lex) + Task 2 (type); §7 folding → Task 4; §8 `@as_int`/`@as_float` removal → Task 2; §9 `lilac-decimal` → Task 5; §10 stdlib → Task 2; §11 testing → all; §12 gates → all. No section unmapped.

**Ordering:** Task 1 is lexer-only (no behavior change). Task 2 is the atomic literal+removal change (typing, `@as_int`/`@as_float` removal, stdlib conversions, fixtures). Tasks 3–5 build on it; Task 5 removes the now-subsumed AST fold.

**Type/name consistency:** `@bit8/16/32/64/native`, `@fbit16/32/64/128`, `check_number_literal`, `E060`/`E010`, `@negate`, `@conversion` are spelled identically wherever they appear.
