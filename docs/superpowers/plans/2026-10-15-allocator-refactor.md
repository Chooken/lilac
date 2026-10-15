# Allocator Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `Allocator` a thin forwarding handle and each heap allocator a self-validating, panicking backend with no zero-init dependence.

**Architecture:** Backends move first (each validates via the shared helpers and panics on OOM while the tolerant handle still accepts live returns), then the handle thins to pure forwarding, then tests and docs follow.

**Tech Stack:** Lilac (`lib/Standard/Heap/*.lilac`), Rust harness (`crates/lilac-cli/tests/run.rs`), native-link run suites.

**Spec:** `docs/superpowers/specs/2026-10-15-allocator-refactor-design.md`

## Global Constraints

- `inkwell` / `llvm-sys` stay confined to `lilac-codegen-llvm`; workspace `unsafe_code = "forbid"` holds for our own code.
- User-facing refs are always live: no null sentinel crosses any allocator boundary.
- No new `Result`-based OOM recovery; OOM panics with the existing messages.
- Run-test headers keep their directives (`expect-stdout`, `expect-exit`, `expect-stderr`, `build-arg`, `lib:`); `Standard` mounts for every program.
- Gates before each commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (run suites need `LLVM_SYS_181_PREFIX` with `lld`/`clang`; without them `run_programs` skips).

## Review Focus

- Zero-size `allocate(0)` still returns an aligned, never-read address instead of panicking or mapping: pinned in Tasks 1-3.
- `allocate[T](count)` overflow (`element * count > Int.MAX`) still panics via shared `array_size`: pinned in Task 4.
- Non-power-of-two or negative alignment panics in every backend, including through the handle: pinned in Tasks 1-4.
- `GeneralAllocator.init()` panics (not a half-built allocator) when the class-table mapping fails: pinned in Task 3.
- One shared `GeneralAllocator` across four threads stays sound via its lock: pinned in Task 5.

---

### Task 1: Page and C allocators to the new contract

**Files:**
- Modify: `lib/Standard/Heap/page_allocator.lilac`
- Modify: `lib/Standard/Heap/c_allocator.lilac`
- Test: `tests/run/heap_allocators.lilac`, `tests/run/heap_allocators_libc.lilac` (existing suites; they name `PageAllocator.ALLOCATOR` / `CAllocator.ALLOCATOR`)

**Interfaces:**
- Consumes: shared helpers in `lib/Standard/Heap/allocator.lilac` (`check_request`, `empty_allocation`) — same-namespace folder merge, already callable as today.
- Produces: `PageAllocator.allocate_raw(self, size: Int, alignment: Int) allocation: ref unknown` (live, panics); `PageAllocator.reallocate_raw(...)` (live, panics); `PageAllocator.ALLOCATOR: Allocator`; same three for `CAllocator`.

- [ ] **Step 1: Confirm the failing baseline**

Run: `cargo test -p lilac-cli --test run run_programs` (needs LLVM toolchain; without it the suite skips — note that and fall back to `cargo run -p lilac -- check --lib=Standard=lib/Standard tests/run/heap_allocators.lilac` from `tests/run`, expecting missing-`ALLOCATOR` errors)
Expected: FAIL (no `PageAllocator.ALLOCATOR` / `CAllocator.ALLOCATOR`; duplicate validation lives only in the handle)

- [ ] **Step 2: Implement `PageAllocator` — own validation, panic, stateless `ALLOCATOR`**

In `lib/Standard/Heap/page_allocator.lilac`: call `check_request(size, alignment)` in `allocate_raw`/`reallocate_raw`/`free_raw`; return `empty_allocation(alignment)` for `size == 0`; panic `"Out of memory."` when `Os.map_pages` fails instead of returning null; keep `within_page` cap; add a stateless global instance plus `ALLOCATOR: Allocator => Allocator.from[PageAllocator](ref <instance>)`.

- [ ] **Step 3: Implement `CAllocator` the same way**

In `lib/Standard/Heap/c_allocator.lilac`: same validation plus zero-size rule; panic when `calloc`/`realloc` returns null; keep the two-pointer cap and post-growth zeroing; add stateless `CAllocator.ALLOCATOR`.

- [ ] **Step 4: Verify Page/C suites**

Run: `cargo test -p lilac-cli --test run run_programs` (or the single-program `lilac build` form from `run.rs::run_one` for `heap_allocators.lilac` and `heap_allocators_libc.lilac`)
Expected: `heap_allocators*` progress; remaining failures only from Tasks 3-5 items (`GeneralAllocator.ALLOCATOR`, `default_general_allocator`)

- [ ] **Step 5: Commit**

```bash
git add lib/Standard/Heap/page_allocator.lilac lib/Standard/Heap/c_allocator.lilac
git commit -m "refactor heap: self-validating panicking Page and C allocators"
```

### Task 2: Arena allocator to the new contract

**Files:**
- Modify: `lib/Standard/Heap/arena_allocator.lilac`
- Test: `tests/run/arena_allocator.lilac`

**Interfaces:**
- Consumes: Task 1 produces nothing structural (same helper pattern); shared `check_request` / `array_size` / `align_up` / `empty_allocation`.
- Produces: `ArenaAllocator.allocate_raw` / `reallocate_raw` (live, panic; zero-size returns aligned address); `free_raw` explicit no-op.

- [ ] **Step 1: Write the failing check**

Run: `lilac check` on a scratch program calling `(ArenaAllocator.init()).allocate_raw(0, 8)` (expect panic today; want the kept aligned-address rule)
Expected: FAIL (panics on 0)

- [ ] **Step 2: Implement Arena validation via shared helpers**

Replace the hand-rolled negative/zero/alignment checks with `check_request` (same messages), route `size == 0` to `empty_allocation(alignment)`, keep block/custom-block flow and `reset`/`@on_drop` semantics, keep `allocate_block` OOM panic.

- [ ] **Step 3: Verify the arena suite**

Run: `cargo test -p lilac-cli --test run run_programs` (or single-program build of `arena_allocator.lilac`)
Expected: `arena_allocator` passes

- [ ] **Step 4: Commit**

```bash
git add lib/Standard/Heap/arena_allocator.lilac
git commit -m "refactor heap: self-validating panicking ArenaAllocator"
```

### Task 3: GeneralAllocator eager init and new contract

**Files:**
- Modify: `lib/Standard/Heap/general_allocator.lilac`
- Test: `tests/run/general_allocator.lilac`, `tests/run/general_allocator_release.lilac`, `tests/run/general_allocator_double_free.lilac`, `tests/run/general_allocator_foreign_free.lilac`, `tests/run/general_allocator_wrong_size.lilac`, `tests/run/general_allocator_misaligned_free.lilac`, `tests/run/general_allocator_use_after_free.lilac`, `tests/libs/HeapChecks/general.lilac`

**Interfaces:**
- Consumes: shared helpers (same pattern as Tasks 1-2).
- Produces: `GeneralAllocator.init() allocator: GeneralAllocator` (eager: maps `ClassTable` or panics; assigns every field); `allocate_raw` / `reallocate_raw` (live, panic) / `free_raw`; Debug ownership panics unchanged in behavior; no `EMPTY`.

- [ ] **Step 1: Confirm the failing baseline**

Run: single-program build of `general_allocator.lilac`
Expected: FAIL on `GeneralAllocator.ALLOCATOR` (does not exist; Task 5 removes that usage — this task only needs `init()` + `from` paths green)

- [ ] **Step 2: Implement eager `init()` and non-optional state**

Remove `EMPTY`; change `classes` to a live non-optional table reference; `init()` maps the class table (panic `"Out of memory."` on failure) and assigns `used = 0`, `mapped = <table size>`, `lock = 0x0`. Keep `Optional` only for genuine links (`SizeClass.free`/`slab`, `Slab.next`, `LargeHeader.next`/`previous`). Keep `MAX_SMALL`, slab/large logic, lock, and Debug poisoning/`NOT_OURS` checks.

- [ ] **Step 3: Implement raw functions on the live contract**

`allocate_raw` / `reallocate_raw` validate via `check_request`, handle zero sizes via `empty_allocation`, panic on OOM; internals (`allocate_unlocked`, `map_class_table`, `add_slab`, `allocate_large`, `map`) return live refs or panic — `or_null` and every `Optional[ref unknown]`-as-nullable-result disappears.

- [ ] **Step 4: Verify Debug and Release suites plus trace line numbers**

Run: `cargo test -p lilac-cli --test run run_programs` (or single-program builds for each `general_allocator*` file)
Expected: all `general_allocator*` suites pass except the `GeneralAllocator.ALLOCATOR` lines Task 5 owns; update any `expect-stderr: at Standard.Heap.GeneralAllocator.* (.lilac:LINE:COL)` lines to the new line numbers

- [ ] **Step 5: Commit**

```bash
git add lib/Standard/Heap/general_allocator.lilac tests/run/general_allocator_double_free.lilac
git commit -m "refactor heap: eager GeneralAllocator without zero-init"
```

### Task 4: Thin the Allocator handle

**Files:**
- Modify: `lib/Standard/Heap/allocator.lilac`
- Test: `tests/libs/HeapChecks/checks.lilac` (`exercise`), `tests/run/heap_allocators.lilac`

**Interfaces:**
- Consumes: Tasks 1-3 live-return backends.
- Produces: thin `Allocator.allocate_bytes` / `reallocate_bytes` / `free_bytes` (forward only); typed `allocate` / `reallocate` / `deallocate` keep overflow-checked `array_size` math; `create` / `destroy` / `free` keep `@write` / `@drop` glue; `AllocatorTable[T]` documented as live-return; helpers stay for backends.

- [ ] **Step 1: Write the failing assertion**

Add nothing permanent: verify by inspection that `allocator.lilac` still calls `check_request`, special-cases `size == 0`, and null-checks table results
Expected: present (to be removed)

- [ ] **Step 2: Implement forwarding-only handle**

Delete handle-level `check_request` calls, the `size == 0` early returns, and the `@ptr_to_address(...) == 0x0` panic branches; keep `array_size` in typed paths, `@write` in `create`, `@drop` in `destroy`; rewrite `AllocatorTable` docs (no "or null when out of memory", no "the `Allocator` handle checks them").

- [ ] **Step 3: Verify all handle-level suites**

Run: `cargo test -p lilac-cli --test run run_programs`
Expected: every allocator suite that does not name a Task 5 symbol passes

- [ ] **Step 4: Commit**

```bash
git add lib/Standard/Heap/allocator.lilac
git commit -m "refactor heap: thin forwarding Allocator handle"
```

### Task 5: Fix tests to the new contract

**Files:**
- Modify: `tests/run/general_allocator.lilac`, `tests/run/general_allocator_release.lilac`, `tests/run/general_allocator_threads.lilac`
- Modify: `tests/libs/HeapChecks/checks.lilac` only if zero-size expectations need it (no null-based checks remain)
- Test: the files above

**Interfaces:**
- Consumes: Tasks 1-4 (`PageAllocator.ALLOCATOR`, `CAllocator.ALLOCATOR`, eager `init()`, thin handle).
- Produces: no test names `GeneralAllocator.ALLOCATOR` or `default_general_allocator`; thread suite shares one local `GeneralAllocator` via the `Allocator` handle.

- [ ] **Step 1: Replace `GeneralAllocator.ALLOCATOR` with local instances**

In `general_allocator.lilac` / `general_allocator_release.lilac`: replace `exercise(GeneralAllocator.ALLOCATOR, 0x60)` with a second local (`other: GeneralAllocator = GeneralAllocator.init()`, `exercise(Allocator.from[GeneralAllocator](ref other), 0x60)`).

- [ ] **Step 2: Give the threads suite a shared local allocator**

In `general_allocator_threads.lilac`: create `general: GeneralAllocator = GeneralAllocator.init()` in `main`, add an `allocator: Allocator` field to `Work`, set `job.allocator = Allocator.from[GeneralAllocator](ref general)` before each `Os.spawn`, read it in `work()` instead of `GeneralAllocator.ALLOCATOR`, and check `general.get_used() == 0` at the end instead of `default_general_allocator.get_used()`.

- [ ] **Step 3: Verify the thread and general suites**

Run: `cargo test -p lilac-cli --test run run_programs`
Expected: `general_allocator*`, `heap_allocators*`, `arena_allocator` all pass

- [ ] **Step 4: Commit**

```bash
git add tests/run/general_allocator.lilac tests/run/general_allocator_release.lilac tests/run/general_allocator_threads.lilac tests/libs/HeapChecks/checks.lilac
git commit -m "refactor heap: tests use local allocators under new contract"
```

### Task 6: Box, Runtime, docs, and full gates

**Files:**
- Modify: `lib/Standard/box.lilac` (only if the new contract requires it — otherwise verify unchanged), `lib/Standard/Runtime/runtime.lilac` (same)
- Modify: `AGENTS.md` (`Standard.Heap` bullet), `STDLIB.md` (allocator rows)
- Test: `cargo test --workspace`, plus `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings`

**Interfaces:**
- Consumes: Tasks 1-5 final behavior.
- Produces: docs matching the shipped contract; green gates.

- [ ] **Step 1: Verify Box and Runtime need no changes**

Inspect `Box.create_in` / `@on_drop` and `Runtime.@entry` against the thin handle; make only required edits
Expected: behavior unchanged (default arena via `get_default_allocator`, boxes keep their `Allocator`)

- [ ] **Step 2: Rewrite the Heap docs**

Update the `Standard.Heap` bullet (`AGENTS.md`) and allocator rows (`STDLIB.md`): thin handle, live-return panicking backends, eager `init()`, no `EMPTY`, stateless `PageAllocator.ALLOCATOR` / `CAllocator.ALLOCATOR`.

- [ ] **Step 3: Run the full gates**

Run: `cargo fmt --check`
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Run: `cargo test --workspace`
Expected: all green (run suites skip cleanly without the LLVM toolchain)

- [ ] **Step 4: Commit**

```bash
git add lib/Standard/box.lilac lib/Standard/Runtime/runtime.lilac AGENTS.md STDLIB.md
git commit -m "refactor heap: docs and consumers match new contract"
```
