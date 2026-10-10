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
  `@lilac_abort`, `@syscall`: the Linux system-call instruction, and the
  thread leaves below).
- Instruction builtins (`Intrinsic::leaf_builtin`): `@memcpy`/`@memmove`/
  `@memset`/`@syscall` call their leaf, `@trap` calls `@lilac_abort` then
  ends the block `unreachable`, and the pointer builtins (`@ptr_add`,
  `@ptr_to_address`, `@address_to_ptr`) lower inline to `bitcast`s (and an
  `i.add`). `@align_of` is a constant like `@size_of`. A reachable user
  `@syscall` or `@thread_clone` off Linux x86-64/aarch64
  (`Target::has_syscall`) is `E035`, checked after DCE like `E031`
  (`lilac_ir::check_live_syscalls`).
- Threads (`lilac-codegen-llvm/src/threads.rs`): `@thread_entry` is the
  address of `.lilac.thread_entry`, a C-ABI `ptr (ptr start)` that calls
  `start`'s `{function, argument}` with the place ABI; `CreateThread` and
  `pthread_create` start there. `@thread_clone` is Linux's own thread:
  `.lilac.tls_init` lays out the child's TLS block from `PT_TLS` (through
  `__ehdr_start`; variant II on x86-64, I on aarch64), then assembly
  `clone`s. That code is LLVM IR text linked into the module
  (`append_ir`/`append_assembly`: linking appends module assembly, where
  `set_inline_assembly` would replace it). The trace head is
  `thread_local` on Windows, Linux (local-exec) and macOS, so a native
  Linux `.lilac.native_main` first calls `.lilac.main_tls` (builds the main
  thread's block unless a loader set the thread pointer), native Windows
  defines `_tls_used`/`_tls_index`, and macOS links `__tlv_bootstrap`.
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
- Globals: a top-level `name: T = constant` (or `name: T`, zeroed) is a
  `DefKind::Global` (`=>` stays a `Const`), checked into
  `HirProgram::globals` and named by `HirExprKind::Global`, a place (an
  assignment target too: `Checker::item_place`). `lilac-lower/src/globals.rs`
  folds each initializer with the constant evaluator and encodes it to
  bytes in the C layout (a non-constant one is `E038`,
  `check_global_initializers`); a use is `addr.global` into an indirect
  place. `LirModule::globals` are pruned by DCE like data, never dropped.
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
  over `Standard.Os`.
- `Standard.Os` is two levels. The raw namespaces `Os.Windows` (kernel32
  `extern`s), `Os.Linux` (`@syscall` wrappers, the calls x86-64 and Arm
  share) and `Os.MacOs` (libSystem `extern`s) mirror the platform exactly:
  its names, argument orders, constant values and failure convention
  (`GetLastError`, raw `-errno` with `is_error`/`errno_of`, `errno()`).
  `Os` itself is the portable surface (`Handle`, `read`/`write`, `open`,
  `allocate`, `map_pages`, clocks, `fill_random`, `exit`, `Error`, threads:
  `spawn`/`join`/`yield_thread`), each
  function one `match @builtin.Target.os` over them; `--libc` only moves
  the heap and the exit to C. The rest of `Standard` uses only `Os`, and
  `Standard.C` is ISO C only. Every `Os.Windows`/`Os.MacOs` function must
  be in the `toolchain/` stubs (`tests/toolchain.rs` checks).
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
- Projects and mounted libraries load whole: after the `using` fixpoint
  the loader reaches every identifier-named folder under the root and each
  library (`Loader::reach_children`; a missing library folder is `E025`),
  and typeck already checks every non-generic item, so unused code is
  checked and the editor compiles every project file in its project. The
  root skips `build`, a folder with its own `lilac.json` and a library's
  mount name; a root with no `*.lilac` file (a folder of projects) loads
  only what `using`s reach. `using` is visibility only. DCE prunes unused functions, data and type layouts
  (`prune_layouts`), so an unused library emits nothing. Extern `ref` slots
  lower as `ref #unknown` (casts at each call), so declarations of one C
  symbol that differ only in pointee merge. `lib/Standard`
  is only usable mounted (`--lib=Standard=lib/Standard`); the run test
  `tests/run/stdlib` exercises all of it.
- `using A.B as Name` (`ItemKind::Using::alias`, `lilac_module::Using::alias`)
  loads `A.B` but imports none of its names: the resolver binds `Name` in
  the file's `FileScope::aliases` (`Resolved::alias`), and `lookup_head`
  tries it before the visible names, so it shadows ancestor-reached and
  imported names and namespaces. An alias named like an item of the file's
  own namespace, or a second alias of one name, is `E054`
  (`collect_aliases`). Locals shadow aliases; a failed member access on such
  a local says so (`Checker::shadowed_alias`).
- Conditional items (`ItemKind::Conditional`: a top-level `if` chain or
  `match` over items) are decided in the driver before resolution
  (`lilac-driver/src/conditional.rs`), in rounds: resolve with scratch
  diagnostics (the resolver skips conditionals), build each outermost
  one's selector (an `if`/`match` value of the arm index), check just the
  selectors (`lilac_typeck::check_selectors`: hook bodies are parked and
  queued only when a checked body calls them), fold them
  (`lilac_lower::evaluate_selectors`), splice the arm in. A selector that
  fails to check waits while others progress, then reports its in-condition
  errors (or `E039`); one that checks but is not constant is `E039`. The
  loader loads every arm's `using`s; `lilac_module::usings_of` resets a
  file's `using`s to its live items.
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
  (`kernel32.lib` and `bcryptprimitives.lib`, a hand-written
  `libSystem.tbd`, listing what `Standard.Os.Windows`/`Os.MacOs` declare),
  no C library: the sysroot is `LILAC_SYSROOT`, else
  `<exe dir>/../lib/lilac` (`scripts/package.sh` builds that layout). The
  program is entered at `.lilac.native_entry` (`LinkInfo::entry_symbol`),
  the backend's wrapper of the stub `.lilac.start(count, arguments,
  environment)` (`lilac_ir::entry_point`, added and rooted only under
  `require_entry`; `Lower::emit_entry_wrapper`): the C `main` shape on
  macOS (`LC_MAIN`), no arguments on Windows (zero and nulls), and on
  Linux an assembly entry reading `argc`/`argv`/`envp` off the stack that
  calls the C-ABI `.lilac.native_main`. Standard's `@entry` stores them in
  `Standard.Os` (`argument_count`, `argument_vector`,
  `environment_vector`); `main` takes nothing. The backend also defines
  `memcpy`/`memmove`/`memset` (marked `no-builtins`), plus `_fltused` and
  `__chkstk` on Windows (`lilac-codegen-llvm`'s `native.rs`). Linux links `-static`, or with `-dynamic-linker` (from `LinkInfo`)
  when a `--link-arg` names a shared library (`.so`, `-l`). macOS links
  `libSystem` (Apple's only stable system interface; lld ad-hoc signs arm64
  executables) and takes `memcpy`/`memset`/`bzero` from it, so the backend
  defines none of the C-runtime replacements there; a `libSystem` function
  the stub does not list needs adding to `toolchain/*-apple-darwin/`.
- `--libc` links through a C compiler driver (`--linker`, `LILAC_LINKER`,
  else `cc`/`clang` on `PATH`) with a C `main(argc, argv, envp)`
  (`Entry::CMain`) that calls `.lilac.start`; `@builtin.Target.libc` is then set, so `Standard.Os`
  takes its heap and exit from the C library. It is the
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
  (`retain_partial_analysis`); the template HIR of uninstantiated generics
  (`Checked::templates`) feeds highlighting and hovers. URIs are compared via
  `roots::canonical_file_uri`.
- A diagnostic with a trace is also published at each call site in it, in
  that site's file (`convert::trace_diagnostics`), linking back to the
  error, and the error lists the sites as related information. So a
  `@compile_error` in a library marks the user's call too.
- Diagnostics: a compile runs once no change has arrived for 200 ms (a
  deadline, so requests never postpone it), on the worker thread
  (`compile.rs`: a `Job` snapshots the documents, root and libraries), so
  requests are answered from the last compile meanwhile. One compile runs
  at a time; its analysis is installed when it finishes, but its
  diagnostics are published only if no edit arrived since (`generation`).
  Each publish carries the version of the document it compiled. The
  retained analysis must stay `Send + Sync` (no `Rc` in `Resolved`, and
  `lilac_driver::Progress` is `Send + Sync`). A diagnostic without a span goes to
  `lilac.json`, else the unit's `main.lilac`, else its first open file by
  URI (`Server::project_anchor`). The server registers watchers for
  `**/*.lilac` and `**/lilac.json` (recompile; a `lilac.json` change
  re-reads the root and libraries) and takes `lilac.libraries` from
  `workspace/didChangeConfiguration` without a restart.
- Navigation (`navigate.rs`): a name is an item (`DefId`: the index's
  `defs` and `uses`), a local, keyed by its declaration's name span
  (`HirProgram::locals`, a `LocalSite` per declaration and use, recorded by
  typeck's `record_local`/`record_local_use`), or a `using … as` alias,
  keyed by its name's span in the `using` (`HirProgram::alias_uses`, an
  `AliasSite` per path head, recorded in `lookup_item`; the index adds
  each declaration as a site). References, highlights,
  definition, type definition, rename (`prepareRename` refuses hooks and any
  edit in the builtins or in a library other than the requesting file's),
  document and workspace symbols are all built from those. A `.Variant` tag
  is a use of its enum and of the variant; the variant wins (its name is
  the span's text). Hovers show a function's signature, a field, variant or
  constant's declaration, and an intrinsic's signature.
- Editing (`complete.rs`, `actions.rs`): completion and signature help
  run before the edit is compiled, so the context is read from the
  document's current text and names come from the last compile, the cursor
  mapped into its text (`complete::map_offset`). Locals in scope come from
  `LocalSite::scope` and `ty` (typeck's `FnCx::open`/`close` track block
  spans); `.` resolves the receiver path (local, field, type, namespace)
  to its members; `@` lists `Intrinsic::names()` and the builtin types.
  A diagnostic's fixes ride in its published `data` (`convert::fixes`) and
  `textDocument/codeAction` turns them into quick fixes without a compile.
- Semantic tokens (`handlers::semantic_tokens`) merge item declarations
  and uses with `Analysis::semantics`: typeck's `SemanticSite`s, the
  driver's top-level-condition checks (`conditional::select` returns
  them) and the resolver's `Resolved::namespace_sites` (each `using`
  segment naming a loaded namespace, and each alias name, from the
  parser's `ItemKind::Using::segments`). A path segment that walks a
  namespace (`Checker::lookup_item`) is a `Namespace` site, placed by
  `paths::segment_spans` only when the path's span is exactly its text
  (no space or comment between segments); otherwise it is left uncolored.
  VS Code maps `namespace` under `entity.name.type`, so it colors like a
  type.
- Faded hints (`hints.rs`) are editor-only: computed from the retained
  analysis and published with a file's diagnostics as `Hint` +
  `Unnecessary`; the compiler reports none, and the CLI prints nothing.
  They cover unused locals and parameters (a `LocalSite` declaration with
  no other site; `LocalSite::kind` leaves out `LocalKind::Signature` names,
  `self` and `_`-prefixed names), unused `using … as` aliases, unused
  top-level `private` items (no `NameSite`), and unreachable statements
  (`HirProgram::unreachable`: the rest of a block after `return`, `break`,
  `continue`, `@panic` or `@trap`, recorded by typeck's `check_stmts`),
  and statements whose value has no effect (`HirProgram::no_effect`, from
  `HirExpr::has_no_effect`: nothing in them is a call, so `5 + 5` but not
  `a + b`, which calls an operator hook). A scope with an error gets none.
  Discarded call results are never flagged, and nothing here is an error.
- Resilience: a compile that panics keeps the previous analysis and shows
  one `window/showMessage` error until a compile succeeds
  (`Server::reported_panic`); a request handler that panics is answered
  with `InternalError` (`compile::guarded`), and the server keeps running.
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

Every pass recurses over the tree, so depth is bounded once, in the
parser: past `lilac_parse::MAX_NESTING` (256) levels is `E001`, reported
once per item, and the construct is skipped (`Parser::nested`). Each link of
a chain built left to right (an operator of `a + b + c`, an `as`, a `.b` or
call of `a.b().c`, a prefix `-`/`ref`) is a level too (`Parser::link`). Every
compile runs on a thread with `lilac_driver::COMPILE_STACK_BYTES` (the CLI's
`main`, the LSP server and its worker), which covers that depth in a debug
build (`deepest_accepted_nesting_checks`). A new recursive syntax form must
count its levels the same way.

Where things live:

- `lilac-parse`: `lib.rs` holds the `Parser` and its recovery; `items.rs`,
  `types.rs`, `stmt.rs` and `expr.rs` parse each kind of syntax.
- `lilac-lower`: `lib.rs` drives lowering and holds `FnState` (one
  function's emission state); `symbols.rs` (names, mangling, externs),
  `ownership.rs` (copies, moves, drops, glue), `control.rs` (`if`,
  `match`, loops, `and`/`or`), `expr.rs` (values by expression kind),
  `calls.rs` and `intrinsics.rs`.
- `lilac-ir`: `lib.rs` holds the LIR types, `print.rs` the printer,
  `verify.rs` the verifier and the post-DCE `check_live_*` checks.
- LIR names are typed (`names.rs`): `Reg` (`&`), `PlaceName` (`%`),
  `Label` (`$`) and `Symbol` (`@`, data `!`); each displays with its sigil.
  A `const` holds a `LirConst` (`Int(u64)`, `Float(f64)`, `Data(Symbol)`)
  built once in lowering (`number_const`); the verifier checks it fits its
  type (`check_const`) and the backend never parses text.
- AST type syntax is a `lilac_ast::TypeShape` (`Named`, `Function`,
  `Array`, `SelfType`, `Error`): a type the parser could not read is
  `Error`, never a name. Builtin types are `lilac_ty::Builtin`, whose
  `id()` is its fixed `TypeId` in every table; look one up by spelling
  (`Types::builtin`) only for source names. Intrinsic slots carry a
  `Builtin`.

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
  the `reached from:` caller tree (levels past five elided unless
  `--verbose`); the JSON emitter nests it as `trace`. The checks run right
  after DCE, before `trace::instrument`.
- `E037` (a constant `@array_at` index out of bounds) is found while
  lowering (`Lowered::bad_indices`) and reported by
  `lilac_lower::check_constant_indices` before DCE, traced through the full
  call graph, so it fires in functions nothing calls. Arms a constant
  condition rules out are never lowered, so they are not checked.
- Typeck emits through `Checker::diags`, a `Sink` (`sink.rs`): while an
  instance body is checked it holds each diagnostic as a `Pending` (with
  its `Requirement` and whether it is `dependent` on a parameter), and
  `check_instance` hands them to `settle` when the body is done, which
  reports them, folds a failed generic instance's requirements into `E042`
  (`instance_failure`), or filters a template's (`template_failure`). A
  body failed when what it held has an error. `Diags` keeps every
  diagnostic; only the terminal shows the first `SHOWN_ERRORS` (10), and
  the summary counts the rest (`Session::hidden_errors`).
- One error must not hide another. An instance whose body failed is
  `State::BodyFailed`, and its callers are still checked against its
  declared signature; only `State::Unusable` (a signature type that failed,
  the `E051` depth cap, a dropped `@entry`) makes `request` fail. When a
  callee cannot be found or used, `Checker::callee_failed` (or `callee`,
  for a `Result`) still checks every argument against the error type, so
  errors inside them are reported. A silent `Err(ErrorGuaranteed::new())`
  carries a comment naming the diagnostic that covers it.
- `E042` traces every site that requested the failing instance
  (`Checker::origins`, recorded on every `request`, cached or not), through
  generic requesters down to plain functions (`Checker::instance_trace`).
  `E042`s are held in `Checker::failures` until the worklist drains
  (`drain_queue`), so origins requested later are included. `E051` keeps its
  first-origin `instantiation chain:` note.
- Every compile checks each uninstantiated generic as a template
  (`Checker::check_templates`: its own parameters as abstract `T`s, never
  lowered). Diagnostics emitted through `emit_requirement`/`emit_involving`
  with a type that `contains_param` are held as `dependent`;
  `template_failure` drops those and reports the rest plainly (never
  `E042`), but only for the template instance of a generic with no concrete
  instance (`reports_template`), so nothing is reported twice. A new error
  site whose cause can be `T` must go through `emit_involving`, or unused
  generics in `lib/Standard` start failing. Templates also get `E037` from
  typeck (`indices.rs`): a literal or `const` index, skipping arms whose
  condition reads no local.
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

## Diagnostics

- Codes live in one registry, `lilac_error::codes`: each `Code` is listed
  once with its page `docs/errors/<code>.md` (`include_str!`), and
  `Diagnostic::error` takes a `Code`, so a new code needs its page to
  compile (a unit test checks each page starts with `# <code>: `).
- `ErrorGuaranteed` is earned: only `Diags::emit` makes one, or
  `Diags::already_reported` (debug-asserts an error was recorded) for
  poison passed along. Typeck uses `Sink::already_reported`, which also
  accepts an error it holds (`ErrorGuaranteed::held`). The one stop with
  no error is named: `ErrorGuaranteed::unreported(Unreported::DroppedEntry)`.
- A did-you-mean is a fix, not prose: `Diagnostic::suggest(span, name)`
  adds the `help: did you mean ...?` line and the `Suggestion` the editor
  applies (and the JSON emitter's `suggestions`). `span` must cover just
  the misspelled name. Typeck's `suggest.rs` has the candidate lists
  (`value_names`, `type_names`, `member_names`); the matcher is
  `lilac_error::closest`, which the CLI also uses for misspelled commands,
  flags and `lilac explain` codes. A dotted
  path is walked to its first segment that names nothing
  (`suggest_path`), and that segment alone is matched against what the
  previous namespace holds, or at the head against the file's aliases,
  the namespaces within reach and its visible types (`head_names`); so a
  path's span must start at its head.
- Parse errors say what they found: `Parser::expected` writes "expected
  {what}, found {token}" (`Token::describe`: "`function`", "a string",
  "end of file"). A delimiter the file ends inside is `Parser::unclosed`:
  the error is at the opening `{`/`(`, with the end of file as a
  secondary label.
- The CLI never dumps the usage on a usage error: it prints the problem,
  a did-you-mean when one is close, and "run `lilac help` for all
  options". `lilac explain <code>` prints the code's page
  (`codes::lookup`). `--timings` and `-q` are common flags
  (`Common::progress`), so `check` and `emit-*` take them too.

## Gates

Run before committing:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
