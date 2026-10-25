# F-string Plan

> Status: proposal. The direction (the formatter type belongs to the
> library, not the compiler) was agreed on 2026-10-12; the details below
> are not yet. This replaces the first draft, which had a builtin
> `@byte_sink`.
>
> Goal: an f-string is **data, not text**. `$"Hello {name}"` evaluates to
> what its source says: static literal text, borrowed hole values, and for
> each hole how to format it (resolved at compile time). The compiler never
> learns what a string is, and it names no sink or formatter either: the
> library chooses the type formatted text goes into, so ordinary code
> touches only library types. The IR is small and readable, there is no
> piece limit, and holes stay typed until a consumer formats them.
>
> Decisions to confirm with the owner (the plan assumes the first option
> of each):
> - **The formatter type is a type argument.** An f-string is a
>   `@format_args(W)`, and a hole formats through `@format(ref T, ref W)`,
>   where `W` is whatever the library chooses (`Standard`'s `Formatter`).
>   Alternative: one formatter type per program, fixed by the `@format`
>   hooks. The type would be simpler, but a library with a second one
>   would break every program that mounts it.
> - **Type aliases**, so `Standard` can name the type: `Format: alias
>   @format_args(Formatter)`. `alias` becomes a keyword (nothing in `lib`
>   or `tests` uses it as a name). Alternative: a wrapper structure, as `Int`
>   wraps `@bit_native`. It would hold a borrowed view, so the rules in
>   "Where a format can live" would have to follow formats into user
>   types, and the slot rules in "Typing" would not reach it.
> - **`@panic` takes what `@compiler_print` takes**, so the runtime
>   library decides what a panic message is.
> - **A literal or a single value in a format slot is a format**:
>   `@panic("x")`, `@panic(name)`, `print(count)`.
> - **A format lives only in locals and parameters**, never in a field,
>   payload, array element, out or global, as C# rules for a `ref
>   struct`. Alternative: an escape analysis in the ownership pass.
> - **Format specs are values, not syntax.** `{count.hex()}`,
>   `{pad_left($"{name}", 10)}`: a variation is a value with its own
>   `@format`, so the compiler knows no spec mini-language.
> - **The IR gets a `format` data entry and a `format` instruction**, so
>   an f-string is one line at its site and the verifier checks each
>   hole's type against its formatter. Alternative: general relocatable
>   data plus plain stores (about three instructions per hole, holes
>   unchecked).
> - **Hooks of generic instances are checked on first use** (all hooks
>   but `@on_drop`/`@on_copy`/`@on_override`), as C++ instantiates class
>   template members. Without this, `Optional[T]` cannot offer `@format`
>   only for the `T`s that have one.
> - **Names**: `@format_args(W)`, the `@format` hook,
>   `@format_args_holes`, `@format_args_piece`, `@format_args_hole`,
>   `alias`; in `Standard`, `Formatter` and `Format`.

## Problems this fixes

Read from the code on 2026-10-12 (`check_fstring` in
`crates/lilac-typeck/src/expr.rs`, `format_args_value` in
`crates/lilac-lower/src/expr.rs`, the runtime in
`crates/lilac-ir/src/runtime.rs`):

1. **Holes become text too early, and unsoundly.** Each hole converts to
   `@byte_buffer` through `@conversion` when the f-string is built. A
   `@byte_buffer` never owns, so the conversion has nowhere to put an
   integer's digits: a static buffer (two integer holes alias it), its own
   frame (dangling), or a leak. An owned hole is passed to the conversion
   by value: a copyable one is copied (for a `String`, an allocation) and
   a move-only one moves. Either way the conversion owns its parameter and
   drops it when it returns, so the piece views freed memory, and a
   move-only variable is empty afterwards (`E063` on its next use). The
   ownership pass treats the pieces as consumed (`places.rs`,
   `HirExprKind::FormatArgs`).
2. **The IR is a table, built by calls.** `#format_args` is a count plus
   sixteen `#byte_buffer`s (264 bytes on 64-bit), zeroed by
   `.lilac.format_args_new`, filled by one `.lilac.format_args_push` call
   per piece (each with a `#byte_buffer` temporary and a `ref` temporary),
   and passed *by value* (a 264-byte copy) to `count` and `piece`.
   `$"a{part}b"` is 20 instructions and 7 temporaries in `main`, plus two
   runtime functions (`tests/ui/fstring.lir`).
3. **A 16-piece cap** (`E061`) that exists only because of that table.
4. **Consumers cannot tell literal text from interpolated text**, so an
   HTML builder cannot escape holes, a query builder cannot bind them as
   parameters, and a disabled log level still formats.
5. **Panics take bytes only**: `@panic` is `(@byte_buffer)`, so a panic
   cannot say which index or which value went wrong.
6. Smaller: literal pieces lose their spans (`FStrPart::Lit` is bytes
   only), and `@format_args_piece` out of range traps with no message.

## The model

### What an f-string is

```lilac
print($"{name} is {age} years old")
```

evaluates to a `@format_args(Formatter)` (`Format` in `Standard`): two
pointers.

- **The template** (static, read-only, one per distinct f-string): the
  literal pieces and, for each hole, its formatter (the `@format` hook
  of the hole's type, found at compile time).
- **The holes** (on the stack): one pointer per hole, to the value it
  borrows.

Nothing is formatted when the f-string is evaluated. Formatting happens
when a consumer walks the data, as often as it likes (zero times when a
log level is off).

`@format_args(W)` is a builtin type with a type argument, like
`@array(T, N)`. `W` is the type every formatter in the template writes
into. The compiler never looks inside a `W`; it only passes a `ref W`
along.

### Formatting is a hook

A type is formattable into `W` when it has a `@format` hook for `W`.
With `Standard`, user code reads:

```lilac
Point: structure {
    x: Int
    y: Int

    @format: function (self, out: ref Formatter) nothing {
        out.write($"({self.x}, {self.y})")
    }
}
```

- Shape: `@format: function (value: ref T, out: ref W) nothing`, for any
  `T` but `unknown` and any `W` (`E022` otherwise). Like every hook it is
  found by exact signature: a hole of type `T` in a `@format_args(W)`
  uses `@format(ref T, ref W)`, with no conversion search. A `ref T` hole
  is formatted as `T`.
- A hole whose type has none is `E071` at the hole ("`Point` cannot be
  formatted into `Formatter`"), with the hook to write as help. In a
  generic body it goes through `emit_involving`, so a template never
  reports it and an instance folds it into `E042`.
- A hook is how the compiler calls user code, with no method-name
  coupling, and anyone can add one for a type they do not own, builtins
  included. `Standard` defines them for the bit containers,
  `@byte_buffer`, `Format` itself (a nested format) and its own types.
- The compiler formats nothing itself. A `@byte_buffer` or nested-format
  hole works only because a library defines that hook.
- A `@format` hook may be called directly (`@format(ref x, out)`), like
  any hook.

### The formatter belongs to the library

The compiler fixes nothing about `W`: no builtin sink, no fields, no
functions. `Standard` defines one, an ordinary structure. This is library
code, and the library is the only place the builtins appear:

```lilac
// Where formatted text goes. A consumer makes one over its own storage;
// a type's `@format` writes into the one it is given.
Formatter: structure {
    private {
        target: ref unknown
        sink: function (ref unknown, @byte_buffer) nothing
    }

    // A formatter that hands every byte to `sink(target, bytes)`.
    over: function (target: ref unknown, sink: function (ref unknown, @byte_buffer) nothing) out: Formatter {
        out = Formatter { target = target, sink = sink }
    }

    // An f-string, a literal, or one formattable value (`out.write(self.x)`).
    write: function (self, text: Format) nothing {
        last: @bit_native = @format_args_holes(text)
        index: @bit_native = 0x0
        loop {
            self.write_bytes(@format_args_piece(text, index))
            if index == last { break }
            @format_args_hole(text, index, self)
            index = @i_add_native(index, 0x1)
        }
    }

    write_bytes: function (self, bytes: @byte_buffer) nothing {
        sink: function (ref unknown, @byte_buffer) nothing = self.sink
        sink(self.target, bytes)
    }
}

Format: alias @format_args(Formatter)
```

- N formattable types and M consumers need N hooks and M sink functions,
  not N × M: every hook writes into a `Formatter`, and every consumer (a
  `String`, `print`, a log, a fixed buffer) makes one over its own
  storage.
- The library can grow the type (an error flag for a failed write, a byte
  count, padding state, an indent level for pretty printing) with no
  compiler change, because no compiler code reads its fields. A builtin
  sink could carry none of these.
- A program without `Standard` defines its own `W` (the UI tests format
  into a fixed buffer). Two libraries may each have one; a type that
  formats into one does not format into the other until someone writes
  that hook too.

### Typing

An f-string's `W` comes from where it goes, the way a number literal's
width does (`SPEC.md` §2):

1. An expected `@format_args(W)` (or an alias of one) gives `W`.
2. Any other expected type `E` takes the `W` of its one `@conversion`
   from a `@format_args(W)`, and the f-string converts through it:
   `text: String = $"..."`. This is the search `conversion_source`
   (`lilac-typeck/src/literals.rs`) already makes for a number literal's
   width. With none it is `E043`; with two (from two formatter types) it
   is `E073`, and `as` picks one: `$"..." as Format`.
3. With nothing expected (a statement, an operand) it is `E073`: "an
   f-string needs a type here". (Today such an f-string is checked and
   dropped.)

An expected type that contains a generic parameter makes the f-string
dependent, so a template drops its errors (`emit_involving`).

In a `@format_args(W)` slot, and only there (never in a `String` slot or
any other type that merely converts from a format), two more forms are
formats:

- **A string literal is a format with no holes**, as a number literal
  takes an expected width. So `print("hi")` and `print($"{x}")` both
  work.
- **Any other value is the one-hole format `$"{value}"`**: `@panic(name)`
  with a `String`, `@panic(bytes)` with a `@byte_buffer`,
  `out.write(self.x)`. A value already of type `@format_args(W)` is
  itself; one with no `@format` for `W` is `E071`.

The slot rules apply where conversions do (typed contexts and call
arguments), never to an operator's or a direct hook call's arguments,
which stay exact. A `String` slot never formats a value behind the
writer's back: `name: String = count` stays `E043`.

Holes are full expressions, checked with no expected type, evaluated left
to right when the f-string is. `{{` and `}}` are unchanged.

### Consuming a format

Three intrinsics read one. Libraries use them (they are what
`Formatter.write` is made of); ordinary code does not:

| Intrinsic | Does |
|---|---|
| `@format_args_holes(args)` | The number of holes `k`, as `@bit_native`. |
| `@format_args_piece(args, i)` | Literal piece `i` (`0..=k`) as a `@byte_buffer` over static data. Piece `i` comes before hole `i`. |
| `@format_args_hole(args, i, out)` | Formats hole `i` (`0..k`) into `out`, a `ref W` for the format's own `W` (`E010` otherwise). |

Out-of-range indices panic with a message, checked at the call site the
way `@array_at` is, so the panic points at the caller.

There is no intrinsic that writes a whole format: the compiler cannot
write a piece into a `W` it knows nothing about, so the one walk lives in
the library (`Formatter.write` above). A structural consumer walks the
format its own way:

```lilac
// HTML: literal text raw, interpolated text escaped.
append: function (self, template: Format) nothing {
    raw: Formatter = Formatter.over(self as ref unknown, Html.push_raw)
    escaped: Formatter = Formatter.over(self as ref unknown, Html.push_escaped)
    last: @bit_native = @format_args_holes(template)
    index: @bit_native = 0x0
    loop {
        raw.write_bytes(@format_args_piece(template, index))
        if index == last { break }
        @format_args_hole(template, index, ref escaped)
        index = @i_add_native(index, 0x1)
    }
}
```

### Panics

`@panic(message)` takes what `@compiler_print` takes. The runtime
library decides what a panic message is, as Rust's `#[panic_handler]`
does, and the compiler names no formatter here either.

- `@compiler_print`'s shape becomes `function (text: M) nothing`, where
  `M` is `@byte_buffer` or a `@format_args(W)` (`E022` otherwise).
  `Standard`'s takes `Format`, so every form above works in a panic:
  `@panic("x")`, `@panic($"index {i} past {n}")`, `@panic(name)`. A
  minimal runtime that takes `@byte_buffer` keeps today's panics.
- With no `@compiler_print` in the compile (`check` or `emit-*` without
  a runtime), `M` is `@byte_buffer` and the hook is declared external
  with that type, as today (`lilac_ir::hook_externs`).
- The handler `.lilac.panic(msg: M, site: #byte_buffer)` writes `panic: `,
  the message, a newline and the trace lines through the hook. Its own
  text (those lines, and the division and bounds messages that lowering
  generates) is built as an `M`: a data constant, or a format with no
  holes. A trace frame's site is a `#byte_buffer` known only at run time,
  so the handler wraps it in a one-piece template on its own stack.
- A formatter that panics while a message is written must not recurse
  forever. A thread-local panic depth, beside the trace head, makes a
  second panic write the fixed `panic while writing a panic message` and
  exit 101, and a third (the hook itself is broken) trap at once. Go's
  runtime has the same rule ("panic while printing panic value").

### Naming the format type: type aliases

Code that declares its own format-taking function (a logger, an
assertion helper) needs a name for `@format_args(Formatter)`, and it
should not be a builtin. Lilac has namespace aliases (`using … as`) but no
type aliases (`STDLIB.md` §0.1). This plan adds them:

```lilac
Format: alias @format_args(Formatter)
```

- **Transparent**: an alias *is* its type (one `TypeId`). There is no
  conversion, and it has the same hooks, layout and symbols. It may be
  used wherever a type can: parameters, locals, generic arguments, `as`,
  and struct literals and static paths when it names a structure.
- **An item like a structure**: top level only (`E036` inside a type),
  namespaced and visible like any item (`private { }`, `using`), and a
  name clash is `E020`.
- **Not generic** (`Name[T]: alias …` is `E001`), but it may name an
  instance (`Bytes: alias List[@bit8]`). Arguments on an alias, or a
  generic named without its arguments, are `E044`. An alias that reaches
  itself is `E074`.
- **Display**: a type written with arguments (an instance, an array, a
  function type, and `@format_args(W)` from Phase 4) that exactly one
  alias names prints through it (`Types::display`), so diagnostics and
  hovers say `Format`, not `@format_args(Formatter)`. A plain name never
  does, so `Byte: alias @bit8` does not rename every `@bit8`. Symbols and
  layouts never use aliases.
- **Names a type, not a reference**: `Name: alias ref T` is `E001`;
  `ref Name` is written where it is used.
- **Editor**: an alias is a `DefId` like any item, so definition,
  references, rename, hover and semantic tokens (as a type) come from the
  existing index.

### Where a format can live

A format borrows twice: its holes point at values, and its hole pointers
sit in the frame of the function that built it. So it is a value of the
call that made it, and these rules keep it there with no borrow checker:

- **Only locals and parameters hold one.** `@format_args(W)` may be the
  type of a local, a parameter (by value or `ref`) or a `ref` local,
  never of a field, either payload, array element, out or global (`E072`).
  A generic instance that would put one there (`Optional[Format]`) fails
  with `E072`, folded into `E042` at the use. So no format leaves its
  frame through a type; to keep the text, format it into a `String`. (C#
  has the same rule for `ref struct`s, its interpolated-string handlers
  among them.) A `ref Format` field is allowed, with the usual `ref`
  caveat.
- **No stores through a `ref`.** Assigning a format through a `ref`, or
  with `@write`, is `E072`: the target may outlive this call.
- **Holes are borrowed, never moved or copied**: `$"{s}"` leaves `s`
  full, and the hole is a `RefOf` (the ownership pass reads it, as it
  reads a method receiver).
- **A place hole** (a local, a parameter, a field of one, a global, or
  behind a `ref`) follows the `ref` rule: the format must not outlive the
  place. Lilac tracks no borrows, so this stays a documented caveat, as
  it is for `ref`.
- **A temporary hole** (a call result, a literal, a field of a temporary,
  the value of a one-hole format) lives until the end of its statement.
  So does a format passed as an argument or converted, which is sound.
- **Extension:** when the f-string is the initializer of a local, or an
  arm of a value `if`/`match` that is (`args: Format = $"took {elapsed()}
  ms"`), typeck evaluates each hole, in order, into a hidden local
  declared just before that local. Its temporaries then live as long as
  the local, and the ownership pass drops them with the block, after it.
  An arm's hidden locals are full on that arm only, so they get drop
  flags.
- **An f-string with a temporary hole assigned to an existing local**
  (`message = $"failed: {reason()}"`) is `E072`, with "declare a local
  for it" as help: the temporary ends with the statement, and the local
  does not.

### Format specs are values

There is no spec syntax. A variation is a value with its own `@format`:

- type-specific, a method returning a wrapper: `{count.hex()}`,
  `{ratio.fixed(2)}` (`Hex { value: Int }` with its own hook);
- a text transform, a function over a nested format:
  `{pad_left($"{name}", 10)}`. Its wrapper holds a `ref Format` (a
  format cannot be a field) and lives as a statement temporary. It works
  for every type with no generics, because `Format` is one type.

Both are checked at compile time, discoverable by completion, and need
nothing from the compiler. `{x:…}` and `{x, …}` are parse errors
(`E001`) with that advice, so the syntax stays free for later sugar.

### The IR

`$"a{part}b"`, with `part: @byte_buffer` and a root-namespace
`Formatter` (as in a UI test, which has no `Standard`):

```lilac-ir
!fmt0: format "a" {@@format(ReR9FormatterE)} "b"

function @main
{
  $entry:
    ...
    &3: ref #byte_buffer = addr %part
    format !fmt0 (&3) -> %msg
    ...
}
```

- **The `format` data entry** holds the pieces and one formatter per
  hole, printed as the f-string reads. Identical f-strings (same pieces,
  same formatters) share one entry. In memory it is the C struct
  `{ uintptr_t holes; struct { struct { uintptr_t len; const uint8_t
  *data; } text; void (*format)(void); } parts[holes + 1]; }`: each
  piece is a `#byte_buffer` in place, and the last part's `format` is
  null. The layout comes from `lilac-layout` like every other one, and an
  empty piece points at a real, empty constant (never null).
- **The `format` instruction** stores the hole addresses into an array
  that lives as long as the function's frame, then sets the
  destination's `template` and `holes` (null when there are none).
  `#format_args` becomes `{ template: ref #unknown, holes: ref #unknown }`
  (16 bytes on 64-bit, down from 264). `W` is not in LIR: every
  `@format_args(W)` lowers to `#format_args`.
- **The verifier** checks that an entry's formatters are functions with
  ins `ref #T_i, ref #W` (one `#W` for the whole entry) and no outs, that
  an instruction passes exactly `holes` registers, and that register `i`
  is a `ref #T_i`. DCE keeps a live instruction's entry, and an entry
  keeps its formatters.
- **A formatter is an ordinary place-ABI function.** Its first in place
  holds the hole's address, which is exactly what `holes[i]` is, so a
  consumer passes `holes[i]`'s address and no thunk is generated.
- **LLVM** emits the entry as a constant with relocations to the text
  and the functions, and the instruction as an entry-block
  `alloca [k x ptr]`, `k` stores and two field stores. There are no calls,
  and no runtime function is linked until something consumes a format.
- **Consumption** is three small always-inline runtime functions in
  `lilac_ir::runtime` (`.lilac.format_args_holes`, `_piece`, `_hole`);
  `_hole` is one indirect call. Where `Formatter.write` is inlined at a
  constant template, LLVM can make those calls direct.

## Ground rules

- Every phase ends with the `AGENTS.md` gates green.
- Every bug lands with the test that reproduces it first: a run test for
  generated code, a `FAILS_` test for a new error, a unit test otherwise.
- Phases 4 and 5 change generated code on purpose and re-bless the
  `tests/ui` snapshots once each, read line by line in review. Every other
  phase keeps them byte-identical.
- `SPEC.md`, `IR_SPEC.md`, `STDLIB.md` and `AGENTS.md` are updated in the
  phase that changes what they describe.

Order: 0, then 1, 2 and 3 in any order, then 4, 5 and 6. Phase 7 follows
Phase 4.

## Phase 0: Pin today's behaviour

Each test defines its own small formatter type and hooks, so it passes
at Phase 4 without waiting for `Standard`.

- **0.1 Owned holes.** A `// pending:` run test written for the new
  model: a structure with `@on_drop` and a `@format`, used in a hole and
  then again. Target: it compiles, prints once, and drops once. (Written
  today with a `@conversion` to `@byte_buffer`, the same program is
  `E063` on the second use.)
- **0.2 Integer holes.** A `// pending:` run test formatting two integers
  in one f-string. Target: both print correctly.
- **0.3 No cap.** A `// pending:` run test with 40 holes. Today: `E061`.
- **0.4 Panics.** A `// pending:` run test, `@panic($"bad {x}")`, with a
  test runtime whose `@compiler_print` takes a format. Target: `panic:
  bad 7`, exit 101. It passes at Phase 5.

## Phase 1: Type aliases

- **1.1 Syntax** (`lilac-lex`, `lilac-parse/src/items.rs`): the `alias`
  keyword (and `Token::describe`); `Name: alias Type` at the top level.
  Inside a type body the parser accepts it and the resolver rejects it
  (`E036`), as with nested types; `Name[T]: alias …` is `E001`.
- **1.2 AST and resolve**: `ItemKind::Alias { ty }` and `DefKind::Alias`,
  visible and reachable like any item; a duplicate name is `E020`.
- **1.3 Typeck**: an alias resolves to its type on first use and is
  memoized; a cycle is `E074` at the alias that closes it; arguments on an
  alias, and an alias of a generic without its arguments, are `E044`.
  Static paths and struct literals through an alias of a structure resolve
  to the structure. Display goes through a table of the types exactly one
  alias names (`Types::display`, `display_bounded`); mangling and layouts
  ignore it.
- **1.4 Language server**: hover (`Format: alias @format_args(Formatter)`),
  definition, references, rename, completion, and semantic tokens as a
  type.
- **1.5 Tests and docs**: parser, resolver and typeck unit tests;
  `FAILS_` tests for `E074`, `E036`, `E044` and `E020`; a run test with
  aliases of a builtin, a structure, a generic instance and a function
  type; LSP tests for hover and rename. `SPEC.md` §6 (a "Type aliases"
  subsection), `STDLIB.md` §0.1 (type aliases exist; re-exports still do
  not), `docs/errors/E074.md`, and the TextMate grammar's keywords.

## Phase 2: Lazy hooks of generic instances

- **2.1** Today `register_instance_hooks` (`types.rs`) requests every
  member hook of each generic instance, which queues every body. Instead,
  register the signature (so lookups find it) and park the body; queue it
  on its first use (`Checker::use_hook`: a hook call, an operator, a
  conversion, an f-string hole), and record that use as the instance's
  origin, so a body that fails is `E042` at the use. `@on_drop`,
  `@on_copy` and `@on_override` stay eager: glue uses them wherever the
  type drops or copies. `check_selectors` already parks hook bodies this
  way, so share its mechanism. Every program that compiles today still
  compiles. A generic type's `@add` (or `@format`) that fails for some
  `T` now errors only where it is used with that `T`.
- **2.2 Tests and docs**: unit tests (a generic `@add` that fails for one
  `T` and is never used with it; then used, `E042` at the use); `SPEC.md`
  §7 (a hook in a generic body is checked per instance, when used).

## Phase 3: Format templates in the IR

- **3.1 Data** (`lilac-ir`): `LirModule::formats`, each a `LirFormat {
  name, pieces: Vec<Vec<u8>>, formats: Vec<Symbol>, span }`, printed
  `!fmt0: format "a" {@…} "b"`.
- **3.2 Instruction**: `LirInst::Format { template, holes: Vec<Reg>, dst,
  span }`, printed `format !fmt0 (&3) -> %msg`.
- **3.3 Layouts**: the new `#format_args`, and the template's part
  `#.lilac.format_part { text: #byte_buffer, format: ref #unknown }` in
  `builtin_fields`. The header plus parts is documented in `IR_SPEC.md`
  as the C struct above.
- **3.4 Verifier**: the checks in "The IR" above, plus each data symbol
  defined once.
- **3.5 DCE** (`dce.rs`): instruction → entry → formatters and pieces.
- **3.6 LLVM** (`lilac-codegen-llvm`): the constant, built at the
  layout's offsets (the backend computes no layout), and the
  instruction's lowering. `target_tests` checks the template layout on
  every target `Target::parse` accepts (32- and 64-bit pointers).
- **3.7 Stack traces**: `format` is not a call, so `trace::instrument` is
  unchanged.
- **3.8**: `lilac_ir = "0.5"`. `IR_SPEC.md`'s sentence "f-strings …
  introduce no new instruction shapes" gives way to the two forms above.
  Tests use hand-built modules (verifier accepts and rejects, printer,
  DCE, LLVM).

## Phase 4: Front end and consumers (the switch)

- **4.1 Syntax**: `FStrPart::Lit { bytes, span }` (`lilac-ast`,
  `lilac-parse/src/expr.rs`). A hole's top-level `:` or `,` is the spec
  `E001` above. Check that `parse_fstring` counts a nesting level (a hole
  can hold an f-string), as `AGENTS.md` requires of recursive syntax.
- **4.2 The type**: `@format_args(Type)` parses like `@array` (a
  `TypeShape` variant); `TypeKind::FormatArgs` carries its `W`, and
  `Builtin::FormatArgs` leaves the fixed builtins (later `TypeId`s
  shift). It displays as `@format_args(W)`, mangles as `w` followed by
  `W` (as `A` is followed by its element; `IR_SPEC.md`'s symbol grammar),
  is laid out as two pointers (`layout.rs`), and lowers to
  `#format_args`.
- **4.3 The hook** (`sigs.rs`, `hook_shape_ok`): `"@format"` is `(ref T,
  ref W)` with no returns and `T` not `unknown`, else `E022`.
- **4.4 HIR**: `HirExprKind::Format(HirFormat { formatter: TypeId,
  pieces, holes: Vec<HirHole { value, format: FuncKey }> })` replaces
  `FormatArgs`; `walk` and every match on it are updated (`places.rs`,
  `indices.rs`, `lilac-lower/src/constant.rs`: never constant).
- **4.5 Typeck**:
  - `W` from the destination, by the rules in "Typing" (a
    `format_source` beside `conversion_source`), and `E073`;
  - each hole: check, auto-deref, find `@format(ref T, ref W)` exactly
    (`E071` through `emit_requirement` with a new
    `Requirement::Format`), `RefOf` the value, and `use_hook` the
    formatter (Phase 2). Record the formatter as a use at the hole's
    span, for the language server. A failed hole does not stop the
    others from being checked, and an unknown `W` still checks every
    hole;
  - the slot rules in `coerce`: a literal becomes a hole-free `Format`,
    another value a one-hole one;
  - "Where a format can live": the type positions (fields, payloads,
    array elements, outs, globals, through `emit_requirement` so an
    instance folds into `E042`), stores through a `ref` and `@write`,
    the extension (it rewrites the `Let`), and a temporary hole assigned
    to an existing local;
  - the ownership pass reads holes (`self.expr(value, false)`) instead of
    consuming them.
- **4.6 Lowering**: `Lower::intern_format` (deduplicated, numbered in
  first-use order, so snapshots are stable) and the instruction, with
  each hole's register from `ref_value`; the three consumer intrinsics,
  with call-site bounds checks, and their runtime functions.
- **4.7 Removed**: `@format_args_new`, `@format_args_push`,
  `@format_args_count` (now `@format_args_holes`; old names are `E021`
  with a note saying what replaced them), their runtime functions, the
  16-piece layout, the stride plumbing (`format_args_stride_follows_
  target_width` in the driver), and `E061` (registry and page).
- **4.8 Tests**: re-bless `tests/ui/fstring.*` and `format_args_use.*`
  (each defines a small formatter type and its hooks); replace
  `FAILS_fstring_piece_cap` with a passing 40-hole run test; `FAILS_`
  tests for `E071`, each form of `E072`, `E073`, the spec `E001` and a
  bad `@format` shape; run tests for nested f-strings, a format consumed
  twice, a format never consumed (holes still evaluated once, in order),
  extended temporaries dropped once at block end after the format (an
  `if` arm's included), an owned hole left usable (0.1), the literal and
  one-value slot rules, two formatter types in one program, `{{`/`}}`,
  UTF-8 pieces, and a generic type whose `@format` formats its `T`;
  typeck unit tests for template-dependent holes (no error) and instance
  folding (`E042`); update `m3.rs`, `m5b.rs`, `runtime.rs` and the
  codegen tests that name the old runtime. Phase 0's tests but 0.4 lose
  their `// pending:` lines.
- **4.9 Docs**: `SPEC.md` §1, §2 "Formatted strings" (rewritten from this
  plan), §3, §8 (the `@format` hook) and the Lilac-to-IR example;
  `IR_SPEC.md`; `AGENTS.md`'s runtime bullet and pipeline;
  `docs/errors/E071.md`, `E072.md` and `E073.md`.

## Phase 5: Panics through `@compiler_print`

- **5.1 Shape** (`sigs.rs`, `platform_hook_shape_ok`): `@compiler_print`
  takes one `@byte_buffer` or one `@format_args(W)`; the `E022` message
  shows both.
- **5.2 Typeck**: `@panic`'s argument is checked against `M` (the
  program's hook's parameter type, else `@byte_buffer`), slot rules
  included, in place of `Intrinsic::Panic`'s fixed signature. The HIR
  records `M` (`HirProgram::panic_message`) for lowering and the runtime.
- **5.3 Lowering**: `panic_with` takes an `M` place, and generated
  messages (division, bounds) are built as `M`: a data constant, or a
  hole-free format.
- **5.4 Runtime**: `panic_runtime` takes `M` and writes each line through
  the hook as one, wrapping a frame's site in a one-piece template on the
  handler's stack. `hook_externs` declares the hook with `#byte_buffer`
  (no hook means `M` is `@byte_buffer`).
- **5.5 Re-entrancy**: the thread-local panic depth, a second slot beside
  the trace head (`@trace_head`'s storage, thread-local where the head
  is): depth 1 writes the fixed message and exits 101, depth 2 traps.
- **5.6 Tests**: 0.4 un-pended; a formatter that panics inside a panic
  (the fixed message, exit 101); a `@compiler_print` that panics (a
  trap); a bytes runtime unchanged (`tests/libs/TinyRuntime`); `E022`
  shape tests; snapshots re-blessed (`.lilac.panic`'s signature).
- **5.7 Docs**: `SPEC.md` §3 (`@panic`) and §8 (the platform hooks
  table); `IR_SPEC.md`'s panics section; `AGENTS.md`'s "Panics and stack
  traces".

## Phase 6: Standard

- `Formatter` and `Format` in the root `Standard` namespace, beside `Int`
  and `Optional`: every formattable type names `Formatter`, so `using
  Standard` brings it.
- `@format` for `@bit8`…`@bit64` and `@bit_native` (unsigned decimal),
  the `Int` types (signed decimal), `Bool`, `@byte_buffer`, `Format`
  (nested), and `Optional[T]`/`Result[T, E]` (generic hooks, checked per
  use from Phase 2).
- Wrappers: `hex()`/`binary()` on the integer types; `pad_left`,
  `pad_right` and `pad_center` over a `ref Format` (a counting formatter
  measures it first).
- `Io.print`, `print_line`, `print_error` and `print_error_line` take
  `Format` and write through a stack line buffer: one `write` per line,
  not per piece. `print_byte` and `print_char` stay until `Standard` has
  a `Char` type: a char literal is a `@bit32`, so `{'A'}` prints `65`
  until then.
- `Runtime`'s `@compiler_print` takes `Format`: a `Formatter` over
  standard error.
- `tests/run/stdlib` covers each hook and wrapper. `STDLIB.md` gets a
  formatting section, and its `ToString` convention gives way to
  `@format`.
- Floats wait for their own work item: shortest round-trip printing is a
  project of its own. Until then a float hole is `E071`, never wrong
  digits.

## Phase 7: Language server and editor

- Hover on a hole shows its type and its formatter (`Int.@format`);
  definition and references go from a hole to its hook, and a hook's
  references list the holes it formats.
- `E071` carries a quick fix that inserts a `@format` skeleton for the
  format's `W` into the type, when the type is declared in the project.
- The new intrinsics in completion and hover signatures, and
  `@format_args(` in the TextMate grammar. (Aliases get their editor
  support in Phase 1.)

## Out of scope (later, if wanted)

- `{x, spec}` sugar over the value model.
- Formats in fields, payloads, outs and globals: they need an escape
  analysis or lifetimes.
- Tracking the borrows of place holes (and of `ref`s in general).
- Generic aliases.
- A template identity for structured logging (`@format_args_template`):
  a static, per-call-site message id at no cost.
- Bounds and division panics that print the index and length. With a
  format-taking `@compiler_print`, lowering could build a two-hole
  message when the runtime has a `@format` for `@bit_native`.
- Libraries compiled apart from their runtime: both must agree on `M`.
  Every build is whole-program today.
