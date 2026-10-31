# Lilac Standard Library — Layout & Feature Inventory

> Status: design inventory, not implemented. Companion to `SPEC.md`.
> `SPEC.md` is normative for the language; this document describes the
> intended `Standard` namespace tree and enumerates its features. It is a
> plan, not a contract — names may change as phases land.

The standard library is ordinary Lilac code. The compiler names **zero**
standard-library entities (`SPEC.md` §2); everything here is built from bit
containers, `structure`/`either`, generics, `ref`, `@` hooks, and `extern`.

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

* **Namespaces, types, structures, eithers, fields**: `PascalCase` (`String`,
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
* `Result`/`Option` are plain `either`s, driven by `match` (statements-only in
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
| `Byte` | `@bit8` | alias of `Uint8` |
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

**Core eithers & types:**

| Type | Definition | Key methods |
|---|---|---|
| `Option[T]` | `either { Some: T; None }` | `is_some`, `is_none`, `unwrap`, `unwrap_or`, `unwrap_or_else`, `expect`, `map`, `and_then`, `or`, `or_else`, `take`, `replace`, `iter` |
| `Result[T, E]` | `either { Ok: T; Error: E }` | `is_ok`, `is_err`, `unwrap`, `unwrap_err`, `unwrap_or`, `expect`, `map`, `map_err`, `and_then`, `or_else` |
| `Ordering` | `either { Less; Equal; Greater }` | `and_then`, `reverse`, `is_less`… |
| `Never` | `either {}` (empty) | uninhabited; result of `panic`/`abort` |
| `Unit` | alias for `nothing`-shaped empty structure | for generic uniformity |

> **Today:** `Optional[T]` (`Some`, `None`; `get: value: T else nothing`,
> `get_ref: value: ref T else nothing` (the value in place),
> `unwrap_or`, `unwrap_or_default`), a first `Result[T, E]` (`Ok`,
> `Error`; `is_ok`, `get: value: T else error: E`, `error: error: E else
> nothing`, `unwrap_or`), and `Ordering` (`Less`, `Equal`, `Greater`;
> `is_less`, `is_equal`, `is_greater`, `is_less_or_equal`,
> `is_greater_or_equal`, `reverse`, `and_then`, `@format`). `get`, `error` and the `unwrap_or`s take `self`
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

> **Today** (math plan phases 2-4, generated one file per type): every
> integer type has `MIN`/`MAX`, the operators with a bit container on
> either side (so `n * 2` and `1 < n` need no `as`), `@format`,
> `hex`/`binary`, `compare`, `abs`/`unsigned_abs`/`sign`,
> `min`/`max`/`clamp`, `is_power_of_two`/`next_power_of_two`,
> `div_floor`/`div_ceil`/`div_euclid`/`rem_euclid`/`div_rem`/`gcd`/`lcm`,
> `pow`/`isqrt`/`ilog2`/`ilog10`, bit counts/rotates/byte swaps/endian
> conversions, the four overflow families (`checked_*`, `wrapping_*`,
> `saturating_*`, `overflowing_*` for `add`/`subtract`/`multiply`/
> `negate`/`pow`), and `widening_multiply`/`carrying_add`/
> `borrowing_subtract` (`self` by value, `=>` where one intrinsic does
> it).
> `+`, `-`, `*` and negation panic on overflow in Debug builds
> (`attempt to add with overflow`, …) and wrap in Release; `/` and `%`
> panic on a zero divisor (and `MIN / -1`) in every mode. Floats
> (`Float16`, `Float32`, `Float64`) have their arithmetic and
> comparisons with an `@fbitW` on either side, mathematical and format
> constants (`PI`, `TAU`, `E`, `SQRT_2`, `INFINITY`, `NAN`, `EPSILON`,
> `MIN_POSITIVE`, `MIN_SUBNORMAL`, `MAX`, `MIN`, `MANTISSA_BITS`,
> `EXPONENT_BITS`), classification (`is_nan`, `is_infinite`, `is_finite`,
> `is_normal`, `is_subnormal`, `is_zero`, `sign_bit`, `is_sign_negative`,
> `is_sign_positive`, `classify` → `FloatClass`), exact operations
> (`abs`, `copy_sign`, `floor`, `ceil`, `trunc`, `round`, `round_even`,
> `fract`, `sqrt`, `multiply_add`, `min`, `max`, `min_number`,
> `max_number`, `clamp`, `recip`, `to_radians`, `to_degrees`), the
> elementary functions (§3.8), bit/ULP
> operations (`to_bits`, `from_bits`, `hex`, `binary`, `next_up`,
> `next_down`, `ulp`, `exponent`, `mantissa`, `scale_by_power_of_two`,
> `split_exponent`), comparisons (`equals`, `approximately_equals`,
> `total_compare`, `compare`), and interpolation (`lerp`, `inverse_lerp`,
> `remap`, `smooth_step`). `as` is an `@conversion` only where every
> value converts exactly: widening of one signedness, unsigned into a
> wider signed type, an integer into a float whose significand holds it,
> and a narrower float into a wider one. Everything else is a method
> that says what it does: `to_X() value: X else nothing` (fails out of
> range, and for a float on a fraction or NaN), `to_X_wrapping()` (the
> low bits), a float's `to_X_truncated()` (toward zero, saturating, NaN
> is 0) and `to_floatN_rounded()` (to nearest, ties to even).

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
| `Formatter` | Where formatted text goes: a structure holding a `target: ref unknown` and a `sink: FormatSink`. `Formatter.over(target, sink)` makes one; `write(text: Format)` writes a format (an f-string, a literal or one formattable value), `write_bytes(bytes)` raw bytes. |
| `Format` | `alias @format_args(Formatter)`: the type of an f-string written into a `Formatter`. |
| `FormatSink` | `alias function (target: ref unknown, bytes: Slice[Byte]) nothing`: what a consumer hands a `Formatter`. |
| `Hex`, `Binary` | A number's bits in base 16 (lowercase) or 2, no prefix, at the type's width: `{n.hex()}`, `{n.binary()}`. |
| `Fixed`, `Scientific` | A float formatted with `precision` digits after the decimal point (ties rounded to even), fixed-point or scientific: `{x.fixed(3)}`, `{x.scientific(4)}`. |
| `Padded` | `pad_left(ref f, width)`, `pad_right` and `pad_center` (an odd space goes after) pad a `Format` with spaces to `width` code points; a counting `Formatter` measures it first. |

A type is formattable when it has a `@format` hook for `Formatter`, found
by exact signature (`SPEC.md` §8):

```lilac
Point: structure {
    x: Int
    y: Int

    @format: function (self, out: ref Formatter) nothing {
        out.write($"({self.x}, {self.y})")
    }
}
```

`Standard` defines `@format` for `@bit8`…`@bit64` and `@bit_native`
(unsigned decimal), `Int`/`Int8`…`Int64` (signed decimal),
`Uint`/`Uint8`…`Uint64` (unsigned decimal), `@fbit16`/`@fbit32`/`@fbit64`
and `Float16`/`Float32`/`Float64` (shortest round-trip decimal), `Bool`
(`true`/`false`), `@byte_buffer` (its bytes), `Format` (a nested format),
`Ordering`, `FloatClass`, `ParseError` and `Optional[T]` / `Result[T, E]` (`Some(v)`, `None`, `Ok(v)`,
`Error(e)`: their hooks capture the payload through `self`, so they work
when the payload types copy and have a `@format`, checked where they are
used). Every numeric type also provides `parse(text: @byte_buffer) value: T else error: ParseError`. A character literal
is a `@bit32`, so `{'A'}` prints `65` until `Standard` has a `Char` type.

A consumer makes a `Formatter` over its own storage: `Io.print` over
standard output's buffer, `Runtime`'s `@compiler_print` over standard
error (so `@panic($"index {i} past {n}")` formats), and the counting
formatter `Padded` uses. N types and M consumers need N hooks and M sinks.

### 3.2 `Standard.Memory`

> **Today:** `Standard.Heap.Allocator` is any allocator as a handle
> (`Allocator.from[T]`; `PageAllocator`, `ArenaAllocator`,
> `GeneralAllocator`, `CAllocator` with `--libc`). The handle only
> forwards: each allocator checks its own sizes and alignments, answers
> a zero size with an aligned address, and panics when out of memory, so
> a returned reference is always live. `Heap.default_allocator` is an
> `Optional[Allocator]`
> that `Standard.Runtime`'s `@entry` sets to a general allocator
> (`get_default_allocator()` reads it). The handle has
> `create[T](value) ref T` (allocate, then `@write` the value in),
> `destroy[T](ref T)` (`@drop`, then free) and `free[T](ref T)` (free a
> slot whose value was moved out with `@read`). Arrays of `T` are
> `Slice[T]`s: `allocate[T](count)` (zeroed), `reallocate[T](memory,
> new_count)` and `deallocate[T](memory)` take the whole slice, never a
> count beside an address. `Box[T]` (in `Standard`)
> is the owned heap box: `new(value)` (`new_using(value, allocator)` for
> another allocator), `into_inner() T` (by `@discard`
> and `@read`), and its drop drops the value and frees it. The inventory
> below is the target design.

**Allocators:**

| Type | Purpose | Key methods |
|---|---|---|
| `Allocator` | structural convention (§2) | `alloc(size, align)`, `free(ptr, size, align)`, `realloc(ptr, old, new, align)`, `alloc_type[T]() Unique[T]`, `alloc_slice[T](count) Slice[T]` |
| `SystemAllocator` | malloc/free-backed; **the process default** | implements `Allocator`; `global() ref Allocator` |
| `Arena` | bump/region allocator; bulk free via `reset` | `new(backing)`, `alloc`, `reset`, `reset_retain`, `used`, `capacity` |
| `Pool[T]` | fixed-size structure freelist (nice-to-have) | `alloc`, `free`, `reserve` |

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

**Helpers:** `defer`/guard structures for RAII scopes; `align_up`; `mem_copy`,
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
| `ParseError` | `either { Empty; InvalidDigit; Overflow; Trailing }` | |
| `Utf8Error` | `either { InvalidStart; InvalidContinuation; Truncated }` | |

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

> **Today:** `Standard.Collections.List[T]` owns its elements in one
> allocation from an `Allocator` (`new`, `new_using`): `add`,
> `add_slice` (copies, through `@on_copy`), `insert`, `get` (a `ref T`),
> `pop() Optional[T]`, `remove`, `remove_range`, `reserve`, `trim`,
> `get_count`, `get_capacity`; its drop drops each element and frees
> the memory, and its copy copies each element (through `@on_copy`)
> into new memory from the same allocator, so `T` must be copyable
> (`E042` otherwise; a move-only value needs a copying wrapper, such as
> a future reference-counted `Arc`). `Box[T]` stays move-only: it is
> the wrapper for a single owner. A `ref T` a call returns is
> assignable, so `list.get(i) = v` and `list.get(i) += 1` write in
> place. In `Standard` itself, `Slice[T]` is an
> unowned view (`new(ptr, count)`, `get`, `slice(range)`, `base`,
> `copy_to`, which copies through `@on_copy`, and `move_to`, which moves
> bytes and may overlap), and `Range` is a half-open `Int` range
> (`of(from, to)`, either order). The inventory below is the target
> design.

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
* Adapters (generic structures/functions): `map`, `filter`, `filter_map`, `fold`
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

Bytes in and out: two handle types any byte source or sink plugs into,
buffers over them, files, memory, the standard streams, and one-call
text functions on top. Text is a `Format` (an f-string, a literal or one
formattable value, §3.1a). Every failure is an `Io.Error` (an alias of
`Os.Error`: `.NotFound`, `.PermissionDenied`, `.InvalidData`,
`.WriteZero`, `.Interrupted`, ..., `.Other(code)` for a code with no name).

**Handles** (`writer.lilac`, `reader.lilac`):

| Type | Purpose | Key methods |
|---|---|---|
| `Writer` | anything bytes go to, as a target address and a table of its functions (`WriterTable[T]`: `write`, `flush`) | `from[T]` (the type's `WRITER_TABLE`), `over[T](target, table)`, `write_bytes`, `write(Format)`, `write_line`, `flush` |
| `Reader` | anything bytes come from (`ReaderTable[T]`: `read`) | `from[T]` (`READER_TABLE`), `over[T]`, `read(into) count`, `read_all(ref List[Byte])`, `read_all_text(ref String)` (`.InvalidData`, string unchanged, when not UTF-8) |

A type becomes a writer or reader by keeping its table as a constant
global and naming it; the handle forwards to it. Table functions return
a `Result` (a function value cannot fail), and the handle's methods turn
it into a failing call. `write_bytes` writes everything (retrying
`.Interrupted`, `.WriteZero` on a zero write), and `write` formats
through a formatter that keeps the first error: a `@format` hook cannot
fail, so the format runs to its end and the call then fails.

**Buffers** (`buffered.lilac`, `reader.lilac`):

* `BufferedWriter.over(writer, lines)`: holds `BUFFER_COUNT` (8 KiB)
  bytes and passes them on when full, at `flush` and, in line mode, at
  each line end. It does not flush when dropped.
* `BufferedReader.over(reader)`: reads 8 KiB at a time; `read_line(ref
  String)` appends one line without its `\n` (or `\r\n`), validated
  whole (`.InvalidData` and the string unchanged when not UTF-8),
  `read_line_lossy` repairs bad UTF-8 to U+FFFD; both fail with `.None`
  at the end of the input. `read` and `reader()` read through the
  buffer.

**Files** (`file.lilac`): `File` is a system handle and whether it owns
it. `open`, `create`, `open_with(path, OpenMode)` (`.Read`, `.Write`,
`.Append`, `.ReadWrite`; the path a `Format` of at most
`MAX_PATH_BYTES`) give owned files, closed when dropped or by
`close`; `standard_output`/`_input`/`_error` give the streams, not
owned. `read`, `write_bytes`, `write`, `write_line`, `seek(offset,
SeekFrom)`, `size`, `is_terminal`, `reader()`, `writer()`. Written to a
Windows console, UTF-8 is shown as UTF-16 (bad sequences as U+FFFD), so
text shows the same in any code page.

**Memory** (`memory.lilac`): `ByteListWriter.over(ref List[Byte])`
appends raw bytes; `StringWriter.over(ref String)` appends text and
keeps the string valid UTF-8 (a scalar split between writes is joined,
bad bytes become U+FFFD); `ByteReader.over(Slice[Byte])` reads from bytes. Each points
at what it was made over, so keep it in place while its handle is used.

**Standard streams** (`standard.lilac`, `input.lilac`):

* `standard_output()`: one `BufferedWriter` shared by every thread
  behind a `Sync.ReentrantMutex` (a `@format` hook may print), set up
  on first use, in line mode on a terminal. `flush_standard()` writes
  it out; `Standard.Runtime`'s `@exit` and `Process.exit` call it.
* `standard_error()`: unbuffered, a system call per write.
* `standard_input()`: one buffer behind a lock; each read flushes
  standard output first, so a prompt shows. `read_line`,
  `read_line_lossy` and `reader()` as `BufferedReader`'s. A Windows
  console is read as UTF-16 and converted (a lone surrogate becomes
  U+FFFD; Ctrl+Z at a line start ends the input).

**Text functions** (`io.lilac`): `print`, `print_line`, `print_error`,
`print_error_line` (each one write, flushed before it returns; an error
is dropped, as `printf` does), `read_line() String else nothing` (lossy;
fails at the end of input), `ask(prompt) String else nothing` (prints,
then reads a line), and the whole-file calls `read_file` (the text,
`.InvalidData` when not UTF-8), `write_file`, `append_file`,
`remove_file`.

`Standard.Process.exit` and `Standard.Heap.Allocator` likewise sit on
`Standard.Os` (`exit`, `map_pages`), and `Standard.Runtime` defines the
platform hooks (SPEC §8) over it; its `@compiler_print` takes a
`Format` and writes it to standard error through `print_error`.

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
  `seek(handle, offset, SeekFrom) Result[@bit64, Error]`, `size`,
  `remove_file(path) Optional[Error]`.
* **Memory:** `allocate(size)` (zeroed) and `reallocate(memory, size)`
  (both `Result[ref unknown, Error]`; a failed `reallocate` leaves
  `memory` as it was), `deallocate(memory)`; `map_pages(size)
  Result[ref unknown, Error]` and `unmap_pages(memory, size)
  Optional[Error]` (always the OS), `page_size()`.
* **Process:** `exit(code)`; `argument_count`, `argument_vector`,
  `environment_vector` (stored by `@entry`);
  `environment_variable(name, buffer, capacity) Optional[@bit_native]`
  (copies a variable's value into `buffer` and gives its length, past
  `capacity` when it does not fit; `.None` when unset).
* **Time:** `monotonic_nanos()`, `wall_clock_nanos()`, `sleep(nanos)`.
* **Random:** `fill_random(buffer, length)`.
* **Threads:** `spawn(entry, argument) Result[Thread, Error]` runs
  `entry(argument)` (a `function (argument: ref unknown) nothing`) on a new
  thread, `join(thread) Optional[Error]` waits for it and frees it,
  `yield_thread()`, `available_parallelism()` (at least 1). Every thread's
  stack is `default_stack_size` (8 MiB on 64-bit targets, 2 MiB on 32-bit,
  reserved rather than committed) on every OS, or a size given to
  `spawn_with(entry, argument, stack_size)`, with a guard below it so an
  overflow crashes. Windows uses `CreateThread`, macOS `pthread_create`,
  and native Linux its own `clone` thread (`@linux_thread_clone`) on a
  mapping holding a guard, its stack and its thread-local block. Each starts in the
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
| `OpenMode` | `either { Read; Write; Append; ReadWrite }` + create/truncate flags | |
| `Path` | path value | `new(s)`, `join(part)`, `parent() Option[Path]`, `file_name() Option[String]`, `extension() Option[String]`, `is_absolute`, `normalize`, `to_string` |
| `Directory` | directory ops | `create`, `remove`, `exists`, `list() Result[List[Path], IoError]` |
| `Reader` | structural read shape | `read(buf) Result[@bit_native, IoError]`, `read_u8`, `read_line() Result[Option[String], IoError]`, `read_all` |
| `Writer` | structural write shape | `write(bytes)`, `write_u8`, `write_str`, `flush` |
| `BufferedReader[R]` | buffered wrapper | over any `Reader`; `read_line`, `read_until` |
| `BufferedWriter[W]` | buffered wrapper | over any `Writer`; `flush` |
| `Stdin` / `Stdout` / `Stderr` | standard streams | `print`, `println`, `eprint`, `eprintln`, `read_line`, `write`, `flush`; `@on_drop` flushes |
| `IoError` | `either { NotFound; PermissionDenied; AlreadyExists; IsADirectory; NotADirectory; Interrupted; UnexpectedEof; Other: @bit32 }` | `message() String` |

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
| `ThreadError` | `either { SpawnFailed; Poisoned }` | |

Atomics live in `Standard.C` (`Atomic[T]`); `Arc[T]` in `Standard.Memory`
becomes fully thread-safe through them.

---

### 3.8 Numeric methods and `Standard.Math`

> **Today:** the numeric types' methods (math plan phases 1-4) and float
> printing and parsing (phase 6) and the elementary functions (phase 5)
> are implemented. `Random` (phase 7) and the rest of `Standard.Math`
> (phases 9-10) are planned (`docs/math-plan.md`).

Math is methods on the numeric types (§3.1), not free functions: Lilac
has no overloading and no type-argument inference, so `x.sqrt()`,
`n.count_ones()` and `Float32.PI` serve every width and show up in `.`
completion. `Standard.Math` holds only what is not one scalar's method.
Nothing links libm or calls C: each operation is an LLVM intrinsic, an
aarch64 instruction or an internal routine the backend writes, and gives
the same bits on every target in every build mode.

* **Integer** (`Int*`, `Uint*`): `abs`, `unsigned_abs`, `sign`, `min`,
  `max`, `clamp`, `compare`, `pow`, `isqrt`, `ilog2`, `ilog10`, `gcd`,
  `lcm`, `div_rem`, `div_floor`, `div_ceil`, `div_euclid`, `rem_euclid`,
  `is_power_of_two`, `next_power_of_two`; the bit toolkit (`count_ones`,
  `leading_zeros`, `trailing_zeros`, `rotate_left`, `swap_bytes`,
  `reverse_bits`, endian conversions); `widening_multiply`,
  `carrying_add`, `borrowing_subtract`; and the overflow families
  `checked_*` (fail), `wrapping_*`, `saturating_*` and `overflowing_*`
  for `add`/`subtract`/`multiply`/`negate`/`pow`. `+ - *` panic on
  overflow in Debug and wrap in Release.
* **Float** (`Float16/32/64`): `abs`, `min`/`max` (and the IEEE
  `min_number`/`max_number`), `clamp`, `copy_sign`, `sqrt`, `recip`,
  `multiply_add` (fused, one rounding), `floor`, `ceil`, `round`,
  `round_even`, `trunc`, `fract`; classification (`classify` →
  `FloatClass`, `is_nan`, `is_finite`, `is_subnormal`, …); bits and ULPs
  (`to_bits`, `from_bits`, `next_up`, `next_down`, `ulp`, `exponent`,
  `mantissa`, `scale_by_power_of_two`); `compare`, `total_compare`,
  `approximately_equals`; `lerp`, `inverse_lerp`, `remap`, `smooth_step`,
  `to_degrees`, `to_radians`. `%` is the exact `fmod` remainder.
* **Constants** (per width): `PI`, `TAU`, `E`, `SQRT_2`, `LN_2`, `LN_10`,
  `LOG2_E`, `LOG10_E`, `INFINITY`, `NAN`, `EPSILON`, `MIN_POSITIVE`,
  `MIN_SUBNORMAL`, `MAX`, `MIN`; integers have `MIN`/`MAX`.
* **Printing and parsing:** `{x}` is the shortest decimal that reads back
  to the same bits, `{x.fixed(p)}`/`{x.scientific(p)}` round the exact
  binary value (ties to even) to any precision, and every numeric type
  has `parse(text) value: T else error: ParseError`, correctly rounded
  for floats (§3.1a; `lib/Standard/float_format.lilac`). They are tested
  bit for bit against Rust (`numeric_differential.rs`,
  `float_format_differential.rs`).
* **Numeric conversions:** `as` only where exact; otherwise `to_X()`
  (fails), `to_X_wrapping()`, `to_X_truncated()` and
  `to_floatN_rounded()`.
* **Elementary functions** (`Float16/32/64`, math plan phase 5, in
  Lilac on double-double kernels; no libm): `exp`, `exp2`, `exp10`,
  `exp_m1`, `ln`, `log2`, `log10`, `ln_1p`, `log(base)`; `pow`,
  `pow_int(n)`, `cbrt`, `hypot`, `root(n)`; `sin`, `cos`, `tan`,
  `sin_cos`, `sin_pi`, `cos_pi`, `tan_pi` (any finite argument, reduced
  exactly by Payne-Hanek), `asin`, `acos`, `atan`, `atan2`; `sinh`,
  `cosh`, `tanh`, `asinh`, `acosh`, `atanh`; `erf`, `erfc`, `gamma`,
  `ln_gamma`. Domain errors return NaN and overflow returns an infinity
  (no errno), with C Annex F's special values. Each `///` states its
  accuracy: within 0.5 ulp plus 2^-37 ulp for `Float64` (correctly
  rounded unless the exact value is that close to a rounding boundary)
  and plus 2^-60 ulp for `Float32`/`Float16`. The reference tables are
  in `tools/math-reference/`.
* **Planned — `Standard.Math`:** `Random` (phase 7: no global state, seeded
  or from `Os.fill_random`), linear algebra (phase 9) and
  SIMD types (phase 10).

---

### 3.9 `Standard.Diagnostics`

| Type | Purpose | Key methods |
|---|---|---|
| `Diagnostic` | structured message | `error(code, msg)`, `warning`, `note`, `help`, `with_label(span, text)`, `render() String` |
| `Level` | `either { Error; Warning; Note; Help }` | |
| `Span` | `{ lo; hi; file }` source location | `merge`, `contains` |
| `SourceMap` | file id ↔ name registry | `add(path)`, `name(id)`, `lookup(span)` |
| `Color` | ANSI styling | `red`, `green`, `bold`, `reset`, `enabled` (tty detection) |
| `EmitMode` | `either { Human; Json }` | |

**Functions:** `format(spec, args)` (format specifiers beyond f-strings),
`pretty_print(value)` (over `@format`), `write_diagnostic(w, d)`,
`trace`/`debug`/`info` logging to stderr with levels.

---

### 3.10 `Standard.Testing`

The library side of `test "name" { … }` blocks (SPEC §8, "Tests").
`Standard.Runtime`'s `@test_entry` calls `run`; a test fails by panicking.

| Function | Purpose |
|---|---|
| `run()` | runs the test `LILAC_TEST` names by index, else every test in order |
| `expect(condition)` | fails unless `condition` holds |
| `expect_with(condition, message)` | the same, with a `Format` message |
| `expect_equal[T](actual, expected)` | fails unless `actual == expected`, showing both (`T` needs an `@format`) |
| `expect_text(actual, expected)` | fails unless the `Format` `actual` writes exactly the bytes `expected`; compares as it writes, allocating nothing |

`Standard`'s own tests are the project `tests/standard` (`lilac test
tests/standard`).

---|---|---|
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

---

## 6. Non-goals / deferred

* **Traits/protocols/concepts** — abstraction stays structural; a first-class
  feature is deferred (revisit after the stdlib proves the shapes).
* **Async** — futures/executors are structures over `loop` + `match`; `await` is
  deferred (`SPEC.md` §9). Not in this layout yet.
* **Networking, JSON, regex, compression** — Tier-3 extras, later.
* **A `Standard` umbrella that re-exports children** — impossible without
  re-export; the root is a real prelude instead.
* ~~**Floating-point formatting edge cases**~~ — done (math plan phase 6;
  §3.1a, §3.8): shortest round-trip output, `fixed`/`scientific` at any
  precision and correctly rounded `parse`, in Lilac with no libm and no heap.
