# Moves Plan

> Status: phases 1-4 implemented, 2026-10-19; phase 5 proposed.
>
> Goal: a reference never points at a value that moved out from under
> it, unless the code says `@move`. Today a value that cannot be copied
> moves at every by-value use, even when a `ref` to it is still around:
>
>     a: List[Int] = List[Int].init()
>     first: ref List[Int] = ref a
>     b: List[Int] = a        // moves `a`; `b` owns the elements now
>     first.add(1)            // writes through a moved-from place
>
> The move is invisible, and `first` now points at a place the compiler
> treats as empty. When `b` drops, it frees the memory `first` still
> reaches. This plan makes moves of a borrowed place opt-in, then lets
> the compiler move more often where that is safe.
>
> Decisions:
> - **A borrowed place never moves implicitly** (agreed). Once a `ref`
>   to a local, or to a field of one, is taken, a by-value use of that
>   local that would move it is `E079`. `@move(a)` still moves it: the
>   person who writes it takes responsibility for any `ref` left behind.
>   A type that copies is unaffected, because a use copies it and the
>   `ref` still sees a live value.
> - **Borrowing lasts until the local goes out of scope** (agreed). A
>   `ref` has no lifetime. Once a function receives one, it may store it
>   anywhere, so the compiler cannot know when the last one is gone.
>   Assigning the local again does not end it: the `ref` points at the
>   place, which now holds the new value.
> - **Borrowing is flow-sensitive** (agreed). It is part of the
>   ownership pass's state. A local is borrowed from the point a `ref` to
>   it is taken, along every path from there; where paths join, it is
>   borrowed if it is on either one. A loop is walked until that, too,
>   stops changing. So `if c { r = ref a } else { b = a }` is fine: on
>   the path that moves `a`, no `ref` was taken.
> - **No "both hooks or neither" rule yet** (agreed). With the rule
>   above, a type with `@on_drop` and no `@on_copy` is still safe: its
>   values move, and only where no `ref` can see them. Forcing the choice
>   when the type is written would move the error earlier for beginners.
>   That is for later, if the errors here turn out to confuse them
>   (phase 5).
>
> What borrows a local (each marks the local, whatever field the `ref`
> is to):
> - `ref a` and `ref a.f`;
> - a method call whose receiver is `self` (bare `self` is `ref T`), and
>   operator and conversion hooks taking `ref` parameters, which typeck
>   writes as `RefOf` of the place;
> - an f-string hole (`$"{a}"`), which holds the value's address;
> - a by-reference capture (`match a { .Some -> v: ref P … }`).
>
> Reading a place through a `ref` it already is (`p.f`, with `p: ref T`)
> borrows nothing new.

## Phase 1: borrowed places do not move implicitly

- `places.rs`: the state (`State`) becomes a `Flow`: the `Own` of each
  local, plus each borrowed local with the span of its first `ref`.
  `merge_states` unions the borrowed locals; `same_states` compares them,
  so a loop's walk sees a `ref` taken later in its body; leaving a scope
  forgets its locals as it does their `Own`s. `round_bound` counts one
  more rise per local (unborrowed to borrowed).
- `Pass::expr`'s `RefOf` arm marks the place's local borrowed (holes and
  receivers come through it), and a by-reference capture marks its
  scrutinee's.
- A by-value read that would move an owned path (`Pass::expr` with
  `consume` and a type that does not copy) and a capture that would move
  a payload out of one (`Pass::capture`) check the local first. A
  borrowed one is `E079`, at the use, with the `ref` as a secondary
  label, a note on why, and a fix to `@move(a)` (for a capture: capture
  by reference, or match on `@move(a)`). The place is then treated as
  moved, as before, so one mistake is one error.
- `E079` goes through `emit_requirement` like `E063`/`E064`, so a
  failing generic instance folds it into `E042`.
- `docs/errors/E079.md`, `SPEC.md` (ownership, "What can move"), unit
  tests in `lilac-typeck` (a borrow then a move; a borrow on one branch
  only; a borrow later in a loop; `@move` allowed; a copyable type
  allowed; a capture; a receiver), and a `FAILS_` run test. No
  `Standard` or test program moved a value after borrowing it, so none
  needed a `@move`. A hidden local (the compiler's own) is exempt.

## Phase 2: say why a value moved

`E063` ("used after it was moved") points at the move. When that move
was implicit, it says why: the label reads "moved here: `Pair` cannot
be copied", a note names the type that stopped the copy (the moved type,
or the type with an `@on_drop` and no `@on_copy` it holds, however
deep), and help offers a `ref` or an `@on_copy` on that type. This is
most of what a "both hooks" rule would give a beginner, at the place the
problem shows.

- `places.rs`: `Pass::implicit` records each implicit move (a by-value
  use of a type that does not copy, and a moving capture) by span, with
  the moved type; `@move` records none. `Pass::moved_here` labels the
  move in `E063` (`require_full`, `parent_present`) and explains a
  recorded one; `Checker::copy_blocker` finds the type to blame.
- `docs/errors/E063.md` shows the new output (and its good example no
  longer moves after a `ref`, which phase 1 made `E079`); a unit test in
  `lilac-typeck` covers the moved type, a held type and `@move`.

## Phase 3: last-use moves

A copyable local's last by-value use moves it instead of copying, so
the `@on_copy`, and the drop of the original, never run. It applies only
where moving is safe:

- the local is not borrowed (phase 1's state);
- no later read of it is reachable: the pass needs to know, at each
  use, whether the local is read again on any path from there. That is
  a backward liveness walk over the same tree, run before the forward
  one, and a use inside a loop is live while the loop can read the local
  again.

It is a language rule, decided in the ownership pass and the same in
every build mode, not an optimization LLVM may or may not make: hooks
are documented to run only where a copy is needed, and code must not
count them. Only whole locals in this phase; a field's last use
(`b = a.f`) can follow.

Implemented:

- `liveness.rs`: `last_uses` walks each body backward before the
  ownership pass and returns the reads of a whole local (by local and
  span) after which no path reads it again. A whole assignment, a
  declaration or a capture ends a value; `return`/`fail` and the body's
  end read the outs; a `@panic`/`@trap` statement reads nothing after. A
  loop is walked until its head's set stops growing. A key some read has
  the local live after (a span typeck reuses, an earlier round) is never
  a last use, so the answer errs towards copying.
- `places.rs`: `Pass::last_use` turns a consuming read of a copyable
  type into a `HirExprKind::Move` when it is a last use, the local is not
  borrowed, and copying would run a hook (`Checker::needs_copy` or
  `needs_drop`; a plain-data move and copy are the same bytes, so its
  generated code is unchanged). It is not an implicit move for `E063`'s
  explanation, since nothing reads the local after it.
- `SPEC.md` ("A last use moves"), the run test
  `tests/run/last_use_moves.lilac` (one use, two uses, borrowed, a loop,
  one branch, `g = pass(g)`), and `copy_and_override.lilac` now reads `a`
  after the copy so it still copies.

## Phase 4: `Standard`

Remove the `@move`s phase 3 makes unneeded (`Heap`'s `create`, `Box`,
`List`, `Const`, `Optional` and `Result`), and turn
`tests/run/box_moves_copyable.lilac` into a check that the copy is
elided. Revisit the `AGENTS.md` note on `@move` in generic code.

Implemented: every `@move` in `Standard` was a last use, so all are
gone (`Heap`'s five `create`s, `Box.create`/`create_in`, `List.add`/
`insert`, `Const.create`, `Optional` and `Result`'s getters).
`box_moves_copyable.lilac` already checks the elision (no `+`, one
drop) and now says it holds with no `@move`; the `AGENTS.md` note says
when generic code still needs one (a value read again or borrowed).

## Phase 5 (if needed): "both hooks or neither"

If beginners still trip over moves, make a type with `@on_drop` and no
`@on_copy` an error unless it says it cannot be copied (spelling to be
decided), with fixes to add the copy or the marker. Nothing in phases
1-4 changes for it.
