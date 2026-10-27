# String Plan

> Status: proposed, 2026-10-27. Every decision below is a proposal
> until agreed.
>
> Goal: `String` is Unicode text the way Swift's is. It is always valid
> UTF-8, it counts and walks what a reader sees as characters, and it
> offers bytes and code points as views. The everyday operations are one
> obvious call each, with no indices to learn first:
>
>     greeting: String = "Zoë 👋🏽"
>     greeting.get_count()                 // 5 characters
>     greeting.scalars().get_count()       // 6 code points (👋 + 🏽)
>     greeting.bytes().get_count()         // 13 bytes
>
>     greeting.add("!")
>     greeting.replace("👋🏽", "🙂")
>     if greeting.starts_with("Zo") { … }
>     print_line(greeting.uppercased())    // ZOË 🙂!
>
>     characters: StringCharacters = greeting.characters()
>     loop if characters.next() -> c: Character {
>         if c.is_letter() { print_line($"{c}") }
>     }
>
>     "café" == "cafe\u{301}"              // true: canonically equivalent
>
> (`\u{…}` is not a Lilac escape today; the last line shows what equal
> means, not syntax.)

## Where we are

- `lib/Standard/string.lilac` is a byte buffer with a small-string
  optimization. `StringLayout` is an `either`: `Inline` (23 bytes and a
  count) or `Heap` (a `List[Uint8]`). The tag and padding make a
  `String` 32 bytes on 64-bit targets (check with `@size_of`).
- It has `new(Format)`, `new_using`, `add(@byte_buffer)`, `formatter()`,
  `@conversion` from `Format`, and `@format`. That is all.
- Bugs:
  - `==` has no `@eql`, so it is bit equality. Two equal strings on the
    heap compare unequal, because their pointers differ.
  - `new_using` ignores its allocator, and the move from inline to heap
    always uses the default allocator.
  - There is no `hash`, so a `String` cannot be a `Map` key.
- Nothing outside `string.lilac` uses `String`, and nothing tests it.
- Nothing checks UTF-8. A `@format` hook can write any bytes
  (`write_bytes`), and a literal can hold any byte (`"\xFF"`).
- What the language gives us:
  - String literals are `@byte_buffer`s backed by data; char literals
    (`'é'`) are `@bit32` holding one scalar.
  - `[]` is reserved for generics, so there is no `s[i]`.
  - There is no `for` loop. A sequence is walked with
    `loop if it.next() -> x { … }`.
  - Every method call borrows its receiver for the rest of the scope.
  - `x += e` is `x = x + e`, and there are no `@add_assign`-style hooks
    (`docs/operators-plan.md`).
  - In a `Format` slot a literal, an f-string or any formattable value
    is accepted; a value is passed by address, not copied.
- The Io plan (not built yet) already expects `String` from
  `read_line`/`read_file`, and leaves validation open "if `String`
  starts promising valid UTF-8" (`docs/io-plan.md`). This plan makes
  that promise.
- `count_characters` in `format.lilac` (used for padding) counts code
  points, not characters.

## What Swift does (the bar)

- Native storage is UTF-8. Small strings (up to 15 bytes on 64-bit) are
  stored inline.
- A `String` is a collection of `Character`s. A `Character` is an
  extended grapheme cluster (UAX #29), what a reader sees as one
  character, so `"👍🏽".count == 1`.
- It has views: `utf8`, `utf16` and `unicodeScalars`. One opaque
  `String.Index`, a UTF-8 offset, is shared by every view.
- `==` is canonical equivalence (NFC "é" equals "e" + U+0301). `<`
  orders by the NFC scalars, and `hash` agrees with `==`. ASCII and NFC
  fast paths keep this cheap.
- Building from bytes either repairs (`String(decoding:)`: U+FFFD for
  each maximal invalid subpart) or fails (`String(validating:)`).
- Case mapping and character properties follow Unicode and ignore the
  locale.

Where Swift is hard on beginners (no integer positions at all, a
`Substring` type to convert back), this plan differs on purpose; see
decisions 4 and 5.

## Decisions (proposed)

1. **A `String` is always valid UTF-8.** Every way bytes get in checks
   them once. Formats and literals are *repaired*: each maximal invalid
   subpart becomes U+FFFD, the Unicode/WHATWG practice Swift follows. A
   `@conversion` cannot fail, and a beginner's `print` should never
   fail. Raw bytes are *checked* instead:
   `String.from_bytes(bytes) String else error: Utf8Error` gives the
   offset and kind; `from_bytes_lossy` repairs. The check runs over each
   whole append, after the format is written, so a hook that writes a
   multi-byte sequence a byte at a time is still valid.
2. **A string is a sequence of characters.** `get_count`, `first`,
   `last`, `reverse` and the default iteration (`characters()`) are in
   extended grapheme clusters (UAX #29, every current rule including
   GB9c). Bytes, code points and UTF-16 are views: `bytes()`,
   `scalars()`, `utf16()`.
3. **`==`, ordering and `hash` follow canonical equivalence**, as in
   Swift. There is one fast path: when both pass the NFC quick check
   (ASCII always does), compare bytes. Otherwise both sides are normalized as a stream, with
   no allocation. Byte equality is still available as
   `a.bytes() == b.bytes()`.
4. **Positions are typed.** `StringIndex` is a byte offset that only
   `String` makes (`find`, `start_index`, `index_after`, …). All views
   share it, and every use checks it is in range and on a boundary, so
   it can never split a code point. Beginners rarely see it: the common
   operations take **character counts** instead (`prefix(3)`,
   `remove_first(2)`, `character_at(4)`). Those cost O(n), which is
   documented, and O(1) on a string of ASCII without `\r`. A count can
   never be confused with a byte offset, because the two are different
   types.
5. **One owning type, and text parameters are `Format`s.** There is no
   `Substring` or `&str`: `slice`, `prefix`, `split` and the rest return
   `String`s, and most are inline and allocate nothing. Methods that
   only read text (`contains`, `find`, `replace`, `split`, `join`'s
   separator) take a `Format`. A literal, an f-string, a `String`, a
   `Character` or a `Scalar` all pass, and none is copied. A private
   `gather` turns the format into bytes; when it arrives as one piece
   (a literal, or a `String` hole) it uses those bytes in place.
6. **Copies are deep.** That is the copies plan's rule, and there is no
   copy-on-write. A short string has no heap part, so copying it is
   cheap, and a last use moves. Copy-on-write is revisited only if the
   benchmarks show a need (phase 8).
7. **Naming follows Swift's mutating pairs.** A verb edits in place
   (`trim()`, `uppercase()`, `reverse()`, `replace(…)`). Its participle
   returns a new string (`trimmed()`, `uppercased()`, `reversed()`,
   `replacing(…)`). Counts are `get_count` (characters), like
   `List.get_count`.
8. **Standard is not blessed.** The compiler gets no special cases for
   `String`, `Character` or Unicode. Unicode data comes from a pinned
   Unicode Character Database (UCD) through a checked-in generator, as
   `float_tables.lilac` does, and lives in `Standard.Unicode`. DCE drops
   the tables a program never reaches.
9. **Locale-free.** Case mapping, case folding and properties are
   Unicode's defaults, with no Turkish dotless i. Locale-aware text is a
   separate library.
10. **Allocators.** A string uses the default allocator. `new_using`
    starts the string on the heap, whose header remembers the allocator,
    because an inline string has no room for one.

## Types

The user-facing types live in the root of `Standard`, where `String`
already is. The machinery lives in `Standard.Unicode`.

| Type | File | What it is |
| --- | --- | --- |
| `String` | `string.lilac` | Owned, growable, valid UTF-8 text. |
| `Character` | `character.lilac` | One extended grapheme cluster. Privately a `String`, so almost every character is inline; 👨‍👩‍👧‍👦 is 25 bytes and goes to the heap. |
| `Scalar` | `scalar.lilac` | A Unicode scalar value (a code point that is not a surrogate). Wraps `@bit32`. `'é'` converts to it through a `=>` `@conversion`, so a bad constant is caught at compile time (`E060`). |
| `StringIndex`, `StringRange` | `string_index.lilac` | A typed byte offset, and a half-open pair of them. |
| `StringBytes`, `StringScalars`, `StringCharacters`, `StringUtf16` | `string_views.lilac` | Views: a `ref String` plus a front and a back position. They have `next`, `next_back`, `get_count`, `is_empty`, `first`, `last`, `index_after`, `index_before` and `get(StringIndex)`. `StringBytes` also has `get(Int) Uint8`, which is O(1), and `as_byte_buffer()`. |
| `Utf8Error`, `Utf16Error` | `Unicode/errors.lilac` | Offset plus kind (`Truncated`, `Overlong`, `Surrogate`, `TooLarge`, `UnexpectedContinuation`). |
| `GeneralCategory` | `Unicode/properties.lilac` | The 30 categories, as an `either`. |

A view reads through its `ref String` on every step and checks its
position against the string's current length and boundaries. Editing
the string while a view walks it is therefore memory-safe; the view
panics if a position no longer fits.

## API sketch

Signatures are Lilac. The phase that adds each group is in brackets.

```lilac
// Making [1, 2, 4, 7]
s: String = "text"                       // also $"…": @conversion from Format, repairing
String.new(text: Format) String
String.with_capacity(bytes: Int) String
String.from_bytes(bytes: Slice[Uint8]) String else error: Utf8Error
String.from_bytes_lossy(bytes: Slice[Uint8]) String
String.from_utf16(units: Slice[Uint16]) String else error: Utf16Error
String.repeating(text: Format, count: Int) String
String.join(parts: ref List[String], separator: Format) String

// Asking [2, 3, 4]
is_empty() Bool                          // O(1)
get_count() Int                          // characters
first() Character else nothing
last() Character else nothing
character_at(position: Int) Character    // panics past the end, like List.get
contains(text: Format) Bool
starts_with(text: Format) Bool
ends_with(text: Format) Bool
find(text: Format) StringRange else nothing
find_last(text: Format) StringRange else nothing
count_of(text: Format) Int

// Editing in place [2, 3, 4, 6]
add(text: Format) nothing                // append; replaces add(@byte_buffer)
add_line(text: Format) nothing
insert(at: StringIndex, text: Format) nothing
remove(range: StringRange) nothing
remove_first(count: Int) nothing         // characters
remove_last(count: Int) nothing
remove_prefix(text: Format) Bool
remove_suffix(text: Format) Bool
replace(text: Format, replacement: Format) Int   // how many it replaced
replace_first(text: Format, replacement: Format) Bool
replace_range(range: StringRange, replacement: Format) nothing
trim() / trim_start() / trim_end()       // Unicode White_Space
uppercase() / lowercase()
reverse()                                // by characters
clear() / reserve(bytes: Int)

// New strings [3, 4, 6]
slice(range: StringRange) String
prefix(count: Int) String / suffix(count: Int) String
dropping_first(count: Int) String / dropping_last(count: Int) String
trimmed() / uppercased() / lowercased() / reversed() String
replacing(text: Format, replacement: Format) String
split(separator: Format) List[String]
lines() List[String]                     // \n, \r\n and the Unicode newlines
words() List[String]                     // split on whitespace, empty pieces dropped

// Views and positions [2, 3, 7]
bytes() StringBytes / scalars() StringScalars / characters() StringCharacters / utf16() StringUtf16
start_index() StringIndex / end_index() StringIndex
index_after(index: StringIndex) StringIndex     // by character; views step by their unit
index_before(index: StringIndex) StringIndex

// Comparing [2, 5, 6]
@eql (String, String), (String, @byte_buffer), (@byte_buffer, String)
@less_than … (String, String), and compare(other: ref String) Ordering
equals_ignoring_case(text: Format) Bool
hash() @bit64                            // agrees with ==, so String is a Map key

// Joining [4]
@add (String, String), (String, @byte_buffer), (String, Character) String

// Converting [7]
to_int() Int else error: ParseError      // forwards to the numeric parse methods
to_float64() Float64 else error: ParseError
```

`Character` has `==` with `Character`, `@bit32` (`c == 'a'`) and
`@byte_buffer` (`c == "é"`). Its properties follow Swift:
`is_letter`, `is_number`, `is_whitespace`, `is_newline`,
`is_punctuation`, `is_symbol`, `is_uppercase`, `is_lowercase`,
`is_ascii`, `ascii_value() Uint8 else nothing`,
`digit_value() Int else nothing`, `uppercased()`, `lowercased()` and
`scalars()`.

`Scalar` has `value() Uint32`, `Scalar.from(value: Uint32) Scalar else
nothing`, `general_category()`, the same `is_*` tests, `utf8_length()`,
`@format`, and `==` and `<` (with `@bit32` too).

Every panic says what was asked and what exists, for example
"character 7 of a 5-character string", or "index 5 is inside a
character". Every public item has a `///` doc with a one-line example,
which the editor shows on hover and in completion.

## Phases

Each phase ends with the gates green and with `SPEC.md`, `AGENTS.md`
(a `String` bullet beside `Standard.Heap`'s) and the docs updated.
Library tests go in the project `tests/standard`, in a new `Strings`
namespace. Panics and their messages go in `tests/run/string_*`.

### Phase 0: baseline and bug fixes

- Write tests for today's behaviour: inline, the switch to the heap at
  23, 24 and 25 bytes, copies, drops.
- Fix the bugs above:
  - `@eql` compares bytes.
  - `new_using` keeps its allocator.
  - Add `hash` (Wyhash over the bytes).
- Done when a `Map[String, Int]` works in `tests/standard`.

### Phase 1: UTF-8 core

- `Standard.Unicode`'s `utf8.lilac` gives `decode`/`encode` of one
  scalar, `validate(bytes) nothing else error: Utf8Error`, and `repair`
  with maximal subparts. Validation has an ASCII fast path that checks
  a word at a time.
- Add `Scalar`, and its conversion from char literals.
- Enforce the `String` invariant on every entry point: the `Format`
  conversion, `add`, and the formatter sink, which repairs the appended
  range once the format is written. Add `from_bytes` and
  `from_bytes_lossy`.
- `formatter()` becomes private. `add(text: Format)` replaces it, and
  the Io plan's `Writer` will cover streaming.
- Tests:
  - The malformed-sequence table from Unicode §3.9 (overlongs,
    surrogates, past U+10FFFF, truncated, stray continuation bytes),
    each with its exact U+FFFD count.
  - A sequence split across two `write_bytes` calls.
  - Round trips of every scalar class.

### Phase 2: storage, positions, bytes and scalars

- **Storage stays the `either`** (`Inline`: 23 bytes and a count;
  `Heap`: a `List[Uint8]`), 32 bytes on 64-bit targets. It is simple,
  needs no hand-written hooks (the list copies and drops itself), and a
  zeroed string is a valid empty one. All code goes through a few
  private accessors (`count`, `data`, `capacity`, `bytes`, `reserve`),
  so the layout can change later without touching anything else.
- **No stored flags.** There is no spare byte in the layout. The ASCII
  and NFC fast paths scan instead (a word-at-a-time ASCII check, the NFC
  quick check), which is the same order as the work they guard.
- Add `StringIndex`, `StringRange`, `StringBytes` and `StringScalars`
  (forward and backward). Add `insert`, `remove`, `replace_range`,
  `slice`, `clear` and `reserve` on indices.
- `get_count` is temporarily the code-point count, and is documented
  as such.
- Tests:
  - Every edit across the inline/heap boundary, in both directions.
  - Every index error.
  - `@size_of(String)` is pinned on each target.
  - Add `String` to `tests/sync_link.rs`'s cross-link program, or a
    sibling, so it links on the six native targets with no libcall.

### Phase 3: Unicode data and characters

- **Data and generator.**
  - Check in the UCD files needed, from the newest released version,
    under `tools/unicode/ucd/` with the Unicode license and a `VERSION`
    file.
  - `python tools/unicode/gen_tables.py lib/Standard/Unicode/tables.lilac`
    writes the tables. It is deterministic and uses only the Python
    standard library. Change the generator and rerun it, as with
    `float_tables.lilac`.
- **Table form.** Each table is a two-stage lookup encoded as one
  `\xHH` string literal (a `=>` `@byte_buffer` constant, read with
  `@load_u8`). A structure with one field per entry, as the float tables
  use, does not scale to Unicode's sizes.
  - Measure the lex and check time of `Standard` before and after.
  - If the time hurts, a byte-embedding intrinsic gets its own plan.
- **Segmentation.** `grapheme.lilac` implements UAX #29 forward, and
  backward with lookback for regional-indicator pairs, emoji ZWJ
  sequences (GB11) and Indic conjuncts (GB9c). Its fast path: two ASCII
  bytes other than `\r` always break.
- Add `Character` and `StringCharacters`.
  - `get_count` becomes characters.
  - Add `first`, `last`, `character_at`, `prefix`, `suffix`,
    `remove_first`/`_last`, `dropping_*`, `reverse`/`reversed`, and
    `index_after`/`index_before` by character.
- `Padded`'s `count_characters` counts characters. A format is not a
  string, so it runs the segmenter over the streamed bytes.
- **Conformance.** `crates/lilac-cli/tests/unicode.rs` embeds the
  generated `GraphemeBreakTest` rows as string literals, as `math.rs`
  does. It checks forward and backward segmentation in Debug and
  Release.
- **Property test** in `tests/standard`: random sequences, from a seeded
  generator, of one scalar per break class. Backward segmentation must
  equal forward, reversed.

### Phase 4: the everyday text API

- **`gather`** turns a `Format` into bytes. It uses the bytes in place
  when they come as one piece, and otherwise writes them into a stack
  buffer, then the heap.
- **Searching** finds the bytes, then accepts only matches on character
  boundaries. `"é".contains("e")` is false; no match can split a
  character. After phase 5, both sides are compared in NFC.
  - Add `contains`, `starts_with`, `ends_with`, `find`, `find_last`,
    `count_of` and `replace*`.
- **Trimming.** Add `trim*`, using a small `White_Space` table that
  phase 3's generator adds.
- **Splitting.** Add `split`, `lines` and `words`, which return
  `List[String]`.
- **Building.** Add `join`, `repeating`, `add_line`, and the `@add`
  hooks.
- Tests cover each function, including empty strings, emoji and
  combining marks, and a separator at either end.

### Phase 5: normalization and Swift equality

- `normalize.lilac` implements UAX #15:
  - NFD and NFC, algorithmic Hangul, canonical ordering, composition
    exclusions, and the quick check;
  - a streaming normalizer with a small fixed buffer for combining
    marks, so comparison allocates nothing;
  - public `normalized(form: NormalForm) String`.
- `==`, `compare`/`<` (NFC scalars, in order) and `hash` (the NFC bytes
  streamed into Wyhash) take the byte fast path when the quick check
  passes.
- `Character` and `Scalar` comparisons follow the same rules.
- Conformance: the generated `NormalizationTest` rows go in `unicode.rs`.
- Tests:
  - `==` and `hash` agree on canonically equal pairs, including as
    `Map` keys.
  - The fast paths give the same answers as the full path.

### Phase 6: properties and case

- Add `GeneralCategory`, the `is_*` properties of `Scalar` and
  `Character`, and `digit_value`.
- Case is full mapping:
  - `uppercase*`/`lowercase*` use `SpecialCasing`, so `ß` becomes `SS`,
    and they handle the final sigma;
  - `case_folded()` uses `CaseFolding`;
  - `equals_ignoring_case` and the case-insensitive search use the
    folded forms.
- Tests are generated from the UCD (the case pairs, and a sample of
  every category) into `unicode.rs`.

### Phase 7: UTF-16, C and the outside world

- Add `StringUtf16` and `from_utf16`/`_lossy`.
- Add C interop: `to_c_string` and `from_c_string` over `CString`.
- `Standard.Os.Windows` wide-string helpers take and give `String`s.
- Io plan: `read_line`, `read_file` and `read_all_text` validate, and
  fail with a new `.InvalidData` error. Settle this with the Io plan,
  whichever lands second.
- Add `to_int`, `to_float64` and their siblings over the existing
  `parse` methods.

### Phase 8: performance

- Add `bench/string`, hand-run like `bench/math`: build, append, search,
  split, count, compare, and hash, on ASCII, Latin, CJK and emoji text.
- Where a toolchain is installed, compare against Swift and Rust.
- Add word-at-a-time fast paths where the profile shows a need.
- Decide whether copy-on-write or Swift's UTF-16 "breadcrumbs" are
  worth it, with numbers, not before.

## Language follow-ups (separate plans, not blocking)

None of these is needed for the phases above, and each would make
strings friendlier:

- **`s += x` on a borrowed string copies.** `x += e` is `x = x + e`.
  When `s` is unborrowed there, the last-use rule moves it into `@add`,
  which appends in place: amortized O(1). But any earlier method call
  on `s` borrows it for the rest of the scope, so `s += x` in a loop
  copies the string every time, O(n²). Until this is fixed, the docs
  teach `s.add(x)` for appending.
  - The fix is a small plan of its own: compound assignment moves its
    target into the operator once `e` is evaluated, since the place is
    written again right after.
  - That is only safe if no hook can observe the empty place, which
    needs settling first.
- **`for c in s.characters() { … }`**: sugar over the
  `next() T else nothing` shape every view already has. It is the
  biggest win for beginners across all collections.
- **`match` on strings** (`match command { "quit" -> … }`), lowered to
  `@eql` calls. String patterns are `E010` today.
- **Multi-scalar character literals** (`'👍🏽'` as a `Character`), and a
  compile-time check that a literal going into a `String` is valid
  UTF-8 (`"\xFF"`), rather than repairing it at run time.

## Open questions

- **Names.** `scalars()`/`Scalar` is Swift's and Unicode's word. The
  alternative is `code_points()`/`CodePoint`, more familiar to
  beginners; both are reasonable.
- **`character_at` failing or panicking.** It panics, like `List.get`,
  and `first`/`last` fail with `nothing`. Alternatively everything that
  can miss fails, so `E057` makes the caller handle it.
- **`split` returns `List[String]`.** That is simple, but copies each
  piece. An iterator form can be added alongside if phase 8 shows it
  matters.
- **Table size and compile time.** Normalization and case tables are
  tens of kilobytes. DCE keeps them out of programs that never compare
  or change case, but every compile still lexes and checks them, since
  `Standard` loads whole. Phase 3 measures this.
