# Reflection Plan

> Status: proposed, 2026-10-26.
>
> Goal: a generic body can ask what its type arguments have, and pick
> an implementation by the answer, **with every arm type-checked in
> every instance**. `Map` can hash a key with its own `hash` when it
> has one, and hash its bytes when it does not:
>
>     hash_key[K]: function (key: ref K) hash: @bit64 {
>         if @has_method(K, hash, function (ref K) @bit64) {
>             hash = key.hash()
>         } else {
>             hash = Wyhash.hash(0x0, bytes_of(key))
>         }
>     }
>
> Each test states the exact type of what it asks for, folds to a
> `@bit8` literal per instance (like `@needs_drop(T)`), and wherever
> the code runs only when the test is true, typeck **assumes** the
> member exists with that type. For a `K` that has it, the assumption
> is the real member (the test is true only on an exact match). For a
> `K` that does not, uses of it there check against a stand-in of the
> stated type. Lowering compiles only what a constant condition can
> reach, so a stand-in never reaches it.
>
> So a dead arm is never skipped. The test excuses exactly the member
> it names; a typo, a wrong argument, a missing conversion or any
> other member's use there is reported in every instance, as it is in
> a runtime `if`.
>
> Decisions:
> - **Every arm is checked** (agreed). No condition makes typeck skip
>   code. `@builtin` conditions keep checking both arms too.
> - **A false test is silent** (agreed). Reflection is for the
>   generic's writer, who handles the other case. A `K` whose `hash`
>   has another type, or that has none, simply takes the `else`; no
>   hint, no error, for the writer or the generic's user.
> - **A test states the member's type** (proposed): a field's type, a
>   function's exact signature (receiver included, failure outs
>   included: phase 1). An exact match, after the explicit-operator
>   rule; a generic member (`convert[U]`) never matches, since no type
>   can spell one.
> - **A test answers "would this use resolve here?"** (proposed).
>   Visibility counts (`Checker::private_denied`): a private field of
>   the user's type is not there for a generic in `Standard`.
>   References are looked through, as member access does. A type with
>   no members (a builtin, an array, a function reference) has none:
>   false, never an error.
> - **Members are named by identifier** (proposed):
>   `@has_field(T, count, Int)`, not `"count"`. When the member exists
>   the identifier is a use of it (hover, references, rename).
> - **Results are `@bit8`** (proposed), like `@needs_drop`. The
>   compiler names no `Standard` type.

## The tests

| Test | True when | Assumed where it holds |
| --- | --- | --- |
| `@has_field(T, name, F)` | `value.name` is a field of type `F` | `value.name: F` |
| `@has_method(T, name, function (R, P…) O)` | `value.name(…)` resolves to a method of exactly that type, `R` its receiver (`ref T`, or `T` for `self: T`) | that method |
| `@has_function(T, name, function (P…) O)` | `T.name(…)` resolves to a static function of exactly that type | that function |
| `@has_constant(T, NAME, C)` | `T.NAME` is a member constant of type `C` | `T.NAME: C` |
| `@is_copyable(T)` | a by-value use copies (`Checker::is_copyable`) | nothing (phase 5) |
| `@is_same(T, U)` | one type (aliases are transparent: by `TypeId`) | nothing |
| `@needs_drop(T)` | (exists) dropping a `T` runs a hook | nothing |

A test that assumes nothing still picks an arm, but both arms must be
valid for every `T`, as for any constant condition: enough to choose a
faster path that is valid anyway (`@needs_drop(T)` before a loop of
drops).

The first operand is `Slot::Type` (a type, or a value standing for
its type, `size_of_operand`); the name is a new descriptive slot,
`Slot::MemberName` (a bare identifier, `E022` otherwise); the last is
a type, where a bare function type is allowed as it is for an
alias's target (`Checker::bare_function_type`).

### Where a test holds

A test's assumption holds in every expression and arm that runs only
when the test is true. `and` and `or` short-circuit, so this is
decided by their shape, wherever the test sits (phase 3 makes
lowering drop all of it when the test is false):

- `if t { A } else { B }`: `A`. `if not t { A } else { B }`: `B`.
- `a and b`: `b` runs only when `a` is true, so it sees `a`'s
  assumptions; the `then` arm sees both. `x > 0 and t and value.n > 0`
  is fine: `value.n > 0` and the `then` arm run only after `t`.
- `a or b`: `b` runs only when `a` is false, so it sees what `not a`
  proves; the `else` arm sees what `not a` and `not b` prove. The
  `then` arm sees nothing either one proves alone
  (`if not t or x { … } else { B }`: `x` and `B` see `t`).
- An `else if` link sees what the earlier links' failed conditions
  prove, then its own.
- A value `if` and an `and`/`or` outside a condition follow the same
  rules.

## Phase 1: failing function types

A test must be able to state a member that can fail, so the type is
exact. This is not syntax: the parser already reads
`function (…) outs else outs` (`FuncType::failures`). Typeck refuses
it (`E057`, `resolve_function`) because `TypeKind::Func` has no place
for failure outs, which was enough while function values could not
fail (`docs/function-values-plan.md`).

- `TypeKind::Func` gains `failures: Option<Vec<TypeRef>>`.
  `resolve_function` fills it; `Types::display`, the mangler,
  `demangle`, `contains_param`/`substitute` and the editor's
  `render::type_outs_text` print and walk it (`function (…) T else E`,
  outs parenthesized past one, as for successes).
- A failing function type is accepted in a test's type slot and as an
  alias's target. `ref` of one stays `E057` ("a function value cannot
  fail yet"): values are a follow-up (below), not needed here.
- A member's own type (for the comparison) is built from its
  declaration's `failures`, so `next: function (self) item: T else
  nothing` is `function (ref Iterator[T]) T else nothing`.
- Tests: display and mangle round trips; `E057` still on `ref` of a
  failing type.

## Phase 2: the tests

- `lilac-hir/src/intrinsics.rs`: the new `Intrinsic`s, `is_special`,
  `signature` (`Slot::Type`, `Slot::MemberName`, the type slot, out
  `@bit8`), `doc`, and the lookup test.
- `lilac-typeck/src/reflect.rs` (new): `check_reflect`, routed beside
  `@needs_drop`. It resolves the type operands, then answers by the
  lookups a use takes: `field_access`'s field search, `method_call`'s
  member search, `static_member` / `member_constant`, each through
  `private_denied` with the call's span and no diagnostic, then
  compares the found type with the stated one. It returns
  `HirExprKind::Int(0 or 1)` at `@bit8`.
- A template (the type contains a parameter) gets no answer: a `@bit8`
  the template keeps, never lowered (`bug!` in `lilac-lower` if one
  arrives). A query that cannot vary with the arguments is still
  answered: `List[T]`'s fields are `List`'s whatever `T` is, but a
  field's type may contain `T` (then the comparison waits too).
- A found member records its use (name sites), so the identifier is a
  reference of the member. A miss records nothing, and says nothing.
- Run test (`tests/run/reflection_tests.lilac`): each test true and
  false against a structure, a builtin, a `ref`, an alias, a private
  member seen from another namespace, a same-named member of another
  type and a failing method, in an ordinary `if` whose both arms are
  valid for every `T`.

## Phase 3: conditions whose outcome is known

`and`/`or` already short-circuit: `logic_value` branches on `a` and
runs `b` only in its own block. But the result is a temporary, and
the `if` branches on it at run time. In `if x and t { A }` with a
false test `t`, both paths write `0x0` to the temporary, yet `A` is
still lowered, since only a wholly constant condition
(`self.constant`) picks one arm. `A` can never run, but a stand-in in
it would reach the backend.

- `lilac-lower/src/constant.rs`: `Lower::outcome(cond) ->
  Option<bool>`, the condition's truth when it is known even though
  part of it runs: a constant is its value; `not c` flips `c`'s;
  `a and b` is false when either side's is false and true when both
  are; `a or b` is true when either side's is true and false when
  both are false. It evaluates nothing: it only says which way the
  branch would go.
- `control.rs`: where `HirStmt::If` and `if_value` now ask
  `constant`, a known outcome that is not a constant lowers the
  condition for its effects (`x` still runs, its value is dropped),
  then only the arm it picks. `logic_value` asks `outcome` of `a` the
  same way, so in `(x and t) and value.n > 0` the right side is never
  lowered: the left side runs, then the result is `0x0`.
- Nothing else changes: the short-circuit order, the temporaries, and
  every condition whose outcome depends on a run-time value lower as
  now.
- Run tests: `x() and t` still calls `x` once; a false test after a
  run-time operand leaves no `then` arm (no stand-in reaches the
  backend); `or`, `not` and nesting with side effects in each operand
  give the same results as before.

## Phase 4: assumptions

- `FnCx::assumed`: a stack of `Assumption { owner: TypeId, name,
  member: Assumed, held: bool }` (`Assumed::Field(TypeRef)`,
  `Method(TypeRef)`, `Function(TypeRef)`, `Constant(TypeRef)`, the
  function ones a `TypeKind::Func`), pushed where a test holds (above)
  by `check_if`, `check_if_value`, the `and`/`or` operands and `not`,
  and popped when that code ends. `held` is the test's answer, unknown
  in a template.
- Member lookup (`field_access`, `method_call`, `static_member`,
  `own_value`/`own_call` for bare own names) consults the stack when
  the owner and name match. A held assumption changes nothing: the
  real member is found and matches. Otherwise the use checks against
  the stand-in:
  - a field is `HirExprKind::Assumed { base, name }` of type `F`: a
    place read and written as through a `ref` (the ownership pass
    borrows `base`, and moves out of it only as out of `*p`);
  - a method or function is `HirCallee::Assumed { name, sig }`: its
    arguments and outs check against `sig` and move like any call's,
    and one that can fail must be handled (`FnCx::handled`) and binds
    its outs as a named failing call does (`check_subject`'s `Fails`);
  - a constant is `HirExprKind::Assumed` with no base.
  Nothing else changes: every other expression checks as it does now.
- The `if` stays an ordinary `if` with both arms in the HIR, so the
  ownership pass, liveness and `E063`/`E079` treat the body exactly as
  for a runtime `if`, the same in every instance. Lowering reaches no
  stand-in (phase 3) and calls `bug!` on one it meets; the constant
  evaluator never evaluates code a false test guards.
- A template checks with unheld assumptions, so an unused generic's
  guarded code now type-checks against the stated signature; only
  uses of members no test names stay dependent, as today.
- Editor: hover on a stand-in shows the assumed type and the test
  that assumed it; definition goes to the test.
- Run tests: `hash_key[K]` above for a key with `hash`, one without,
  and one whose `hash` returns another type (takes the `else`); a typo
  in a dead arm is reported in every instance; a stand-in field
  written, read and passed by `ref`; a stand-in method with a
  by-value receiver; a failing stand-in in a capture `if`; an
  assumption through `x and t and value.n > 0` and through `not t or
  x`; a move in a dead arm, then a use after the `if`, is `E063` like
  a runtime `if`'s.
- `Standard.Collections.Map` hashes keys through such a test, and a
  run test uses a key type with no `hash`.

## Phase 5: hooks, variants, copyability

- `@has_hook(@format, ref T, ref W)`: whether a hook of exactly these
  parameter types binds (`@format`, operators like `@add(T, T) T`, by
  the exact-signature lookup an operator uses). Where it holds, an
  f-string hole or an operator binds a stand-in hook.
- `@has_variant(T, Name, P)` (or no payload): a stand-in variant needs
  a tag, in construction and as a `match` arm's pattern.
- `@is_copyable(T)` assuming copyability: the ownership pass copies
  there instead of moving; the copy glue is never lowered.
- Completion of the name operand lists the type's members of the
  test's kind when the type operand is concrete.

## Not in this plan

- Failing function values (`ref f` of a function that can fail): the
  type exists after phase 1, and lowering needs nothing (the failure
  flag is an out), but `value_call` must go through the failing-call
  path and C function types must refuse failure (`E028`). Its own
  change in `docs/function-values-plan.md`.
- Interfaces: a named bundle of tests (`@satisfies(T, Describable)`)
  a generic could also require up front. The tests above are its
  parts, so this plan does not block it.
- Iterating over fields (`@fields(T)`), names as text
  (`@type_name(T)`), or any run-time reflection.
- Deciding `=>` predicates (`is_hashable[T]()`) as tests.
