# AGENTS.md

## LLVM toolchain (M5b)

- **LLVM 18.1.8 dev tree is required** at `C:\Users\joshu\llvm-18.1.8`
  (`bin\llvm-config.exe`, `include\llvm`, component `.lib`s, `clang.exe`).
- `LLVM_SYS_181_PREFIX` is set to that path in `.cargo/config.toml` (which is
  gitignored) so cargo finds LLVM from any shell. Do not duplicate it in
  per-shell environment scripts.
- The official release omits `libxml2s.lib`. A link-time stub was placed at
  `C:\Users\joshu\llvm-18.1.8\lib\libxml2s.lib`. **Do not remove it** or
  `llvm-sys`/`inkwell` linking breaks.
- **`inkwell` is confined to `lilac-codegen-llvm`.** No other crate may depend
  on `inkwell`, `llvm-sys`, or link LLVM. Only `lilac-codegen-llvm` needs
  `unsafe` internally via those crates; the workspace `unsafe_code = "forbid"`
  lint stays in force for our own code.

## Runtime and symbols

- The builtin runtime (byte buffers, `@load_u8`/`@store_u8`, format-args
  tables, the entry stub `.lilac.start` and the panic handler) is written
  once as ordinary LIR in `lilac-ir::runtime` and shared by all backends. It
  makes no OS call: the compiler has no OS layer. Backends hand-write only
  the leaf intrinsics (`@ptr_add`, `@memcpy`, `@memmove`, `@memset`,
  `@lilac_abort`, and `@syscall`: the Linux system-call instruction).
- Instruction builtins (`Intrinsic::leaf_builtin`): `@memcpy`/`@memmove`/
  `@memset`/`@syscall` call their leaf, `@trap` calls `@lilac_abort` then
  ends the block `unreachable`, and the pointer builtins (`@ptr_add`,
  `@ptr_to_address`, `@address_to_ptr`) lower inline to `bitcast`s (and an
  `i.add`). `@align_of` is a constant like `@size_of`. A reachable user
  `@syscall` off Linux x86-64/aarch64 (`Target::has_syscall`) is `E035`,
  checked after DCE like `E031` (`lilac_ir::check_live_syscalls`).
- Atomics (`Intrinsic::Atomic(AtomicOp, Ordering, IntWidth)`,
  `Intrinsic::Fence`): the ordering is in the name and an invalid one is no
  intrinsic. They lower to the LIR instructions `AtomicLoad`/`AtomicStore`/
  `AtomicRmw`/`CmpXchg`/`Fence` (the verifier checks types and orderings)
  and to LLVM atomics aligned to their width.
- Arrays (`TypeKind::Array`, `LirTy::Array`, spelled `[N x #t]`): the
  element is by value and hook-free (`E010`, checked after hooks bind,
  `Checker::check_array_hooks`), so arrays copy and drop as bytes and need
  no glue. `@array(T, N) {}` is a zeroed `Struct` with no fields.
  `@array_at` lowers to an unsigned bounds check that panics, then the
  `elem.addr` instruction (LLVM: `ptrtoint`/`inttoptr` arithmetic, since a
  dynamic GEP is `unsafe` in inkwell); `@array_len` folds in typeck.
- **Platform hooks** (`lilac_hir::PLATFORM_HOOKS`): the top-level
  `@entry`, `@exit` and `@compiler_print`, emitted as `.lilac.entry`,
  `.lilac.exit` and `.lilac.compiler_print`. Typeck checks their shapes
  (`E022`, `sigs::platform_hook_shape_ok`) and duplicates (`E034`); an
  `@entry` body falls back to the root namespace for names
  (`Checker::in_entry`, `Resolved::root_item`), so `Standard`'s `@entry`
  calls `main`, and a miss is `E032` under `Options::require_entry` or drops
  the hook otherwise. The driver reports a missing hook under
  `require_entry` as `E033` (`check_platform_hooks`). The stub calls
  `.lilac.entry`, `.lilac.exit(0)`, then traps; the panic handler prints
  through `.lilac.compiler_print`, calls `.lilac.exit(101)`, then traps. A
  missing `@exit`/`@compiler_print` outside an executable is declared by
  `lilac_ir::hook_externs`: a `.lilac.`-named `extern`, which keeps the
  place ABI (`LirFunc::uses_c_abi`). `lib/Standard/Runtime` defines the hooks
  over `Standard.Os` (per-OS `extern`s and `@syscall`s picked by
  `@builtin.Target`).
- Compiler-generated symbols use the reserved `.lilac.` namespace
  (`.lilac.byte_buffer_ptr`, `.lilac.main`, `.lilac.ptr_add`). User
  identifiers cannot contain `.`, so the namespace is collision-proof; the IR
  verifier also rejects duplicate function names.
- `inkwell` is still confined to `lilac-codegen-llvm` (see above); the runtime
  move does not add LLVM dependencies elsewhere.
- The single pointer-width rule lives in `lilac-target` and funnels through
  `Arch::is_64_bit`. The driver reads `Target::bit_native_width()` (which calls
  `Arch::is_64_bit`) to bake the `format_args` piece stride into the injected
  runtime; `lilac-codegen-llvm` calls `is_64_bit_triple` (which also delegates
  to `Arch::is_64_bit`) to pick `@bit_native`'s LLVM width. Because both funnel
  through `Arch::is_64_bit`, their widths cannot drift.

## Folder modules and visibility (M6a)

- `lilac-module` is the folder loader: it discovers the root folder, reads every
  `*.lilac` file directly in it, and runs a `using` fixpoint over an injected
  `Fs` trait (the driver stays I/O-free; `lilac-cli` supplies `StdFs`). A folder
  is one namespace and its files merge. `E025` is the module-load error: a
  missing/not-a-directory `using` target, an unreadable root folder, or an
  unreadable file.
- `lilac-resolve::resolve` builds one definition table (`Resolved`): every
  item, member, field and variant is a `Def` addressed by `DefId`, namespaces
  are `NsId`s, and each file has a `FileScope` (its visible-name map, already
  filtered by visibility and reach). Typeck and lowering look names up through
  `Resolved::lookup` with the file's id (`E029` on an ambiguous use).
- Libraries: `--lib=Name=dir` (CLI) / `lilac.json` `"libraries"` and the
  `lilac.libraries` setting (LSP) mount `dir` as the top-level namespace `Name`;
  `Session.libraries` carries them into `lilac_module::load_with_status`.
  `using *.A` is relative to the importing file's namespace.
- Mounted libraries load whole: after the `using` fixpoint the loader
  reaches every identifier-named folder under each library
  (`Loader::reach_tree`; a missing library folder is `E025`), and typeck
  already checks every non-generic item, so unused library code is checked.
  `using` is visibility only (in the project's own tree it still decides
  what loads). DCE prunes unused functions, data and type layouts
  (`prune_layouts`), so an unused library emits nothing. Extern `ref` slots
  lower as `ref #unknown` (casts at each call), so declarations of one C
  symbol that differ only in pointee merge. `lib/Standard`
  is only usable mounted (`--lib=Standard=lib/Standard`); the run test
  `tests/run/stdlib` exercises all of it.
- Visibility reach is ancestor-public + descendant-private (Rust's rule). A
  top-level `private { … }` excludes an item from `using` (importers see it as
  unresolved, not `E026`); an object-member `private { … }` gates external access
  with `E026`. Top-level hooks are unit-global and carry no namespace prefix.
- Paths (`lilac-typeck/src/paths.rs`): a type name is a dotted path
  (`A.B.Type`), and an expression member chain is flattened into one
  (`Checker::qualified_name`) while it walks namespaces;
  `Checker::lookup_item` resolves every name and path. The head is the
  file's name, else a namespace by reach (`Resolved::namespace_named`), and
  each step is `Resolved::ns_member` (an item, a `private` one only within
  reach, else a child namespace); an item ends the path. `@builtin` is an
  ordinary top-level namespace in this.
- Types are top-level only: the resolver rejects an `object`/`enum` in a
  type's body as `E036` (`Collector::nested_type`) and defines nothing for
  it. The parser still accepts the syntax (also in enum bodies) so the
  error is that one, not a parse error.
- Namespace qualification: every non-root symbol/type is prefixed by its dotted
  namespace path (`A.B.foo`, `A.Object.Method`). A **root free function is
  `.root.`-prefixed** (`.root.foo`, `lilac_lower::namespace_prefix`) so it
  can never take a C symbol; root `main` stays `main`, and root types and
  their methods stay unprefixed (`Object.Method` already has a `.`).
  `--export=<name>` takes the source name (`lilac_lower::export_symbol`).
  `extern` symbols stay unmangled, and a file argument on the CLI
  is single-file mode (its parent is not a project root).

## Extern C ABI (M6b)

- `extern` declarations and calls use the platform C ABI on the LLVM backend:
  bit integers by value, `ref T` → pointer, 0 outs = `void`, 1 out = the C
  return value, and an unmangled symbol: the `extern "name"` link name, else
  the declared name (`lilac_lower::extern_symbol`). `>1` out or an
  unsupported param/return is `E028`.
- Externs sharing a link name lower to one `LirFunc` (named by the link
  name); `check_extern_signatures` makes a signature mismatch `E028`.
- Variadic externs (`...`, `FuncDecl::variadic` → `LirFunc::variadic`,
  printed `in ...`): typeck checks the extra arguments
  (`check_variadic_arg`), `lilac_ir::is_variadic_arg_type` is the accept-set
  the verifier shares, and LLVM gets `fn_type(…, true)` with the extras
  passed by value.
- `E028` is validated pre-codegen in the driver via
  `lilac_lower::check_extern_signatures`, so `check`/`emit-llvm` report it as an
  ordinary diagnostic rather than a backend failure. `lilac_ir::is_c_abi_type` is the single accept-set shared by that
  frontend check and `lilac-codegen-llvm`'s signature lowering, so they cannot
  drift.

## Build and link

- `lilac build [<file-or-dir>]` compiles, writes an object file
  (`Backend::emit_object`, with an `Entry` and a `CodegenStep` callback) and
  links an executable. The CLI takes all build configuration as flags;
  `lilac.json` is for the language server only.
- The build folder is `<project>/build` (folder project) or `./build`
  (single file), with one folder per mode (`debug`, `release`,
  `release-small`) and a `<triple>/` level for a cross target. The mode folder
  holds the executable and `obj/` with the kept object file. `--build-dir`
  and `-o` override it.
- `Target::link_info` in `lilac-target` is the single table of per-OS link
  facts (`LinkInfo`: lld flavor, suffixes, entry symbol, system stubs).
- **Native mode is the default** (`Entry::Native`, on Windows, Linux and
  macOS). It links with the sysroot's `lld` and `toolchain/` stubs
  (`kernel32.lib`, a hand-written `libSystem.tbd`, listing what
  `Standard.Os` calls), no C library: the sysroot is `LILAC_SYSROOT`, else
  `<exe dir>/../lib/lilac` (`scripts/package.sh` builds that layout). The
  program is entered at `.lilac.start` (`lilac_ir::entry_point`, added and
  rooted only under `require_entry`), and
  the backend defines `memcpy`/`memmove`/`memset` (marked `no-builtins`),
  plus `_fltused` and `__chkstk` on Windows (`lilac-codegen-llvm`'s
  `native.rs`; on x86-64 Linux it also gives `.lilac.start` the
  `stackrealign` attribute, since the kernel enters it unaligned for a
  call). Linux links `-static`, or with `-dynamic-linker` (from `LinkInfo`)
  when a `--link-arg` names a shared library (`.so`, `-l`). macOS links
  `libSystem` (Apple's only stable system interface; lld ad-hoc signs arm64
  executables) and takes `memcpy`/`memset`/`bzero` from it, so the backend
  defines none of the C-runtime replacements there; a `libSystem` function
  the stub does not list needs adding to `toolchain/*-apple-darwin/`.
- `--libc` links through a C compiler driver (`--linker`, `LILAC_LINKER`,
  else `cc`/`clang` on `PATH`) with a C `main` (`Entry::CMain`) that calls
  `.lilac.start`; `@builtin.Target.libc` is then set, so `Standard.Os`
  uses the C library (and Windows exits through C `exit`). It is the
  permanent opt-in for static C libraries, and how code using `Standard.C`
  builds while that is phased out. Shared C libraries work in native mode
  (an `extern` plus `--link-arg=`); free their memory with their own free
  function, and note a native exit skips C `atexit`/`stdio` flushing.
- `build` sets `Session.require_entry` (all three hooks required, see
  "Runtime and symbols") and
  `Session.progress`, a `Stage`/`StageEvent` callback. The driver only calls
  it; timing and printing stay in `lilac-cli` (`progress.rs`), so the driver
  stays I/O-free.
- A file argument is compiled with `Session::compile_file_with_stages`
  (`lilac_module::load_file`): the file is the root namespace's only file,
  and its `using`s load mounted libraries (and folders next to it).
- Run tests build natively on Windows, Linux and macOS, with a development
  sysroot assembled under cargo's test scratch folder from `toolchain/` and
  `$LLVM_SYS_181_PREFIX/bin/lld` (`tests/support`); `// libc` programs and
  other hosts use `--libc` with `LILAC_LINKER` set to the toolchain `clang`.
  The harness mounts `Standard` for every program (`// no-standard` opts
  out; `tests/libs/TinyRuntime` is a minimal runtime library). Tests that
  link and run a program use `lilac build` (`support::build_and_run`):
  `emit-llvm` output has no entry stub.

## Language server (lilac-lsp)

- `lilac-lsp` is the editor language server: a stdio binary that speaks the
  Language Server Protocol and publishes compiler diagnostics for `.lilac`
  projects. It is editor tooling, not part of the compiler pipeline.
- It is the **only** crate permitted to depend on `lsp-server`, `lsp-types`,
  and `serde_json` (and thus serde). The compiler crates stay std-only.
- The VSCode extension under `editors/vscode/` launches it via the
  `lilac.serverPath` setting (default `lilac-lsp` on PATH).
- Compile units: the project (`lilac.json` `root`, else the workspace
  folder); an open file it does not reach is compiled as its library
  namespace (`Session::compile_namespace_with_stages`), else its folder, else
  alone (`server::unit_of`). Sessions run tolerant
  (`retain_partial_analysis`), which also checks uninstantiated generic
  bodies for highlighting (`lilac_typeck::check_for_editor`; diagnostics
  discarded, never lowered). URIs are compared via
  `roots::canonical_file_uri`.
- A diagnostic with a trace is also published at each call site in it, in
  that site's file (`convert::trace_diagnostics`), linking back to the
  error, and the error lists the sites as related information. So a
  `@compile_error` in a library marks the user's call too.
- The driver's `Session::project_files()` (fed by
  `lilac_module::load_with_status`) is the `FileId -> PathBuf` map the server
  uses to attach diagnostics to document URIs.
- The `lilac/lir` request and `lilac/lirInvalidated` notification are the
  IR-view protocol: the request serves a captured stage's rendered LIR text, and
  the notification tells the client the view is stale. `lilac-ir` is now also a
  `lilac-lsp` dependency (alongside the compiler crates).

## Pipeline

lex → parse → `lilac-module` load (plus the generated `@builtin`
namespaces) → `lilac-resolve` → `lilac-typeck` (a
worklist of `(def, type args)` instances emitting HIR directly) →
`lilac-lower` (HIR → LIR, including the ownership model: copies, moves,
drops, drop/copy glue, and constant conditions: only the live arm of an
`if`/`and`/`or`/`match` on a constant expression is lowered) → runtime
injection → DCE →
live `@compile_error` check → stack-trace instrumentation (when on) →
verify → backend. `Session::run` in `lilac-driver` is the only
orchestration.

`@builtin` (`lilac-driver/src/builtin.rs`) is Lilac source the driver
generates from the session (`Target`, `Entry`, `OptimizeMode`) and appends to
every project as the namespaces `@builtin`, `@builtin.Target` and
`@builtin.Build`, file name `<builtin>`. Code reaches them only qualified:
the parser takes dotted type names after `@builtin`, typeck flattens a
`@builtin.…` member chain into a name, and `Resolved::lookup_path` finds it.
Its types and constants are checked on first use (not eagerly like user
items), so a program that never names `@builtin` lowers byte-identically.
`lilac-target` owns the variant names (`Arch::builtin_name`,
`Os::builtin_name`) and `Target::page_size`.

Run tests live in `tests/run/` (a file, or a folder with `main.lilac`); see
the header of `crates/lilac-cli/tests/run.rs` for the `// expect-*`
(including `expect-stderr:`), `// build-arg:`, `// pending:` and `// lib:`
directives. The harness runs `lilac` in `tests/run` with the program's bare
name, so source locations are machine-independent. Add a run test for every
bug fix that changes generated code.

## Panics and stack traces

- `@panic(msg)` lowers to a call to `.lilac.panic(msg, site)` followed by
  `unreachable`; `site` is the panic's own `"  at <fn> (<file:line:col>)\n"`
  line as a data constant. The handler is LIR in
  `lilac_ir::panic_runtime` (injected beside `build_runtime`, pruned when
  unused). It writes `panic: <msg>` and the trace through `@compiler_print`
  (stderr in `Standard`), then calls `@exit(101)` and traps. A panic is an
  abort: no drops run.
- Stack traces are a shadow stack, not native unwinding:
  `lilac_ir::trace::instrument` runs after DCE when `Session.stack_traces` is
  set and links a `#.lilac.frame` record per user function call (the head
  slot comes from `@trace_head`, the only backend leaf it adds; the frame is
  a builtin layout). `lilac build` turns it on in debug and
  `--stack-traces=on|off` overrides; `check`/`emit-*` leave it off unless
  asked, so IR dumps stay readable.
- `E031` and `E035` carry a compile-time trace: `Diagnostic::trace`, a
  `TraceFrame` tree of every call site that reaches the error, built by
  `lilac_ir::CallGraph::trace` from the module after DCE (so only live
  callers), with `trace::is_hidden` (`.lilac.*`) glue spliced out. Each
  function's callers are listed once; a later frame of it is a `repeat`
  (`[see above]`), which also ends recursion. `lilac_error::trace_text` is
  the `reached from:` caller tree; the JSON emitter nests it as `trace`.
  The checks run right after DCE, before `trace::instrument`.
- Drops carry real spans so both traces can place them: a scope-end drop is
  at the block's closing `}` (`end_of`), an early exit's drops at the
  `return`/`break`/`continue`, and statement temporaries at the statement's expression
  (`stmt_span`, in `lilac-lower`). Only compiler-generated glue and runtime
  code use `Span::dummy()`; a `LirBlock`'s span is its terminator's.
- `SourceMap::location` (`file:line:col`) and `trace::site_text` (the `at`
  line) are the only formatters. Source-map file names are namespace display
  paths (`Checks/checks.lilac`, `Standard/Io/io.lilac`); a root-namespace
  file keeps its bare (or given) name. Function names in traces come from
  `lilac_lower::Lowered::names` (`Holder[@bit32].make`, not the mangled
  symbol).

## Gates

Run before committing:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
