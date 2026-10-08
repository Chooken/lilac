# Lilac `@panic` and Stack Traces Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `@panic(msg)`. It writes `panic: <msg>`, the panic site and a
stack trace of Lilac frames to stderr, then exits with code 101. It works the
same on every target, in native and `--libc` modes, with no debug info, no
unwinder and no symbolizer.

**Architecture:** Stack traces come from a **shadow stack** that one LIR pass
inserts. Native unwinding is not used.

- Each instrumented function keeps a `#.lilac.frame { prev, site }` record in
  its own stack frame. When the function starts, the record is linked onto a
  global list head. Before each call, the record's `site` is set to a
  preformatted string constant such as `"  at A.foo (src/a.lilac:12:5)\n"`.
  Before every `return`, the record is unlinked.
- `@panic` lowers to a call to the LIR runtime function `.lilac.panic(msg,
  site)`. That function writes the message, walks the list writing each
  `site`, and exits with code 101.
- All text is formatted at compile time, so the runtime only copies bytes.
- The only new backend leaf is `@trace_head`, which returns the address of
  one pointer-sized global.

**Why not native stack traces:** a native trace needs three
platform-specific subsystems:

- debug info (CodeView/PDB on Windows, DWARF elsewhere);
- a runtime unwinder;
- a symbolizer (DbgHelp, a DWARF parser, or `atos`).

That conflicts with native mode, which links no C library. Walking frame
pointers is per-architecture, breaks on foreign frames, and gives no line
numbers. The shadow stack gives exact `file:line:col` everywhere, at the cost
of a few stores per call. Those stores are paid only when traces are on.

**Tech stack:** a Rust workspace. No new dependencies, and `inkwell` stays
confined to `lilac-codegen-llvm`.

## Decisions

1. A panic exits with code **101**, not a trap.
2. `lilac build` defaults traces **on in `--mode=debug`** and off in
   `release` and `release-small`. `--stack-traces=on|off` overrides the
   default. (Changed while implementing: `check`, `emit-ir` and `emit-llvm`
   leave traces off unless `--stack-traces=on`, so IR dumps and the `.lir`/
   `.ll` snapshot tests stay unchanged. `cmd_build` applies the mode default
   from `Common.stack_traces`.)
3. **Lowering writes the panic site directly** as a `str` constant. There is
   no placeholder for a later pass to fill in, so every LIR module is
   complete as soon as it is lowered.

## Global Constraints

- `unsafe_code = "forbid"` and `clippy::pedantic = deny` stay in force. No
  `unwrap`/`expect`/`panic!` on user input in production code; tests may use
  them.
- Panic is an abort: there is no unwinding, drops do not run, and
  `.lilac.panic` never returns. Output goes through `.lilac.write`, which is
  unbuffered, so everything printed before the panic is kept.
- **One formatter per piece of text.** `SourceMap::location` turns a span into
  `name:line:col`. `lilac_ir::trace::site_text` turns a function name and a
  location into an `"  at …\n"` line. Lowering (for the panic site) and the
  trace pass (for call sites) both use these two functions.
- **Locations are display paths.** Each file is named by its namespace folder
  path with `/` separators, as in `Checks/checks.lilac` or
  `Standard/Io/io.lilac`. A root-namespace file keeps its bare name, or in
  single-file mode the path it was given as. Output is therefore identical on
  every OS.
- **Function names are source names.** Generic arguments are written out, as
  in `Holder[@bit32].make`, not the mangled `Holder.1..bit32.make`. Lowering
  records `symbol → name`.
- `Session.stack_traces` defaults to `false`. Every existing LIR and LLVM
  test and snapshot stays byte-identical, and the LSP is unaffected.
- The trace pass runs **after fold and DCE**. Folding never sees its stores,
  and only live functions pay for them. It instruments only non-`extern`
  functions whose names do not start with `.lilac.`.
- Compiler-made names start with `.`: the `#.lilac.frame` layout, the
  `%.lilac.frame`/`%.lilac.head` places, the `&.lilac.tN` registers, the
  `!.lilac.site.N` and `!.lilac.panic.*` data, and the `.lilac.panic` and
  `.lilac.trace_head` symbols. User identifiers cannot contain `.`, so none of
  these can collide with user code.

## Output Format

```
panic: index 3 is out of range
  at check (panic.lilac:13:9)
  at middle (panic.lilac:18:5)
  at main (panic.lilac:23:5)
```

The first `at` line is the `@panic` site, and each following line is the call
a caller is waiting on. With traces off, the output is the panic site
followed by `  (stack trace disabled; build with --stack-traces=on)`. After
128 frames the trace ends with `  ...`.

## Review Focus

- **Every `return` unlinks its frame.** If one does not, the head points at a
  dead stack slot. Pinned by the Task 4 unit test (two returns) and the Task 6
  `panic_after_return` run test.
- **The frame's allocas stay static.** The backend emits each `alloca` where
  it appears, so the prologue must go *after* the entry block's leading
  allocas and must allocate its own two places there. Pinned by the Task 4
  layout test.
- **Instrumented modules verify.** Pinned by the Task 4 `verify` test.
- **Nothing changes when traces are off.** Pinned by the Task 5 driver test.

## File Map

| File | Change |
| --- | --- |
| `crates/lilac-span/src/lib.rs` | `SourceMap::location` |
| `crates/lilac-module/src/lib.rs` | register files under display names |
| `crates/lilac-ir/src/ty.rs` | `LirTy::trace_frame` |
| `crates/lilac-ir/src/lib.rs` | `#.lilac.frame` builtin layout, `@trace_head` signature, `pub mod trace`, re-exports, IR version |
| `crates/lilac-ir/src/runtime.rs` | `panic_runtime` (`.lilac.panic` plus its data) |
| `crates/lilac-ir/src/trace.rs` (new) | `site_text`, `instrument` |
| `crates/lilac-codegen-llvm/src/{lower.rs,runtime.rs}` | `@trace_head` leaf, `#.lilac.frame` struct |
| `crates/lilac-hir/src/intrinsics.rs` | `Intrinsic::Panic` |
| `crates/lilac-lower/src/lib.rs` (+ `tests.rs`) | `Lowered`, `display_of`, `@panic` lowering, a `&SourceMap` parameter |
| `crates/lilac-driver/src/lib.rs` | inject the panic runtime, `Session.stack_traces`, run the pass |
| `crates/lilac-cli/src/main.rs` | `--stack-traces=on|off` |
| `crates/lilac-cli/tests/run.rs` | run from `tests/run`; `expect-stderr:` and `build-arg:` directives |
| `tests/run/panic*.lilac`, `tests/run/panic_namespace/` | new run tests |
| `crates/lilac-ir/src/{lib.rs,codec.rs,dce.rs}`, `crates/lilac-const/src/lib.rs`, `crates/lilac-codegen-llvm/src/{lib.rs,lower.rs}` | remove the unused `LirTerm::Panic` (Task 7) |
| `AGENTS.md`, `README.md` | docs |

---

### Task 0: Display paths in the source map

**Files:** `crates/lilac-span/src/lib.rs`, `crates/lilac-module/src/lib.rs`.

**Produces:** `SourceMap::location(span) -> Option<String>`. Source-map file
names become display paths, which also changes what diagnostics print for
files outside the root namespace, for the better.

- [ ] **Step 1: Add `SourceMap::location`** (`lilac-span`, in `impl SourceMap`, after `line_col`):

```rust
    /// `name:line:col` (1-based) for the start of a span, as diagnostics and
    /// stack traces print it; `None` for a dummy span or an unknown file.
    #[must_use]
    pub fn location(&self, span: Span) -> Option<String> {
        if span.is_dummy() {
            return None;
        }
        let file = self.file(span.file)?;
        let (line, col) = file.line_col(span.lo.0 as usize);
        Some(format!("{}:{line}:{col}", file.name))
    }
```

- [ ] **Step 2: Test it** (in `lilac-span`'s test module; create
  `#[cfg(test)] mod tests` if there is none):

```rust
    #[test]
    fn location_is_name_line_col() {
        let mut sources = SourceMap::new();
        let file = sources.add_file("A/b.lilac", "x\n  yz\n");
        assert_eq!(
            sources.location(Span::new(4, 5, file)).as_deref(),
            Some("A/b.lilac:2:3")
        );
        assert_eq!(sources.location(Span::dummy()), None);
    }
```

- [ ] **Step 3: Register files under display names** (`lilac-module`). In
  `Loader::add_file`, replace `let file_id = sources.add_file(&name, &src);`
  with:

```rust
        let display = display_name(&self.namespaces[ns].path, &name);
        let file_id = sources.add_file(&display, &src);
```

  Add the following next to `file_name_str`. `FileUnit.name` stays the bare
  file name, so only the source map changes.

```rust
/// A file's name in diagnostics and stack traces: its namespace's folder path
/// from the root, `/`-separated, then the file name (`Std/Heap/alloc.lilac`
/// for namespace `Std.Heap`, a mounted library's included), so it reads the
/// same on every OS. A root-namespace file keeps `name`: its bare file name,
/// or in single-file mode the path it was given as.
fn display_name(ns_path: &str, name: &str) -> String {
    if ns_path.is_empty() {
        name.to_owned()
    } else {
        format!("{}/{name}", ns_path.replace('.', "/"))
    }
}
```

- [ ] **Step 4: Test it** (in the `lilac-module` tests, next to
  `file_unit_carries_read_path`):

```rust
    /// Source-map names are namespace paths, so same-named files in two
    /// folders stay distinguishable in diagnostics and stack traces.
    #[test]
    fn source_names_are_namespace_paths() {
        let root = PathBuf::from("proj");
        let lib = PathBuf::from("libs").join("std");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "using util\nusing Std.Heap\n");
        fs.add(&root, "util/helper.lilac", "helper: function () nothing {}\n");
        fs.add(&lib, "Heap/alloc.lilac", "alloc: function () nothing {}\n");

        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let libraries = [("Std".to_owned(), lib.clone())];
        let (project, failed) =
            super::load_with_status(&root, &libraries, &mut fs, &mut sources, &mut diags);
        assert!(!failed, "{diags:?}");
        let mut names: Vec<&str> = project
            .files
            .iter()
            .map(|unit| sources.file(unit.file).expect("registered").name.as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["Std/Heap/alloc.lilac", "main.lilac", "util/helper.lilac"]);
    }
```

- [ ] **Step 5: Run the gates.** `cargo test --workspace` will flag any
  assertion that pinned a non-root file's bare name in a rendered diagnostic.
  Update each to the display path; that is the intended change. Assertions
  that use `contains("x.lilac:N")` keep passing.
- [ ] **Step 6: Commit:** `module: name source files by namespace path`.

### Task 1: LIR support (`#.lilac.frame` and `@trace_head`)

**Files:** `crates/lilac-ir/src/{ty.rs,lib.rs}`, new
`crates/lilac-ir/src/trace.rs` (formatter only, for now),
`crates/lilac-codegen-llvm/src/{lower.rs,runtime.rs,lib.rs}`.

**Produces:** `LirTy::trace_frame()`; the builtin layout `#.lilac.frame { prev:
ref #.lilac.frame, site: #byte_buffer }`; the builtin
`@trace_head() -> ref ref #.lilac.frame`; and
`lilac_ir::trace::site_text`.

- [ ] **Step 1: Add `LirTy::trace_frame`** (`ty.rs`, after `format_args`):

```rust
    /// `#.lilac.frame`: one shadow-stack record (`prev`, `site`; see
    /// [`crate::trace`]). The leading `.` keeps it apart from user types.
    #[must_use]
    pub fn trace_frame() -> Self {
        Self::Named(".lilac.frame".to_owned())
    }
```

- [ ] **Step 2: Add the builtin layout** (`lib.rs`, `builtin_layout`). Add an
  arm, and mention `#.lilac.frame` in the doc comment:

```rust
        ".lilac.frame" => Some(vec![
            ("prev".to_owned(), LirTy::trace_frame().to_ref()),
            ("site".to_owned(), LirTy::byte_buffer()),
        ]),
```

- [ ] **Step 3: Add the builtin signature** (`lib.rs`, `builtin_sig`):

```rust
        "@trace_head" => (vec![], vec![LirTy::trace_frame().to_ref().to_ref()]),
```

- [ ] **Step 4: Bump `LILAC_IR_VERSION`** to `"0.4"`. These are normative IR
  additions. Update any test that pins the `lilac_ir = "0.3"` header.
- [ ] **Step 5: Create `trace.rs` with the shared line formatter.** Declare
  `pub mod trace;` in `lib.rs`.

```rust
//! Shadow-stack stack traces: the call-site text, and (Task 4) the pass
//! that records it.

/// One stack-trace line, newline included: `"  at main (main.lilac:3:5)\n"`,
/// or `"  at main\n"` for a compiler-generated call with no location. The
/// panic site (lowering) and every call site (the pass) are formatted here.
#[must_use]
pub fn site_text(func: &str, location: Option<&str>) -> String {
    match location {
        Some(location) => format!("  at {func} ({location})\n"),
        None => format!("  at {func}\n"),
    }
}
```

- [ ] **Step 6: Build the frame struct in the backend.** In
  `define_layouts` (`lower.rs`), change the builtin loop to
  `for ty in [LirTy::byte_buffer(), LirTy::format_args(), LirTy::trace_frame()]`.
  `byte_buffer` must still come first, and the frame's fields are a `ptr` and
  a `#byte_buffer`. Update the doc comment.
- [ ] **Step 7: Emit the `@trace_head` leaf.**
  - In `lower.rs`, add `"@trace_head" => ".lilac.trace_head",` to
    `builtin_symbol`.
  - Add `pub(crate) fn module(&self) -> &Module<'ctx> { &self.module }` next to
    `context()`.
  - In `runtime.rs`, emit the leaf from `emit_runtime` with
    `if used.contains("@trace_head") { trace_head(lower); }`, and add
    `use inkwell::module::Linkage;` plus:

```rust
/// `@trace_head() -> ref ref #.lilac.frame`: the address of the shadow-stack
/// head (`lilac_ir::trace`), a zeroed pointer-sized internal global.
fn trace_head(lower: &Lower<'_, '_>) {
    let ptr = lower.ptr_type();
    let slot = lower.module().add_global(ptr, None, ".lilac.trace_head.slot");
    slot.set_linkage(Linkage::Internal);
    slot.set_initializer(&ptr.const_null());
    let function = lower.runtime_fn(".lilac.trace_head", ptr.fn_type(&[], false));
    let builder = entry(lower, function);
    checked(
        builder.build_return(Some(&slot.as_pointer_value())),
        "return trace head",
    );
}
```

  Also extend the module doc comment's list of leaves.

- [ ] **Step 8: Test the backend** (`lilac-codegen-llvm/src/lib.rs` tests,
  next to `lilac_abort_builtin_calls_runtime_definition`):

```rust
    /// `@trace_head` resolves to the emitted `.lilac.trace_head`, which
    /// returns a zeroed internal pointer global.
    #[test]
    fn trace_head_builtin_returns_the_slot() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        let mut insts = vec![LirInst::Alloca {
            ty: LirTy::trace_frame().to_ref().to_ref(),
            place: "head".to_owned(),
            span: span(),
        }];
        call(
            &mut insts,
            LirCallee::Builtin("@trace_head".to_owned()),
            &[],
            &["head"],
        );
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = emit(&module);
        assert!(
            text.contains("@.lilac.trace_head.slot = internal global ptr null"),
            "{text}"
        );
        assert!(text.contains("define ptr @.lilac.trace_head()"), "{text}");
        assert!(text.contains("call ptr @.lilac.trace_head()"), "{text}");
    }
```

- [ ] **Step 9: Run the gates and commit:** `ir: shadow-stack frame layout and @trace_head leaf`.

### Task 2: The panic runtime (`.lilac.panic`)

**Files:** `crates/lilac-ir/src/{runtime.rs,lib.rs}`, `crates/lilac-driver/src/lib.rs`.

**Produces:** `lilac_ir::panic_runtime() -> (LirFunc, Vec<LirData>)`, which
the driver injects alongside `build_runtime`.

`.lilac.panic` is a separate function from `build_runtime`, for two reasons:

- The runtime tests and backend tests take `build_runtime`'s functions
  without any data. If `.lilac.panic` were among them, it would reference
  data those tests lack.
- DCE drops `.lilac.panic` and its data from any program that never panics.

- [ ] **Step 1: Add the constants and the function** (`runtime.rs`, after
  `entry_point`). Extend the module doc comment's list to include it.

```rust
/// Data written around a panic message.
const PANIC_HEAD: &str = ".lilac.panic.head";
const PANIC_NEWLINE: &str = ".lilac.panic.newline";
const PANIC_ELIDED: &str = ".lilac.panic.elided";
const PANIC_NO_TRACE: &str = ".lilac.panic.no_trace";
/// Frames written before a trace is cut short (deep recursion).
const PANIC_MAX_FRAMES: &str = "0x80";
/// A panicking process's exit code (101, Rust's convention).
const PANIC_EXIT_CODE: &str = "0x65";

/// The panic handler `.lilac.panic(#byte_buffer %msg, #byte_buffer %site)`
/// and the data it writes: `panic: <msg>`, then the stack trace, then
/// `.lilac.exit(101)`. It never returns.
///
/// The trace walks the shadow stack `@trace_head` points at (see
/// [`crate::trace`]), writing each frame's `site` line, innermost first, and
/// `  ...` after 128 frames. With no frame pushed (a build without stack
/// traces) it writes the panic's own `site` and a hint instead. Every line
/// already ends in a newline.
#[must_use]
pub fn panic_runtime() -> (LirFunc, Vec<LirData>) {
    let data = [
        (PANIC_HEAD, "panic: "),
        (PANIC_NEWLINE, "\n"),
        (PANIC_ELIDED, "  ...\n"),
        (PANIC_NO_TRACE, "  (stack trace disabled; build with --stack-traces=on)\n"),
    ]
    .into_iter()
    .map(|(name, text)| LirData {
        name: name.to_owned(),
        bytes: text.as_bytes().to_vec(),
        span: span(),
    })
    .collect();
    (panic_handler(), data)
}

fn panic_handler() -> LirFunc {
    let frame_ref = LirTy::trace_frame().to_ref();
    let mut entry = vec![
        alloca(LirTy::byte_ref(), "data"),
        alloca(LirTy::BitNative, "len"),
        alloca(LirTy::byte_buffer(), "text"),
        alloca(frame_ref.clone().to_ref(), "head"),
        alloca(frame_ref.clone(), "frame"),
        alloca(LirTy::BitNative, "count"),
    ];
    entry.extend(const_place("fd", "0x2", LirTy::Bit(32)));
    entry.extend(write_data(PANIC_HEAD, "a"));
    entry.extend(write_buffer("msg", "b"));
    entry.extend(write_data(PANIC_NEWLINE, "c"));
    entry.extend([
        call_builtin("@trace_head", &[], &["head"]),
        load("head", "top", frame_ref.clone()),
        copy_place(LirOperand::Reg("top".to_owned()), "frame"),
    ]);
    entry.extend(is_null("top", "untraced"));

    let mut untraced = write_buffer("site", "a");
    untraced.extend(write_data(PANIC_NO_TRACE, "b"));

    let mut walk = vec![copy_reg(
        LirOperand::Place("frame".to_owned()),
        "cur",
        frame_ref.clone(),
    )];
    walk.extend(is_null("cur", "done"));

    let check = vec![
        copy_reg(LirOperand::Place("count".to_owned()), "n", LirTy::BitNative),
        constant("max", PANIC_MAX_FRAMES, LirTy::BitNative),
        arith(ArithOp::Eq, "n", "max", "full", LirTy::Bit(8)),
    ];

    let mut print = vec![
        field("frame", "site", 1, "site", LirTy::byte_buffer()),
        copy_place(LirOperand::Reg("site".to_owned()), "text"),
    ];
    print.extend(write_buffer("text", "a"));
    print.extend([
        field("frame", "prev", 0, "prev", frame_ref),
        copy_place(LirOperand::Reg("prev".to_owned()), "frame"),
        copy_reg(LirOperand::Place("count".to_owned()), "n", LirTy::BitNative),
        constant("one", "0x1", LirTy::BitNative),
        arith(ArithOp::IAdd, "n", "one", "next", LirTy::BitNative),
        copy_place(LirOperand::Reg("next".to_owned()), "count"),
    ]);

    let mut exit = const_place("code", PANIC_EXIT_CODE, LirTy::Bit(32)).to_vec();
    exit.push(call(".lilac.exit", &["code"], &[]));

    lir_func(
        ".lilac.panic",
        &[("msg", LirTy::byte_buffer()), ("site", LirTy::byte_buffer())],
        &[],
        vec![
            block("entry", entry, branch_if("untraced", "untraced", "walk")),
            block("untraced", untraced, branch("exit")),
            block("walk", walk, branch_if("done", "exit", "check")),
            block("check", check, branch_if("full", "elided", "print")),
            block("print", print, branch("walk")),
            block("elided", write_data(PANIC_ELIDED, "a"), branch("exit")),
            block("exit", exit, unreachable()),
        ],
    )
}

/// Writes the `#byte_buffer` place `buffer` to `%fd` through the reused
/// `%data`/`%len` places; `tag` keeps the registers unique within a block.
fn write_buffer(buffer: &str, tag: &str) -> Vec<LirInst> {
    let (data, len) = (format!("{tag}.data"), format!("{tag}.len"));
    vec![
        field(buffer, "data", 1, &data, LirTy::byte_ref()),
        copy_place(LirOperand::Reg(data), "data"),
        field(buffer, "len", 0, &len, LirTy::BitNative),
        copy_place(LirOperand::Reg(len), "len"),
        call(".lilac.write", &["fd", "data", "len"], &[]),
    ]
}

/// Writes the data constant `name` through `%text`.
fn write_data(name: &str, tag: &str) -> Vec<LirInst> {
    let reg = format!("{tag}.text");
    let mut insts = vec![
        constant(&reg, &format!("!{name}"), LirTy::byte_buffer()),
        copy_place(LirOperand::Reg(reg), "text"),
    ];
    insts.extend(write_buffer("text", tag));
    insts
}

/// `&out = (&ptr as address) == 0`.
fn is_null(ptr: &str, out: &str) -> [LirInst; 3] {
    let (address, zero) = (format!("{out}.address"), format!("{out}.zero"));
    [
        convert(ConvertOp::Bitcast, ptr, &address, LirTy::BitNative),
        constant(&zero, "0x0", LirTy::BitNative),
        arith(ArithOp::Eq, &address, &zero, out, LirTy::Bit(8)),
    ]
}
```

  Add `LirData` to the `use crate::{…}` list. Re-export from `lib.rs` with
  `pub use runtime::{build_runtime, entry_point, panic_runtime, Entry, Syscalls, System};`.
  If `ArithOp::Eq`'s verifier rule wants a different result type than `#bit8`,
  match what `ILtU` uses in `format_args_push`.

- [ ] **Step 2: Test that it verifies** (`runtime.rs` tests):

```rust
    /// The panic handler verifies on top of every OS layer.
    #[test]
    fn panic_handler_verifies() {
        for system in [System::Windows, System::Libc, System::Linux(Syscalls::X86_64), System::None] {
            let mut module = LirModule::new("x86_64-pc-windows-msvc");
            module.funcs = build_runtime(8, system, Entry::Native);
            let (handler, data) = super::panic_runtime();
            module.funcs.push(handler);
            module.data = data;
            let mut diags = lilac_error::Diags::new();
            assert!(verify(&module, &mut diags).is_ok(), "{system:?}: {:?}", diags.diagnostics());
        }
    }
```

- [ ] **Step 3: Inject it** (driver, `inject_runtime`, after the `for func in
  runtime` loop):

```rust
        let (panic, data) = lilac_ir::panic_runtime();
        lir.funcs.push(panic);
        lir.data.extend(data);
```

  Extend the doc comment. Programs that never panic are unchanged, because
  `prune_unreachable` drops the handler and its data. A module with no `main`
  and no `--export` is not pruned, and it already carries the whole runtime.
- [ ] **Step 4: Test the backend lowers it.** Build the Step 2 module for
  `x86_64-pc-windows-msvc`, emit it with `emit`, and assert that the text
  contains `define void @.lilac.panic(` and `@.lilac.panic.head = private`.
- [ ] **Step 5: Run the gates and commit:** `runtime: .lilac.panic handler`.

### Task 3: `@panic` intrinsic

**Files:** `crates/lilac-hir/src/intrinsics.rs`,
`crates/lilac-lower/src/{lib.rs,tests.rs}`, `crates/lilac-driver/src/lib.rs`.

**Produces:**
- `Intrinsic::Panic`;
- `lilac_lower::lower(hir, res, types, sources, triple, ptr_bytes) -> Lowered`
  with `Lowered { module, names }`;
- `@panic(msg)` lowers to `call @.lilac.panic (in msg, site)` followed by
  `unreachable`.

- [ ] **Step 1: Add the intrinsic.** In `intrinsics.rs`:
  - add the variant `/// \`@panic(message)\`: ends the program (see \`.lilac.panic\`). Panic,`;
  - add `"@panic" => Self::Panic,` to `lookup`;
  - add `Self::Panic => sig(vec![Builtin("@byte_buffer")], None),` to
    `signature`;
  - add `assert_eq!(Intrinsic::lookup("@panic"), Some(Intrinsic::Panic));` to
    `lookup_parses_families_and_widths`.

  Typeck needs no change: the generic intrinsic path checks the one
  `@byte_buffer` argument.
- [ ] **Step 2: Record source names in lowering.** Add `sources: &'a
  SourceMap` and `names: HashMap<String, String>` to `Lower`, and initialize
  both in `lower` and in `check_extern_signatures`. The latter has no source
  map, so give `check_extern_signatures` an empty `SourceMap` local. Next to
  `symbol_of`, add:

```rust
    /// A function's name as written in source, with generic arguments:
    /// `Holder[@bit32].make` for the symbol `Holder.1..bit32.make`. Stack
    /// traces print it.
    fn display_of(&self, func: &HirFunc) -> String {
        let def = self.res.def(func.key.def);
        let generics = |args: &[TypeRef]| {
            if args.is_empty() {
                String::new()
            } else {
                let args: Vec<String> =
                    args.iter().map(|arg| self.types.display_ref(*arg)).collect();
                format!("[{}]", args.join(", "))
            }
        };
        let (owner, own) = match def.parent {
            Some(parent) => {
                let count = self.res.def(parent).generic_params.len();
                let owner = self.res.qualified_name(parent);
                (
                    format!("{owner}{}", generics(&func.key.args[..count])),
                    &func.key.args[count..],
                )
            }
            None if matches!(def.kind, DefKind::Hook(_)) => (String::new(), &func.key.args[..]),
            None => (
                self.res.namespace_path(def.ns).to_owned(),
                &func.key.args[..],
            ),
        };
        let name = if owner.is_empty() {
            def.name.clone()
        } else {
            format!("{owner}.{}", def.name)
        };
        format!("{name}{}", generics(own))
    }
```

  In the symbol loop in `lower`, record `lower.names.insert(symbol.clone(),
  lower.display_of(func));` for each function with a body.
- [ ] **Step 3: Return `Lowered`:**

```rust
/// Lowered LIR plus each function's source name.
pub struct Lowered {
    /// The module.
    pub module: LirModule,
    /// LIR symbol → name as written in source (`Holder[@bit32].make`);
    /// stack traces print these.
    pub names: HashMap<String, String>,
}
```

  Add the parameter `sources: &SourceMap` after `types`, and end `lower` with
  `Lowered { module: lower.module, names: lower.names }`. Update the callers:
  - `crates/lilac-lower/src/tests.rs`: `let mut lir = lower(&checked.hir,
    &res, &types, &sources, "x86_64-unknown-none", 8).module;`
  - `Session::run` in the driver: `let lowered = lilac_lower::lower(&checked.hir,
    &resolved, &types, &self.source_map, &triple, ptr_bytes); let mut lir =
    lowered.module;`. Keep `lowered.names` for Task 5.
- [ ] **Step 4: Lower `@panic`.** In `lower_intrinsic`, add an arm before
  `_ =>`:

```rust
            Intrinsic::Panic => {
                let msg = self.arg(fs, func, &args[0]);
                let location = self.sources.location(span);
                let site = lilac_ir::trace::site_text(&self.display_of(func), location.as_deref());
                let name = self.intern_data(site.as_bytes());
                let reg = fs.reg();
                fs.emit(LirInst::Const {
                    dst: reg.clone(),
                    lit: format!("!{name}"),
                    ty: LirTy::byte_buffer(),
                    span,
                });
                let site = self.fresh_place(fs, LirOperand::Reg(reg), args[0].ty, span);
                fs.emit(LirInst::Call {
                    func: LirCallee::Func(".lilac.panic".to_owned()),
                    ins: vec![msg, site],
                    outs: Vec::new(),
                    span,
                });
                fs.end(LirTerm::Unreachable { span });
                LirOperand::Reg(String::new())
            }
```

  `args[0].ty` is `@byte_buffer` because the signature requires it.
  Statements after `@panic` go into a fresh block with no predecessors, as
  they already do after `return` (`Fs::emit` reopens one).
- [ ] **Step 5: Add driver tests** (`lilac-driver` tests; `compile_lir`
  injects the runtime, so the result verifies):

```rust
    /// `@panic` calls the handler with its own site, then ends the block.
    #[test]
    fn panic_lowers_to_the_handler_with_its_site() {
        let mut session = Session::new();
        let module = session
            .compile_lir("p.lilac", "main: function () nothing {\n    @panic(\"stop\")\n}\n")
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
        let text = module.to_text();
        assert!(text.contains("call @.lilac.panic"), "{text}");
        assert!(text.contains("unreachable"), "{text}");
        assert!(
            module.data.iter().any(|datum| datum.bytes == b"  at main (p.lilac:2:5)\n"),
            "{text}"
        );
    }

    /// The handler and its data are pruned from a program that never panics.
    #[test]
    fn no_panic_no_handler() {
        let mut session = Session::new();
        let module = session
            .compile_lir("p.lilac", "main: function () nothing {\n}\n")
            .expect("compiles");
        assert!(module.funcs.iter().all(|func| func.name != ".lilac.panic"));
        assert!(module.data.is_empty());
    }
```

  Also add a typeck test that `@panic(0x1)` is an error (assert
  `diags.has_errors()`, following the intrinsic tests in
  `crates/lilac-typeck/src/tests.rs`).
- [ ] **Step 6: Run the gates and commit:** `lang: @panic intrinsic`.

### Task 4: The instrumentation pass

**Files:** `crates/lilac-ir/src/trace.rs`, plus a re-export in `lib.rs`.

**Produces:** `lilac_ir::trace::instrument(module, sources, names)`.

- [ ] **Step 1: Write the pass** (append to `trace.rs`; extend the module doc):

```rust
use std::collections::HashMap;

use lilac_span::{SourceMap, Span};

use crate::{LirCallee, LirData, LirFunc, LirInst, LirModule, LirOperand, LirTerm, LirTy};

/// The frame record place and the place holding the head slot's address.
const FRAME: &str = ".lilac.frame";
const HEAD: &str = ".lilac.head";

/// Instruments every user function with a shadow-stack frame.
///
/// - **Prologue** (entry block, after its leading `alloca`s, so every
///   `alloca` stays static): the frame and head places, `call @trace_head`,
///   `frame.prev ← *head`, `*head ← addr frame`.
/// - **Before each call** (not to a leaf builtin, which cannot panic):
///   `frame.site ← !site`, the call's [`site_text`] as a data constant,
///   deduplicated.
/// - **Before each `return`**: `*head ← frame.prev`.
///
/// `.lilac.*` runtime functions and `extern`s are skipped. `names` maps a
/// LIR symbol to its source name (`lilac_lower::Lowered::names`); a missing
/// one prints as the symbol. Run it after DCE.
pub fn instrument(module: &mut LirModule, sources: &SourceMap, names: &HashMap<String, String>) {
    let mut sites = Sites::new(&module.data);
    for func in &mut module.funcs {
        if func.is_extern || func.name.starts_with(".lilac.") || func.blocks.is_empty() {
            continue;
        }
        let name = names.get(&func.name).cloned().unwrap_or_else(|| func.name.clone());
        instrument_func(func, &name, sources, &mut sites);
    }
    module.data.extend(sites.added);
}

/// Site data constants: existing ones are reused by content, new ones are
/// named `.lilac.site.N`.
struct Sites {
    by_bytes: HashMap<Vec<u8>, String>,
    added: Vec<LirData>,
}

impl Sites {
    fn new(data: &[LirData]) -> Self {
        let by_bytes = data
            .iter()
            .map(|datum| (datum.bytes.clone(), datum.name.clone()))
            .collect();
        Self { by_bytes, added: Vec::new() }
    }

    fn intern(&mut self, text: String) -> String {
        let bytes = text.into_bytes();
        if let Some(name) = self.by_bytes.get(&bytes) {
            return name.clone();
        }
        let name = format!(".lilac.site.{}", self.added.len());
        self.by_bytes.insert(bytes.clone(), name.clone());
        self.added.push(LirData { name: name.clone(), bytes, span: Span::dummy() });
        name
    }
}

fn instrument_func(func: &mut LirFunc, name: &str, sources: &SourceMap, sites: &mut Sites) {
    let frame_ref = LirTy::trace_frame().to_ref();
    let mut regs = 0_u32;
    let mut reg = || {
        regs += 1;
        format!(".lilac.t{regs}")
    };
    for (index, block) in func.blocks.iter_mut().enumerate() {
        let span = block.span;
        let mut old = std::mem::take(&mut block.insts).into_iter().peekable();
        let mut insts = Vec::new();
        if index == 0 {
            while let Some(alloca) = old.next_if(|inst| matches!(inst, LirInst::Alloca { .. })) {
                insts.push(alloca);
            }
            let (top, me) = (reg(), reg());
            insts.extend([
                LirInst::Alloca { ty: LirTy::trace_frame(), place: FRAME.to_owned(), span },
                LirInst::Alloca { ty: frame_ref.clone().to_ref(), place: HEAD.to_owned(), span },
                LirInst::Call {
                    func: LirCallee::Builtin("@trace_head".to_owned()),
                    ins: Vec::new(),
                    outs: vec![HEAD.to_owned()],
                    span,
                },
                LirInst::Load { src: HEAD.to_owned(), dst: top.clone(), ty: frame_ref.clone(), span },
                LirInst::StoreField {
                    value: LirOperand::Reg(top),
                    obj: FRAME.to_owned(),
                    name: "prev".to_owned(),
                    idx: 0,
                    span,
                },
                LirInst::Addr { place: FRAME.to_owned(), dst: me.clone(), ty: frame_ref.clone(), span },
                LirInst::Store { src: LirOperand::Reg(me), dst: HEAD.to_owned(), span },
            ]);
        }
        for inst in old {
            if let LirInst::Call { func: callee, span: call_span, .. } = &inst {
                if !matches!(callee, LirCallee::Builtin(_)) {
                    let location = sources.location(*call_span);
                    let data = sites.intern(site_text(name, location.as_deref()));
                    let site = reg();
                    insts.push(LirInst::Const {
                        dst: site.clone(),
                        lit: format!("!{data}"),
                        ty: LirTy::byte_buffer(),
                        span: *call_span,
                    });
                    insts.push(LirInst::StoreField {
                        value: LirOperand::Reg(site),
                        obj: FRAME.to_owned(),
                        name: "site".to_owned(),
                        idx: 1,
                        span: *call_span,
                    });
                }
            }
            insts.push(inst);
        }
        if matches!(block.term, LirTerm::Return { .. }) {
            let prev = reg();
            insts.extend([
                LirInst::Field {
                    obj: FRAME.to_owned(),
                    name: "prev".to_owned(),
                    idx: 0,
                    dst: prev.clone(),
                    ty: frame_ref.clone(),
                    span,
                },
                LirInst::Store { src: LirOperand::Reg(prev), dst: HEAD.to_owned(), span },
            ]);
        }
        block.insts = insts;
    }
}
```

  Use the actual field names of each `LirInst` variant as declared in
  `lib.rs`; the ones above were read from it. Then run `cargo fmt`.
  Nothing branches to the entry block (the backend's entry block is LIR
  block 0, and LLVM forbids branches into the entry block), so the prologue
  runs once per call.
- [ ] **Step 2: Write the unit tests** (`trace.rs` `#[cfg(test)] mod tests`).
  Build by hand a module containing:
  - `f`: an entry block of `alloca #bit32 %x`, then `call @g`, then a
    `branch.if` to `a` or `b`; blocks `a` and `b` each end in `return`;
  - `g`: a single `return`;
  - `.lilac.x`: a function whose single block contains a call;
  - an `extern` named `e`.

  Give the calls real spans from a `SourceMap` file named `m.lilac`. Call
  `instrument(&mut module, &sources, &HashMap::from([("f".into(), "F".into())]))`,
  then assert:
  - `f`'s entry block starts with `alloca %x`, then the two frame allocas,
    then `call @trace_head` (the allocas stay static);
  - the instruction before `call @g` stores `!.lilac.site.0`, whose bytes
    are `"  at F (m.lilac:L:C)\n"` (it uses the source name);
  - both `a` and `b` end with `field … .prev` then `store … %.lilac.head`
    (every return unlinks);
  - `.lilac.x` and `e` are unchanged;
  - two calls at the same span share one data constant;
  - `crate::verify(&module, …)` succeeds.
- [ ] **Step 3: Run the gates and commit:** `ir: shadow-stack instrumentation pass`.

### Task 5: Driver and CLI wiring

**Files:** `crates/lilac-driver/src/lib.rs`, `crates/lilac-cli/src/main.rs`.

- [ ] **Step 1: Add `Session.stack_traces`.** Add
  `pub stack_traces: bool` with the doc `/// Whether to instrument functions
  for panic stack traces (\`lilac_ir::trace\`); the CLI sets it from the mode
  and \`--stack-traces\`.`. Set it to `false` in both `new` and `Default`.
- [ ] **Step 2: Run the pass.** In `Session::run`, right after
  `prune_unreachable` and its `report`:

```rust
        if self.stack_traces {
            lilac_ir::trace::instrument(&mut lir, &self.source_map, &lowered.names);
        }
```

  Extend the pipeline doc comment on `run` (`… → prune → trace → …`) and the
  pipeline line in `AGENTS.md`.
- [ ] **Step 3: Add driver tests:**

```rust
    /// Stack traces off (the default) adds no frame; on, `main` links one.
    #[test]
    fn stack_traces_instrument_only_when_on() {
        let src = "main: function () nothing {\n    @panic(\"stop\")\n}\n";
        let mut off = Session::new();
        let text = off.compile_lir("p.lilac", src).expect("compiles").to_text();
        assert!(!text.contains(".lilac.frame"), "{text}");
        let mut on = Session::new();
        on.stack_traces = true;
        let text = on.compile_lir("p.lilac", src).expect("compiles").to_text();
        assert!(text.contains("call @trace_head"), "{text}");
        assert!(text.contains("%.lilac.frame"), "{text}");
    }
```

- [ ] **Step 4: Add the CLI flag.** In `parse_common`, declare `let mut
  stack_traces: Option<bool> = None;` and add an arm:

```rust
        } else if let Some(value) = arg.strip_prefix("--stack-traces=") {
            stack_traces = Some(parse_on_off("--stack-traces", value)?);
```

  After the loop, add
  `session.stack_traces = stack_traces.unwrap_or(session.optimize == OptimizeMode::Debug);`.
  Then add:

```rust
/// `on`/`off` for a switch flag.
fn parse_on_off(flag: &str, value: &str) -> Result<bool, ExitCode> {
    match value {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => {
            eprintln!("unknown {flag}={value}; want on|off");
            Err(ExitCode::from(2))
        }
    }
}
```

  If clippy's `too_many_lines` trips on `parse_common`, move the
  `--cache-dir=`/`--dump-lir=` arms into a helper, the way
  `parse_session_flag` was split out. Add `[--stack-traces=on|off]` to every
  command in `usage()`.
- [ ] **Step 5: Test the CLI flag.** Following the existing `parse_common`
  tests in `main.rs`, check that:
  - the default is on in debug;
  - `--release` defaults to off;
  - `--release --stack-traces=on` is on;
  - `--stack-traces=maybe` is rejected.
- [ ] **Step 6: Run the gates and commit:** `cli: --stack-traces, on in debug builds`.

### Task 6: Run tests

**Files:** `crates/lilac-cli/tests/run.rs`, `tests/run/panic.lilac`,
`tests/run/panic_after_return.lilac`, `tests/run/panic_generic.lilac`,
`tests/run/panic_no_trace.lilac`, `tests/run/panic_namespace/{main.lilac,Checks/checks.lilac}`.

- [ ] **Step 1: Harness changes.**
  1. Build and check with `.current_dir(run_dir())`, passing
     `source.file_name()` instead of the absolute path. Single-file display
     names then stay `panic.lilac` on every machine.
  2. Add `stderr: Vec<String>` and `build_args: Vec<String>` to `Expect`.
     `// expect-stderr: <line>` is repeatable, parsed like `expect-stdout`;
     `// build-arg: <flag>` is repeatable and is passed to `lilac build`.
  3. When `stderr` is non-empty, compare the program's stderr (with `\r\n`
     normalized) to `stderr.join("\n") + "\n"`. When it is empty, ignore
     stderr, so existing programs are unaffected.
  4. Document both directives in the header comment.
- [ ] **Step 2: Add the run tests.** The run tests build in debug, so traces
  are on. Columns are those of each call expression's first character. If a
  first run shows a different convention for method calls (for example
  `Holder[@bit32].make(…)` starting at `.make`), the implementation is still
  correct: fix the expectation and note the convention in the commit
  message.

`tests/run/panic.lilac` checks the full trace, that stdout before the panic
is kept, and the exit code:

```
// expect-stdout: before
// expect-exit: 101
// expect-stderr: panic: index 3 is out of range
// expect-stderr:   at check (panic.lilac:13:9)
// expect-stderr:   at middle (panic.lilac:18:5)
// expect-stderr:   at main (panic.lilac:23:5)
// lib: Standard=lib/Standard

using Standard.Io

check: function (index: @bit32) nothing {
    if @i_greater_than_or_equal_u_32(index, 0x3) {
        @panic("index 3 is out of range")
    }
}

middle: function () nothing {
    check(0x3)
}

main: function () nothing {
    print_line("before")
    middle()
}
```

`tests/run/panic_after_return.lilac` checks that frames which returned, by
either `return` path, are gone:

```
// expect-exit: 101
// expect-stderr: panic: late
// expect-stderr:   at main (panic_after_return.lilac:17:5)

// Recursion through two return paths: an early `return` at zero, and the
// fall-through end of the body.
deep: function (n: @bit32) nothing {
    if @i_less_than_u_32(n, 0x1) {
        return
    }
    deep(@i_sub_32(n, 0x1))
}

main: function () nothing {
    deep(0x5)
    deep(0x0)
    @panic("late")
}
```

`tests/run/panic_generic.lilac` checks that a generic instance shows its
source name:

```
// expect-exit: 101
// expect-stderr: panic: no value
// expect-stderr:   at Holder[@bit32].make (panic_generic.lilac:10:9)
// expect-stderr:   at main (panic_generic.lilac:15:25)

Holder[T]: object {
    value: T

    make: function (v: T) out: Holder[T] {
        @panic("no value")
    }
}

main: function () nothing {
    h: Holder[@bit32] = Holder[@bit32].make(0x3)
}
```

`tests/run/panic_no_trace.lilac` checks the output with traces off:

```
// expect-exit: 101
// build-arg: --stack-traces=off
// expect-stderr: panic: stop
// expect-stderr:   at main (panic_no_trace.lilac:8:5)
// expect-stderr:   (stack trace disabled; build with --stack-traces=on)

main: function () nothing {
    @panic("stop")
}
```

`tests/run/panic_namespace/main.lilac` checks namespace display paths and
qualified names:

```
// expect-exit: 101
// expect-stderr: panic: requirement failed
// expect-stderr:   at Checks.require (Checks/checks.lilac:4:9)
// expect-stderr:   at main (main.lilac:9:5)

using Checks

main: function () nothing {
    require(0x0)
}
```

`tests/run/panic_namespace/Checks/checks.lilac`:

```
// Panics when `ok` is zero.
require: function (ok: @bit32) nothing {
    if @i_less_than_u_32(ok, 0x1) {
        @panic("requirement failed")
    }
}
```

- [ ] **Step 3: Check the frame cap by hand, once.** Write a scratch program
  that recurses 200 deep and then panics. Confirm the output is exactly 128
  `at` lines followed by `  ...`, and exit code 101. This is not committed:
  pinning 129 stderr lines would make an unreadable test.
- [ ] **Step 4: Run the gates on Windows.** CI covers Linux and macOS
  natively. Commit: `tests: @panic and stack-trace run tests`.

### Task 7: Remove `LirTerm::Panic`

Nothing produces it any more, and keeping two panic paths invites drift.

- [ ] **Step 1:** Delete the variant and every arm that matches it:
  - `crates/lilac-ir/src/lib.rs`: the enum, the printer (`panic !{msg}`),
    `term_span`, and the verifier's undeclared-const check;
  - `dce.rs`: the data retention, its test `panic_message_retains_data`, and
    the doc mention;
  - `crates/lilac-const/src/lib.rs`: the three match arms;
  - `crates/lilac-codegen-llvm/src/lower.rs`: `used_builtins` (and its doc)
    and the terminator lowering;
  - `crates/lilac-codegen-llvm/src/lib.rs`: the tests near lines 1084 and
    1333.

  `@lilac_abort` stays.
- [ ] **Step 2:** In `codec.rs`, drop the tag-3 encode and decode arms. Leave
  `Unreachable` at tag 4 with the comment `// 3 was \`panic\`; never reused.`,
  so decoding tag 3 becomes `Malformed`. Bump `LIR_CODEC_VERSION` to `6`.
  Remove or adapt the codec round-trip test case that built a `Panic`.
- [ ] **Step 3: Run the gates and commit:** `ir: remove the unused panic terminator`.

### Task 8: Docs

- [ ] **AGENTS.md:** add a section "Panics and stack traces" with these
  points:
  - `@panic` lowers to `.lilac.panic(msg, site)` plus `unreachable`; the
    handler lives in `lilac_ir::panic_runtime`.
  - Stack traces are a shadow stack inserted by `lilac_ir::trace::instrument`
    after DCE when `Session.stack_traces` is set (the CLI sets it in debug or
    with `--stack-traces=on`).
  - `@trace_head` is the only backend leaf, and `#.lilac.frame` is a builtin
    layout.
  - `SourceMap::location` and `trace::site_text` are the only formatters, and
    file names are namespace display paths.
  - A panic exits with code 101 and runs no drops.

  Also add `trace` to the pipeline line, and the `expect-stderr:` and
  `build-arg:` directives to the run-test note.
- [ ] **README.md:** add `--stack-traces=on|off` to the flag table ("Print a
  stack trace when the program panics; on by default in debug builds"). Add a
  short "Panics" paragraph showing `@panic("message")` and the output format.
- [ ] Commit: `docs: @panic and stack traces`.
