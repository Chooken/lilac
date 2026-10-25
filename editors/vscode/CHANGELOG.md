# Changelog

## 0.9.0

- Types are declared with `structure` and `either` (`A: structure {}`,
  `B: either { … }`): both are highlighted as keywords, and hovers,
  outlines and completion use them. `object` and `enum` are plain names
  now, and code that still declares a type with one shows the error with
  a quick fix to the new keyword.
- A mistake in a generic body that no type argument could fix is shown
  where it is, once, in the generic's own terms (`Holder[T]`), instead of
  as an `E042` at every call.

## 0.8.0

- Function values: hovers print `ref function (…) …` types (several
  outs in parentheses) and `ref id[@bit32]`-style instances, and
  signature help works for a call through a function reference (a
  local, a field such as `self.on_done(`, or a global such as
  `HANDLER(`), naming its slots from the declared `function (…)` or
  its alias.
- Inside an object or enum, its constants and static functions complete
  as bare names, and `Int.MAX.` lists the constant's type's members.
- Go to definition, references and rename follow function references
  and members used both bare and qualified.

## 0.7.0

- `not`, `!=` and the compound assignments `+=`, `-=`, `*=`, `/=`, `%=`
  are highlighted.
- Hovers print `(-a).f()` and `(not a).b` with their parentheses.
- Code in an f-string's `{…}` holes is highlighted as code, no longer in
  the string's color; only the text between holes is string.

- The server reads the workspace's `lilac.toml` (the manifest
  `lilac build` reads) instead of `lilac.json`: its root, libraries,
  target, mode and `link.libc` apply to every check, its mistakes are
  shown on it, and saving it rechecks. A workspace with only a
  `lilac.json` is told to move it. The extension starts in a workspace
  with a `lilac.toml`, so its errors show before a `.lilac` file opens.

## 0.5.0

- **Lilac: Restart Language Server** command, to pick up a rebuilt
  `lilac-lsp` or recover a stuck server without reloading the window.
- `as` (the cast keyword) is highlighted.

- The server compiles on a background thread: hovers, completion and other
  requests are answered right away, from the last compile, while the next
  one runs. Diagnostics from a compile that an edit has already overtaken
  are no longer shown.

- Completion: locals, parameters and items in scope, members after `.`
  (fields and methods of a value, a type's methods and variants, a
  namespace's items), variants after a bare `.`, intrinsics and builtin
  types after `@`, and keywords. It works mid-edit, before the file
  compiles again.
- Signature help for function, method and intrinsic calls, highlighting
  the argument being typed.
- Quick fixes: a misspelled name, type, field, method or variant offers the
  closest match (also shown as `help: did you mean ...?` by `lilac`).

- Find All References, highlighting of a name's other uses, the outline
  and breadcrumbs, Go to Symbol in Workspace, Go to Type Definition, and
  Rename. These work for locals and parameters as well as items. Rename
  refuses hooks and names used in a library or defined by the compiler.
- Go to Definition works on locals and parameters.
- Hovers show a field's type, a variant's payload, a constant's value and
  an intrinsic's signature.

- Diagnostics no longer wait for a burst of hovers or highlighting requests
  to end, and are tagged with the document version they were computed for.
- Errors without a location (an unreadable library folder) are shown on
  `lilac.json`, else the project's `main.lilac`, instead of a random open
  file.
- Files created, deleted or changed outside the editor (`git checkout`)
  and edits to `lilac.json` are picked up without reopening anything.
  Changing `lilac.libraries` no longer restarts the server.

- A `@compile_error` (or `@syscall` on a target without one) is also marked
  at every call that reaches it, in the caller's file, linking back to the
  error. An error inside a library now shows in your code. Unsatisfied
  generic requirements (`E042`) mark every instantiating call the same way.
- A constant `@array_at` index out of bounds is an error (`E037`) in every
  function, called or not.
- Generic functions nothing instantiates yet are type-checked: errors that
  depend on `T` are skipped, everything else (a misspelled name, a wrong
  concrete type, an out-of-bounds index) is reported.

## 0.4.0

- Add the `lilac.libraries` setting and `lilac.json` `libraries` field: mount
  libraries (the CLI's `--lib=Name=dir`) for the language server.
- Library files are checked as their namespace, so opening `lib/Standard`
  files no longer reports every sibling type as unknown.
- Generic bodies nothing instantiates are highlighted too, and tokens are
  refreshed after each compile (they were missing until the first edit).
- Add semantic highlighting: map the language server's semantic token types to
  standard TextMate scopes so themes color functions, types, enums, fields,
  variants, variables, and parameters.

## 0.3.0

- Add a live LIR view: `lilac.showLir` opens the current project's LIR in a
  read-only `lilac-lir:` virtual document that refreshes on edit.
- Add stage selection: `lilac.selectLirStage` chooses which pipeline stage the
  live IR view renders, and the view keeps its scroll position across refreshes.
- Add provenance navigation: hover-highlight and click-jump between an IR line
  and its source, plus a peek of the emitted IR for a source expression.

## 0.2.0

- Add a language server client that launches `lilac-lsp` and shows compiler
  diagnostics.

## 0.1.0

- Initial release: syntax highlighting for Lilac (`.lilac`) and Lilac IR
  (`.lir`).
