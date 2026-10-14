# Lilac Standard Library — Layout & Feature Inventory

> Status: design inventory, not implemented. Companion to `SPEC.md`.
> `SPEC.md` is normative for the language; this document describes the
> intended `Standard` namespace tree and enumerates its features. It is a
> plan, not a contract — names may change as phases land.

The standard library is ordinary Lilac code. The compiler names **zero**
standard-library entities (`SPEC.md` §2); everything here is built from bit
containers, `object`/`enum`, generics, `ref`, `@` hooks, and `extern`.

What each declaration does is documented in the source, in `///` doc
comments above it, which the language server shows in hovers, signature
help and completion; this document is the layout and the plan, not the
reference. Comments above a group of declarations, and inside bodies,
stay `//` (`crates/lilac-parse/tests/standard_docs.rs` checks that every
`///` documents a declaration).

---

## 0. Foundations this layout assumes

These were decided before the layout and shape every namespace below.

| Decision | Choice |
|---|---|
| Runtime target | Host OS via libc `extern` bindings (allocation, files, atomics). |
| Abstraction model | Structural duck-typed conventions (fixed method shapes). **No language traits/interfaces.** |
| Container semantics | Value semantics: assignment/passing deep-copies via `@on_copy`, scope exit frees via `@on_drop`. |
| Allocator model | Process-wide malloc/free default, plus an optional `Allocator` argument on allocating constructors. |
| Ownership | `Unique[T]` (moves by its `@on_drop`), `Rc[T]` (non-atomic), `Arc[T]` (atomic), `Weak[T]`. |

### 0.1 How namespaces work (drives the root-vs-subfolder split)

Namespaces mirror folders; every `*.lilac` file directly in a folder belongs
to that folder's namespace and files in one folder merge. `using A.B` imports
that namespace's public names **into the importing file only**; imports are
non-transitive. A namespace automatically sees its **ancestors** (ancestor
reach) and its descendants.

There is **no re-export**: an item must be *defined* in the namespace you
import it from. Type aliases (`Name: alias Type`, `SPEC.md` §6) exist,
but an alias is a new item naming a type, not a way to re-export an item
under its own name (a function or namespace cannot be aliased into
another namespace). Therefore the root namespace is a real prelude
(items *defined* at `Standard`), not an umbrella that re-exports its children.
`Standard` plays the role `System` plays in C#.

### 0.2 Naming conventions (assumed — confirm)

* **Namespaces, types, objects, enums, fields**: `PascalCase` (`String`,
  `HashMap`, `Size`, `Buffer`) — matches `SPEC.md`'s `Object`, `String`,
  `Size`, `Buffer`.
* **Functions, methods, hooks, locals**: `snake_case` (`at`, `slice`,
  `push_back`, `@less_than`, `call_something`) — matches the hook spellings and
  `SPEC.md` §2's `s.at(0x0)`, `s.slice(0x0, 0x2)`. (`SPEC.md` also shows
  `x.toHex()`; that camelCase example is treated as illustrative, not binding.)

### 0.3 Error-handling philosophy

* **Recoverable** failures return `Result[T, E]` / `Option[T]` (files, parsing,
  allocation, network).
* **Programmer errors** `panic` (index out of bounds).
* `Result`/`Option` are plain `enum`s, driven by `match` (statements-only in
  v1) and helper methods. Their `get` (and `Result.error`) are functions
  that can fail (`SPEC.md` §4) and take `self` by value (the payload moves
  out), so the caller handles the empty side with a
  capture `if`, a guard or a `loop if`; there is no panicking `unwrap`.

### 0.4 The `==` hazard (why a conventions layer exists)

`==` is **non-overridable bit-equality** (`SPEC.md` §8). For any heap-backed
type this compares the *handle*, not the contents: two `String`s with equal
text but different buffers are `!=` under `==`; two `List[T]`s compare
`{len, ptr, cap}`. Content equality therefore **must** go through named
methods (`equals`, `compare`) supplied by the structural conventions in
§2 — never `==`. This is why `Standard` documents `Equatable`/`Comparable`/
`Hashable` as conventions rather than relying on the operator.

---

## 1. Namespace tree

```
Standard/                 root prelude: scalars, operators, Option/Result, panic, conventions
├── Memory/               allocators + ownership types
├── Text/                 strings, unicode, parse/format
├── Collections/          containers, iterators, algorithms
├── Io/                   files, paths, streams, console
├── Os/                   the portable OS layer, picked by @builtin.Target
│   ├── Windows/          raw Win32 (kernel32, bcryptprimitives)
│   ├── Linux/            raw Linux system calls
│   └── MacOs/            raw libSystem
├── Runtime/              the platform hooks: @entry (calls main), @exit, @compiler_print
├── C/                    ISO C externs and C types (--libc)
├── Threading/            threads, mutex, rwlock, channel (over C)
├── Math/                 numeric math + random
├── Diagnostics/          error types, formatting, color, tracing
├── Testing/              test harness + assertions
└── Compiler/             self-host toolkit (interner, source map, text buffers, arena AST)
```

Dependency direction (lower depends on nothing above it):

```
Standard  ←  Memory  ←  Text  ←  Collections
                    ↖        ↖        ↑
Standard  ←  C  ←  Io / Threading / Math
Standard  ←  Diagnostics  ←  Testing / Compiler
```

`Standard.Os.Windows`, `Standard.Os.Linux`, `Standard.Os.MacOs` and
`Standard.C` are the non-portable namespaces; everything above them is
portable Lilac. `Standard.Os` picks each OS call per target at compile
time, so code using it is portable too, and the rest of the library sits
only on `Standard.Os`, never on a raw namespace.

A typical program imports the prelude plus the domains it uses:

```lilac
using Standard
using Standard.Text
using Standard.Collections
using Standard.Io
```

A file inside `Standard.Collections` sees `Standard` root automatically by
ancestor reach and only needs `using Standard.Text` for cross-domain needs.

---

## 2. Cross-cutting conventions (structural shapes)

These are **documented method shapes**, not declarations. Conformance is
proven at generic instantiation (`SPEC.md` §7). Nothing here is imported; the
shapes are the contract.

| Convention | Required shape | Used by |
|---|---|---|
| `Equatable` | `equals(self, other: ref T) @bit8` (or static `equals(a, b) @bit8`) | `Set`, `Map`, `List.contains`, `index_of` |
| `Comparable` | `compare(self, other: ref T) Ordering` | `sort`, `binary_search`, `PriorityQueue`, `min`/`max` |
| `Hashable` | `hash(self) @bit64` **and** `Equatable` | `HashMap`, `HashSet` |
| Formattable | a `@format: function (self, out: ref Formatter) nothing` hook (§3.1a; a hook, not a method) | f-strings, `Io.print`, panics, `Diagnostics`, `join` |
| `Iterator` | `next(self) Option[T]` (advances, returns `None` at end) | all adapters and `for`-style `loop` |
| `Sequence` | `iter(self) I` where `I` is `Iterator`-shaped | `map`/`filter`/`fold`/`collect` |
| `Allocator` | `alloc(self, size, align) ref @bit8`, `free(self, ptr, size, align)`, `realloc(...)` | every allocating container |
| `Numeric` | operator hooks (`@add`…`@negate`) + `zero`/`one` | generic math, `sum`, `product` |

Adapter/conversion helpers for these live with their consumers:
`Ordering` in `Standard`; `hash`/`DefaultHasher` in `Standard.Collections`;
`alloc_*` helpers in `Standard.Memory`.

---

## 3. Feature inventory

Notation: `T`/`K`/`V`/`E` are generic parameters; `!name` is a `data` const;
`ref T` is a reference. Methods listed with `self` are instance methods.

### 3.1 `Standard` (root prelude)

**Scalar aliases** (thin names over bit containers; meaning from use, per
`SPEC.md` §1):

| Type | Underlying | Notes |
|---|---|---|
| `Bool` | `@bit8` | `true`/`false` data consts (`0x1`/`0x0`); strict non-zero test in conditions |
| `Byte` | `@bit8` | |
| `Char` | `@bit32` | Unicode scalar value; helpers in `Standard.Text` |
| `Int8/16/32/64` | `@bit8/16/32/64` | signed interpretation |
| `Int` | `@bit_native` | pointer-width signed |
| `Uint8/16/32/64` | `@bit8/16/32/64` | unsigned interpretation |
| `Uint` | `@bit_native` | pointer-width unsigned |
| `Float16/32/64` | `@fbit16/32/64` | IEEE-754 via `@f_*_W` intrinsics |

**Operator hooks** (one overload per width/kind; `SPEC.md` §8): `@add`,
`@sub`, `@mul`, `@div`, `@mod`, `@less_than`, `@greater_than`,
`@less_than_or_equal`, `@greater_than_or_equal`, `@negate` — signed and
unsigned variants; float variants. Unsigned comparisons bind
`@i_less_than_u_W`/`@i_greater_than_u_W`; signed bind `@i_less_than_W`/…; float bind
`@f_less_than_W`/….

**Bit operations** (require new intrinsics — see §5): `@bit_and`, `@bit_or`,
`@bit_xor`, `@bit_not`, `@shift_left`, `@shift_right` (arithmetic/logical),
`@rotate_left`, `@rotate_right`, `@popcount`, `@leading_zeros`,
`@trailing_zeros`, `@byte_swap`.

**Core enums & types:**

| Type | Definition | Key methods |
|---|---|---|
| `Option[T]` | `enum { Some: T; None }` | `is_some`, `is_none`, `unwrap`, `unwrap_or`, `unwrap_or_else`, `expect`, `map`, `and_then`, `or`, `or_else`, `take`, `replace`, `iter` |
| `Result[T, E]` | `enum { Ok: T; Error: E }` | `is_ok`, `is_err`, `unwrap`, `unwrap_err`, `unwrap_or`, `expect`, `map`, `map_err`, `and_then`, `or_else` |
| `Ordering` | `enum { Less; Equal; Greater }` | `then`, `reverse`, `is_less`… |
| `Never` | `enum {}` (empty) | uninhabited; result of `panic`/`abort` |
| `Unit` | alias for `nothing`-shaped empty object | for generic uniformity |

> **Today:** `Optional[T]` (`Some`, `None`; `get: value: T else nothing`,
> `get_ref: value: ref T else nothing` (the value in place),
> `unwrap_or`, `unwrap_or_default`) and a first `Result[T, E]` (`Ok`,
> `Error`; `is_ok`, `get: value: T else error: E`, `error: error: E else
> nothing`, `unwrap_or`). `get`, `error` and the `unwrap_or`s take `self`
> by value, so they move the optional or result. Both start zeroed as their first variant:
> a zeroed `Optional` is `Some`, so set `.None` explicitly.

**Functions:**

* `panic(message: @byte_buffer) Never` — abort with message (`@lilac_abort` /
  C `abort`).
* `assert(condition: @bit8)` / `assert_eq(a, b)` (over `Equatable`) — panic on
  failure; compiled out in release if a `Debug`/release flag exists.
* `unreachable() Never`.
* `min[T]`, `max[T]`, `clamp[T]` (over `Comparable`) — or in `Standard.Math`.
* `swap[T](ref a, ref b)`.

**Conversions:** width widening (`@conversion` per pair, e.g.
`@conversion(from: @bit8) to: @bit32`), `Char`↔`@bit32`, `@bit*`↔float,
`@bit*`→`@byte_buffer` textual? (textual lives in `Text`).

**Conventions:** documented in §2 (no declarations).

---

### 3.1a Formatting (in `Standard`)

> **Today:** this section is implemented (`lib/Standard/format.lilac`).

An f-string is an `@format_args(W)` (`SPEC.md` §2, "Formatted strings"):
its literal pieces and, per hole, a borrow of the value and its `@format`
hook. `Standard` supplies the one `W`, so every formattable type and every
consumer meet in it:

| Item | What it is |
|---|---|
| `Formatter` | Where formatted text goes: an object holding a `target: ref unknown` and a `sink: FormatSink`. `Formatter.over(target, sink)` makes one; `write(text: Format)` writes a format (an f-string, a literal or one formattable value), `write_bytes(bytes)` raw bytes. |
| `Format` | `alias @format_args(Formatter)`: the type of an f-string written into a `Formatter`. |
| `FormatSink` | `alias function (target: ref unknown, bytes: @byte_buffer) nothing`: what a consumer hands a `Formatter`. |
| `Hex`, `Binary` | An integer's bits in base 16 (lowercase) or 2, no prefix, at the type's width: `{n.hex()}`, `{n.binary()}` on `Int`, `Int8`…`Int64`. |
| `Padded` | `pad_left(ref f, width)`, `pad_right` and `pad_center` (an odd space goes after) pad a `Format` with spaces to `width` code points; a counting `Formatter` measures it first. |

A type is formattable when it has a `@format` hook for `Formatter`, found
by exact signature (`SPEC.md` §8):

```lilac
Point: object {
    x: Int
    y: Int

    @format: function (self, out: ref Formatter) nothing {
        out.write($"({self.x}, {self.y})")
    }
}
```

`Standard` defines `@format` for `@bit8`…`@bit64` and `@bit_native`
(unsigned decimal), `Int` and `Int8`…`Int64` (signed decimal), `Bool`
(`true`/`false`), `@byte_buffer` (its bytes), `Format` (a nested format)
and `Optional[T]` / `Result[T, E]` (`Some(v)`, `None`, `Ok(v)`,
`Error(e)`: their hooks capture the payload through `self`, so they work
when the payload types copy and have a `@format`, checked where they are
used). Floats have none yet: shortest round-trip printing is its own work
item, so a float hole is `E071`, never wrong digits. A character literal
is a `@bit32`, so `{'A'}` prints `65` until `Standard` has a `Char` type.

A consumer makes a `Formatter` over its own storage: `Io.print` over a
line buffer on the stack, `Runtime`'s `@compiler_print` over standard
error (so `@panic($"index {i} past {n}")` formats), and the counting
formatter `Padded` uses. N types and M consumers need N hooks and M sinks.

### 3.2 `Standard.Memory`

> **Today:** `Standard.Heap.Allocator` over `Standard.Os`:
> `create[T](value) ref T` (allocate, then `@write` the value in),
> `destroy[T](ref T)` (`@drop`, then free) and `free[T](ref T)` (free a
> slot whose value was moved out with `@read`). `Box[T]` (in `Standard`)
> is the owned heap box: `create(value)`, `into_inner() T` (by `@discard`
> and `@read`), and its drop drops the value and frees it. The inventory
> below is the target design.

**Allocators:**

| Type | Purpose | Key methods |
|---|---|---|
| `Allocator` | structural convention (§2) | `alloc(size, align)`, `free(ptr, size, align)`, `realloc(ptr, old, new, align)`, `alloc_type[T]() Unique[T]`, `alloc_slice[T](count) Slice[T]` |
| `SystemAllocator` | malloc/free-backed; **the process default** | implements `Allocator`; `global() ref Allocator` |
| `Arena` | bump/region allocator; bulk free via `reset` | `new(backing)`, `alloc`, `reset`, `reset_retain`, `used`, `capacity` |
| `Pool[T]` | fixed-size object freelist (nice-to-have) | `alloc`, `free`, `reserve` |

**Ownership:**

| Type | Semantics | Key methods |
|---|---|---|
| `Unique[T]` | owned heap box; single logical owner | `new(value)`, `get() ref T`, `get_mut() ref T`, `replace(value)`, `into_inner() T`, `is_valid()` |
| `Rc[T]` | non-atomic reference count | `new(value)`, `strong_count`, `weak_count`, `downgrade() Weak[T]`, `get() ref T` |
| `Arc[T]` | atomic reference count | same surface as `Rc[T]`; thread-safe |
| `Weak[T]` | non-owning handle for cycles | `upgrade() Option[Rc[T] / Arc[T]]`, `strong_count` |

Constructors are static functions (`b: Unique[T].new(value)`). Hooks:
`@on_copy` (`Unique` deep-copies or transfers; `Rc`/`Arc` bump the count),
`@on_override` (drop the previous value), `@on_drop` (`Unique` frees;
`Rc`/`Arc` decrement and free at zero).

**Helpers:** `defer`/guard objects for RAII scopes; `align_up`; `mem_copy`,
`mem_move`, `mem_set`, `mem_compare` (thin wrappers over `@memcpy`/`C`).

---

### 3.3 `Standard.Text`

| Type | Purpose | Key methods |
|---|---|---|
| `String` | owned UTF-8 (`{ Size: @bit_native; Buffer: ref @bit8 }`) | `len`, `byte_len`, `is_empty`, `byte_at(i)`, `at(i) Char`, `slice(lo, hi) String`, `clone`, `concat(other)`, `push_byte`, `push_char`, `push_str`, `clear`, `truncate`, `find(needle) Option[@bit_native]`, `rfind`, `contains`, `starts_with`, `ends_with`, `split(delim) List[String]`, `lines`, `trim`/`trim_start`/`trim_end`, `to_lower`/`to_upper`, `replace`, `repeat(n)`, `compare(other) Ordering`, `equals(other) @bit8`, `hash() @bit64`, `to_int`/`to_float`, `bytes() Iterator[@bit8]`, `chars() Iterator[Char]` |
| `StringView` | borrowed view (no allocation; wraps `@byte_buffer`) | `len`, `at`, `slice`, `equals`, `to_string` |
| `StringBuilder` | growable text buffer | `new`, `with_capacity`, `push_char`, `push_str`, `push_int`, `push_float`, `append_literal(@byte_buffer)` (f-string handler shape), `append[T](T)`, `build() String`, `clear`, `len` |
| `Utf8` | encoding/decoding | `encode(codepoint) String`, `decode(view) Result[Char, Utf8Error]`, `next_codepoint(view, ref i) Option[Char]`, `validate(view) Result[@bit_native, Utf8Error]`, `count_codepoints` |
| `Unicode` | scalar properties | `category(c)`, `is_letter`, `is_digit`, `is_whitespace`, `is_alphanumeric`, `to_lower(c)`, `to_upper(c)` |
| `ParseError` | `enum { Empty; InvalidDigit; Overflow; Trailing }` | |
| `Utf8Error` | `enum { InvalidStart; InvalidContinuation; Truncated }` | |

**Functions:** `parse_int(view) Result[Int64, ParseError]`,
`parse_uint`, `parse_int_radix(view, radix)`, `parse_float(view)
Result[Float64, ParseError]`; `format_int(value, radix) String`,
`format_hex`, `format_bin`, `format_oct`, `format_float`, `format_decimal`
(bridges `lilac-decimal`), `pad_left`/`pad_right`/`pad_center`, `join`.

**Conversions & hooks:** `@conversion(from: @byte_buffer) to: String`,
`@conversion(from: String) to: @byte_buffer`,
`@conversion(from: @format_args(String)) to: String`, `@conversion(from:
Char) to: String`, and `@format(ref T, ref String)` for the types it formats.
`@format` for `Char`, and for floats once shortest round-trip printing lands (§3.1a).

---

### 3.4 `Standard.Collections`

**Containers:**

| Type | Purpose | Key methods |
|---|---|---|
| `List[T]` | dynamic array (Vec-like) | `new`, `with_capacity(n)`, `push`, `pop() Option[T]`, `insert(i, v)`, `remove(i) T`, `at(i) ref T`, `set(i, v)`, `get(i) Option[ref T]`, `len`, `capacity`, `reserve`, `clear`, `truncate`, `reverse`, `extend(other)`, `contains(v)`, `index_of(v) Option[@bit_native]`, `first`/`last`, `iter`, `slice(lo, hi) Slice[T]`, `sort`, `sort_by(cmp)` |
| `Array[T]` | fixed-capacity array | `new(capacity)`, `at`, `set`, `len`, `iter`, `slice` |
| `Deque[T]` | double-ended queue (ring) | `push_front`, `push_back`, `pop_front() Option[T]`, `pop_back`, `at`, `len` |
| `HashMap[K, V]` | open-addressing hash map (needs `Hashable[K]`) | `new`, `with_capacity`, `insert(k, v) Option[V]`, `get(k) Option[ref V]`, `get_mut`, `remove(k) Option[V]`, `contains_key`, `len`, `clear`, `iter() Iterator[(K, V)]`, `keys`, `values`, `entry(k)` |
| `HashSet[T]` | set over `HashMap` | `insert`, `contains`, `remove`, `len`, `iter`, `union`, `intersection`, `difference` |
| `Stack[T]` | LIFO wrapper | `push`, `pop`, `peek`, `len` |
| `Queue[T]` | FIFO wrapper | `enqueue`, `dequeue`, `peek`, `len` |
| `PriorityQueue[T]` | binary heap (needs `Comparable`) | `push`, `pop() Option[T]`, `peek`, `len` |
| `Range[T]` | half-open interval | `new(start, end)`, `new_step`, `start`, `end`, `step`, `iter`, `contains` |
| `Slice[T]` | borrowed view `{ ptr; len }` | `len`, `at`, `slice`, `iter`, `to_list` |

**Conventions (implemented by containers):** `Iterator`, `Sequence`,
`Equatable` (elementwise `equals`), `Comparable` (lexicographic `compare`),
`Hashable` (order-independent for sets/maps if desired).

**Iterator / Sequence** (structural shapes):

* `Iterator[T]`: `next(self) Option[T]`.
* `Sequence[T]`: `iter(self) Iterator-shaped`.
* Adapters (generic objects/functions): `map`, `filter`, `filter_map`, `fold`
  / `reduce`, `zip`, `enumerate`, `take`, `take_while`, `skip`, `chain`,
  `flatten`, `collect_into[List[T]]`, `count`, `any`, `all`, `find`,
  `position`, `sum`, `product`, `min`/`max`.

**Algorithms:** `sort[T]` / `sort_by`, `stable_sort`, `binary_search`,
`reverse`, `rotate`, `partition`, `dedup`.

**Hashing:** `Hasher` shape (`write_byte`, `write_bytes`, `finish() @bit64`),
`DefaultHasher` (e.g. FNV-1a 64), `hash[T](v) @bit64`, `Hashable` for scalars,
`String`, tuples, and composites.

---

### 3.5 `Standard.Io`

> **Today:** `print`, `print_line`, `print_error` and `print_error_line`
> take a `Format` (an f-string, a literal or one formattable value, §3.1a)
> and write it through a line buffer on the stack: one `Standard.Os.write_all`
> per line (and per full buffer), not per piece. `print_char` (a code point,
> as UTF-8) and `print_byte` stay until `Standard` has a `Char` type.
> `Standard.Process.exit` and `Standard.Heap.Allocator` likewise sit on
> `Standard.Os` (`exit`, `allocate`/`deallocate`), and `Standard.Runtime`
> defines the platform hooks (SPEC §8) over it; its `@compiler_print`
> takes a `Format` and writes it to standard error through `print_error`.
> The inventory below is the target design.

### 3.5a `Standard.Os` (the OS layer)

Two levels. The raw namespaces expose each platform's own interface,
faithfully: the platform's names, argument orders, constant values and
error convention, so its documentation (MSDN, the man pages) describes
them and C examples port one for one. Each is split into files by area
(`io`, `memory`, `process`, `time`, `constants`, ...) that merge into one
namespace; unused declarations cost nothing (DCE).

| Namespace | Interface | Failure |
|---|---|---|
| `Os.Windows` | kernel32 (`CreateFileW`, `VirtualAlloc`, `SRWLOCK`s, ...), `ProcessPrng`; `windows.h` constants (`STD_OUTPUT_HANDLE`, `MEM_COMMIT`, `ERROR_*`) and structs (`SYSTEM_INFO`, `WIN32_FIND_DATAW`) | `BOOL`/`HANDLE`, then `GetLastError()` |
| `Os.Linux` | system calls through `@linux_syscall`, the ones x86-64 and 64-bit Arm share (`openat`, `mmap`, `clone`, `futex`, `getrandom`, ...); kernel constants, per architecture where they differ (`O_DIRECTORY`) | the raw `-errno`: `is_error(r)`, `errno_of(r)` |
| `Os.MacOs` | libSystem (`open`, `mmap`, `clock_gettime_nsec_np`, `getentropy`, `pthread_*`, `os_unfair_lock_*`, ...); Darwin constant values | `-1`/null, then `errno()` |

Every `Os.Windows` function is in `toolchain/windows/*.def` and every
`Os.MacOs` one in both `libSystem.tbd` stubs; `tests/toolchain.rs` checks.

The portable surface, each function one `match @builtin.Target.os` over
the raw namespaces (a bare target, `.None`, gets the C library where one
exists, else a compile error):

* **Handles:** `Handle` (a `HANDLE` or a descriptor), `standard_input()`,
  `standard_output()`, `standard_error()`; `read`, `write` (both
  `Result[@bit_native, Error]`), `write_all` (`Optional[Error]`),
  `open(path, OpenMode) Result[Handle, Error]`, `close`,
  `seek(handle, offset, SeekFrom) Result[@bit64, Error]`, `size`.
* **Memory:** `allocate(size)` (zeroed) and `reallocate(memory, size)`
  (both `Result[ref unknown, Error]`; a failed `reallocate` leaves
  `memory` as it was), `deallocate(memory)`; `map_pages(size)
  Result[ref unknown, Error]` and `unmap_pages(memory, size)
  Optional[Error]` (always the OS), `page_size()`.
* **Process:** `exit(code)`; `argument_count`, `argument_vector`,
  `environment_vector` (stored by `@entry`).
* **Time:** `monotonic_nanos()`, `wall_clock_nanos()`, `sleep(nanos)`.
* **Random:** `fill_random(buffer, length)`.
* **Threads:** `spawn(entry, argument) Result[Thread, Error]` runs
  `entry(argument)` (a `function (argument: ref unknown) nothing`) on a new
  thread, `join(thread) Optional[Error]` waits for it and frees it,
  `yield_thread()`. Windows uses `CreateThread`, macOS `pthread_create`,
  and native Linux its own `clone` thread (`@linux_thread_clone`) on a 1 MiB
  mapping holding its stack and thread-local block. Each starts in the
  `export` `thread_start` (C symbol `lilac_thread_start`), which every
  program mounting `Standard` keeps; under `--libc`, Linux
  uses `pthread_create`, and a native program that links a shared C
  library should too. The stack-trace head is per thread.
* **Errors:** `Error`, the common cases by name (`NotFound`,
  `PermissionDenied`, ...) plus `Other: @bit32`, from `error_of_errno` and
  `error_of_windows`.

Under `--libc`, the C library owns the heap (`allocate` is `calloc`, so C
and Lilac share one) and the exit (C `exit`, so `stdio` flushes); Linux
still makes system calls for everything else.

| Type | Purpose | Key methods |
|---|---|---|
| `File` | OS file handle (`@on_drop` closes) | `open(path, mode) Result[File, IoError]`, `create`, `read(buf) Result[@bit_native, IoError]`, `read_to_string() Result[String, IoError]`, `write(bytes)`, `write_str`, `write_all`, `seek(off, whence)`, `size() Result[@bit_native, IoError]`, `flush`, `close` |
| `OpenMode` | `enum { Read; Write; Append; ReadWrite }` + create/truncate flags | |
| `Path` | path value | `new(s)`, `join(part)`, `parent() Option[Path]`, `file_name() Option[String]`, `extension() Option[String]`, `is_absolute`, `normalize`, `to_string` |
| `Directory` | directory ops | `create`, `remove`, `exists`, `list() Result[List[Path], IoError]` |
| `Reader` | structural read shape | `read(buf) Result[@bit_native, IoError]`, `read_u8`, `read_line() Result[Option[String], IoError]`, `read_all` |
| `Writer` | structural write shape | `write(bytes)`, `write_u8`, `write_str`, `flush` |
| `BufferedReader[R]` | buffered wrapper | over any `Reader`; `read_line`, `read_until` |
| `BufferedWriter[W]` | buffered wrapper | over any `Writer`; `flush` |
| `Stdin` / `Stdout` / `Stderr` | standard streams | `print`, `println`, `eprint`, `eprintln`, `read_line`, `write`, `flush`; `@on_drop` flushes |
| `IoError` | `enum { NotFound; PermissionDenied; AlreadyExists; IsADirectory; NotADirectory; Interrupted; UnexpectedEof; Other: @bit32 }` | `message() String` |

**Conversions:** `@conversion(from: String) to: Path`,
`@conversion(from: @byte_buffer) to: Path`; writers accept `String` /
`@byte_buffer`.

---

### 3.6 `Standard.C` (platform layer — libc FFI)

> **Being phased out.** Programs no longer link a C library by default; the
> library's own OS bindings (`Standard.Os`) replace it piece by piece, and code that
> still uses `Standard.C` builds with `lilac build --libc`. See
> `docs/history/build-command-plan.md`. It holds ISO C only: POSIX and
> platform calls (`write`, `mmap`, ...) are in the `Standard.Os` raw
> namespaces, whose constants differ per OS.


The single non-portable namespace. Everything here is either a raw `extern`
declaration or a thin wrapper over one. Above this line the library is
portable Lilac.

**Raw `extern` bindings** (C ABI per `SPEC.md` §9: bit ints by value,
`ref T` → pointer, single return):

* **Memory:** `malloc`, `calloc`, `realloc`, `free`, `memcpy`, `memset`,
  `memmove`, `memcmp`.
* **Files:** `open`, `close`, `read`, `write`, `lseek`, `fsync`, `fstat`,
  `unlink`, `rename`, `mkdir`, `rmdir`, `opendir`, `readdir`, `closedir`,
  `access`, `stat`.
* **Process:** `exit`, `abort`, `getenv`, `setenv`, `getpid`, `fork` (opt),
  `execve` (opt), `waitpid` (opt).
* **Time:** `clock_gettime`, `time`, `nanosleep`, `gettimeofday`.
* **Strings:** `strlen`, `strcmp`, `strncmp`.
* **Atomics:** `__atomic_load_n`, `__atomic_store_n`, `__atomic_fetch_add`,
  `__atomic_fetch_sub`, `__atomic_compare_exchange_n`, plus memory-order
  constants.
* **Threads:** `pthread_create`, `pthread_join`, `pthread_self`,
  `pthread_mutex_init/lock/unlock/destroy`,
  `pthread_cond_init/wait/signal/broadcast`, `sched_yield`.

**C scalar aliases:** `c_int`, `c_uint`, `c_long`, `c_ulong`, `c_char`,
`c_short`, `size_t`, `ssize_t`, `off_t`, `pid_t`, `mode_t`, `time_t`,
`timespec`, `stat` (opaque/opaque-sized).

**Constants:** `O_RDONLY`, `O_WRONLY`, `O_RDWR`, `O_CREAT`, `O_TRUNC`,
`O_APPEND`, `SEEK_SET`, `SEEK_CUR`, `SEEK_END`, `PROT_*`/`MAP_*` (if mmap),
`SIG*`, `W_OK`/`R_OK`.

**Pointer primitives:** `null_ptr[T]() ref T`, `ptr_is_null(p) @bit8`,
`ptr_cast[T, U](p) ref U`, `ptr_add_bytes[T](p, n) ref T`,
`ptr_to_native[T](p) @bit_native`, `native_to_ptr[T](n) ref T`,
`ref_of[T](ref v) ref T`, `ref_to_native`/`native_to_ref` (per `SPEC.md` §5,
raw addresses require explicit conversion to `@bit_native`).

**Atomic primitive:** `Atomic[T]` — `new`, `load`, `store`, `swap`,
`fetch_add`, `fetch_sub`, `compare_exchange`, `compare_exchange_weak`.

**OS access (thin wrappers over the externs):**

| Type | Key methods |
|---|---|
| `Instant` | `now()`, `elapsed_since(other) Duration`, `as_nanos` |
| `Duration` | `from_secs`, `from_millis`, `from_nanos`, `as_secs`, `as_nanos`, `+`/`-` |
| `Env` | `get(name) Option[String]`, `set(name, value) Result[..]`, `vars() Iterator[(String, String)]` |
| `Process` | `args() List[String]`, `exit(code) Never`, `id()`, `spawn`/`wait` (opt) |
| `Clock` | `monotonic() Instant`, `system() Instant` |

> If the thin OS wrappers ever warrant their own namespaces, they can split
> into `Standard.Time` / `Standard.Process`; `C` would then hold only the raw
> FFI.

---

### 3.7 `Standard.Threading`

| Type | Purpose | Key methods |
|---|---|---|
| `Thread` | OS thread (over `Standard.Os`'s `spawn`/`join`, today's raw form) | `spawn(entry, arg) Result[Thread, ThreadError]`, `join() Result[T, ..]`, `yield_now`, `sleep(Duration)` |
| `Mutex[T]` | mutual exclusion | `new(value)`, `lock() MutexGuard[T]`, `try_lock() Option[MutexGuard[T]]`; guard `@on_drop` unlocks |
| `RwLock[T]` | reader/writer lock | `read()`, `write()`, `try_read`, `try_write` |
| `Condvar` | condition variable | `wait(guard)`, `notify_one`, `notify_all` |
| `Channel[T]` | MPMC/SPSC queue | `new(capacity)`, `send(v)`, `recv() Option[T]`, `try_send`, `try_recv`, `close` |
| `ThreadError` | `enum { SpawnFailed; Poisoned }` | |

Atomics live in `Standard.C` (`Atomic[T]`); `Arc[T]` in `Standard.Memory`
becomes fully thread-safe through them.

---

### 3.8 `Standard.Math`

* **Integer:** `abs`, `min`, `max`, `clamp`, `pow`, `gcd`, `lcm`, `div_rem`,
  `div_floor`, `div_ceil`, `is_power_of_two`, `next_power_of_two`,
  `checked_add`/`sub`/`mul` (`Result`), `saturating_*`, `wrapping_*`.
* **Float:** `abs`, `min`, `max`, `clamp`, `sqrt`, `cbrt`, `pow`, `exp`, `ln`,
  `log2`, `log10`, `sin`, `cos`, `tan`, `floor`, `ceil`, `round`, `trunc`,
  `fract`, `is_nan`, `is_infinite`, `is_finite`, `copysign`, `lerp`.
* **Constants:** `PI`, `E`, `TAU`, `INFINITY`, `NAN`, `EPSILON` (per width).
* **`Random`:** `new(seed)`, `next_u32`, `next_u64`, `next_int(range)`,
  `next_float()`, `shuffle[T](ref List[T])`; `Seed`/entropy from `C.Time`.
* **Numeric conversions:** `@conversion` int↔float (lossy, explicit),
  rounding helpers.

> `sqrt`/`pow`/trig need `libm` (`link = ["m"]`) or C externs — see §5.

---

### 3.9 `Standard.Diagnostics`

| Type | Purpose | Key methods |
|---|---|---|
| `Diagnostic` | structured message | `error(code, msg)`, `warning`, `note`, `help`, `with_label(span, text)`, `render() String` |
| `Level` | `enum { Error; Warning; Note; Help }` | |
| `Span` | `{ lo; hi; file }` source location | `merge`, `contains` |
| `SourceMap` | file id ↔ name registry | `add(path)`, `name(id)`, `lookup(span)` |
| `Color` | ANSI styling | `red`, `green`, `bold`, `reset`, `enabled` (tty detection) |
| `EmitMode` | `enum { Human; Json }` | |

**Functions:** `format(spec, args)` (format specifiers beyond f-strings),
`pretty_print(value)` (over `@format`), `write_diagnostic(w, d)`,
`trace`/`debug`/`info` logging to stderr with levels.

---

### 3.10 `Standard.Testing`

| Type | Purpose | Key methods |
|---|---|---|
| `Test` | one test case | `new(name)`, `run()`, `expect(cond)`, `expect_eq(a, b)`, `expect_err` |
| `TestSuite` | collection + runner | `add(test)`, `run_all() TestReport` |
| `TestReport` | results | `passed`, `failed`, `failures() List[TestFailure]` |
| `TestFailure` | `{ name; message; span }` | |

**Functions:** `assert_eq[T: Equatable]`, `assert_ne`, `assert_contains`,
`assert_panics`, `fail(msg)`.

---

### 3.11 `Standard.Compiler` (self-host toolkit)

Utilities the Lilac compiler needs when it is itself written in Lilac. Kept
separate so they do not pollute the general surface.

| Type | Purpose | Key methods |
|---|---|---|
| `Interner` | string → id interning | `intern(view) Symbol`, `resolve(Symbol) StringView`, `len` |
| `Symbol` | interned id (`@bit32`) | |
| `SourceMap` | (shared with Diagnostics) | |
| `TextBuffer` | rope/gap buffer for incremental text | `insert`, `remove`, `slice`, `len`, `lines` |
| `ArenaTree[T]` | arena-backed tree nodes | `alloc(node) Ref`, `get(Ref)` |
| `SmallVec[T, N]` | inline-then-heap vector (nice-to-have) | `push`, `pop`, `at`, `len` |
| `Bump` | phase allocator (alias/refinement of `Memory.Arena`) | |
| `DiagSink` | accumulate + sort diagnostics | `emit`, `sorted`, `count` |

---

## 4. Import examples

```lilac
using Standard                 // Bool, Int32, Option, Result, operators, panic
using Standard.Text            // String, StringBuilder, parse_int
using Standard.Collections     // List, HashMap, sort
using Standard.Io              // File, Stdout
```

```lilac
using Standard
using Standard.Memory          // Arc, Unique, Arena
using Standard.Collections     // List, HashMap
using Standard.Threading       // Mutex, Thread
```

A file under `Standard.Collections/` sees `Standard` (ancestor) for free and
adds `using Standard.Memory` for the allocator convention and
`using Standard.C` for raw pointer/atomic primitives.

---

## 5. Known language gaps this layout depends on

Recorded, not solved here. Each is either a small compiler addition or a
stdlib workaround via `extern`.

1. **No rotate/popcount/clz/ctz intrinsics.** `and`/`or`/`xor`/`not` and
   the shifts exist (`@bit_and_W` … `@bit_shift_right_u_W`); the rest of the bit
   toolkit does not yet, and there are no bitwise operators (`&`, `|`, `<<`).
2. **No host `run`.** The CLI has no one-shot host-native run. Stdlib tests
   must go `emit-llvm` → `clang` link → execute.
3. **No const generics.** `Array[T, N]` needs const-generic parameters
   (`SPEC.md` §7/§9.2 says fixed arrays use `Array(T, len)` constructors, not
   `T[N]`). Either add const generics or use a runtime-length `Array[T]`.
4. **No type-argument inference.** Every generic use spells its arguments
   (`List[@bit32]`); this is an ergonomic tax on the whole surface.
5. **No `[]` indexing/slicing/array literals.** All element access is methods
   (`list.at(i)`, `s.slice(lo, hi)`); constructors must be explicit
   (`List[@bit32](...)` or a builder).
6. **`==` cannot be overridden.** Content equality and map/set keys must route
   through `equals`/`compare`/`hash` conventions (§0.4), never `==`.
7. ~~**No move/non-copyable types.**~~ Resolved: a type with its own
   `@on_drop` and no `@on_copy` moves, the compiler tracks moved-out places
   (`SPEC.md` §1), and `@move(a)` moves a copyable value. `Box` and `Unique[T]`
   need no `move()`; `@write`/`@read`/`@drop` move values in and out of
   raw memory, and `@discard` lets a type end itself without its `@on_drop`
   (`Box.into_inner`).
8. **Non-transitive `using`, no re-export.** There is no umbrella import; each
   domain is imported explicitly, and cross-domain needs are explicit imports.
9. **libm dependency.** `sqrt`/`pow`/trig come from libm (`link = ["m"]`) via
   the package manifest, or from `C` externs.

---

## 6. Non-goals / deferred

* **Traits/protocols/concepts** — abstraction stays structural; a first-class
  feature is deferred (revisit after the stdlib proves the shapes).
* **Async** — futures/executors are objects over `loop` + `match`; `await` is
  deferred (`SPEC.md` §9). Not in this layout yet.
* **Networking, JSON, regex, compression** — Tier-3 extras, later.
* **A `Standard` umbrella that re-exports children** — impossible without
  re-export; the root is a real prelude instead.
* **Floating-point formatting edge cases** — depends on `lilac-decimal` and
  libm; specified when `Text` lands.
