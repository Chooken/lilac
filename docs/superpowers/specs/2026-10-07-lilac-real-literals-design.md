# Lilac Real Literals

> Scope: make numeric literals real typed values — integer literals
> (decimal/hex/binary) smallest-fit and grow-only, negative literals folded
> to two's-complement constants, float literals (`.`/exponent) defaulting to
> `@fbit64`, and literal-only arithmetic folded at 64-bit; remove the
> `@as_int`/`@as_float` intrinsics and the `lilac-decimal` crate. Second of
> three numeric sub-projects (then `as`). Branch `ai`. LLVM-only.

## 1. Outcome / constraints / success

**Outcome**

- A decimal, hex, or binary integer literal is a real literal whose type is
  the smallest `@bit8/16/32/64` holding its magnitude, grown (never shrunk)
  to an expected bit type. A magnitude needing more than 64 bits, or one that
  does not fit an expected narrower type, is `E060`.
- `-<integer-literal>` folds into a two's-complement constant with signed
  smallest-fit (`-1`→`@bit8` `0xFF`, `-128`→`@bit8` `0x80`, `-129`→`@bit16`).
- A float literal (`3.14`, `1e3`, `1.5e-4`, `2E10`) is a real literal,
  defaulting to `@fbit64`; an expected `@fbitN` pins it; inexact values round
  to nearest-even; a magnitude beyond the width's max finite is `E060`.
- Literal-only `+ - * / %` folds at i64/f64 then fits to the consumed width;
  integer results are range-checked (`E060`); mixed int/float literals are
  `E010`.
- `@as_int`/`@as_float` no longer exist; `lilac-decimal` is removed.
- `lib/Standard` conversions take literals from the bit types.

**Constraints**

- Compiler crates stay Rust std-only; no new dependencies. `unsafe_code` and
  `clippy::pedantic` hold.
- Operators on non-literal operands stay hook-based (SPEC §8); only
  all-literal arithmetic folds.
- `@truncate`/`@extend_zero` and every other intrinsic are unchanged.
- The C ABI, the type system, and the intrinsic surface from sub-project 1 are
  unchanged.

**Success**

- `x: @bit32 = 0x1` and `x: @bit32 = 1` both type as `@bit32`; `0x100000000`
  (`2^32`) unconstrained is `@bit64`; `0x1_0000_0000_0000_0000` is `E060`.
- `-1` is `@bit8` `0xFF`; `-129` is `@bit16`; `@i_less_than_8(-1, 0)`-style
  signed use works.
- `3.14` is `@fbit64`; `x: @fbit32 = 3.14` is `@fbit32` (rounded).
- `2 + 3` folds to `5: @bit8`; `200 + 100` unconstrained is `300: @bit16`;
  `x: @bit8 = 200 + 100` is `E060`; `2 + 3.0` is `E010`.
- `@as_int`, `@as_float`, and the `lilac-decimal` crate are gone.
- `lilac check lib/Standard` and `lilac emit-ir lib/Standard` exit 0.
- `cargo test --workspace` is green.

**Non-goals**

- The `as` operator (sub-project 3).
- Runtime or stdlib string→number parsing (`@byte_buffer` → `IntN`/`FloatN`).
- A `@bit128` integer type; hex-float (`0x1.8p3`) literals.

### 1.1 Decisions locked during brainstorming

1. **Integer literals are smallest-fit grow-only**: smallest `@bit8/16/32/64`
   for the magnitude; an expected bit type grows the literal to it, never
   shrinks; `>64` bits or exceeding the expected width is `E060`. Decimal
   literals are no longer `@byte_buffer`.
2. **Negative literals fold** (Rust-style): `-<literal>` is a two's-complement
   constant, signed smallest-fit, grow-only; `-` on a non-literal stays the
   `@negate` hook.
3. **Literal-only arithmetic folds** `+ - * / %` over two same-kind literals
   at i64/f64, then smallest-fit/grow to the consumed width; integer results
   range-checked; mixed int/float is `E010`.
4. **Float literals** are `.`/exponent forms, default `@fbit64`, expected
   `@fbitN` pins, nearest-even rounding, `E060` past the max finite.
5. **`@as_int`/`@as_float` are removed**; string→number parsing is deferred to
   the stdlib.
6. **`lib/Standard` conversions** become `@conversion(from: @bitN) IntN` /
   `@fbitN → FloatN`; the `@byte_buffer` conversions are dropped.
7. **Folding lives in typeck**; the `lilac-decimal` crate is removed.

## 2. Problem

Today a numeric literal carries no type. A decimal literal is a `@byte_buffer`
holding its source bytes (`check_number_literal`, `lilac-typeck/src/check.rs`),
and hex/bin literals take an expected bit type or are `E024`. A separate
pre-typeck pass (`lilac-decimal`) folds decimal `Number <op> Number` at the
AST level. This forces every numeric type to carry a `@conversion` from
`@byte_buffer`, and makes an unconstrained literal an error rather than a
value.

Now that `@bit8/16/32/64/native` and `@fbit16/32/64/128` are real types
(sub-project 1), a literal can carry a real type directly, and literal-only
arithmetic can fold with a real width.

## 3. Architecture / components

```
crates/lilac-lex/src/lib.rs             # lex_number: exponent forms
crates/lilac-typeck/src/check.rs        # real literal typing; literal-only fold; negative fold
crates/lilac-typeck/src/sigs.rs         # remove AsInt/AsFloat
crates/lilac-lower/src/lib.rs           # drop @as_int/@as_float lowering; typed number consts
crates/lilac-const/src/lib.rs           # drop eval_as_int/eval_as_float
crates/lilac-decimal/                    # deleted (crate)
crates/lilac-driver/src/lib.rs          # drop lilac_decimal::fold calls
lib/Standard/int*.lilac, float32/64.lilac  # @conversion(from: @bitN/@fbitN)
tests/ui/*.lilac, *.lir, *.ll           # update/remove; regenerate
```

**Untouched:** `lilac-ast`, `lilac-parse` (the number token stays source
bytes), `lilac-hir`, `lilac-ir`, `lilac-codegen-llvm`, `lilac-target`,
`lilac-module`, `lilac-lsp`.

## 4. Integer literals

`check_number_literal` (`lilac-typeck/src/check.rs`) types a hex/binary/decimal
integer literal from its source bytes:

- Compute the literal's magnitude and the smallest width `W ∈ {8,16,32,64}`
  whose unsigned range holds it (`0` → 8; `0xFF` → 8; `0x1FF` → 16; …).
- If an expected bit type `T` is present: if `W ≤ width(T)`, the literal is
  `T` (it **grows** to `T`, never shrinks); if `W > width(T)`, `E060`.
- If no expected bit type: the literal is `@bitW`. If `W > 64`, `E060`.
- `@bit_native` as an expected type grows the literal to native width (64 on
  a 64-bit target).
- A non-bit expected type (e.g. a user object) does not constrain the literal;
  the literal takes its smallest-fit type and the normal conversion path
  applies (`expect_type`/`@conversion`), as today.

Hex/bin literals keep their `lit_bits` overflow check against the chosen
width. The `E024` "number literal needs an expected bit type" path is removed.

## 5. Negative literals

The parser turns unary `-` into a `@negate` call. Typeck special-cases a
`@negate` whose single argument is an integer literal: it folds to a
two's-complement constant whose value is `-magnitude`, typed by the smallest
width `W` whose **signed** range holds it (`-1` → 8, `-128` → 8, `-129` →
16), grown to an expected bit type exactly as §4 (never shrunk; `E060` on
overflow). A `@negate` on a non-literal is unchanged (the user hook).

Because the containers are unsigned, the value is the two's-complement bit
pattern; the signed `@i_*` intrinsics (`@i_less_than_8` is `icmp slt`)
interpret it correctly. `@fbit` negatives are not folded this way (a negative
float literal is `-<float>` → `@negate`, or an expected-typed float).

## 6. Float literals

The lexer accepts a float form in `lex_number`: a decimal integer part, an
optional `.` with at least one following digit, and an optional exponent
`e`/`E`, an optional sign, and at least one digit. Forms: `3.14`, `1e3`,
`1.5e-4`, `2E10` (a bare `1e3` with no dot is a float).

Typeck types a float literal:

- Expected `@fbitN` (N ∈ 16/32/64): the value rounded to nearest-even at `N`;
  a magnitude beyond the width's max finite is `E060`.
- No expected float type: `@fbit64`.
- A float literal where an integer/bit type is expected is `E010` (no implicit
  float→int).

`@fbit128` literals are out of scope (no folding; consistent with
sub-project 1's deferred 128 const folding).

## 7. Literal-only folding

When a binary `+ - * / %` has **both** operands as literals of the same kind:

- Two integer literals fold at `i64`; two float literals fold at `f64`.
- The result is a single literal typed by §4 (integers) / §6 (floats): it
  grows to the expected width, and an integer result that does not fit is
  `E060`. Division/modulo by zero is `E060`.
- A mixed int/float literal pair is `E010` (no promotion).

Any non-literal operand keeps today's behavior: the operator desugars to its
`@add`/`@sub`/… hook and resolves by exact type. Folding happens in typeck, at
the point the binop is checked, so it sees the consumed width.

## 8. Removing `@as_int`/`@as_float`

- `Intrinsic::AsInt`/`AsFloat` are removed from `sigs.rs`, from
  `check_intrinsic`/`converting_out`, and from `lilac-lower` (the
  `lower_fold_intrinsic` arms); `lilac-const`'s `eval_as_int`/`eval_as_float`
  are removed.
- The `tests/ui/as_int_*`, `as_float_*` fixtures and the
  `tests/as_int_*`/`as_float_*` integration fixtures and their `.lir`/`.ll`
  snapshots are deleted, along with the `FAILS_as_int_overflow` fixture.
- `docs/errors/E060.md` and `SPEC.md` §3/§11 references to `@as_int`/
  `@as_float` are updated (the intrinsics are gone).

## 9. Removing `lilac-decimal`

- Delete `crates/lilac-decimal/`; the driver's `lilac_decimal::fold(...)` calls
  (project and single-file paths) are removed. Folding now lives in typeck
  (§7), so the AST-level decimal fold is subsumed.
- The `DecimalPolicy`/`DecimalRound` session options and the
  `--decimal-scale=`/`--decimal-round=` CLI flags configure that fold; remove
  them with it and update `lilac-driver`/`lilac-cli`. Before removing, grep for
  other consumers of `DecimalPolicy`/`DecimalRound`/`session.decimal`: if one
  exists, keep the type and only remove the fold and the CLI flags.

## 10. `lib/Standard`

- `int.lilac`, `int8/16/32/64.lilac`: replace `@conversion(from: @byte_buffer)`
  with `@conversion(from: @bitN)` (`@bit_native` for `int.lilac`) that wraps
  the value (no `@as_int`); the comparison/arith hook bodies are unchanged.
- `float32.lilac`, `float64.lilac`: replace the `@byte_buffer` conversions with
  `@conversion(from: @fbit32)` / `@conversion(from: @fbit64)` (identity/wrap);
  the `@bitN ↔ FloatN` pairs from sub-project 1 stay.
- No string→number conversion is added (non-goal).

## 11. Testing

- **`lilac-lex`:** exponent forms lex (`1e3`, `1.5e-4`, `2E10`); `1.` and `1e`
  are rejected.
- **`lilac-typeck`:** int smallest-fit (`0`→`@bit8`, `0xFF`→`@bit8`,
  `0x1FF`→`@bit16`, `0x1_0000_0000`→`@bit64`); grow-only (`x: @bit32 = 0x1`
  →`@bit32`); overflow (`E060`); negative fold (`-1`→`@bit8`, `-129`→`@bit16`);
  float default/expected/rounding/`E060`; literal folding (`2 + 3`→`5: @bit8`,
  `200 + 100`→`300: @bit16`, `x: @bit8 = 200 + 100`→`E060`, `2 + 3.0`→`E010`,
  divide-by-zero→`E060`).
- **`lilac-cli`:** `stdlib.rs` (check + emit-ir on `lib/Standard`) stays green;
  refresh affected `tests/ui/*.lir`/`*.ll` snapshots and delete the `as_int`/
  `as_float` fixtures.
- **`lilac-driver`:** the removed `lilac-decimal` fold leaves no dangling call;
  `cargo build --workspace` is clean.
- All gates stay green.

## 12. Gates

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`

## 13. Maintenance / source of truth

- `check_number_literal` is the single place an integer/float literal gets its
  type; the smallest-fit table and the signed table are defined once there.
- Literal folding lives beside literal typing in typeck; no pre-typeck numeric
  fold pass exists after this sub-project.

## 14. Follow-ups (separate specs)

- **Sub-project 3:** the `as` operator (force a `@conversion`; the
  `ref unknown` ↔ any `ref T` exception).
- Stdlib string→number parsing (`@byte_buffer` → `IntN`/`FloatN`), if wanted.
- `@fbit128` and `@bit128` literal/fold support.
