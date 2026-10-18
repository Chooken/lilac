# Copies and Call Places Plan

> Status: phases 1 and 2 implemented, 2026-10-19.
>
> Goal: collections copy like values, and an element a method hands
> back by `ref` can be assigned to in place:
>
>     a: List[Int] = List[Int].init()
>     a.add(1)
>     a.get(0) = 5            // writes through the returned `ref Int`
>     b: List[Int] = a        // a copy: `b` has its own elements
>     consume(a)              // fine: `a` was borrowed, so this copies
>
> Today `List` has an `@on_drop` and no `@on_copy`, so it only moves.
> Every method call borrows it for the rest of its scope
> (`docs/moves-plan.md`), so after `a.get(0)` any implicit move of `a`
> is `E079`. Assigning to `a.get(0)` is `E010` ("only variables and
> fields are assignable").
>
> Decisions:
> - **Most types copy** (agreed). A type that owns memory copies it
>   deeply. When sharing or a single owner is wanted, a wrapper type
>   says so (a reference-counted `Arc[T]`, which copies by bumping a
>   count; a move-only handle). Allocators stay move-only: an
>   `Allocator` handle points at its allocator.
> - **Borrows are unchanged** (agreed). A borrow still lasts to the end
>   of the scope, which is safe: a callee may keep a `ref`
>   (`GeneralAllocator.allocator(self)` does). A borrowed copyable local
>   copies at its last use instead of moving (phase 3 of the moves
>   plan). `E079` remains only for move-only types.
> - **`List` holds copyable types only** (agreed). `Box` stays
>   move-only (agreed): it is the wrapper for a single owner, as `Arc`
>   is for sharing. Its `@on_copy` copies the elements, so an instance over a type that
>   cannot be copied fails that body (`E042`, traced to where the
>   instance was named). A move-only value goes in an `Arc`
>   (`List[Arc[GeneralAllocator]]`), so resource APIs (threads, files)
>   should work through `ref`, not by consuming the value. A per-argument
>   hook condition (`if @copies(T) { @on_copy … }`) could widen this
>   later without breaking code that compiles today; see "Later".
> - **A call whose result is a `ref T` is a place** (agreed).
>   Assigning to it writes through the reference. Like every
>   assignment, the value is evaluated first, then the place, so
>   `list.get(i) = list.make()` takes the reference after anything
>   `make` did to the list.

## Phase 1: assigning through a call result

- `lilac-typeck/src/expr.rs`, `check_place`: add an `Expr::Call` arm
  (method calls are calls too). Check the call as a value. A non-opaque
  `ref T` result gives `auto_deref(call)`: a `Deref` place. A by-value
  result is `E010`, labelled "this call returns a value, not a `ref`".
  An opaque `ref` (`ref unknown`, a function reference) is `E010` too:
  there is nothing to write through. The `Member` arm already recurses
  through `check_place(obj)`, so `list.get(i).field = v` follows.
- `body.rs`, `assign_checked`: a call place always writes through.
  When the value is a `ref T`, read through it (`auto_deref`) instead
  of rebinding: a call's result is a temporary, so rebinding it would
  change nothing.
- `body.rs`, `check_compound_assign`: the target is read twice (as the
  left operand and as the place), which is only right while a place
  has no side effects. When the place contains a call, first bind the
  call's `ref` to a hidden local, then use `*hidden` as both operand
  and place, so `list.get(i) += 1` calls `get` once. Fix the comment
  that says "a place has no side effects".
- Nothing new downstream. Lowering's `place_of` already spills a
  `Deref` of a value into an indirect place. The ownership pass's
  `assign` sends a non-path place to `borrowed_place`, which walks the
  call (its arguments move and borrow as usual), and returns
  `HirOld::Full`, so the old value drops or `@on_override` runs.
  Liveness walks a non-local place as an expression. A failing call
  as the target is `E057` already (it is not `handled`).
- Tests:
  - typeck unit tests: plain, field and compound assignment through a
    call; `E010` for a by-value result and for `ref unknown`;
    compound assignment calls once (one `Call` in the HIR);
  - run test `tests/run/assign_through_call.lilac`: a `List[Int]`
    element set with `=` and `+=`; a field of an element; an element
    type with `@on_drop`, to see the old value drop once.
- Docs: `SPEC.md` (assignment targets), `docs/errors/E010.md` (the new
  case), and the `AGENTS.md` note on places.

Implemented as planned, except that `assign_checked` needed no change:
`check_place` already hands back `*call`, so a `ref T` value is read
through by `coerce` as for any other place. `op=`'s hidden local is
`body.rs`'s `bind_call_refs`.

## Phase 2: copyable `List`

No compiler change: an `@on_copy` already makes a type copyable, and
glue already calls it.

- `Collections/List.lilac`: an `@on_copy` that allocates `count`
  slots from the same allocator, copies the elements with
  `Slice.copy_to` (each through `T`'s `@on_copy`), and keeps `.None`
  for an empty list. The doc comment says the list copies its
  elements, that `T` must copy, and to wrap a move-only value in an
  `Arc`.
- `Box` stays move-only and unchanged: it is how a single owner holds a
  move-only value (`box_drops_contents`, `box_into_inner` and
  `standard_io` box one), and `stdlib/main.lilac` and
  `box_moves_copyable` keep checking that it moves.
- Allocators, `ArenaAllocator` and `GeneralAllocator`, stay move-only,
  as do `Os` handles.
- Check every `List[…]` in `Standard`, `tests/` and
  `tests/libs` for a move-only element. Each one now fails, so it
  either becomes copyable or moves to a different design.
- `Arc[T]` is not part of this plan: it needs atomics and its own
  plan. Until it lands, a move-only element has no container.
- Tests:
  - `tests/run/list_copies.lilac`: a copy is independent (change
    one, read the other); copies and drops of a counting element type
    balance; the last use of an unborrowed list moves, with no copy;
    the last use of a borrowed one copies;
  - a typeck or run test that `List[T]` over a move-only `T` is
    `E042`, so the error stays traced to the user's `List[…]`;
  - `tests/run/list.lilac` keeps passing; drop any `@move` it no
    longer needs.
- Docs: `STDLIB.md` and the `AGENTS.md` `Standard.Heap` note (`List`
  copies and needs a copyable `T`; `Box` stays move-only).

Implemented: the `@on_copy` builds an `init_using` list, then allocates
exactly `count` slots and `copy_to`s the elements (capacity is the
count). The only move-only element in the tests was `Leaf` in
`assign_through_call.lilac`, which gained an `@on_copy`. The `E042` run
test is `FAILS_list_of_move_only.lilac`; its trace runs from
`Slice.copy_to[Handle]` through `List.@on_copy[Handle]` to `main`.
`list.lilac` had no `@move` to drop.

## Later, if needed: copy hooks for some arguments

If a container of move-only values without an `Arc` turns out to
matter, a generic type's RAII hook could carry a condition on its
arguments, `if @copies(T) { @on_copy: … }` in the type body, decided
in `register_instance_hooks`. A skipped hook leaves the instance
move-only. "Every argument must copy" would not work as the rule,
since it stops `Arc[T]` from copying, and inferring the condition from
the hook's body would be invisible. Nothing in phases 1-2 needs to
change for it.

## Gates

Each phase runs `cargo fmt --check`, `cargo clippy --workspace
--all-targets -- -D warnings` and `cargo test --workspace` before it
lands.
