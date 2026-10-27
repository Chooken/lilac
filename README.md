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
bin/lilac-lsp(.exe)    the language server, for editors
lib/lilac/             the linker and system stubs it uses
lib/Standard/          the standard library
templates/project/     what `lilac init` copies into a new project
```

Unzip the folder anywhere and add its `bin` folder to your `PATH`. `lilac`
finds `lib/lilac` relative to its own executable, so the folder can live
anywhere as long as it stays together.

### Building the install folder from source

You need Rust and an LLVM 18.1.8 development tree, with
`LLVM_SYS_181_PREFIX` set to that tree (in the environment or
`.cargo/config.toml`). Then run:

```bash
scripts/package.sh
```

The script builds `lilac` and `lilac-lsp` in release mode and assembles the
folder above in `package/`.

### The VS Code extension

With Node.js and npm installed, run:

```bash
scripts/package-vscode.sh
```

It packages `editors/vscode` as `package/lilac-<version>.vsix`. Install it
with `code --install-extension package/lilac-<version>.vsix`; it starts
`lilac-lsp` from your `PATH`.

## Starting a project

```bash
lilac init hello
cd hello
```

`lilac init` creates the folder if needed and copies the project
template into it:

```
hello/lilac.toml        every setting with its default, and the standard library
hello/src/main.lilac    a program printing "Hello, world!"
```

Then `lilac build` builds `build/debug/hello`. See
[Projects](#projects-lilactoml) for the settings.

`lilac init` never changes a `lilac.toml` that is already there, and keeps
any other file it would have written. The template is the install
folder's `templates/project/` (or `$LILAC_TEMPLATE_DIR/project/`): edit
it to change what every new project starts with. In its files,
`{{name}}` becomes the project's name, `{{host}}` this machine's triple,
and a line holding `{{libraries}}` one line per bundled library.

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
| `--target=<triple>` | Cross-compile, for example `aarch64-apple-darwin` or `x86_64-unknown-linux-gnu`; `host` is this machine. |
| `--lib=<Name>=<dir>` | Mount a library folder as namespace `Name`. |
| `--link-arg=<arg>` | Pass an argument to the linker, for example a library to link. |
| `--libc[=on\|off]` | Link through a C compiler and the C library (see below). |
| `--stack-traces=on\|off` | Print a stack trace when the program panics; on by default in debug builds. |
| `-q`, `--timings` | Print nothing, or print how long each stage took. |

A project folder records its settings, and the libraries it uses, in a
`lilac.toml` (see [Projects](#projects-lilactoml)), so none of this needs
retyping. A single file mounts the standard library with a flag:

```bash
lilac build hello.lilac --lib=Standard=<install>/lib/Standard
```

Programs build for Windows, Linux and macOS, on x86-64 and 64-bit Arm, from
any of those machines.

### Panics

`@panic("message")` stops the program. It prints the message and where each
active function was, innermost first, to stderr, and exits with code 101:

```
panic: index 3 is out of range
  at check (main.lilac:13:9)
  at middle (main.lilac:18:5)
  at main (main.lilac:23:5)
```

Debug builds record the trace; release builds print only the panic's own
location. `--stack-traces=on|off` overrides either default.

### Native builds and `--libc`

By default a program talks to the operating system directly
(`kernel32` on Windows, system calls on Linux, `libSystem` on macOS), so it
does not link a C library. Shared C libraries still work: declare their
functions with `extern` and pass the library with `--link-arg=`.

`--libc` links through a C compiler (`cc` or `clang` on `PATH`, or
`--linker=<path>`) with the C library instead. Use it to link static C
libraries, or code that still uses `Standard.C`.

## Projects: `lilac.toml`

A folder with a `lilac.toml` is a project. `lilac build`, `check`,
`emit-ir` and `emit-llvm` read it when given the folder, or when run in it
with no argument, so they all build and check the same program:

```toml
[project]
name = "hello"
root = "src"
usings = ["Standard", "Standard.Io as Io"]

[build]
mode = "release"
target = "host"
stack-traces = true
dir = "build"

[libraries]
Standard = { bundled = true }
Util = "vendor/Util"
Json = { path = "../json/lib" }

[link]
libc = true
linker = "clang"
args = ["-lm", "vendor/libgreet.a"]
```

| Setting | Value | Default | Flag |
| --- | --- | --- | --- |
| `project.name` | letters, digits, `-`, `_`, `.` | the folder's name | none |
| `project.root` | the folder holding the code | `"."` | none |
| `project.usings` | `using`s every project file has: `"A.B"` or `"A.B as Name"` | `[]` | none |
| `build.mode` | `"debug"`, `"release"`, `"release-small"` | `"debug"` | `--mode=`, `--release` |
| `build.target` | a triple, or `"host"` | `"host"` | `--target=` |
| `build.stack-traces` | `true` or `false` | on in debug builds only | `--stack-traces=` |
| `build.dir` | where builds go | `"build"` | `--build-dir=` |
| `libraries.<Name>` | `"folder"`, `{ path = "folder" }` or `{ bundled = true }` | none | `--lib=Name=dir` |
| `link.libc` | `true` or `false` | `false` | `--libc[=on\|off]` |
| `link.linker` | a program on `PATH`, or a path | `LILAC_LINKER`, else `cc` or `clang` | `--linker=` |
| `link.args` | an array of strings | `[]` | `--link-arg=` |

- **Flags win, for one run.** A flag beats the manifest, and the manifest
  beats the default. `--lib=Name=dir` replaces the manifest's `Name`,
  `--link-arg=`s come after `link.args`, and the linker is `--linker=`,
  then `LILAC_LINKER`, then `link.linker`.
- **Paths** in the manifest are relative to its folder. In a double-quoted
  TOML string `\` starts an escape, so write Windows paths with `/` or in
  single quotes (`'C:\libs\Util'`). The linker runs in the manifest's
  folder, so `link.args` paths are relative to it too.
- **Global `using`s.** Each of `project.usings` is in every file of the
  project, after the file's own `using`s; a mounted library's files do not
  get them. A file's own declarations and aliases come first: a global
  alias yields to a name the file declares or reaches.
- **Bundled libraries.** `Standard = { bundled = true }` mounts the
  `Standard` installed with Lilac (`lib/Standard`, or the folder
  `LILAC_LIB_DIR` names), so the manifest holds no path on your machine,
  and the library always matches the compiler.
- A file argument reads no manifest, and nothing searches parent folders.
  A folder below the project's root with its own `lilac.toml` is another
  project and is not loaded.
- A mistake in the manifest is an error (`E075` for TOML Lilac does not
  read, `E076` for an unknown setting or a wrong value), and nothing
  builds until it is fixed.

## Testing a program

A `test` block is a named piece of code that `lilac test` runs; it passes
when it returns and fails when it panics. `Standard.Testing` has the
expectations:

```lilac
using Standard
using Standard.Testing

test "adding two numbers" {
    x: Int32 = 1
    expect_equal[Int32](x + 2, 3)
}
```

```bash
lilac test --filter=adding
```

`lilac test [<file-or-dir>]` builds the program's tests (no `main`
needed; `lilac build` leaves them out) with `lilac build`'s options, runs
each in its own process, in parallel, and prints each result, the output
of every failure and a summary; it exits 1 when one fails. `--filter=TEXT`
(repeatable) runs only the tests whose name or namespace contains a
`TEXT`. Tests of mounted libraries are not run. `Standard`'s own tests
are `tests/standard`.

## Checking a program

`lilac check [<file-or-dir>]` reports errors without building, and takes the
same `-q`, `--timings` and `--lib=` flags. Every error has a code; `lilac
explain <code>` prints its page:

```bash
lilac explain E042
```
