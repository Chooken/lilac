# Standard Math Plan

> Status: agreed, 2026-10-14; phases 0-3 done.
>
> Goal: a production-grade math library that feels like Lilac and serves
> two kinds of user. A beginner should be able to write `x.sqrt()`,
> `angle.sin()`, `Float64.PI` and `rng.between(1, 6)` and get the right
> answer, with errors that explain themselves. An expert should be able
> to reach every instruction the hardware has: fused multiply-add, the
> bit-counting instructions, overflow flags, wide multiplies, SIMD lanes
> and a chosen CPU, with nothing hidden in between.
>
> Decisions (all agreed):
> - **Methods on the numeric types are the main surface.** Lilac has no
>   function overloading and no type-argument inference (SPEC §7), so a
>   free `sqrt` could serve only one width, and a generic `sqrt[T]`
>   would have to be spelled `sqrt[Float64](x)`. `x.sqrt()`,
>   `n.count_ones()` and `Float32.PI` work for every width and show up in
>   the editor's `.` completion. `Standard.Math` holds what is not one
>   scalar's method: random numbers, linear algebra, SIMD types and
>   numeric algorithms.
> - **The compiler gets only the hardware's operations, as intrinsics.**
>   These are conversions, the exactly rounded float operations, bit
>   counting, overflow tests, wide multiplies and, later, vectors. Every
>   function libm would provide (`sin`, `exp`, `pow`, …) is Lilac code
>   in `Standard`. That keeps native mode free of libc, and `Standard`
>   is not special (`standard-not-blessed`).
> - **The same bits on every target, in every build mode.** Lilac
>   already makes `==` bit-equality and defines integer division on
>   every input, so math follows the same rule:
>   - no fast-math flags and no FMA contraction by the backend;
>   - no x87 (32-bit x86 builds use SSE2);
>   - no global rounding mode;
>   - every intrinsic defined on every input (a float→int conversion
>     saturates and maps NaN to 0, never LLVM poison).
>
>   Speed comes from explicit operations (`multiply_add`, vectors, a
>   chosen CPU), never from loosening an existing one.
> - **Elementary functions are correctly rounded.**
>   Every `Float32` and `Float64` function returns the exactly rounded
>   result. A correctly rounded answer is the same whatever the
>   implementation does, so a kernel may take a hardware-FMA path on one
>   CPU and a different path on another without breaking the rule above.
>   - `Float32`: cheap. Evaluate in `Float64` with a proven error bound,
>     then round once (CORE-MATH's binary32 approach).
>   - `Float64`: a fast path that is right almost always, plus a rare
>     accurate fallback. These are CORE-MATH's binary64 algorithms
>     (MIT-licensed), ported to Lilac.
>   - If a function's correctly rounded `Float64` version benchmarks
>     far behind libm, that function alone may drop to "≤ 1 ULP", with
>     its bound documented.
> - **Integer overflow panics in Debug and wraps in Release.** This applies to `Standard`'s `+ - *` on its integer types.
>   It is the Rust model, and it catches the commonest beginner bug.
>   - It is library policy, not language policy: the intrinsics still
>     wrap (SPEC §3), and the hooks branch on the constant
>     `@builtin.Build.mode`, so Release pays nothing.
>   - Wrapping is the opt-in: code that means to wrap calls
>     `wrapping_add`, `wrapping_sub`, `wrapping_multiply` or
>     `wrapping_negate`, which wrap in every mode.
> - **`as` converts only when nothing is lost.** `Int32 as Int64` and
>   `Int32 as Float64` are `@conversion` hooks. A conversion that can
>   lose information is a named method saying how: `to_int32_truncated`,
>   `to_float32_rounded`, or a failing `to_int32() value: Int32 else
>   nothing`. This extends `explicit-operator-hooks`: no silent rounding
>   anywhere.
> - **Failure outs are Lilac's checked arithmetic.** For example,
>   `checked_add: function (self, other: Int32) sum: Int32 else
>   nothing` is used as `if a.checked_add(b) -> sum: Int32 { … }`.
>   Multi-out functions do the rest: `sin_cos` returns both values,
>   `widening_multiply` returns `high, low`, and `overflowing_add`
>   returns `value, overflowed`.

## Today

What exists:
- `Int`, `Int8`–`Int64`, `Float32` and `Float64` in `lib/Standard`, with
  operator hooks and `MIN`/`MAX`. `Int` alone has the mixed literal
  hooks `(Int, @bit_native)`.
- Intrinsics (SPEC §3):
  - integer: `@i_add`/`sub`/`mul`/`div`/`mod`/`negate`/comparisons, signed
    and unsigned;
  - bitwise: `@bit_and`/`or`/`xor`/`not`/shifts;
  - float: `@f_add`/`sub`/`mul`/`div`/`mod`/`negate`/comparisons;
  - `@fbit_to_bits_W`/`@bits_to_fbit_W` reinterpret casts;
  - `@truncate`/`@extend_zero`/`@extend_sign`.
- `lilac-float` folds `+ - * / %` at all four float widths, exactly.
- `Standard.C`'s `double_math.lilac`/`float_math.lilac`: libm `extern`s,
  `--libc` only.

What is missing or broken:
- **No int↔float conversion at all, and no float width conversion.**
  Nothing can turn an `Int` into a `Float64`.
- **Float `%` does not link in native mode** (checked this session;
  fixed in phase 0):
  `@f_mod_64` lowers to LLVM `frem`, which becomes a call to `fmod`,
  and native mode has no libm:
  `lld: error: undefined symbol: fmod >>> referenced by … Standard.Float64.@mod`.
  Other LLVM operations may also become C-runtime calls the native
  sysroot does not have, depending on target and CPU:
  - `floor`/`ceil`/`trunc`/`round` on an SSE2-only x86;
  - `fma`/`fmaf` without hardware FMA;
  - `@fbit16` and `@fbit128` arithmetic (`__extendhfsf2`, `__addtf3`, …);
  - 64-bit division on 32-bit targets (`__divdi3`, `_alldiv`).
- **The backend always targets the `generic` CPU** (phase 0 fixed
  i686)
  (`create_target_machine(&triple, "generic", "")`). On x86-64 that is
  SSE2 only (no FMA, AVX or POPCNT). On i686 it means x87, which double
  rounds and breaks the rule above.
- **No unsigned types.** STDLIB.md lists `Uint8`–`Uint64` and `Uint`;
  none exist.
- **No mixed literal hooks on floats.** `c > 1.0` with `c: Float64` is
  `E043 no @greater_than for (Standard.Float64, @fbit64)` (checked this
  session).
- **No float `@format`** (`FAILS_standard_float_hole`), so a beginner
  cannot print a result.
- **No vectors, slices or `Ordering`.** `List`/slices are the
  Collections plan's work; this plan works over `@array(T, N)` and
  `(ref T, count)` until they land.

## Phase 0: every numeric operation links everywhere

Before adding operations, the existing ones must link and run on every
native target. **Done**, as follows.

1. **An audit test** (`crates/lilac-cli/tests/numeric_link.rs`). It
   generates a program with one `export` per numeric intrinsic and width
   (an export is kept and its arguments are unknown, so nothing folds
   away), and builds it for all six native targets (x86-64 and 64-bit
   Arm on Windows, Linux and macOS) at `debug` and `release`,
   cross-linked with the development sysroot's `lld`. A leaked libcall
   is an undefined symbol, which fails the link. Linking checks more
   than listing an object's undefined symbols would: it also checks the
   system stubs. Every later phase adds its intrinsics to `program`.
2. **The backend defines what its own lowering calls.** In native mode
   that is only two things today:
   - `f.mod` (`frem`), which LLVM turns into a call to `fmod`. The backend
     instead calls its own exact remainder (`.lilac.fmod.32`/`.64`,
     internal, `float.rs`, after musl), in every mode, since `--libc` on
     Linux does not link `-lm`. A 16-bit remainder goes through the 32-bit
     one, which is exact.
   - x86-64 converts half-precision floats through `__extendhfsf2`
     and `__truncsfhf2`, so native mode defines them (`float::HALF`, after
     compiler-rt). 64-bit Arm converts them in hardware.

   `fma`, `floor` and the rest are not reached by any of today's
   intrinsics. The phase that adds each one defines its routine, and
   the audit makes sure it does.
3. **`fp128` arithmetic** is `E078`, checked after DCE like `E035`
   (`lilac_ir::check_live_wide_floats`). It covers arithmetic, negation
   and ordered comparisons. Literals (folded exactly), storage, copies,
   `==` and `@fbit_to_bits` still work. Software `fp128` is a later
   phase if anyone needs it.
4. **i686 uses SSE2.** The target machine's CPU is
   `Arch::baseline_cpu` (`pentium4` on 32-bit x86), and its features are
   `Arch::baseline_features` (`+vfp2` on 32-bit Arm, keeping the
   integer-register C ABI). Phase 8 makes them a setting.

Tests:
- The audit test. Without the half conversions it fails with lld's
  `undefined symbol: __extendhfsf2` on each x86-64 target.
- `tests/run/float_remainder.lilac`: `%` at 16, 32 and 64 bits, checked
  bit for bit, including signs, NaNs, far exponents and subnormals.
- `tests/run/FAILS_float128_arithmetic.lilac`: `E078` with its trace. An
  unreachable use is not reported.
- `x86_floats_use_sse2` (`lilac-codegen-llvm`): an i686 `fadd double` is
  emitted as `addsd`, with no x87 stack.

Left for later:
- 32-bit targets have no native mode, so their 64-bit division routines
  (`__divdi3`, …) are needed only once one does.
- `__truncdfhf2`, which phase 1's `@float_convert` would reach on
  x86-64 (phase 1 converts halves through 32 bits instead).
- `--libc` builds on Windows (MSVC's C runtime has no half conversions).

## Phase 1: the hardware's numeric operations

Every new intrinsic follows the usual path:
- a variant in `lilac-hir/src/intrinsics.rs`, with `name`/`signature`
  rows and a `doc`;
- typeck's common path where its slots allow;
- a LIR instruction or a `leaf_builtin`, checked by the verifier;
- LLVM lowering;
- folding by the constant evaluator, for every exact operation;
- a row in SPEC §3.

Each one is defined on every input.

**Done**, as below. What the implementation adds to the plan:
- The LIR has `LirInst::Unary` (the one-operand operations),
  `LirInst::MulAdd` (`f.mul.add`), new `ArithOp`s (`f.min`, `rotate.l`,
  `i.add.sat`, `i.mul.high`, `i.add.overflows`, …) and new `ConvertOp`s
  (`i.to.f`, `u.to.f`, `f.to.i`, `f.to.u`, `f.convert`). `IR_SPEC.md`
  lists them.
- The backend (`lilac-codegen-llvm/src/lower/math.rs`) uses LLVM
  intrinsics for the integer operations, which every target expands
  inline. Float ones must not become C library calls (`floor`, `fma`,
  `fmin`): aarch64 has an instruction for each, and elsewhere the
  backend calls its own internal routines, IR text. A 16-bit operation
  runs at 32 bits between exact conversions, except the fused
  multiply-add, which rounds to odd at 53 and then 24 bits so only the
  last rounding counts.
- `lilac-float` (`ops.rs`) has the exact operations at every width:
  `sqrt`, `multiply_add`, `round_to_integral` (a `Rounding`),
  `minimum`/`maximum`, `copy_sign`, `compare`, `convert`, `from_int` and
  `to_int`. The evaluator (`lilac-lower/src/constant.rs`) folds floats
  with them, never a NaN result (targets make different NaNs) and
  arithmetic only on finite operands with a finite result, as
  `FloatBits::apply` does. A global may be a float constant.
- On `@fbit128` every new float operation and conversion is `E078`.
- Beyond the plan's tests, `crates/lilac-cli/tests/numeric_differential.rs`
  builds a program running every operation at every width on generated
  inputs (random bits, special values, values near integers), in debug
  and release, and compares each result bit for bit with Rust's (an
  exact model for `@fbit16`; a NaN only has to be a NaN).
  `tests/run/numeric_folding.lilac` checks that the folds give the
  run-time bits.

**Conversions.** Like `@truncate`, these take their result width from
the expected type, so there is one name per direction rather than per
width pair.

| Intrinsic | Meaning | LLVM |
|---|---|---|
| `@int_to_float(@bitW)` | signed integer to the expected `@fbitV`, rounded to nearest even | `sitofp` |
| `@uint_to_float(@bitW)` | unsigned integer to the expected `@fbitV`, rounded to nearest even | `uitofp` |
| `@float_to_int(@fbitW)` | to the expected `@bitV` as signed; truncates; saturates at `MIN`/`MAX`; NaN is 0 | `llvm.fptosi.sat` |
| `@float_to_uint(@fbitW)` | the unsigned form | `llvm.fptoui.sat` |
| `@float_convert(@fbitW)` | to the expected `@fbitV`, widened exactly or narrowed with round to nearest even | `fpext`/`fptrunc` |

**Float operations, exactly rounded** (`W` is 16/32/64, and 128 per
phase 0):

| Intrinsic | Meaning | LLVM |
|---|---|---|
| `@f_sqrt_W` | square root | `llvm.sqrt` |
| `@f_multiply_add_W(a, b, c)` | `a * b + c` rounded once | `llvm.fma` |
| `@f_abs_W` | clears the sign bit | `llvm.fabs` |
| `@f_copy_sign_W(magnitude, sign)` | the first operand's magnitude with the second's sign | `llvm.copysign` |
| `@f_floor_W`, `@f_ceil_W`, `@f_trunc_W` | round to an integral value, down, up or toward zero | `llvm.floor`/`ceil`/`trunc` |
| `@f_round_W` | to the nearest integral value, ties away from zero | `llvm.round` |
| `@f_round_even_W` | to the nearest integral value, ties to even | `llvm.roundeven` |
| `@f_min_W`, `@f_max_W` | IEEE 754-2019 `minimum`/`maximum`: NaN propagates, and `-0 < +0` | `llvm.minimum`/`maximum` |

IEEE equality needs no intrinsic: `a <= b and a >= b` is false for NaN
and true for `-0 == +0`, and LLVM folds it to `fcmp oeq`.
`minimumNumber` (NaN loses) is library code, since LLVM 18's `minnum`
has sNaN quirks.

**Integer operations** (`W` is 8/16/32/64/native):

| Intrinsic | Meaning | LLVM |
|---|---|---|
| `@bit_count_ones_W` | population count | `llvm.ctpop` |
| `@bit_leading_zeros_W`, `@bit_trailing_zeros_W` | the count; `W` for zero (never poison) | `llvm.ctlz`/`cttz` with `is_zero_poison = false` |
| `@bit_rotate_left_W`, `@bit_rotate_right_W` | rotate by the amount modulo `W` | `llvm.fshl`/`fshr` |
| `@bit_swap_bytes_W` (W ≥ 16), `@bit_reverse_W` | byte swap, bit reversal | `llvm.bswap`/`bitreverse` |
| `@i_add_overflows_W`, `_sub_`, `_mul_`, and `_u_` forms | `@bit8`: whether the wrapped result overflowed | `llvm.{s,u}{add,sub,mul}.with.overflow`, extracting the flag |
| `@i_add_saturating_W`, `_sub_`, and `_u_` forms | clamp to the range | `llvm.{s,u}{add,sub}.sat` |
| `@i_multiply_high_W`, `_u_` | the upper `W` bits of the double-width product | widen, `mul`, shift (LLVM selects `mulh`/`umulh`) |

An intrinsic has one result (`Signature::result`), so an overflow test
is a separate predicate. `a + b` next to `@i_add_overflows_32(a, b)`
lowers to one `with.overflow` call after LLVM's CSE.

**Constant folding.** `lilac-float` gains exact `sqrt` and `fma` at all
four widths (the big-integer arithmetic the 16/128 converters use), the
rounding operations, `min`/`max`, `copy_sign` and the conversions. The
integer operations fold in the evaluator. So
`Float64.SQRT_2: Float64 => (2.0 as Float64).sqrt()` is a constant, and
a `=>` method over these intrinsics is a constant expression on constant
arguments.

Tests:
- Unit tests for each fold against known bit patterns (IEEE edge cases:
  ±0, subnormals, NaN payloads, ties).
- The intrinsics unit test that `names()` and the table agree.
- A run test, `numeric_intrinsics.lilac`, that prints results at every
  width. It includes the defined-everywhere cases: `@float_to_int` of
  NaN and ±∞, and `@bit_leading_zeros_32(0x0)`.
- The phase 0 audit module, extended.

## Phase 2: the numeric types

All in `lib/Standard`, one file per type as today.

- **Unsigned types.** `Uint8`, `Uint16`, `Uint32`, `Uint64` and `Uint`
  (pointer width), with the unsigned intrinsics (`div_u`,
  `less_than_u`, …), `MIN`/`MAX`, `@format`, `hex`/`binary`. `Byte` is
  an alias of `Uint8`.
- **`Float16`** as a storage type: conversions, classification and
  `@format` through `Float32`. Its arithmetic is phase 0's question.
  (Landed: its arithmetic and conversions. Classification is phase 4's
  and `@format` phase 6's, as for every float, since no float has
  either yet.)
- **Mixed literal hooks.** Every type gets `(T, @bitW)`/`(@bitW, T)`
  (integers) or `(T, @fbitW)`/`(@fbitW, T)` (floats) for each operator,
  as `Int` has, so `x > 1.0` and `n * 2` work. First check that a float
  literal opposite a `Float32` pins to `@fbit32` through the
  `(Float32, @fbit32)` hook. If operator lookup cannot pin it today, fix
  that in typeck, not with more hooks.
- **Conversions.**
  - `as` (an `@conversion` hook) exists only where it is exact:
    - integer widening of the same signedness;
    - unsigned to a wider signed type;
    - integers into a float whose mantissa holds them (`Int8`/`Int16`/
      `Int32` and the unsigned forms into `Float64`; 8- and 16-bit into
      `Float32`);
    - `Float32 as Float64`.
  - Everything else is a named method:
    - `to_int32_truncated()` (saturating, NaN → 0, as the intrinsic);
    - `to_int32() value: Int32 else nothing` (fails on NaN, on out of
      range and on a fraction: the exact conversion);
    - `to_float32_rounded()` (round to nearest even), and the same for
      the other widths.
  - The names say what happens; there are no silent rounding casts.
- **Integer overflow policy** (decision above). Each `@add`/`@sub`/`@mul`
  hook stays `=>`:

      @add: function (lhs: Int32, rhs: Int32) Int32 =>
          Int32 { data = if @builtin.Build.mode == .Debug and @i_add_overflows_32(lhs.data, rhs.data) then @panic("attempt to add with overflow") else @i_add_32(lhs.data, rhs.data) }

  (Landed inline rather than through a private `add_checked_32`, so a
  panic's trace ends at the operator, as division's does.) It branches on
  `@builtin.Build.mode == .Debug` (a constant condition, so Release
  lowers only the wrapping arm) and panics with
  `attempt to add with overflow`. `@negate` of `MIN` and `abs` of `MIN`
  follow the same rule. The constant evaluator folds a literal overflow
  as `E060`, as it already does for division.
- **The wrapping opt-in lands with the policy.** `wrapping_add`,
  `wrapping_subtract`, `wrapping_multiply` and `wrapping_negate` are `=>`
  methods over the raw intrinsics on every integer type, and they wrap in
  every mode. In the same commit, any code in `Standard` that relies on
  wrapping switches to them. Find it by running the run tests in Debug,
  where a wrap now panics, and by reading the hashing and address
  arithmetic (`Heap`'s `align_up`, `Os`'s pointer math). Phase 3 adds
  the other overflow families.

Tests:
- A run test per family: unsigned arithmetic and comparisons, mixed
  literals and the conversion matrix.
- `integer_overflow_debug.lilac` (exit 101, the message; a run-time
  panic, so no `FAILS_` prefix), and `FAILS_integer_overflow_constant.lilac`
  (`E060`).
- `integer_overflow_release.lilac` (`// build-arg: --release`, wraps).
- `integer_wrapping.lilac`: the `wrapping_*` methods wrap in Debug.
- `FAILS_lossy_as.lilac` (`E043` for `Int64 as Float64` that names the
  method to use, through a fix-it if the conversion search can suggest
  one).
- Landed with it: `@panic` and `@trap` are values of any expected type
  (`panic_value.lilac`); a `=>` call whose constant arguments reach a
  `@panic` is `E060` with that message; a negative literal into a type
  with no `@negate` is `E043` (`FAILS_negative_unsigned.lilac`), and a
  negative float literal folds like an integer one; any missing
  conversion's `E043` names the source type's `to_…` methods that give
  the target, with a fix-it on an `as`.

## Phase 3: integer methods

**Done.** On every integer type (`self` by value, all `=>` where one
intrinsic does it):

- Basics:
  - `abs`, `unsigned_abs` (to the unsigned type, defined for `MIN`),
    `sign`;
  - `min`, `max`, `clamp(low, high)`;
  - `is_power_of_two`, `next_power_of_two` (failing on overflow).
- Division:
  - `div_floor`, `div_ceil`;
  - `div_euclid`, `rem_euclid`;
  - `div_rem(other) quotient, remainder`;
  - `gcd`, `lcm` (binary GCD over `trailing_zeros`).
- Powers and roots: `pow(exponent: Uint32)` by squaring, overflow per
  the policy, then `isqrt` and `ilog2`/`ilog10` (failing on ≤ 0).
- Bits:
  - `count_ones`, `count_zeros`;
  - `leading_zeros`, `trailing_zeros`, `leading_ones`, `trailing_ones`;
  - `rotate_left`, `rotate_right`;
  - `swap_bytes`, `reverse_bits`;
  - `to_big_endian`, `to_little_endian`, `from_big_endian`,
    `from_little_endian` (constant on `@builtin.Target.arch`'s byte
    order).
- Overflow, in four families for `add`/`subtract`/`multiply`/`negate`/`pow`:
  - `checked_*` fail;
  - `wrapping_*` wrap (phase 2 has `add`/`subtract`/`multiply`/`negate`;
    this adds `wrapping_pow`);
  - `saturating_*` clamp;
  - `overflowing_*` return `value, overflowed: Bool`.
- Wide arithmetic:
  - `widening_multiply(other) high, low` (through `@i_multiply_high`);
  - `carrying_add(other, carry: Bool) sum, carry_out`;
  - `borrowing_subtract(other, borrow: Bool) difference, borrow_out`.

`Ordering` (STDLIB §3.1, `lib/Standard/ordering.lilac`) landed here:
`enum { Less; Equal; Greater }` with `is_less`/`is_equal`/`is_greater`/
`is_less_or_equal`/`is_greater_or_equal`, `reverse`, `and_then` (`then`
is a keyword) and `@format`, plus `compare` on every numeric type
(on floats it fails when either operand is a NaN).

Tests:
- `integer_basics.lilac`, `integer_division.lilac`,
  `integer_powers_roots.lilac`, `integer_bits.lilac` and
  `integer_overflow_methods.lilac`, printing boundary values (`MIN`,
  `MAX`, 0, ±1, powers of two).
- `checked_*` exercised through `if … -> sum` and guards.

## Phase 4: float core

On `Float32` and `Float64` (and on `Float16` as far as phase 2 goes):

- **Constants** (member constants, folded through phase 1):
  - `PI`, `TAU`, `E`, `SQRT_2`, `FRAC_1_SQRT_2`, `LN_2`, `LN_10`,
    `LOG2_E`, `LOG10_E`;
  - `INFINITY`, `NEG_INFINITY`, `NAN`;
  - `EPSILON`, `MIN_POSITIVE` (smallest normal), `MIN_SUBNORMAL`, `MAX`,
    `MIN`;
  - `MANTISSA_BITS`, `EXPONENT_BITS`.

  Each constant is the correctly rounded value, written as a literal and
  checked by a test against its bits.
- **Classification:**
  - `is_nan`, `is_infinite`, `is_finite`, `is_normal`, `is_subnormal`,
    `is_zero`;
  - `sign_bit`, `is_sign_negative`;
  - `classify` (an enum: `Nan`, `Infinite`, `Zero`, `Subnormal`,
    `Normal`).

  All are bit tests through `@fbit_to_bits`.
- **Exact operations**, each one intrinsic and `=>`:
  - `abs`, `copy_sign`;
  - `floor`, `ceil`, `trunc`, `round` (ties away), `round_even`,
    `fract`;
  - `sqrt`, `multiply_add(b, c)`;
  - `min`/`max` (NaN-propagating);
  - `min_number`/`max_number` (NaN loses);
  - `clamp(low, high)`;
  - `recip`;
  - `to_radians`, `to_degrees`.
- **Bits and ULPs:**
  - `to_bits`/`from_bits` (to and from the same-width `Uint`);
  - `next_up`, `next_down`, `ulp`;
  - `exponent`, `mantissa`;
  - `scale_by_power_of_two(n)` (exact `ldexp`);
  - `split_exponent() fraction, exponent` (`frexp`).
- **Comparison and equality.** `==` on floats is bit-equality (SPEC §8),
  which surprises anyone who expects IEEE behavior: `0.0 == -0.0` is
  false, and `NAN == NAN` is true for the same NaN.
  - `equals(other) Bool` is IEEE equality.
  - `approximately_equals(other, relative, absolute) Bool` is the
    standard combined tolerance.
  - `total_compare(other) Ordering` is IEEE `totalOrder`, for sorting.
  - The float docs and SPEC's equality paragraph each get a short
    "comparing floats" note. Optionally, the editor could show an info
    hint on `==` between floats. It would be editor-only and not faded
    (`no-noisy-warnings`); decide after using the library.
- **Interpolation:**
  - `lerp(to, t)`: exact at `t = 0` and `t = 1`, monotonic, as in
    C++20's `std::lerp`;
  - `inverse_lerp`, `remap`, `smooth_step`.

Tests:
- A run test per family, printing bits through phase 6's `@format` (or
  `hex()` until it lands).
- The NaN, ±0, ±∞ and subnormal rows, on every function.

## Phase 5: elementary functions

The libm surface, in pure Lilac, correctly rounded (decision above).

- **Exponential and logarithm:**
  - `exp`, `exp2`, `exp10`, `exp_m1`;
  - `ln`, `log2`, `log10`, `ln_1p`, `log(base)`.
- **Power and root:**
  - `pow`, `pow_int(n: Int32)`;
  - `cbrt`, `hypot`, `root(n)`.
- **Trigonometry:**
  - `sin`, `cos`, `tan`, `sin_cos() sine, cosine`;
  - `sin_pi`, `cos_pi`, `tan_pi` (exact for multiples of π, which
    games and DSP want);
  - `asin`, `acos`, `atan`, `atan2(x)`.
- **Hyperbolic:** `sinh`, `cosh`, `tanh`, `asinh`, `acosh`, `atanh`.
- **Special:** `erf`, `erfc`, `gamma`, `ln_gamma`. These come last, and
  are ≤ 1 ULP if correct rounding proves too costly for them.

Structure:
- **Where the code lives.** The methods are declared in
  `float32.lilac`/`float64.lilac`, and their bodies call kernels.
  - The kernels are `private` items of `Standard`, a file per family:
    `float_exponential.lilac`, `float_trigonometry.lilac`, … in
    `lib/Standard`.
  - `private` keeps them out of users' names, and descendant reach
    still lets `Standard.Math` (the SIMD forms, phase 10) call them.
  - A child folder would turn the dependency upside down, since root
    `Standard` would need its child.
- **Tables** (exp/log tables, the bits of 2/π for Payne–Hanek
  reduction) are globals with `@array` initializers. They are
  constant-encoded and pruned by DCE when nothing uses them.
- **The `Float64` kernels** are written so their fast paths use
  `multiply_add` where the algorithm wants it. With a CPU that has FMA
  (phase 8) the instruction is used. Without one, LLVM calls phase 0's
  software `fma`, which is slower but gives the same bits. Where that
  matters for speed, a kernel picks an FMA-free path on the constant
  `@builtin.Target.Features.fma`. Correct rounding makes both paths
  return the same bits.
- **Argument reduction:** Cody–Waite for normal ranges, and Payne–Hanek
  for huge trig arguments, so `(1e300).sin()` is correct, not garbage.
- **Errors:** none signalled and no errno. Domain errors return NaN
  and overflow returns ±∞, as IEEE 754 says. Each method's `///` doc
  states its domain, its special values (C Annex F's table) and
  "correctly rounded".

Verification (the production-grade gate):
- **Reference data.** `tools/math-reference/` holds a generator script
  (mpmath or MPFR; run offline, since this machine has no Python) and
  the checked-in tables of hex bit patterns it wrote. The tables cover:
  - random inputs over each function's domain;
  - the special values;
  - the CORE-MATH worst cases for each function (inputs whose exact
    result lies nearest a rounding boundary).
- **The harness** is `crates/lilac-cli/tests/math.rs`. It builds one
  Lilac program per function family that reads a table embedded as a
  global array, checks every row, and exits with the first failing row.
  It runs under `cargo test --workspace` in Debug and Release.
- **Exhaustive `Float32`.** An ignored-by-default test (`--ignored`)
  checks all 2^32 inputs of each unary `Float32` function against the
  `Float64` path's result.
- **Identities:** properties checked on random inputs, such as `sin` odd
  and `exp(ln x) ≈ x` with no ULP claim, and monotonicity at table
  boundaries.
- **Benchmarks.** `bench/math/` holds throughput and latency per
  function against the C library's (`--libc` and a C `extern`). They
  are run by hand and are not a gate. Results go into the plan's status
  when each family lands.

## Phase 6: printing and parsing floats

A beginner's first math program prints a float.

- **`@format`.** `Float32`/`Float64` print the shortest decimal string
  that reads back to the same bits, using the Ryū or Schubfach
  algorithm (no big integers, no allocation). Output looks like `0.1`,
  `1e300`, `-0`, `inf`, `nan`.
- **Fixed and scientific.** Two wrappers, like `hex()`: `{x.fixed(3)}`
  and `{x.scientific(4)}`. They are correctly rounded and need exact
  decimal expansion: Ryū-printf or a fixed-size big integer on the
  stack.
- **Parsing.** `Float64.parse(text: @byte_buffer) value: Float64 else
  error: ParseError` and `Int*.parse`. Floats use Eisel–Lemire with a
  big-decimal fallback, so every input is correctly rounded. The
  formats are the language's literal formats plus `inf`/`nan`. This sits
  in `Standard` until `Text` exists (STDLIB §3.3 names `parse_float`).
- Remove `FAILS_standard_float_hole` and update STDLIB §3.1a's note.

Tests:
- Round-trip on random bits and on the hard cases (from Ryū's test
  suite).
- The fixed-precision rounding cases (`0.125` to 2 digits, `1e23`).
- Parse cases at the halfway points.

## Phase 7: `Standard.Math.Random`

- **`Random`.** A fast, small generator with no global state:
  `xoshiro256**`, or `PCG64 DXSM` (NumPy's default).
  - `Random.from_seed(seed: Uint64)` is deterministic, with SplitMix64
    expanding the seed. `Random.from_entropy()` seeds from
    `Os.fill_random`.
  - Methods: `next_uint32`, `next_uint64`, `next_float64` (53 random
    bits in `[0, 1)`), `next_float32`, `next_bool`.
  - `below(n)` and `between(low, high)`, unbiased by Lemire's
    nearly-divisionless method (`widening_multiply`).
  - `normal(mean, deviation)` by the ziggurat method,
    `exponential(rate)`.
  - `shuffle[T](items: ref T, count: Int)` (Fisher–Yates; it takes a
    slice once Collections has slices), `choose`.
  - `jump()` (xoshiro) for independent streams per thread.
- **`SecureRandom`** is `Os.fill_random` behind the same methods, for
  keys and tokens. Its docs say plainly which one to use when.
- No global generator: threads would share it, and the
  `default_allocator` pattern (a global `Optional` filled at entry) is
  there if a program wants one.

Tests:
- Known-answer sequences from the reference implementations.
- `below` checked for bias over small `n` (counts, deterministic seed).
- `next_float64` never 1.0.

## Phase 8: choosing the CPU

The default stays portable: x86-64-v1 (SSE2), `pentium4` on i686 and
`armv8-a`. An expert opts in.

- **The setting.** `--cpu=<name>` and `lilac.toml`'s `build.cpu`.
  - The values are `baseline` (the default), `native` (the host:
    `TargetMachine::get_host_cpu_name`/`features`; refused for a cross
    target) and LLVM's names (`x86-64-v3`, `znver4`, `apple-m1`,
    `neoverse-v2`, …).
  - `--cpu-features=+avx2,-fma` is for overrides.
  - A row in `SETTINGS`, a line in `templates/project/lilac.toml`, and
    the build folder gains the CPU (for anything but `baseline`) beside
    `<triple>/`.
  - An unknown name is `E076` at the key, with did-you-mean from LLVM's
    CPU list.
- **The backend** passes them to `create_target_machine`, and the audit
  test from phase 0 runs at `baseline` and at the highest level per
  architecture.
- **`@builtin.Target.Features`.** Inline `@bit8` constants for the
  features math cares about: `fma`, `popcount`, `avx`, `avx2`,
  `avx512f`, `neon`, `sve`, `f16c`, … (one fixed list in `lilac-target`;
  absent features are `0x0`). So
  `if @builtin.Target.Features.avx2 { … }` lowers one arm, as `os` does
  today. The generated `@builtin` changes only for programs that name
  it.
- **Runtime dispatch is deferred.** That is one binary, several kernels
  picked at entry. It needs a CPU query intrinsic (`@x86_cpuid`,
  `E035` off x86 like `@linux_syscall`) and then follows the
  `default_allocator` pattern: a global function value filled at entry.

Tests:
- Manifest and flag tests for the setting.
- A `check` of a program that matches on `Features` under two `--cpu`
  values: each compiles only its arm.
- `native` refused for a cross target.

## Phase 9: linear algebra (`Standard.Math`)

For games, graphics and simulations; generic over the scalar.

- `Vector2[T]`, `Vector3[T]`, `Vector4[T]` (fields `x y z w`):
  - operators with themselves and with `T`;
  - `dot`, `cross` (3), `length`, `length_squared`, `normalized`,
    `distance`, `lerp`, `min`/`max`/`clamp` per component, `abs`;
  - constants `ZERO`, `ONE`, `X`, `Y`, `Z` (on the generic type, so
    they need `T`'s constants: a member constant of a generic type has
    no `T` today, `E021`; see "Language needs").
- `Matrix3[T]`, `Matrix4[T]`:
  - column-major (the GPU convention), C layout `@array(T, 16)`;
  - `IDENTITY`, `multiply`, `transpose`, `determinant`, `inverse`
    (failing when singular), `transform_point`, `transform_vector`;
  - constructors `translation`, `scale`, `rotation_x/y/z`,
    `look_at_right_handed`, `perspective_right_handed` (and
    left-handed forms; the handedness is in the name, never implied).
- `Quaternion[T]`: `from_axis_angle`, `multiply`, `rotate`, `slerp`,
  `normalized`, `to_matrix`.
- Naming: `Standard` ships no short aliases (agreed) until type
  inference exists. Users write `Vector3[Float32]`, or their own
  `Vec3: alias Vector3[Float32]`.
- Layout matches C (`struct { float x, y, z; }`), so a `ref
  Vector3[Float32]` array goes straight to a graphics API through an
  `extern`.
- Scalar fields, not SIMD: LLVM's SLP vectorizer handles the small
  fixed operations, and the explicit SIMD types (phase 10) are for hot
  loops.

Tests:
- A run test per type: identities (`m * inverse(m) ≈ I`), known
  rotations, and the C layout (`@size_of`, field offsets) checked
  against a C struct through `enum_c_layout`'s pattern.

## Phase 10: SIMD

A language addition, then a library.

- **The language.** `@vector(T, N)` is a builtin container, like
  `@array`:
  - `T` is a `@bitW` or `@fbitW` (not `128`), and `N` is a power of two
    from 2 to 64.
  - The size is `N × size(T)`; the alignment is the size up to the
    target's largest vector alignment (as GCC's `vector_size`, so the C
    ABI matches).
  - `@vector(T, N) {}` is zero; there are no fields.
  - An invalid `T` or `N` gets a new code (with its `docs/errors` page).
- **Lane-wise intrinsics** reuse the scalar names with a `WxN` suffix:
  `@f_add_32x4`, `@i_add_saturating_u_8x16`, `@f_sqrt_64x2`,
  `@f_multiply_add_32x8`, `@bit_count_ones_32x4`.
  - `IntWidth::parse`/`FloatWidth::parse` take the `xN` part, so the
    table stays one row per operation.
  - Comparisons give a mask: `@vector(@bitW, N)` with all-ones lanes.
- **Vector-only intrinsics:**
  - `@vector_splat`, `@vector_extract`/`@vector_insert`;
  - `@vector_shuffle(a, b, mask)` (the mask a constant `@array`);
  - `@vector_select(mask, a, b)`;
  - `@vector_reduce_add`/`_min`/`_max`/`_and`/`_or`;
  - `@vector_load(ref unknown)`/`@vector_store` (unaligned) and the
    `_aligned` forms;
  - `@vector_convert` (lane-wise conversions, as phase 1).
- **The rest of the pipeline:**
  - LIR: `LirTy::Vector` is a scalar for registers (`is_scalar`), since
    LLVM keeps vectors in registers.
  - The verifier checks lane types; layouts come from `lilac-layout`
    (`vector_layout`, with C conformance cases).
  - LLVM lowers to native vector types and ops. A width the CPU lacks
    is legalized by LLVM (split into narrower ops), so every program
    runs on `baseline`, only slower.
  - The phase 0 audit covers every vector intrinsic.
- **The library: `Standard.Math.Simd`.**
  - `Float32x4`, `Float32x8`, `Float32x16`, `Float64x2`, `Float64x4`,
    `Float64x8`, `Int32x4`, `Int32x8`, `Uint8x16`, `Uint8x32`, …
    objects over `@vector`, with operator hooks.
  - Methods: `splat`, `load`/`store` from `ref T` (and slices later),
    `sum`, `min`/`max`, `select`, `shuffle` with constant masks, and the
    phase 4/5 math methods lane-wise (`sqrt`, `floor`, `sin`, `exp`, …,
    as vector kernels with the same correctly rounded results).
  - A `LANES` constant, and the preferred width as a constant on
    `Features` (`Float32xPreferred: alias …` via a top-level `if` on
    `@builtin.Target.Features.avx512f`).
  - Masks are `Mask32x4`, … with `any`, `all`, `count`.
  - Concrete types because there are no const generics. If const
    generics arrive, `Vector[T, N]` can replace them behind aliases.

Tests:
- A run test per type family (arithmetic, masks, shuffles,
  reductions, loads at unaligned addresses).
- The same program under `--cpu=x86-64-v3`, giving identical output.
- Layout conformance against GCC vector types with Clang.

## Later

Recorded, not planned in detail:
- `Complex[T]`.
- `BigInt`/`BigUint` over `Heap.Allocator` (Karatsuba past a
  threshold), and `Decimal`.
- Numeric algorithms over slices once Collections has them:
  - `sum_accurate` (Neumaier), `dot`;
  - mean/variance (Welford), `median`;
  - `polynomial(x, coefficients)` (Horner, with `multiply_add`).
- Software `fp128` arithmetic, and `Float128` methods.
- Runtime CPU dispatch (phase 8's note).
- `Float16` arithmetic where hardware has it (`f16c`, Arm `fp16`).
- Fixed-point `Q` types for embedded targets.

## Language needs found on the way

The plan does not add these, but each phase states its workaround:
- **Type-argument inference** (STDLIB §5.4) is the biggest ergonomic
  tax: `Vector3[Float32] { … }` everywhere.
- **Member constants of a generic type have no `T`** (AGENTS.md, own
  names). `Vector3[T].ZERO` needs `T.ZERO`; until then it is a static
  function, `Vector3[Float32].zero()`.
- **Const generics** (STDLIB §5.3) would replace the concrete SIMD
  types and give `Matrix[T, R, C]`.
- **Slices** for anything over a run of numbers.
- **Multi-result intrinsics** would make the overflow intrinsics one
  call each. LLVM's CSE makes the two-call form free, so this is
  cosmetic.

## Docs

Each phase updates:
- SPEC §3 (intrinsics; `@vector` in §1 with `@array`; conversions in
  §8);
- STDLIB §3.1 and §3.8 (rewritten from this plan; §5's libm item goes);
- AGENTS.md (the numeric-intrinsic and native-libcall bullets, the CPU
  setting, `@vector` in the layouts section);
- `docs/errors` pages for each new code;
- this plan's status line.

Every `Standard` math item gets a `///` doc with its domain, its special
values and its accuracy, and `standard_docs` checks they attach.

## Order and size

| Phase | Depends on | Size |
|---|---|---|
| 0 Linking everywhere | — | M (IR leaves, audit test) |
| 1 Hardware intrinsics | 0 | L (≈40 intrinsics, folds) |
| 2 Numeric types | 1 | M |
| 3 Integer methods | 2 | M |
| 4 Float core | 2 | M |
| 5 Elementary functions | 4, reference tables | XL (the bulk; one family per commit) |
| 6 Float printing and parsing | 4 | L |
| 7 Random | 3 | S |
| 8 CPU choice | 0 | M |
| 9 Linear algebra | 4 (5 for rotations) | M |
| 10 SIMD | 1, 8 | XL |

Phases 3, 4, 7 and 8 are independent once 2 lands. Phase 6 is worth
pulling forward right after 4, since everything after it is easier to
test with printed floats.
