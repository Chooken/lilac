# Lilac Inline Control Flow & Stdlib Compile Fixes

> Scope: let inline `=>` bodies contain control flow (short-circuit `and`/`or`
> and `if`-expressions) by carrying a statement prelude through inline
> expansion; stop the `Cmp` intrinsic from seeding operand width from its
> `@bit8` result type; fix the intrinsic-name typos in `lib/Standard`; add the
> standard library as a CLI test; and remove the VM integration/bench tests.
> Branch `ai`. Compiler crates only.

## 1. Outcome / constraints / success

**Outcome**

- An inline (`=>`) function body may contain any value-position control flow —
  short-circuit `and`/`or`, `if`-expressions, `match`-expressions — and still
  inline at each call site with correct per-call temp identity.
- `@i_less_than` / `@i_greater_than` / `@f_less_than` / `@f_greater_than`
  accept operands of any width; the operands are inferred from the operands
  themselves, never from the comparison's `@bit8` result type.
- `lilac check lib/Standard` and `lilac emit-ir lib/Standard` both exit 0.
- The VM integration and bench tests are gone, along with their gate lines.

**Constraints**

- Compiler crates stay Rust std-only; no new dependencies. `unsafe_code` and
  `clippy::pedantic` hold.
- `=>` bodies keep textual-substitution semantics: an argument used twice is
  evaluated twice (the existing `subst_ident` behavior).
- No grammar, AST, HIR-shape, lowering, `lilac-ir`, or backend changes: the
  prelude is ordinary HIR statements that lowering already handles.
- Existing behavior is preserved for block bodies, `if`-statements, and
  non-control-flow `=>` bodies.

**Success**

- The `Standard` shape `@less_than_or_equal: function (lhs: T, rhs: T) Bool =>
  (lhs == rhs) or @i_less_than(lhs.data, rhs.data)` checks and emits.
- `@f_less_than(lhs.data, rhs.data)` with `lhs.data: @bit32`/`@bit64` inside an
  `or` (where a `@bit8` is expected) no longer reports `E010`.
- `f( a or b, c or d )` inlines two copies of the same body with distinct join
  temps (no aliasing).
- `lilac check lib/Standard` is clean; `lilac emit-ir lib/Standard` succeeds.
- `cargo test --workspace` is green with the VM test files removed.

**Non-goals**

- A C ABI for aggregate/float types, `@c_*` builtins, or a native
  `i8..i64`/`f32`/`f64` type system (separate sub-project, with `as` casts).
- Runtime `@as_int`/`@as_float` parsing (still compile-time-only fold
  intrinsics). `lib/Standard`'s `@byte_buffer` conversions are unchanged and
  are never reached by the emit test (see §8).
- Making `and`/`or` eager or overloadable — they stay short-circuit builtins
  (SPEC §8).
- Multi-root projects, editor tooling, or changing hook resolution.

### 1.1 Decisions locked during brainstorming

1. **Inline `=>` bodies carry a statement prelude.** The body's control-flow
   statements are captured at check time and emitted at each call site, with
   per-expansion temp renaming. This makes short-circuit `and`/`or` and
   `if`-expressions usable in `=>` bodies.
2. **`and`/`or` stay short-circuit** (SPEC §8); the fix is representational,
   not a semantics change.
3. **`Cmp` never seeds operand width from its result type.** Only `Arith` and
   `Negate` forward the expected type into operand unification.
4. **The stdlib typos are fixed in `lib/Standard`**, not worked around in the
   compiler: `@iadd…@imod` → `@i_add…@i_mod`, and int-file `@negate` uses
   `@i_negate`.
5. **The stdlib test** runs `lilac check lib/Standard` and `lilac emit-ir
   lib/Standard`, asserting both exit 0.
6. **VM tests removed**: `crates/lilac-vm/tests/runner.rs`,
   `crates/lilac-vm/tests/bench.rs`, the `bench-baseline` feature + dead
   `treewalk.rs` if unused, and the two bench lines in `AGENTS.md`.
7. **C ABI / numeric types deferred** to a dedicated sub-project (with `as`
   casts/asserts and the pointer cast case).

## 2. Problem

**Bug 1 — control flow in `=>` bodies (`E030`).** `and`/`or` are short-circuit
builtins (`check_logic_op`, `check.rs:5429`) that desugar to an `HirStmt::If`
over a join `Temp` and push it into the function's statement buffer. An inline
`=>` body is stored as a single `HirExpr` (`Checker.inline_bodies`,
`check.rs:251`) with no statement buffer, so `check_func_body` rejects any
inline value containing a temp via `has_temp` (`check.rs:1137`, `:524`) with
`E030 "control flow in `=>` bodies is not implemented yet (M3)"`. The same
applies to `if`-expressions and `match`-expressions, which desugar the same way.
Inlining itself is trivial; the defect is that control flow leaves the
expression tree.

**Bug 2 — `Cmp` operands forced to `@bit8`.** `arith_out` (`check.rs:4042`)
forwards the expression's expected *result* type into `unify_width`, which seeds
operand width from any bit-typed expected type (`check.rs:3874`). For `Cmp` the
result type is `@bit8` (`check.rs:4058`). When a comparison appears where a
`@bit8` is expected — e.g. the right operand of `or`, seeded via
`check_nested(rhs, Some(bit8))` (`check.rs:5464`) — the operands are unified to
`@bit8`, so `@f_less_than(lhs.data, rhs.data)` with `@bit32`/`@bit64` data
reports `E010 "expected @bit8, found @bit32"`. Comparisons whose expected type
is the `Bool` object (e.g. a `Bool`-returning `=>` body) escape because `Bool`
is not a bit type.

**Stdlib defects** (surfaced once the above are fixed): every `int*.lilac`
calls `@iadd/@isub/@imul/@idiv/@imod`, but the compiler's intrinsics are
`@i_add/@i_sub/@i_mul/@i_div/@i_mod` (`sigs.rs:131`) → `E021`; and every int
file's `@negate` calls `@f_negate` (float) instead of `@i_negate`.

## 3. Architecture / components

```
crates/lilac-typeck/src/check.rs            # InlineBody prelude; capture in check_func_body;
                                            #   substitute + rename temps in check_inline_call;
                                            #   emit prelude at CallOutcome consumers; Cmp width
crates/lilac-typeck/src/check/generics.rs   # same prelude handling for instance_inline
crates/lilac-typeck/src/ops.rs              # (no change; Cmp width is in check.rs)
crates/lilac-cli/tests/stdlib.rs            # new: check + emit-ir lib/Standard
crates/lilac-vm/tests/runner.rs             # deleted
crates/lilac-vm/tests/bench.rs              # deleted
crates/lilac-vm/src/treewalk.rs             # deleted if nothing else uses it
crates/lilac-vm/src/lib.rs                  # drop the treewalk/feature export
crates/lilac-vm/Cargo.toml                  # drop the bench-baseline feature
AGENTS.md                                   # drop the two release VM bench gate lines
lib/Standard/int*.lilac                     # @i_add…, @i_negate
docs/errors/E030.md                         # drop the `=>`-body control-flow trigger
```

**Untouched:** `lilac-hir`, `lilac-lower`, `lilac-ir`, `lilac-codegen-llvm`,
`lilac-lex`, `lilac-parse`, `lilac-ast`, `lilac-resolve`. No HIR variant is
added or removed.

## 4. Inline bodies: data model

`CheckedBody::Inline` and the two body caches gain a statement prelude:

```rust
enum CheckedBody {
    Extern,
    Block(HirBlock),
    Inline {
        param_names: Vec<String>,
        /// Statements the body's control flow produced (emitted at each call).
        prelude: Vec<HirStmt>,
        /// The body's value expression.
        value: HirExpr,
    },
    Rejected,
}

// Checker
inline_bodies: HashMap<DefId, (Vec<String>, Vec<HirStmt>, HirExpr)>,
instance_inline: HashMap<(DefId, Vec<TypeRef>), (Vec<String>, Vec<HirStmt>, HirExpr)>,
```

`CallOutcome::Inline` carries the prelude plus the value and type:

```rust
/// A substituted `=>` body: statements to run, then the value it produces.
struct InlineBody {
    prelude: Vec<HirStmt>,
    value: HirExpr,
    ty: TypeRef,
}

enum CallOutcome {
    Call(CheckedCall),
    Inline(InlineBody),
    CompilerError(HirExpr),
}
```

**Capture.** In `check_func_body`'s `FuncBody::Inline(expr)` arm, after the
existing check/convert, take the statements accumulated in the body block
(`std::mem::take(&mut fctx.blocks[0])`) as `prelude`, and store it with the
value. The `has_temp` rejection and its `E030` are deleted. An inline body with
no control flow yields an empty prelude and behaves exactly as before.

## 5. Inline expansion: substitution and temp renaming

`check_inline_call` (concrete) and the generic inline path
(`check/generics.rs`) both:

1. Fetch `(param_names, prelude, value)`.
2. Check the call arguments (`check_args`) and build the
   `param name -> arg HIR` map.
3. **Rename the body's temps, then substitute.** Allocate one fresh `TempId`
   (`fresh_temp`) per distinct temp in the *un-substituted* body and rewrite
   every occurrence: `HirExprKind::Temp`, and the `out: Option<TempId>` slot of
   `HirStmt::If` and `HirStmt::Match`. Renaming before substitution is
   essential: an argument may itself be a `Temp` whose defining prelude the
   caller already emitted under its original id, so renaming after
   substitution would desynchronize the definition (`use of unassigned join
   temp` in lowering). Renaming the body first makes each expansion
   independent — two inlined copies of the same body in one expression
   (`f( a or b, c or d )`) use distinct join temps — while argument temps pass
   through untouched.
4. **Substitute** params in the renamed prelude *and* the value. `subst_ident`
   is generalized to walk `HirStmt` (recursing into nested `HirBlock`s and
   every embedded `HirExpr`) as well as `HirExpr`.
5. Return `CallOutcome::Inline(InlineBody { prelude, value, ty })`.

**Emission.** Every `CallOutcome::Inline` consumer pushes the prelude into the
current block before using the value, preserving evaluation order:
`check_expr`'s call arm (`check.rs:2493`), the call-as-statement arm
(`check.rs:1638` — still `E022`, prelude not emitted because the statement is
rejected), the multi-`let` arm (`check.rs:1899`), and the poison paths
(`check.rs:5019`–`:5080`, which carry an empty prelude). The prelude lands in
`fctx.blocks.last_mut()` at the point the sub-expression is checked, so nested
preludes are ordered correctly.

## 6. `Cmp` operand width (`check.rs`)

`arith_out` forwards `expected` into `unify_width` **only for `Arith` and
`Negate`**; for `Cmp` it passes `None`. Rationale: `Cmp`'s result is `@bit8`
(SPEC §3), unrelated to operand width; `Arith`/`Negate`'s result width *is* the
operand width, so seeding is correct there. Consequence: a comparison whose
operands are all unanchored literals (`@i_less_than(0x1, 0x2)`) has no width
anchor and reports `E024` exactly as an unanchored `@i_add(0x1, 0x2)` does;
existing tests are checked for this and updated if they pinned the old seeding.

## 7. `lib/Standard` fixes

- `lib/Standard/int.lilac`, `int8.lilac`, `int16.lilac`, `int32.lilac`,
  `int64.lilac`: `@iadd/@isub/@imul/@idiv/@imod` →
  `@i_add/@i_sub/@i_mul/@i_div/@i_mod`.
- Same files: `@negate` body `@f_negate(value.data)` → `@i_negate(value.data)`.
- Any further diagnostics surfaced once the two bugs and the typos are fixed are
  corrected in `lib/Standard` (the goal is `check` + `emit-ir` clean).
- No C ABI changes: `lib/Standard` has no `using`, so `C/` is not loaded by the
  folder loader and is out of scope.

## 8. Stdlib test (`crates/lilac-cli/tests/stdlib.rs`)

A CLI test with the existing `lilac()` helper that runs:

- `lilac check lib/Standard` (path `../../lib/Standard`), asserting exit 0 and
  empty stderr; and
- `lilac emit-ir lib/Standard`, asserting exit 0.

`check` is the meaningful assertion: it type-checks every root `.lilac` file and
exercises both fixes. `emit-ir` is a pipeline smoke test — because `=>` bodies
inline and a library with no `main` is not pruned, the module is the injected
runtime plus any non-inlined declarations, so the stdlib's `@byte_buffer`
`@conversion` bodies (which call the compile-time `@as_int`) are never folded.
This is expected and not a defect.

## 9. VM test removal

- Delete `crates/lilac-vm/tests/runner.rs` and `crates/lilac-vm/tests/bench.rs`.
- Delete `crates/lilac-vm/src/treewalk.rs`, its `lib.rs` export, and the
  `bench-baseline` feature in `crates/lilac-vm/Cargo.toml` if no other consumer
  remains (verify with a grep for `bench-baseline` / `treewalk`).
- Remove the two release VM bench lines from `AGENTS.md`.
- The VM backend itself (`lilac-vm` src, the `emit-vm` CLI path, `lilac-cli`
  VM tests) stays for now; only its integration/bench tests and gates go.

## 10. Errors & docs

- `docs/errors/E030.md`: remove the "control flow in constants and `=>` bodies"
  trigger (no longer emitted); keep any remaining triggers.
- `SPEC.md` §8: note that `and`/`or` remain short-circuit and may now appear in
  `=>` bodies (no rule change; a clarification if the text implies otherwise).
- `RUST_REWRITE_PLAN.md` and historical `docs/superpowers/…` milestone docs are
  left as historical record.

## 11. Testing

- **`lilac-typeck` unit:** an inline body with `or` checks and inlines; an
  inline body with an `if`-expression checks and inlines; `f( a or b, c or d )`
  inlines two bodies with distinct temps; `@f_less_than` on `@bit32`/`@bit64`
  operands inside an `or` is clean; `@i_less_than` on `@bit_native` in a
  `@bit8`-expected position is clean; unanchored all-literal comparison is
  `E024`; block bodies and plain `=>` bodies are unchanged.
- **`lilac-cli` integration:** `stdlib.rs` asserts `check lib/Standard` and
  `emit-ir lib/Standard` exit 0. The test uses the folder (a single
  `int64.lilac` file alone cannot see `Bool`, so folder mode is required).
- **`lilac-vm`:** the crate still builds and its unit tests pass without the
  removed integration/bench files.
- All existing gates stay green.

## 12. Gates

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`

(The release `lilac-vm --features bench-baseline` gates are removed with the
bench test.)

## 13. Maintenance / source of truth

- `InlineBody.prelude` is the only channel for an inline body's control flow;
  `check_func_body` is its single producer and `check_inline_call` the single
  expander.
- Temp renaming is required for correctness of every inline expansion; the
  rename walk must cover `HirExprKind::Temp`, `HirStmt::If.out`, and
  `HirStmt::Match.out`.
- `Cmp` operand width derives from operands only; the result type never seeds
  `unify_width`.

## 14. Follow-ups (separate specs)

- `as` casts/asserts (`0x1 as @bit8`, `ref unknown as ref T`, and the pointer
  case).
- C ABI builtins / aggregate extern passing and a native numeric-type decision
  (`@c_float`/`@c_double`/`@c_int`/`@c_long`/… vs `i8..i64`/`f32`/`f64`), so
  `lib/Standard/C/*` checks and emits.
