# Threads Plan

> Status: proposed, 2026-10-27. Every decision below is a proposal
> until agreed. Phase 1 is done (guard pages, the default stack size,
> `spawn_with`, `available_parallelism`); the native Linux thread changes
> are cross-linked but have not yet run on Linux.
>
> Goal: starting a thread, handing it work, and getting the result back
> are each one obvious call, a program that shares data between threads
> unsafely does not compile, and the error says how to fix it. The
> low-level layer (`Os.spawn`, atomics, `RawMutex`) stays for library
> authors.
>
>     using Standard.Io
>     using Standard.Threads
>
>     /// One thread's work: what it needs is fields, `run` does it, and
>     /// `join` hands the job back with its results.
>     SumJob: structure {
>         first: Int
>         last: Int
>         total: Int
>
>         run: function (self) nothing {
>             number: Int = self.first
>             loop {
>                 if number > self.last { break }
>                 self.total += number
>                 number += 1
>             }
>         }
>     }
>
>     main: function () nothing {
>         low: Thread[SumJob] = Thread[SumJob].start(SumJob { first = 1, last = 500, total = 0 })
>         high: Thread[SumJob] = Thread[SumJob].start(SumJob { first = 501, last = 1000, total = 0 })
>         total: Int = low.join().total + high.join().total
>         print_line($"{total}")             // 500500
>     }
>
> And the mistake every beginner makes is a compile error:
>
>     error[E080]: `CountJob` cannot be sent to another thread
>       --> main.lilac:14:23
>        = note: field `counter` is `ref Int`: two threads could change
>                one `Int` at the same time
>        = help: make it a `Sync.AtomicInt`, or share it as
>                `Shared[Sync.Mutex[Int]]`
>
> (The examples show the proposed API. Spellings follow the language as
> it is today: type arguments are explicit, and there are no closures.)

## Where we are

- **`Standard.Os` threads** (`lib/Standard/Os/thread.lilac`):
  `spawn(entry: ref ThreadEntry, argument: ref unknown)`, `join`,
  `yield_thread`. The thread runs an untyped `entry(argument)`, returns
  nothing, and leaks unless it is joined exactly once. `join` returns an
  `Optional[Error]` a caller cannot act on.
- **Waiting and locks** (`docs/mutex-plan.md`, phases 1 to 6 done):
  `Os.wait_on_address`/`wake_*`, `Os.current_thread_id`, `@spin_hint`,
  `Sync.RawMutex`, `Sync.Mutex[T]`/`MutexGuard[T]`, `Sync.Condvar`.
  Debug catches relocking and foreign unlocks.
- **Atomics**: every operation, width and ordering as intrinsics;
  `Sync.AtomicBool` and `Sync.AtomicInt` wrap a few, sequentially
  consistent only.
- **Sharing**: `Shared[T]`/`Weak[T]` (an atomic reference count), so
  `Shared[Sync.Mutex[T]]` is the shared, locked form.
- **Allocation**: `Heap.default_allocator` is a `GeneralAllocator`,
  which is locked, so allocating from any thread is safe.
  `ArenaAllocator` has no lock.
- **Panics** abort the process from any thread (exit 101), with that
  thread's own stack trace (the trace head is thread-local).
- **Tests**: `tests/standard/src/Sync/threads` and the mutex and
  condvar tests, `tests/run/threads_libc`, `tests/sync_link.rs`.

### Problems found while reading the code

These are problems now, before anything new is built.

1. **Native Linux threads have no guard page.** The `clone` mapping is
   laid out as the id word at 0, the start block at 0x10, the TLS block
   from 0x40 to 0x4000, then the stack down from the top
   (`clone_mapping`). A stack overflow runs *down into the TLS block and
   the id word* that `join` waits on, then off the bottom of the
   mapping. That can corrupt memory silently or hang `join`, where it
   should crash at once.
2. **Stack sizes differ by OS.** Native Linux gets just under 1 MiB,
   macOS secondary threads get pthread's 512 KiB, Windows gets the
   executable's default (1 MiB). Deep recursion that works on one OS
   overflows on another, and a function that works on the main thread
   (8 MiB on Linux and macOS) overflows when moved into a thread.
3. **TLS size is fixed** (`clone_tls_size`, about 16 KiB). That is fine
   while the only thread-locals are `@trace_head` and `@panic_depth`,
   but phase 5 must size it from `PT_TLS`.
4. **No thread safety at all.** Any `ref` can go to another thread
   through `ref unknown`, and so can a value holding one. Nothing stops
   two threads writing one `Int`, or two threads using one
   `ArenaAllocator`.
5. **A stale comment**: `Heap/allocator.lilac` says `Standard.Runtime`
   sets an arena as the default allocator. It sets a general allocator.

## What "production quality" and "beginner friendly" mean here

- **No data races in code that doesn't cast.** A program that never
  uses `as` on a `ref` cannot share mutable data between threads
  without a lock or an atomic. The check is at compile time.
- **Errors teach.** Each refusal names the field that caused it and
  points to the type that fixes it (`AtomicInt`, `Mutex`, `Channel`,
  `Shared`).
- **One way to do the common thing.** Start, join, send and receive
  are each one call. No `ref unknown`, no casts, no error handling a
  beginner cannot act on: failing to start a thread panics with a clear
  message, and `try_start` exists for code that can recover.
- **Nothing leaks, nothing dangles.** Dropping a `Thread` joins it, so
  a thread never outlives the data it was given and is never left
  unfreed.
- **Same behaviour on every OS.** Same stack size, guard pages
  everywhere, and the same panic output.
- **Fast enough to not think about.** Starting a thread costs one
  allocation and one system call. Channels and locks take no system
  call when uncontended.
- **Deterministic tests.** Every test gets its result from exact counts
  or a handshake, never timing (the mutex plan's rule).

## Decisions

1. **A thread runs a job structure, not a closure.** `Thread[T].start(job)`
   moves `job` into the thread, which calls `job.run()`, and `join` hands
   the job back. *Recommended.* It needs no language feature, captures
   are explicit fields (a beginner can see exactly what the thread
   owns), results come back with no extra type parameter, and the
   thread-safety check has one type to look at. A closure feature later
   would be sugar for building this structure. The alternative is
   `spawn[A, R](ref f, argument)`, which is two explicit type arguments
   at every call while there is no inference, and a second object for the
   result.
2. **Dropping an unjoined `Thread` joins it** (C++20's `jthread`).
   *Recommended.* Rust detaches instead, which lets a thread outlive
   `main` and leaves its result unread. `detach()` exists for when that
   is wanted.
3. **Thread safety is a compile-time rule** (phase 2), checked
   structurally like copyability, and types declare exceptions with
   hooks. *Recommended.* The alternative is documentation only, which
   is not production quality for a beginner language.
4. **No read-only references in v1.** Every `ref` in Lilac can write,
   so a `ref` to plain data can never safely go to two threads. To share
   data, a program moves it (each thread gets its own chunk and `join`
   returns it), copies it (`List` copies deeply), or locks it
   (`Shared[Mutex[T]]`). *Recommended for now.* Zero-copy shared reads
   need a read-only reference kind, which is its own language plan (see
   "Later").
5. **A panic in any thread still stops the program**, now printing which
   thread panicked (phase 5). *Recommended.* Panics are aborts with no
   unwinding, and a half-dead program is harder for a beginner to debug
   than a stopped one. That is also what Go does with an unrecovered
   panic.
6. **Default stack: 8 MiB reserved on 64-bit targets, 2 MiB on 32-bit,
   with a guard page**, settable per thread. *Recommended.* Matching the
   Linux and macOS main thread means a function behaves the same when it
   moves into a thread. Reserved memory costs only address space until
   it is touched.
7. **Names.** The beginner layer is a new namespace `Standard.Threads`
   (`Thread[T]`, `sleep`, `available_parallelism`, `current`), and the
   tools threads share go in `Standard.Sync` (`Mutex`, `Channel`,
   `Once`, ...). `Os.Thread` stays the raw handle.
8. **Targets with no threads** (`none`, wasm): a reachable
   `Thread[T].start` is a compile-time `@compile_error` traced to the
   call, like `E035`, not a run-time `Unsupported`.

## Phase 1: fix the foundation (`Standard.Os`, no compiler change)

- **Guard pages.** Native Linux `clone_thread` changes its mapping to
  `[guard page][stack, growing down][id word, start block, TLS]`: the
  header moves to the top, and the lowest page is `mprotect`ed
  `PROT_NONE`. An overflow then faults on the guard. pthread threads
  already get a guard. On Windows, `CreateThread` with
  `STACK_SIZE_PARAM_IS_A_RESERVATION` keeps the OS guard.
- **Stack size.** `Os.spawn_with(entry, argument, stack_size)`, where
  `spawn` passes the default (decision 6): the Linux mapping size,
  `pthread_attr_setstacksize` on macOS and `--libc`, and `dwStackSize` on
  Windows. Add any new externs to the `toolchain/` stubs
  (`tests/toolchain.rs` checks).
- **`Os.available_parallelism()`**: `sched_getaffinity` (Linux, counting
  set bits), `GetSystemInfo`'s processor count (Windows, already
  declared), `sysctlbyname("hw.activecpu")` (macOS). Returns at least 1.
- Fix the stale `allocator.lilac` comment.
- **Tests**: a thread that recurses 4 MiB deep succeeds on every OS
  (`tests/standard`); `tests/run/thread_stack_overflow` overflows a
  thread and expects it to crash, not hang. The exact crash code is per
  OS, so the test checks only that the program did not exit 0 and did
  not time out.

## Phase 2: thread safety in the type system (compiler)

Two properties, decided per type like `Checker::is_copyable`:

- **Sendable**: a value of the type can be *moved* to another thread.
- **Shareable**: a `ref` to it can be used by *several* threads at once.

Structural rules, which hold unless the type declares otherwise:

| Type | Sendable | Shareable |
| --- | --- | --- |
| scalar (`@bitN`, floats, `Int`, ...) | yes | no (a `ref` to it writes) |
| `ref X` | when `X` is shareable | when `X` is shareable |
| `ref unknown` | no | no |
| function reference | yes | yes (code does not change) |
| structure, either, array | every field, payload or element | every field, payload or element |

Exceptions are hooks, so `Standard` is not special
(`standard-not-blessed`): a type declares either property with a
member hook whose body is a condition over its type parameters.

```lilac
Mutex[T]: structure {
    @sendable: function () yes: @bit8 => @is_sendable(T)
    @shareable: function () yes: @bit8 => @is_sendable(T)
    ...
}
```

The condition is `T` *sendable*, not shareable: making a type that is
unsafe to share safe to share is the mutex's job, so `Mutex[Int]` and
`Mutex[List[Int]]` are shareable. Sendable is still needed because the
lock guards only what the mutex owns. A `Mutex[ref Int]` would guard
the `ref` but not the `Int`, which other code can still write without
the lock, so it is not shareable (`ref Int` is not sendable). Rust's
rule is the same: `Mutex<T>: Sync` when `T: Send`.

- **Hook shape** (`sigs::hook_shape_ok`, `E022`): no parameters, one
  `@bit8` out, and a `=>` body made only of `@bit8` literals,
  `@is_sendable(X)`, `@is_shareable(X)`, `not`, `and` and `or`. Typeck
  decides it with no constant evaluator. It is a member hook, so a
  generic instance answers for its own arguments.
- **Reflection tests** `@is_sendable(T)` and `@is_shareable(T)`
  (`Intrinsic::is_reflection`, `reflect.rs`) fold to `@bit8` per
  instance, like `@is_copyable`. In a template the answer is
  `Unanswered`. They are memoized by `TypeId`. A cycle (a type that
  reaches itself through `ref`) answers *yes* for the type being
  decided, as Rust's auto traits do, since its other fields still decide.
- **Requirements** `@require_sendable(T)` and `@require_shareable(T)`:
  a statement that is `E080` when the answer is no, emitted through
  `emit_requirement`. A failed instance folds into `E042` traced to the
  user's `start` call, and the template case is `dependent`. The
  diagnostic walks to the first field that blocks it
  (`Checker::send_blocker`, like `copy_blocker`) and suggests by kind: a
  `ref` to a scalar gets `AtomicInt`/`AtomicBool`, a `ref` to anything
  else gets `Shared[Sync.Mutex[…]]`, and `ref unknown` says it is never
  sendable. The `E080` page has a worked example of each.
- **`Standard` annotations**:
  - shareable: `AtomicBool`, `AtomicInt`, `RawMutex`, `Condvar`;
  - `Mutex[T]`: both when `T` is sendable;
  - `Shared[T]`/`Weak[T]`: both when `T` is sendable and shareable;
  - sendable when `T` is (they own their memory): `Box`, `List`, `Map`,
    `Set`, `String`;
  - `Allocator`: both, trusting each allocator to be safe to call from
    any thread. `ArenaAllocator` is not, so it gets a Debug owner-thread
    check (phase 5) rather than a compile-time one, since the handle
    erases the allocator's type;
  - `Slice`, `Format`/`Formatter` stay structurally unsendable (they
    borrow), which is right.
- **What it does not cover**: casts through `ref unknown` (the
  deliberate escape hatch; library code that casts must check itself),
  and globals, which no `ref` reaches. Globals are phase 6.
- **Docs**: SPEC section "Threads and sharing", the AGENTS.md entry, the
  `E080` page, and `///` on each hook.
- **Tests**: `tests/language` programs for each table row and each
  `Standard` annotation (an `@is_sendable` answer printed per type);
  `FAILS_` run tests for a job with `ref Int`, `Shared[List[Int]]`, a
  `ref unknown` field, and a generic job whose argument fails (`E042`
  traced to the user's line); hook shape errors (`E022`).

## Phase 3: `Standard.Threads` (library)

```lilac
Thread[T]: structure {
    /// Starts a thread running `job.run()`. Panics if the OS cannot
    /// start one.
    start: function (job: T) thread: Thread[T]
    /// `start`, with a stack size, or failing with the OS's reason.
    try_start: function (job: T, options: ThreadOptions) thread: Thread[T] else why: Os.Error
    /// Waits for the thread to finish and hands back its job.
    join: function (self: Thread[T]) job: T
    is_finished: function (self) finished: Bool
    /// Lets the thread run on alone; its job is dropped when it ends.
    detach: function (self: Thread[T]) nothing
}

sleep: function (duration: Duration) nothing
available_parallelism: function () count: Int
current_id: function () id: ThreadId
```

- **Inside**: `start` checks `@require_sendable(T)` and that `T` has
  `run: function (self) nothing` (`@has_method`, with a
  `@compile_error` that spells the expected declaration). It allocates
  a `ThreadBlock[T] { job: T, state: AtomicInt, os: Os.Thread }` from
  the default allocator, `@write`s the job in (a last-use move, so no
  `@move` is needed), and spawns `ref run_block[T]`, a generic instance
  as a value (function values phase 4). `join` waits, `@read`s the job
  out and frees the block. `@on_drop` joins and drops the job.
  `Thread[T]` is move-only, and sendable when `T` is.
- **`detach`** sets a state bit so whichever side finishes last frees
  the block: the thread when it ends, or the handle when it drops. That
  needs one atomic swap on each side.
- **`Duration`**: if `Standard` has no duration type by then, `sleep`
  takes nanoseconds as `Os.sleep` does, and a `Duration` comes with the
  time plan.
- **Unsupported targets**: decision 8, through `match
  @builtin.Target.os` with a `@compile_error` arm.
- **Tests** (`tests/standard/src/Threads`): start/join returning
  results; 64 threads at once; drop joins (a job's `@on_drop` counts);
  detach with a handshake; `try_start` with a huge stack fails cleanly;
  the job moves (a move-only job compiles). `sync_link.rs` covers
  `Standard.Threads` on all six native targets, Debug and Release.

## Phase 4: talking between threads (`Standard.Sync`, library)

- **`Channel[T]`**: the beginner's main tool. `Channel[T].new()` gives
  a `Sender[T]` and a `Receiver[T]` (both copyable, holding one
  `Shared` state: many producers, many consumers). `send(value)`,
  `receive() item: T else nothing` (fails once every sender is gone and
  the queue is empty, so `loop if receiver.receive() -> item: T { … }`
  drains it), `try_receive`, `receive_timeout`, and a `bounded(capacity)`
  form where `send` waits when full. The first version is a `List`
  behind a `Mutex` plus two `Condvar`s, which is simple and correct.
  Lock-free comes later, and only if a benchmark asks for it.
- **`WaitGroup`** (add, done, wait) and **`Barrier`**: the "start N
  workers and wait for all of them" pattern without keeping N handles.
- **`Once` and `Lazy[T]`** (the mutex plan's phase 7): one-time
  initialization of a global, the safe answer for globals in phase 6.
- **Atomics for every integer width** (`AtomicInt8` to `AtomicUint64`,
  generated by `scripts/numeric_types.pl`), each with a
  sequentially-consistent default and `_with(ordering)` forms for experts.
- **Tests**: producer/consumer counts (each item received exactly
  once), closing on the last sender's drop, a bounded channel's
  backpressure, `Once` racing from 16 threads, and the mutex plan's
  state-machine model extended to the channel (every interleaving of
  two senders and a receiver: no lost item, no lost wakeup).

## Phase 5: thread-locals and better failures (compiler and library)

- **Thread-local globals**: `name: thread_local T = constant`, lowered
  to LLVM `thread_local` (local-exec on Linux, as `@trace_head` is now).
  The native Linux mapping sizes its TLS block from `PT_TLS` instead of
  `clone_tls_size`. A thread-local is never shared, so phase 6 leaves it
  alone. Its type must have no `@on_drop`, since nothing runs at thread
  exit (`E0xx`).
- **Named panics**: `ThreadOptions.name`, kept in a thread-local; the
  panic handler prints `panic in thread "worker 3": …` (the main thread
  prints as today, so existing `expect-stderr` tests stay put).
- **Debug checks**: `ArenaAllocator` records its owner thread and
  panics when used from another; `RawMutex` caches the Debug thread id
  in a thread-local (the mutex plan's open item).
- **Stack overflow message** (optional): a signal or vectored handler on
  an alternate stack that prints `stack overflow in thread …` before
  exiting. It is per-OS work, so it lands only if phase 1's plain crash
  confuses people.

## Phase 6: globals across threads (compiler)

A global of a non-shareable type that both a thread's job and other
code touch is a race no `ref` shows. Proposed rule: after DCE, from each
live `Thread[T]`'s `run_block[T]` instance, find the globals its call
graph reaches (`CallGraph`, as `E031` does); one also reached from
outside that thread, whose type is not shareable, is `E081` with the
`reached from:` trace, suggesting `Sync.Mutex`, an atomic or
`Sync.Lazy`. Globals written only before the first `start` (such as
`Heap.default_allocator`, which `@entry` sets) would be false
positives, so the rule needs an exemption: either `@entry`'s own writes
or an explicit marker. *This phase needs its own design pass* before it
is built.

## Later (separate plans)

- **Read-only references** (`view T`, or `ref const T`): the one
  language change that makes zero-copy sharing safe (`Shared[List[Int]]`
  read by many threads) and lets scoped threads borrow locals.
- **Scoped threads**: `Threads.scope` that joins every thread it started
  before returning, so jobs may hold borrows. It only pays off with
  read-only references.
- **A thread pool and `parallel_for`** over chunks of a `List`, moving
  each chunk to a worker and back.
- **`RwLock`**, closures (as sugar over job structures).
- **Async I/O**: see below.
- **A Debug race detector** (ThreadSanitizer needs compiler-rt and a C
  runtime, so it would be `--libc` only).

## Async I/O (later, its own plan)

Not now. Blocking I/O on real threads covers tools, games, compilers and
servers with hundreds or a few thousand connections. Async pays off
around tens of thousands of mostly idle connections. It also sits on top
of everything above: `Standard.Io`, threads, thread-locals (phase 5) and
the sendable rule (tasks hand values between threads too).

**Direction: stackful tasks, not `async`/`await`.** A task is a
lightweight thread with its own stack, scheduled in user space (Go,
Java's virtual threads, Zig's new `Io`), so code that reads like
blocking code (`read_line()`) suspends only its task.

| | `async`/`await` (Rust, C#, JS) | Stackful tasks |
| --- | --- | --- |
| For beginners | Two kinds of function ("coloring"); an `async` call from plain code needs a runtime | No new kind of function |
| Compiler work | Large: every `async` body becomes a state machine | Small: a context switch (assembly per architecture) and stack allocation |
| Fit with Lilac | Poor: aggregates live in memory and `ref`s point at locals, so a suspended frame points into itself (why Rust needed `Pin`) | Good: a task's stack never moves, so its `ref`s stay valid; reuses phase 1's guard pages and stack reservations |
| Memory per task | Small (only state live across an `await`) | A stack each, mostly untouched virtual memory on 64-bit; tight on 32-bit |

**What has to hold until then**: all blocking goes through `Standard.Os`
(`read`, `write`, `wait_on_address`, `sleep`, ...), and nothing else in
`Standard` makes a system call around it. Those are the points where a
scheduler can step in later: when a task would block, it registers the
handle with the OS's event system (epoll or io_uring on Linux, IOCP on
Windows, kqueue on macOS) and switches to another task, and user code
does not change. `docs/io-plan.md` keeps the same rule.

**Hard parts the plan must answer:**

- **C calls.** A C function that blocks, or needs a large stack, stalls
  its scheduler thread (why Rust removed its green threads in 2014).
  Options: run `extern` calls on the thread's own stack, or mark the
  calls that block.
- **Thread-locals.** `@trace_head`, `@panic_depth` and phase 5's
  thread-locals become per task. A task that moves between OS threads
  can meet a thread-local address LLVM read before the switch, so tasks
  stay on one thread at first.
- **Locks.** `Mutex`, `Condvar` and `Channel` block the whole OS thread
  in `Os.wait_on_address`; inside a task they must park only the task.
  Routing them through the same `Os` wait keeps that a scheduler change.
- **Cancellation and timeouts**, designed in from the start.

## Order and size

| Phase | Depends on | Size | Notes |
| --- | --- | --- | --- |
| 1. Foundation | — | small | Fixes real bugs today; land first |
| 2. Sendable/shareable | — | large | The compiler work; before phase 3 goes public, so adding it breaks nothing |
| 3. `Standard.Threads` | 1, 2 | medium | The beginner API |
| 4. Channels, `Once`, atomics | 2 (3 for tests) | medium | |
| 5. Thread-locals, named panics | 3 | medium | |
| 6. Globals across threads | 2, 5 | medium | Needs its own design pass |

Phases 1 and 2 are independent and can run in parallel. Each phase lands
alone with the gates, its tests, SPEC/AGENTS.md/STDLIB.md updates and
error pages. STDLIB.md gets a "Threads" chapter written as a tutorial:
start and join, then a channel, then a `Mutex`, then the `E080` errors
and how each is fixed.

## Risks

- **The sendable rule may refuse correct code** that hides a `ref`
  inside a type the author knows is safe. The answer is the hook, and
  the `E080` page has to show how to write one, including that the
  author takes responsibility.
- **Job structures may feel heavy** for one-line work. If they do, that
  is the case for closures, not for weakening the check.
- **Thread tests can be flaky.** Exact counts and handshakes only, and
  time-based tests assert "at least", never "at most".
- **Bigger default stacks** use more address space on 32-bit targets,
  which is why decision 6 gives those 2 MiB.
