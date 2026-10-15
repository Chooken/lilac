# Allocator refactor design

Date: 2026-10-15. Scope: `lib/Standard/Heap` allocators (`allocator.lilac`,
`general_allocator.lilac`, `arena_allocator.lilac`, `page_allocator.lilac`,
`c_allocator.lilac`), plus `Box`, `Runtime` entry, run tests / `HeapChecks`,
and the `Standard.Heap` bullets in `AGENTS.md` / `STDLIB.md`. Approach A
approved 2026-10-15.

## Intent

The user wants `Allocator` to be a thin interface that forwards to the
allocator behind it: the generic `create`/`destroy` glue (move a value in
with `@write`, drop with `@drop`) stays, but all bounds checking,
zero-size handling, and out-of-memory policy move into each concrete
allocator so every allocator is directly usable without the handle.
`GeneralAllocator` must stop depending on zero-initialized state being
valid (no `EMPTY`-as-valid-zero, no lazy `None`-means-unmapped fields that
only work because a zeroed `Optional` reads as `Some`); objects are
assigned via `init()` functions. Null pointers must never appear in
user-facing code as "out of memory": every allocator guarantees live
memory by panicking internally. Out-of-memory recovery is out of scope;
anyone who needs it can write their own allocator.

Agreed answers: panic inside allocators (no null, no `Result` in the
user API); shared private validation helpers in `allocator.lilac` called
by backends, never enforced by the handle; eager `GeneralAllocator.init()`
with `Optional` kept only for genuine links; keep the zero-size
aligned-address rule in each backend; full sweep including `Box`,
`Runtime`, tests, and docs.

## Architecture

`Allocator` remains the type-erased handle (target address + erased
function table) and `AllocatorTable[T]` remains the per-type vtable, but
the contract changes on both sides:

- Table functions (`allocate`, `reallocate`, `free`) take
  `(target, size, alignment, ...)` and return a live `ref unknown`. They
  never return null. Any failure (out of memory, unmapped pages, failed
  `calloc`/`realloc`) panics inside the backend.
- The handle (`allocate_bytes`, `reallocate_bytes`, `free_bytes`, and the
  typed `allocate` / `reallocate` / `deallocate` / `create` / `destroy` /
  `free`) forwards straight to the table. It performs no `check_request`,
  no zero-size shortcut, and no null-to-panic conversion. Typed helpers
  keep only size math (`@size_of`, `@align_of`, overflow-checked
  `array_size`) plus `@write` / `@drop` / `@read` glue.
- Shared private helpers (`check_request`, `array_size`, `align_up`,
  `smaller`, `empty_allocation`) stay in `allocator.lilac` as
  same-namespace (folder-merged `Standard.Heap`) helpers. Backends call
  them directly; the handle does not enforce them. This keeps one
  implementation of alignment / overflow / zero-size math without making
  the interface responsible for policy.
- `GeneralAllocator` drops `EMPTY` and all zero-init dependence. `init()`
  maps the `ClassTable` eagerly and assigns every field (`classes` becomes
  a live non-optional table reference, `used` / `mapped` / `lock`
  assigned). `Optional` remains only where it models a real link: free
  lists, slab chains, the large-allocation list. The same rule applies to
  arena block chains (`next`, custom blocks); those `Optional`s stay
  because they are links, not initialization state.
- Refs returned to user code are always live. `or_null` and every
  null-sentinel path disappear.

## Components

- `Heap/allocator.lilac`: thin handle + live-return `AllocatorTable`
  docs. Keep `from` / `over` / `create` / `destroy` / `free` /
  `allocate` / `deallocate` / `reallocate` shapes; remove handle-level
  `check_request` calls, the `size == 0` early returns, and the
  `@ptr_to_address(...) == 0x0` panic branches. Keep the private helper
  block for backend use. Update doc comments (no "or null when out of
  memory", no "the `Allocator` handle checks them").
- `Heap/general_allocator.lilac`: remove `EMPTY`, make `init()` eager
  (map class table or panic, assign `used = 0`, `mapped = <table size>`,
  `lock = 0x0`), change `classes` to a non-optional table reference.
  `allocate_raw` / `reallocate_raw` / `free_raw` validate via shared
  helpers, handle `size == 0` locally via `empty_allocation`, and panic
  on OOM instead of returning `Optional` / `or_null`. Internal
  `allocate_unlocked` / `reallocate_unlocked` / `map_class_table` /
  `add_slab` / `allocate_large` / `map` return live refs or panic. Keep
  Debug-only ownership checks (double free, wrong size/alignment,
  use-after-free poisoning, `NOT_OURS`) unchanged in behavior.
- `Heap/arena_allocator.lilac`: keep bump/block structure and `reset`
  semantics. Replace the local negative/zero/alignment checks with the
  shared helpers (same messages), keep the zero-size rule consistent with
  the other backends (currently it panics on 0; align it to the kept
  "aligned address, no memory" rule), panic on block-map failure inside
  `allocate_block` / `add_new_custom_block` (already does), and make
  `free_raw` an explicit no-op with named unused params.
- `Heap/page_allocator.lilac`: validate alignment via shared helpers plus
  the existing page-cap panic; panic (not null) when `Os.map_pages`
  fails; fix `reallocate_raw` to panic on failed re-map instead of
  returning null. Add the missing stateless `ALLOCATOR` handle the run
  tests already name (`heap_allocators.lilac`, `heap_allocators_libc.lilac`
  use `PageAllocator.ALLOCATOR`).
- `Heap/c_allocator.lilac`: same treatment (shared alignment check + page
  cap, panic on `calloc`/`realloc` null, zero new bytes after growth).
  Add the missing `CAllocator.ALLOCATOR` handle.
- `Heap/general_allocator.lilac` typed conveniences + `Box` (`box.lilac`)
  + `Runtime/runtime.lilac` `@entry`: no behavior change expected, but
  they are in scope for verification because they sit on the new panic
  contract (`default_allocator` setup, `Box.create_in` forwarding).
- Tests + lib: `tests/run/heap_allocators.lilac`,
  `heap_allocators_libc.lilac`, `arena_allocator.lilac`, all
  `general_allocator_*`, and `tests/libs/HeapChecks/{checks,general}.lilac`.
  Update expectations that assumed null returns or `EMPTY`/zero-init
  construction; add direct-backend-without-handle coverage.
- Docs: rewrite the `Standard.Heap` bullet in `AGENTS.md` and the
  allocator rows in `STDLIB.md` (no null contract, no `EMPTY`, eager init,
  panic-on-OOM, stateless `ALLOCATOR` handles).

Non-goals: no `Result`-based OOM recovery, no removal of the
`Allocator`/`AllocatorTable` indirection (approach C rejected), no change
to slab size classes, block sizes, thread-safety, or Debug poisoning
policy beyond what eager init requires.

## Data flow and error handling

`typed convenience -> handle forward -> backend validate -> OS/libc ->
live ref or panic`. Concretely, `allocate[T](count)` computes
`array_size(@size_of(T), count)` and forwards `(bytes, @align_of(T))`;
the backend validates (negative size, non-power-of-two alignment,
per-backend caps), returns `empty_allocation(alignment)` for `size == 0`,
otherwise maps/allocates, zeroes new bytes, and returns the live ref. Any
failure panics with the existing messages (`"Out of memory."`,
`"Can't allocate a negative size."`,
`"The alignment must be a power of two."`,
`"Can't allocate a negative count."`,
`"PageAllocator aligns to at most a page."`,
`"CAllocator aligns to at most two pointers."`) plus `GeneralAllocator`'s
Debug ownership panics. `free` / `free_bytes` validate and forward; zero
sizes return without touching the backend. No null refs cross into user
code at any layer.

## Testing

The existing run suites carry the refactor: `heap_allocators`,
`heap_allocators_libc`, `arena_allocator`, every `general_allocator_*`
(including Debug `double_free`, `foreign_free`, `wrong_size`,
`misaligned_free`, `use_after_free`, `threads`, `release`), plus `Box`
and default-allocator coverage via `HeapChecks.exercise` /
`general_checks`. Success means those suites pass unmodified in intent
(with only expectation updates for the new contract), new coverage shows
each backend usable without the handle, and the pre-commit gates pass:
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace`.
