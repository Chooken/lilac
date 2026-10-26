# Ownership Plan

> Status: Phases 0 to 4 done. Source: the arena allocator review and the
> ownership design discussion of 2026-10-11.
> Goal: `Rc`, `Arc`, `Box` and custom allocators can be written in
> `Standard` and used like plain values. No value ever has two owners or
> none, and the compiler never invents a value.
>
> Decisions made with the owner:
> - **One owner per value.** A `ref` never owns and never drops.
> - **Moves end the source's life.** The compiler tracks which places it
>   owns are full or empty and drops only full ones. There are no "empty"
>   states and no zero tricks: a handle with id 0 is an ordinary value.
> - **Moves are implicit**: reading a non-copyable place moves it
>   (`b = a`). `@move(a)` moves a copyable value without its copy hook
>   (hand off an `Rc` without a bump). It is a builtin, not a keyword:
>   like `@read` and `@discard` it is a rare, low-level operation that
>   skips a hook, and `ref` is a keyword only because it is also a type.
> - **Fields and `match` payloads move by one rule**: out of an owned
>   value whose type has no `@on_drop` of its own, never through a `ref`.
>   Field moves arrive with the ownership checker.
> - **Copying is decided per type:** its own `@on_copy` copies; otherwise
>   its own `@on_drop` makes it move; otherwise it copies field by field
>   when every field copies, and moves when any field moves.
> - **The compiler never invents a value.** Builtins and `ref` have no
>   default (a null reference only exists inside `Optional`); `default()`
>   is an ordinary function a type opts into. Every out slot is set on
>   every path that returns.
> - **Functions can fail:** `value: T else error: E`, left by `fail`, and
>   handled by a guard, an `if` capture or a `loop if`. Captures bind by
>   position, under any names. A failure must be handled where it happens:
>   no `try` for now.
> - **`Os` keeps returning `Result`**; `Result.get()` is one call away.
> - **`@discard(x)`** works only inside functions declared in the body of
>   `x`'s type.
> - **Bare `self` is `self: ref T`**; a consuming method writes `self: T`.
> - **`@init` is removed.** `name: Type.function(args)` is the declaration
>   shorthand; constructors get distinct names (plain functions do not
>   overload).
> - **Allocators are runtime values** (a context pointer plus functions,
>   as in Zig). `Standard` sets up a general-purpose one in its `@entry`.
> - **Storage stays zero-filled** (deterministic padding for bitwise `==`),
>   but zeroed storage is never a value: an unset place is empty.

## Problems this fixes

Each was reproduced with a small program on 2026-10-11:

- Assigning an out slot drops its zero value first: `make_top()` printed
  `T\0\0\0` before its real value. The same drop makes
  `ArenaAllocator.init` panic in every Debug build (`unmap_pages(null)`).
- A type with `@on_drop` but no `@on_copy` copies bitwise and both copies
  drop: two copies of one arena handed out the same address.
- `Box.create` copies `data` into the heap, then drops the parameter. The
  box never drops its contents today, so this is an early drop; once it
  does, it is a double drop.
- A zeroed `Box` drops as `Some(null)` (a zeroed `Optional` is `Some`).
- There is no way to end a value's life manually (an allocator's `free`)
  or to place a value into fresh memory without dropping what the
  compiler thinks is there.
- Non-copyable types (`@on_copy` that is a `@compile_error`) cannot be
  passed, returned through `Result`, or bound by `match`, because each of
  those copies.

## The model

### Places are full or empty

A place is a local, a parameter, an out slot, a field, or a slot in
memory. It is **full** (holds a live value) or **empty**.

- Reading a full place copies it (the place stays full) or moves it (the
  place becomes empty), by the copy rules below.
- Writing a place drops its old value first if it was full; an empty one
  is just filled. The first assignment of an out slot drops nothing.
- At scope end, full places are dropped and empty ones are skipped. A
  place that may be either (moved on one branch only) gets a hidden flag;
  drops test it.
- Reading an empty or maybe-empty place is an error that names the move.
- Every out slot is full on every path that returns. Paths ending in
  `@panic`/`@trap` are exempt (no drops run).
- The compiler tracks only places it owns. Memory behind a `ref` is
  assumed full: that is what `ref T` means. Raw memory is handled with the
  builtins below.

Storage is still zero-filled, but zeroed storage is never a value: an
unset place is empty, not "zero".

### Copy rules (first match wins)

| The type has | Reading a place |
|---|---|
| its own `@on_copy` | copies through the hook |
| its own `@on_drop` (no `@on_copy`) | moves |
| neither | copies field by field if every field copies, else moves |

A struct holding a `Box` moves; one holding an `Rc` copies, and each copy
bumps the count. Moves are implicit; `move a` moves a copyable value
instead of copying it, and `a` is empty afterwards.

### Parameters, `self`, field moves and `match`

- `x: ref T` borrows: the caller keeps ownership. `x: T` takes ownership:
  the caller's variable moves (or is copied, for copyable types) and the
  callee drops it unless it moves it on. Both pass an address under the
  place ABI; only ownership differs.
- Bare `self` is `self: ref T`. A consuming method writes `self: T`, so
  `box.into_inner()` moves `box` at the call site.
- A field moves out of a structure the function owns (a local or by-value
  parameter) when the structure's type has no `@on_drop` of its own. The
  structure can no longer be used as a whole; its other fields can, and the
  ones still full drop at scope end. Moving out through a `ref` is an
  error.
- A `match` payload binding follows the same rule: matching an owned
  value moves the bound payload out (an error if the either has its own
  `@on_drop`), and matching through a `ref` cannot move a non-copyable
  payload. Matching a temporary (`match Arena.create(size) { … }`) moves
  out of the temporary.

### Functions that can fail

```lilac
get: function (self: Optional[T]) value: T else nothing
map: function (size: @bit_native) memory: ref unknown, length: @bit_native else error: Error
```

- Success outs (before `else`) are set on every `return` and at the end of
  the body; failure outs (after `else`) are set before every `fail`.
- A call that can fail must be handled. The three forms bind outs left to
  right, under any names:

```lilac
// Guard: the outs land in the current scope; the else block must leave
memory: ref unknown, length: @bit_native = map(size) else -> why: Error {
    return
}
node: ref Node = parent.get() else { return }   // failure outs unused

// If: success outs in the then-block, failure outs in the else-block
if map(size) -> memory: ref unknown {
    ...
} else -> why: Error {
    ...
}

// Loop: repeat until the call fails
loop if lines.next() -> line: Text {
    ...
}
```

- A guard's `else` block ends in `return`, `break`, `continue`, `fail`,
  `@panic` or `@trap`.
- `Optional` and `Result` provide `get` (written with `match`). `Os`
  keeps `Result`, so a caller chooses `os_call(...).get() else ...` to
  handle a failure at once.

### Raw memory (for buffer and allocator authors)

| Builtin | Moves | The `T` at the address |
|---|---|---|
| `@write(ref T, value)` | `value` in | not touched: the slot is empty |
| `@read(ref T) T` | the `T` out | left empty, nothing dropped |
| `@drop(ref T)` | nothing | dropped, left empty |
| `@needs_drop(T)` | constant: whether dropping a `T` does anything | |

Assignment through a `ref` (`p = value`) is for a full slot (it drops the
old value); `@write` is for an empty one. Types like `Box` and `Rc` use
the allocator's `create(value)`/`destroy(ref)`, not the builtins.

### `@discard`

`@discard(x)`: `x` is an owned place of type `T`, and the call is inside a
function declared in `T`'s body. `T`'s own `@on_drop` does not run; `x`'s
fields become ordinary places (move out what you need, the rest drop at
scope end). Anywhere else it is an error, so for every other caller
`@on_drop` is a guarantee.

```lilac
into_inner: function (self: Box[T]) value: T {
    @discard(self)
    value = @read(self.pointer)
    Heap.free(self.pointer)
}
```

### Declaration shorthand

`name: Type.function(args)` means `name: Type = Type.function(args)`.
`function` is a static function of the written type whose success result
is exactly one `Type`; it may fail
(`arena: Arena.create(size) else -> why: Os.Error { return }`). In a dotted
path the segment before `(` is the function and the rest is the type.
`name: Type { fields }` stays. `Type(args)` and `@init` go.

## Ground rules

- Every phase ends with the `AGENTS.md` gates green and is committed.
- Every bug fix lands with a test that reproduces it first. Phase 0 adds
  the target behaviour as `// pending:` run tests; the phase that delivers
  it removes the `pending` line.
- These phases change generated code. Each `tests/ui` snapshot change is
  read and explained in the commit, never re-blessed blind.
- `AGENTS.md`, `SPEC.md`, `IR_SPEC.md` and `STDLIB.md` are updated in the
  phase that changes what they describe.
- Each new diagnostic gets its own code and `docs/errors/<code>.md` page.

Order: 0 → 1 → 2 → 3 → 4 → 5. Phase 1 is independent and small. Phase 2
must precede 3: the out-slot rule needs failable `get` to replace the
`(ok, value)` unwraps. Phase 4 needs 3 (`@read`, `@write` and `@discard`
use the full/empty tracking), and 5 needs 4.

Prerequisite: the owner's in-progress `Standard` changes land first
(`map_pages` returning `Result`, the arena allocator), with
`tests/run/allocator/free_list.lilac` updated for `map_pages`.

---

## Phase 0: Pin today's behaviour and the targets

No compiler change.

- **0.1 Drop recursion.** `tests/run/drop_nested.lilac`: a hook runs
  before its fields, fields drop in declaration order at every depth, an
  structure with no hook drops its fields, generics (`Holder[Holder[T]]`),
  eithers drop only the live payload, `Optional` of a hooked type. Hooks
  ignore a zero `c` so the output does not depend on zero drops.
- **0.2 Pending targets.** `// pending:` run tests for: an out slot's
  first assignment drops nothing; copying an arena-like type is a move;
  `Box[Leaf]` drops its `Leaf` exactly once; a value moved on one branch
  drops once.

---

## Phase 1: Replace `@init` with the static-function shorthand

- **1.1 Parser** (`crates/lilac-parse/src/stmt.rs`, `types.rs`): parse
  `name: Path.function(args)` (generic arguments allowed on the type and
  the function); the segment before `(` is the function. Remove
  `name: Type(args)`.
- **1.2 Typeck** (`crates/lilac-typeck/src/calls.rs`): the shorthand needs
  a static function of the written type returning exactly one value of
  that type; anything else is an error (a namespace before the function
  included). Remove `init_call`, `static_init_type`, the `Expr::TypeApp`
  call path and the `@init` shape check (`sigs.rs`); `Type(args)` gets an
  error with the hint "call a static function: `Type.create(args)`". An
  `@init` hook in a type body is an error pointing at the new form.
- **1.3 Language server**: completion after the `.` of a declaration's
  type lists its static functions.
- **1.4 Tests and docs**: rewrite `tests/run/declaration_shorthand.lilac`;
  `FAILS_` tests for a wrong return type and a namespace path. `SPEC.md`
  §5 (Variable Declaration) and §8 (the RAII table loses `@init`).

---

## Phase 2: Functions that can fail

Runs on today's ownership rules; Phase 3 adds the set-on-every-exit
checks.

- **2.1 Syntax** (`lilac-lex`, `lilac-ast`, `lilac-parse/src/items.rs`,
  `stmt.rs`): the `fail` keyword and statement; `outs else outs` in
  signatures (`else nothing` for no failure outs); capture lists
  `-> a: A, b: B` and `else -> e: E` on `if`; the guard declaration
  `a: A, b: B = f() else [-> e: E] { … }`.
- **2.2 Typeck** (`crates/lilac-typeck/src/body.rs`):
  - `fail` only in a function that can fail; `return` and `fail` pick the
    side.
  - A call that can fail is only allowed as the condition of a capture
    `if`, a guard, or a `loop if`; anywhere else is an error.
  - Captures bind left to right, as `check_multi_let` does; outs past the
    bindings drop. The first-`Bool` capture convention and capture by
    name (`check_if_capture`) go.
  - A guard's `else` block must leave. Reuse the end-of-block knowledge
    behind `HirProgram::unreachable` (`check_stmts`), and add `fail` to
    it.
  - `extern`/`export` functions cannot fail (`E028`); function values
    cannot fail yet (a failing function type is only named:
    `docs/reflection-plan.md`).
- **2.3 Lowering**: a function that can fail gets a hidden `@bit8` success
  out after its other outs (place ABI); `return` sets it, `fail` clears
  it. Call sites branch on it. `IR_SPEC.md` notes the convention.
- **2.4 Standard**: `Optional.get`, `Result.get` and `Result.error` (the
  error side as success) can fail. Migrate every `.unwrap() ->` capture
  (`box.lilac`, `Heap/arena_allocator.lilac`, `Os/memory.lilac`,
  `Os/thread.lilac`, `tests/run`) and every first-`Bool` capture. Remove
  the `(ok, value)` `unwrap`; `unwrap_or(default)` stays.
- **2.5 Language server**: `fail` in keyword completion, highlighting and
  the VS Code grammar; statements after `fail` are faded as unreachable;
  `LocalSite`s for every capture binding.
- **2.6 Tests and docs**: run tests for each form, failure outs, a guard
  that falls through (error), an unhandled call (error). `SPEC.md` §4
  (signatures, captures, loops), `STDLIB.md` §0.3 and §3.1.

---

## Phase 3: Ownership checker

The largest phase. One pass decides moves, copies and drops; lowering
follows its decisions and never re-derives them.

- **3.1 The pass** (new `crates/lilac-typeck/src/places.rs`): runs on each
  checked instance body, since move-or-copy depends on the concrete
  types, and reports through the `Sink` so a failing generic instance
  folds into `E042` with its origins. Templates hold their diagnostics as
  dependent (`emit_involving`). Control flow is structured, so the pass
  is a tree walk:
  - State is the set of full places (locals and field paths).
  - Branches merge into "definitely full" and "maybe full".
  - `return`, `break`, `continue`, `fail`, `@panic` and `@trap` end a
    path.
  - A loop body is walked a second time with the state at its end, so a
    place moved in one iteration must be set again before the next.
- **3.2 Copy rules**: `Checker::is_copyable(T)` per the table. Reading a
  place of a non-copyable type moves it. Errors: reading an empty or
  maybe-empty place (with the move's span as a label), moving out through
  a `ref` (deref, field of a `ref`, by-ref `self`), and moving a field out
  of a type with its own `@on_drop`.
- **3.3 `move a`**: a `move` keyword (`lilac-lex`, `lilac-parse`) before a
  place moves it whatever its type. The only identifier named `move` is
  `Box.move` (and its call in `tests/run/stdlib/main.lilac`), which 3.9
  removes: `moved: Box[Int32] = box` moves by rule.
- **3.4 Out slots**: start empty; the first assignment initializes (no
  drop, no `@on_override`). Success outs are full at every `return` and
  at the end of the body; failure outs at every `fail`. The rule covers
  every type. Audit `lib/Standard` and `tests/run` for outs left unset
  (mostly the `unwrap` shape Phase 2 removed).
- **3.5 Facts for lowering**: record in `HirFunc` which reads move and
  which places need a drop flag, plus the drop decision for each scope
  exit and assignment.
- **3.6 Lowering** (`crates/lilac-lower/src/ownership.rs`, `control.rs`,
  `lib.rs`):
  - A move reads the place without copy glue.
  - Scope ends and assignments drop only full places.
  - A flagged place gets a hidden `@bit8` local, set on initialization
    and cleared on move; its drops test it.
  - Out slots no longer drop on first assignment.
  - Storage stays zero-filled (`zero` at function entry and in struct
    literals is unchanged).
- **3.7 `self` by value**: the parser accepts `self: T` (and
  `self: ref T`); bare `self` stays `ref`. `@on_drop`/`@on_copy` keep
  bare `self`. `Optional.get` and `Result.get` take `self` by value.
- **3.8 `match` bindings**: the field rule. A payload binding on an owned
  scrutinee moves the payload (the pass treats the payload as a field
  path of the scrutinee); on an either with its own `@on_drop`, or through
  a `ref`, binding a non-copyable payload is an error.
- **3.9 Standard**: `Box` loses `move()` and its `@on_copy` error (it moves
  by rule). `Box.create` moves `data` into the heap. The `ArenaAllocator`
  copy becomes a move. The pending tests from 0.2 that this phase
  delivers are un-pended.
- **3.10 Language server**: the move's span as related information on a
  use-after-move error; `move` in keyword completion, highlighting and
  the VS Code grammar.
- **3.11 Tests and docs**:
  - Run tests: use after move, a move on one branch (drops once), a move
    in a loop (error unless reassigned), an unset out slot (error), field
    moves, `match` moving a payload, `move a` on an `Rc`-like type (no
    copy hook runs), consuming methods, `ArenaAllocator.init` no longer
    panics.
  - `SPEC.md` §1 (Zero-Initialization reworded; Ownership rewritten), §5,
    §8.
  - `STDLIB.md` §5 gap 7 resolved.
  - `AGENTS.md` Pipeline and the lowering notes.
- **Delivered notes.** `@compile_error` ends a path like `@panic`, so
  `Standard.Os`'s unsupported-target arms need no outs; a `match` naming
  every variant has no `else` path. The `ArenaAllocator` items wait for
  the owner's `Standard` changes, which are not in the repository yet.
  `standard_io`'s `é` came from `Box.create` dropping its parameter early
  and is gone until `Box` drops its contents (Phase 4).
  (Phase 4 brought it back.)

---

## Phase 4: Raw memory builtins and `@discard`

- **4.1 Intrinsics** (`crates/lilac-hir/src/intrinsics.rs`): `@write`,
  `@read`, `@drop`, `@needs_drop`, `@discard`, checked specially (like
  `@array_at`).
  - Typeck rejects `ref unknown` for `@write`/`@read`/`@drop` (no type to
    move or drop), and checks errors through `emit_involving`.
  - `@needs_drop` folds in `crates/lilac-lower/src/constant.rs` like
    `@size_of`, so `if @needs_drop(T)` lowers only the live arm.
- **4.2 Lowering** (`crates/lilac-lower/src/intrinsics.rs`):
  - `@write` stores without dropping.
  - `@read` loads into a fresh temporary without copy glue.
  - `@drop` calls the type's drop routine on the indirect place, with no
    zeroing.
- **4.3 `@discard`**:
  - The argument must be an owned place of type `T`, and the enclosing
    function's parent definition must be `T` (the template for generic
    types); anything else is an error.
  - The pass marks `T`'s hook skipped and turns the fields into places of
    their own. Lowering drops the remaining full fields one by one.
- **4.4 Standard**:
  - `Allocator.create(value) ref T` (allocate plus `@write`) and
    `destroy(ref T)` (`@drop` plus free).
  - `Box` uses them; `Box.into_inner(self: Box[T])` uses `@discard`.
  - Remove the "zeroed allocation so the first assignment drops a zero"
    convention.
- **4.5 Language server**: hover signatures in `handlers.rs`
  (`intrinsic_signature`).
- **4.6 Tests and docs**:
  - Run tests: `Box[Leaf]` drops once, `into_inner`, a write/read round
    trip, `@needs_drop` as a constant condition.
  - `FAILS_` tests: `@discard` outside the type, the memory builtins on
    `ref unknown`.
  - `SPEC.md` §3, `AGENTS.md` runtime notes.
- **Delivered notes.**
  - `move a` became `@move(a)` (owner's call, 2026-10-11): the keyword
    is gone, so `move` is an ordinary name again.
  - `@needs_drop` folds in typeck (`Checker::needs_drop`, the rule the
    ownership pass uses), like `@array_len`, rather than in lowering.
  - `@write`/`@read`/`@drop` lower through the existing assignment, move
    and drop paths on `*p`; they need no LIR of their own.
  - `@discard` errors are `E066`, also for discarding on some paths only
    (torn: one path would run `@on_drop`, the other not). Using the value
    whole after is `E063`. A type with no `@on_drop` of its own discards
    as a no-op. Eithers cannot be discarded (no fields to keep).
  - `Allocator` also gained `free[T](ref T)` for a slot whose value was
    moved out with `@read` (`Box.into_inner`); Phase 5 replaces it.
  - `Os.allocate` still zeroes (it is the OS heap's contract); nothing
    relies on it any more.

---

## Phase 5: Standard: allocators, `Rc`, `Arc`

- **5.1 Allocator**: an `Allocator` value (a context pointer plus
  `allocate`/`free` functions; `free` takes the size and alignment).
  - `Standard`'s `@entry` (`lib/Standard/Runtime`) sets up the
    general-purpose allocator in a global before calling `main`;
    `Heap.allocator()` returns it. It must be safe across threads, since
    every thread shares it (the OS heaps are).
  - Allocating types take one optionally: `Box.create(value)` uses the
    general allocator, `Box.create_in(allocator, value)` a given one, and
    the value keeps the allocator it was made with so its drop frees there.
- **5.2 Arena** (the owner's file):
  - The review fixes from 2026-10-11 (exact fill, overflow, signed sizes,
    alignment by offset).
  - `allocator()`.
  - Raw `create[T]` refuses `@needs_drop(T)` with `@compile_error`
    (values with drop hooks go in a `Box` or container that uses the
    arena).
  - `reset` reclaims memory only.
- **5.3 `Rc[T]` and `Arc[T]`**: a shared block holding the count and the
  value. `@on_copy` increments, and `@on_drop` decrements and, at zero,
  `destroy`s the block. `Arc` uses the atomic intrinsics.
- **5.4 Tests and docs**:
  - Run tests: counts across copies, the value dropped exactly once,
    `Rc[Box[T]]`, `Arc` across `Os.spawn`.
  - `STDLIB.md` §2 (`Allocator` shape) and §3.

---

## Out of scope

- `try` or any automatic propagation of failures.
- Function types that can fail, and generic function instances as values
  (needed for an arena that records drop functions; a typed `Pool[T]`
  covers the common case without them).
- Borrow checking: a `ref` can still dangle.
- Making `Os` functions fail directly instead of returning `Result`.
- Globals: `name: Type` stays zeroed storage (globals are never dropped
  or moved).
