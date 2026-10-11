# Export, Linux Leaves and Editor Plan

> Status: all phases done. Source: the review of the OS layer and
> the editor on 2026-10-10.
> Goal: Lilac can define C-callable functions, the Linux-only intrinsics say
> so in their names, namespaces color like types, and completion after `.`
> lists only the variants the compiler knows are expected.
>
> Decisions made with the owner:
> - **No completion beats a wrong one.** Completion never guesses from the
>   text before the cursor; a `.` whose type the compiler does not know
>   offers nothing.
> - **`export` is `extern` with a body**: a C-ABI definition, the reserved
>   keyword (SPEC "export (reserved)"). It replaces `@thread_entry`.
> - **Linux-only intrinsics carry `linux_`**: `@linux_syscall`,
>   `@linux_thread_clone`. Apple supports only `libSystem`, and Windows
>   system-call numbers change between builds, so neither will work
>   elsewhere.
>
> - **Every `export` is kept**: each is a DCE root, in the project and in
>   mounted libraries alike. `--export=<name>` is removed.
>
> - **`x.` member completion** moved to recorded sites as well
>   (`HirProgram::members`, see AGENTS.md "Editing"). A name after a `.`
>   must be on the `.`'s line, for tags, members and path segments alike.

## Ground rules

- Every phase ends with the `AGENTS.md` gates green and is committed.
- Every bug fix lands with a test that reproduces it first.
- `AGENTS.md`, `SPEC.md`, `IR_SPEC.md` and `STDLIB.md` are updated in the
  phase that changes what they describe.

Order: 1 → 2 → 3 → 4. Phase 1 is small and independent; 2 must precede 3
(both touch the thread code); 4 is the largest.

---

## Phase 0: Constant `==` on enums (done, `70e6f5e`)

- `HirExprKind::Eq` in the constant evaluator
  (`crates/lilac-lower/src/constant.rs`) compares whole values, so enum and
  object equality is constant.
- A tag operand (`.Windows`, `.Some(x)`) takes its type from the other
  side (`needs_context` in `crates/lilac-typeck/src/expr.rs`).
- Tests: `tests/run/const_cond_enum_eq.lilac`, and the `==` form in
  `tests/run/conditional_items`.

---

## Phase 1: Namespaces color like types (done)

As built, 1.2 places segments by offset only when a path's span is
exactly its text (`paths::segment_spans`); a path written with space
between segments is left uncolored, never colored wrong. Typeck flattens
member chains into dotted names in many places, so carrying parser spans
through all of them was not worth it. Top-level conditions are colored
too (the driver keeps the selector checks' sites).

- **1.1 `SemanticKind::Namespace`.** Add the variant in
  `crates/lilac-hir/src/lib.rs` (`SemanticKind`).
- **1.2 Expression and type paths.** In `crates/lilac-typeck/src/paths.rs`,
  record a `Namespace` site for each segment that resolves to
  `Segment::Ns` while `lookup_item` and `path_prefix` walk a path
  (`Os` in `Os.write`, `@builtin` and `Target` in `@builtin.Target.os`,
  `Standard.Os` in a type path). Take segment spans from the `Expr::Member`
  nodes for expressions. For type paths (`TypeShape::Named` holds one
  dotted string), make the parser keep one span per segment rather than
  computing offsets from text.
- **1.3 Aliases.** A path head that is a `using … as` alias
  (`Resolved::alias_span`, already recorded as an `AliasSite`) also gets a
  `Namespace` site, as does the alias name in its `using`.
- **1.4 `using` paths.** Give `ItemKind::Using` (`crates/lilac-ast`) one
  span per path segment (parser: `crates/lilac-parse/src/items.rs`). The
  resolver records a `Namespace` site for each segment it resolves, and
  carries them into the analysis's `semantics` (the resolver has no
  `HirProgram`: add the sites to `Resolved` and merge them in the driver).
- **1.5 Language server.** Add `SemanticTokenType::NAMESPACE` to
  `TOKEN_TYPES` (`crates/lilac-lsp/src/lib.rs`) and map it in
  `handlers::semantic_type`.
- **1.6 VS Code.** In `editors/vscode/package.json` `semanticTokenScopes`,
  map `"namespace": ["entity.name.type.namespace"]`: themes that color
  `entity.name.type` color it like a type. Update
  `test/semanticTokenScopes.test.js`.
- **1.7 Tests.** In `crates/lilac-lsp/tests/semantic_tokens.rs`: a
  `using Standard.Os as Os` line (both segments and the alias), a call
  `Os.write(…)`, a type path `Os.Handle`, and `@builtin.Target.os` (two
  namespaces, then a constant).

---

## Phase 2: `linux_` prefix on the Linux-only intrinsics (done)

A mechanical rename; no behavior changes.

- **2.1 Intrinsics.** In `crates/lilac-hir/src/intrinsics.rs`, rename the
  names `@syscall` → `@linux_syscall` and `@thread_clone` →
  `@linux_thread_clone` (the table near line 436 and `name()` near line
  699), and the variants to `LinuxSyscall`/`LinuxThreadClone`.
- **2.2 Leaves.** Rename the `LirCallee::Builtin` names and their entries
  in `lilac_ir::builtin_sig`, `lilac_ir::check_live_syscalls`,
  `crates/lilac-codegen-llvm/src/lower.rs` (`builtin_symbol`: symbols
  `.lilac.linux_syscall`, `.lilac.linux_thread_clone`), `runtime.rs`
  (`emit_runtime`) and the codegen tests in `lib.rs`.
- **2.3 Library.** Update every use in `lib/Standard/Os/Linux/*.lilac`,
  `lib/Standard/Os/thread.lilac` and `tests/libs/TinyRuntime`.
- **2.4 Docs.** `E035` page and message, `SPEC.md`, `IR_SPEC.md`,
  `STDLIB.md`, `AGENTS.md`. Leave `docs/history` as written.
- **2.5 Check.** `git grep -n "@syscall\b\|@thread_clone"` finds only
  history; the Linux cross builds still link (`--target` x86-64 and
  aarch64, see `AGENTS.md`).

---

## Phase 3: `export`, C-ABI definitions (done)

As built: the export's body keeps its ordinary symbol (`.root.compare`),
not `.lilac.export.<symbol>`, since `.lilac.` frames are hidden from stack
traces; the C entry is `LirFunc::export`. Exports are DCE roots once the
module has a root (`main` or the entry stub); a library checked alone is
not pruned, as before. A non-variadic `extern` named as a value is a C
function too (it was a place-ABI value, which miscompiled a call through
it). `@linux_thread_clone` takes the entry and its argument and puts both
on the child's stack. Every program that mounts `Standard` keeps its
`thread_start` export (`lilac_thread_start`) and `null`.

- **3.1 Syntax.** Make `export` a keyword (`crates/lilac-lex`). Parse
  `name: export function (…) outs { body }` and
  `name: export "symbol" function …` beside `extern`
  (`crates/lilac-parse/src/items.rs`); `FuncDecl` gets `is_export` (an
  export has a body; a body on an `extern` stays an error). An export is a
  top-level free function, never a method or generic (new error code with
  its page).
- **3.2 Signatures.** Exports take the same C-ABI rules as externs:
  `lilac_ir::is_c_abi_type` params, at most one output, `E028` otherwise,
  checked by `lilac_lower::check_extern_signatures`. The symbol is the link
  name, else the declared name, unmangled
  (`lilac_lower::extern_symbol`). Two exports, or an export and a defined
  function, with one symbol is an error (the verifier's duplicate check
  already rejects it in LIR; report it as a diagnostic first).
- **3.3 Lowering.** The body lowers as an ordinary place-ABI function under
  an internal name (`.lilac.export.<symbol>`), and `LirFunc` gains an
  `export: Option<Symbol>` naming the C symbol. The backend emits the C
  entry: take the arguments by value, store each into a stack slot, call
  the body with the slots, load and return the output. This is
  `threads::thread_entry` generalized; that function then goes.
- **3.4 Roots.** The driver adds every export, the project's and the
  libraries', to the DCE roots (`lilac_ir::dce`'s `extra_roots`); remove `Session.exports`, `--export=<name>` and
  `lilac_lower::export_symbol`, and update the run tests and SPEC text that
  use them.
- **3.5 C function values.** An `export` named as a value has a new type,
  `export function (…) outs`: a C function pointer (`TypeShape::Function`
  and the `lilac-ty` function type gain an ABI flag). It is a C-ABI type
  (`is_c_abi_type`), so it passes to `extern`s; it converts with
  `as ref unknown`; and calling one is an indirect call with the C ABI
  (`LirCallee::Reg` gains the ABI, or a separate callee kind). A place-ABI
  `function` value never converts to it.
- **3.6 Threads.** In `lib/Standard/Os/thread.lilac`, define the thread
  start as an `export` (reads the start block, calls `entry(argument)`,
  returns null) and pass it to `CreateThread`/`pthread_create`. The Linux
  `clone` child calls it too (pass its address to `@linux_thread_clone`
  instead of the backend's `.lilac.thread_entry`). Then delete
  `@thread_entry` (`Intrinsic::ThreadEntry`, its `builtin_sig` row,
  `.lilac.thread_entry.address`, docs).
- **3.7 Docs.** Replace SPEC "export (reserved)" with the rules; update
  `AGENTS.md` ("Extern C ABI", "Threads") and `STDLIB.md`.
- **3.8 Tests.**
  - Run test (`// libc`): an `export` comparator passed to C `qsort`,
    which calls it back.
  - Run test: an export with an `@bit32` result is called from Lilac
    through its `export function` value.
  - Check tests: a method/generic export, a non-C-ABI parameter (`E028`),
    a duplicate symbol.
  - The thread run tests still pass, natively and under `--libc`; the
    Linux and macOS cross builds link.

---

## Phase 4: Completion after `.` from the compiler (done)

As built: a tag's name must follow its `.` directly (SPEC), so a `.`
ending a line does not take the next line's name; the empty tag's `E001`
is at the `.`. Tag sites carry the variant names and payload types as
text (`TagSite::variants`), since the selector checks use their own type
tables. A parked completion is answered when the newest generation
compiles; one asked after a compile panicked gets no answer. A tag arm
with a name but no body also recovers (with its `E001`).

- **4.1 Parser recovery.** A `.` with no name after it parses as a tag with
  an empty name, with one `E001`, and parsing continues: in expressions
  (`crates/lilac-parse/src/expr.rs` near line 307) an `Expr::Tag("", span)`,
  in `match` arms (`stmt.rs` near lines 253 and 278) an arm with an empty
  tag and no body. Test: the rest of the function still checks (today a
  bare `.` in a `match` loses the function, reported as
  `expected declaration name`).
- **4.2 Tag sites.** Typeck records every tag position with the enum it
  expects: `HirProgram::tags: Vec<TagSite { span, ty: Option<TypeId> }>`.
  Patterns get the scrutinee's type (`body.rs` `check_pattern`, and
  `check_match_value`); expression tags get their expected type
  (`expr.rs` near line 52, `calls.rs` near line 96 for `.Some(x)`), `None`
  when there is none. An empty tag records its site and reports nothing
  beyond the parse error.
- **4.3 Top-level `match`.** Conditional items are removed before the final
  check, so `lilac-driver/src/conditional.rs` keeps the tag sites from the
  selector checks and adds them to the analysis.
- **4.4 Completion waits for the current text.** In
  `crates/lilac-lsp/src/server.rs`, a completion request is parked with
  the generation it was asked at, and a compile starts now (skipping the
  debounce). When a compile of that generation or later finishes, the
  parked request is answered from it; a later edit keeps it parked until
  the next compile. Cancellation (`$/cancelRequest`) drops it.
- **4.5 Answer.** After a bare `.`: the tag site containing the cursor; if
  its type is a known enum, that enum's variants (in a `match` pattern,
  minus the variants other arms already name); otherwise no items.
  Remove `Scope::variants`.
- **4.6 Docs.** `AGENTS.md` "Editing" (completion now waits for a compile
  for tags).
- **4.7 Tests.** In `crates/lilac-lsp/tests/editing.rs`:
  - `match c { .Red => … ⟨.⟩ }` lists only `Color`'s remaining variants.
  - `x: Color = .`, `f(.)`, and `os == .` list that enum's variants.
  - A `.` with no expected enum lists nothing.
  - Top-level `match @builtin.Target.os { . }` lists the `Os` variants.
  - An edit arriving while the request is parked: the answer comes from
    the newer compile.
