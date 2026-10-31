# Views Plan

> Status: phases 1 and 2 implemented, 2026-10-31; phases 3-4 proposed.
>
> Goal: a beginner never holds a `ref` without writing `ref`. Today
> `Slice[T]` and the planned string views hold one inside them, so
> `s: Slice[Int] = list.get_slice()` dangles once the list grows, and
> nothing at the call says a borrow was made. This plan makes every type
> that hides a borrow a *view*: named `…View`, checked on each access, and
> confined to the frame that made it, the rules `@format_args` already
> follows (`E072`).
>
> Decisions:
> - **The line is memory safety, not speed** (agreed). The beginner tier
>   cannot corrupt memory: its worst outcome is a panic with a message.
>   It may lose a constant factor (a bounds check, a reload per access),
>   never a complexity class. The expert tier is visible at the call: an
>   explicit `ref`, `@move`, or a type named `Unsafe…`.
> - **`ref` is opt-in already.** A local's type is always written, so
>   `x: Int = list.get(0)` copies and keeping the reference takes
>   `x: ref Int = list.get(0)`. Methods returning `ref T` stay; that is
>   the expert's spelling.
> - **A type that hides a `ref` is a view, and its name says so**
>   (agreed): `MemoryView[T]`, `StringByteView`, `StringScalarView`,
>   `StringCharacterView`, `StringUtf16View`. A beginner reading the type
>   knows it owns nothing.
> - **Views are the iterators** (agreed). There is no `for` loop and no
>   iterator protocol in the language: a view has `next() T else
>   nothing`, so `loop if view.next() -> x { … }` walks it. Iteration is
>   library code.
> - **A view reads through its container on every access.** `MemoryView`
>   holds `ref Memory[T]` (whose handle `resize` rewrites in place) plus
>   a range, and checks the range against the memory's current count, so
>   a view over a list that grew sees the new memory, and one that shrank
>   panics. It never reads freed memory while its container lives.
> - **A view cannot outlive its container** (phases 1 and 2): it is a
>   `scoped structure`, held only by locals and parameters, returned only
>   from what a `ref` parameter points at.
> - **The raw slice stays, renamed** `UnsafeSlice[T]` (an address and a
>   count). `Heap`'s typed arrays, `Memory` and expert code use it.
> - **`SharedMemory` is not how the default containers stay safe**
>   (agreed). Sharing a buffer costs an atomic per copy and a
>   copy-on-write branch per edit; it is for data that must really be
>   shared or escape (a later `SharedSlice`/`Substring`), decided with
>   numbers.

## Phase 1: scoped structures

A `scoped structure` is a structure that borrows. The language confines it
the way it confines a format, with the same code (`E072`, reworded from
"format" to "scoped value"):

```lilac
MemoryView[T]: scoped structure {
    private {
        memory: ref Memory[T]
        front: Int
        back: Int
    }
}
```

- **Syntax.** `scoped` before `structure` is contextual: the parser reads
  the identifier `scoped` followed by `structure` as the marker, so
  `scoped` stays a valid name. `ItemKind::Object::scoped`, then
  `Def::scoped` in the resolver.
- **Where one may live.** A local, a parameter (by value or `ref`), a
  `ref` local, and a field of another scoped structure. Never a field of
  an ordinary structure, an either payload, an array element, a global or
  a constant. A generic instance that would put one there
  (`Optional[MemoryView[T]]`, `List[StringByteView]`) fails with `E072`
  at the generic's slot, as a format does (`Requirement::ScopedPlace`,
  formerly `FormatPlace`).
- **Outs.** A scoped structure may be an out, so `list.view()` can return
  one. (A format still may not: its hole pointers sit in the frame.)
  Phase 2 limits what an out may borrow.
- **No stores through a `ref`.** `*p = view` and `@write(p, view)` are
  `E072`, as for a format. A field of a scoped value assigned through
  `self` (`self.inner = other`) is allowed: the target is itself scoped.
- `Checker::is_scoped(TypeId)`: `@format_args(W)`, or a structure (plain
  or an instance) whose definition is scoped. `format_place` and
  `format_store` become `scoped_place` and `scoped_store`, keeping the
  format-only rules (outs, temporary holes) where they are.
- The editor renders `scoped structure` in hovers (`render.rs`) and the
  VS Code grammar colors `scoped` before `structure`.
- Tests: typeck unit tests beside `a_format_where_it_cannot_live_is_e072`
  (field, payload, array, global, generic instance, store through `ref`,
  `@write`; a scoped field of a scoped structure, an out and a parameter
  allowed), a `FAILS_` run test, `docs/errors/E072.md`, `SPEC.md`.

## Phase 2: what a scoped value borrows

Phase 1 keeps views out of the heap and globals; this phase keeps them
inside their frame. The ownership pass (`places.rs`) tracks each scoped
value's **sources**: the owned locals of this function whose address it
may hold.

- A `ref x` (or `ref x.f`) of an owned local `x` adds `x`. A call whose
  out is scoped adds the sources of every `ref` argument (the receiver
  included: `list.view()` borrows `list`) and of every scoped argument.
  A `ref` parameter, or a `ref` local, adds nothing: what it points at
  lives in a caller, or is the expert's responsibility.
- **Returning** (assigning an out) or **storing through a `ref`** a value
  with a source is an error: the source dies with this frame.
- **Assigning** a scoped local a value whose source is declared in a
  deeper scope than the local is an error: the source drops first.
  (Locals drop in reverse order, so a view declared after its container
  in one scope is always fine.)
- **`@move(x)`** of a source turns each scoped value from it stale, and a
  later use of a stale one is an error (a forward walk, so a view that is
  never used again is fine; setting it again clears it). Implicit moves
  are already `E079`, since taking the view borrowed `x`.
- **A call that takes `ref v` of a scoped local** may store its other
  arguments in `v` (`pair.set(view)`), so `v` takes their sources (and
  the scope rule applies to them).
- New code `E080` ("a scoped value outlives what it borrows"), through
  `emit_requirement` so a generic instance folds it into `E042`.
- `Flow` carries the sources per scoped local, merged as a union; the
  loop bound grows by the number of locals per scoped local.
- Implemented 2026-10-31 (`places.rs`: `Pass::sources`, `store_sources`,
  `hold`, `call_stores`, `invalidate`, `check_stale`); tests beside the
  phase 1 ones and `tests/run/FAILS_scoped_returns_local.lilac`.

## Phase 3: `UnsafeSlice` and `MemoryView`

- Rename `Slice[T]` to `UnsafeSlice[T]` (`lib/Standard/slice.lilac` →
  `unsafe_slice.lilac`), with every user in `Standard`, the tests and the
  examples. `Memory.get_slice`/`slice`, `Heap`'s typed arrays and
  `Memory.byte_buffer_to_slice` keep returning it.
- `MemoryView[T]` (`lib/Standard/Memory/memory_view.lilac`), scoped:
  `ref Memory[T]`, `front`, `back`. `get_count`, `is_empty`, `get(i) ref
  T`, `view(range)`, `next`/`next_back` (`T else nothing`, copying the
  element), `first`/`last`, `@format`. Each access checks `back` against
  the memory's current count and panics "the memory changed while a view
  read it" when it no longer fits.
- `List.view()` and `List.view(range)` return a `MemoryView[T]` over the
  list's count; `List.get_slice()` is removed from the beginner surface
  (`add_slice` takes a `MemoryView[T]`; the internals use `UnsafeSlice`).
  Readers and writers that take `Slice[Byte]` take a `MemoryView[Byte]`
  or an `@byte_buffer`, decided per call site.
- Tests in `tests/standard/src/Memory` (a view over a list that grows, one
  that shrinks and panics, `next`/`next_back` meeting in the middle).

## Phase 4: string views

`docs/string-plan.md` and `docs/string-implementation.md` take the new
names (`StringByteView`, `StringScalarView`, `StringCharacterView`,
`StringUtf16View`), each a scoped structure over `ref String`, already
checked per access. `as_byte_buffer` returns a `MemoryView[Byte]`-like
scoped view rather than a bare `@byte_buffer`.

## Later

- **`MutexGuard` as a scoped structure.** It holds `ref Mutex[T]`, and a
  function that returns `m.lock()` of its own local mutex leaves it
  dangling with nothing spelled. Phase 2's rules close that; check
  `Standard` for guards kept in fields first.
- **Exclusivity.** `f(ref list, list.get(0))`, where `f` grows `list`
  and then reads the element, needs a law of exclusivity (Swift, Hylo).
  Until then `Standard`'s mutating methods take elements by value.
- **`SharedSlice`/`Substring`** over `SharedMemory`, for views that must
  escape, once something needs one.
- **Arrays.** An `@array` has no `Memory`; an `ArrayView` (or a view that
  can also hold an address) waits until something needs it.
