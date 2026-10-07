# Lilac Float Types & Width-Suffixed Intrinsics

> Scope: make `@fbit16/32/64/128` real builtin types; replace the
> width-polymorphic numeric intrinsics with per-width names
> (`@i_add_8`…`@i_add_native`, `@f_add_16`…`@f_add_128`); add per-width
> fbit↔bit reinterpret bitcasts; and extend the C ABI to the float widths.
> First of three numeric sub-projects (then real literals, then `as`).
> Branch `ai`. LLVM-only (the VM backend is removed).

## 1. Outcome / constraints / success

**Outcome**

- A float value has a float type: `@fbit16`, `@fbit32`, `@fbit64`, or
  `@fbit128`. `@i_add_32(@fbit32, …)` is a type error.
- Every arithmetic, comparison, and negate intrinsic names its width:
  `@i_add_8`, `@i_sub_8`, … `@i_add_native`, `@f_add_16`, … `@f_add_128`,
  `@i_less_than_8`, `@f_negate_32`, etc. Each accepts exactly its own width;
  the old unsuffixed names (`@i_add`, `@f_add`, …) are gone.
- `@fbit_to_bits_N(@fbitN): @bitN` and `@bits_to_fbit_N(@bitN): @fbitN`
  reinterpret the bits at each width.
- `extern` accepts `@fbit16/32/64/128` (LLVM `half`/`float`/`double`/`fp128`).
- `lib/Standard` uses the new names and real float types; the stdlib CLI test
  still checks and emits.

**Constraints**

- Compiler crates stay Rust std-only; no new dependencies. `unsafe_code` and
  `clippy::pedantic` hold.
- Operators (`+ - * / % < > <= >=`) stay hook-based (SPEC §8); the width
  intrinsics are called explicitly. No operator behavior change.
- `@truncate`/`@extend_zero` (bit↔bit width changes) and all non-numeric
  intrinsics keep their current names.
- Existing behavior is preserved for `@bit8`/`@bit16`/`@bit32`/`@bit64`/
  `@bit_native` integer code, modulo the intrinsic rename.

**Success**

- `@i_add_8(@bit8, @bit8): @bit8` … `@i_add_native` typecheck with exact
  operands; a width mismatch is `E010` (no auto-unification).
- `@f_add_32(@fbit32, @fbit32): @fbit32` typechecks; `@f_add_32` on `@bit32`
  is `E010`.
- `@fbit_to_bits_32(@bits_to_fbit_32(x))` round-trips at every width.
- An `extern` with a `@fbit64` parameter and a `@fbit32` return is accepted;
  LLVM emits `double`/`float`.
- `lilac check lib/Standard` and `lilac emit-ir lib/Standard` exit 0.
- `cargo test --workspace` is green (with refreshed snapshots).

**Non-goals**

- Real int/float literals, exponent literals, smallest-fit/grow-only typing,
  and turning `@as_int`/`@as_float` into string→number parsers (sub-project 2).
- The `as` operator (sub-project 3).
- `@fbit128` **constant folding** of `@as_float` (quad precision): deferred;
  `@as_float` keeps folding 16/32/64 and reports a clear error for 128 until a
  later sub-project. Runtime `@fbit128` arithmetic is in scope.
- A `Float128` stdlib type (none exists today).

### 1.1 Decisions locked during brainstorming

1. **`@fbit16/32/64/128` are distinct real types**, with full runtime support
   including 128-bit.
2. **Hard rename, no aliases**: `@i_add_8`-style, `_native` for `@bit_native`.
3. **Only Arith/Cmp/Negate are width-suffixed**; conversions and every other
   intrinsic keep their names.
4. **fbit↔bit casts are per-width reinterpret bitcasts** (pure bitcast, same
   width).
5. **C ABI**: `@fbit16`→`half`, `@fbit32`→`float`, `@fbit64`→`double`,
   `@fbit128`→`fp128`; all four extern-able.
6. **`@as_float` produces `@fbitN`** now (interim; becomes a string parser in
   sub-project 2).
7. **`lib/Standard` is updated here** to the new names and real float types.

## 2. Problem

Today there are **no float types**. A float is a `@bit32`/`@bit64` container
whose bits the `@f_*` intrinsics reinterpret (SPEC §1). The LLVM backend
`bit_as_float`s the integer bits, does the float op, and `bit_cast`s back
(`lilac-codegen-llvm/src/lower.rs:1048-1089`). This makes C `float`/`double`
interop impossible (`is_c_abi_type` rejects any float spelling) and forces the
type checker to infer widths.

The numeric intrinsics are width-polymorphic: `@i_add` accepts any bit width,
and `arith_out`/`unify_width` (`lilac-typeck/src/check.rs:4256-4455`) seed the
operand width from a sibling or the expected type. A concrete per-width
signature removes that inference: each intrinsic has one exact signature, and
width changes become explicit conversions.

## 3. Architecture / components

```
crates/lilac-ty/src/lib.rs              # TypeKind::FBit16/32/64/128; intern; builtin; mangle; display
crates/lilac-typeck/src/sigs.rs         # Intrinsic names -> width-suffixed; lookup_intrinsic table
crates/lilac-typeck/src/check.rs        # exact-signature numeric check; float width helpers;
                                        #   bitcast intrinsics; drop arith_out/unify_width inference
crates/lilac-lower/src/lib.rs           # lir_ty fbits; arith_op_for suffixed names; bitcast lowering
crates/lilac-ir/src/lib.rs              # valid_type #fbit*; check_arith_width; is_c_abi_type
crates/lilac-codegen-llvm/src/lower.rs  # lir_type half/float/double/fp128; float ops on fbits;
                                        #   extern_c_type fbits
crates/lilac-const/src/lib.rs           # float constants carry width; @as_float -> @fbitN
crates/lilac-cli/tests/stdlib.rs        # unchanged; must stay green
lib/Standard/int*.lilac                 # @i_*_N, @i_negate_N
lib/Standard/float32.lilac, float64.lilac  # wrap @fbit32/64; @f_*_32/64
tests/ui/*.lir, *.ll                    # regenerated snapshots
```

**Untouched:** `lilac-lex`, `lilac-parse`, `lilac-ast`, `lilac-hir` (no HIR
variant change), `lilac-resolve`, `lilac-module`, `lilac-codegen`,
`lilac-lsp`, `lilac-driver` (beyond nothing), `lilac-target`.

## 4. Builtin types

`crates/lilac-ty/src/lib.rs`:

- `TypeKind` gains `FBit16`, `FBit32`, `FBit64`, `FBit128`.
- `Types::new` interns them after the integer builtins (index order fixed).
- `Types::builtin` accepts `"@fbit16"`, `"@fbit32"`, `"@fbit64"`,
  `"@fbit128"`.
- `Types::mangle` → `.fbit16` … `.fbit128`; `Types::display` → `@fbit16` ….

**LIR type spellings:** `#fbit16`, `#fbit32`, `#fbit64`, `#fbit128`.

## 5. Width-suffixed numeric intrinsics

The intrinsic table (`sigs.rs`) and every dispatch in typeck/lowering replace
the unsuffixed names with the width-suffixed set. Signatures are exact:

| Intrinsic (per width `W` ∈ `8`,`16`,`32`,`64`,`native`) | Signature |
| --- | --- |
| `@i_add_W`, `@i_sub_W`, `@i_mul_W`, `@i_div_W`, `@i_mod_W` | `(@bitW, @bitW): @bitW` |
| `@i_div_u_W`, `@i_mod_u_W` | `(@bitW, @bitW): @bitW` |
| `@i_negate_W` | `(@bitW): @bitW` |
| `@i_less_than_W`, `@i_greater_than_W` | `(@bitW, @bitW): @bit8` |
| `@i_less_than_u_W`, `@i_greater_than_u_W` | `(@bitW, @bitW): @bit8` |

| Intrinsic (per width `W` ∈ `16`,`32`,`64`,`128`) | Signature |
| --- | --- |
| `@f_add_W`, `@f_sub_W`, `@f_mul_W`, `@f_div_W`, `@f_mod_W` | `(@fbitW, @fbitW): @fbitW` |
| `@f_negate_W` | `(@fbitW): @fbitW` |
| `@f_less_than_W`, `@f_greater_than_W` | `(@fbitW, @fbitW): @bit8` |

`@bitW` is the integer builtin of width `W` (`@bit_native` for `native`);
`@fbitW` is the float builtin of width `W`. `@fbit128` has no `native`
analogue.

**Checking.** A numeric intrinsic checks each argument against its exact
parameter type (`check_expr(arg, Some(param))` + `expect_type`), and returns
its exact result type. There is no `unify_width`/`arith_out` width inference:
`arith_out` and `unify_width` are removed (they served only these intrinsics).
Comparison still returns `@bit8`. Literals in an argument position adopt the
parameter width through the existing expected-type path.

## 6. fbit↔bit casts

New intrinsics (exact signatures), per width `W` ∈ `16`,`32`,`64`,`128`:

- `@fbit_to_bits_W(@fbitW): @bitW`
- `@bits_to_fbit_W(@bitW): @fbitW`

They are **reinterpret** casts (IEEE-754 bits ↔ integer bits), lowered to an
LLVM `bitcast` between the float type and the integer type of the same width.
They are runtime values (not fold-required), but a constant operand folds to
the reinterpreted constant. A `Bitcast` convert op is added to LIR.

## 7. Typeck

- `is_bit` stays (integer containers). Add `is_float` and `float_width`
  (`FBit16=>16`, …), mirroring `bit_width`.
- `check_intrinsic` dispatches the suffixed names to exact-signature checking
  (no width unification).
- `converting_out` handles the four cast intrinsics (reinterpret) alongside
  `@as_int`/`@as_float`/`@truncate`/`@extend_zero`; `@as_float`'s result is an
  `@fbitN` (N ∈ 16/32/64).
- `expect_type`/`convert_if_needed`/`ops::convert` are unchanged: `@fbitN` is
  just another type, and conversions to/from it are user `@conversion` hooks
  (e.g. the stdlib's `Float32` from `@byte_buffer`).

## 8. Lowering and LIR

- `lir_ty` maps `FBitN` → `#fbitN`.
- `arith_op_for` maps the suffixed names to the existing `ArithOp`s
  (`@i_add_8`→`IAdd`, `@f_add_32`→`FAdd`, …); the operand/result types are the
  bit/fbit containers now, so `Arith`/`Cmp`/`Negate` no longer reinterpret.
- The cast intrinsics lower to a new `LirInst::Convert { op: Bitcast, … }`.
- `lilac-ir`: `valid_type` accepts `#fbit16/32/64/128`; `check_arith_width`
  accepts the fbit widths for `FAdd…FGt` and the bit widths for `I*`;
  `is_c_abi_type` accepts `#fbit16/32/64/128` by value.

## 9. LLVM backend

- `lir_type`: `#fbit16`→`f16`, `#fbit32`→`f32`, `#fbit64`→`f64`,
  `#fbit128`→`fp128`.
- Float `Arith`/`Cmp`/`Negate` operate directly on the float values
  (`build_float_add`, …; comparisons `OLT`/`OGT` widened to `@bit8`); the
  `bit_as_float`/`build_bit_cast` dance is removed.
- The `Bitcast` convert op emits `build_bit_cast` between the int and float
  types.
- `extern_c_type`: `#fbit16`→`f16`, `#fbit32`→`f32`, `#fbit64`→`f64`,
  `#fbit128`→`fp128`; `extern_fn_type` uses them.

## 10. C ABI

`is_c_abi_type` (`lilac-ir`) accepts `#fbit16/32/64/128`; `extern_c_type`
(`lilac-codegen-llvm`) maps them to `half`/`float`/`double`/`fp128`. The shared
accept-set keeps `check_extern_signatures` (`lilac-lower`) and the backend
signature lowering from drifting. `E028` docs (`docs/errors/E028.md`) gain the
float types in the accepted list.

## 11. Const folding

- Float constants use `ConstValue::Float(value: f64, width: u32)` and lower to
  the matching LLVM float constant (`f16`/`f32`/`f64`); a `@fbit128` constant
  is out of scope (see below).
- `@as_float` folds a constant `@byte_buffer` to a `ConstValue::Float` of the
  expected `@fbitN` for N ∈ 16/32/64 (today it folds to an integer bit pattern;
  this changes the representation, not the accepted inputs). `@fbit128` folding
  is deferred: `@as_float` to 128 reports a clear error until a later
  sub-project.
- `@fbit_to_bits_W`/`@bits_to_fbit_W` fold a constant operand by reinterpreting
  the bits.

## 12. `lib/Standard`

- `int.lilac`, `int8/16/32/64.lilac`: `@i_add`…`@i_mod` → `@i_add_N`…; the
  `@negate` body uses `@i_negate_N`. `int.lilac` uses the `_native` variants.
- `float32.lilac`, `float64.lilac`: wrap `@fbit32`/`@fbit64` (not `@bit32`/
  `@bit64`); `@add`/… bodies call `@f_add_32`/`@f_add_64` …; `@negate` uses
  `@f_negate_32/64`; the `@conversion` from `@byte_buffer` uses `@as_float`
  producing `@fbit32/64`.
- `bool.lilac` and the int comparison hooks use the suffixed comparison names
  with the right width.

## 13. Testing

- **`lilac-ty`:** `@fbitN` interns, parses, mangles, and displays.
- **`lilac-typeck`:** each numeric intrinsic typechecks at its exact width; a
  width mismatch is `E010`; comparison returns `@bit8`; the cast intrinsics
  round-trip; `@f_add_32` on `@bit32` is `E010`.
- **`lilac-const`:** `@as_float` folds to `@fbit16/32/64`; a 128 target errors
  clearly; the cast intrinsics fold.
- **`lilac-codegen-llvm`:** `#fbit16/32/64/128` lower to
  `half/float/double/fp128`; float arithmetic emits float ops; an `extern` with
  float params/return emits the C types.
- **`lilac-cli`:** `stdlib.rs` (check + emit-ir on `lib/Standard`) stays green;
  refresh `tests/ui/*.lir`/`*.ll` snapshots affected by the rename and the
  float representation.
- All gates stay green.

## 14. Gates

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`

## 15. Maintenance / source of truth

- `Types::builtin` is the single accept-set for builtin type names; adding a
  builtin type means `TypeKind`, `Types::new`, `builtin`, `mangle`, `display`,
  `valid_type`, `lir_type`, `is_c_abi_type`, and `extern_c_type`.
- Each numeric intrinsic has exactly one signature; width changes are explicit
  conversions, never inference.

## 16. Follow-ups (separate specs)

- **Sub-project 2:** real int and float literals (`0x…`/`0b…`/decimal smallest
  fit, grow-only; `.`/exponent float literals defaulting to `@fbit64`); literal
  arithmetic folded at 64-bit then cast/fit; `@as_int`/`@as_float` become
  string→number parsers.
- **Sub-project 3:** the `as` operator (force a `@conversion`; the
  `ref unknown` ↔ any `ref T` exception).
- `@fbit128` constant folding (quad precision).
