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

## Bytecode VM (M5c)

- `lilac-vm` is a **second backend**: it lowers verified LIR to canonical `.lbc`
  text and interprets it, without linking LLVM. The VM and its runner never
  depend on `inkwell`/`llvm-sys`, which stay confined to `lilac-codegen-llvm`.
- The VM target is `lilacvm-unknown-none` (`Arch::LilacVm`, 64-bit). Selecting
  it selects the VM backend; the single pointer-width rule in `lilac-target`
  now covers `Arch::LilacVm` as well.
- The ISA, `.lbc` format, memory model, entry/panic/`extern` behavior, and
  debugger are documented in `docs/VM_SPEC.md`.

## Gates

Run before committing:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo clippy -p lilac-vm --all-targets --features bench-baseline -- -D warnings
cargo test -p lilac-vm --features bench-baseline
```
