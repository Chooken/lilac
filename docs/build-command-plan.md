# Plan: `lilac build` and a self-contained toolchain

## Goal

`lilac build` compiles a Lilac file or project, links it, and leaves a
runnable executable, printing its progress as it goes.

Installing Lilac should be pain-free: unzip one folder and build, with no C
toolchain, Visual Studio, Xcode or `gcc` required. The design should stay
simple and robust: no searching the machine for toolchains, and the same
build behaviour everywhere.

```
lilac build [<file-or-dir>] [-o <path>] [--build-dir=<dir>] [--mode=…|--release]
            [--target=<triple>] [--lib=<Name>=<dir>]… [--libc] [--linker=<path>]
            [--link-arg=<arg>]… [--timings] [-q|--quiet] [--error-format=…]
```

### Configuration comes from the command line

Everything a build needs is passed as a flag. `lilac.json` stays an
editor-only convenience for the language server, and the CLI does not read
it.

The long-term plan is a Zig-style build file: a Lilac program that tells the
compiler what to build. That waits until Lilac is self-hosted. Until then
the flags are the whole interface, so keep them complete and scriptable.

### The build folder

`lilac build` owns a build folder, and everything it produces goes there:

```
build/
  debug/            hello.exe, plus obj/hello.obj and anything else the link needs
  release/
  release-small/
  <triple>/debug/   only when --target is not the host, like cargo's target/<triple>/
```

- **Location:** for a folder project, `<project>/build`. For a single file,
  `build` in the current directory.
- **Overrides:** `--build-dir=<dir>` moves the build folder. `-o <path>`
  puts the final executable somewhere else; intermediate files stay in the
  build folder.
- **Contents:** the mode folder holds the executable, `obj/` with the object
  files, and later anything else the compiler needs between builds (stubs
  copied for the link, a future cache). Objects are kept, not deleted, so
  `--keep-obj` is unnecessary.
- **Folder names** come from `OptimizeMode` (`debug`, `release`,
  `release-small`), and the executable is named after the file stem or the
  project folder.

## Design decision: Lilac talks to the OS, not to libc

This is Go's model. Each platform has one stable interface for programs, and
Lilac's runtime uses exactly that:

| OS | Stable interface | What Lilac ships |
|---|---|---|
| Windows | `kernel32.dll` | a small `kernel32` import stub (`.lib`) |
| macOS | `libSystem.dylib` (raw system calls are not stable; Go switched off them in Go 1.12) | a small `libSystem.tbd` text stub |
| Linux | direct kernel system calls | nothing |

**Linking:** Lilac ships one `lld` binary and runs it with a fixed command
line for each target. lld covers every platform, so cross-compiling is just
`--target`.

**Entry point:** Lilac provides its own process entry point instead of C's
startup code.

**libc is an opt-in, never the default.** Neither the stdlib nor a default
build uses it. `--libc` is a permanent opt-in for programs that link
*static* C libraries, the same way Go keeps cgo:

- It links through a C compiler driver (`--linker`, `LILAC_LINKER`, else
  `cc`/`clang` on `PATH`), with a C `main` and C's startup code, as today.
- It is the only mode that looks anything up on the machine, and only users
  who choose it need a C toolchain.
- During the `Standard.C` phase-out it also keeps today's programs building.

**C libraries in native mode.** Shared libraries work without `--libc`. An
`extern` plus `--link-arg=<import lib or .so/.dylib>` is enough, because a
shared library brings its own libc dependency:

- **Windows:** a DLL carries its own C runtime, built in or imported from
  `ucrtbase.dll`, which ships with Windows.
- **macOS:** libc is part of `libSystem`, which every program links.
- **Linux:** the dynamic loader loads `libc.so` for the library. This needs
  dynamically linked output (step 3).

Rules for C interop, for the docs:
- Free memory a library allocated with that library's own free function,
  never with Lilac's allocator.
- A native program exits with the raw OS call, so libc's exit hooks don't
  run. A library's buffered `stdio` output can be lost unless the library
  flushes it or the program uses `--libc`.

Static C libraries (`.lib`/`.a`) almost always need libc at link time. They
use `--libc`.

Why this fits Lilac:

- **It extends the existing runtime rule.** The runtime is written once as
  LIR, and backends hand-write only leaf intrinsics. The OS functions follow
  the same rule:
  - on Windows and macOS they are plain LIR `extern` calls into the shipped
    stubs, so no backend code is needed;
  - only Linux needs one new leaf, `@syscall`.
- **One table holds the per-platform facts** (see below). This is the same
  idea as `Arch::is_64_bit` being the single pointer-width rule.
- **Nothing depends on the host machine.** There's no toolchain discovery
  and no C runtime version to match. Builds come out the same on any machine.

### Install layout

```
lilac(.exe)
lib/lilac/bin/lld(.exe)                       one binary, run as `lld -flavor <link|gnu|darwin>`
lib/lilac/x86_64-pc-windows-msvc/kernel32.lib
lib/lilac/aarch64-apple-macos/libSystem.tbd
lib/Standard/…                                the stdlib
```

`lilac` finds `lib/lilac` relative to its own executable, as rustc finds its
sysroot. No path from the build machine is ever compiled in.

- **Development builds** (`target/debug/lilac`) have no `lib/lilac` next to
  them. `LILAC_SYSROOT=<dir>` points at one. The test harness sets it to a
  folder it assembles from the repo's stub files plus
  `$LLVM_SYS_181_PREFIX/bin/lld` (cargo already provides that variable from
  the local `.cargo/config.toml`).
- **Stub files** live in the repo under `toolchain/<triple>/`:
  - `kernel32.lib` is generated from a checked-in `kernel32.def` (only the
    functions the runtime uses) with `llvm-lib /def:`;
  - `libSystem.tbd` is hand-written text listing the symbols we use.

### Phasing out `Standard.C`

`Standard.C` is removed piece by piece. Each piece gets a native Lilac
replacement, the run tests move to it, and then the C binding is deleted.
Until a piece is gone, programs that use it build with `--libc`.

Proposed order, easiest and most used first:

1. **`Heap/c_allocator`** (`calloc`/`free`) becomes `Standard.Heap.Allocator`
   over the runtime's `.lilac.alloc`/`.lilac.free`:
   - Windows: `HeapAlloc`/`HeapFree`;
   - Linux: `mmap`/`munmap`;
   - macOS: libSystem's `malloc`/`free`.

   Done in step 2.
2. **`io`** (`putchar`, `puts`, `fwrite`, …) becomes `Standard.Io` over
   `.lilac.write`. It starts with standard output and standard error; files
   come later. Done in step 2.
3. **`memory`** (`memcpy`/`memset`/`memcmp`) becomes plain Lilac functions,
   or the runtime's own `memcpy`/`memset` (which native mode defines anyway).
4. **Character classification** (`isalpha`, `toupper`, …) becomes pure Lilac.
   These are table lookups or range checks, with no OS involved.
5. **`c_string`** (`strlen`, `strcmp`, …) becomes Lilac string and
   byte-buffer helpers.
6. **Float math** (`double_math`, `float_math`) is ported to Lilac, which is
   the largest piece. Simple operations can use LLVM intrinsics that lower to
   instructions (`sqrt`, `fabs`, `floor`/`ceil` on x86-64 SSE4.1 and
   AArch64). Transcendental functions (`sin`, `exp`, `log`) need real
   implementations; a port of musl's libm is the usual source.
7. **The C type aliases** (`c_int_windows`, `c_long_long`, …) go with the last
   user.

When the list is done, delete `lib/Standard/C`, and keep the run-test
`// libc` directive only for tests of C interop itself. `--libc`,
`Entry::CMain` and the libc branch of `link.rs` stay, as the permanent
opt-in for static C libraries.

## Where things stand

- `emit-llvm` produces textual IR for the host. Whenever `main` exists,
  `lower.rs` emits a C `i32 main()` wrapper that calls `.lilac.main`.
- The run tests (`crates/lilac-cli/tests/run.rs`) write the `.ll`, link it
  with the toolchain `clang`, and run it. 24 of them print through
  `extern putchar` or set the exit code with `exit`.
- `Backend::codegen` is a stub that returns an error, and nothing calls it.
- `Session::run` reports only the four LIR stages through `on_stage`. The
  driver does no I/O, and inkwell stays confined to `lilac-codegen-llvm`.
- `@lilac_abort` already uses `llvm.trap`. `.lilac.memcpy` wraps
  `llvm.memcpy`, which may lower to a call to the C library's `memcpy`; the
  native runtime must define `memcpy`/`memset` itself (step 2).

## Step 1: the `lilac build` command (libc mode only)

**Status: done.** Native mode currently fails at object emission with "the
native entry point is not implemented yet; use --libc" (exit 1) rather than
the up-front exit-2 check described below, so the native link path is
already wired and only needs the entry point and runtime from step 2.

At the end of this step, `lilac build --libc` works everywhere a C
toolchain exists, and the progress output is done. Until step 2, native mode
reports "not yet supported for <target>; use --libc" (exit 2).

### 1a. Object emission (`lilac-codegen`, `lilac-codegen-llvm`)

- In the `Backend` trait, replace the dead `codegen` method with:

  ```rust
  fn emit_object(&self, module: &LirModule, entry: Entry,
                 on_step: &mut dyn FnMut(CodegenStep)) -> Result<Vec<u8>, String>;
  ```

  These types live in `lilac-codegen`:
  - `CodegenStep { Lower, Verify, Optimize, Emit }`, fired before each step;
  - `Entry { CMain, Native }`.

  `CMain` is today's wrapper. `Native` is used from step 2 on.
- In `LlvmBackend`, split the body of `emit_llvm` into a private
  `build_module` that does lower, verify and optimise. Both `emit_llvm` and
  `emit_object` call it. `emit_object` then calls
  `machine.write_to_memory_buffer(&module, FileType::Object)`.
- Object output uses `RelocMode::PIC` on ELF targets. The `emit-llvm` text
  stays byte-identical.
- Tests:
  - a minimal module produces an object with the right header bytes
    (`0x64 0x86` for COFF x86-64, `\x7fELF` for ELF);
  - `emit_llvm` output for an existing case is unchanged.

### 1b. Per-target link table (`lilac-target`)

```rust
pub struct LinkInfo {
    pub flavor: LinkFlavor,          // Coff | Elf | MachO  → lld -flavor link|gnu|darwin
    pub exe_suffix: &'static str,    // ".exe" | ""
    pub obj_suffix: &'static str,    // ".obj" | ".o"
    pub entry_symbol: &'static str,  // native entry: ".lilac.start" (Mach-O adds its `_` prefix)
    pub system_libs: &'static [&'static str], // ["kernel32.lib"] | ["libSystem.tbd"] | []
}
impl Target { pub fn link_info(&self) -> Option<LinkInfo> }   // None: unsupported target
```

This is the single place for per-OS linking facts. It is pure data and has a
unit test for each supported triple.

### 1c. Linking (new module `crates/lilac-cli/src/link.rs`)

- **Native mode** (enabled in step 2):
  - find the sysroot: `LILAC_SYSROOT`, else `<exe dir>/../lib/lilac`, else an
    error naming both;
  - run:

    ```
    <sysroot>/bin/lld -flavor <flavor> <obj> <sysroot>/<triple>/<system_libs>
        <entry/subsystem flags for the flavor> -o <exe> [link-args…]
    ```

    The extra flags are `/entry:` and `/subsystem:console` for COFF,
    `-e`/`-static` for ELF, and `-e`, `-platform_version` and `-arch` for
    Mach-O. The flags are built from `LinkInfo`, with no host checks.
- **libc mode** (`--libc`):
  - find a driver: `--linker`, else `LILAC_LINKER`, else `cc`, then `clang`,
    on `PATH`;
  - run `<driver> <obj> -o <exe> [--target=<triple> when cross] [link-args…]`.
- Object files are written to `<build>/<mode>/obj/<name><obj_suffix>` and
  kept, so a failed link can be inspected and retried.
- Errors:
  - a missing sysroot or driver gets one message listing where `lilac`
    looked, and exit 1;
  - a non-zero linker exit prints `error: linking failed` with the linker's
    stderr and the command line, and exits 1.
- Unit tests are pure: an injected environment and an injected exists-check
  for discovery, and argument construction for each flavor and for libc mode
  (host and cross).

### 1d. Entry-point check (`lilac-driver`, new `E032`)

- `Session.require_entry` is set by `build` only. When it is set and the root
  namespace has no non-`extern` `main`, report
  `E032: no \`main\` function; \`lilac build\` needs an entry point`, with a
  hint to use `lilac check` for libraries.
- Add the doc `docs/errors/E032.md` and a driver unit test. The other
  commands don't change.

### 1e. Progress reporting

**Driver hook.** A field on `Session`, so existing signatures and the LSP
don't change:

```rust
pub enum Stage { Load, Parse, Resolve, Check, Lower, Fold, Runtime, Prune, Verify }
pub enum StageEvent { Begin, End { count: Option<usize> } }
pub progress: Option<Box<dyn FnMut(Stage, StageEvent)>>
```

The driver stays I/O-free and clock-free. The counts are files loaded,
functions checked, and functions left after pruning.

**CLI reporter** (`crates/lilac-cli/src/progress.rs`). It writes to stderr in
cargo's style, with verbs right-aligned in 12 columns, and prints each line
when its step *starts*:

```
   Compiling hello (tests/run/hello.lilac) [debug, x86_64-pc-windows-msvc]
     Loading 1 file
    Checking 14 functions
    Lowering
  Optimizing O0
     Writing hello.obj
     Linking hello.exe (lld)
    Finished debug in 0.84s → D:\Repositories\Lilac\hello.exe
```

- The verbs group the pipeline into the steps a user cares about:
  - Loading: load and parse;
  - Checking: resolve and type check;
  - Lowering: lower, fold, runtime, prune and verify;
  - Optimizing: the backend's lower, verify and optimise steps;
  - Writing: object emission;
  - Linking: the link.
- `--timings` adds each step's duration, shows the individual driver stages,
  and prints the full linker command line.
- `-q`/`--quiet` prints only diagnostics and errors.
- `--error-format=json` turns progress off.
- Colour (bold green verbs, bold red `error:`) is used only when stderr is a
  terminal and `NO_COLOR` is unset.
- On failure, the diagnostics print first, then
  `error: could not compile \`hello\` (3 errors)`, and the exit code is 1.

### 1f. The subcommand (`crates/lilac-cli/src/main.rs`)

- Reuse `parse_common`. Build-only flags go in `parse_build_flags`, to stay
  under the clippy pedantic line budget.
- The default target is the host, as with `emit-llvm`.
- `<file-or-dir>` defaults to `.`. A directory is built as a folder project,
  a file as a single file.
- Output goes to the build folder (see "The build folder") unless `-o` is
  given. The CLI creates the folders; the driver stays I/O-free.
- Flow:
  1. compile (with `require_entry` and progress set);
  2. print the diagnostics;
  3. `emit_object` (`CMain` with `--libc`, else `Native`);
  4. write the object file;
  5. link;
  6. print `Finished`.

  `--dump-lir` works here too.
- Exit codes: 0 on success, 1 on a compile or link failure, 2 on bad usage.
  Update `usage()` and the module docs.

### 1g. Tests

- New `crates/lilac-cli/tests/build.rs`. Libc-mode cases are skipped when no
  driver is found. It covers:
  - build and run `hello` (stdout matches);
  - progress lines in order;
  - `-q` printing nothing;
  - json mode printing no progress;
  - `E032` with no output file;
  - a type error giving `could not compile` and exit 1;
  - a missing linker giving a clear error;
  - the build-folder layout (`build/debug/…`, `build/release/…`, the
    `<triple>/` folder for a cross target), `--build-dir`, and `-o`;
  - a folder project using `--lib=Standard=lib/Standard`.
- `tests/run.rs` switches to `lilac build --libc -q -o …`. The harness sets
  `LILAC_LINKER` from `LLVM_SYS_181_PREFIX`, and its own clang lookup only
  decides whether to skip.

## Step 2: native runtime on Windows (the default becomes native)

- **The OS layer in `lilac-ir::runtime`.** `build_runtime` takes the target
  OS. On Windows it adds:
  - `.lilac.write(fd, ptr, len)`, built on `GetStdHandle` + `WriteFile`;
  - `.lilac.exit(code)`, built on `ExitProcess`;
  - `.lilac.alloc(size)` / `.lilac.free(ptr)`, built on `GetProcessHeap` +
    `HeapAlloc`/`HeapFree`.

  These are plain LIR `extern` calls, so no backend code is added.
- **`memcpy`/`memset`.** The native runtime defines these symbols itself,
  because LLVM may emit calls to them, as Rust's `compiler-builtins` does.
  libc mode leaves them to libc.
- **Native entry.** `Entry::Native` emits `.lilac.start`, which calls
  `.lilac.main` and then `.lilac.exit(0)`. The COFF link uses
  `/entry:.lilac.start /subsystem:console`.
- **Toolchain stubs.** Add `toolchain/x86_64-pc-windows-msvc/kernel32.def`,
  plus a script or build step that turns it into `kernel32.lib` with
  `llvm-lib`. Decide whether to check in the generated `.lib` (it is tiny);
  I recommend checking it in so the build doesn't need `llvm-lib`.
- **Stdlib.**
  - Add `Standard.Io` (`print`, `print_line` over `.lilac.write`) and
    `Standard.Process` (`exit`), plus `Standard.Heap.Allocator` (native).
  - The rest of `Standard.C` stays for now and requires `--libc` (see
    "Phasing out `Standard.C`").
  - `tests/run/stdlib` keeps `// lib:` plus a new `// libc` directive.
- **Run tests.** Move the 24 programs from `extern putchar`/`exit` to
  `Standard.Io`/`Standard.Process`. Tests that use `Standard.C` get
  `// libc`.
- **Make native the default** on Windows targets. Running `build` without
  `--libc` on Linux or macOS reports "native runtime not yet available for
  <target>; use --libc".
- **Release packaging.** Add a `scripts/package` script that assembles the
  install layout: `lilac`, `lld` from the LLVM tree, the stubs, and
  `lib/Standard`.

## Step 3: Linux

- Add one new backend leaf, `@syscall(n, a0..a5)`, as inline assembly in
  `lilac-codegen-llvm`, for x86-64 and AArch64.
- The OS layer uses `write`, `exit_group`, and `mmap`/`munmap`.
- The `_start` entry point has to fix stack alignment (the x86-64 entry
  stack is 16-byte aligned *without* a return address). Use a tiny naked
  stub or an `alignstack` attribute.
- With no shared libraries, the link is
  `lld -flavor gnu -static -e .lilac.start`, with no stubs.
- **Dynamically linked output** for programs that link a `.so`: emit an
  executable with an interpreter entry (`-dynamic-linker
  /lib64/ld-linux-x86-64.so.2`, or `/lib/ld-linux-aarch64.so.1`, in
  `LinkInfo`) and link the `.so` files given with `--link-arg`. The loader
  then brings in `libc.so` for those libraries. lld can link straight
  against a `.so` file, so this needs no sysroot. Add a run test that links
  a small shared library, built by the test itself, without `--libc`.

## Step 4: macOS

- Add a hand-written `libSystem.tbd` listing `write`, `exit`,
  `malloc`/`free`, and, optionally, the math symbols.
- The OS layer makes plain `extern` calls into it.
- The link is `lld -flavor darwin -arch arm64|x86_64 -platform_version macos
  <min> <sdk> -e .lilac.start libSystem.tbd`.
- On Apple Silicon executables must be code-signed. lld adds an ad-hoc
  signature itself; verify this on real hardware.

## Docs (with each step)

- **AGENTS.md:**
  - a "Build and link" section: native mode as the default; `--libc` as the
    permanent opt-in for static C libraries; shared C libraries working in
    native mode, plus the interop rules (free with the library's own free
    function, exit hooks don't run); `LinkInfo` as the single per-target table; the
    build-folder layout; sysroot discovery (`LILAC_SYSROOT`, then next to the
    executable); the rule that the CLI takes all build configuration as
    flags (`lilac.json` is for the language server only); and the rule that
    the progress hook keeps the driver I/O-free;
  - add `Entry`/`CodegenStep` to the Pipeline section.
- **SPEC.md:** `main` is the entry point and `build` requires it; programs
  don't depend on libc.
- **The README:** installation (unzip and add to `PATH`) and `lilac build`
  usage.

## Open questions

1. **Native float math** (phase-out item 6): port musl's libm, or write our
   own. This isn't needed for `build`.
2. **Build cache.** Should the build folder later cache per-module LIR or
   objects (`Session.cache_dir` is already reserved for this)? This isn't
   needed for the first version.
