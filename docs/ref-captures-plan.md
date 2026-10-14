# Reference Captures Plan

> Status: implemented, 2026-10-14 (phases 1-7). Global initializers also
> gained literals of hooked types (`global_constant`), which phase 7
> needed for `Optional[T] = .None`.
>
> Goal: code can take a reference to a value that lives inside an enum,
> such as the `Some` of an `Optional`, without moving it out. The
> motivating case is a stateful global that starts empty and is filled
> in at entry:
>
>     default_arena: Optional[ArenaAllocator] = .None
>
>     @entry: function () nothing {
>         default_arena = .Some(ArenaAllocator.init())
>         arena: ref ArenaAllocator = default_arena.get_ref() else { @panic("…") }
>         Heap.default_allocator = .Some(Allocator.from[ArenaAllocator](arena))
>         main()
>     }
>
> Today this cannot be written. `get` moves the payload out (a global is
> never moved out, `E064`), and a capture copies or moves, so nothing
> gives the payload's address. This plan closes that gap in the language,
> then redoes `Standard.Heap` on top of it.
>
> Decisions:
> - **`-> v: ref P` captures by reference** (agreed). It gives a
>   reference to what the payload refers to.
>   - When the payload is a value `P`, `v` is the address of the payload's
>     slot inside the enum. Nothing moves, and the matched value stays
>     full. It works on an enum with its own `@on_drop`, and through a
>     `ref`.
>   - When the payload is already a reference `ref P`, `v` is that
>     reference, copied out. This is what happens today.
>
>   The rule follows from `ref` being a flag, not a wrapper. `ref T` with
>   `T = ref X` is `ref X` (`lilac_ty::substitute_in`), and `ref ref X`
>   does not exist. So in both cases `v` refers to the same thing: the `P`
>   the variant carries.
> - **`v` behaves like any `ref` local** (agreed).
>   - Field and method access through `v` work in place.
>   - `v = value` writes through: the old payload drops, and the new one
>     goes into the slot. The tag is unchanged.
>   - `v = other_ref` rebinds `v`.
>
>   One consequence: `v` cannot reseat the reference an
>   `Optional[ref X]` stores, because that would need a `ref ref X`.
>   Write `opt = .Some(other)` instead. SPEC states this.
> - **A by-reference capture of a value payload needs the enum in
>   memory** (agreed). The scrutinee must be one of:
>   - a place: a local, a parameter, a global, or a field of one;
>   - anything behind a `ref`, including `self` in a method;
>   - an expression of type `ref E`.
>
>   A temporary (`match make() { .Some -> v: ref P … }`) is `E010`: "cannot
>   take a reference into a temporary", with help to bind it to a local
>   first. This is the same requirement `ref x` already makes
>   (`check_place_for_ref`). A payload that is already a reference needs
>   nothing, since it is copied out.
> - **Generic bodies decide per instance** (agreed). In `Optional[T]`,
>   `-> v: ref T` is a slot address when `T` is a value type, and a copy
>   when `T = ref X`. A template cannot tell which, so the temporary error
>   goes through `emit_involving` with the payload type:
>   - The template checks clean.
>   - An instance with a value `T` on a temporary fails as `E042` at the
>     user's site.
>   - `Optional[ref X]` never fails.
>
>   `get_ref` matches on `self`, a `ref`, so it never fails at all.
> - **No new error code.** A temporary scrutinee is `E010`. A mismatch is
>   the existing "mismatched types: capture holds …" `E010`, with a help
>   line when the declared type is `ref` of something other than the
>   payload. A capture on a payload-less variant stays `E022`.
> - **`Optional[T].get_ref: function (self) value: ref T else nothing`**
>   (agreed): a reference to the `Some` value, or failure on `None`. For
>   `T = ref X` it returns the stored `ref X`.
> - **No lifetime checking.** As with any `ref` in Lilac, `v` dangles if
>   the matched value is overwritten or dropped while `v` is still in use
>   (`opt = .None` inside the arm, or a `ref` saved past it). A slot's
>   memory stays valid as long as its enum does, so this is no worse than
>   `ref local` today. SPEC says so; no new check.

## Phase 1: typeck

`crates/lilac-typeck/src/body.rs`, the `MatchPattern::Tag` case of the
arm-pattern check (the "capture holds" site, about line 893):

1. Classify the capture. It is by reference when `declared.is_ref` and
   `!payload.is_ref` and `declared.id == payload.id`. Equal types
   (`payload == declared`) are by value, as today, and that includes a
   `ref` payload captured as the same `ref`. Anything else is the
   mismatch error. When `declared` is `ref Q` and `Q` is not the
   payload's type, add help: "a reference capture of this payload is
   `ref {payload}`".
2. A by-reference capture needs the enum in memory. The scrutinee must
   be `is_place()`, or have `ty.is_ref`. Otherwise emit `E010` through
   `emit_involving(payload, …)` (`Requirement::Ownership`, or the
   closest existing requirement). That way a template holds it as
   dependent and an instance folds it into `E042`. The label goes on
   the scrutinee ("a temporary"), and a second label on the capture.
   This needs the scrutinee's `HirExpr`, which the arm check does not
   take today: pass it in from `check_match` (both the statement
   `match` and the value `match` call this helper).
3. Record the classification on the arm. `HirArm` (`lilac-hir`, about
   line 534) gains `by_ref: bool`, set here and never changed by the
   ownership pass. The value-match arm gets the same field if it is a
   separate struct. Its doc comment says: "the capture is the address
   of the payload slot (`-> v: ref P` on a `P` payload)".
4. The local is declared with `declared` (`ref P`), as now, so locals,
   hovers and completion see a `ref P`.

The editor needs nothing: the capture is an ordinary `ref` local.

## Phase 2: ownership

`crates/lilac-typeck/src/places.rs`, `Pass::capture` (about line 972):

- A by-reference capture moves nothing. Return `false` before any of
  the move logic, so there is no `E064` for `@on_drop` enums or for
  matching through a `ref`, and the scrutinee's path stays full. The
  capture's type, `ref P`, is copyable, so today's `is_copyable` early
  return already does this. Make it explicit (`if arm.by_ref`) anyway,
  so it does not depend on that.
- The capture is a `ref` local, so it has no drop and no flag.
- Unit tests in the places tests:
  - a by-reference capture on a local, then the local is still used
    and dropped once;
  - the same on an enum with its own `@on_drop`;
  - through a `ref` parameter.

## Phase 3: lowering

`crates/lilac-lower/src/control.rs`:

- `bind_capture` takes `by_ref`. For a by-reference capture, build the
  payload's field place as now (`payload_field` and `field_place`: the
  payload sits at the tagged layout's fixed offset), then store its
  address into the capture's place. Do not use `copy_out` or
  `read_into`. Factor the address-of logic out of `expr.rs`'s
  `ref_value`:
  - a `Place::Direct` is an `Addr`;
  - a `Place::Indirect` copies the register.

  Both `ref_value` and `bind_capture` then use the shared helper,
  `address_of(place)`.
- The subject must be the real place, never a copy. `subject` already
  uses `place_of` for a place scrutinee. For a scrutinee of type
  `ref E` that is not a place (a call returning `ref E`), check that
  `subject` gives a `Place::Indirect` through the returned address,
  not a spill of the `E`. If it spills, fix `subject` to go through
  the reference. `detach` never takes a temp for a by-reference arm,
  since `moves` is false.
- The constant-choice path (one arm of a constant scrutinee) goes
  through the same `bind_capture`. Typeck's place requirement already
  excludes a constant `=>` scrutinee.
- The constant evaluator (`constant.rs`, the match arm about line 512)
  treats a by-reference capture as not constant (`None`), as it treats
  `RefOf`.
- No new LIR. The verifier and backend see an `addr` (or a register
  copy) and stores of a pointer.
- Writing through `v` keeps the enum invariant ("every enum write is a
  whole value", AGENTS, Layouts). The tag is unchanged. The write is a
  whole `P`, a `memcpy` of its layout into a slot of exactly that
  layout, and the bytes past `P` in the payload area were zeroed when
  the variant was built. State this in a comment at `bind_capture`.

## Phase 4: library

- `lib/Standard/optional.lilac`:

  ```lilac
  /// A reference to the `Some` value, left in place; fails on `None`.
  /// For an `Optional[ref X]` it is the stored reference.
  get_ref: function (self) value: ref T else nothing {
      match self {
          .Some -> some_value: ref T {
              value = some_value
              return
          }
      }
      fail
  }
  ```

  This is a template that checks clean, and every instance works, since
  `self` is a `ref`.
- Search `lib/Standard` for workarounds this replaces (copying a
  payload out, mutating it, writing it back) and use `-> v: ref P` or
  `get_ref` there. `ArenaAllocator`'s `Optional[ref ArenaBlock]` walks
  stay as they are: their payload is already a reference.

## Phase 5: documents

- `SPEC.md`, "Pattern Matching":
  - add the reference capture after the field-rule paragraph: the
    referent rule, the place requirement, `v = value` versus
    `v = other_ref`, and that an `Optional[ref X]`'s stored reference is
    reseated by assigning the optional;
  - add a short example of the global-filled-at-entry pattern;
  - say that `v` dangles if the enum is overwritten.

  Update the Optional section (or the Standard summary) for `get_ref`.
- `AGENTS.md`: one bullet under Pipeline/ownership naming `HirArm::by_ref`,
  where typeck decides it, the `emit_involving` rule for generic bodies,
  and that lowering stores the payload slot's address (the shared
  `address_of`).
- `docs/errors/E010.md`: add the temporary case to its list, if the
  page lists cases.

## Phase 6: tests

Run tests (`tests/run/`), each printing what it checks:

- `ref_capture.lilac`, by-reference captures of a value payload on:
  - a local, changed in place and read back after the `match`;
  - a global, filled at run time from `.None`;
  - a field of a local object;
  - through a `ref` parameter;
  - `self` in a method of the enum;
  - an enum with its own `@on_drop`: the payload is changed through `v`,
    and the drop runs once, at the end, with the new value.

  It also covers:
  - `v = value` dropping the old payload exactly once (a counted
    payload);
  - `v = other_ref` rebinding without touching the enum.
- `ref_capture_generic.lilac`:
  - `Optional[T].get_ref` for a value `T` (a counted object, an
    `@bit32`) and for `Optional[ref X]`, which returns the stored
    reference, so a write through it reaches `X`;
  - a generic function of the user's own that matches `-> v: ref T` on
    a place, instantiated at both kinds.
- `ref_capture_entry.lilac`: the motivating pattern. A global
  `Optional[Counter]` starts `.None`, is filled in a setup function,
  and is used through `get_ref` from several functions.

Error tests (`FAILS_` run tests, or typeck unit tests where those
exist):

- `FAILS_ref_capture_temporary.lilac`: `match make() { .Some -> v: ref P
  … }` is `E010` with the bind-a-local help.
- `FAILS_ref_capture_generic_temporary.lilac`: a generic function doing
  that on `Optional[T]` checks clean as a template. Called with
  `Optional[Thing]` it is `E042`, with the trace at the call. Called
  with `Optional[ref Thing]` it compiles (a second, passing run test, or
  the same file checking that only the value instance fails).
- `FAILS_ref_capture_mismatch.lilac`: `-> v: ref Q` on a `P` payload,
  with the help line.
- A typeck test that a template using `-> v: ref T` reports nothing.

Lowering: an `emit-ir` snapshot (or an `lilac-lower` unit test) shows
`addr` of the payload field, and no copy, for a by-reference capture.

## Phase 7: Standard.Heap redo

This builds on the user's current layout: `PageAllocator` and
`CAllocator` in their own files, each with `ALLOCATOR_TABLE` and an
`ALLOCATOR` constant.

1. Fix the tree's current errors (`lilac check` today reports 3):
   - `within_c_alignment` is unresolved in `Heap/c_allocator.lilac`.
     Add it to that file's `private` block, with its doc.
   - Move `within_page` from `allocator.lilac` into
     `page_allocator.lilac`'s `private` block, so `allocator.lilac`
     holds only what `Allocator` uses (`check_request`,
     `empty_allocation`, `align_up`, `array_size`). Leave `smaller`
     wherever both files can see it.
   - `tests/run/heap_allocators*.lilac` name `Allocator.PAGES` and
     `Allocator.C_HEAP`. Use `PageAllocator.ALLOCATOR` and
     `CAllocator.ALLOCATOR`.
2. `Allocator` names no allocator (agreed). Remove the conditional
   `default_allocator` initializer from `allocator.lilac` (it names
   `CAllocator` and `PageAllocator`), and its stale docs
   (`Allocator.C_HEAP`, `Allocator.PAGES`). Replace it with:

   ```lilac
   /// The program's allocator, for `Box` and anything not given one.
   /// `None` until the runtime's `@entry` sets it; what one allocator
   /// gives must go back to the same one.
   default_allocator: Optional[Allocator] = .None
   ```

   Add an accessor that panics when it is unset ("no default allocator:
   set `Heap.default_allocator` first"), used by `Box.create` and the
   tests (`memory_write_read.lilac`). `Allocator` is two references and
   copies, so the accessor can use `get`.
3. The runtime chooses the default (proposed: the user decides which).
   `lib/Standard/Runtime` owns the stateful global, filled at entry
   with `get_ref`:

   ```lilac
   private {
       program_arena: Optional[Heap.ArenaAllocator] = .None
   }
   ```

   `@entry` sets `program_arena = .Some(Heap.ArenaAllocator.init())`,
   takes `program_arena.get_ref()`, and sets
   `Heap.default_allocator = .Some(Heap.Allocator.from[Heap.ArenaAllocator](arena))`
   before `main()`. The arena now outlives `main`, unlike today's
   `@entry` local, which is sound only because `@exit` follows. A
   program that wants another default assigns `Heap.default_allocator`
   first thing in `main`.
4. `default_c_allocator` and `default_page_allocator` are public
   top-level globals today. They need no state, so either keep them
   private next to their `ALLOCATOR` constants, or drop them and give
   `from` a zero-size static (proposed: keep them, private).
5. Docs:
   - fix the header comment of `allocator.lilac`;
   - rewrite the `Standard.Heap` bullet in `AGENTS.md`: `Allocator` is a
     handle, allocators bring their own `ALLOCATOR_TABLE` and `ALLOCATOR`,
     `default_allocator` is `Optional` and set by `Runtime`'s `@entry`,
     and `Os` has no heap;
   - check that every `///` still attaches (`standard_docs`), and that
     the LSP test `a_library_file_checks_clean_and_highlights_generic_bodies`
     sees no hints in `Heap/allocator.lilac`.
6. Tests:
   - `heap_allocators.lilac` and `heap_allocators_libc.lilac` exercise
     the default (whatever `@entry` set), `PageAllocator.ALLOCATOR`,
     `CAllocator.ALLOCATOR` (libc only) and an arena through `from`;
   - add one check that `default_allocator` is `Some` in `main`;
   - add a `FAILS_` or `// no-standard` test with `TinyRuntime` showing
     the unset-default panic message.

Phases 1 to 6 land as one commit (the language feature with its tests).
Phase 7 lands as a second. Run the gates before each:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Edge cases

- **`Optional[ref T]`** is a copy of the stored reference (decision
  above). A write through `v` reaches the referent, not the optional.
- **Function-reference payloads** (`ref function (…)`) are references,
  so they are copied. `-> f: ref function (…)` is today's capture.
- **`ref unknown` payloads** are copied, the same.
- **Failure captures** (`if x.get_ref() -> v: ref T`, `loop if`) are
  outs, not payloads. They already hold whatever `get_ref` returns, and
  nothing changes there.
- **Nested enums**: `-> inner: ref Optional[P]`, then `match inner {
  .Some -> v: ref P … }`, is a capture through a `ref` and is allowed.
- **A method call on a temporary** (`make().get_ref()`) is the
  existing `ref self` on a temporary, which dangles after the
  statement. It is not new here. If it is not already an error, record
  it as a separate issue, not part of this plan.
- **Constant scrutinees** (`=>` constants) are values, not places, so a
  by-reference capture of a value payload is `E010`. A `ref` payload
  copies as now.
