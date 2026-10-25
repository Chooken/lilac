# Editor Plan

> Status: proposal. The decisions below were agreed with the owner on
> 2026-10-13.
>
> Goal: what the editor says about code comes from the compiler's own
> structures (tokens, the syntax tree, the definition table, the intrinsic
> table), never from scanning text or re-parsing strings it wrote itself.
> On that footing: doc comments, and hovers that read like declarations.
>
> Decisions:
> - **`///` is a doc comment** on the declaration that follows it: an item,
>   a member, a field or a variant. The text is Markdown, shown in hovers,
>   completion and signature help.
> - **A doc comment that documents nothing is a plain comment.** No error,
>   no hint.
> - **`Standard`'s item comments become doc comments.**
> - **`@memcpy`, `@memmove` and `@memset` take `ref unknown`**, like C's
>   `void *` and like `@ptr_add`'s result. A `ref T` still needs one `as
>   ref unknown`; there is no implicit conversion.
> - **Namespace docs (`//!`) are later.** A namespace spans files, so
>   which file documents it needs its own design.
> - **Quick fixes that insert code wait for a formatter** (see `E071`,
>   removed in `91ceeea`).

## Problems this fixes

Read from the code on 2026-10-13:

1. **Intrinsic signature help is wrong.** `complete::signature_help`
   renders a label, then finds the parameters again with `find('(')`,
   `find(')')` and a split on `,`. For `@format_args_hole(@format_args(W),
   index: @bit_native, out: ref W)` the first `)` closes `@format_args(W)`,
   so the highlighted parameter is wrong.
2. **Intrinsic signatures are partly hand-written strings**
   (`handlers::intrinsic_signature`: thirteen `special` cases), and the
   rest have no parameter names: `@memcpy` hovers as `@memcpy(ref @bit8,
   ref @bit8, @bit_native) nothing`, which does not say which is the
   destination.
3. **Function hovers have no parameter names** and are not Lilac:
   `function main.copy(ref @bit8, @bit_native) -> @bit8`. Builtin types
   (`@bit8`, `@byte_buffer`, `@bit_native`) have no hover at all.
4. **Field, variant and constant hovers copy source text**
   (`handlers::signature`): the declaration's span, whitespace squashed,
   cut at 160 characters. A comment inside the declaration ends up in the
   hover.
5. **Signature help finds its call by scanning bytes backwards**
   (`complete::call_at`), toggling a string flag at each `"`. Escaped
   quotes, `'('`, comments and f-string holes confuse it, and it gives up
   at any `{`, so `f(P { x = 1 }, |` has no help.
6. **Completion's cursor context is byte scanning.** `in_comment` is "a
   `//` earlier on the line", so `"http://|` gets no completion;
   `needs_current_compile` and `path_before` know nothing of strings, so a
   `.` typed in a string parks a member completion.
7. **The compile unit is chosen by text** (`compile::unit_of`): a file
   outside the project is a program when a line starts with `main:`.
   `main : function` (a space before the colon) does not, and a global
   named `main` does.
8. **Positions rescan the file.** `convert::position`, `byte_offset` and
   `line_len_utf16` walk from the top for every call; the compiler already
   keeps each file's line starts (`SourceFile::line_starts`).
9. **The edit since the last compile is guessed.** The server asks for
   full-text sync, and `complete::map_offset` diffs the common prefix and
   suffix of the two texts. With several edits between compiles, a cursor
   inside the union of the edits maps to its start.
10. **Rename re-implements the identifier rule** (`navigate::is_identifier`)
    instead of asking the lexer.
11. **`ref T` to `@memcpy` takes two casts** (`as ref unknown as ref
    @bit8`), and a `@ptr_add` result takes one.

## Ground rules

- Every phase ends with the `AGENTS.md` gates green, and is one commit.
- Every bug lands with the test that reproduces it first.
- Only Phase 2 changes generated code (the `@memcpy` slots), and it keeps
  the `tests/ui` snapshots byte-identical: a `ref unknown` and a `ref
  @bit8` lower to the same LIR pointer.
- `SPEC.md`, `STDLIB.md` and `AGENTS.md` are updated in the phase that
  changes what they describe.

Order: 0, then 1, 2, 4 and 5 in any order, then 3 (it needs 1 and 2),
then 6 (it needs 1 and 2).

## Phase 0: Pin today's behaviour

Failing editor tests (`crates/lilac-lsp/tests/`), each marked `#[ignore]`
with the phase that fixes it, and un-ignored there:

- signature help on `@format_args_hole(args, 0x0, |` highlights `out`
  (problem 1);
- signature help after `f(P { x = 0x1 }, |` and after `f("a)", |`
  (problem 5);
- after `f("a//b", point.|` a member completion is due (the `//` is in
  a string), and after `s = "x.|` none is (problem 6);
- a lone file declaring `main : function` compiles alone (problem 7);
- a field hover with a comment inside its declaration (problem 4);
- two separate edits before a compile, then completion inside the
  second (problem 9).

## Phase 1: Doc comments

- **Lexer.** `///` followed by anything but `/` starts a doc comment
  (`////` is a plain comment, as in Rust). It stays trivia, so the token
  stream is unchanged, and the lexer records it as side output, like
  its diagnostics: `Lexer::take_comments` gives every comment's span and
  kind (`Line`, `Doc`) in source order. Phase 4 uses the plain ones too.
- **Parser.** A run of doc comments on consecutive lines, ending on the
  line just before a declaration, documents it. A blank line or a plain
  comment between them ends the run. Only a declaration takes one: an
  item (`Item::doc`), a member or
  field (both are `Item`s), or an either variant (`EnumVariant::doc`).
  The text is each line after `///` and at most one space, joined with
  `\n` (`lilac_ast::Doc`, which keeps the span too). Anything else
  leaves the run as a plain comment.
- **Resolver.** `Def::doc: Option<Doc>`, copied from the AST, so every
  `DefId` has its doc (the language server's `DefInfo` takes it).
- `SPEC.md`: a "Comments" paragraph: `//`, `///`, what a doc comment
  attaches to, that it is Markdown.
- Tests: lexer (`///`, `////`, `/// ` at end of file, CRLF), parser (each
  declaration kind, blank-line and plain-comment breaks, stray runs),
  resolver (a member's and a variant's doc).

## Phase 2: Intrinsic and builtin tables

The intrinsic table (`lilac-hir/src/intrinsics.rs`) becomes the only
description of an intrinsic, so the editor renders, never writes, one.

- **Named slots.** `Signature::params` becomes `Vec<Param { name, slot
  }>` with every intrinsic's parameter named (`@memcpy(destination: ref
  unknown, source: ref unknown, length: @bit_native) nothing`).
- **Every shape is a slot.** The thirteen hand-written signatures become
  slots: `Slot::Type` (`@size_of(T)`), `Slot::Place` (`@move`,
  `@discard`), `Slot::Array` and `Slot::Element` (`@array_*`),
  `Slot::Param(name)` (`ref T` in `@write`/`@read`/`@drop`),
  `Slot::FormatArgs` and `Slot::Writer` (the format consumers),
  `Slot::Message` (`@compile_error`). Typeck's special checking stays
  where it is; the slot only describes it, and a unit test checks that
  every intrinsic whose check is special has a describing slot.
- **Docs.** `Intrinsic::doc()` (one or two sentences each, in the table),
  and `Builtin::doc()` in `lilac-ty` for every builtin type.
- **`@memcpy`, `@memmove`, `@memset`** take `ref unknown` for their
  pointers. Lowering already passes a pointer, so the LIR is unchanged;
  the callers in `lib/Standard` and `tests/run` drop their `as ref
  @bit8` casts. `SPEC.md`'s intrinsic list is updated.

## Phase 3: One signature renderer

`lilac-lsp/src/render.rs` renders every declaration as Lilac source
writes it, from the definition table, the AST declaration and typeck's
types, and returns where each parameter is:

```
Rendered { text: String, params: Vec<(u32, u32)> }  // UTF-16 ranges
```

- Functions and hooks: `copy: function (into: ref @bit8, count:
  @bit_native) @bit8`, with the owner (`Point.length: function (self)
  Int`), generic parameters (`get[T]: …`), failure outs (`… else
  Error`), `extern`/`export`, and `=>` bodies left out.
- Fields `x: Int` (in `Point`), variants `Some: T` (in `Optional[T]`),
  constants `limit: Int => 0x40`, globals, aliases `Format: alias
  @format_args(Formatter)`, structures and eithers (`Point: structure` and its
  fields, up to a limit, from the definition table, never source text).
- Intrinsics from Phase 2's table, builtin types with `Builtin::doc()`,
  `@array`/`@format_args` from the slot descriptions.
- Hovers are the rendered declaration in a `lilac` block, then the doc.
  Signature help uses `params` for its highlights (no re-parsing) and
  shows the doc; completion items carry the rendered text as `detail` and
  the doc as `documentation`.
- Un-ignores the problem 1 and 4 tests.

## Phase 4: Cursor context from tokens

`lilac-lsp/src/context.rs` lexes the document's current text (the lexer
never fails) and answers every "what is at the cursor" question from the
tokens and Phase 1's comment spans:

- in a comment, in a string literal, or in an f-string hole (which is
  code);
- the token before the cursor (`.`, `@`, a name) and the dotted path
  ending there (replaces `in_comment`, `needs_current_compile` and
  `path_before`);
- the call the cursor is in: matching brackets over tokens, so strings,
  characters, comments, holes and struct literals are skipped correctly,
  and the argument index counts commas at the call's own depth (replaces
  `call_at`);
- whether a name is an identifier: one `Ident` token (replaces
  `is_identifier`).
- `unit_of` asks the parser whether the file declares a top-level `main`
  function.
- Un-ignores the problem 5, 6 and 7 tests.

## Phase 5: Positions and edits

- **A line index per document.** `convert` maps positions through a
  `LineIndex` (line starts, plus whether each line is ASCII so a column
  is a subtraction) built once per text: `SourceFile::line_starts` for
  compiled files, one built on change for open documents.
- **Incremental sync.** The server asks for
  `TextDocumentSyncKind::INCREMENTAL`, applies each change to its copy,
  and records the edits since the last compile's snapshot. `map_offset`
  maps through those edits exactly (an offset inside replaced text maps
  to the replacement's start), instead of diffing.
- Un-ignores the problem 9 test.

## Phase 6: Standard

- Every comment directly above an item, member, field or variant in
  `lib/Standard` becomes `///`. Section headers and comments inside
  bodies stay `//`. Each converted comment is read: one that describes
  how a body works, not what the item is, moves into the body.
- `STDLIB.md` notes that the docs live in the source.
- A test hovers a few `Standard` items (`pad_left`, `Formatter.write`,
  `Optional.Some`) and checks their docs show.

## Out of scope (later, if wanted)

- `//!` namespace docs.
- Doc comments on parameters, generic parameters and locals.
- Doc tests and a `lilac doc` generator.
- A formatter, and with it fixes that insert code (`E071`'s missing
  `@format`).
- An implicit `ref T` to `ref unknown` conversion.
