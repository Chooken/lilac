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
  tables) is written once as ordinary LIR in `lilac-ir::runtime` and shared by
  all backends. Backends hand-write only the three leaf intrinsics
  (`@ptr_add`, `@memcpy`, `@lilac_abort`).
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
- Libraries: `--lib=Name=dir` (CLI) / `lilac.libraries` (LSP
  `initializationOptions`) mount `dir` as the top-level namespace `Name`;
  `Session.libraries` carries them into `lilac_module::load_with_status`.
  `using *.A` is relative to the importing file's namespace. `lib/Standard`
  is only usable mounted (`--lib=Standard=lib/Standard`); the run test
  `tests/run/stdlib` exercises all of it.
- Visibility reach is ancestor-public + descendant-private (Rust's rule). A
  top-level `private { … }` excludes an item from `using` (importers see it as
  unresolved, not `E026`); an object-member `private { … }` gates external access
  with `E026`. Top-level hooks are unit-global and carry no namespace prefix.
- Namespace qualification: every non-root symbol/type is prefixed by its dotted
  namespace path (`A.B.foo`, `A.Object.Method`); the **root namespace adds no
  prefix**, so existing single-file snapshots stay byte-identical and `main`
  stays `main`. `extern` symbols stay unmangled, and a file argument on the CLI
  is single-file mode (its parent is not a project root).

## Extern C ABI (M6b)

- `extern` declarations and calls use the platform C ABI on the LLVM backend:
  bit integers by value, `ref T` → pointer, 0 outs = `void`, 1 out = the C
  return value, and an unmangled symbol equal to the declared name. `>1` out or
  an unsupported param/return is `E028`.
- `E028` is validated pre-codegen in the driver via
  `lilac_lower::check_extern_signatures`, so `check`/`emit-llvm` report it as an
  ordinary diagnostic rather than a backend failure. `lilac_ir::is_c_abi_type` is the single accept-set shared by that
  frontend check and `lilac-codegen-llvm`'s signature lowering, so they cannot
  drift.

## Language server (lilac-lsp)

- `lilac-lsp` is the editor language server: a stdio binary that speaks the
  Language Server Protocol and publishes compiler diagnostics for `.lilac`
  projects. It is editor tooling, not part of the compiler pipeline.
- It is the **only** crate permitted to depend on `lsp-server`, `lsp-types`,
  and `serde_json` (and thus serde). The compiler crates stay std-only.
- The VSCode extension under `editors/vscode/` launches it via the
  `lilac.serverPath` setting (default `lilac-lsp` on PATH).
- The driver's `Session::project_files()` (fed by
  `lilac_module::load_with_status`) is the `FileId -> PathBuf` map the server
  uses to attach diagnostics to document URIs.
- The `lilac/lir` request and `lilac/lirInvalidated` notification are the
  IR-view protocol: the request serves a captured stage's rendered LIR text, and
  the notification tells the client the view is stale. `lilac-ir` is now also a
  `lilac-lsp` dependency (alongside the compiler crates).

## Pipeline

lex → parse → `lilac-module` load → `lilac-resolve` → `lilac-typeck` (a
worklist of `(def, type args)` instances emitting HIR directly) →
`lilac-lower` (HIR → LIR, including the ownership model: copies, moves,
drops, drop/copy glue) → `lilac-const` fold → runtime injection → DCE →
live `@compile_error` check → verify → backend. `Session::run` in
`lilac-driver` is the only orchestration.

Run tests live in `tests/run/` (a file, or a folder with `main.lilac`); see
the header of `crates/lilac-cli/tests/run.rs` for the `// expect-*`,
`// pending:` and `// lib:` directives. Add a run test for every bug fix that
changes generated code.

## Gates

Run before committing:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
