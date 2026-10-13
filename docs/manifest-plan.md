# Manifest Plan

> Status: proposal, 2026-10-13. The decisions below are proposed and none
> is agreed yet.
>
> Goal: one file, `lilac.toml`, says how a project builds. `lilac build`,
> `lilac check`, `lilac emit-*` and the language server all read it, so
> the editor checks the program the build builds and nobody retypes
> flags. `lilac init` writes a default one.
>
> Decisions:
> - **`lilac.toml` replaces `lilac.json`.** The CLI reads it, which
>   reverses the build plan's rule that configuration comes only from
>   flags (`docs/history/build-command-plan.md`). The language server
>   reads it instead of `lilac.json`, and nothing reads `lilac.json`
>   afterwards. A Zig-style build program can still come later, on top
>   of these settings.
> - **Flags win, for one run.** A flag beats the manifest, and the
>   manifest beats the default. Every build setting has a key, except the
>   ones that describe one run rather than the project (see the table).
> - **No manifest, no change.** A file argument never reads a manifest. A
>   folder without one checks, emits and builds exactly as it does today,
>   so `tests/ui` and `tests/run` pass unchanged.
> - **The folder you name is the project.** `lilac build [<dir>]` reads
>   `<dir>/lilac.toml`. Nothing searches parent folders.
> - **Our own TOML reader.** A small std-only reader lives in a new
>   `lilac-manifest` crate, and the compiler crates take no `toml` or
>   `serde` dependency. Every key and value keeps its span, so manifest
>   errors print like compiler errors, with did-you-mean fixes.
> - **Unknown keys are errors** (`E076`, with a did-you-mean). A mistyped
>   setting that is silently ignored builds the wrong program.
> - **Bundled libraries are named, not pathed.** `Standard = { bundled =
>   true }` mounts the `Standard` folder installed beside the compiler.
>   A manifest then holds no machine-specific path, and the library
>   always matches the compiler's version. The compiler names no
>   library: any folder in the install's `lib/` except the sysroot
>   `lilac` can be bundled.
> - **`lilac init` copies a template folder from the install.** Its
>   `lilac.toml` lists every setting with its default and its flag and
>   mounts every bundled library, beside a hello-world `src/main.lilac`.
>   `init` never overwrites a file.

## Where things stand

Read from the code on 2026-10-13:

1. **Every build setting is a flag, every time.** A program that uses
   `Standard` needs `--lib=Standard=<install>/lib/Standard` on every
   `build` and `check`, as the README says. One that links a C library
   also needs `--libc`, `--linker=` and its `--link-arg=`s. Nothing
   records them for the next person or for CI.
2. **The editor checks a different program from the one that builds.**
   `compile::new_session` sets only the libraries. The editor therefore
   compiles for `Target::default()` (`x86_64-unknown-none`), with
   `Entry::CMain` (which sets `@builtin.Target.libc` to 1), in debug.
   `lilac build` compiles for the host, with `Entry::Native`, in the mode
   it is given. The two decide top-level conditional items and every
   `@builtin.Target` and `@builtin.Build` fact differently. `lilac check`
   uses the editor's defaults.
3. **`lilac.json` fails silently.** `roots::config` returns `None` for a
   file that does not parse, and an unknown key is ignored. A `root` that
   is not a folder falls back to the workspace folder
   (`roots::project_root`). None of this is reported.
4. **The CLI does not know a project's layout.** Take a project whose
   code is in `src/` (`"root": "src"` for the editor). It builds with
   `lilac build src`, so the program is named `src`
   (`build::program_name` takes the folder's name) and lands in
   `src/build/` (`build::layout`).
5. **Link arguments are relative to wherever `lilac build` runs.** The
   linker inherits the working folder (`link::run` sets none), so a
   project's `--link-arg=libgreet.so` works only from the project's own
   folder.
6. **The build folder is a name, not a setting.** The loader skips a
   folder named `build` under the root (`Loader::reach_children`). So no
   namespace can be called `build`, while a `--build-dir` inside the root
   is walked like any other folder.
7. **There is no way to start a project** except writing the folder by
   hand and remembering the flags.

## The manifest

```toml
[project]
name = "hello"
root = "src"

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

| Key | Value | Default | Flag | Read by |
| --- | --- | --- | --- | --- |
| `project.name` | letters, digits, `-`, `_`, `.` | the manifest folder's name, with any other character turned into `-` | none | `build` |
| `project.root` | a folder | `"."` | none: the argument names the manifest's folder | all |
| `build.mode` | `"debug"`, `"release"`, `"release-small"` | `"debug"` | `--mode=`, `--release` | all |
| `build.target` | a triple, or `"host"` | `"host"` | `--target=`, which now also takes `host` | all |
| `build.stack-traces` | a boolean | on in `debug` only | `--stack-traces=on\|off` | `build` |
| `build.dir` | a folder | `"build"` | `--build-dir=` | `build`; every load skips it |
| `libraries.<Name>` | `"folder"`, `{ path = "folder" }` or `{ bundled = true }` | none | `--lib=Name=dir` | all |
| `link.libc` | a boolean | `false` | `--libc[=on\|off]` | all, since it sets `@builtin.Target.libc` |
| `link.linker` | a program on `PATH`, or a path | `LILAC_LINKER`, else `cc` or `clang` | `--linker=` | `build` |
| `link.args` | an array of strings | `[]` | `--link-arg=`, appended | `build` |

"All" means `build`, `check`, `emit-ir`, `emit-llvm` and the editor.

Some flags stay flags only, because they describe one run, chosen by
whoever runs it (a terminal, CI, an editor), and not the project:

- `-o <path>`. One output path for every mode and target would make those
  builds overwrite each other; `build.dir` places them instead.
- `--dump-lir=` and `--check`, which dump IR and compare snapshots.
- `--verbose`, `--error-format=`, `--color=`, `-q` and `--timings`, which
  control how the run prints.

### Paths

- A path in the manifest is relative to the manifest's folder; an
  absolute path also works. A path in a flag is relative to the working
  folder, as today.
- `\` starts an escape in a double-quoted TOML string. Write a Windows
  path with `/`, or in single quotes (`'C:\libs\Util'`).
- A `link.linker` with no path separator is a program looked up on
  `PATH`, like `cc`. Anything else is a path.
- In a manifest build the linker runs in the manifest's folder. Every
  path Lilac passes it is made absolute: the object file, the
  executable, and the sysroot's `lld` and stubs. A relative link
  argument, from `link.args` or from `--link-arg=`, is then relative to
  the manifest. When `lilac build` runs in the project's folder, which
  is the usual case, the two folders are the same.

### Precedence

A flag beats the manifest, and the manifest beats the default, one
setting at a time:

- `--lib=Name=dir` replaces the manifest's `Name` and adds any other name.
- `--link-arg=`s come after `link.args`.
- The `--libc` linker is `--linker=`, then `LILAC_LINKER`, then
  `link.linker`, then `cc` or `clang` on `PATH`. The environment beats
  the file, as in Cargo, so CI and the run tests can pin a linker.
- The editor's `lilac.libraries` setting acts like `--lib`.

### Discovery

- A folder argument reads `<folder>/lilac.toml` when it exists. `build`
  already defaults to `.`, and `check` and `emit-*` will too. The
  project is then `project.root` under that folder, and the build folder
  is `build.dir` beside the manifest.
- A file argument is single-file mode, as today. No manifest is read,
  even one beside the file.
- Nothing searches parent folders. `lilac build src` builds `src` as a
  plain folder even when `./lilac.toml` names `src` as its root.
- Below a project's root, a folder that holds its own `lilac.toml` is
  another project and is not loaded. Today `lilac.json` marks one.

### What a manifest changes

With a manifest, `check` and `emit-*` check and emit what `build` would
build. They use the manifest's target (the host by default), its mode
and its `link.libc` (so `Entry::Native` unless it is set). Stack traces
stay off unless `--stack-traces=on` is given, so dumps stay readable.

Without a manifest, each command keeps today's defaults: `x86_64-unknown-none`
and `Entry::CMain` for `check` and `emit-ir`, and the host for
`emit-llvm` and `build`.

### Errors

There are two new codes, each with its page in `docs/errors/`:

- `E075`: `lilac.toml` is not TOML that Lilac reads. This covers a
  syntax error, a key or table defined twice, and a TOML feature no
  setting takes (numbers, dates, multi-line strings, arrays of tables).
- `E076`: a setting is unknown or has a wrong value. An unknown section,
  key or mode comes with a did-you-mean fix.

```
error[E076]: unknown setting `stack_traces` in `[build]`
 --> lilac.toml:9:1
  |
9 | stack_traces = true
  | ^^^^^^^^^^^^
help: did you mean `stack-traces`?
```

## Ground rules

- Every phase ends with the `AGENTS.md` gates green, and is one commit.
- No phase changes a `tests/ui` snapshot or a `tests/run` expectation,
  because of "no manifest, no change". New behaviour gets new tests.
- The compiler crates stay std-only. `lilac-manifest` adds no external
  dependency.
- Every error the manifest can produce is tested, with its message and
  span.
- `SPEC.md`, `AGENTS.md`, `README.md` and the VS Code docs are updated in
  the phase that changes what they describe.

Order: 1, 2 and 3, then 4 and 5 in either order. Phase 5 relies on
Phase 3, which makes `lilac.toml` the marker for another project.

## Phase 1: A TOML reader

- A new crate, `crates/lilac-manifest`, is std-only and for now depends
  only on `lilac-span` and `lilac-error`.
- Its `toml.rs` provides `read(file, text, diags)`, which returns a
  `Table`: the keys in source order, each with the span of the key and
  of its value. A `Value` is a string, a boolean, an array or a table.
  Nothing after this phase sees TOML text.
- It reads TOML 1.0, minus what no setting takes:
  - comments, blank lines, LF and CRLF line endings, and a byte-order mark
    at the start;
  - bare, `"quoted"` and `'literal'` keys, and dotted keys (`a.b = …`);
  - table headers (`[build]`, `[a.b]`), with TOML's rules: no key or
    table is defined twice, and an inline table or array is closed once
    it is written;
  - basic strings with TOML's escapes (`\"`, `\\`, `\b`, `\t`, `\n`,
    `\f`, `\r`, `\uXXXX`, `\UXXXXXXXX`);
  - literal strings, which have no escapes, so `'C:\libs'` works;
  - `true` and `false`;
  - arrays, which can span lines and allow a trailing comma and
    comments inside;
  - inline tables.
- Everything else is `E075` at its span:
  - an integer, float, date or time ("no setting takes a number");
  - a multi-line string;
  - an array of tables (`[[…]]`);
  - an invalid escape;
  - a string that its line ends inside;
  - a stray character;
  - a key with no value;
  - a second definition, with a secondary label at the first.

  The reader recovers at the next line, so several mistakes are reported
  together. It returns no table when it finds any.
- Arrays and inline tables nest at most 32 deep (`E075`), which bounds
  the recursion as the parser bounds its own.
- `E075` and its page.
- Tests:
  - each accepted form;
  - each `E075`, with its message and span;
  - CRLF line endings and the byte-order mark;
  - redefinitions through dotted keys and headers (`a.b = "x"`, then
    `[a]` with `b = "y"`);
  - a random-input test, like `lilac-lex/tests/progress.rs`, showing the
    reader never panics and always finishes.

## Phase 2: The settings

- `lilac-manifest` gains the schema (`settings.rs`) and dependencies on
  `lilac-driver`, `lilac-module` and `lilac-target`. `MANIFEST_FILE`
  (`"lilac.toml"`) lives in `lilac-module`, because the loader needs it,
  and `lilac-manifest` re-exports it.
- `Manifest::read(dir, fs, sources, diags)` returns `None` when `dir`
  has no `lilac.toml`. Otherwise it:
  - reads the file through `lilac_module::Fs`, with no I/O of its own
    (the CLI passes `StdFs`, the server `OverlayFs`);
  - adds the file to the `SourceMap`, so its diagnostics render with
    snippets;
  - reads each setting.

  A `Manifest` holds every setting it was given, with the span of each
  value.
- The keys are one table, `SETTINGS`. Each entry has the section, the
  key, its kind, its default, its flag and a one-line description. The
  table is used in three places:
  - the reader checks keys against it. An unknown section or key is
    `E076`, with a did-you-mean among that section's keys
    (`Diagnostic::suggest`, on the key alone);
  - `lilac init`'s template states every key in it (Phase 4, checked
    by a test);
  - the README lists it.
- `E076` and its page cover a wrong value:
  - a value of the wrong type ("`build.mode` takes a string");
  - an unknown mode, with a did-you-mean;
  - a triple `Target::parse` refuses, with its message;
  - an invalid `project.name`;
  - a library name that is not an identifier or that starts with `@`,
    as `--lib` refuses today;
  - a library entry with both or neither of `path` and `bundled`, or
    with `bundled = false`;
  - a library folder or `root` folder that does not exist. This is
    reported at the value, not as the loader's spanless `E025`.
- Bundled libraries:
  - `bundled_dir(env, exe_dir)` is pure, like `link::find_sysroot`. It
    returns `LILAC_LIB_DIR`, else `<exe dir>/../lib`. `bin/lilac` and
    `bin/lilac-lsp` both find the same folder.
  - `Name = { bundled = true }` mounts `<bundled dir>/Name`.
  - `bundled_libraries` lists the identifier-named folders in the
    bundled folder, except `lilac`, for `lilac init`.
- `Settings::resolve(manifest, overrides, host)` applies the flags over
  the manifest, and the manifest over the defaults. `Overrides` holds
  each flag in the table as an `Option`. `resolve` also makes the
  manifest's paths absolute against its folder.
- `Settings::apply(&mut Session)` sets `libraries`, `target`,
  `optimize`, `entry` and the new `Session::build_dir`. The CLI and the
  language server both go through it, so they cannot drift.
- `Session::build_dir` is the folder the loader skips under the root,
  in place of the name `build`. `load_with_status` takes it. When it is
  unset the loader skips `<root>/build`, as today. Nothing sets it yet,
  so this phase changes no behaviour.
- Tests:
  - each key's values, and each `E076` with its span and fix;
  - the defaults for an empty manifest;
  - every flag overriding its key;
  - `--lib` merging, and the order of `--link-arg`s;
  - the linker order, including `LILAC_LINKER`;
  - relative and absolute paths;
  - finding bundled libraries through `LILAC_LIB_DIR` and through
    `<exe>/../lib`, and a missing one;
  - the loader skipping a `build_dir` with another name, and then
    loading a namespace called `build`.

## Phase 3: The CLI reads `lilac.toml`

- `build`, `check`, `emit-ir` and `emit-llvm` read the manifest of the
  folder they are given, or of `.` when given nothing (now for all four
  commands), with `Manifest::read`. They resolve their flags over it with
  `Settings::resolve`, and the flag parsers fill `Overrides` instead of
  the session. Without a manifest every command keeps today's defaults
  exactly (problems 1 and 2).
- New flag forms:
  - `--target=host`;
  - `--libc=on|off`, where a bare `--libc` still means `on`. It becomes
    a common flag, because it sets `@builtin.Target.libc`.

  `FLAGS`, `usage()` and the README's flag table change to match.
- In `build`:
  - the program's name is `project.name`;
  - the project is `project.root`;
  - the build folder is `build.dir`. `layout` takes the resolved folder,
    and `-o` still wins (problem 4);
  - in a manifest build the linker runs in the manifest's folder, with
    Lilac's own paths absolute. `link::run` takes the folder
    (problem 5).
- The loader skips the resolved build folder (problem 6).
- A manifest with errors is reported through the session: human or
  JSON, coloured, and counted in the summary. The command then exits
  with 1 before anything compiles.
- The loader marks another project by `lilac.toml`
  (`Loader::is_other_project`). From this phase on, `lilac.json` marks
  nothing.
- Docs:
  - The README gets a "Projects: `lilac.toml`" section: the example,
    the settings table, precedence, paths and bundled libraries. Its
    advice to mount `Standard` from the install folder points to the
    manifest instead.
  - `AGENTS.md` "Build and link": the manifest replaces "the CLI takes
    all build configuration as flags". Add a `lilac-manifest` paragraph,
    and update the loader paragraphs.
  - `SPEC.md`: the sentence about nested projects.
  - `docs/errors/E025.md`: the vendored-library example becomes
    `[libraries] Util = "vendor/Util"`.
- Tests, in `crates/lilac-cli/tests/manifest.rs`, using `tests/support`:
  - the manifest's name, root, mode, target and build folder place the
    executable (`build/<mode>/<name>`, `build/<triple>/<mode>/<name>`),
    and each flag overrides its key;
  - `check` reads the manifest. A top-level item that exists only `if
    @builtin.Target.os == .Linux` checks clean under `target =
    "x86_64-unknown-linux-gnu"`, and fails without the manifest;
  - an unknown key prints `E076` at `lilac.toml:<line>:<col>` with its
    did-you-mean, and exits 1 without compiling. A syntax error prints
    `E075`, and `--error-format=json` carries both;
  - a file argument ignores a `lilac.toml` beside it;
  - a nested folder with its own `lilac.toml` is not loaded into the
    outer project;
  - on Linux, `link.args = ["libgreet.so"]` links from another working
    folder (following `native_linux_links_shared_libraries`).
- A new run test, `tests/run/manifest_project/`:
  - its `lilac.toml` sets `mode = "release"` and mounts `Util = "util"`;
  - the program prints through `Util`, and prints `release` when
    `@builtin.Build.mode` is `.Release`.

  The harness passes a folder by its bare name, so the manifest is read.
  The harness's own flags (`--lib=Standard`, `--build-dir`, `-o`) merge
  over it.

## Phase 4: `lilac init`

- `lilac init [<dir>]` defaults to `.`. It creates `<dir>` if it is
  missing and copies the project template into it. It never touches an
  existing `lilac.toml`: that is an error, with exit 1, before anything
  is written. Any other file the template would write that is already
  there is kept, with a note.
- The template is a folder in the install folder,
  `templates/project/` (`LILAC_TEMPLATE_DIR` overrides the
  `templates/` folder, like `LILAC_LIB_DIR`), so it can be edited
  without rebuilding `lilac`. The repository's `templates/` is what
  `scripts/package.sh` copies. It holds:
  - `lilac.toml`: every setting under a comment saying what it does
    and naming its flag, set to its default or commented out when its
    default is computed (`stack-traces`, `linker`), and `root = "src"`;
  - `src/main.lilac`: a hello world through `Standard.Io`.
- Placeholders, in any template file: `{{name}}` is the folder's name,
  made valid; `{{host}}` is this machine's triple; the line holding
  `{{libraries}}` becomes one `Name = { bundled = true }` per bundled
  library found (`bundled_libraries`), or a commented example when
  there is none, and `init` says where it looked. The template's
  `lilac.toml` is valid TOML as it is (the placeholders sit in a string
  and in comments).
- When the folder has a `lilac.json`, `init` adds a note: the editor
  reads `lilac.toml` now, so the `root` and `libraries` from
  `lilac.json` belong in the new file.
- `init` prints one status line (``Created `hello` in hello
  (lilac.toml, src/main.lilac)``), and `-q` silences it. `COMMANDS` and
  the usage text list `init`.
- The README gets a "Starting a project" section: `lilac init`, then
  `lilac build`.
- Tests:
  - the repository's template, filled in, reads back with no
    diagnostic and resolves to the same settings as an empty manifest
    but for its root, so it states the true defaults;
  - it mentions every key in `SETTINGS` with its flag;
  - running `init` twice fails and leaves the files alone; a file
    already there is kept;
  - with `LILAC_LIB_DIR` pointing at a folder holding `Standard/` and
    `lilac/`, `init` mounts only `Standard`;
  - without the template, `init` fails and says where to point it;
  - what `init` makes builds and prints `Hello, world!` with the
    run-test sysroot.

The template's `lilac.toml`, filled in on a Windows machine with
`Standard` installed:

```toml
# How `lilac build` builds this project, and what the editor checks.
# Each setting shows its default. A flag overrides it for one run;
# `lilac help` lists them.

[project]
# The program's name: the executable's file name. By default, the
# folder's name.
name = "hello"
# The folder holding the project's code, relative to this file.
root = "src"

[build]
# "debug", "release" or "release-small" (--mode=, --release).
mode = "debug"
# A target triple, or "host" for this machine (--target=). This machine
# is x86_64-pc-windows-msvc.
target = "host"
# Record panic stack traces (--stack-traces=). By default, in debug
# builds only.
# stack-traces = true
# Where builds go, relative to this file (--build-dir=).
dir = "build"

[libraries]
# Name = "folder", or Name = { bundled = true } for a library installed
# with Lilac (--lib=Name=dir).
Standard = { bundled = true }

[link]
# Link through a C compiler and the C library (--libc).
libc = false
# The C compiler that links when libc is on (--linker=). By default
# LILAC_LINKER, else cc or clang.
# linker = "clang"
# More linker arguments (--link-arg=). Relative paths are relative to
# this file.
args = []
```

## Phase 5: The language server reads `lilac.toml`

- `configure` reads the workspace's `lilac.toml` with `Manifest::read`
  and `Settings::resolve`, passing the `lilac.libraries` setting as the
  `--lib` overrides. `roots::config`, `config_libraries` and
  `project_root` go, along with the server's last reading of a JSON file
  (`serde_json` stays, for the protocol).
- Every compile applies the settings: the project's root, libraries,
  target, mode, libc and build folder (problem 2). `Job` carries them,
  `new_session` calls `Settings::apply`, and `require_entry` stays off.
  Without a manifest the session is the same as today.
- Manifest diagnostics are published on `lilac.toml` together with the
  compile's spanless project diagnostics (`project_anchor` is
  `lilac.toml` when it exists). They go in one publish, so neither
  clears the other. They convert like any diagnostic (`convert`), using
  a line index of the manifest's text.
- An `E075` keeps the last good settings, or none. An `E076` drops only
  the setting it reports (problem 3).
- The watcher `**/lilac.toml` replaces `**/lilac.json`. A change
  re-reads the settings and recompiles. The server reads the manifest
  from disk, so edits apply when the file is saved.
- A workspace with a `lilac.json` and no `lilac.toml` gets one
  `window/showMessage` warning: `lilac.json` is no longer read, so move
  its settings into `lilac.toml` (`lilac init` writes one).
- The repository's `lilac.json` becomes `lilac.toml`, with
  `[libraries] Standard = "lib/Standard"`.
- VS Code:
  - add `workspaceContains:lilac.toml` activation, so manifest errors
    show before any `.lilac` file opens;
  - update the `lilac.libraries` description;
  - rewrite the README's project-settings section, and drop "restart
    the server after changing either", which the watcher already made
    stale;
  - add a CHANGELOG entry.
- `AGENTS.md`: the language-server section (compile units, the anchor,
  the watchers) and the libraries bullet. `docs/compiler-review-plan.md`:
  its `lilac.json` decision gets a pointer to this plan.
- Tests:
  - `spanless_errors_go_to_lilac_json`, the watcher assertion,
    `a_vendored_library_from_lilac_json_checks_clean` and
    `definition_uses_the_lilac_json_root` move to `lilac.toml`;
  - the JSON tests in `roots` go;
  - new: an unknown key is published on `lilac.toml` at its range, with
    its did-you-mean;
  - new: a syntax error keeps the last libraries;
  - new: the manifest's target reaches the compile (Phase 3's
    Linux-only item checks clean);
  - new: saving a change to `lilac.toml` recompiles;
  - new: the `lilac.json` message is shown once.

## Out of scope (later, if wanted)

- A Zig-style build program. The manifest would stay its declarative
  part.
- Searching parent folders for `lilac.toml`, and `--manifest-path`.
- Profiles and per-target settings (`[mode.release]`,
  `[target.<triple>]`).
- Projects without a `main` (a static library or object file as the
  output).
- Several targets in one build.
- Workspaces of several projects, and in the editor a manifest per
  sub-project.
- Package dependencies (versions, registries, git). `[libraries]` takes
  folders and bundled libraries only.
- `lilac.toml` as an editor document: errors while typing, quick fixes,
  key completion and hovers, and a schema for TOML extensions.
- `lilac init` writing a `.gitignore`, or `--template=<name>` choosing
  another folder of `templates/`.
- Numbers, dates, multi-line strings and arrays of tables in the reader,
  once a setting takes one.
