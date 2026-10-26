# Math speed plan

Make `Standard`'s elementary functions about as fast as the C library's
while keeping them correctly rounded (`math-plan.md`, phase 5).

> Target, measured by `bench/math` against `bench/math/c` (the Windows C
> runtime, which is not correctly rounded):
> - `Float64`: within 1.5 times the C library on the usual ranges, and
>   within 2 times for `pow`, `erf`, `gamma` and huge trigonometric
>   arguments. CORE-MATH, which is correctly rounded, is in that range
>   of glibc, so the target is reachable.
> - `Float32`: as fast as the C library's `sinf`, `expf`, ... or faster.
>   Correct rounding is cheap at 32 bits because plain doubles carry
>   enough extra precision.
> - Correctness is unchanged. Every new fast path keeps the current
>   kernel and the 384-bit fallback behind it, and every existing test
>   still passes.

## Where the time goes today

These measurements come from the development machine in Release, in
nanoseconds per call:

| function | Lilac | C |
|---|---:|---:|
| `exp` | 12.8 | 2.4 |
| `ln` | 15.3 | 3.0 |
| `sin` | 298 | 3.5 |
| `atan` | 220 | 3.2 |
| `cbrt` | 130 | 14.7 |
| `erf` | 3716 | 4.7 |

1. **Every other function runs the double-double kernel on every call.**
   Only `exp` and `ln` have a fast tier. The kernels aim at 2^-90 and
   evaluate long Horner polynomials in double-double. Without a fused
   multiply-add, each step is a Dekker product: about 17 operations in
   one long dependent chain. That costs about 1,000 cycles. This is the
   main cost, and it is in the library, not the compiler.
2. **`Float32` and `Float16` run the `Float64` double-double kernel.**
   For example, `Float32.sin` calls `sin_kernel(self as Float64)`, so it
   is about 100 times slower than `sinf`.
3. **The compiler's call overhead dominates the fast tiers.** Each Lilac
   function is emitted with external linkage and the all-pointer place
   ABI. So LLVM keeps every call, passes every argument and result
   through memory, and cannot promote arguments.
   - `Float64.exp`'s prologue saves 5 general registers and 3 vector
     registers.
   - It also zeroes the slow path's locals on every call.
   - A trial that made every non-entry function `internal` before the
     O2 pipeline brought `exp` from 12.8 to 7.7 ns and `ln` from 15.3 to
     11.1 ns, with the checksum unchanged.
   - The kernels did not get faster in that trial (they were within
     noise to 15% slower).
4. **The baseline CPU has no FMA.** The Windows C runtime picks FMA code
   at run time. Lilac's default target, x86-64-v1, does not have FMA,
   and there is no way to opt in yet (phase 8).
5. **`erf` uses a continued fraction** where piecewise polynomials would
   do. That is why it is about 800 times slower than C.

## Step 0: the surface, `Float64.sin(x)` and `Float`

The elementary functions become static functions of their type, and
`Float` names `Float64` for code that does not care about widths. This
replaces `math-plan.md`'s decision that methods are the main surface for
these functions: `angle.sin()` reads as a property of the value, where
math writes `sin(angle)`.

- **Static functions, not a `Math` namespace.** `Math.sin(x)` would need
  one function per width under one name. Lilac has no overloading and
  infers no type arguments, so it would serve one width only, or be
  spelled `Math.sin[Float32](x)`. `Float32.sin(x)` works for every width
  today, with no language change:
  - a static receiver sees through aliases, so `Float.sin(x)` works;
  - inside a numeric type's body a static is an own name, so `Float64`'s
    own code writes `sin(x)` bare (`own.rs`);
  - the editor lists them after `Float64.` (the static receiver's
    members).
  - `Standard.Math` keeps what is not one scalar's function (random
    numbers, linear algebra, SIMD).
- **Which functions move.** The rule: what math writes as `f(x)` is a
  static function; a question about the value, or a reshaping of it,
  stays a method.
  - Static: the `@math_functions` list (`exp` … `tan_pi`), `log`,
    `sin_cos`, `atan2`, `pow`, `pow_int`, `cbrt`, `hypot`, `root`, and
    `sqrt`, so the roots sit together. Two-argument ones take the
    arguments in C's order: `Float64.atan2(y, x)`, `Float64.pow(x, y)`,
    `Float64.log(x, base)`, `Float64.root(x, n)`.
  - Methods, unchanged: classification (`is_nan`, `classify`, …),
    rounding (`floor`, `round`, …), `abs`, `copy_sign`, `min`/`max`/
    `clamp`, `lerp` and the other interpolations, bits and ulps
    (`to_bits`, `next_up`, `ulp`, …), conversions, `compare` and
    formatting.
  - The method forms go away; no shim keeps `x.sin()`.
- **`Float: alias Float64`**, beside `Byte: alias Uint8` (the script
  writes it into `float64.lilac`).
  - An alias, not a structure of its own: `Float` and `Float64` are one
    type, so a beginner's `Float` passes to any `Float64` function with
    no conversion, and the literal hooks, constants (`Float.PI`) and
    functions all come along.
  - Not native-sized. Every supported target has 64-bit floats in
    hardware (SSE2 on i686, `+vfp2` on 32-bit Arm, wasm's `f64`), so
    nothing gets faster at another width, and a width that varied by
    target would break "the same bits on every target". `Int` is
    pointer-sized because it indexes memory; a float never does.
  - Its `///` doc says it is `Float64`, and to name a width when
    storage or SIMD lanes matter.
- **Work:**
  - `numeric_types.pl`: `math_methods` writes `name: function (x: T) r:
    T` (and `(x: T, y: T)` …); `%quick` and `correct_method` take the
    parameter names. Rerun it.
  - Callers: `Standard`'s own uses (`float_*.lilac`, `Float32`/`Float16`
    bodies calling `Float64` functions), the programs `math.rs`,
    `numeric_differential.rs` and `numeric_link.rs` generate, the run
    tests, `bench/math`, `examples/`, and every `///` doc that writes
    `(1e300).sin()`. Find them by the compile errors: a method call of a
    static is `E022`.
  - Docs: `math-plan.md`'s decision and phase 5's list, and the
    `AGENTS.md` paragraph on elementary functions.
- **Tests:** the existing suites, rewritten; a run test that
  `Float.sin(x)`, `Float.PI` and a `Float` passed to a `Float64`
  parameter work; a typeck test that `x.sin()` is `E022` and names
  `Float64.sin(x)`, if the message can say so (a fix that rewrites the
  call is better still).

Land this before step 3: step 3 edits the same generated methods (`%quick32`),
and every later benchmark and test is written once, in the new form.

## Step 1: compiler, internal linkage

- The backend gives every defined function `internal` linkage, except
  these symbols:
  - the entry symbol (`LinkInfo::entry_symbol`), `.lilac.native_main`,
    and C `main` under `Entry::CMain`;
  - each export's C entry;
  - the weak C-runtime replacements (`memcpy`, `memmove`, `memset`,
    `memcmp`, `_fltused`, `__chkstk`);
  - `lilac_thread_start`, and any function that appended IR text or
    module assembly names.
- Under internal linkage LLVM may inline, promote arguments and remove
  dead arguments across the whole program, so the place ABI costs
  nothing inside the module.
- **Watch for:**
  - IR text linked by `append_ir` that declares a module function: the
    linker must still resolve it, or that function stays external.
  - The Linux entry assembly calls `.lilac.native_main`.
  - Stack traces and panic sites must still name functions (the names
    stay; only linkage changes).
- **Tests:**
  - an `m5b.rs`-style test that a plain function is `internal` and the
    entry and exports are not;
  - every run test on all hosts;
  - `numeric_link.rs` and `numeric_differential.rs`.
- **Then measure the prologue.** If saving registers and zeroing the slow
  path's locals still costs more than the fast path in `exp`, add a
  branch-weight intrinsic (`@expect(cond, 0x1)`), so LLVM lays the slow
  path out of line and shrink-wraps the prologue.
  - The kernels use it on the "not sure" branch.
  - This is a standalone language addition with its own tests. It is not
    needed if internal linkage is enough.

This step is small: one backend function plus its tests. It speeds up
every Lilac program, not just math.

## Step 2: generator, minimax polynomials

`gen_kernels.py` fits Taylor series. Fast tiers need the shortest
polynomial that meets their error bound, so they need minimax fits.

- Add a Remez exchange in standard-library Python (`fractions`/`decimal`,
  as `reference.py` already does). It fits relative or absolute error
  over an interval, with chosen leading coefficients fixed (for example
  `1 + r + r^2 P(r)`).
- After fitting, round the coefficients to doubles, then certify the
  error of the rounded polynomial by dense sampling, plus a bound
  between samples from the derivative. The generator writes the
  certified bound into the evaluator's `///` comment.
- Evaluators come out as unrolled Horner, or Estrin when a polynomial is
  long enough for the shorter dependency chain to pay off.

## Step 3: `Float32` fast tiers (every function)

One pattern for all of them: evaluate in plain `Float64` with a
relative error below about 2^-45, then round to `Float32`.
- If the error interval does not contain a `Float32` rounding boundary,
  that result is final. This is a check on the bits of the result plus
  and minus the error, like `quick64`.
- Otherwise, use today's kernel.
- The fallback runs for about one argument in 2^20.

- **Small arguments:** a short polynomial.
- **Argument reduction:**
  - `exp`: a 32- or 64-entry table of `2^(j/N)`.
  - `ln`: a reciprocal table.
  - `sin`/`cos`: Cody–Waite on the double value, then Payne–Hanek for
    huge arguments with the existing reduction.
- **Derived functions:** `log2`/`log10` from `ln`; the hyperbolics from
  `exp`/`ln`; `tan` from the sine and cosine pair; inverse trigonometry
  from a table and a short polynomial.
- **`cbrt`/`hypot`/`root`/`pow`:** these are nearly free in doubles.
  - `hypot` is one double square root.
  - `pow` is `exp(y ln x)` with `ln x` from a slightly longer polynomial.
- **`erf`/`erfc`/`gamma`/`ln_gamma`:** these only promise 1 ulp. Use
  plain double approximations (piecewise polynomials, and Stirling for
  large arguments) with the same rounding test against the current
  kernel.
- **`Float16`:** calls `Float32`'s fast tier on `self as Float32`.
  Double rounding needs care, so the tier's margin also covers rounding
  to 11 bits.
- **Verification:**
  - Run the existing `--ignored` exhaustive test
    (`float32_agrees_with_the_float64_path_on_every_input`) for each
    function. It checks all 2^32 inputs against the correctly rounded
    path, so a fast tier is proven right, not sampled.
  - `math.rs` tables.
  - Add `Float32` rows to `bench/math` and `bench/math/c`.
- **Script:** `numeric_types.pl` gains a per-function `%quick32` entry,
  so the generated `Float32` method tries the fast tier first, as
  `%quick` does for `Float64` `exp`/`ln`.

## Step 4: `Float64` fast tiers

This is the bulk of the work. Each fast tier follows `exp_fast`:
- plain doubles with a short double-double correction where it matters;
- a relative error below 2^-66, proven in its `///` comment;
- `quick64`'s 2^-63 margin;
- otherwise, the existing kernel and 384-bit fallback.

CORE-MATH (MIT-licensed) has a published fast path for each function.
Its designs, table sizes and error analyses are the reference; credit it
in the file headers where a design is followed.

In order, by how much each family reuses what is already there:

1. **Exponentials:** `exp2`, `exp10` and `exp_m1`.
   - `exp2` and `exp10` are `exp_fast` with a different reduction
     constant (`exp10` takes `x log2(10)` as a pair).
   - `exp_m1` uses `exp_fast` for `|x| > 1/4`, otherwise a minimax
     polynomial.
2. **Logarithms:** `log2`, `log10` and `log(base)` are `ln_fast`'s pair
   times a double-double constant. `ln_1p` is `ln_fast` on `1 + x` as an
   exact pair, with a polynomial near zero.
3. **Hyperbolic:**
   - `sinh`, `cosh` and `tanh` combine two `exp_fast` pairs; `sinh` and
     `tanh` use a polynomial for small `|x|`.
   - `asinh`, `acosh` and `atanh` use `ln_fast`/`ln_1p`'s pair on an
     argument built in double-double.
4. **Trigonometry:** `sin`, `cos`, `tan`, `sin_cos`, and the `*_pi`
   functions.
   - Reduce with Cody–Waite or Payne–Hanek (both exist).
   - Then use a table of `sin`/`cos` at `k/128` (double-double entries)
     and two short polynomials in the remainder.
   - `tan` is the quotient of the pair, using a double-double division.
   - The `*_pi` functions reduce exactly, then reuse the same table.
5. **Inverse trigonometry:**
   - `atan` uses a table of `atan(k/64)` and the identity
     `atan x = atan c + atan((x - c)/(1 + xc))`, with a short
     polynomial.
   - `atan2` uses the same, after an exact quotient pair.
   - `asin`/`acos` use a polynomial near zero, and near 1 use `sqrt` of
     `1 - x` as a pair.
6. **Power and root:**
   - `cbrt`: a bit-trick estimate, two Newton steps in doubles, and one
     double-double correction.
   - `hypot`: an exact sum of squares as a pair, then a corrected `sqrt`.
   - `pow`: CORE-MATH's fast path, which needs `ln x` to about 2^-68 as a
     pair (a longer `ln_fast`) and `exp` of a pair (`exp_fast` taking
     `r.lo`). `root` and `pow_int` reuse it.
7. **Special:** `erf` and `erfc` with piecewise minimax polynomials (and
   `exp_fast` for `erfc`'s tail); `gamma` and `ln_gamma` with a
   polynomial on `[1, 2]` plus the recurrence, and Stirling for large
   arguments. These stay within 1 ulp, so the margin test only has to
   protect that promise.

For each function:
- Run `stress.py` on fresh rows.
- Check that the worst-case rows still pass. They reach the fallback,
  which tests the margin.
- Add a counter build (Debug only, or a temporary benchmark flag) that
  measures the fallback rate. It should be below 1 in 1,000.
- Re-measure `bench/math` and update the table in `math-plan.md`'s phase
  5 status.

## Step 5: FMA

- Phase 8's `--cpu` and `@builtin.Target.Features.fma`:
  - `Dd.two_prod` becomes one `multiply_add` when `fma` is set;
  - the fast tiers' polynomials use `multiply_add`.
  - Correct rounding makes both paths return the same bits, and
    `math.rs` runs at the highest `--cpu` too.
- Runtime dispatch (deferred in phase 8) is what closes the gap on
  baseline builds, since the C runtime does it:
  - a `@x86_cpuid` intrinsic;
  - a global function table filled at entry, like `default_allocator`.
  - Plan it only if baseline numbers stay above target after step 4.

## Order and size

| step | size | gain |
|---|---|---|
| 0. static functions, `Float` | small (generator plus callers) | the surface: `Float64.sin(x)`, `Float.sin(x)` |
| 1. internal linkage | small | about 40% on small functions, every program |
| 2. minimax fits | small to medium | needed by 3 and 4 |
| 3. `Float32` fast tiers | medium (one pattern, proven exhaustively) | about 100 times, to C's speed |
| 4. `Float64` fast tiers | large (about 15 designs) | 10 to 100 times, to within 1.5 times C |
| 5. FMA | medium (phase 8) | about 1.5 times on top, with `--cpu` |

Steps 0, 1 and 2 are independent and can run in parallel. Step 3 can
start once steps 0 and 2 land. Step 4's families are independent of each other once
step 2 lands: one session per family, each touching only its own kernel
file, the shared `float_tables.lilac` generator section, and its own
`%quick` entries.
