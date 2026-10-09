# Lilac `continue`, Platform Hooks and Low-Level Builtins Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** move every operating-system call out of the compiler and into
library code, and give that library code the low-level tools it needs. In
order:

- **A.** `continue`.
- **B.** `@builtin.Target` / `@builtin.Build`: target and build facts as
  constants, so library code picks a platform with constant conditions.
- **C.** `extern` link names and variadic externs.
- **D.** Instruction builtins: pointer math, alignment, `@memmove`/`@memset`,
  `@syscall`, `@trap`.
- **E.** Eager library loading: every mounted library is loaded and checked
  whole.
- **F.** Platform hooks `@entry`, `@exit` and `@compiler_print`. Removing the
  `@os_*` builtins and the compiler's OS layer, and rewriting
  `Standard.Runtime`/`Io`/`Process`/`Heap` on externs and `@syscall`.
- **G.** Atomics.
- **H.** `@array(T, N)`.

**Why:** today the compiler contains a per-OS runtime (`kernel32` calls,
Linux system calls, libc) in `lilac-ir::runtime`. Each new OS feature
(files, directories, page memory) would mean more compiler code. Rust and Zig
keep OS code entirely in libraries:
- Zig's `@import("builtin")` gives library code target constants;
- Rust's `#[panic_handler]` lets the program supply what the compiler needs
  from the platform.

After this plan the compiler only emits instructions. A library declares
`extern`s or issues `@syscall`s chosen by `@builtin.Target`, and supplies
three hooks.

**Tech stack:** Rust workspace, no new dependencies.

## Design decisions

### A. `continue`

1. `continue` runs the drops for every scope opened inside the innermost
   `loop` body, then branches to the loop head. For
   `loop if c { … } else break` the head re-tests `c`, which gives
   `while`-style behavior with no special case.
2. Outside a `loop` it is `E022`, the same as `break`.
3. Its drops point at the `continue` keyword (`drop_scope(fs, index, *span)`).
4. No labels. `break` has none either.

### B. `@builtin`

5. **`@builtin` is a compiler-provided namespace.** `@`-names are already
   reserved, so it cannot collide with user code. It holds:

   ```lilac
   @builtin.Os: enum { Windows, Linux, MacOs, None }
   @builtin.Arch: enum { X86_64, Aarch64, … }   // one per lilac-target Arch
   @builtin.Mode: enum { Debug, Release, ReleaseSmall }

   @builtin.Target.os: Os => …
   @builtin.Target.arch: Arch => …
   @builtin.Target.pointer_size: @bit_native => …
   @builtin.Target.page_size: @bit_native => …   // 4096; 16384 on aarch64-apple-darwin
   @builtin.Target.libc: @bit8 => …              // 0x1 under --libc and on macOS

   @builtin.Build.mode: Mode => …
   ```

   Every member is an inline constant or enum, so `match @builtin.Target.os`
   and `if @builtin.Target.libc` compile only the live arm (SPEC "Constant
   conditions").
6. **Implementation:** the driver generates one source file from `Target` and
   the session, and adds it to the module load as namespace `@builtin`. It
   uses the ordinary resolver and typeck paths; the only new part is letting
   the lexer and parser accept `@builtin` as a namespace segment. The
   file's display path in diagnostics is `<builtin>`.
7. `Build` gets user `--define=name=value` constants later; this plan only
   adds `mode`.

### C. Externs

8. **Link names:** `os_write: extern "write" function(…) …`. The Lilac name is
   namespaced and mangled like any other function; the string is the symbol
   the linker sees. Without a string the link name is the declared name, as
   today.
9. **Collisions:**
   - two externs with the same link name and identical signatures become one
     declaration;
   - different signatures are `E028` ("`write` is declared with two
     signatures"), labelling both declarations.
10. **Variadic externs**, for calling C only. A trailing `...` parameter
    `open: extern "open" function(path: ref @bit8, flags: @bit32, ...) result: @bit32`.
    - **Where it is allowed:** only in an `extern`. Anywhere else it is a
      parse error. Lilac functions cannot be variadic, since that would need
      `va_list` builtins.
    - **Merging:** the variadic flag is part of the signature for decision 9.
    - **Function values:** none (`E028`), because function types have no
      variadic form.
    - **Extra arguments are checked with no expected type:**
      - an untyped literal is an error ("a variadic argument needs a type";
        hint: `0o666 as @bit32`);
      - `@bit8`, `@bit16` and `@fbit32` are `E028`, because C promotes them
        and Lilac integers carry no sign to promote with (hint: widen
        explicitly);
      - accepted: `@bit32`, `@bit64`, `@bit_native`, `@fbit64` and any `ref`.

      None of these has hooks, so they are plain copies.
    - **LIR:**
      - `LirFunc::variadic`;
      - a `Call` to a variadic extern may have more `ins` than the
        declaration;
      - the verifier checks the extras against the accept-set above;
      - printed as `extern @open …, ...`.
    - **LLVM:** `fn_type(&fixed, true)` and a call with every value. LLVM
      does the per-ABI work:
      - Apple arm64 puts varargs on the stack;
      - x86-64 System V sets `%al`;
      - Windows x64 copies float varargs into the integer registers.

      A fixed prototype gets Apple arm64 wrong, which is why this is needed
      for `open`.

### D. Instruction builtins

11. **Pointer math:** `@ptr_add(ref unknown, @bit_native): ref unknown`
    (the existing `.lilac.ptr_add` leaf),
    `@ptr_to_address(ref unknown): @bit_native` and
    `@address_to_ptr(@bit_native): ref unknown`.
    - Typed access then uses the `ref unknown ↔ ref T` cast and auto-deref.
    - Null is `@address_to_ptr(0x0)`.
12. **`@align_of(T)` / `@align_of(value)`:** a constant, like `@size_of`.
13. **`@memmove(dst, src, len)` and `@memset(dst, byte, len)`:** these sit
    beside `@memcpy` and lower to `llvm.memmove`/`llvm.memset`.
14. **`@syscall(number, a0…a5): @bit_native`:** the existing backend leaf,
    exposed. Library code passes fewer than six arguments as `0x0`.
    - It is supported on x86-64 and aarch64 Linux.
    - On any other target it is a lowering error (new code `E035`), so
      Standard keeps it in Linux-only arms.
15. **`@trap()`:** the existing `@lilac_abort` leaf (`llvm.trap`), exposed.

### E. Eager library loading

16. **Every mounted library is loaded and checked whole.**
    - `lilac-module` reads every `*.lilac` file in every folder under each
      `--lib`/`lilac.json` library, not only the folders a `using` names.
    - `using` only controls which names a file can see.
    - Errors in unused library code are reported on every build, and a
      library's hooks (part F) are present whenever it is mounted.
17. **Non-generic items are type-checked eagerly.** Generic bodies are still
    checked per instantiation; they cannot be fully checked without a `T`
    (that is why the LSP's `check_for_editor` discards their diagnostics).
18. **Output is unchanged:** DCE still removes everything unreachable. The
    cost is compile time.

### F. Platform hooks and the OS layer

19. **Three top-level hooks the compiler calls, each declared at most once in
    the program:**

    | Hook | Signature | Called |
    | --- | --- | --- |
    | `@entry` | `function() nothing` | by the process entry stub |
    | `@exit` | `function(code: @bit32) nothing` | after `@entry` returns (code 0), and by the panic handler (code 101) |
    | `@compiler_print` | `function(text: @byte_buffer) nothing` | by the panic handler, for the message and each trace line |

20. **The entry stub stays in the compiler.** It makes no OS calls:
    - it is `.lilac.start` natively (with the x86-64 Linux `stackrealign`
      attribute), or the C `main` under `--libc`;
    - its body is `@entry(); @exit(0x0); trap`.
21. **The panic handler stays compiler LIR** (`lilac_ir::panic_runtime`).
    - It walks the shadow stack as now, writing through `@compiler_print`.
    - It then calls `@exit(101)`, then traps.
    - A wrong `@exit` that returns therefore stops at a trap rather than
      running on.
    - `@build.stack_traces` and trace-walking builtins are not needed.
22. **`@entry` calls `main` itself.**
    - Inside an `@entry` body, a name not found in the hook's own scope is
      looked up in the program's root namespace.
    - Standard calls `main()`. Another runtime library can call any name it
      likes.
    - Reachability makes `@entry` the root, replacing `main`/`.lilac.start`
      as DCE roots.
    - When the lookup fails, the error is `E032`, reworded: "the program has
      no `main`, which `@entry` calls". It labels the call in `@entry` and
      notes the project root.
23. **When the hooks are required:**
    - **Building an executable** (`Session.require_entry`): all three are
      required. A missing hook is `E033` (with a hint to mount `Standard` or
      define the hook), and a second declaration is `E034`.
    - **Otherwise** (`check`, `emit-*`, and static libraries later): every
      hook is optional, and `@entry` is not a root. A program that lacks
      `@compiler_print`/`@exit` calls the extern symbols
      `.lilac.compiler_print`/`.lilac.exit` instead. A hook, when present,
      is emitted under that same fixed symbol.

      So a static library's panics resolve against the final executable's
      hooks at link time (Rust's `#[panic_handler]` model). A future
      static-library output makes `@entry` an error.
24. **The OS layer leaves the compiler:**
    - `@os_write`, `@os_exit`, `@os_alloc` and `@os_free` are removed;
    - so are `System`, `Syscalls` and the per-OS functions in
      `lilac-ir::runtime`.
    - `Entry` (native or C `main`) stays, because it picks the stub.
    - `Target::link_info` stays: linking is still the compiler's job.
25. **Standard supplies the hooks and the OS calls:**
    - `Standard.Runtime` declares the hooks;
    - `Standard.Os` holds the raw bindings (per-OS `extern`s with link names,
      and Linux `@syscall` wrappers), chosen by `match @builtin.Target.os` and
      `@builtin.Target.libc`;
    - `Io`, `Process` and `Heap.Allocator` call `Standard.Os`;
    - an unsupported OS reaches a `@compile_error` in its arm.
26. **Run tests mount `Standard` by default** (the harness adds
    `--lib=Standard=lib/Standard`). Tests that call `@os_*` switch to
    `Standard.Io`. A minimal runtime library can be added later if wanted.

### G. Atomics

27. **Family names, with the ordering in the name**, parsed like `@i_<op>_W`:
    - `@atomic_<op>_W` is sequentially consistent;
    - `@atomic_<op>_<order>_W` takes `relaxed`, `acquire`, `release` or
      `acq_rel`.

    Invalid combinations make `lookup` return `None` (an unknown intrinsic).
    `W` is 8/16/32/64/native.

    | Intrinsic | Signature |
    | --- | --- |
    | `@atomic_load_W` | `(ref @bitW): @bitW` |
    | `@atomic_store_W` | `(ref @bitW, @bitW)` |
    | `@atomic_swap_W` | `(ref @bitW, @bitW): @bitW` (old) |
    | `@atomic_compare_swap_W` | `(ref @bitW, expected, desired): @bitW` (old; succeeded iff `old == expected`) |
    | `@atomic_add_W` / `sub` / `and` / `or` / `xor` / `max` / `min` / `max_u` / `min_u` | `(ref @bitW, @bitW): @bitW` (old) |
    | `@atomic_fence` / `@atomic_fence_<order>` | `()` |

    Compare-swap is strong. Its failure ordering is the strongest valid one
    for the success ordering.
28. **New LIR instructions** (`AtomicLoad`, `AtomicStore`, `AtomicRmw`,
    `CmpXchg`, `Fence`, each with a span) so the verifier checks widths and
    orderings. Alignment is the natural width.

### H. Arrays

29. **`@array(T, N)` is a builtin value type:** `N × @size_of(T)` bytes with
    `T`'s alignment.
    - It can be an object field or a local, and is copied by value.
    - `N` is a constant.
    - `T` must not have hooks (no per-element drop/copy glue yet).
    - It exists for C struct layout (`WIN32_FIND_DATAW`'s 260 `u16`s) and
      fixed buffers.
30. **`@array_at(ref a, i): ref T`** is bounds-checked and panics.
    **`@array_len(a)`** is a constant. There is no bracket syntax, so there
    is no ambiguity with generics.

## Tasks

### Part A: `continue`

- [x] Lexer `KwContinue`; add it to the LSP semantic-token keyword set if
      it has one.
- [x] Add `Stmt::Continue { span }`. Parse it beside `KwBreak`
      (`parse/src/lib.rs:1009`), add it to the statement-start set (`:1894`),
      and accept it after `else`.
- [x] Typeck: `E022` when `fcx.loops == 0`; emit `HirStmt::Continue(span)`.
      Handle it wherever `Break` is matched (`hir/src/lib.rs:457`).
- [x] Lower:
  - `Fs::loops` becomes `(head, exit, depth)`;
  - `Continue` drops scopes `depth..` at `*span` and branches to `head`;
  - add a `stmt_span` arm.
- [x] Add `continue` to the keyword regex in
      `editors/vscode/syntaxes/lilac.tmLanguage.json`.
- [x] Tests:
  - a parser test (`else continue`);
  - an `E022` typeck test;
  - `tests/run/loop_continue.lilac` (skip odd numbers, nested loops,
    `loop if … else break`);
  - `tests/run/panic_drop_continue.lilac` (the drop frame is at the
    `continue`).
- [x] Docs:
  - SPEC §4 Control Flow;
  - SPEC §1 ownership ("`return`, `break` and `continue`");
  - `docs/errors/E022.md`;
  - the AGENTS.md drop-span bullet.

### Part B: `@builtin`

- [x] `lilac-target`: add a `page_size()` and a way to name the OS/arch
      variants for generated source.
- [x] Driver: generate the `@builtin` source from `Target`, `Entry` and the
      optimize mode, and add it to the load as namespace `@builtin` with the
      display path `<builtin>`.
- [x] Lexer/parser/resolve: accept `@builtin` as the first segment of a
      path. Reject a user declaration of the namespace.
- [x] Tests:
  - a driver unit test (the generated text per target);
  - a UI test that `match @builtin.Target.os` keeps one arm (`emit-ir`
    shows only that arm's call);
  - a `@compile_error` in the dead OS arm is silent.
- [x] Docs:
  - a SPEC section "The `@builtin` namespace";
  - the constant-conditions table gains `@builtin` constants;
  - an AGENTS.md pipeline note.

Done (2026-10-09). Differences from the tasks above:
- Lilac had no qualified names, so typeck flattens a `@builtin.…` member
  chain into one name and `Resolved::lookup_path` finds it; the parser
  takes dotted type names only after `@builtin`.
- `@builtin` types and constants are checked on first use, so programs
  that never name them lower byte-identically (no new layouts in
  snapshots).
- "Reject a user declaration": the CLI rejects `--lib=@…` names; a
  folder cannot be reached by `using @…`.
- The dead-arm `@compile_error` test is a driver unit test, since UI
  success fixtures must also build and run on the host.

### Part C: externs

- [x] AST/parser:
  - an optional string after `extern`, stored as the link name;
  - a trailing `...` parameter, recorded as `variadic`.
- [x] Carry the link name and `variadic` through HIR and LIR
      (`LirFunc::link_name`, `LirFunc::variadic`).
  - Lowering uses the link name as the extern's symbol.
  - The verifier allows variadic only on externs.
  - The verifier and lowering merge identical declarations; a signature
    mismatch is `E028`.
- [x] `lilac-codegen-llvm`:
  - a variadic `fn_type`;
  - call lowering passes the extra arguments, checked by
    `check_extern_signatures`.
- [x] Typeck: check each extra argument against decision 10's rules (an
      untyped literal is an error; narrow types are `E028`). Taking a
      variadic extern as a function value is `E028`.
- [x] Tests:
  - parser tests, including `...` rejected on a non-extern;
  - two namespaces declaring `write` with the same signature link once;
  - a mismatch is `E028`;
  - an untyped literal and a `@bit8` variadic argument are errors;
  - `emit-llvm` shows `declare i32 @open(ptr, i32, ...)` and a call with the
    extra argument;
  - a run test under `// libc` that calls `snprintf` into a buffer and
    prints the result.
- [x] Docs: SPEC §9 extern; `docs/errors/E028.md`; AGENTS.md "Extern C ABI".

Done (2026-10-09). Differences from the tasks above:
- No `LirFunc::link_name`: an extern's LIR name *is* its link name
  (`lilac_lower::extern_symbol`), so merging is keeping one `LirFunc` per
  name. The mismatch check is in `check_extern_signatures`, before
  lowering, and also covers externs nothing calls.
- The untyped-literal error is `E028` too, so every variadic rule has one
  code.
- `...` lexes as one `Ellipsis` token.
- Tests: the `emit-llvm` shape is a codegen unit test; run tests
  `extern_variadic` (`snprintf` with an `@fbit64`) and `extern_link_merge`
  (two namespaces, one `abs`), plus four `FAILS_*` cases.

### Part D: instruction builtins

- [x] `intrinsics.rs`:
  - add `PtrAdd`, `PtrToAddress`, `AddressToPtr`, `AlignOf`, `Memmove`,
    `Memset`, `Syscall` and `Trap`;
  - `AlignOf` gets the `SizeOf`-style typeck special case.
- [x] Lowering:
  - `@ptr_add` → `Builtin("@ptr_add")`;
  - the address builtins → `Convert` (ptr→int `Bitcast`, as
    `runtime::is_null` does, and int→ptr);
  - `@align_of` comes from the layout code;
  - `@syscall` → `Builtin("@syscall")`, `E035` off Linux x86-64/aarch64;
  - `@trap` → `@lilac_abort`.
- [x] `lilac-ir`: `builtin_sig` rows for `@memmove` and `@memset`; the
      verifier accepts int→ptr `Convert`.
- [x] `lilac-codegen-llvm`: `llvm.memmove`/`llvm.memset`; `inttoptr`.
- [x] Tests:
  - lookup/signature unit tests;
  - an IR snapshot;
  - `tests/run/allocator/`: a free-list allocator over the existing
    `@os_alloc` for now (switched to `Standard.Os` page mapping in Part F),
    with `alloc(size, align)`, `realloc` (in place when the next block is
    free, else moves), `free` and coalescing; it checks contents and
    alignment;
  - `@memmove` with overlapping ranges, and `@memset`;
  - `@trap` exits abnormally.
- [x] Docs: SPEC §3 rows; IR_SPEC.

Done (2026-10-09). Differences from the tasks above:

- `@ptr_add` lowers inline (`bitcast` to `#bit_native`, `i.add`, `bitcast`
  back) rather than calling the `@ptr_add` leaf, whose signature is
  `ref #bit8`; the operand keeps its `ref #unknown` type without extra casts.
- `E035` is checked after DCE in the driver (`check_live_syscalls`, beside
  `E031`) rather than during lowering, which has no diagnostics. So only a
  reachable `@syscall` is an error, and the runtime's own calls are exempt.
- The verifier already accepted any `Convert`, and the backend already
  emitted `inttoptr` for an int→ref `bitcast`, so neither needed changes.
- `@trap` is tested by a new run-test directive, `// expect-exit: trap`
  (a signal on Unix, a negative exception status on Windows). `E035` is
  covered by a driver unit test, since `expect-error` runs `check` without
  a `--target`.

### Part E: eager library loading

- [x] `lilac-module`: walk every folder of every mounted library. `using`
      stays a visibility edge only.
- [x] Resolve/typeck: check every non-generic item in every loaded file.
- [x] Check that all of `lib/Standard` (including `Standard.C`) checks
      cleanly when loaded whole. Fix anything that only worked because it
      was never loaded.
- [x] LSP: compile units still work; a library's diagnostics appear without a
      `using`.
- [x] Tests:
  - a library error in an unused namespace is reported;
  - an unused library adds nothing to `emit-ir`.
- [x] Docs: AGENTS.md "Folder modules and visibility"; SPEC namespaces.

Done (2026-10-09). Differences from the tasks above:

- Typeck already checked every non-generic item it was given; only the
  loader changed. The project's own tree still loads through `using`.
- DCE now also prunes type layouts nothing surviving names. Without that,
  an unused library's types reached `emit-ir`. The `enum_decl`,
  `enum_payload_decl`, `generic_duck_typing_static` and
  `hook_unused_compile_error` snapshots lose their unused layouts.
- Two problems appeared once `Standard.C` was always loaded:
  - a root-namespace function named like an `extern`'s link symbol (the
    allocator test's `free`/`realloc`) collided at the IR level, since root
    symbols had no prefix. Root free functions are now `.root.`-prefixed
    (`.root.free`; `main` stays bare, and `--export=api` maps to
    `.root.api`), so they cannot take a C symbol. Most snapshots changed;
  - `extern_variadic`'s `puts(ref Buffer)` clashed with `Standard.C`'s
    `puts(ref Int8)`. Extern `ref` slots now lower as `ref #unknown`, with
    a cast at each call, so declarations that differ only in pointee merge.
    The `extern_*` `.lir`/`.ll` snapshots changed accordingly.
- A missing library folder is `E025`.

### Part F: platform hooks and the OS layer

- [ ] Hooks:
  - recognize top-level `@entry`, `@exit` and `@compiler_print`, checked
    against the decision 19 signatures (`E010` on a mismatch);
  - `E034` for a duplicate, `E033` for a missing one under `require_entry`;
  - add `docs/errors/E033.md` and `E034.md`.
- [ ] `@entry` lookup: a name that is not found falls back to the root
      namespace inside an `@entry` body. A missing `main` is the reworded
      `E032`.
- [ ] Emit the hooks under fixed symbols (`.lilac.entry`, `.lilac.exit`,
      `.lilac.compiler_print`). When a hook is absent and not required,
      declare the symbol as an `extern`.
- [ ] `lilac_ir::entry_point`: the stub calls `.lilac.entry`, then
      `.lilac.exit(0)`, then traps. Under `Entry::CMain` the C `main` does
      the same.
- [ ] Panic handler: `.lilac.write` → `.lilac.compiler_print`, then
      `.lilac.exit(101)`, then `@lilac_abort`.
- [ ] DCE roots: `.lilac.entry` (and therefore `main`) replaces `main` when
      building an executable. `--export` roots are unchanged. The `E031`
      chain hides `.lilac.*` frames, so it still ends at `main` →
      `@entry`. Check this.
- [ ] Remove:
  - the `@os_*` intrinsics;
  - `System`, `Syscalls` and the OS-layer functions in `lilac-ir::runtime`,
    with their tests;
  - the driver's `System` choice.
- [ ] `lib/Standard`:
  - `Runtime/runtime.lilac`: the three hooks. `@entry` calls `main()`.
  - `Os/`: per-OS bindings. Windows: `kernel32` externs with link names.
    Linux: `@syscall` (`write`, `exit_group`, `mmap`, `munmap`). Libc:
    `write`, `exit`, `calloc`, `free`.
  - `Io`, `Process` and `Heap.Allocator` call `Standard.Os`.
  - The allocator run test moves onto `Standard.Os` page mapping.
- [ ] Toolchain stubs: `kernel32.lib` and `libSystem.tbd` list every symbol
      `Standard.Os` uses.
- [ ] Run-test harness: mount `Standard` by default. Migrate the tests that
      call `@os_*`. Re-bless any LIR snapshots that change.
- [ ] Tests:
  - a missing hook is `E033`, and a duplicate is `E034`;
  - a program without `main` gives the reworded `E032`;
  - a custom runtime library that names its entry `start` builds and runs;
  - panic output and exit code 101 still hold (the existing
    `tests/run/panic_*`);
  - an `@exit` that returns hits the trap.
- [ ] Docs:
  - SPEC: a new "Platform hooks" section, with the §3 `@os_*` rows removed;
  - AGENTS.md: rewrite "Runtime and symbols", the OS-layer bullets of "Build
    and link", and "Panics and stack traces".

### Part G: atomics

- [ ] `intrinsics.rs`: `Atomic(AtomicOp, Ordering, IntWidth)` and
      `Fence(Ordering)`, with the family parser and ordering validity.
- [ ] `lilac-ir`:
  - `AtomicOrdering` and `RmwOp`;
  - the five instructions (printer, `inst_span`, verifier);
  - DCE and trace pass them through.
- [ ] Lowering arms.
- [ ] `lilac-codegen-llvm`:
  - atomic load/store with ordering and alignment;
  - `build_atomicrmw`;
  - `build_cmpxchg`, then `extractvalue 0`;
  - `build_fence`.
- [ ] Tests:
  - lookup tests (valid and rejected orderings);
  - verifier tests;
  - `emit-llvm` checks;
  - `tests/run/atomics.lilac` (single-threaded semantics).
- [ ] Docs: SPEC §3; IR_SPEC.

### Part H: `@array(T, N)`

- [ ] Parser: `@array(Type, constant)` in type position.
- [ ] Types: `TypeRef` gains an array form; layout is `N × size`, aligned to
      `T`; `E010` when `T` has hooks.
- [ ] LIR: `LirTy::Array(elem, n)`; the verifier and LLVM `[N x T]`.
- [ ] Intrinsics:
  - `@array_at` → field address plus a bounds check that panics with
    "index out of bounds";
  - `@array_len` → a constant.
- [ ] Tests:
  - `@size_of`/`@align_of` of an object holding an array matches C;
  - copying an object copies the array;
  - out-of-bounds panics;
  - `T` with a hook is `E010`.
- [ ] Docs: SPEC §1 types and §3; IR_SPEC.

### Order

Do A first. B, C and D come before F, which needs `@builtin` arms, link
names and `@syscall`. E also comes before F, because Standard's hooks must
be present whenever it is mounted. G and H can go any time. Every part ends
with the gates:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Decided (2026-10-09)

1. Atomic ordering goes in the intrinsic name.
2. OS functionality is library code: externs and `@syscall` chosen by
   `@builtin.Target`. The compiler keeps no OS layer.
3. The required hooks are `@entry`, `@exit` and `@compiler_print`.
   - Standard supplies them.
   - A program without Standard must supply its own.
   - `@entry` calls `main` itself.
   - The panic handler and the shadow-stack walk stay in the compiler.
   - Traps back up every place that expects `@exit` not to return.
4. Libraries load eagerly; `using` is visibility only.
5. Extern link names (`extern "symbol"`) with merging of identical
   declarations.
6. Compiler constants live in `@builtin.Target` and `@builtin.Build`.
7. Arrays are a builtin `@array(T, N)` with `@array_at`/`@array_len`; there
   is no bracket syntax.

## Open questions

1. **Threads:** `@builtin`/`extern` cover spawning threads from Standard, and
   atomics come in Part G. Wait/wake for locks (futex, `WaitOnAddress`) is
   also just externs and syscalls, so there may be no compiler work at all.
2. **Process arguments:** the entry stub could pass the raw argument data
   (the initial stack pointer on Linux, `argc`/`argv` under `--libc`;
   Windows needs nothing) to `@entry`. That is left for a later change.
3. **Static library output:** the hook rules here are ready for it (decision
   23); the output mode itself is separate.
