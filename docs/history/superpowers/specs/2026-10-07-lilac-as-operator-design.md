# Lilac `as` Operator

> Scope: add the postfix `as` cast — the explicit form of the implicit
> `@conversion` path, usable in any expression position — with the builtin
> `ref unknown` ↔ any `ref T` exception. Third and final numeric
> sub-project. Branch `ai`. LLVM-only.

## 1. Outcome / constraints / success

**Outcome**

- `expr as T` is a postfix cast. `as` is a reserved keyword, left-associative,
  binding tighter than binary operators and looser than `.`/call/index.
- `as T` supplies `T` as the operand's expected type:
  - a **literal** operand pins/grows to `T` with no conversion (`0x1 as @bit16`
    is identity; `5 as Int8` grows then converts; `3.14 as @fbit32` pins);
  - a **typed** operand not already `T` requires exactly one `@conversion` to
    `T`, else `E043`;
  - a redundant `x as T` (already `T`) is an identity no-op.
- Builtin exception: `ref unknown` ↔ any `ref T`, both directions, no hook —
  a no-op pointer cast.
- `as` is usable where implicit conversion is not (e.g. an operator operand).

**Constraints**

- Compiler crates stay Rust std-only; no new dependencies. `unsafe_code` and
  `clippy::pedantic` hold.
- `as` uses the same one-step `@conversion` machinery as the implicit path
  (`ops::convert`/`convert_if_needed`); no chaining of multiple conversions in
  one `as`.
- No new HIR variant and no lowering/codegen change: a real cast emits the
  same `@conversion` call the implicit path emits; the ref-unknown cast is a
  retype (identity at codegen).
- The implicit conversion path (assignment, arguments, returns) is unchanged.

**Success**

- `x as @bit16` where `x: @bit8` and a `@bit8 → @bit16` `@conversion` exists
  compiles; without one it is `E043`.
- `0x1 as @bit16` is `@bit16`; `x: @bit32 = 5 as @bit32` is `@bit32`;
  `3.14 as @fbit32` is `@fbit32`.
- `x as @bit16` where `x: @bit16` is allowed (identity).
- `p as ref Foo` (p is `ref unknown`) and `q as ref unknown` (q is any
  `ref T`) compile with no hook.
- `a + b as @bit16` parses as `a + (b as @bit16)`.
- `cargo test --workspace` is green.

**Non-goals**

- Chaining multiple `@conversion` steps in one `as`.
- Builtin numeric casts beyond the `ref unknown` exception (bit↔fbit
  reinterpretation stays the `@bits_to_fbit_N`/`@fbit_to_bits_N` intrinsics).
- `as` for `@fbit128`-specific handling.

### 1.1 Decisions locked during brainstorming

1. **`ref unknown` ↔ any `ref T`**, both directions, ref-to-ref only, builtin
   (no hook). A non-ref target still needs a `@conversion`.
2. **Postfix, left-associative**, tighter than binary operators, looser than
   `.`/call/index; `as` is reserved.
3. **`as T` supplies `T` as the operand's expected type**: literals pin/grow;
   a typed operand forces one `@conversion`; a redundant cast is identity.
4. **Reuse `E043`** for a missing conversion; a redundant `x as T` is allowed.

## 2. Problem

The implicit conversion path (`expect_type`/`convert_if_needed`, SPEC §8)
applies a single `@conversion` at typed slots — assignment, arguments, returns.
There is no way to convert in an arbitrary expression position (e.g. an
operator operand, which is never converted), and no way to make a conversion
explicit. `as` adds that, and the `ref unknown` escape hatch for opaque
pointers.

## 3. Architecture / components

```
crates/lilac-lex/src/lib.rs        # KwAs (reserved)
crates/lilac-ast/src/lib.rs        # Expr::Cast(Box<Expr>, Type)
crates/lilac-parse/src/lib.rs      # parse_postfix: `as Type`
crates/lilac-typeck/src/check.rs   # check_cast: expected type + one @conversion + ref-unknown builtin
crates/lilac-typeck/src/ops.rs     # (reuse convert; no change unless a helper is needed)
SPEC.md, docs/errors/*             # document `as`
```

**Untouched:** `lilac-hir` (no new variant), `lilac-lower`,
`lilac-codegen-llvm`, `lilac-ir`, `lilac-const`, `lilac-resolve`,
`lilac-module`, `lilac-lsp` (beyond syntax/formatting awareness).

## 4. Surface and precedence

- The lexer reserves `as` (`KwAs`); it can no longer be a user identifier.
- The AST gains `Expr::Cast(Box<Expr>, Type)`.
- `parse_postfix` (which already loops over `.`, `(`, `{`, `[`) gains an `as`
  arm: after the postfix chain, an `as` consumes a type (`parse_type`, which
  already accepts `ref T`). The loop makes `as` left-associative and
  tighter-than-binary.
- `a.b as T` is `(a.b) as T`; `f(x) as T` is `(f(x)) as T`; `a + b as T` is
  `a + (b as T)`; `x as A as B` is `(x as A) as B` (two single-step casts).

## 5. Type checking `as T`

`check_cast` (in `lilac-typeck/src/check.rs`):

1. Check the operand's type first (with no expected type). If it is
   `ref unknown` and `T` is a ref type, or the operand is a `ref T'` and `T` is
   `ref unknown`, apply the §6 exception and stop. (The exception is checked
   before the expected-type path so the pointer cast is not mistaken for a
   `ref` mismatch.)
2. Otherwise, check the operand with `T` as its expected type
   (`check_expr(operand, Some(T))`). A literal operand therefore pins/grows to
   `T` (sub-project 2); a typed operand is checked against `T`.
3. If the operand's resulting type is exactly `T` and **both are non-`ref`**,
   the cast is identity: return the operand HIR with type `T`. (The `ref`
   cases are §6's exception when one side is `ref unknown`; any other `ref`
   mismatch, including `ref T as T`, is `E010` — a `ref` never silently
   becomes a non-`ref` value.)
4. Otherwise, resolve exactly one `@conversion` from the operand's type to `T`
   via the existing machinery (`ops::convert` + `convert_if_needed`). Success
   yields the `@conversion` call HIR typed `T`; failure is `E043`.
5. A `ref`-mismatch that is not the §6 exception is `E010`, consistent with
   `expect_type`.

Because step 2 threads `T` as the expected type, a literal pins without a
conversion; a non-literal must have a real `@conversion`. No new HIR node is
needed: a real cast is the conversion call, and an identity cast is the operand.

To check the operand exactly once, `check_cast` branches on whether `T` is a
ref type: when `T` is `ref _`, it checks the operand with no expected type and
applies §6 (or `E010`); otherwise it checks the operand with `T` as the expected
type and applies the identity/`@conversion` path.

## 6. The `ref unknown` exception

Before the `@conversion` lookup, `check_cast` handles the pointer exception:

- operand `ref unknown`, target `ref T` (any `T`, including `ref unknown`
  itself): retype the operand to `ref T`, no hook, no codegen change.
- operand `ref T`, target `ref unknown`: retype to `ref unknown`.
- Any other `ref`-mismatch falls through to the normal path (`E010`/`E043`).

`unknown` stays legal only behind `ref` (unchanged); `as ref unknown` is the
only way to produce it from a concrete `ref`, and `as ref T` is the way to
recover a concrete reference.

## 7. Lowering and codegen

No change. A real cast is the `@conversion` call HIR, which lowers like any
other call. The ref-unknown cast and the identity cast produce no instruction
(both are opaque pointers / the same value), so `lilac-lower` and
`lilac-codegen-llvm` are untouched.

## 8. Errors and docs

- A missing conversion is `E043` (existing). A non-exception ref mismatch is
  `E010`. `docs/errors/E043.md`/`E010.md` gain an `as` example.
- `SPEC.md` gains an `as` section: the surface/precedence, the `as T`
  semantics, the ref-unknown exception, and that `as` is reserved.
- `docs/errors/` and `SPEC.md` must not contradict the implicit-conversion
  rules (one step, exact match, no chains).

## 9. Testing

- **`lilac-lex`:** `as` lexes as `KwAs`; it is rejected as an identifier.
- **`lilac-parse`:** `x as T`, `a.b as T`, `f(x) as T`, `a + b as T`
  (→ `a + (b as T)`), `x as A as B` (left-assoc), `x as ref T`.
- **`lilac-typeck`:** `@bit8 as @bit16` with a hook compiles and emits the
  conversion; without a hook is `E043`; `0x1 as @bit16` is `@bit16`;
  `x as @bit16` (already `@bit16`) is identity; `3.14 as @fbit32` is `@fbit32`;
  `ref unknown as ref Foo` and `ref Foo as ref unknown` compile with no hook;
  a non-`unknown` ref mismatch is `E010`.
- **`lilac-cli`:** an `emit-llvm` fixture exercising a real cast and a
  ref-unknown cast through codegen.
- All gates stay green.

## 10. Gates

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`

## 11. Maintenance / source of truth

- `as` and the implicit path share one conversion mechanism (`ops::convert`);
  neither chains, and both are exact-match. A change to conversion rules
  applies to both.
- The `ref unknown` exception is the single pointer-cast escape hatch.

## 12. Follow-ups

- None required for the numeric refactor; `as` completes it.
- If wanted later: a `ref unknown → ref T` null-check convention, or an
  explicit "unchecked cast" marker for the pointer exception.
