# Robustness Plan

> Status: in progress (Phases 0–2 done).
> Source: the robustness review of 2026-10-11 (fuzzing, adversarial
> programs, and a read of the lexer, layout, backend and instantiation
> code).
> Goal: no input makes the compiler hang, crash, or emit code whose meaning
> depends on the optimizer. The compiler owns its memory layout, aggregates
> are handled as memory, and every limit it enforces is a real one with a
> clear error.
>
> Decisions made with the owner (2026-10-12):
> - **Layouts are C layouts, strictly.** An `object` is laid out exactly as
>   the C struct with the same fields in the same order on the target:
>   `sizeof`, `_Alignof` and every `offsetof` are equal. A `ref Object`
>   crosses an `extern` as a `struct Object *` with no conversion, and C
>   headers translate field by field. Fields are never reordered. Scalars
>   map to `uint8_t`…`uint64_t`, `uintptr_t` (`@bit_native`), `_Float16`,
>   `float`, `double`, `__float128` (where the target's C has it), and
>   `T *` (`ref T`); arrays are C arrays. An object with no fields has size 0
>   and alignment 1, as GCC and Clang lay out an empty struct (ISO C has
>   none).
> - **Enums are tagged unions**, laid out as the C
>   `struct { uintN_t tag; union { P0 p0; P1 p1; … } payload; }`, where
>   `uintN_t` is the tag container and variants without a payload add no
>   union member (no payloads at all: just the tag). No niche layouts yet
>   (`Optional[ref T]` keeps its tag).
> - **The compiler owns layout.** One layout function, driven by a
>   per-target C ABI table in `lilac-target`, decides every size,
>   alignment and offset. LLVM sees aggregates as byte arrays and never
>   computes a layout. A conformance test checks it against Clang's C
>   layouts on every supported target.
> - **Aggregates live in memory.** LIR registers hold scalars and pointers
>   only. Aggregates are copied with `memcpy`, zeroed with `memset`, and
>   compared with `memcmp`. `==` stays bit equality over every byte,
>   padding included (`SPEC.md` §8). Byte-exact copies are what make that
>   deterministic.
> - **Division panics.** Integer division and remainder by zero, and signed
>   `MIN / -1` and `MIN % -1`, panic in every mode and on every target.
>   When both operands are constant in a live arm, the panic is `E060` at
>   compile time, found the way `E037` is.
> - **Generic limits count recursion, not chain length.** A definition may
>   reappear on its own instantiation chain at most 64 times (as rustc
>   counts per definition). A compile builds at most a fixed number of
>   generic instances, with its own error.
> - **No object is larger than the target allows**: the target's
>   `isize::MAX` (2^31 − 1 or 2^63 − 1 bytes). A larger type is a new error,
>   `E067`.
> - **32-bit x86 follows its platform C ABI** (`i64`/`f64` aligned to 4 on
>   i386 System V, 8 on Windows), since C structs reached through `extern`
>   must match.

## Ground rules

- Every phase ends with the `AGENTS.md` gates green.
- Every bug lands with the test that reproduces it first: a run test for
  generated code, a `FAILS_` test for a new error, a unit test otherwise.
- Phases 3, 4 and 5 change generated code on purpose; Phase 2 adds the
  size and alignment to `.lir` layout lines, and Phase 8 changes how
  `@fbit16`/`@fbit128` constants print. Each of these re-blesses the
  `tests/ui/*.lir`/`*.ll` snapshots once, and the diff is read line by line
  in review: only the intended change may appear. Every other phase keeps
  the snapshots byte-identical.
- `SPEC.md`, `IR_SPEC.md` and `AGENTS.md` are updated in the phase that
  changes what they describe.

Order: 0 → 1 → 2 → 3 → 4, then 5, 6, 7, 8 and 9 in any order. Phase 1 is
the most urgent (the language server can hang). Phase 4 needs 3, and 3
needs 2.

## Problems this fixes

Each was reproduced on 2026-10-11 against the `ai` branch:

| # | Program | Result |
|---|---|---|
| 1 | `$"a}b"`, `$"\q"`, `$"a\"` | the lexer never terminates; in the editor the compile worker hangs until restart |
| 2 | a struct holding `@array(@array(@bit64, 0xFFFFFFFF), 0xFFFFFFFF)` | debug: overflow panic in `lilac_ir::size_align`; release: wrapped, wrong `@size_of` |
| 3 | `y: B = x` with a 64 KiB `B` | `lilac build` segfaults inside LLVM; 32 KiB takes 3.7 s (copies are first-class `load`/`store` of the whole struct) |
| 4 | `==` on a struct over 1 MiB | ICE "comparison of a zero-sized aggregate" (compared as one LLVM integer, max 2^23 bits) |
| 5 | `enum { A: [256 x @bit8], B: …, C: … }` | 769 bytes, where a union needs 257 |
| 6 | `@i_div_u_32(0x1, 0x0)` in a body | silently `store i32 poison`; at runtime x86 traps, aarch64 returns 0 |
| 7 | 17 different generic functions calling each other, no recursion | `E051` "possible infinite recursion" |
| 8 | `f[T]` calling `f[W[T]]` and `f[V[T]]` | 2^16 instances, 16 s to check six lines |
| 9 | `--target=x86-linux` | triple `x86-unknown-linux`; LLVM rejects it ("internal error") |
| 10 | every aggregate access | emitted `align 1`; pointer arithmetic is `ptrtoint`/`inttoptr`, which hides provenance from LLVM |
| 11 | `"\q"` | 5 errors (the string ends at the bad escape and the rest re-lexes) |
| 12 | a UTF-8 byte-order mark | "unexpected character; `[`/`]` are generics-only…" |
| 13 | `E: enum { A: E  B }` then `e: E = .B` | correct `E053`, plus a spurious `E024` |
| 14 | a folder junction `A/B → A` | the same file loaded 64 times, until the OS path limit |

Also found by reading: the ownership pass stops a loop's fixpoint after 16
rounds and carries on silently (`places.rs:529`); backend ICEs carry no
span; float literals go through `f64`, so `@fbit128` constants lose
precision and `@fbit16`/`@fbit32` round twice; the driver deep-clones the
resolver output, the type table and the HIR on every compile, even for the
CLI; the language server clones the LIR three times on every compile.

---

## Phase 0: Guard rails first

Tests that pin the problems, plus the fuzzing that found them, so every
later phase runs against them.

- **0.1 Pending tests.** One test per problem above, marked `// pending:`
  where the harness supports it, `#[ignore]` with the phase number
  otherwise:
  - run tests for 3, 5 (`@size_of` of the three-payload enum is 257 on
    x86-64), 6 (the program panics with "division by zero", exit 101);
  - `FAILS_` tests for 2 (`E067`), 4 (no ICE; it compiles and compares);
  - CLI tests for 1 (each input finishes within 5 s with `E000`), 7, 8, 9.
- **0.2 Mutation smoke test** (`crates/lilac-cli/tests/fuzz.rs`, std-only):
  - A fixed-seed generator (a small xorshift; no new dependency) mutates
    every `tests/run` program and `lib/Standard` file: line deletion,
    duplication and swap, token insertion from a fixed list (including
    `$"`, `}`, `\`, `{`, `@`), byte insertion, deletion and replacement,
    truncation.
  - Each mutant is compiled in process (`Session`, tolerant and strict)
    on a thread with `COMPILE_STACK_BYTES`, under a watchdog: a panic or a
    compile over 10 s fails the test and prints the mutant.
  - It runs a few hundred mutants in `cargo test`; `LILAC_FUZZ_ITERATIONS`
    raises the count for a long local run.
- **0.3 Lexer property test** (`lilac-lex`): for random byte strings (fixed
  seed), lexing ends, yields at most `len + 1` items, and every `Err`
  covers at least one byte past where the previous item ended.

**Exit:** gates green with the new tests pending; the smoke test passes
on today's code once the lexer inputs it hangs on are excluded (Phase 1
removes the exclusion).

- **Delivered notes.**
  - Run tests (`// pending:` names the phase): `aggregate_copy_large`
    (3: a 64 KiB object copied, then passed by value), `enum_union_size`
    (5: 257 bytes, aligned 1), `division_by_zero` (6: exit 101, `panic:
    division by zero`, the trace's site is the `@i_div_u_32` call, like
    `@array_at`'s), `FAILS_object_too_large` (2: `E067`) and
    `FAILS_division_by_zero_constant` (6: `E060`).
  - Problem 4 is a run test, `aggregate_equality_large` (two 2 MiB
    globals compare equal, then unequal after one byte changes), not a
    `FAILS_` test: what it must do is compile and run, which an
    `expect-error` header cannot say.
  - CLI tests are `crates/lilac-cli/tests/robustness.rs`, each
    `#[ignore = "robustness plan phase N"]`: 1 (each of the three
    f-strings ends within 5 s with `E000`), 7 (18 distinct generics
    check), 8 (`E051` within 1 s, process start included), 9 (`emit-llvm
    --target=x86-linux` emits `i686-`), and also 11 (`"\q"` is one
    `E000`), 12 (a byte-order mark checks clean), 13 (only `E053`) and 14
    (a folder link `A/B -> A` is `E025`; skipped where the OS refuses the
    link). Every compile there is a child process killed past its limit,
    so a hang fails the test instead of the suite.
  - `$"a\"` hangs only because the program's closing `}` lands in the
    literal's text: `\"` is a valid escape. Problem 10 has no pending
    test; it is the shape of the emitted IR, which Phase 3's snapshots pin.
  - The smoke test runs 400 mutants by default (about 5 s on 16 threads
    in a debug build), spread over `available_parallelism` workers, each
    compile on its own `COMPILE_STACK_BYTES` thread. Mutant `i` depends
    only on `i`, so raising `LILAC_FUZZ_ITERATIONS` extends the same run.
    Strict compiles use `lilac build`'s session (host target, entry
    required, native entry, stack traces) and stop before the backend.
  - Pending run programs are left out of the corpus: they pin known
    crashes (its first run hit problem 2 through
    `FAILS_object_too_large`). Each joins when its `// pending:` line goes.
  - The f-string exclusion is `hits_fstring_hang` in `fuzz.rs`: a mutant
    with a `$"` whose text (up to the next `"`) holds a `\`, a lone `{` or
    a lone `}`. It over-approximates, never under: 18 of 3,000 mutants.
    A 20,000-mutant run (debug, 256 s) found nothing else past it.
  - The lexer property is `crates/lilac-lex/tests/progress.rs`, 100,000
    inputs built from pieces the lexer branches on. It holds today without
    `$` and `\` (`lexing_makes_progress`). With them it fails, so
    `lexing_makes_progress_through_escapes` and the review's inputs
    (`fstring_errors_make_progress`) wait for Phase 1. Plain strings fail
    it too, not just f-strings: after `"\q` reports the escape, the `\`
    re-lexes as an unexpected character inside the reported span
    (problem 11), so the test needs 1.2 as well as 1.1.

---

## Phase 1: The lexer always makes progress

**Why:** in `lex_fstr_literal` (`crates/lilac-lex/src/lib.rs:653-668`), a
lone `}` and a bad escape return an error without moving `pos`, so the
same error is produced forever. The parser keeps collecting them, so
memory also grows without bound.

- **1.1 Recover inside the literal.** A lone `}` in f-string text reports
  `E000` (with the `}}` help), consumes the `}` and continues the literal.
  A bad escape reports and consumes the `\` and the character after it
  (two characters, or the `\x` plus the hex digits present), then
  continues. The literal still ends at its closing `"` or at the end of
  the line or file.
- **1.2 The same recovery in strings and chars.** `lex_string` and
  `lex_char` record a bad escape and keep scanning to the closing quote,
  so `"\q"` is one error and one string token. `'ab'` scans to the
  closing `'` on the same line and reports once ("holds exactly one
  scalar"). A number followed by letters (`0x1g2`) consumes the whole
  alphanumeric run into one error.
- **1.3 Diagnostics are side output.** The lexer pushes recoverable errors
  to a `Vec<Diagnostic>` and keeps yielding tokens: a bad string is still
  a `StringLit` (with the bytes decoded so far), so the parser sees no
  hole. `Err` items remain only for bytes that form no token.
- **1.4 A progress invariant.** `Iterator::next` remembers `pos` on entry.
  If it is about to return without `pos` moving (and not at end of file),
  it is a lexer bug: `debug_assert!` in debug builds; release builds force
  `pos` forward one character and report `E000`. The property test (0.3)
  makes the assertion fire on any regression.
- **1.5 Name what was found.** "unexpected character `#`" (control and
  invisible characters by code point: "unexpected character U+FEFF"). The
  `[`/`]` text goes, since brackets are valid tokens.
- **1.6 Byte-order mark.** A U+FEFF at offset 0 is trivia, so offsets stay
  those of the file's bytes (the editor and the CLI agree on positions).
- **1.7 Defence in depth in the editor.** `Session` takes an optional
  cancel token (`Arc<AtomicBool>`), checked between stages, per instance
  in typeck's worklist, and per function in lowering, where a cancelled
  compile returns early. The language server sets it when a newer
  generation is due, so a slow compile (problem 8 before Phase 6) never
  holds up the next one. A compile that ends cancelled publishes nothing
  and keeps the previous analysis.

**Tests:** the Phase 0 lexer tests un-pended; unit tests for each recovery
(token and error counts); an LSP test that sends `$"a}` then a fix and
gets diagnostics for the fix; an LSP test where a slow compile is cancelled
by an edit.

- **Delivered notes.**
  - The lexer's interface keeps `Iterator<Item = Result<Token,
    Diagnostic>>`; `Err` is left for bytes that form no token (an
    unexpected character, a lone `$`). Errors inside a token go to
    `Lexer::take_diagnostics`, and `Lexer::lex_all` (every caller's entry
    point) merges them in source order, so no caller changed.
  - F-string text now ends at its line, like a string's (1.1 says so; the
    old lexer ran on to the end of the file). An f-string its line or file
    ends inside reports `E000` over `$"…` and is closed by a zero-width
    `FStrEnd`, so the parser sees a whole f-string and adds no second
    "unterminated f-string". That token and the end of file are the only
    items that consume nothing, and the progress check (1.4) allows exactly
    those: a zero-width `FStrEnd` only when leaving f-string text. Nested
    f-strings keep their own `$"` offsets (`Lexer::fstrings`) for that span.
  - A lone `}` is read as the literal brace its fix writes, and its
    help is a real fix (`Diagnostic::suggest(…, "}}")`), so the editor
    offers it. A bad escape decodes to nothing; the rest of the literal
    keeps decoding. `'…'` that is unterminated consumes its line, like a
    string; `''` is "empty char literal".
  - Malformed numbers (1.2) are not `Err` items but number tokens plus a
    side error, so the parser sees no hole: before, `x: @bit32 = 0b102`
    also got `E001` "expected expression, found `}`". The token's text is
    the well-formed literal read before the error (`0b10`, `0x1`, `1` for
    `1.foo`; `0x0` when no digit was read), so the editor's tolerant
    typeck adds no `E060` either. Messages name the bad digit ("invalid
    digit `2` in binary literal"); `docs/errors/E000.md` follows.
  - 1.4 in release: `force_progress` skips one character (or, at the end
    of the file, leaves f-string text) with an internal `E000`, keeping any
    error the stalled item carried. The property test runs with debug
    assertions, so the `debug_assert!` fires on a regression (checked by
    reintroducing an unconsumed closing quote: both property tests fail at
    the stalled item); `a_stalled_lexer_skips_one_character` covers the
    release path directly. The property now also checks side errors: each
    covers consumed bytes, at most one per byte.
  - 1.5: control, whitespace other than ASCII's, the common invisible
    format characters (zero-width, bidi, variation selectors, tags) and
    the backtick (which cannot be quoted in backticks) are named by code
    point.
  - 1.6 also makes positions agree: `lilac_span::SourceFile` starts line 1
    after a byte-order mark and the language server's position conversion
    skips it, so columns are the editor's while offsets stay the file's
    bytes. `lilac_span::BYTE_ORDER_MARK` is the one constant.
  - 1.7: `Session::cancel` is `Option<Arc<AtomicBool>>`; typeck takes it
    in `Options::cancel` (so `Options` is `Clone`, no longer `Copy`),
    lowering as a `lower` argument, and conditional selection checks it
    between rounds. A cancelled compile returns
    `ErrorGuaranteed::unreported(Unreported::Cancelled)`, reports nothing
    of its own, and clears `Session::analysis`; `Session::cancelled` says
    why it stopped. The server cancels the running compile when the next
    one's debounce deadline passes (not on every keystroke), so a compile
    that finishes within the debounce still installs its analysis;
    `Outcome` is now `Compiled | Cancelled | Panicked`.
  - The cancellation tests (`cancelling_stops_a_slow_check` in the driver,
    `an_edit_cancels_a_slow_compile` in the LSP) use problem 8's program as
    the slow compile. Once Phase 6 makes it fail fast they still pass but
    no longer show anything; Phase 6 should give them a program that is
    slow for another reason, or a test-only hook that keeps a compile busy.
  - `fuzz.rs` lost `hits_fstring_hang`; a 5,000-mutant run (debug, 71 s)
    found nothing. `$` and `\` are back in the progress test's main
    alphabet.

---

## Phase 2: One layout authority

**Why:** struct layout is computed by `lilac_ir::size_align`
(`crates/lilac-ir/src/lib.rs:901`) with unchecked arithmetic, again by
`globals.rs`'s `offsets`, and a third time by LLVM from struct types.
Alignment is assumed to equal size, which is wrong for 32-bit x86. An
oversized type wraps or panics.

- **2.1 Target ABI facts.** `lilac-target` gains `Target::scalar_layout(
  Scalar) -> (size, align)` for `Bit(8|16|32|64)`, `Float(16|32|64|128)`
  and `Pointer`, from one table per architecture and OS (x86 System V:
  `i64`/`f64` align 4; Windows x86: 8; everything else: natural, with
  `fp128` aligned to 16). `Target::max_object_size()` is `2^(ptr_bits-1) − 1`.
- **2.2 `lilac-layout` crate** (depends on `lilac-target` only; std-only;
  no `unsafe`). A pure function over a shape tree:
  ```text
  Shape = Scalar(Scalar) | Array(Shape, len) | Struct([Shape])
        | Tagged { tag: Scalar, payloads: [Shape] }
  Layout = { size, align, fields: [offset] }
  ```
  - C rules for structs: each field at the next multiple of its
    alignment, the size padded to the largest.
  - Tagged: tag at 0; the payload area at the tag size rounded up to the
    strictest payload alignment; size = that offset plus the largest
    payload, rounded to the overall alignment; every payload's offset is
    the area's offset.
  - All arithmetic is checked: `Err(TooLarge)` past
    `max_object_size`, never a wrap.
  - Unit tests: the C layouts of every scalar mix on x86-64, aarch64 and
    i686-linux, tagged layouts, the overflow boundary.
- **2.3 Typeck reports oversized types.** `lilac_typeck::Options` carries
  the `Target` (replacing `native_bits`). When `ensure_type` first builds
  a concrete object, enum or array type, it computes its layout through
  `lilac-layout`; `TooLarge` is `E067` "type is larger than the target
  allows" at the field or array length that crossed the limit, with the
  size reached. Generic templates are skipped (sizes are only known for
  concrete types), and a failing instance folds into `E042` as usual.
  Add `docs/errors/E067.md`.
- **2.4 Lowering and the backend read layouts, never compute them.**
  `LirModule::layouts` becomes `HashMap<String, LirLayout>` with
  `LirLayout { kind: Struct | Tagged, size, align, fields: Vec<LirField
  { name, ty, offset }> }`, filled once in lowering from `lilac-layout`.
  `lilac_ir::size_align` becomes a lookup. `globals.rs` uses the field
  offsets (its `offsets` goes). The verifier checks that every layout's
  offsets, size and alignment agree with `lilac-layout` for the module's
  target, and the printer shows `size`/`align` on each layout line.
- **2.5 LLVM triples.** `Arch::llvm_name` (`X86` → `i686`) is used by
  `Target::to_triple`; the user-facing name stays `x86`. A test builds a
  `TargetMachine` for every architecture and OS pair `Target::parse`
  accepts. A pair LLVM rejects is refused by `parse` with a usage error,
  never a backend ICE.
- **2.6 Cross-check against LLVM.** A `lilac-codegen-llvm` unit test
  compares `scalar_layout` with LLVM's `TargetData` ABI size and alignment
  for every scalar on every supported triple, so the table cannot drift.
- **2.7 C conformance test.** A test generates C declarations for a corpus
  of Lilac types, with `_Static_assert`s of `sizeof`, `_Alignof` and every
  `offsetof` against `lilac-layout`'s numbers, and compiles them with the
  toolchain Clang (`$LLVM_SYS_181_PREFIX/bin/clang -fsyntax-only
  --target=<triple>`) for every supported triple. The corpus covers every
  scalar after every other, nested objects, arrays of objects, trailing
  padding, empty objects (GCC/Clang extension), and enums as their
  struct-with-union equivalent. `@fbit128` is included on targets whose C
  has a 128-bit float. Skipped with a message when Clang is missing.

No generated code changes on 64-bit targets: the offsets are the ones
used today. (The enum change waits for Phase 4.)

**Tests:** problem 2's `FAILS_` test un-pended (`E067`); a run test whose
`@size_of` values are pinned for objects and arrays; the 2.5 and 2.6
target tests; snapshots byte-identical apart from the new layout header
line in `.lir` files.

- **Delivered notes.**
  - 2.1: `lilac_target::Scalar` has one variant per C scalar (`Bit8`…
    `Bit64`, `Float16`…`Float128`, `Pointer`); `@bit_native` is laid out
    as `Pointer` (`uintptr_t` has the pointer's size and alignment on every
    target, which the conformance test checks separately). The table
    differs from the plan's in one place: 32-bit Arm on macOS (APCS) also
    aligns `uint64_t`/`double` to 4, as do 32-bit x86 Darwin and bare
    metal (i386 System V); Clang and LLVM agree. 32-bit Arm on Linux with
    `gnu` is AAPCS in LLVM 18 and Clang 18 (natural alignment), so no
    `gnueabi` rendering was needed.
  - 2.5: `Target::parse` refuses WebAssembly with a vendor or an OS
    (`ParseError::Unsupported`): LLVM builds a `TargetMachine` for
    `wasm32-unknown-linux` but Clang has no C ABI for it, so no C layout
    could be checked. Every other pair is accepted; `target_tests` in
    `lilac-codegen-llvm` builds a `TargetMachine` for all 376 accepted
    four-component triples and compares every scalar (and the pointer-sized
    integer) with LLVM's data layout for each. `Target` implements
    `Display` (its triple), `Target::supported()` lists one target per
    accepted architecture and OS pair (34), and `parse` reads `i686`
    back.
  - 2.2: the API composes (`struct_layout`, `array_layout`,
    `tagged_layout` over `SizeAlign`s; `layout` over a `Shape` tree), so
    callers lay each type out once. `TooLarge` carries the crossing part
    (`Part::Field(i)` or `Part::Length`), the exact size reached (a
    `u128`) and the limit. Trailing padding that crosses is blamed on the
    last field.
  - 2.3: arrays are reported at their length (`Checker::ensure_syntax`
    walks the written type, so `@array(@array(…), N)` blames the `N` that
    crossed); a too-large array type is not remembered, so each place that
    names it is an error, while an object that fails is `Failed` as
    before. `Requirement::Size` folds a generic instance's `E067` into
    `E042` (whose notes are now de-duplicated). A `Failed` type met again
    takes its token from a pending `E042` when one waits for the worklist
    to drain (`Diags::already_reported` would otherwise assert in debug
    builds). Template instances (`Box[T]`) are never laid out.
  - 2.4: `LirModule::target` is a `Target` (not a string), so the verifier
    and the backend lay out for the module's own target.
    `lilac_ir::builtin_layout` became `builtin_fields` (the field list)
    plus `builtin_layout(ty, target)` (the computed `LirLayout`);
    `size_align` returns `Option<SizeAlign>` (`None` for an unsized type
    or an array past the limit, which the verifier also rejects as a
    type). Lowering lays types out in dependency order (`Lower::lay_out`:
    `TypeId` order puts an outer object before the types it holds), from
    `lilac_hir::Layout::slots`, the one place that says an enum is its tag
    then every payload. The LLVM backend also checks LLVM's placement of
    every struct type against the `LirLayout` (`Lower::check_layout`, an
    ICE on a mismatch) until Phase 3 removes the struct types.
    `LILAC_IR_VERSION` stays `0.4`: bumping it would change every
    snapshot's header, past the one-line diff this phase allows.
  - 2.7: `crates/lilac-layout/tests/c_conformance.rs` checks the 34
    supported targets plus six ABI variants (MinGW x86 and x86-64, musl on
    x86-64, aarch64 and Arm, Arm bare metal): every scalar pair (alone and
    between bytes), nested structs, arrays of structs and arrays, trailing
    padding, empty structs, tagged unions as their C struct-with-union, and
    200 fixed-seed random shapes; over 1,500 asserts per target (checked).
    `_Float16` and the 128-bit float are declared where Clang's predefined
    macros say C has them (`__FLT16_MANT_DIG__`; `__SIZEOF_FLOAT128__`,
    else `__LDBL_MANT_DIG__ == 113`), and the test asserts the 128-bit
    float was covered on x86 Linux, aarch64 Linux, RISC-V and wasm.
    Exception: under the Microsoft C ABI (Windows `msvc` or no ABI) C has
    no empty struct (Clang's emulation makes one 4 bytes), so shapes with
    an empty struct are skipped there; Lilac keeps size 0, align 1, as
    decided. Making the table wrong (for one, natural alignment on i686
    Linux) fails it.
  - Snapshots: 14 `tests/ui/*.lir` and 3 `tests/ui/modules/*.lir` files
    changed, each only in its `type #… {` lines (now `type #… size S align
    A {`); every `.ll` snapshot is unchanged. `tests/run/layout_sizes`
    pins `@size_of`/`@align_of` of objects, nested objects, arrays of
    objects, nested arrays, an empty object and `@fbit128` on 64-bit
    hosts; `lilac-lower`'s `layouts_follow_the_target` pins i686.

---

## Phase 3: Aggregates live in memory

**Why:** the backend lowers an aggregate copy to a first-class LLVM
`load %T` / `store %T` (`crates/lilac-codegen-llvm/src/lower.rs:618-674`).
LLVM handles those badly: compile time grows with the size, and 64 KiB
crashes it. Its rules also leave padding unspecified through an aggregate
store, which breaks bit equality. Zeroing stores `zeroinitializer` the
same way. `==` on aggregates loads the bytes as one integer, which caps
the size at 1 MiB. Every access is `align 1`.

- **3.1 LIR rule: registers are scalars.** A register's type is a `#bit`,
  `#fbit`, `#bit_native` or `ref`. The verifier rejects an aggregate
  register (`Load`, `Field`, `CopyReg`, `Const` of an aggregate type are
  `E070`). Lowering already passes aggregates by place; the remaining
  paths (`field_value` of an aggregate field, `@read` of an aggregate,
  aggregate temporaries in `read_place`) use places instead.
- **3.2 One copy instruction for aggregates.** `copy %a -> %b` between two
  aggregate places, and stores of an aggregate through a `ref`, lower to
  `llvm.memcpy` with the layout's size and alignment; `zero %x` to
  `llvm.memset`. Copy glue keeps its shape (a whole-value byte copy, then
  per-field hooks).
- **3.3 Aggregate equality.** `==` on an aggregate becomes a new leaf
  builtin, `@memcmp(ref, ref, size)`, compared with zero: the backend
  calls C `memcmp`, which LLVM expands inline for small constant sizes.
  Native mode defines `memcmp` beside `memcpy`/`memmove`/`memset`
  (`native.rs`, `no-builtins`); the macOS `libSystem.tbd` stub lists it.
  `eq_operand`'s aggregate branch and its `TargetData` per comparison go.
- **3.4 Aggregates are bytes to LLVM.** An aggregate's LLVM type is
  `[size x i8]`. Allocas and globals take the layout's alignment
  (`set_alignment`). A field's address is the base plus the field's
  offset. Every scalar load and store carries its type's alignment, or
  the field's alignment when that is lower. LLVM's SROA works from the
  access pattern, not the declared type, as `rustc` relies on.
- **3.5 Address arithmetic keeps provenance.** inkwell's `build_gep` is
  `unsafe` (forbidden here), which is why `byte_ptr` uses `ptrtoint`/
  `inttoptr` today. Instead, the backend appends two always-inline
  helpers as IR text (`append_ir`, as the thread code does):
  `.lilac.offset(ptr, iN) -> ptr` (`getelementptr inbounds i8`) for
  fields and bounds-checked elements, and `.lilac.wrapping_offset` (plain
  `getelementptr i8`) for `@ptr_add`. `byte_ptr` and the inline
  `ptrtoint` arithmetic go; `@ptr_to_address`/`@address_to_ptr` stay casts
  by definition.
- **3.6 Sparse global initializers.** `LirGlobal::init` becomes runs:
  `Vec<(offset, bytes)>` over an implicitly zero object, and the backend
  emits a packed constant of those runs with `zeroinitializer` gaps.
  Compiler memory is proportional to the initialized bytes, not the
  object size, so a large array inside an initialized global costs
  nothing.
- **3.7 Docs.** `IR_SPEC.md` "Memory": the register rule, what `copy`
  and `zero` mean for aggregates, layouts with offsets, `@memcmp`.

**Tests:** problem 3 and 4 tests un-pended (a 1 MiB copy and a 2 MiB `==`
build and run); a run test where a struct with padding is copied through
a parameter, an out, a field and a `ref`, then compared with `==` after a
`--release` build; a verifier unit test for an aggregate register; the
snapshots re-blessed once, with every change in the diff being a
`memcpy`, `memset`, `memcmp`, alignment or offset.

---

## Phase 4: Enums are tagged unions

Built on Phase 3, it is a layout change plus the few places that compute
payload positions.

- **4.1 Layout.** Lowering describes an enum as `Tagged { tag, payloads }`
  (`crates/lilac-lower/src/lib.rs:216`). `payload_field` keeps returning a
  field index, and every payload field's offset is the payload area's.
- **4.2 Construction keeps `==` deterministic.** A variant is built in a
  zeroed temporary and copied whole (`variant_value` does this today), so
  bytes beyond the active payload are zero. Audit every other way an enum
  place is written: assignment, outs, `@write`, captures, globals. Each
  must write a whole value, never a payload into a place that held
  another variant. The ownership pass and the drop and copy glue already
  touch only the live payload (`variant_guard`).
- **4.3 Globals and constants.** `globals.rs` writes a variant's payload
  at the area's offset; `@size_of`/`@align_of` of enums come from the
  layout.
- **4.4 Docs.** `SPEC.md` §Enums: the layout, written as its C
  equivalent (`struct { uintN_t tag; union { … } payload; }`), and that
  `==` compares the whole value, inactive bytes included, which
  construction keeps zero. `SPEC.md`'s extern section states the C layout
  guarantee for objects and enums. `IR_SPEC.md`: `Tagged` layouts.
- **4.5 C conformance.** The 2.7 corpus gains mixed enums (no payloads,
  one payload, payloads of different alignments, nested enums), now
  checked against Lilac's real enum layout.

**Tests:** problem 5 un-pended (257 bytes); `drop_nested` and
`match_moves_payload` unchanged; a run test that assigns a large variant,
then a small one, then compares with a freshly built small one (`==` is
true); a global holding each variant of a mixed enum; `Optional` and
`Result` sizes pinned.

---

## Phase 5: Integer division has one meaning

- **5.1 Runtime checks.** Lowering of `@i_div*`/`@i_mod*`
  (`crates/lilac-lower/src/intrinsics.rs`) emits a test before the
  division: divisor zero panics "division by zero"; for signed division
  and remainder, `MIN` with `-1` panics "division overflow". These are
  ordinary `.lilac.panic` calls with the site, so stack traces work. LLVM
  removes the tests when it can prove them false.
- **5.2 Constant cases are errors.** The constant evaluator returns why a
  division is not constant. A constant zero divisor, or `MIN / -1`, in a
  live arm is `E060`. It is collected like `E037` (`Lowered::bad_…`,
  reported before DCE and traced), so it fires in uncalled functions too.
  `eval_arith`'s silent `wrapping_div` goes.
- **5.3 Docs.** `SPEC.md` §Integer arithmetic; `docs/errors/E060.md`.

**Tests:** problem 6 un-pended; run tests for each panic (exit 101,
message, trace) on x86-64 and aarch64 where available; a `FAILS_` test for
each constant form; snapshots re-blessed for the added checks.

---

## Phase 6: Generic instantiation limits that mean something

**Why:** `request` (`crates/lilac-typeck/src/lib.rs:538`) counts every
generic instance on the chain against `MAX_DEPTH = 16`, so long
non-recursive chains fail. Meanwhile branching recursion builds b^16
instances before failing. Tests also run with a different cap (4) than
production.

- **6.1 Count recursion per definition.** An `Instance` records, for its
  own definition, how many times that definition appears on its first-
  origin chain. `E051` fires when one definition reaches 64. Chains of
  distinct definitions are unlimited (a program has finitely many).
- **6.2 Bound the work.** Typeck counts instances built per compile; past
  a budget it stops instantiating and reports a new code, `E068` "too many
  generic instances". The report names the definitions with the most
  instances, and their growing argument types. Pick the budget by
  measuring the release build (the worst case must check in under 2 s),
  and document it with the measurement.
- **6.3 Say why.** When `E051` fires and the type arguments grew along
  the chain, a note says so and shows the first two (`f[@bit32]` →
  `f[W[@bit32]]`).
- **6.4 One configuration.** The test-only `MAX_DEPTH` goes; tests that
  need a deep chain build one.

**Tests:** problem 7 un-pended (a chain of 100 distinct generics checks);
problem 8 un-pended (fails fast with `E051`, under 1 s in debug); the
existing `E051`/`E042` fixtures keep their chains; a three-way branching
recursion ends with `E051` or `E068` within the time bound.

---

## Phase 7: No silent fallbacks

- **7.1 Ownership loop fixpoint.** `lower_loop`
  (`crates/lilac-typeck/src/places.rs:524`) iterates until the head state
  is stable, with no fixed cap. Termination follows from `merge` being
  monotone over a finite-height lattice (a place only moves from full or
  empty to maybe, `torn` only becomes true, parts only refine). Assert
  it: unit tests that `merge` is commutative and idempotent (spans
  aside), and a computed bound (locals × the height of their `Own` trees,
  plus one); exceeding it is an ICE, never silently wrong drops.
- **7.2 Cascades.** A type that failed `E053` is an error type for later
  uses, so `.B` gets no `E024` (problem 13). Audit the other "type
  failed" paths in `types.rs` the same way.
- **7.3 Loader cycles.** `Fs` gains `canonicalize`. `reach_tree` keeps
  the canonical folders it has loaded, and a second path to one is
  `E025` "folder `A.B` is `A` again (through a link)", which also stops
  the walk (problem 14). `OverlayFs` and the test `Fs` implement it.
- **7.4 ICEs point at code.** Backend `bug!`s take the LIR instruction's
  or terminator's span instead of `Span::dummy()`.

**Tests:** a junction or symlink test in `lilac-module` (skipped where the
OS refuses to create one); unit tests for 7.1; `FAILS_` test for 7.2.

---

## Phase 8: Literals are parsed once, exactly

- **8.1 Typed HIR literals.** Typeck already reads every literal to check
  its width (`literals.rs`). It stores the result: `HirExprKind::Int(u64)`
  and `HirExprKind::Float(FloatBits)`, and `HirPattern::Int(u64)`. Lowering's
  `parse_int`, `number_const`'s parsing and its two `bug!`s go, and so
  does the re-parse in `Lower::choose`.
- **8.2 Exact floats.** `FloatBits` is the exact IEEE bit pattern of the
  literal's type, rounded once:
  - Rust's correctly rounded parser for `@fbit32` and `@fbit64`;
  - a small exact decimal-to-binary converter (big-integer mantissa,
    round half to even) for `@fbit16` and `@fbit128`, unit-tested against
    known bit patterns and boundary cases (subnormals, the largest
    finite value, values that round up to infinity, which are `E060`).
  
  `LirConst::Float` holds those bits, and the backend builds the constant
  by bitcasting an integer of the same width.

**Tests:** run tests printing the bits of `@fbit16`/`@fbit128` literals;
unit tests for the converter; snapshots byte-identical except `@fbit16`
and `@fbit128` constants.

---

## Phase 9: Do less work per compile

- **9.1 Analysis without clones.** `Analysis` takes ownership of the
  resolver output, the type table and the HIR. Templates are kept in
  `Analysis::templates`, not appended to `hir.funcs`, and lowering
  borrows the HIR from the analysis. The CLI does not retain an analysis
  at all (`Session::keep_analysis`, set by the language server).
- **9.2 LIR stages on demand.** `Job` carries `capture_lir`, set once the
  client has sent a `lilac/lir` request; otherwise no stage is cloned.
- **9.3 One loaded-project path.** `compile_namespace_with_stages` goes
  through `run_loaded`, so it reports the Load stage like the others.

**Tests:** the existing driver and LSP tests; an LSP test that the IR view
still works after its first request.

---

## Out of scope

- Passing objects or enums **by value** across `extern` or `export` (still
  `E028`). It needs each target's C calling-convention classification;
  the C layout guarantee here makes `ref Object` ↔ `struct Object *` work.
- Niche layouts (`Optional[ref T]` as a nullable pointer). They need their
  own design for `==` and C interop.
- Overflow checks on `+`, `-` and `*` (the intrinsics wrap by
  definition).
- A separate fuzzing toolchain (cargo-fuzz needs nightly). The Phase 0
  smoke test runs in `cargo test`; a long run raises its iteration count.
- Debug info.
