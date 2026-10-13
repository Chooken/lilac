# `!=`, `not`, and compound assignment

Three additions, none of them a new hook:

- `a != b` is the negation of `==`: bit inequality of two values of one
  type. It cannot be overridden, exactly like `==`.
- `not c` is the negation of a `@bit8` condition, built in like `and`/`or`.
  It cannot be overridden either.
- `x += e`, `-=`, `*=`, `/=`, `%=` are statements that mean
  `x = x + e` (and so on): the existing `@add`/`@sub`/`@mul`/`@div`/`@mod`
  hook, found by exact signature, then an ordinary assignment. SPEC.md
  already says `a += b` desugars to `a = @add(a, b)`; this makes it true.

`!=` and `not` fold wherever `==` and `and`/`or` fold: constant conditions
in lowering, top-level conditional items, global initializers, `=>`
constants and inline calls, and typeck's template `E037` guard.

## Decisions

1. **`not` is a prefix operator like `-`.** It binds tighter than every
   binary operator and `as`, and looser than postfix member access and
   calls. So `not a == b` is `(not a) == b`, `not x as T` is
   `(not x) as T` (as `-x as T` is today), and `not a.ok()` is
   `not (a.ok())`. To negate a comparison, write `a != b` or
   `not (a < b)`.
2. **`!=` is `not (a == b)` in the HIR**: one new node, `HirExprKind::Not`,
   serves both, so every pass that understands `Eq` needs only `Not` added.
   No separate `Ne` node or LIR instruction (LLVM folds the double `icmp`).
3. **Compound assignment is desugared in typeck, not the parser.** Copying
   the target expression in the parser would record each local use twice
   (navigation, unused-local hints) and report a bad target twice. Typeck
   checks the target place once and reuses its HIR as the left operand.
   A place is a variable, global or field path, so it has no side effects
   and reading it twice is the same as writing `x = x + e`.
4. **Compound assignment is a statement only**, like `=`.
5. **No `@add_assign`-style hooks.** In-place update stays the
   composition of the existing hook and assignment. Operators already
   resolve by exact signature, and this keeps it that way.

## Lexer (`lilac-lex`)

- Two-character tokens in the operator table (next to `==`, `<=`):
  `BangEq` (`!=`), `PlusEq`, `MinusEq`, `StarEq`, `SlashEq`, `PercentEq`.
  `!` alone stays `Bang`; `//` still wins over `/=` because comments are
  lexed first. Check that.
- Keyword `not` → `TokenKind::KwNot`, added to the keyword list (line 22),
  which also feeds completion. No `lib/` or `tests/` program uses `not` as
  an identifier (checked: it shows up only in strings and comments).
- Lexer tests: each new token, `a!=b` with no spaces, `x/=2` against
  `x//c`.

## AST (`lilac-ast`)

- `BinOp::NotEq` (doc: "`!=` (strict bit-inequality, never overridden)").
- `Expr::Not { value: Box<Expr>, span }` (keyword through operand), added
  to `Expr::span`.
- `Stmt::CompoundAssign { target: Expr, op: BinOp, op_span: Span, value:
  Expr, span }`, where `op` is one of `Add`/`Sub`/`Mul`/`Div`/`Mod`, added
  to `Stmt::span`.

## Parser (`lilac-parse`)

- `binary_power`: `BangEq` at 3, beside `EqEq`. `binary_op`: `BangEq →
  NotEq`.
- `not` in `parse_unary`: add `KwNot` to the prefix loop beside `Minus`
  and `KwRef`, and wrap the operand in `Expr::Not` in the same
  right-to-left pass, through `Parser::link`. Prefixes mix
  (`not -x`, `- not x`) and the operand is a postfix expression, exactly
  as for `-`. Each prefix is already a nesting level (AGENTS.md: a new
  recursive syntax form must count its levels), so `not not … x` past
  `MAX_NESTING` is one `E001`.
- `starts_expression`: add `KwNot`.
- `stmt.rs` (assignment, around line 503): after the target, a
  `PlusEq`/`MinusEq`/`StarEq`/`SlashEq`/`PercentEq` token makes a
  `Stmt::CompoundAssign` the same way `Eq` makes `Stmt::Assign`. In a
  declaration (`x: T += 1`) the existing "expected `=`" error stands.
- Tests: precedence (`not a == b and c` is `((not a) == b) and c`,
  `not a.b()` is `not (a.b())`, `not x as T` is `(not x) as T`,
  `a != b == c` is left-associative), `x += 1` statement shape, a compound
  assignment inside an expression is an error, deep `not` nesting.

## Typeck (`lilac-typeck`)

- `check_binop`, `BinOp::NotEq`: the `EqEq` branch with the message naming
  `!=`, wrapped in `HirExprKind::Not`. Share the branch rather than copy
  it (`check_equality(fcx, lhs, rhs, span, op)`).
- `Expr::Not`: `check_cond(fcx, value, "not")`, the same as an `and`
  operand (a `@bit8`, or a value with a `@conversion` to `@bit8`, else
  `E010`). Result type `@bit8`, kind `HirExprKind::Not`.
- `Stmt::CompoundAssign` → `check_compound_assign`:
  1. `check_place(fcx, target)` once (bad target: check `value` against
     the error type, as `check_assign` does).
  2. The left operand is the place, auto-derefed (`Self::auto_deref`).
  3. Check `value` with the left operand's type as its expected type, so a
     literal takes it (`x += 1`), as `check_sides` does for a literal side.
  4. Find the hook with the code `check_binop` uses after `check_sides`.
     Pull that out as `operator_call(op, left, right, span) -> HirExpr`
     and call it from both. The `E043` diagnostic labels `op_span` and adds
     a note "`x += e` is `x = x + e`".
  5. The rest is `check_assign` from the point the value is checked: deref
     a `ref` place, `coerce` to the place type, `format_store`. Pull that
     tail out as `assign_checked(place, value_hir, span, out)`. The
     coercion matches the desugaring: an `@add` that returns another type
     needs a `@conversion` back, or it is `E010`.
  The ownership pass then sees the same HIR as for `x = x + e`: the read of
  `x` is a copy or a `Move`, and the assignment's `old` is `Empty` when it
  moved. No change to `places.rs` for this.
- Walks that match every `HirExprKind` need a `Not` arm:
  - `places.rs` (`Pass::expr`, beside `Eq`): `self.expr(inner, false)`.
  - `indices.rs` (`expr_indices`, beside `Eq`): recurse with the same
    `guarded`. If `may_be_constant` matches on kinds, `Not` is constant
    when its operand is.
- Literal folding (`literals.rs`) does not change: SPEC.md says
  comparisons and logical operators are never folded there; they fold in
  the constant evaluator, like `==`.
- Tests (`tests.rs`): `!=` mismatched types is `E010`; `not` of a
  non-`@bit8` is `E010` with the `@conversion` help; `not` through a
  `@conversion`; `x += 1` emits `Assign` whose value calls `@add`;
  `+=` with no hook is `E043` at the operator; a moved non-copyable `x`
  gets `HirOld::Empty`; `self.count += 1` through a `ref` receiver;
  `x += e` in a generic body with no `@add` for `T` folds into `E042`
  where it is instantiated.

## HIR (`lilac-hir`)

- `HirExprKind::Not(Box<HirExpr>)`: "the `@bit8` negation of a
  condition: 1 when it is 0, else 0".
- `HirExpr::walk`: visit the operand. `has_no_effect` needs nothing
  (`not x` has no effect, like `x == y`).

## Lowering (`lilac-lower`)

- `expr.rs`, `value`: `Not(inner)` lowers the operand to a register and
  emits `LirInst::Arith { op: ArithOp::Eq, lhs: operand, rhs: const 0,
  ty: Bit(8) }`. That is right for every `@bit8`, not only 0 and 1,
  because conditions are true when nonzero (`Value::truthy`).
- `control.rs`, `branches`: no new arm; `walk` reaches an operand that
  branches. Where a condition is lowered straight to a branch, a `Not`
  could swap the targets instead of computing a value. That is optional,
  since LLVM does it anyway.
- **Constant folding** (`constant.rs`, `eval`): `Not(inner)` evaluates
  `inner`, takes `truthy()?` (an aggregate is `None`, so not constant), and
  returns `Value::Int(u64::from(!t), 8)`. `!=` is then
  `Not(Eq(..))` over the existing `Eq` arm. This one arm covers every
  consumer of the evaluator:
  - constant conditions (only the live arm of `if not DEBUG { … }` is
    lowered);
  - top-level conditional items (`lilac_lower::evaluate_selectors`), so
    `if not @builtin.Target.libc { … }` at the top level picks an arm;
  - global initializers (`G: @bit8 = A != B`) and `=>` constants;
  - `=>` inline calls in `Lower::inline_call_panic`, so a division guarded
    by `if d != 0x0` in a `=>` hook is not reported as `E060` when `d`
    is known.
- Tests: evaluator unit tests for `not` of 0, 1 and 2, `!=` over
  integers, objects and enum variants, and `not` of a non-constant
  (`None`).

## Editor (`lilac-lsp`, `editors/vscode`)

- `render.rs`: `power(BinOp::NotEq) = 3`, `op_text` `"!="`; `Expr::Not`
  renders like `Expr::RefOf`: `not ` and then
  `operand(value, CAST_POWER, …)`, so a binary or cast operand is
  parenthesized (`not (a == b)`). Its own `expr_power` stays the default
  (tightest), so `(not a) == b` renders as `not a == b`, which parses
  back the same way. Add a test for a constant whose initializer uses
  both.
- The completion keyword list comes from the lexer's list, so it picks up
  `not`. `context.rs` needs nothing, since it reads tokens.
- `lilac.tmLanguage.json`: `\b(and|or|not)\b` as `keyword.operator`, and
  `!=`, `+=`, `-=`, `*=`, `/=`, `%=` in the `operators` pattern, longest
  first so `+=` is not colored as `+` then `=`.

## Docs

- SPEC.md, operator table: rows for `lhs != rhs` (not a hook: `not (lhs
  == rhs)`), `not value` (built in, like `and`/`or`) and the compound
  forms (`x op= e` is `x = x op e`, a statement; the target is evaluated
  once). State in the text that `not` binds like unary `-`.
- SPEC.md, constant-expression table (line ~343): `a != b` and `not c`.
- SPEC.md, "Strict Equality": `!=` is strict too.
- AGENTS.md, pipeline notes: one line under typeck saying where
  compound assignment is desugared.

## Run tests (`tests/run/`)

- `not_equal.lilac`: `!=` on integers, an object, enum variants with and
  without payloads; `not` on comparisons and on a `@bit8` that is 2.
- `compound_assign.lilac`: each of the five on `@bit32`, a field
  (`p.x += 1`), a field through a `ref` parameter, a global, a
  user-defined `@add` on an object, and `/=` by a zero known only at run
  time, which panics with "division by zero" through `Standard`'s hooks.
  A literal `x /= 0x0` should instead be `E060` (`inline_call_panic`
  already sees the `=>` hook's constant argument); a typeck or lowering
  test covers that.
- `const_cond_not.lilac`: a `@compile_error` in the arm that
  `if not CONSTANT` and `if A != B` rule out; a top-level
  `if not @builtin.Target.libc` item; a global initialized with `!=`.
- `FAILS_compound_assign_no_hook.lilac` (or a typeck test): `E043` at
  `+=`.

## Order of work

1. Lexer and AST, then the parser with its tests.
2. `HirExprKind::Not` and every exhaustive match (the compiler lists
   them); `!=` and `not` in typeck; lowering and the evaluator arm.
3. Compound assignment in typeck (`operator_call`, `assign_checked`
   extracted first, as a no-behavior-change step).
4. LSP rendering, the grammar, SPEC.md, then run tests and the gates.
