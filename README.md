<div align="center">
    <picture>
        <img 
        src="https://github.com/Chooken/lilac/blob/main/assets/LilacLogo.svg" 
        width="60%">
    </picture>
</div>

## Installing

Lilac installs as a single folder and needs nothing else on the machine: no
C compiler, SDK or C library. It links programs with its own copy of `lld`.

```
bin/lilac(.exe)        the compiler
lib/lilac/             the linker and system stubs it uses
lib/Standard/          the standard library
```

Unzip the folder anywhere and add its `bin` folder to your `PATH`. `lilac`
finds `lib/lilac` relative to its own executable, so the folder can live
anywhere as long as it stays together.

### Building the install folder from source

You need Rust and an LLVM 18.1.8 development tree, with
`LLVM_SYS_181_PREFIX` set to that tree. Then run:

```bash
scripts/package.sh <out-dir> <llvm-dir>
```

The script builds `lilac` in release mode and assembles the folder above in
`<out-dir>`.

## Building a program

```bash
lilac build hello.lilac
```

`lilac build` compiles, links and writes an executable, printing its
progress as it goes. It takes a single file, or a project folder (every
`.lilac` file in it is one namespace). With no argument it builds the
current folder. The program needs a `main` function.

The executable goes in a `build` folder next to the project (or in the
current folder, for a single file), with one subfolder per mode:

```
build/debug/hello(.exe)
build/release/hello(.exe)
build/<triple>/debug/hello(.exe)    when cross-compiling
```

Common flags:

| Flag | What it does |
| --- | --- |
| `--release` | Optimized build (`--mode=release`); `--mode=release-small` optimizes for size. |
| `-o <path>` | Write the executable to `<path>` instead of the build folder. |
| `--target=<triple>` | Cross-compile, for example `aarch64-apple-darwin` or `x86_64-unknown-linux-gnu`. |
| `--lib=<Name>=<dir>` | Mount a library folder as namespace `Name`. |
| `--link-arg=<arg>` | Pass an argument to the linker, for example a library to link. |
| `--libc` | Link through a C compiler and the C library (see below). |
| `-q`, `--timings` | Print nothing, or print how long each stage took. |

To use the standard library, mount it from the install folder:

```bash
lilac build hello.lilac --lib=Standard=<install>/lib/Standard
```

Programs build for Windows, Linux and macOS, on x86-64 and 64-bit Arm, from
any of those machines.

### Native builds and `--libc`

By default a program talks to the operating system directly
(`kernel32` on Windows, system calls on Linux, `libSystem` on macOS), so it
does not link a C library. Shared C libraries still work: declare their
functions with `extern` and pass the library with `--link-arg=`.

`--libc` links through a C compiler (`cc` or `clang` on `PATH`, or
`--linker=<path>`) with the C library instead. Use it to link static C
libraries, or code that still uses `Standard.C`.
