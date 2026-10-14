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
  `@memcmp`, `@lilac_abort`, `@linux_syscall`: the Linux system-call
  instruction, `@trace_head` and `@panic_depth`: thread-local slots, and
  the thread leaves below).
- Instruction builtins (`Intrinsic::leaf_builtin`): `@ptr_add`/`@memcpy`/
  `@memmove`/`@memset`/`@linux_syscall` call their leaf (`.lilac.ptr_add`
  is an always-inline wrapping `getelementptr i8`), `@trap` calls
  `@lilac_abort` then ends the block `unreachable`, and `@ptr_to_address`/
  `@address_to_ptr` lower inline to `bitcast`s. `@align_of` is a constant like `@size_of`. A reachable user
  `@linux_syscall` or `@linux_thread_clone` off Linux x86-64/aarch64
  (`Target::has_syscall`) is `E035`, checked after DCE like `E031`
  (`lilac_ir::check_live_syscalls`).
- Threads (`lilac-codegen-llvm/src/threads.rs`): a thread starts in a C
  function, `Standard.Os`'s `thread_start` (an `export`, symbol
  `lilac_thread_start`), which runs the start block's `entry(argument)`;
  `CreateThread`, `pthread_create` and `@linux_thread_clone` get its
  address. `@linux_thread_clone` is Linux's own thread:
  `.lilac.tls_init` lays out the child's TLS block from `PT_TLS` (through
  `__ehdr_start`; variant II on x86-64, I on aarch64), puts the argument
  and entry on the child's stack, then assembly `clone`s and the child
  calls the entry. That code is LLVM IR text linked into the module
  (`append_ir`/`append_assembly`: linking appends module assembly, where
  `set_inline_assembly` would replace it). The trace head is
  `thread_local` on Windows, Linux (local-exec) and macOS, so a native
  Linux `.lilac.native_main` first calls `.lilac.main_tls` (builds the main
  thread's block unless a loader set the thread pointer), native Windows
  defines `_tls_used`/`_tls_index`, and macOS links `__tlv_bootstrap`.
- Aggregates live in memory: a LIR register holds a scalar only
  (`LirTy::is_scalar`; the verifier rejects an aggregate one), so an
  object, array or builtin layout moves place to place. `copy` of one is a
  `memcpy` and `zero` a `memset` of the layout's size at its alignment;
  `copy ref %p -> %dst` (`LirInst::CopyDeref`) copies what a `ref` place
  points to (`FnState::read_place`/`read_into`); aggregate `==` is
  `@memcmp` of the two places against zero (the C `memcmp`; native mode
  defines it). In LLVM an aggregate is `[size x i8]` with the layout's
  alignment, a field is the base plus its offset (`.lilac.offset`, an
  always-inline `getelementptr inbounds i8` linked by `append_ir`), and
  every scalar load and store carries its alignment, so the IR's layout
  (`lilac-layout`) is the only one and LLVM has no struct types. A data
  `const` is a `ref #bit8`; string literals fill a `#byte_buffer` place
  (`lilac_ir::runtime::fill_byte_buffer`).
- F-strings (`lilac-typeck/src/formats.rs`): `@format_args(W)` is
  `TypeKind::FormatArgs { writer }`; `W` comes from the expected type
  (`fstring_writer`: an `@format_args(W)`, else the `W` of its one
  `@conversion` from a format, `E043`/`E073` otherwise), and each hole
  binds the exact `@format(ref T, ref W)` hook (`E071`, `E022` for a bad
  shape). In an `@format_args(W)` slot a string literal or one value is a
  format (`format_slot`). A format lives only in locals and parameters
  (`E072`: `format_place`, `format_store`), and a new local's temporary
  holes are extended into hidden locals (`extend_holes`). Typeck emits
  `HirExprKind::Format`; lowering emits a template (`LirModule::formats`,
  `LirFormat`, printed `!fmt0: format "a" {@f} "b"`) and `format !fmt0
  (&h) -> %d` (`LirInst::Format`), which fills a `#format_args` place
  (template, hole-address array). A template is a constant in
  `format_template_layout` (the hole count, then `#.lilac.format_part`s).
  The consumers `@format_args_holes`/`_piece`/`_hole` call the
  always-inline runtime readers, built per target by
  `build_runtime(&Target)`, after lowering's bounds check
  (`lower_format_consumer`). The verifier checks each formatter is
  place-ABI `ref #T, ref #W` with no outs and each hole register is its
  `ref #T`; DCE keeps a template a live `format` names and treats its
  formatters as taken addresses. `Standard`'s writer is the library
  object `Formatter` (`lib/Standard/format.lilac`; `Format` is its
  alias): `Io.print*` and `Runtime`'s `@compiler_print` take a `Format`,
  so the compiler still names no formatter.
- Atomics (`Intrinsic::Atomic(AtomicOp, Ordering, IntWidth)`,
  `Intrinsic::Fence`): the ordering is in the name and an invalid one is no
  intrinsic. They lower to the LIR instructions `AtomicLoad`/`AtomicStore`/
  `AtomicRmw`/`CmpXchg`/`Fence` (the verifier checks types and orderings)
  and to LLVM atomics aligned to their width.
- Arrays (`TypeKind::Array`, `LirTy::Array`, spelled `[N x #t]`): the
  element is by value and hook-free (`E010`, checked after hooks bind,
  `Checker::check_array_hooks`), so arrays copy and drop as bytes and need
  no glue. `@array(T, N) {}` is a zeroed `Struct` with no fields.
  `@array_at` lowers to an unsigned bounds check that panics, then, in
  the in-bounds block only (the array and index cross the check in
  places, since a register lives in its block), the
  `elem.addr` instruction (LLVM: the index times the element size through
  `.lilac.offset`, since a dynamic GEP is `unsafe` in inkwell);
  `@array_len` folds in typeck.
- Memory builtins (`Intrinsic::is_memory`, checked in
  `lilac-typeck/src/memory.rs`): `@move(place)` becomes
  `HirExprKind::Move`, `@needs_drop(T)` folds to a `@bit8` literal
  (`Checker::needs_drop`), and `@write`/`@read`/`@drop` take a `ref` to a
  known type (`E010` for `ref unknown`). Lowering treats `@write(p, v)` as
  an assignment to `*p` with `HirOld::Empty`, `@read(p)` as a move out of
  `*p`, and `@drop(p)` as the drop routine on `*p`. `@discard(x)` (`E066`:
  an owned object, in a function of its type, `Env::owner`) lowers to
  nothing; the ownership pass gives it its meaning.
- Globals: a top-level `name: T = constant` (or `name: T`, zeroed) is a
  `DefKind::Global` (`=>` stays a `Const`), checked into
  `HirProgram::globals` and named by `HirExprKind::Global`, a place (an
  assignment target too: `Checker::item_place`). `lilac-lower/src/globals.rs`
  folds each initializer with the constant evaluator and encodes it as
  runs (`LirGlobal::init`: `(offset, bytes)` over an otherwise zero object,
  never the whole object; the backend fills the gaps with
  `zeroinitializer`) in the C layout (a non-constant one is `E038`,
  `check_global_initializers`); a use is `addr.global` into an indirect
  place. `LirModule::globals` are pruned by DCE like data, never dropped.
  An initializer may hold addresses (static vtables): the constant
  evaluator's `Value::Func` (`ref f`) and `Value::Global` (`ref G`), passed
  through `ref` casts, never compared or tested (`has_address`), encode as
  `LirGlobal::addresses` (`LirAddress`: an offset and an `AddressTarget`,
  printed `8: addr @f`, `16: addr !g`), slots no run covers. The verifier
  checks each is pointer-aligned, inside, clear and names a module item.
  DCE's `global_funcs` gives an `addr.global` an `EdgeKind::Address` edge
  to every function the global's address closure reaches (so traces pass
  through tables), and a kept global keeps the globals it points at. The
  backend defines globals with null slots, then `fill_global_addresses`
  once every function is declared (`function_address`: an export's is its
  C entry).
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
  `extern`s), `Os.Linux` (`@linux_syscall` wrappers, the calls x86-64 and Arm
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
  `Arch::is_64_bit`. Typeck and lowering read
  `Target::bit_native_width()` (which calls `Arch::is_64_bit`), and
  `lilac-codegen-llvm` calls `is_64_bit_triple` (which also delegates
  to `Arch::is_64_bit`) to pick `@bit_native`'s LLVM width. Because both funnel
  through `Arch::is_64_bit`, their widths cannot drift. Typeck takes the whole
  `Target` (`Options::target`), lowering takes it as a `lower` argument, and
  `LirModule::target` is a `Target` (printed as its triple).
- `Target::to_triple` is the LLVM/Clang triple: `Arch::llvm_name` renders
  `x86` as `i686` (the user-facing and `@builtin` name stays `x86`; `parse`
  reads `i686` back). `Target::parse` refuses a pair no C ABI exists for
  (`ParseError::Unsupported`: WebAssembly with a vendor or an OS), so every
  accepted triple builds an LLVM `TargetMachine` (`target_tests` in
  `lilac-codegen-llvm`). `Target::supported()` lists one target per
  accepted architecture and OS pair.

## Layouts (`lilac-layout`)

- **Layouts are C layouts.** An `object` is the C struct with the same
  fields in the same order on the target (equal `sizeof`, `_Alignof`,
  every `offsetof`); fields are never reordered; an empty object is size 0,
  align 1. `Target::scalar_layout(Scalar)` in `lilac-target` is the one
  table of C ABI scalar sizes and alignments (8-byte scalars align to 4 on
  32-bit x86 off Windows and on 32-bit Arm on macOS; the 128-bit float to
  16 everywhere; otherwise natural); `Target::max_object_size` is the
  target's `isize::MAX`.
- `lilac-layout` (depends on `lilac-target` only) is the one layout
  function: `struct_layout`, `array_layout` and `tagged_layout` compose
  from `SizeAlign`s, `layout` does a whole `Shape` tree, and all
  arithmetic is checked (`TooLarge`, never a wrap).
- **Enums are tagged unions**: the C `struct { uintN_t tag; union { P0
  p0; … } payload; }` (`tagged_layout`), the union one member per payload
  variant, none when no variant has a payload; no niche layouts.
  `lilac_hir::Layout::slots` is the one place that says what an enum holds
  (`tag`, then each payload by variant name); typeck sizes it with
  `tagged_layout` and lowering lays it out as `LirLayoutKind::Tagged`, so
  every payload field has the payload area's offset and `payload_field`
  stays a field index. `==` is a `memcmp` over every byte, so every enum
  write is a whole value: `variant_value` builds in a zeroed temporary,
  copies are whole-object, and globals encode over a zero object; nothing
  writes one payload into a place that held another variant.
- Typeck lays out each concrete object, enum and array type when
  `ensure_type` first builds it (`layout.rs`, `Checker::sizes`); one past
  the limit is `E067` at the field or array length that crossed it, through
  `emit_requirement` (`Requirement::Size`), so a generic instance folds it
  into `E042`. Templates are never laid out.
- Lowering fills `LirModule::layouts` (`LirLayout`: kind, size, align, and
  each field's offset) once, from `lilac-layout`; everything after reads it
  (`lilac_ir::size_align` is a lookup, global initializers use the field
  offsets). The verifier recomputes every layout for the module's target;
  the LLVM backend sees aggregates as `[size x i8]` and computes none.
  `builtin_fields` / `builtin_layout` are the builtin aggregates'.
- Conformance: `crates/lilac-layout/tests/c_conformance.rs` declares a
  corpus in C with `_Static_assert`s and checks it with the toolchain Clang
  for every supported target (skipped without Clang);
  `crates/lilac-lower/tests/enum_c_layout.rs` does the same for the
  layouts lowering gives a corpus of real Lilac enums (the C shape from
  the HIR kind, the numbers from `LirModule::layouts`); `target_tests` in
  `lilac-codegen-llvm` compares the scalar table with LLVM's data layout
  for every target `parse` accepts.

## Folder modules and visibility (M6a)

- `lilac-module` is the folder loader: it discovers the root folder, reads every
  `*.lilac` file directly in it, and runs a `using` fixpoint over an injected
  `Fs` trait (the driver stays I/O-free; `lilac-cli` supplies `StdFs`, the
  language server `OverlayFs`). A folder is one namespace and its files
  merge. `E025` is the module-load error: a missing/not-a-directory `using`
  target, an unreadable root folder, an unreadable file, or a folder
  reached by a second path. `Fs::canonicalize` names a folder with links
  resolved (`OverlayFs` canonicalizes a path only open documents give
  below its nearest folder on disk), and `Loader::reach` keeps each loaded
  namespace's canonical folder (`Loader::folders`): a second path to one,
  such as a link `A/B -> A`, is `E025` "folder `A.B` is `A` again (through
  a link)", once per path, and is neither loaded nor walked ("through a
  link" only when `Loader::through_link` finds one on either path). The
  root folder counts only when the root namespace loads it (not for a
  file argument or a lone library namespace). A library mounted inside
  the tree being walked (a vendored library, `Loader::mounted_at`) loads
  once, under its mount name: the walk skips its folder silently, unless
  it reaches it through a link, and a `using` naming it by its path in
  the project is `E025` "folder `vendor.Util` is mounted as library
  `Util`" with the `using` to write.
- `lilac-resolve::resolve` builds one definition table (`Resolved`): every
  item, member, field and variant is a `Def` addressed by `DefId`, namespaces
  are `NsId`s, and each file has a `FileScope` (its visible-name map, already
  filtered by visibility and reach). Typeck and lowering look names up through
  `Resolved::lookup` with the file's id (`E029` on an ambiguous use).
- Libraries: `--lib=Name=dir` and `lilac.toml`'s `[libraries]` (the
  language server's `lilac.libraries` setting is its `--lib=`) mount `dir`
  as the top-level namespace `Name`;
  `Session.libraries` carries them into `lilac_module::load_with_status`.
  `using *.A` is relative to the importing file's namespace.
- Projects and mounted libraries load whole: after the `using` fixpoint
  the loader reaches every identifier-named folder under the root and each
  library (`Loader::reach_children`; a missing library folder is `E025`),
  and typeck already checks every non-generic item, so unused code is
  checked and the editor compiles every project file in its project. The
  root skips the build folder (`build`, or the manifest's `build.dir`), a
  folder with its own `lilac.toml` and a library's mount name; a root with no `*.lilac` file (a folder of projects) loads
  only what `using`s reach. `using` is visibility only. DCE prunes unused functions, data and type layouts
  (`prune_layouts`), so an unused library emits only its `export`s. Extern `ref` slots
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
  selectors (`lilac_typeck::check_selectors`: every hook body is parked,
  `Checker::park_hooks`, and queued when checked code uses it), fold them
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
  `Checker::lookup_item` resolves every name and path. Locals, generic
  parameters and the own type's names (below) come first. The head is the
  file's name, else a namespace by reach (`Resolved::namespace_named`), and
  each step is `Resolved::ns_member` (an item, a `private` one only within
  reach, else a child namespace); an item ends the path. `@builtin` is an
  ordinary top-level namespace in this.
- Types are top-level only: the resolver rejects an `object`/`enum` in a
  type's body as `E036` (`Collector::nested_type`) and defines nothing for
  it. The parser still accepts the syntax (also in enum bodies) so the
  error is that one, not a parse error. A type alias (`Name: alias Type`,
  `DefKind::Alias`) follows the same rule.
- Type aliases (`lilac-typeck/src/aliases.rs`) are transparent: an alias
  is its type's `TypeId`. Typeck resolves every alias first
  (`Checker::resolve_aliases`, before the hooks, in the alias's own file
  scope, memoized; a cycle is `E074`), so an alias's error is never folded
  into an instance's `E042`; `resolve_named`, `static_receiver` and
  `check_struct_lit` then see through it. `Types::add_alias` records it
  for display only (a type spelled with arguments that one alias names
  prints as the alias); mangling and layouts never see aliases.
- Member constants (`NAME: Type => value` in an object or enum body, a
  `DefKind::Const` whose `parent` is the type): a static access
  `Type.NAME` (`Checker::static_member`, also `Generic.NAME`,
  `Generic[X].NAME` and a parameter's `T.NAME`) tries the type's constant
  (`member_constant`) before a variant, and a miss on a non-enum is
  `E021` through `emit_requirement` (`missing_constant`). A constant is
  checked in `const_env`: the type as `owner` (its private members
  reach), no generic parameters (one value for every instance;
  `owner_param_note` says so). Never reachable through a value; `=` in a
  type's body is a parse error (`E001`).
- Own names (`lilac-typeck/src/own.rs`): in a body whose `Env::owner` is
  a type (methods, hooks, member constants), the type's constants and
  static functions (`Def::is_own_name`) are bare names, after locals and
  generic parameters and before aliases and items: `check_name`
  (`own_value`), `check_named_call` (`own_call`), and `static_receiver`
  and `path_prefix` stop at one (a value, not a path head). A static is
  instantiated at the own type's arguments (`own_type`, as bare `self`
  is typed); a generic type's member constant has none, so naming one
  there is `E021`. A miss naming a field or method gets
  `own_instance_help`; did-you-mean includes `own_names`. The editor
  completes them in the type's span (`Scope::own_type`).
- Namespace qualification: every non-root symbol/type is prefixed by its dotted
  namespace path (`A.B.foo`, `A.Object.Method`). A **root free function is
  `.root.`-prefixed** (`.root.foo`, `lilac_lower::namespace_prefix`) so it
  can never take a C symbol; root `main` stays `main`, and root types and
  their methods stay unprefixed (`Object.Method` already has a `.`).
  `extern` and `export` symbols stay unmangled, and a file argument on the CLI
  is single-file mode (its parent is not a project root).
- Types in symbols (`lilac-ty/src/mangle.rs`, after Rust's v0): the
  readable path stays, and types go in regions, `[…]` for generic
  arguments (`GenObject[m].get[y]`) and `(params E returns)` for a hook's
  signature (`GenBox[m].@on_drop(RI6GenBoxmEE)`), with every repeated type
  or path a backreference, so a symbol is linear in the distinct types it
  names (`IR_SPEC.md`, "Symbol names", has the grammar). `Types::mangle`
  names a type (layouts, glue), `mangle_args`/`mangle_signature` write
  regions, and lowering caches each type's name (`Lower::type_names`).
  `lilac_ty::demangle` (bounded, `None` for a malformed region) and
  `demangle_text` read them back; `lilac demangle` is the CLI filter.
  There is no type length limit: every walk over a type memoizes by
  `TypeId` (`substitute`, `ensure_type`, `has_hooks`, …), and
  `Types::display`/`display_ref` are bounded (`DISPLAY_LIMIT`).

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
- `export` (`FuncDecl::is_export`) is `extern` with a body: C calls it by
  its unmangled symbol. The resolver checks it (`E055`, `check_exports`:
  top-level, not generic, not `main`, no other `export` or `extern` of the
  symbol); `check_extern_signatures` checks its slots (`E028`). It lowers
  as an ordinary place-ABI function with `LirFunc::export` (a
  `LirExport`: the symbol and the C slot types), and the backend emits the
  C entry (`Lower::emit_export`: arguments into stack slots, a place-ABI
  call, the out returned). Direct Lilac calls skip it. The driver roots
  every export in DCE once the module has a root (`main` or the entry
  stub); `--export` is gone.
- Function values (`docs/function-values-plan.md`, phase 1): a function
  value is `ref function (…) …` (`ref export function (…) …` for C), the
  function's address. A function type's outs are `nothing`, one slot or
  a parenthesized list (`Parser::parse_type_outs`, Go's rule), so one
  ends at its out inside a slot list; a declaration may write its outs either way
  (`parse_out_slots` hands a `(` to `parse_type_outs`; no type starts
  with `(`), and `Types::display`, the demangler and the editor's
  `render::type_outs_text` parenthesize more than one out. A type body's
  `name: function` is always a method (`parse_func_rest`). A bare function type is `E077`
  (`Checker::bare_function_type`) except as an alias's target, and it
  recovers as the `ref` type. `ref f`, `ref A.f` and `ref Type.method`
  build `HirExprKind::FuncRef` (`Checker::func_value`: a method's receiver
  is its first parameter; a name without `ref` is `E077`: `Naming`,
  which `check_name`, `static_member` and `own_value` pass on). A generic
  function's value is one instance (phase 4): `ref id[@bit32]` parses as
  an `Expr::TypeApp` and `ref Box[@bit8].convert[@bit32]` (brackets past
  anything but a path) as an `Expr::Instance`; `check_type_app` rebuilds
  the path's chain (`paths::path_expr`) and `check_instance_value`
  resolves it with `Naming::generic_args`, which `func_value` appends to
  the owner's (`E044` on a wrong count, none inferred). Any expression
  of the type is callable (`Checker::value_call`, `E022` otherwise) as
  `HirCallee::Value`; a
  member call `value.f(x)` is a method or a field call by what `f` is.
  Lowering (`Lower::emit_call`) reads the callee into a place first, then
  the arguments, then loads it into a register in the call block (a
  register lives in its block), and calls `LirCallee::Reg`, or `CReg`
  through `emit_c_call` (`Callee::Place`) for C. A function reference is
  opaque like `ref unknown` (`Types::is_opaque_ref`): never
  auto-dereferenced, no fields, `@read`/`@write`/`@drop` `E010`, `==`
  compares addresses; `Types::read_type` is the type a place of it reads
  as and `value_type` restores the `ref` on an expected `TypeId`. Casts go
  only through `ref unknown` (`check_cast`; another function type is
  `E010`). `lir_ty` makes a bare function type an ICE. Addresses are
  identities: the backend never sets `unnamed_addr` on a function
  (`m5b.rs`'s `functions_keep_their_addresses`).
- C function values: `ref export function (…)` (`FuncType::is_export`,
  `TypeKind::Func::c_abi`). `ref` of an `export` or non-variadic `extern`
  has that type, and `addr.func` of an export is its C entry. A call
  through one is `LirCallee::CReg` (`call c &r`), lowered by
  `emit_c_call` like an extern call (wrapper objects pass their field),
  and `check_c_function_types` makes a non-C slot or `>1` out `E028`.

## Build and link

- `lilac build [<file-or-dir>]` compiles, writes an object file
  (`Backend::emit_object`, with an `Entry` and a `CodegenStep` callback) and
  links an executable. A project's configuration is its `lilac.toml`, and
  a flag overrides its setting for one run (`docs/manifest-plan.md`);
  the language server reads it too.
- The CLI (`main.rs`): `build`, `check`, `emit-ir` and `emit-llvm` take a
  file or folder, `.` by default. The flag parsers fill
  `lilac_manifest::Overrides`, not the session; `Common::configure` reads
  a folder argument's manifest (`Manifest::read` over `StdFs`, the folder
  made absolute, so every manifest path is), resolves the flags over it
  (`Settings::resolve`, with `LILAC_LINKER` as `env_linker`), applies it
  to the session and returns what to compile (`project.root`, else the
  argument). A manifest with any error is printed through the session
  (human or JSON, with the summary) and the command exits 1 before
  compiling. A file argument reads no manifest. A setting neither gives
  is `None` and the command keeps its own default: `check`/`emit-ir`
  `x86_64-unknown-none` and `Entry::CMain`, `emit-llvm` and `build` the
  host, `build` `Entry::Native`; with a manifest the target is the host
  and `link.libc` is off. `check`/`emit-*` take stack traces only from
  the flag. `--libc[=on|off]` and `--target=host` are common flags.
- `lilac.toml` (`lilac-manifest`, std-only, no TOML crate): `toml::read`
  is TOML 1.0
  less numbers, dates, multi-line strings and `[[…]]`, all `E075` like
  every other mistake (TOML's redefinition rules: `Kind` says what made a
  table). Each key and value keeps its span; the reader recovers at the
  next line (after any array the statement left open) and returns a
  table only with no error. Arrays and inline tables nest, and keys have
  parts, at most `MAX_NESTING` (32), which bounds its recursion;
  `tests/progress.rs` checks it on random input. `settings.rs` is the
  schema: `SETTINGS` (section, key, kind, default, flag, description) is
  the one table of keys, and `Manifest::read` (through `lilac_module::Fs`,
  `None` without a `lilac.toml`) checks each against it, `E076` at the key
  or value (did-you-mean fixes for sections, keys and modes); a setting
  with an error is left out. `Settings::resolve` puts `Overrides` (the
  flags, and `LILAC_LINKER` after `--linker=`) over the manifest over the
  defaults, joining the manifest's paths to its folder, and
  `Settings::apply` sets the session. `{ bundled = true }` mounts
  `bundled_dir()/Name` (`LILAC_LIB_DIR`, else `<exe dir>/../lib`).
  `Session::build_dir` is the folder the loader skips
  (`load_with_status`'s `build_dir`; `None` is `<root>/build`); `apply`
  sets it only with a manifest, so without one nothing changes.
- `lilac init [<dir>]` (`init.rs`) copies the project template
  `templates/project/` (`lilac.toml`, `src/main.lilac`): the install
  folder's (`template_dir`: `LILAC_TEMPLATE_DIR`, else
  `<exe dir>/../templates`; `scripts/package.sh` copies the repository's
  `templates/`), read through `Fs` (`read_template`) and filled in
  (`fill`: `{{name}}` from the folder via `default_name`, `{{host}}`,
  and the line holding `{{libraries}}` becomes the `bundled_libraries`
  mounts). The manifest is written first with `create_new`, so an
  existing `lilac.toml` is an error (exit 1) and nothing is written;
  any other existing file is kept. The template's `lilac.toml` must
  state every `SETTINGS` key with its flag and the true defaults
  (`settings_tests::the_template_*` read the repository's file), so a
  new setting needs a line there.
- The build folder is the manifest's `build.dir` (`build` beside it),
  else `<project>/build` (folder project) or `./build` (single file), with
  one folder per mode (`debug`, `release`, `release-small`) and a
  `<triple>/` level for a cross target. The mode folder holds the
  executable and `obj/` with the kept object file. `--build-dir` and `-o`
  override it; the program's name is `project.name`, else the folder's or
  file's. In a manifest build the linker runs in the manifest's folder
  (`link::run`'s `dir`), with the object, the executable, the sysroot and
  a linker path made absolute, so a relative `link.args` or
  `--link-arg=` is relative to the manifest.

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
  `memcpy`/`memmove`/`memset`/`memcmp` (IR text marked `no-builtins`),
  plus `_fltused` and
  `__chkstk` on Windows (`lilac-codegen-llvm`'s `native.rs`). Linux links `-static`, or with `-dynamic-linker` (from `LinkInfo`)
  when a `--link-arg` names a shared library (`.so`, `-l`). macOS links
  `libSystem` (Apple's only stable system interface; lld ad-hoc signs arm64
  executables) and takes `memcpy`/`memset`/`bzero`/`memcmp` from it, so the backend
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
- Settings (`manifest.rs`, `Server::read_manifest`): the workspace's
  `lilac.toml`, read from disk by `Manifest::read` and resolved with the
  `lilac.libraries` setting as the `--lib=` overrides
  (`Settings::resolve`), so the editor checks what `lilac build` builds:
  `new_session` calls `Settings::apply` (root, libraries, target, mode,
  libc, build folder; `require_entry` stays off), and without a manifest
  the session is the default one. An `E075` keeps the last good manifest
  (`Server::good_manifest`), an `E076` drops only its setting. The
  manifest's diagnostics ride in each compile's publish on `lilac.toml`
  (`Job::manifest`), with the project's spanless ones, so neither clears
  the other; a manifest makes the server compile at startup. A workspace
  with a `lilac.json` and no `lilac.toml` gets one `window/showMessage`
  warning (`lilac.json` is not read).
- Compile units: the project (the manifest's `project.root`, else the
  workspace folder); an open file it does not reach is compiled as its library
  namespace (`Session::compile_namespace_with_stages`), else its folder, else
  alone (`compile::unit_of`: alone when the parser finds a top-level `main`
  function, `declares_main`, counting a failed declaration whose header
  read `main: function`: `ItemKind::Error` keeps the name and
  `function`). Sessions run tolerant and keep their
  analysis (`Session::keep_analysis` is `Keep::Partial`; the CLI's is
  `Keep::Nothing`). `Analysis` owns the resolver output, the type table
  and the HIR, which lowering borrowed before they moved in, so keeping it
  copies nothing; the bodies of uninstantiated generics
  (`Analysis::templates`, apart from the lowered `hir.funcs`) feed
  highlighting and hovers. Every compile goes through
  `Session::begin_load` and `run_loaded`, which drop the last compile's
  analysis and report the Load stage. URIs are compared via
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
  When the next compile is due while one runs, the running one is
  cancelled: each `Job` carries a cancel token (`Job::cancel`, an
  `Arc<AtomicBool>` the server keeps as `Server::running`) that every
  session of the job gets as `Session::cancel`. The driver checks it
  between stages, typeck before each instance (`drain_queue`) and
  template, and lowering before each function; a cancelled compile
  returns an error with no diagnostic (`Unreported::Cancelled`,
  `Session::cancelled`) and drops its partial analysis. `Job::run` then
  returns `None` (`Outcome::Cancelled`), which installs no analysis and
  publishes nothing, and the due compile starts once it has stopped, so a
  slow compile never holds up the next. The compiler crates take the token
  as a plain `Arc<AtomicBool>` (std-only, no I/O).
  Each publish carries the version of the document it compiled. The
  retained analysis must stay `Send + Sync` (no `Rc` in `Resolved`, and
  `lilac_driver::Progress` is `Send + Sync`). A diagnostic without a span goes to
  `lilac.toml`, else the unit's `main.lilac`, else its first open file by
  URI (`Job::project_anchor`). The server registers watchers for
  `**/*.lilac` and `**/lilac.toml` (recompile; a change to the
  workspace's `lilac.toml` re-reads the settings) and takes `lilac.libraries` from
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
  the span's text). An f-string hole is a use of its `@format` hook
  from its braces and the space inside them (`navigate::holes_at`):
  `HirProgram::holes` has a `HoleSite` per hole and instance (the `{…}`
  span, which `FStrPart::Hole` keeps, the value's span, the formatted
  type and the hook; `Checker::record_hole`), so definition lists a hook
  per instance of a generic body, the hook's references list its holes,
  and the hover shows each type with its hook; a hover of the whole value
  shows the formatted type and adds `formatted by`. Declarations are
  rendered once (`render.rs`), as Lilac writes them, from the definition
  table and the declaration's syntax tree, never the source text: a
  function with its owner, generics, `extern`/`export` and failure outs
  (`Pair[T].get: function (self) value: T`), a type with up to
  `MAX_SLOTS` fields or variants, a field or variant with its owner as
  context, an alias, a constant or global with its initializer printed
  from its expression (elided whole to `…` past a length or depth), and
  an intrinsic from `Intrinsic::signature`. `Rendered::params` are the
  parameters' UTF-16 ranges for signature help; a call through a
  function reference (a field, global or local) renders the reference's
  type (`render::value_call`: the slots of its type, named from the
  declaration's `function (…)` or its alias's, `Scope::func_syntax`). A
  hover, on a use or on
  the declared name (`navigate::declaration_at`), is the
  declaration in a `lilac` block, its context, then its `///` doc
  (`Def::doc`, `Intrinsic::doc`, `Builtin::doc`); signature help shows
  the doc, and completion items carry the declaration as `detail` and
  the doc as `documentation`. `@array` and `@format_args` show their
  shape (`handlers::TYPE_CONSTRUCTORS`, which completion lists after `@`
  too).
- Editing (`complete.rs`, `actions.rs`): completion and signature help
  run before the edit is compiled, so the context is read from the
  document's current tokens (`context.rs`: `Context` lexes it, and answers
  in a comment, in literal text (`Token::unclosed` marks a string or char
  its line ends inside; an f-string's holes are code), the name being
  typed and the `@` or `.` before it, and the call and argument index by
  matching brackets over tokens; rename's `is_identifier` is one `Ident`
  token; nothing scans characters) and names come from the last compile, the cursor
  mapped back into its text through the edits made since
  (`documents::map_offset`). Sync is incremental: each `Document` keeps
  its line starts (`lilac_span::line_starts`, read through
  `convert::LineIndex`, which converts UTF-16 positions and is a
  subtraction on an ASCII line) and a log of its edits by version
  (`Edit`; a change with no range is its smallest edit,
  `Edit::between`), pruned once a compile of that version is installed
  (`Server::compiled_versions`, `Documents::forget_edits`). A log that
  does not account for the text's length falls back to one
  `Edit::between` of the two texts. Locals in scope come from
  `LocalSite::scope` and `ty` (typeck's `FnCx::open`/`close` track block
  spans); `@` lists `Intrinsic::names()` and the builtin types.
  After a `.` (`complete::needs_current_compile`) the request instead
  waits for a compile of the current text: the server parks it
  (`Server::parked`, skipping the debounce), answers it when a compile of
  the newest generation finishes (an edit meanwhile keeps it parked) and
  drops it on `$/cancelRequest`. The answer comes from the sites at the
  cursor (`Scope::dot_entries`): a `HirProgram::members` site lists the
  members of what typeck looked the name up in (`Checker::record_member`:
  a value's fields and methods, a type's statics and variants, a
  namespace's items, a declaration's type's static functions
  (`MemberReceiver::Constructor`, for `name: Type.function(args)`), from
  `field_access`, `method_call`, the static receivers, `check_construct`
  and `lookup_item`'s path steps); else the `HirProgram::tags`
  site lists the expected enum's variants (`Checker::record_tag`), minus
  those other `match` arms name, and none when no enum is expected. The
  driver adds the top-level conditions' tags and namespace members
  (`conditional::Sites`). The parser reads a `.` with no name after it on
  its line as an empty tag, member or path segment (one `E001`, at the
  `.`), so the rest parses; typeck stops there silently, and a selector
  holding one stays undecided (`Checker::empty_names`).
  A diagnostic's fixes ride in its published `data` (`convert::fixes`) and
  `textDocument/codeAction` turns them into quick fixes without a compile.
  A fix only replaces a span the compiler knows; one that would insert
  new code (a missing `@format`) waits for a formatter, since the
  compiler keeps no layout to place it by.
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
  the notification tells the client the view is stale. Compiles capture no
  stage until the client asks (`Job::capture_lir`, on from the first
  request): that request is parked until a compile that captured finishes
  (`Server::lir_wait`, `IrView`), and `compile::Capture`
  copies each stage but `final`, the compile's result, which it moves in.
  `lilac-ir` is now also a `lilac-lsp` dependency (alongside the compiler
  crates).

## Pipeline

lex → parse → `lilac-module` load (plus the generated `@builtin`
namespaces) → `lilac-resolve` → `lilac-typeck` (a
worklist of `(def, type args)` instances emitting HIR directly, then the
ownership pass over each body that checked) →
`lilac-lower` (HIR → LIR: the moves, drops and drop flags typeck decided,
copies, drop/copy glue, and constant conditions: only the live arm of an
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
`Os::builtin_name`), `Target::page_size` and the C ABI scalar table
(`Target::scalar_layout`).

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
  `ownership.rs` (typeck's drops and flags, temporaries, copies, glue),
  `control.rs` (`if`,
  `match`, loops, `and`/`or`), `expr.rs` (values by expression kind),
  `calls.rs` and `intrinsics.rs`.
- `lilac-ir`: `lib.rs` holds the LIR types, `print.rs` the printer,
  `verify.rs` the verifier and the post-DCE `check_live_*` checks.
- LIR names are typed (`names.rs`): `Reg` (`&`), `PlaceName` (`%`),
  `Label` (`$`) and `Symbol` (`@`, data `!`); each displays with its sigil.
  A `const` holds a `LirConst` (`Int(u64)`, `Float(FloatBits)`,
  `Data(Symbol)`); the verifier checks it fits its type (`check_const`)
  and the backend never parses text.
- Literals are read once, in typeck (`literals.rs`): the HIR holds the
  value (`HirExprKind::Int(u64)` at its type's width, a negative one in
  two's complement; `HirExprKind::Float(FloatBits)`; `HirPattern::Int`),
  and nothing after typeck parses literal text. `lilac-float` (std-only)
  is the float model: `FloatBits` is a value's exact IEEE pattern at one of
  the four widths, `FloatBits::parse` rounds a decimal literal once to its
  type (Rust's parser at 32/64 bits, an exact big-integer converter at
  16/128), and `FloatBits::apply` is IEEE arithmetic at a width (the exact
  result rounded once; `%` is `fmod`), so a literal fold
  (`Checker::fold_binop`) runs at the type the expression takes and gives
  the bits the run-time operation would. The LLVM backend bitcasts an
  integer of the pattern's width. A literal match pattern is an integer or
  a character that fits the scrutinee (`E060`); a float or string one is
  `E010` (`Checker::literal_pattern`).
- AST type syntax is a `lilac_ast::TypeShape` (`Named`, `Function`,
  `Array`, `SelfType`, `Error`): a type the parser could not read is
  `Error`, never a name. Builtin types are `lilac_ty::Builtin`, whose
  `id()` is its fixed `TypeId` in every table; look one up by spelling
  (`Types::builtin`) only for source names. Intrinsic slots carry a
  `Builtin`. Every intrinsic parameter is named (`Signature::params`, a
  `Param`); the intrinsics typeck checks specially (`Intrinsic::is_special`,
  `check_special_intrinsic`) have signatures of descriptive slots
  (`Slot::is_checked` is false: `Type`, `Param`, `RefArray`, ...), so the
  table describes every intrinsic and a unit test keeps the two in step.
- Functions that can fail (`outs else outs`, `fail`): typeck
  (`lilac-typeck/src/failure.rs`) lays out the outs as success, failure,
  then a hidden `@bit8` flag (`HirFunc::failure`, a `HirFailure`), set at
  body entry and cleared before `HirStmt::Fail`, which lowers like
  `return`. A call that can fail is allowed only where `FnCx::handled`
  names it (a capture `if`, a guard, a `loop if`); elsewhere it is `E057`.
  Callers bind every out to hidden locals and branch on the flag, so
  lowering and LIR need nothing new. Each side moves every hidden out it
  owns into a capture or a hidden local (`bind_captures`, `bind_rest`):
  the ownership pass tracks a hidden out as full only on its side of the
  flag (`Own::Cond`, refined by `if flag`), so none is left to drop.
- `!=` and `not` are built in (no hook): typeck emits `HirExprKind::Not`
  (`a != b` is `Not(Eq(a, b))`), lowering `c == 0`, and the constant
  evaluator folds it. A compound assignment (`Stmt::CompoundAssign`,
  `x += e`) is desugared in typeck, not the parser
  (`check_compound_assign`): the target is checked once and its place is
  the left operand of `operator_call`, then `assign_checked` stores the
  result, so the HIR is that of `x = x + e`.
- Ownership (`lilac-typeck/src/places.rs`, `Checker::check_places`): a
  tree walk over each instance body that checked without an error
  (`Sink::holds_error`). The state is an `Own` per local (full, empty,
  maybe, per field, or `Cond`); branches merge and a loop body is walked
  on copies (`dry`: no diagnostics, no flags) until its head state is
  stable, then for real. `merge` is the join of a lattice of finite
  height (unit-tested: commutative, associative, idempotent, monotone), so
  the heads only climb, at most `Pass::round_bound` times (the sum over
  the body's locals of `height` of their type, plus one each, plus one);
  a loop still changing past it is an ICE, never a guess. It decides
  everything lowering does with ownership: a moving read is
  `HirExprKind::Move` (`@move(a)` writes one too), each block ends in a
  `HirStmt::Drop` of its full locals (each early exit drops the scopes it
  leaves, and outs per `E065`'s rule), `HirStmt::Assign::old` says what
  happens to the old value (an out's first assignment is `HirOld::Empty`),
  `HirArm::moves` says a capture moves the payload, and `HirFunc::flags`
  are the drop flags of places that may be empty where they drop.
  Lowering sets a place's flags (and those inside it) when it is
  initialized and clears them when it moves (`Lower::mark`), and never
  decides a drop itself; it only drops statement temporaries.
  `Checker::is_copyable`/`needs_drop` are the copy and drop rules
  (`Param` copies, so templates rarely fail). `E063`/`E064` go through
  `emit_requirement` (`Requirement::Ownership`), so a failing generic
  instance folds them into `E042`; `E065` is plain.
  `@panic`, `@trap` and `@compile_error` end a path, and a `match`
  naming every variant has no `else` path. `@discard(x)` makes `x`
  `Own::Discarded`: per field like `Parts`, but its `@on_drop` does not
  run and its fields may move out; a path that joins one without
  discarding it makes it `torn` (`E066`, reported where it drops).
  A method whose first parameter is `self: T` (named `self`, of the
  owner's type) takes its receiver by value (`Checker::method_call`);
  bare `self` is `ref T` (`lilac_ast::Param::is_receiver`).
- Reference captures (`docs/ref-captures-plan.md`): `-> v: ref P` on a
  `P` payload (typeck's `check_pattern`, decided per instance, so a
  generic `ref T` with `T = ref X` is the plain copying capture) sets
  `HirArm::by_ref`/`HirValueArm::by_ref`: the capture holds the payload
  slot's address. The ownership pass skips it (`arm.moves` stays false,
  the enum stays full), lowering binds it with `Binding::Address`
  (`control.rs`, through `Lower::address_of`, which `ref_value` shares),
  and the constant evaluator refuses it. A scrutinee that is not a place
  is `E010` through `emit_requirement` (`check_ref_capture`,
  `Requirement::Ownership`), so it is dependent in templates and `E042`
  in instances.

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
- A panic message is what `@compiler_print` takes (`M`: `@byte_buffer` or
  an `@format_args(W)`, `sigs::platform_hook_shape_ok`; one definition
  whatever its type, `E034`). Typeck checks `@panic`'s argument against
  it with the slot rules (`Checker::check_panic`, `panic_hook_message`;
  `@byte_buffer` with no hook) and records it as
  `HirProgram::panic_message`; lowering builds its generated messages as
  `M` (`Lower::panic_text`: a data constant, or a hole-free template), and
  the driver builds the handler for the hook it finds
  (`lilac_ir::PanicMessage::of`, `panic_runtime(message)`). A format
  handler writes its text as templates and each trace site through a
  `#.lilac.format_line` on its stack. The handler first bumps the
  thread's panic depth (`@panic_depth`, a thread-local slot like the
  trace head's): a second panic writes `panic while writing a panic
  message` and exits 101, a third traps.
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
- Integer division (`@i_div*`/`@i_mod*`) panics: lowering
  (`Lower::check_division`) tests the operands before the `i.div`/`i.mod`
  and branches to `.lilac.panic` with "division by zero", and for the
  signed forms `MIN` with `-1`, "division overflow", sited at the
  intrinsic call. A test the constant operands rule out is not emitted,
  so a backend never sees a zero divisor or the overflow (an `IR_SPEC.md`
  precondition); `branches` counts a division as branching. The constant
  evaluator says why a division is not constant
  (`constant::division_panic`, `eval_arith`'s `Err(DivisionPanic)`), and
  a live one that panics is `E060` like `E037`: `Lowered::bad_divisions`,
  reported by `lilac_lower::check_constant_divisions` before DCE. It is
  found at the intrinsic (a constant zero divisor alone, or both
  operands for `MIN / -1`), at a `=>` call whose body reaches one with
  the call's constant arguments and the others unknown
  (`Lower::inline_call_panic`: `Standard`'s `/` hooks; a body that panics
  with no argument known is its own function's report), and in a global
  initializer. Typeck's literal fold (`1 / 0`) uses the same message.
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
  the `E051` recursion limit, the `E068` instance budget, a dropped
  `@entry`; it keeps the error, which later requests return) makes
  `request` fail. When a callee cannot be found or used,
  `Checker::callee_failed` (or `callee`, for a `Result`) still checks
  every argument against the error type, so errors inside them are
  reported. A silent `Err(ErrorGuaranteed::new())`
  carries a comment naming the diagnostic that covers it.
- Generic limits (`request`, `ensure_type`): recursion is counted per
  definition, as rustc does. `Instance::recursion` counts each generic
  definition on the instance's first-origin chain; one appearing past
  `MAX_RECURSION` (64) is `E051` (`recursion_error`), and so is a
  definition laid out inside itself past 64 times (`Checker::type_counts`
  over `type_stack`). Chains of distinct generics are unlimited; types
  nest at most `MAX_TYPE_NESTING` (1,024) deep, a native-stack bound (also
  `E051`). A recursive request (its definition already on the chain) goes
  to the front of the queue (`Checker::front`), so recursion runs depth
  first and branching recursion meets `E051` after 64 bodies; then every
  later recursive request of that definition fails with it
  (`recursion_limited`, `type_recursion_limited`), so it ends in linear
  time. When the arguments grew (each contains the one it replaced,
  `Checker::grew`), a note shows the first two instances. Each pass (the
  program, then the templates) builds at most `MAX_INSTANCES` (50,000)
  generic instances; the next request is `E068` (`instance_budget`),
  reported at once (`Sink::report_now`, never folded or dropped), naming
  the busiest definitions; later new instances fail with it. Types in
  typeck's diagnostics go through `Checker::show` (`Types::display_bounded`,
  200 characters): a type nested `n` deep can be `2^n` long, so
  `Types::contains_param` is decided once per type at interning.
- `E042` traces every site that requested the failing instance
  (`Checker::origins`, recorded on every `request`, cached or not), through
  generic requesters down to plain functions (`Checker::instance_trace`).
  `E042`s are held in `Checker::failures` until the worklist drains
  (`drain_queue`), so origins requested later are included. `E051` keeps its
  first-origin `instantiation chain:` note.
- A generic instance's member hooks register with the type
  (`register_instance_hooks`) but their bodies are `State::Parked`
  (`Checker::park`) until a use (`Checker::use_hook`: a hook call, an
  operator, a conversion) queues them and becomes their origin, so a hook
  that fails for some `T` is `E042` only where it is used with that `T`.
  `@on_drop`/`@on_copy`/`@on_override` stay eager (glue uses them).
  A parked instance does not count as instantiated, so a member hook no
  instance used is checked as a template.
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

- An ICE is `lilac_error::bug!(span, …)`, at the most specific span in
  scope. The LLVM backend points each at the LIR instruction or
  terminator it is lowering (`Lower::at`, set per instruction in
  `lower_func`); only declarations and the glue it writes itself
  (`runtime.rs`, `threads.rs`) use `Span::dummy()`.
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
- Lex errors (`E000`) never stop the lexer, and one typo is one error.
  `Lexer` yields `Err` only for bytes that form no token (an unexpected
  character, named as "`#`" or by code point "U+FEFF", or a lone `$`); an
  error inside a token (a bad escape, a lone `}` in f-string text, a
  literal its line ends inside, a malformed number) is side output
  (`Lexer::take_diagnostics`, merged in source order by `lex_all`) and
  the token is still yielded, so the parser sees no hole (a malformed
  number keeps the well-formed part it read as its text). Every item
  consumes input, except the end of file and the zero-width `FStrEnd`
  closing an f-string its line ends inside: `Lexer::progressed` is a
  `debug_assert!`, and a release build skips a character with an
  internal `E000` (`force_progress`). `lilac-lex/tests/progress.rs`
  checks the property on random input. A byte-order mark at offset 0 is
  trivia, and `SourceFile` starts line 1 after it, so columns match the
  editor's.
- Comments are trivia kept as side output (`Lexer::take_comments`: each
  `Comment`'s span and `CommentKind`). A run of `///` lines starting their
  lines, ending on the line before a token, is that token's `Token::doc`
  (a `lilac_span::Doc`: the Markdown text and its span); the parser gives
  it to the declaration the token starts (`Item::doc`,
  `EnumVariant::doc`) and drops it anywhere else, and the resolver keeps
  it as `Def::doc`. `Standard` is documented this way; a comment over a
  group of declarations stays `//`, and
  `lilac-parse/tests/standard_docs.rs` checks every `///` in it is
  attached.
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
