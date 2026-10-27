# String implementation guide

How to build `docs/string-plan.md`, step by step. The plan says *what*;
this says *how*: the exact layouts, what each function does, and the
algorithms for the hard ones. Read the plan first for the reasons.

The I/O guide (`docs/io-implementation.md`) is the other half. The two
interleave, so start with the build order below.

## Build order across both plans

| Step | Work | Needs |
|---|---|---|
| 1 | String phase 0: bug fixes, `@eql`, `hash` | - |
| 2 | String phase 1: UTF-8 core, `Scalar`, the invariant | 1 |
| 3 | I/O phase 1: `Writer`, the formatter adapter, memory writers | 2 |
| 4 | I/O phase 2: buffered standard output | 3 |
| 5 | I/O phase 3: `File` | 3 |
| 6 | String phase 2: storage helpers, indices, byte and scalar views | 2 |
| 7 | I/O phase 4: reading, `read_line` | 5, 6 |
| 8 | String phase 3: Unicode tables, characters | 6 |
| 9 | String phase 4: search, split, trim, join | 8 |
| 10 | I/O phase 5: cleanup | 7 |
| 11 | String phases 5 to 8: normalization, case, UTF-16, speed | 9 |

Step 2 comes before I/O so the I/O code never has to be rewritten for
validation. Step 6 can move earlier or later freely: everything outside
`string.lilac` reaches `String` through its public methods, so the layout
change touches no caller.

The code blocks are sketches of the shape and the steps. Check the exact
syntax against existing `Standard` code as you write it.

Every step ends with the gates green (`cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`), `STDLIB.md` updated, and new library tests in
`tests/standard`.

## Things that will bite

Read these before writing code. Each one has caught a real design here.

1. **A value holding its own address breaks when it moves.** A function
   that returns a structure whose field points into the same structure
   (a `Formatter` over its own `Catch`, a gathered `@byte_buffer` into its
   own inline `String`) returns a dangling pointer, because returning
   copies the bytes to a new place. Return the owner, then take the
   pointer from it in place (`catch.formatter()`, `gathered.bytes()`).
   `Io.Line` already does this.
2. **`s.add(s)` reads the string while growing it.** `String`'s `@format`
   hands the sink a pointer into the string's own bytes. If `add` grows
   the string while appending, that pointer dangles. Every append path
   must handle a source inside its own storage (see "Appending" below).
3. **A zeroed `Optional` is `.Some`.** Set `.None` explicitly (`Os.write_all`
   notes this).
4. **Function values cannot fail** (`E057`, checked). A table of functions
   (`WriterTable`, `ReaderTable`) returns `Result`/`Optional`, and only the
   public methods over it use `else` outs.
5. **Every method call borrows its receiver for the rest of the scope.**
   Writing `s.get_count()` and then passing `s` by value later copies it.
   Inside `Standard`, prefer `ref` parameters and `@move` where a value is
   finished.
6. **A raw `@byte_buffer` from a `String` is valid only until the next
   edit.** Never keep one across a call that can append.

## Step 1: String phase 0 (bug fixes)

### Byte helpers (`lib/Standard/Memory/bytes.lilac`)

String and I/O both need these, so they live in `Memory`:

```lilac
/// Whether `a` and `b` hold the same bytes.
equal_bytes: function (a: @byte_buffer, b: @byte_buffer) Bool
/// Less, equal or greater, byte by byte; a prefix orders first.
compare_bytes: function (a: @byte_buffer, b: @byte_buffer) Ordering
/// The first `byte` in `bytes` at or after `from`.
find_byte: function (bytes: @byte_buffer, byte: @bit8, from: Int) at: Int else nothing
```

Write them as plain `@load_u8` loops first. Step 11 makes them
word-at-a-time. There is no user `@memcmp` (the backend has the leaf,
but it is not in `Intrinsic::PLAIN`); don't call C's `memcmp` either,
since a bare target may not have it.

### Fixes in `string.lilac`

- A private `bytes(self) @byte_buffer`: the inline array and its count, or
  the heap list's slice. Everything below reads through it.
- `@eql: function (lhs: ref String, rhs: ref String) Bool => equal_bytes(lhs.bytes(), rhs.bytes())`.
  Add `(ref String, @byte_buffer)` and `(@byte_buffer, ref String)` so
  `s == "yes"` works. Each `@eql` overload is keyed by the operand types
  alone.
- `hash: function (self) @bit64`: `Wyhash.hash(0x0, slice)` over the bytes.
  `Map` calls `key.hash()`. A fixed seed keeps hashes reproducible; a
  random per-process seed (`Os.fill_random` in `@entry`) protects against
  collision attacks and can come later without changing callers.
- `new_using(format, allocator)` starts as `.Heap(StringBuffer.new_using(allocator))`,
  so the allocator is kept. An inline string has no room for one, so a
  plain `new` spilling to the heap uses the default allocator, and that
  stays correct.

### Tests (`tests/standard/src/Strings/basics/`)

- 22, 23 and 24 bytes (the inline limit is 23), and appending across it.
- Copy a heap string, change the copy, check that the original is unchanged.
- `==` on two heap strings with the same text (the bug).
- `Map[String, Int]`: add, look up through a different `String` with the
  same text, remove.
- `new_using` with an `ArenaAllocator`: the arena's used count grows
  (`tests/standard/src/Heap/arena_allocator` shows how to read it).

## Step 2: String phase 1 (UTF-8 core)

### Files

| File | Contents |
|---|---|
| `lib/Standard/Unicode/utf8.lilac` | `decode`, `decode_valid`, `encode`, `validate`, `sequence_length`, the repair loop |
| `lib/Standard/Unicode/errors.lilac` | `Utf8Error`, `Utf8ErrorKind` |
| `lib/Standard/scalar.lilac` | `Scalar` |

### The well-formed table (Unicode §3.9, Table 3-7)

Everything in this step follows from this table. Byte 1 is the lead
byte; byte 2's range depends on it; later bytes are always `80..BF`.

| Lead byte | Length | Byte 2 | Bytes 3, 4 |
|---|---|---|---|
| `00..7F` | 1 | - | - |
| `C2..DF` | 2 | `80..BF` | - |
| `E0` | 3 | `A0..BF` | `80..BF` |
| `E1..EC`, `EE..EF` | 3 | `80..BF` | `80..BF` |
| `ED` | 3 | `80..9F` | `80..BF` |
| `F0` | 4 | `90..BF` | `80..BF` |
| `F1..F3` | 4 | `80..BF` | `80..BF` |
| `F4` | 4 | `80..8F` | `80..BF` |

Any other lead byte (`80..C1`, `F5..FF`) is invalid on its own.

### `Utf8Error`

```lilac
Utf8ErrorKind: either {
    /// A sequence stops before its last byte: the input ends, or a byte
    /// that is not a continuation byte comes too early.
    Truncated
    /// `C0`, `C1`, or `E0`/`F0` with a too-small second byte.
    Overlong
    /// `ED A0..BF`: a UTF-16 surrogate.
    Surrogate
    /// `F4 90..`, or a lead byte `F5..FF`: past U+10FFFF.
    TooLarge
    /// A continuation byte (`80..BF`) where a sequence should start.
    UnexpectedContinuation
}

Utf8Error: structure {
    /// The byte offset where the bad sequence starts.
    offset: Int
    kind: Utf8ErrorKind
}
```

Give both a `@format` ("invalid UTF-8 at byte 7: surrogate").

### `decode`: the one decoder

```lilac
/// The scalar at `at` and its length in bytes; fails with the length of
/// the maximal invalid subpart (1 to 3 bytes) and why.
decode: function (bytes: @byte_buffer, at: @bit_native) scalar: @bit32, length: @bit_native else bad: @bit_native, kind: Utf8ErrorKind
```

Algorithm:

```text
lead = byte[at]
if lead < 0x80:                       return (lead, 1)
if lead < 0xC0:                       fail (1, UnexpectedContinuation)
if lead < 0xC2:                       fail (1, Overlong)
if lead < 0xE0: need = 1, value = lead & 0x1F, low = 0x80, high = 0xBF
elif lead < 0xF0: need = 2, value = lead & 0x0F
    low, high = (A0, BF) for E0, (80, 9F) for ED, else (80, BF)
elif lead < 0xF5: need = 3, value = lead & 0x07
    low, high = (90, BF) for F0, (80, 8F) for F4, else (80, BF)
else:                                  fail (1, TooLarge)

for k in 1 ..= need:
    if at + k == length of bytes:      fail (k, Truncated)
    b = byte[at + k]
    lo, hi = (low, high) if k == 1 else (0x80, 0xBF)
    if b < lo or b > hi:
        if k == 1 and 0x80 <= b <= 0xBF:     // a continuation byte, just out of range
            kind = Overlong for E0/F0, Surrogate for ED, TooLarge for F4
        else:
            kind = Truncated
        fail (k, kind)
    value = (value << 6) | (b & 0x3F)
return (value, need + 1)
```

**Why fail with `k`:** the bytes before the failing one are the *maximal
subpart*, and they become one U+FFFD. The failing byte is not consumed;
the next decode starts on it. That is exactly the Unicode/WHATWG
replacement practice, so `E0 80` gives two U+FFFDs (`E0` alone, then `80`
alone), and `F1 80 80 41` gives one U+FFFD and then `A`.

`decode_valid(bytes, at)` is the same without the checks, for text already
known valid (every `String`): length from the lead byte, then fold the
continuation bytes. All views use it.

### `validate`: with an ASCII fast path

```lilac
validate: function (bytes: @byte_buffer) nothing else error: Utf8Error
```

```text
at = 0
loop:
    // 1. byte by byte until the address is word-aligned
    while at < n and address(at) % W != 0 and byte[at] < 0x80: at += 1
    // 2. a word at a time while every byte is ASCII
    while at + W <= n and word_at(at) & HIGH == 0: at += W
    // 3. byte by byte through anything else
    if at == n: return
    if byte[at] < 0x80: at += 1; continue
    decode(bytes, at) -> length: at += length
        else -> bad, kind: fail Utf8Error { offset = at, kind = kind }
```

- `W` is `@builtin.Target.pointer_size`, and
  `HIGH: @bit_native => @i_mul_native(@i_div_u_native(@bit_not_native(0x0), 0xFF), 0x80)`
  (`0x8080…80` at the native width).
- Read a word only at an aligned address:
  `@ptr_add(base, at) as ref @bit_native`. Loads carry their type's
  alignment, so an unaligned word read is undefined behaviour. Stage 1
  exists to avoid it.

### The `String` invariant

From this step on, every way bytes enter a `String` makes them valid.
There are four entry points, and all of them go through two private
functions:

- `append_raw(self, bytes)` copies bytes in with no check, and handles a
  source inside its own storage (below).
- `repair_from(self, start)` makes the bytes from `start` to the end valid.

**`repair_from(start)`:**

```text
validate(bytes[start ..])           // the common case: valid, done
    else -> error:
first_bad = start + error.offset    // everything before it is fine
tail = copy of bytes[first_bad ..]  // a stack array of 256 bytes, else a heap List[Uint8]
set count to first_bad
at = 0
run = 0                              // start of the current valid run in `tail`
loop while at < tail length:
    decode(tail, at) -> length: at += length
        else -> bad:
            append_raw(tail[run .. at]); append_raw("\xEF\xBF\xBD"); at += bad; run = at
append_raw(tail[run ..])
```

The text before `start` was valid and ended on a complete scalar, so a
sequence can never straddle `start`, and checking from `start` is enough.

**The entry points:**

| Entry point | What it does |
|---|---|
| `new(text: Format)`, `@conversion` | Empty string, then `add(text)`. |
| `add(text: Format)` | `start = count`; write the format through a private formatter whose sink is `append_raw`; then `repair_from(start)`. Repairing once at the end means a hook that writes `é` one byte at a time is still valid. |
| `add_bytes(bytes) nothing else error: Utf8Error` | `validate`, then `append_raw`. Nothing is appended on failure. |
| `add_bytes_lossy(bytes)` | `start = count`; `append_raw`; `repair_from(start)`. |
| `from_bytes(Slice[Uint8]) String else error: Utf8Error`, `from_bytes_lossy` | A new string through the two above. |

`formatter()` becomes private, because a public formatter could append
without the repair. I/O's string writer uses `add_bytes_lossy` instead
(see the I/O guide).

### Appending when the source is inside the string

`append_raw(self, bytes)`:

```text
p = pointer of bytes, n = length
inside = data <= p < data + capacity          // data = this string's first byte now
offset = p - data   (only if inside)
reserve(count + n)                           // may move the bytes, inline -> heap too
if inside: p = data + offset                 // data is the new place
copy n bytes from p to data + count          // never overlaps: [offset, offset + n) is below count
count += n
```

With today's either layout, `data` is the inline array's address or the
list's slice base. Test it with `s.add(s)` and `s.add($"{s}{s}")` on an
inline string that spills, and on a heap string that reallocates.

### `Scalar` (`scalar.lilac`)

```lilac
/// A Unicode scalar value: a code point that is not a surrogate.
Scalar: structure {
    private { value: @bit32 }

    /// The scalar `value`, or nothing for a surrogate or past U+10FFFF.
    from: function (value: Uint32) scalar: Scalar else nothing
    /// A character literal: `c: Scalar = 'é'`. A constant that is not a
    /// scalar is a compile-time error (E060), since a `=>` body that
    /// reaches a `@panic` with constant arguments is.
    @conversion: function (from: @bit32) Scalar =>
        if is_scalar(from) then Scalar { value = from } else @panic("not a Unicode scalar")
    REPLACEMENT: Scalar => Scalar { value = 0xFFFD }

    value: function (self) Uint32
    utf8_length: function (self) Int        // 1 to 4, from the value's range
    @format: function (self, out: ref Formatter) nothing   // encode into a 4-byte array, write_bytes
    @eql (Scalar, Scalar), (Scalar, @bit32); @less_than and the rest by value
}
```

`is_scalar(v)`: `v < 0xD800 or (0xE000 <= v and v <= 0x10FFFF)`.

`encode(value, into: ref @array(@bit8, 4)) length`:

| Range | Bytes |
|---|---|
| `< 0x80` | `v` |
| `< 0x800` | `C0 \| v>>6`, `80 \| v&3F` |
| `< 0x10000` | `E0 \| v>>12`, `80 \| (v>>6)&3F`, `80 \| v&3F` |
| else | `F0 \| v>>18`, `80 \| (v>>12)&3F`, `80 \| (v>>6)&3F`, `80 \| v&3F` |

### Tests (`Strings/utf8/`)

- **The §3.9 table.** Write each malformed input with its exact U+FFFD
  count, from Unicode's own examples (`61 F1 80 80 E1 80 C2 62 80 63 80
  BF 64` repairs to `a FFFD FFFD FFFD b FFFD c FFFD FFFD d`).
- One test per `Utf8ErrorKind`, with its offset.
- A hook that writes `é` as two `write_bytes` calls gives `é`, not two
  U+FFFDs.
- Round trip: encode, then decode, every boundary scalar (`0`, `7F`, `80`,
  `7FF`, `800`, `D7FF`, `E000`, `FFFF`, `10000`, `10FFFF`).
- The fast path: a 1,000-byte ASCII run with one bad byte at each offset
  0 to 15 (so it lands in each alignment stage) reports the right offset.
- `s.add(s)` across the inline limit and across a reallocation.

## Step 6: String phase 2 (storage, positions, views)

### The layout: keep today's `either`

```lilac
String: structure {
    private { layout: StringLayout }
}
StringLayout: either {
    Inline: StringInline        // first, so a zeroed String is an empty inline one
    Heap: StringBuffer
}
StringInline: structure { buffer: @array(Uint8, 23), count: Uint8 }
StringBuffer: structure { private { list: List[Uint8] } }
```

- 32 bytes on 64-bit targets: the tag and padding, then a 24-byte
  payload. That is half a cache line, and `Inline` is 23 bytes on every
  target. Check `@size_of(String)` in a test so a change shows.
- **An all-zero `String` is valid**: tag 0 is `Inline`, count 0. Code
  that zeroes memory (`List.remove`, `Optional` payloads) produces empty
  strings. Keep `Inline` the first variant.
- **No hand-written hooks.** `List[Uint8]` copies and drops itself, and
  the either's glue runs it, so `String` needs no `@on_copy` or
  `@on_drop`. A copy of a heap string stays on the heap, from the same
  allocator.
- **No stored flags.** There is no spare byte for ASCII/NFC flags: one
  more field makes it 40 bytes. The fast paths scan instead (below). If
  a benchmark later shows the scans matter, the flags can go in a
  header of the heap block, which then replaces the `List`. That is a
  change inside `string.lilac` only.

### The private accessors (write these first; nothing else matches on `layout`)

| Accessor | Inline | Heap |
|---|---|---|
| `count(self) Int` | `inline.count as Int` | `list.get_count()` |
| `data(self) ref @bit8` | `(ref inline.buffer) as ref unknown as ref @bit8` | `list.get_slice().base()`, cast the same way |
| `capacity(self) Int` | `StringInline.SIZE` (23) | `list.get_capacity()` |
| `bytes(self) @byte_buffer` | `@byte_buffer_from(data, count)` | same |
| `set_count(n)` | `inline.count = n` | shrinking: `list.remove_range` of the tail (no `truncate` yet; add one if it is clearer) |

Everything else (`append_raw`, the edits, the views) is written over
these, so it never matches on `layout` itself.

### `reserve(total)`

```text
if total <= capacity(): return
match layout:
    .Inline -> inline:
        list = List[Uint8].new()                       // the default allocator
        list.reserve(max(total, 32))
        list.add_slice(inline's bytes)                 // copy out BEFORE replacing the layout
        layout = .Heap(StringBuffer { list })
    .Heap -> buffer:
        buffer.list.reserve(total)                     // List doubles
```

- `new_using(format, allocator)` starts as `.Heap` over
  `List.new_using(allocator)`, so the allocator is kept. An inline string
  has no room for an allocator, so spilling an inline one uses the
  default allocator.
- **Strings never go back to inline by themselves.** A heap string that
  shrinks keeps its list, as `List` does. `trim_capacity()` moves it back
  inline when it fits.
- Writing into the list's spare capacity needs a raw path: `List` has
  `add_slice` but no "write past the count". Either do every append
  through `add_slice` (the source rebase from step 2 still applies: take
  the offset before `reserve`, then rebase), or add a private-to-Standard
  `List.set_count_unchecked(n)` after a `@memmove`. Prefer `add_slice`
  until an edit needs the other.

### The ASCII scan (replaces the flags)

```lilac
/// Whether every byte is ASCII and none is `
`: then bytes, scalars and
/// characters are one each (`

` is one character).
private simple_ascii: function (bytes: @byte_buffer) Bool
```

The same loop as `validate`'s fast path, with an extra `
` test per
word:

```text
x = word; CR = 0x0D * ONES (ONES = 0x0101…01)
has_high = x & HIGH
y = x ^ CR
has_cr = (y - ONES) & ~y & HIGH        // nonzero when some byte of y is 0
stop when has_high | has_cr != 0
```

It costs a pass over the bytes at word speed. Use it for:
- `get_count` (characters): if `simple_ascii`, the byte count;
- the character-count functions (`prefix(n)` …): the offset is `n`;
- search boundaries (step 9), `uppercase`/`lowercase` (step 11).

The NFC fast path (step 11) likewise runs the quick check each time
rather than reading a flag.

### Positions

```lilac
/// A byte offset into a string, on a scalar boundary. Only `String`
/// makes one; using one checks it again.
StringIndex: structure { private { offset: Int } }
/// `start ..< end`.
StringRange: structure { private { start: StringIndex, end: StringIndex } }
```

`check(self, index)` panics unless `0 <= offset <= count` and (`offset ==
count` or the byte there is not `80..BF`). The message names both, for
example "index 7 is inside a scalar of `naïve`" or "index 12 past the end
of an 8-byte string".

Character boundaries come in step 8. A character operation given an index
inside a character rounds down to that character's start (Swift does the
same), rather than panicking.

### Edits on positions

Each edit is `check`, then one `memmove`.

| Function | Algorithm |
|---|---|
| `insert(at, text: Format)` | Gather `text` into a temporary `String` first (it may be `self`). `reserve(count + n)`; `memmove` `[at, count)` to `at + n`; copy the temporary into `at`; update the count. |
| `remove(range)` | `memmove` `[end, count)` to `start`; `count -= end - start`. |
| `replace_range(range, text)` | Gather; reserve the difference; one `memmove` of the tail to `start + n`; copy in. |
| `slice(range) String` | A new string over those bytes through `append_raw` (they are valid already). |
| `clear()` | `count = 0`, capacity kept. |

### Views (`string_views.lilac`)

```lilac
StringBytes: structure { private { string: ref String, front: Int, back: Int } }
StringScalars: same shape
```

- `next(self) Uint8 else nothing`: if `front == back`, fail. If `back >
  string.count()`, panic "the string changed while a view walked it".
  Otherwise read the byte at `front` and move it on.
- `next_back`: the same from `back`.
- `StringScalars.next`: `decode_valid` at `front`, then move `front` on by
  the length. `next_back`: step `back` back over at most 3 bytes in
  `80..BF`, then `decode_valid` there.
- `get_count` on a view: bytes is `back - front`; for scalars, count
  the bytes that are not `80..BF`.
- `StringBytes.get(i: Int) Uint8`: checked, O(1). `as_byte_buffer()`:
  valid until the next edit (trap 6).

In this step `String.get_count()` is the scalar count, documented as
temporary.

### Tests

- `@size_of(String)` is 32 on 64-bit targets (and whatever 32-bit gives,
  pinned).
- Every edit across the inline/heap boundary both ways, at 0, middle and
  end.
- Each index error message (run tests, `tests/run/string_index_*`).
- A copy of a heap string is equal and independent; dropping both frees
  once (a `GeneralAllocator` Debug build checks double frees).
- A string in `sync_link.rs`'s program (or a sibling `string_link.rs`), so
  it links on the six native targets with no libcall.

## Step 8: String phase 3 (Unicode data and characters)

### The data

- `tools/unicode/ucd/` holds the Unicode Character Database (UCD) files,
  from one pinned version (a `VERSION` file, the Unicode license):
  - `UnicodeData.txt`
  - `GraphemeBreakProperty.txt`
  - `emoji-data.txt` (`Extended_Pictographic`)
  - `DerivedCoreProperties.txt` (`InCB`, `Cased`, `Case_Ignorable`, `Uppercase`, `Lowercase`)
  - `PropList.txt` (`White_Space`)
  - `DerivedNormalizationProps.txt` (`NFC_QC`, `Full_Composition_Exclusion`)
  - `SpecialCasing.txt`, `CaseFolding.txt`
  - and the tests: `GraphemeBreakTest.txt`, `NormalizationTest.txt`
- `python tools/unicode/gen_tables.py lib/Standard/Unicode/tables.lilac`
  writes every table. It uses the standard library only, and its output is
  deterministic. As with `float_tables.lilac`, change the generator and
  rerun it.

### Table form: two-stage lookup

For a property with one value per code point (`0..10FFFF`):

```text
value(cp) = STAGE2[ STAGE1[cp >> SHIFT] << SHIFT  |  cp & MASK ]
```

- The generator cuts the 0x110000 values into blocks of `2^SHIFT`, keeps
  each distinct block once (`STAGE2`), and records a block number per
  block (`STAGE1`). It tries `SHIFT` from 5 to 9 and keeps the smallest
  total size.
- A `STAGE1` entry is one byte when there are at most 256 distinct blocks,
  else two (little-endian).
- Each stage is a `=>` constant of `\xHH` escapes,
  `GRAPHEME_STAGE1: @byte_buffer => "\x00\x01…"`, read with
  `@load_u8(@byte_buffer_ptr(GRAPHEME_STAGE1), i)`.
- The generator also writes the lookup function for each table, so the
  hand-written code calls `grapheme_property(cp)`.
- Write one generic `two_stage(values, entry_bytes)` in the generator and
  use it for every table.
- Measure first: time `lilac check` on a project using `Standard` before and after
  the tables land. A 30 KB table is about 120 KB of source; if checking
  slows noticeably, a byte-embedding intrinsic gets its own plan.

### The grapheme table: one byte per scalar

```text
bits 0-3  Grapheme_Cluster_Break: 0 Other, 1 CR, 2 LF, 3 Control, 4 Extend,
          5 ZWJ, 6 Regional_Indicator, 7 Prepend, 8 SpacingMark,
          9 L, 10 V, 11 T, 12 LV, 13 LVT
bit 4     Extended_Pictographic
bits 5-6  Indic_Conjunct_Break: 0 None, 1 Consonant, 2 Linker, 3 Extend
bit 7     White_Space (trimming uses it in step 9)
```

### Segmentation, forward (`Unicode/grapheme.lilac`)

The forward walk is a small state machine, so it also works on streamed
bytes (the formatter counter below). Use your Unicode version's UAX #29
text as the specification and `GraphemeBreakTest.txt` as the oracle.

```lilac
GraphemeState: structure {
    previous: @bit8   // the previous scalar's property byte; NONE (0xFF) at the start
    regional: @bit8   // 1: the previous scalar is an RI that starts a pair
    emoji: @bit8      // 0 none, 1 after ExtPict Extend*, 2 after ExtPict Extend* ZWJ
    conjunct: @bit8   // 0 none, 1 after Consonant [Extend|Linker]*, 2 the same with a Linker seen
}
```

`breaks(state, next) Bool`, where `p` is `previous`'s break class and `n`
is `next`'s:

```text
if previous == NONE:                          return false   // nothing before the first scalar
if p == CR and n == LF:                       return false   // GB3
if p in {Control, CR, LF}:                    return true    // GB4
if n in {Control, CR, LF}:                    return true    // GB5
if p == L and n in {L, V, LV, LVT}:           return false   // GB6
if p in {LV, V} and n in {V, T}:              return false   // GB7
if p in {LVT, T} and n == T:                  return false   // GB8
if n in {Extend, ZWJ}:                        return false   // GB9
if n == SpacingMark:                          return false   // GB9a
if p == Prepend:                              return false   // GB9b
if conjunct == 2 and incb(next) == Consonant: return false   // GB9c
if emoji == 2 and extpict(next):              return false   // GB11
if p == RI and n == RI and regional == 1:     return false   // GB12, GB13
return true                                                  // GB999
```

Then `advance(state, next)` updates the state:

```text
emoji    = 1 if extpict(next)
           else 1 if emoji == 1 and n == Extend
           else 2 if emoji == 1 and n == ZWJ
           else 0
conjunct = 1 if incb(next) == Consonant
           else 2 if conjunct >= 1 and incb(next) == Linker
           else conjunct if conjunct >= 1 and incb(next) == Extend
           else 0
regional = 1 if n == RI and not (p == RI and regional == 1) else 0
previous = next
```

**Fast path:** if both scalars are ASCII and not `\r` then `\n`, there is
a break. No rule can join two ASCII scalars except GB3.

### Segmentation, backward

`is_boundary(bytes, at) Bool` decides whether there is a character break
between the scalar ending at `at` (`a`) and the one starting there (`b`)
by looking back. It serves `next_back`, `index_before`, and the boundary
checks in search (step 9).

```text
if at == 0 or at == length: return true
a = scalar before at, b = scalar at at
apply GB3 to GB9b to (a, b) exactly as forward          // pairwise rules
GB9c: if incb(b) == Consonant and incb(a) in {Extend, Linker}:
        scan back from a over scalars with incb in {Extend, Linker}, noting any Linker;
        if the scan stops on a Consonant and a Linker was seen: return false
GB11: if a == ZWJ and extpict(b):
        scan back from before a over Extend; if it stops on an ExtPict: return false
GB12/13: if a == RI and b == RI:
        count the RIs in a row ending at a; return (count is even)
return true
```

The previous boundary from `p`: step back one scalar at a time until
`is_boundary`. A long run of flags makes the RI count quadratic in the run
length; if a benchmark shows it, cache the count during the walk.

### `Character` and `StringCharacters`

- `Character: structure { private { text: String } }`. The bytes of one
  grapheme, almost always inline.
- `StringCharacters.next`:

  ```text
  start = front
  state = fresh
  step scalars forward with breaks/advance until a break or back
  return Character over [start, end)
  ```

- `String.get_count()` now counts characters: the byte count when
  `simple_ascii`, else walk.
- Character-count functions find a byte offset by walking `n` characters,
  then reuse the index functions:
  - `prefix(n)`, `suffix(n)` and `dropping_*` clamp a too-large `n` to the
    whole string, as Swift does;
  - `remove_first(n)` and `character_at(i)` panic past the end, with the
    count in the message.
- `reverse()`: walk the characters backward, appending each into a new
  string, then replace `self`.

### Counting characters in a `Format` (`Padded`)

A format is not a string, so `count_characters` runs the forward state
machine over the sink's chunks.

- Its target is a structure holding the count, a `GraphemeState`, and up
  to 3 bytes of a scalar the last chunk split.
- Each chunk:
  1. complete the held scalar;
  2. decode the whole scalars;
  3. hold back an incomplete tail;
  4. count a character at each break.
- At the end, a held tail is one U+FFFD.

### Tests

- `crates/lilac-cli/tests/unicode.rs` reads `GraphemeBreakTest.txt` and
  writes it into a generated test program, as `math.rs` does with its
  tables. The program checks forward and backward segmentation of every
  row, in Debug and Release.
- A property test in `tests/standard`. A seeded generator builds random
  sequences, each scalar drawn from one break class (a fixed sample scalar
  per class). The backward boundaries must equal the forward ones.

## Step 9: String phase 4 (the everyday API)

### `gather`: a `Format` as bytes

```lilac
Gathered: structure {
    private { literal: @byte_buffer, copy: String, in_place: Bool }
    /// The bytes, valid while this `Gathered` stays in place and unchanged.
    bytes: function (self) @byte_buffer => if self.in_place then self.literal else self.copy.bytes()
}
gather: function (text: Format) Gathered
```

- **Fast path:** `@format_args_holes(text) == 0x0` means a plain literal.
  If `validate(@format_args_piece(text, 0))` passes, keep the piece in
  place (it is constant data).
- **Otherwise** `copy = String.new(text)`, which repairs it.
- **Trap 1:** `bytes()` reads `copy` where it is now, so moving the
  `Gathered` is safe. Never store a pointer to `copy`'s bytes in a field.
- A later optimization can skip the copy when the format is one `String`
  hole: add a `Formatter.write_borrowed(bytes)` that a sink may keep until
  the format ends. Do it only after measuring, because `s.contains(s)`
  aliases.

### Searching

`Memory.find_bytes(haystack, needle, from) at: Int else nothing`:

```text
m = needle length; if m == 0: return from
first = needle[0], last = needle[m - 1]
i = from
loop while i + m <= n:
    i = find_byte(haystack, first, i) else fail
    if i + m > n: fail
    if haystack[i + m - 1] == last and equal_bytes(haystack[i ..< i + m], needle): return i
    i += 1
fail
```

The worst case is O(n·m). Step 11 swaps in the Two-Way algorithm only if
a benchmark asks.

`String.find(text)` accepts a byte match only when it starts and ends on a
character boundary. When the haystack is `simple_ascii` every byte offset is one;
otherwise use `is_boundary`. On a rejected match, search on from `i + 1`.
So `"é".contains("e")` is false, and so is `"e\u{301}".contains("e")`:
no match splits a character.

Empty needles: `contains("")`, `starts_with("")` and `ends_with("")` are
true; `find("")` is the empty range at the start; `count_of("")` is 0;
`replace("", x)` replaces nothing and returns 0, so it cannot loop.

`find_last`: run `find_bytes` forward and keep the last match. A backward
searcher is a step 11 speed-up.

### Replacing

`replace(text, replacement) Int`:

1. Gather both.
2. Build the result in a new `String`: for each accepted match, append
   the gap before it and then the replacement.
3. Append the rest, replace `self` with the result, and return the count.

`replace_first` stops after one match. Building a new string keeps every
offset valid and costs one pass.

### Trimming

`Character.is_whitespace` is White_Space of its first scalar (bit 7 of the
grapheme table).

- `trim_end()`: walk characters back from the end while whitespace, then
  `remove` that range.
- `trim_start()`: walk forward, then remove.
- `trim()` does the end first, then the start, so only one `memmove`
  shifts the rest.
- The participles (`trimmed()`, `uppercased()`, ...) copy, then edit the
  copy.

### Splitting

- `split(separator) List[String]`: a new string for each gap between
  accepted matches, empty pieces included (`"a,,b"` gives `a`, ``, `b`).
  An empty separator gives each character.
- `lines()`: split at each `Character.is_newline` character:
  - `\n`, `\r\n`, `\r`, U+000B, U+000C, U+0085, U+2028, U+2029
    (`\r\n` is one character, so it is one break);
  - a final newline adds no empty line (`"a\n\nb\n"` gives `a`, ``, `b`).
- `words()`: split on whitespace characters and drop empty pieces.

### Building

- `join(parts: ref List[String], separator)`:
  1. sum the lengths;
  2. `reserve` once;
  3. append each part, with the separator between parts.
- `repeating(text, count)`: gather once, reserve `n * count`, then append.
- The `@add` hooks take `lhs: String` by value, so a last use moves it in:
  `@add: function (lhs: String, rhs: ref String) String`, whose body
  appends `rhs` to `lhs` and returns it.
  Add the `(String, @byte_buffer)` and `(String, Character)` overloads
  too.

## Step 11: String phases 5 to 8

### Phase 5: normalization (`Unicode/normalize.lilac`)

**Data (generated):**

| Table | Entry |
|---|---|
| `ccc` | two-stage, 1 byte: the canonical combining class |
| `nfc_quick` | two-stage, 1 byte: bits 0-1 `NFC_QC` (0 Yes, 1 Maybe, 2 No) |
| `decomposition` | two-stage, 2 bytes: 0, or 1 + an offset into `DECOMPOSITIONS` |
| `DECOMPOSITIONS` | per entry a count byte, then 3 bytes per scalar: the **full** canonical decomposition, already recursive |
| `COMPOSITIONS` | sorted 9-byte rows `first(3) second(3) composite(3)`, primary composites only (no `Full_Composition_Exclusion`); binary search on `first << 21 \| second` |

Hangul is algorithmic, not tabled:

```text
S = AC00, L = 1100, V = 1161, T = 11A7, VCOUNT = 21, TCOUNT = 28, N = 588, SCOUNT = 11172
decompose s in [S, S + SCOUNT):   i = s - S
    L + i / N,   V + (i % N) / TCOUNT,   and T + i % TCOUNT if i % TCOUNT != 0
compose L' + V' (L <= L' < L + 19, V <= V' < V + 21):   S + ((L' - L) * VCOUNT + (V' - V)) * TCOUNT
compose LV + T' (LV a syllable with (LV - S) % TCOUNT == 0, T < T' < T + 28):   LV + (T' - T)
```

**NFD as a stream.** `Nfd` reads scalars from a string and yields its
NFD, buffering one starter plus its non-starters (a fixed array of 32):

```text
refill:
    take a scalar, push its decomposition (Hangul or table) with each part's ccc
    loop: peek the next scalar; if the first part of its decomposition has ccc != 0,
          take it and push its parts; else stop
    stable insertion sort by ccc over the entries with ccc != 0 (starters stay put)
next: hand out the buffer, refill when empty
```

Bound the run as UAX #15's Stream-Safe Text Format does. After 30
non-starters in a row, end the segment as if a U+034F (ccc 0) were there.
Both sides of a comparison do the same, so equality stays consistent, and
only pathological text sees the difference. Document it.

**NFC as a stream.** Compose NFD's output with one buffer `B`, a starter
position and `last` (the ccc of the last scalar kept after the starter):

```text
on NFD scalar c with class k:
    if there is a starter and not blocked and compose(B[starter], c) exists:
        B[starter] = composite; continue                  // c disappears; last unchanged
        // blocked: (last != 0 and last >= k)  or  (k == 0 and something is after the starter)
    if k == 0: emit all of B; B = [c]; starter = 0; last = 0
    else:      B.push(c); last = k
at the end: emit all of B
```

Starter-plus-starter compositions (Hangul `L V`, and a few like U+0B47
U+0B3E) are why a new starter checks against the held one before
flushing.

**Quick check:**

```text
last = 0
for each scalar: k = ccc;  if last > k and k != 0: no;  if NFC_QC == No: no;  if Maybe: maybe
                 last = k
```

"Yes" takes the byte path. "Maybe" and "no" take the stream.

**Equality, order and hash:**

| Operation | Fast path (both pass the quick check) | Slow path |
|---|---|---|
| `==` | `equal_bytes` | compare the two NFC streams scalar by scalar |
| `compare`, `<` | `compare_bytes` (UTF-8 byte order is scalar order) | compare the NFC streams |
| `hash` | Wyhash over the bytes | encode the NFC stream into a 64-byte stack chunk, `Wyhash.update` per chunk |

- Run the quick check on both sides first; almost all text passes it, and
  a pure-ASCII string passes at word speed (`simple_ascii` implies NFC).
  The quick check is one O(n) pass, the same order as the comparison.
- The slow and fast paths agree because NFC is unique: equal NFC means
  canonically equal.
- **Check first:** that `Wyhash` fed in chunks (`new`, `update`, `final`)
  equals `Wyhash.hash` over the same bytes. Add a test, or a
  `Map[String, …]` lookup will miss.

Search under normalization:

- A needle that is not NFC is normalized once, into a temporary.
- If the haystack passes the quick check, step 9's byte search is exact.
- Otherwise compare character by character, using `Character ==`, at
  each boundary. It is slow, but correct and rare.

### Phase 6: properties and case

- `GeneralCategory` (30 variants) comes from a two-stage byte table.
  `is_letter` and the other tests are its groups (L, N, P, S), taken from
  a character's first scalar, as Swift does.
- Case tables are two-stage, 2 bytes each: 0, or 1 + an offset into a
  mapping list (a count, then 3 bytes per scalar). There is one table each
  for upper, lower and fold. Each holds `SpecialCasing`'s unconditional
  mappings, else `UnicodeData`'s simple ones.
- `uppercase()` / `lowercase()`:
  - When `simple_ascii`, flip `a-z` / `A-Z` in place.
  - Otherwise build a new string scalar by scalar from the mapping, then
    replace `self`.
  - Final sigma: U+03A3 lowercases to U+03C2 when a `Cased` scalar comes
    before it and none after it, both skipping `Case_Ignorable` scalars.
    Look both ways in the source.
- `equals_ignoring_case(text)`: fold both into temporary strings, then
  `==`. A streaming version can come later.

### Phase 7: UTF-16, C, parsing

- `StringUtf16.next`: decode a scalar. If it is above U+FFFF, return the
  high surrogate `D800 + ((v - 10000) >> 10)` and hold the low one,
  `DC00 + ((v - 10000) & 3FF)`, for the next call.
- `from_utf16(Slice[Uint16])`:
  - a high surrogate followed by a low one makes one scalar;
  - a lone surrogate is a `Utf16Error` (its offset in units), or U+FFFD in
    the lossy form.
- `to_c_string()` fails on an interior NUL. `from_c_string(ref @bit8)`
  measures up to the NUL, then calls `from_bytes`.
- `Standard.Os`'s own wide-path conversion (`wide_path`) can call these
  once `Os` may depend on `String`. Until then it keeps its own.
- `to_int()`, `to_float64()` and the rest pass `bytes()` to the existing
  `Int.parse(@byte_buffer)` family.

### Phase 8: speed

`bench/string` is hand-run like `bench/math`. Speed-ups only with numbers:

- word-at-a-time `find_byte` (SWAR has-zero on `x ^ (byte * 0x0101…)`);
- Two-Way search;
- caching the RI run count;
- copy-on-write, if copies show up.
