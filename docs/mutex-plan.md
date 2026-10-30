# Mutex plan

A production-quality `Standard.Sync.Mutex[T]`: it owns the data it guards,
waits in the kernel rather than spinning, is one zeroed word plus `T`
(so a global needs no initializer), and is checked in Debug builds. A
`Condvar` comes with it, since a mutex without one cannot express
"wait until".

## Status

Phases 1�6 are done: `Os.wait_on_address`/`_timeout`, `wake_one`/
`wake_all` and `current_thread_id` (`lib/Standard/Os/wait.lilac`),
`@spin_hint`, `Sync.RawMutex`, `Sync.Mutex[T]`/`MutexGuard[T]`,
`Sync.Condvar`, and `GeneralAllocator` on `RawMutex`. Where the build
differs from the design below:

- **The wait layer** is two functions, `wait_on_address(word, expected)`
  and `wait_on_address_timeout(word, expected, nanos)`, instead of one
  with an `Optional` timeout. With no OS a wait returns at once.
- **macOS** uses `__ulock_wait2` (11.0+, a nanosecond timeout) rather
  than `__ulock_wait` (microseconds).
- **`@spin_hint`** is empty on 32-bit Arm too (only x86 and aarch64 get
  an instruction).
- **Condition waits are on the guard**: `guard = guard.wait(ref ready)`
  and `wait_timeout(ref ready, nanos) (guard, timed_out)`. Lilac never
  infers a generic method's arguments (`E044`), so a `Condvar.wait[T]`
  would be spelled `ready.wait[List[Int]](guard)` at every use. The guard
  hands its mutex to `Condvar.wait_raw` (`@discard`, no extra unlock),
  which is also the public wait for a `RawMutex`.
- **A second compiler change**: a global initializer builds a `=>`
  function's body in place (`eval_built`), as it already did a
  constant's, so `G: Mutex[Int] = Mutex[Int].new(0)` is constant. An
  argument with a hook builds in place too, when the body moves it into
  the value (`Mutex[Optional[T]].new(.None)`); one the body copies or
  leaves to drop is not constant.
- **Unlocking a borrowed guard**: once `value` has lent out a `ref`, the
  guard moves only by `@move` (`E079`), so an early unlock or a wait reads
  `@move(guard).unlock()`. This is the moves plan working as intended:
  the `ref` might still be used.
- **Tests** are `tests/run/mutex_counter` (plus a Release twin),
  `mutex_try_and_timeout`, `mutex_guard_exits`, `condvar_queue` (a ring
  buffer rather than a `List`), `mutex_relock_panics`,
  `mutex_drop_locked_panics`, `raw_mutex_foreign_unlock`,
  `global_hooked_constructor`, `FAILS_global_hooked_argument_copied`
  and `_dropped`, and
  `crates/lilac-cli/tests/sync_link.rs` (all six native targets, Debug
  and Release). A global `Mutex` is covered by `mutex_counter`. There are
  no `--libc` twins: `threads_libc` already links `--libc` on Windows,
  where `-lsynchronization` is the only difference
  (`LinkInfo::libc_libs`).
- **Not done**: the state-machine model and `bench/sync` (both optional),
  and phase 7.
- **Since then**: `Sync.RawReentrantMutex` and `ReentrantMutex[T]`
  (section 8), for standard output's lock.

## What "production quality" means here

- **Correct.** Mutual exclusion with acquire/release ordering, no lost
  wakeups, no deadlock in the lock itself. It tolerates spurious wakeups
  from the OS.
- **Fast when uncontended.** Lock and unlock are one atomic each, inlined,
  with no system call and no allocation.
- **Cheap when contended.** A short bounded spin, then a kernel wait on the
  word (futex-style). A waiter never spins with yields indefinitely.
- **Small and constant.** `@bit32` state, zero means unlocked. It is never
  heap-allocated and needs no init or destroy call, so
  `G: Mutex[Int]` works as a global and a `Mutex` may move while unlocked.
- **Safe API.** The data is reachable only through a guard. The guard
  unlocks when it drops, including on `return`, `break`, `fail` and early
  exits.
- **Debug checks.** Debug builds panic when a thread locks a mutex it
  already holds ("deadlock"), and when a locked mutex is dropped.
- **Portable.** It works on Windows, Linux (native and `--libc`) and
  macOS, with no C library in native mode, and degrades sensibly on
  `none`/wasm.

## What exists today

- **Atomics.** Every op, width and ordering (`@atomic_*_acquire_32`,
  `@atomic_compare_swap_*`, `@fence`), lowered to LLVM atomics.
  `Sync.AtomicBool`/`AtomicInt` wrap a few of them.
- **Threads.** `Os.spawn(entry, argument: ref unknown)`, `Os.join` and
  `Os.yield_thread`. A native Linux `join` already waits on a futex.
- **Raw OS declarations.** Nothing uses these yet:
  - `Os.Linux.futex` with the `FUTEX_*` constants, including `_PRIVATE`.
  - `Os.Windows` SRW locks and `SleepConditionVariableSRW` (in
    `kernel32.def`).
  - `Os.MacOs.os_unfair_lock_*` (in `libSystem.tbd`).
  - `gettid` and `GetCurrentThreadId`.
- **The only lock today.** `GeneralAllocator.acquire`/`release` is a CAS
  spin lock that yields under contention, with no kernel wait. It is the
  first customer to migrate.
- **Ownership already gives the guard semantics we need:**
  - A type with `@on_drop` and no `@on_copy` is move-only.
  - Its own hook runs before its fields drop (`tests/run/drop_nested`).
  - Every exit drops its scopes.
  - `@discard(self)` lets a consuming method skip the hook.
  - A call returning `ref T` is a place, so `guard.value() = x` writes
    through.
- **Panics abort.** No unwinding means no poisoning: a panicking thread
  takes the process with it, so no one can observe a half-updated value.
  That is a real simplification over Rust.

## Design

### 1. Waiting on an address (`Standard.Os`, new `wait.lilac`)

The portable primitive every lock is built on:

```lilac
/// Sleeps while `*word == expected`, until woken, `timeout` nanoseconds
/// pass (`.None`: forever), or spuriously. The caller always rechecks.
wait_on_address: function (word: ref @bit32, expected: @bit32, timeout: Optional[@bit64]) nothing
/// Wakes at most one thread waiting on `word`.
wake_one: function (word: ref @bit32) nothing
/// Wakes every thread waiting on `word`.
wake_all: function (word: ref @bit32) nothing
```

Per OS, as one `match @builtin.Target.os` like the rest of `Os`:

| OS | wait | wake | notes |
|----|------|------|-------|
| Linux (native and `--libc`) | `futex(FUTEX_WAIT_PRIVATE, expected, &timespec)` | `FUTEX_WAKE_PRIVATE`, 1 or `INT_MAX` | Already declared. A relative timespec; `EINTR`/`EAGAIN` just return. |
| Windows 8+ | `WaitOnAddress(word, &expected, 4, ms)` | `WakeByAddressSingle` / `All` | These are in the API set `api-ms-win-core-synch-l1-2-0.dll`, not in `kernel32`. They need a new `toolchain/windows/synchronization.def`, both `.lib`s, and an entry in `LinkInfo::system_libs`. Round the timeout up to milliseconds. |
| macOS 11+ | `__ulock_wait(UL_COMPARE_AND_WAIT \| ULF_NO_ERRNO, word, expected, µs)` | `__ulock_wake(…, ULF_WAKE_ALL?)` | Add both to the two `libSystem.tbd`s. `os_sync_wait_on_address` is the public API but needs 14.4, and we target 11.0. See the risks below. |
| `none` / wasm / unknown | `yield_thread()` then return | nothing | Correct because callers recheck. Single-threaded wasm never contends. |

The raw declarations go in `Os.Windows`/`Os.MacOs`, mirroring the
platform exactly (names, argument order, constants), per the two-level
rule. `tests/toolchain.rs` then checks they are in the stubs.

### 2. A spin hint (compiler)

A spin loop without `pause` is rude to hyperthreads and to power use. Add
one leaf intrinsic, `@spin_hint()`, that takes nothing and returns
nothing:

- **Typeck and lowering.** Add it to the `Intrinsic` table with a
  signature, plus the unit test that keeps the table in step.
- **LIR.** It goes through `Intrinsic::leaf_builtin` as a call to a
  backend leaf `.lilac.spin_hint`, like `@ptr_add`. Calls to it count as
  side effects, so DCE keeps them.
- **LLVM codegen.** The leaf lowers to `llvm.x86.sse2.pause` on x86
  (the `pentium4` baseline has SSE2), `llvm.aarch64.hint(1)` (`yield`) on
  aarch64, and `llvm.arm.hint(1)` on 32-bit Arm. Everywhere else it is
  empty.
- **Tests.** Add it to `numeric_link.rs`'s cross-link program (or a
  sibling), so no target leaks a libcall.

This is the only compiler change the plan needs.

### 3. `Sync.RawMutex`: the lock without data

`RawMutex` is a single `state: @bit32` with three values: 0 unlocked, 1
locked, 2 locked with waiters. This is Drepper's futex mutex (also Rust's
and musl's):

```
lock:
    if cas_acquire(state, 0, 1) == 0: return          // fast path, inlined
    lock_contended()

lock_contended:                                      // not inlined
    spin up to SPIN (≈100) times while state == 1:   // only while no waiter
        @spin_hint(); if cas_acquire(state, 0, 1) == 0: return
    loop:
        if swap_acquire(state, 2) == 0: return       // took it, marked contended
        wait_on_address(state, 2, .None)

unlock:
    if swap_release(state, 0) == 2: wake_one(state)  // only pay when someone waits
```

`try_lock` is the fast path alone. `lock_timeout(nanos)` is
`lock_contended` with a deadline from `monotonic_nanos`, passing the
remaining time to each wait. Taking the lock with `swap(2)` keeps "maybe
waiters" sticky, so unlocking never misses a waiter. The cost is at most
one extra wake.

`RawMutex` is public: the allocator and anything building its own
structures (queues, `Once`) needs a lock without a `T`.

### 4. `Sync.Mutex[T]` and `MutexGuard[T]`

```lilac
Mutex[T]: structure {
    private {
        raw: RawMutex
        data: T
    }
    new: function (value: T) Mutex[T] => Mutex[T] { raw = RawMutex.UNLOCKED, data = value }
    lock: function (self) guard: MutexGuard[T]
    try_lock: function (self) guard: MutexGuard[T] else nothing
    lock_timeout: function (self, nanos: @bit64) guard: MutexGuard[T] else nothing
    /// The value with no lock taken: only through a `ref` nothing else holds.
    get_unlocked: function (self) value: ref T
    into_inner: function (self: Mutex[T]) value: T
    @on_drop: function (self) nothing  // Debug: panics if locked; makes Mutex move-only
}

MutexGuard[T]: structure {
    private { mutex: ref Mutex[T] }
    value: function (self) value: ref T
    unlock: function (self: MutexGuard[T]) nothing   // early unlock: @discard + release
    @on_drop: function (self) nothing                // releases
}
```

Using it reads like the rest of Lilac:

```lilac
guard: Sync.MutexGuard[List[Int]] = jobs.lock()
guard.value().add(job)
// unlocks at the closing brace, or at any return/break/fail before it

if counter.try_lock() -> held: Sync.MutexGuard[Int] {
    held.value() += 1
}
```

- **Move-only.** `Mutex` and `MutexGuard` both have `@on_drop` and no
  `@on_copy`, so both are move-only by the existing rule. A guard can't be
  duplicated (no double unlock), and a mutex can't be silently copied into
  a second, unrelated lock.
- **Sharing.** To share a mutex, use a global, a `ref` to one that
  outlives the threads, or `Shared[Mutex[T]]`, which copies the handle.
- **Globals.** Zero is unlocked and `new` is a `=>`, so
  `G: Mutex[Int]` (zeroed) and `G: Mutex[Int] = Mutex[Int].new(5)` are
  both constant globals (`global_constant` already folds hooked
  structure literals).
- **Debug checks** run under `@builtin.Build.mode == .Debug`, a constant
  condition, so Release lowers none of this:
  - The mutex keeps an `owner: @bit32` thread id, from a new
    `Os.current_thread_id()`: `gettid`, `GetCurrentThreadId`, or
    `pthread_threadid_np` on macOS.
  - Locking while `owner` is the current thread panics with "deadlock:
    this thread already holds the mutex".
  - Dropping a locked mutex panics too.
  - The field is present in every mode so layouts don't change between
    modes. That costs 4 bytes; making it Debug-only is an open question.
- **Inlining.** `lock`, `unlock`, `try_lock` and the guard's drop are
  small and inline, with the contended path out of line, so uncontended
  code is one CAS and one swap.

### 5. `Sync.Condvar`

`Condvar` is a futex sequence counter (Rust's design). It works with any
`Mutex[T]` and is one zeroed `@bit32`:

```
wait(guard) -> guard:
    seq = load_relaxed(counter)
    unlock(guard.mutex)              // through the guard; the guard moves in
    wait_on_address(counter, seq, .None)
    relock, return the guard
notify_one: fetch_add_release(counter, 1); wake_one(counter)
notify_all: fetch_add_release(counter, 1); wake_all(counter)
```

The API is `wait: function (self, guard: MutexGuard[T]) guard:
MutexGuard[T]`, which takes the guard by value and hands it back (it is
move-only), plus `wait_timeout(…, nanos) guard: MutexGuard[T], timed_out:
Bool`. Wakeups may be spurious and that is documented: callers loop on
their condition.

A `wait_while(predicate)` helper needs a closure, and Lilac's function
values carry no environment. Until they do, the docs show the loop.

### 6. Migrate `GeneralAllocator`

Replace its `lock: @bit32` with `lock: Sync.RawMutex`, and `acquire`/
`release` with `lock`/`unlock`. Heap already depends on Sync (through
`Shared`), and `RawMutex` allocates nothing, so there is no cycle.
`tests/run/general_allocator_threads` then exercises it under contention.

### 7. Later, separately

- **`RwLock`.** One `@bit32`: a reader count plus writer-waiting and
  reader-waiting bits, with two wait words.
- **`Once` / `Lazy[T]`.** Global initialization.
- **`Barrier` and `Semaphore`.**
- **A typed or scoped `spawn`.** This would let a thread borrow a stack
  `Mutex` without `ref unknown`. It is its own plan, but it is what makes
  sharing a mutex ergonomic.

### 8. `Sync.ReentrantMutex[T]` (done)

`lib/Standard/Sync/reentrant_mutex.lilac`: a lock the holding thread may
take again, for code that can call back into itself while it holds one.
Standard output is the first user: a `@format` hook that prints while
`print_line` holds the lock would otherwise deadlock (Rust's `stdout` is a
`ReentrantLock` for the same reason).

- **`RawReentrantMutex`** is a `RawMutex`, the holder's full
  `current_thread_id` (`owner: @bit64`, `0` when none: the id is never
  zero) and a `depth: Int`. Zero is unlocked. `lock`, `try_lock` and
  `lock_timeout` first check `owner` against the caller's id: a match
  only adds to `depth` (so `try_lock` from the holder succeeds, unlike
  `RawMutex`'s); otherwise they take the `RawMutex` and record the
  owner with depth 1. `unlock` takes one off `depth` and, at zero,
  clears `owner` and unlocks the `RawMutex`.
- **`owner` is atomic (relaxed),** read without the lock. Only the
  holder writes it and clears it before unlocking, so by coherence a
  thread reads its own id there only while it holds the lock. `depth` is
  only touched by the holder, so it is plain.
- **The id costs a system call per lock on Linux** (`gettid`), in every
  mode, since the owner check is the lock itself, not a Debug check. A
  thread-local id would remove it (the same gap as the Debug thread id,
  below).
- **Debug** panics on an unlock from a thread that does not hold it
  (`tests/run/reentrant_mutex_foreign_unlock`). A relock never panics:
  it is the point.
- **`ReentrantMutex[T]`/`ReentrantMutexGuard[T]`** are `Mutex[T]`'s
  shape: `new` is a `=>`, so a global is constant; `try_lock`,
  `lock_timeout`, `get_unlocked`, `into_inner`; a Debug panic when one
  is dropped locked; both move-only. Each guard undoes one lock.
- **Two guards on one thread reach one value,** so `value()` lends the
  same `ref T` twice. Rust gives only `&T` for that reason; Lilac
  doesn't forbid aliased `ref`s, so it is a documented rule: code that
  can be called back while holding a guard leaves the value whole at
  each point it might be.
- **No `wait`/`wait_timeout`.** Waiting on a `Condvar` at depth 2 would
  free a lock an outer guard still counts on.
- **Tests**: `tests/standard/src/Sync/reentrant_mutex` (relocking and
  the last unlock freeing the lock, as other threads see it; four
  threads nesting locks under contention, also for the raw lock;
  `get_unlocked`/`into_inner`) and the run test above. `sync_link.rs`
  links all of `Sync`, so it covers the new file.

## Language gaps (none of them block the plan)

- **A `ref` can outlive its guard.** `r: ref T = guard.value()` stays
  usable after the guard drops. Lilac has no lifetimes, and `Shared.value`
  and `Box` have the same property today, so this is documented and not
  enforced. A later check could extend the scope-long borrow
  (`docs/moves-plan.md`) so a `ref` taken from a guard's `value()` marks
  the guard borrowed and can't be read past the guard's drop. That
  belongs in its own plan.
- **Moving a locked mutex.** `@move` of a mutex while a guard points at it
  leaves the guard dangling. An implicit move is already `E079` once the
  mutex is borrowed by `lock(self)`; only an explicit `@move` gets
  through. This is documented, and it is the same rule as any value
  something points into.
- **No `Send`/`Sync` markers.** Nothing stops a program from sharing a
  non-thread-safe value. This matches how `Shared` is documented today.
  Markers would be a language feature (auto traits over hooks) and are
  out of scope here.
- **Debug thread id.** The check costs one `gettid` system call per Linux
  lock in Debug builds. If that is too slow, cache the id in a
  thread-local, which needs a general thread-local feature (the backend
  has only the `@trace_head`/`@panic_depth` slots). Measure first.

## Tests

Run tests live in `tests/run/` and build natively; each has a `--libc`
twin where threads differ:

- **`mutex_counter`.** 8 threads × 10,000 increments of a plain
  (non-atomic) `Int` behind a `Mutex`, with `yield_thread` inside the
  critical section to force contention. The exact total is printed.
- **`mutex_try_and_timeout`.** `try_lock` fails while held;
  `lock_timeout` times out and later succeeds; `unlock` early.
- **`mutex_guard_exits`.** A guard is released on `return`, `break`,
  `continue`, `fail` and at a match arm's end. Each exit is followed by a
  `try_lock` that must succeed.
- **`mutex_global`.** A zeroed global `Mutex[Int]` and one with a `new`
  initializer, both used from threads.
- **`condvar_queue`.** A producer and consumers over a
  `Mutex[List[Int]]` plus a `Condvar`; every item is received exactly
  once. Also `notify_all` waking N waiters, and `wait_timeout`.
- **`FAILS_`/`expect-stderr` panics (Debug).** Locking twice on one
  thread prints "deadlock …" and exits 101. Dropping a locked mutex
  panics.
- **Release.** A `// build-arg: --release` twin of `mutex_counter`, to
  check that no Debug-only state is read.
- **`general_allocator_threads`.** Unchanged, now on `RawMutex`.
- **Toolchain.** `tests/toolchain.rs` covers the new `.def`/`.tbd`
  symbols, and the cross-link test covers `@spin_hint` on all six native
  targets.
- **Optional state-machine model.** A small Rust test enumerating every
  interleaving of two or three threads over the abstract `RawMutex` and
  `Condvar` steps (`loom`-style, hand-rolled, std-only), checking mutual
  exclusion and no lost wakeup. It is cheap and catches the classic bugs
  before a flaky stress test does.
- **Hand-run benchmark.** `bench/sync`: uncontended lock/unlock and
  2/4/8-thread contention against `pthread_mutex`/SRWLOCK/
  `os_unfair_lock` in C.

## Phases

| # | Work | Size |
|---|------|------|
| 1 | `Os.wait_on_address`/`wake_*`, `Os.current_thread_id`, the Windows `synchronization` stub, macOS `__ulock_*` in the `.tbd`s | small |
| 2 | `@spin_hint` intrinsic (table, leaf, backend, link test) | small |
| 3 | `RawMutex` + run tests + state-machine model | medium |
| 4 | `Mutex[T]`/`MutexGuard[T]` + Debug checks + docs (`///`) | medium |
| 5 | `Condvar` + queue tests | medium |
| 6 | `GeneralAllocator` on `RawMutex`; AGENTS.md section | small |
| 7 | (later) `RwLock`, `Once`/`Lazy`, typed spawn | separate plans |

Phases 1 and 2 are independent of each other. Phase 3 needs both, and
4–6 follow in order.

## Decisions (resolved as recommended)

1. **Is `Mutex` move-only, or does it copy by locking and copying the
   value into a new unlocked mutex?** Move-only is recommended. A copied
   mutex is a different lock, which is never what code sharing a lock
   means; `Shared[Mutex[T]]` is the shared form. This departs from
   "copyable by default", so it is your call.
2. **macOS waiting:**
   - `__ulock_*`: private but stable since 10.12, and used by libc++'s
     `atomic::wait`.
   - Raise the floor to 14.4 for `os_sync_wait_on_address`.
   - Use `os_unfair_lock` for `RawMutex` (public, with priority donation)
     and `__ulock` only for `Condvar`.

   `__ulock` everywhere keeps one algorithm (chosen, as `__ulock_wait2`).
3. **Is the Debug `owner` field in every mode or Debug-only?** Every mode
   is recommended: one layout, and 4 bytes.
4. **Is `RawMutex` public?** Recommended: yes, for the allocator and for
   users building their own structures.

## Risks

- **`__ulock_wait` is not public API.** Apple could change it, though
  libc++ shipping it makes that unlikely. The fallback is option 2.
- **`WaitOnAddress` needs Windows 8.** That is fine for anything current,
  but it means one more import library to keep in the toolchain.
- **Thread tests can be flaky.** Every test must decide its result
  deterministically (exact counts, a handshake), never by timing. Only the
  timeout tests use time, and they assert "at least", never "at most".
