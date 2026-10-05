# Lilac — M5c Bytecode VM Design (`lilac-vm`)

> Scope: a **second execution backend** that compiles verified LIR to a register
> bytecode and interprets it, plus a portable `.lbc` artifact, a standalone
> `lilac-vm` runner with an interactive debugger, and a first-class VM target.
> Work stays on branch `rust-rewrite-m0`. No LLVM is linked by the VM or its
> runner; `inkwell`/`llvm-sys` stay confined to `lilac-codegen-llvm`. Stable
> Rust, `unsafe_code = "forbid"`, std-only, no `serde`.

## 1. Outcome / constraints / success

- **Outcome:** `lilac` can compile a program to a portable `.lbc` bytecode text
  artifact and a standalone `lilac-vm` binary can run it — with breakpoints and
  single-stepping — on any host, without the frontend or LLVM. `lilac run` does
  the same in one step.
- **Speed:** the VM's dispatch loop is a `loop { match op }` over a **decode-once**
  flat instruction array with integer operands; registers are raw `u64`; memory
  is one byte arena. The benchmark gate is **≥3× the bench-only tree-walk**
  baseline on a small program suite, with absolute ns/op recorded.
- **Constraints:** (a) same gates as M0–M5b (`cargo fmt --check`,
  `clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`);
  (b) std-only, no new runtime dependency; (c) no `unwrap`/`expect`/`panic!` on
  user input (`lilac_error::bug!` for ICEs only); (d) `unsafe_code = "forbid"`
  and workspace lints apply to `lilac-vm`; (e) TDD per task, one commit per task;
  (f) `inkwell`/`llvm-sys` never linked outside `lilac-codegen-llvm`.
- **Success:** §11 exit criteria green.
- **Non-goals:** `extern` (foreign C) calls — a clean VM runtime error; JIT; GC;
  OS syscalls/async; `lilac build`/package caching; any language or IR change.

### 1.1 Decisions locked during brainstorming

1. **Safe switch dispatch**, stable Rust. Tail calls (`become`) are nightly-only
   and inline-asm computed goto is unsafe; both are out. On modern x86 a
   `loop { match }` compiles to a jump table within ~0–20% of computed
   goto/tail-call, and the register-vs-stack and decode-once wins dominate.
2. **Register machine, untyped slots.** Registers are raw `u64`; places are
   fixed-size byte slots in a per-frame region of one arena; `ref` is an arena
   offset. Types ride in the decoded stream and a static type table (used for
   widths and for the debugger to render values).
3. **VM bytecode is a second `Backend`.** `lilac-vm` implements the codegen
   backend; the frontend never imports the VM.
4. **Own crate.** New `crates/lilac-vm` (lib + standalone bin).
5. **Portable `.lbc` artifact + runner.** The artifact is **canonical text
   disassembly** (mirrors `.lir`, snapshot-testable); the runner parses it.
6. **First-class VM target.** Add `Arch::LilacVm` (`lilacvm-unknown-none`,
   64-bit pointers) to `lilac-target`; selecting it selects the VM backend.
7. **Interactive REPL debugger** with source-line + instruction granularity.
   `.lbc` embeds a line table `(file, line, col)`; the runner loads source from
   the recorded path (or `--source`), falling back to disassembly.
8. **All LIR except `extern`** must execute correctly in the first milestone.
9. **Perf baseline** is a bench-only tree-walking LIR interpreter in `lilac-vm`
   (not shipped), timed by a std-only harness.

### 1.2 What is already in place (verified before design)

- `LirModule { version, target, data, funcs, layouts }`; `LirDataKind::{Str,
  NumLit}`; `LirFunc { name, ins, outs, is_extern, blocks }`; `LirInst` variants
  `CopyReg/CopyPlace/Const/Load/Addr/AddrFunc/Call/Field/StoreField/Store/
  Convert/FieldAddr/Alloca/Drop/Arith/CompileError/FoldIntrinsic`; `LirTerm`
  `Return/Branch/BranchIf/Panic/Unreachable`; `LirCallee` `Reg/Func/Builtin`.
- Registers (`&r`) are block-local SSA; places (`%p`) are function-scoped,
  `alloca`-zeroed, and mutable. The pointer-place ABI means `addr`/`field.addr`
  produce `ref`s that `load`/`store` dereference.
- The nine runtime functions (`.lilac.byte_buffer_from/ptr/len`, `.lilac.load_u8`,
  `.lilac.store_u8`, `.lilac.format_args_new/push/count/piece`) are ordinary LIR
  injected between `fold` and `prune_unreachable`; only `@ptr_add`, `@memcpy`,
  and `@lilac_abort` are leaf builtins a backend implements
  (`lilac-ir/src/runtime.rs`).
- Types are stringly: `#bit8/16/32/64`, `#bit_native`, `ref #t`, `#byte_buffer`,
  `#format_args`, and named struct/enum types. Floats are **bit patterns** in
  `#bit32`/`#bit64`; `f.*` ops interpret them as IEEE-754 and `cmp.eql` is
  bit-equality (`lilac-const/src/lib.rs:542`, `lilac-codegen-llvm/src/lower.rs:971`).
  Struct field order lives in `LirModule.layouts`.
- `Span` is `(lo, hi, FileId)`; `SourceFile::line_col`/`line_text` render lines
  (`lilac-span/src/lib.rs`).
- `Backend` has `name`, `codegen`, `emit_llvm` (default error),
  `build_exe_and_run` (default post-v1 error) (`lilac-codegen/src/lib.rs`).
- `Target` is structured; `Arch::is_64_bit` is the single width rule
  (`lilac-target/src/lib.rs`).
- The driver's pipeline is private to `Session`; the CLI calls
  `compile_lir`/`compile_lir_with_stages` and then instantiates a backend.

## 2. Architecture

```text
crates/
  lilac-target/     # + Arch::LilacVm ("lilacvm"), is_64_bit=true, Target::is_vm
  lilac-codegen/    # + Backend::emit_bytecode(&self,&LirModule)->Result<String,String>
  lilac-vm/         # NEW: isa, compile, exec, codec(text), disasm, debug, bin
  lilac-cli/        # + emit-vm / run / disasm; --target=lilacvm-... selects VM
  lilac-ir/         # unchanged (no new IR, no codec bump)
```

Dependency edges (no cycles): `lilac-vm → {lilac-ir, lilac-codegen,
lilac-target, lilac-span, lilac-error}`; `lilac-cli → lilac-vm`;
`lilac-codegen` gains no dependency (the bytecode is opaque `Vec<u8>`/text).
`lilac-vm` never links LLVM.

## 3. Components

### 3.1 `lilac-target` — VM target

- `Arch::LilacVm`, parse `"lilacvm"`, render `"lilacvm"`, `is_64_bit() == true`.
- `Target::is_vm(&self) -> bool` = `self.arch == Arch::LilacVm`.
- Canonical VM target: `lilacvm-unknown-none` (vendor `Unknown`, os `None`).
- The all-`Arch` width test gains `(Arch::LilacVm, 64)`.

### 3.2 `lilac-codegen` — backend hook

- Add to `Backend`:
  ```rust
  fn emit_bytecode(&self, _module: &LirModule) -> Result<String, String> {
      Err(format!("backend `{}` cannot emit bytecode", self.name()))
  }
  ```
  mirroring `emit_llvm` (the `.lbc` artifact is text). Default error keeps a
  `--backend=` mismatch diagnosable.

### 3.3 `lilac-vm` crate

- `Cargo.toml` inherits workspace lints; deps: `lilac-ir`, `lilac-codegen`,
  `lilac-target`, `lilac-span`, `lilac-error`. `[[bin]] name = "lilac-vm"`.
- Modules:
  - `isa.rs` — `Program`, `Func`, `Instr`, operand/opcode types, `TypeId`,
    `Data`, `Line`, the program verifier.
  - `compile.rs` — `compile(&LirModule, &SourceMap) -> Result<Program, String>`:
    LIR → register bytecode (types, slots, labels, line table).
  - `exec.rs` — `Machine`, frame/arena memory, the dispatch loop, builtins.
  - `codec.rs` — `Program::to_text()` / `parse_text()` for `.lbc`.
  - `disasm.rs` — human disassembly (used by `.lbc` and the debugger).
  - `debug.rs` — the REPL (`Breakpoint`, `Debugger`, command parsing).
  - `treewalk.rs` — bench-only baseline (feature `bench-baseline` or
    `#[cfg(test)]`; never in the shipped path).
  - `bin/lilac-vm.rs` — standalone runner CLI.
- Public API: `compile`, `Program::{to_text, parse_text}`, `Machine::{new, run}`,
  `VmBackend`, `run_lbc`.

### 3.4 `lilac-cli` integration

- `parse_common` learns `--source=` (for `run`/debug) and, for `emit-vm`, an
  implicit VM target when none is given.
- New subcommands:
  - `lilac emit-vm <file> [--check] [--target=...] [--dump-lir=...] [--export=...]`
    → compiles to the VM target and prints canonical `.lbc` text; `--check`
    compares against `<file>.lbc`.
  - `lilac run <file> [--source=<path>] [--break=<loc>] [--step] [--trace]`
    → compiles and executes; enters the REPL when a debug flag is present.
  - `lilac disasm <file.lbc>` → canonical disassembly.
- `emit-llvm` with a VM target and `emit-vm`/`run` with a non-VM target exit 2.
- `usage()` gains the three subcommands.

## 4. Bytecode ISA

`Program` is the in-memory and on-disk model. It is target-tagged and versioned.

```rust
pub struct Program {
    pub version: String,          // LBC_VERSION
    pub target: String,           // "lilacvm-unknown-none"
    pub data: Vec<Data>,          // string/number constants
    pub globals: Vec<u8>,         // concatenated data bytes (ref targets)
    pub funcs: Vec<Func>,
}

pub struct Func {
    pub name: String,
    pub is_extern: bool,          // retained so the runner can error cleanly
    pub ins: Vec<TypeId>,
    pub outs: Vec<TypeId>,
    pub n_regs: u32,              // register-file slots for this frame
    pub slot_bytes: u32,          // per-frame place region (zeroed at entry)
    pub code: Vec<Instr>,
    pub lines: Vec<Line>,         // pc -> (file, line, col), sorted by pc
}

pub struct Data { pub bytes: Vec<u8>, pub global_off: u32 }
pub struct Line { pub pc: u32, pub file: String, pub line: u32, pub col: u32 }
```

Operands are integers, resolved at compile time:

```rust
enum Reg { R(u32) }            // register index in the frame
enum Place { Slot(u32) }       // byte offset within the frame's slot region
enum Operand { Reg(u32), Slot(u32) }
```

`Instr` (decode-once, one variant per LIR operation; no per-step string decode):

| Variant | Semantics |
|---|---|
| `Const { dst: Reg, imm: u64, ty }` | integer/float literal bits into a register |
| `ConstData { dst: Reg, data: u32, ty }` | `ref` to `globals[data.global_off]` (data const) |
| `Copy { dst: Operand, src: Operand, ty }` | scalar copy (width from `ty`) |
| `CopyBytes { dst: Slot, src: Slot, size: u32 }` | aggregate place copy |
| `Load { dst: Reg, src: Slot, ty }` | read `size(ty)` bytes at the ref in `src` |
| `Store { src: Operand, dst: Slot, ty }` | write `size(ty)` bytes through the ref in `dst` |
| `Addr { dst: Reg, place: Slot }` | ref = frame slot address |
| `AddrField { dst: Reg, place: Slot, off: u32 }` | ref = frame slot + field offset |
| `AddrFunc { dst: Reg, func: u32 }` | ref = function id |
| `Field { dst: Reg, place: Slot, off: u32, ty }` | scalar field read |
| `StoreField { src: Operand, place: Slot, off: u32, ty }` | scalar field write |
| `Arith { op: ArithOp, dst: Reg, lhs: Reg, rhs: Reg, ty }` | typed arithmetic/comparison |
| `Convert { op: ConvertOp, dst: Reg, src: Reg, ty }` | `trunc`/`extend.zero` |
| `Call { func: u32, ins: Vec<Operand>, outs: Vec<Slot> }` | direct call |
| `CallIndirect { callee: Reg, ins: Vec<Operand>, outs: Vec<Slot> }` | `addr.func` value |
| `CallBuiltin { builtin: Builtin, ins: Vec<Operand>, outs: Vec<Slot> }` | `@ptr_add`/`@memcpy`/`@lilac_abort` |
| `Branch { target: u32 }` | unconditional |
| `BranchIf { cond: Reg, then: u32, els: u32 }` | conditional |
| `Return` | copy `outs` to the caller's out slots, pop frame |
| `Panic { data: u32 }` | print `data` bytes to stderr, abort non-zero |
| `Unreachable` | VM internal error / trap |

- `TypeId` is an index into a program type table with `size`, `is_ref`, and a
  printable name; `#bit8/16/32/64`, `#bit_native` (64 on VM), `ref #t`, and
  named types sized from `LirModule.layouts`. Alignment is ignored (packed
  offsets); the VM only shares memory with itself.
- `alloca` and `drop` produce **no code**: the frame's slot region is sized and
  zeroed at entry (`alloca` zero-init is the VM default); `drop` is a marker
  whose destructor calls already precede it.
- `ArithOp`/`ConvertOp` mirror `lilac-ir`; `f.*`/`cmp.eql` follow the existing
  bit-pattern semantics (`#bit32`→f32, `#bit64`→f64).

## 5. `.lbc` text format

Canonical, line-oriented, LF-only, no BOM (like `.lir`), versioned header:

```text
lbc = "0.1"
target = "lilacvm-unknown-none"

data !c0 = "hi" @ 0x0
data !c1 = "5.25" @ 0x2

func @main () -> () regs 4 slots 16
  line 1:1
  $entry:
    &0: #bit_native = const 0x1
    &1: #bit_native = const 0x2
    &2: #bit_native = i.add &0, &1
    call @.lilac.load_u8 (in &0, in &1) (out %0)
    branch $done
  $done:
    return
```

- Deterministic ordering (data by index, funcs by index, code by pc); every
  instruction prints its resolved operands. `Program::to_text()` emits exactly
  this; `parse_text` is its inverse.
- The runner rejects an unsupported `lbc` version or a non-VM `target` with a
  clear message and exit 2.
- Snapshot tests store `<stem>.lbc` and compare byte-for-byte via `--check`
  (same no-BOM/LF capture method as `.lir`).

## 6. LIR → bytecode compilation

`compile(module, source_map)` walks each `LirFunc` and produces a `Func`:

1. **Type table.** Intern `size_of`/name for every type appearing in the
   function; sizes from `#bitN`/`ref`/named layouts (packed).
2. **Places.** Build `place -> (slot_offset, TypeId)` from function `ins`/`outs`
   and each `alloca`; lay out the frame's slot region by packing (offset
   accumulated in declaration order); `slot_bytes` is the total.
3. **Registers.** Assign a dense `u32` per register definition; `n_regs` is the
   count. Registers are block-local in LIR, so the same name in two blocks maps
   to distinct indices.
4. **Blocks → pc.** Emit code per block; record label→pc; resolve `Branch`/
   `BranchIf` targets and `BranchIf`'s missing `els` to the next block in layout
   order (matching the LLVM backend's fall-through rule).
5. **Instructions.** Map each `LirInst` to the table in §4: `Addr`/`FieldAddr`
   carry precomputed field offsets (from `layouts` + field index); `Const`
   literals parse to raw bits at the declared width; `!cN` data consts become
   `ConstData`; direct `Call`→`Call`, `Call` through `Reg`→`CallIndirect`,
   `Call` to `@ptr_add`/`@memcpy`/`@lilac_abort`→`CallBuiltin` (the `.lilac.*`
   runtime funcs are ordinary `FuncId`s).
6. **Line table.** For each instruction/terminator `Span`, `source_map` yields
   `(file name, line, col)`; emit one `Line` per distinct pc. Dummy spans are
   skipped.
7. **Externs.** `is_extern` functions compile to a signature-only `Func` with an
   empty body; a call to one is a VM runtime error at execution time.

The compiled program is re-verified by the VM's own verifier (register/slot
bounds, branch targets in range, arity, type sizes) before it is serialized or
run; failures are `bug!`-class internal errors (the input LIR already verified).

## 7. Interpreter

```rust
pub struct Machine<'a> {
    program: &'a Program,
    regs: Vec<u64>,          // all frames' registers, grown on call
    arena: Vec<u8>,          // frame slot regions + globals
    frames: Vec<Frame>,      // call stack
    // diagnostics/debug hooks
}
struct Frame { func: u32, reg_base: u32, slot_base: u32, ret_pc: u32,
               out_base: u32 /* caller slot for outs */ }
```

- **Memory.** A `ref` is a `u64` byte offset into `arena`. Frame slot regions are
  allocated by bumping `arena` at call entry and zeroed; `globals` occupy a
  reserved low range. `Load`/`Store`/`Field` read/write little-endian bytes;
  `@memcpy` copies bytes; `@ptr_add` adds a byte offset. Out-of-range refs are a
  VM trap (never a panic) with a diagnostic.
- **Calls.** Push a `Frame`, allocate `n_regs` registers and `slot_bytes` arena
  bytes, bind `ins` into the new frame's input slots, jump to the callee's
  entry pc. `Return` copies the callee's `outs` into the caller's out slots and
  restores `ret_pc`. `CallIndirect` reads a function id from a register.
- **Builtins.** `@ptr_add(ref, int) -> ref`; `@memcpy(dst, src, len)`;
  `@lilac_abort()` → print the pending panic message (if any) and abort.
- **Panic.** `Panic` prints `data` bytes to stderr (`panic: <msg>`) and exits
  non-zero; the LLVM backend currently traps without printing, so the VM is
  strictly more informative — recorded as a known, intentional divergence in
  §12.
- **Entry.** `main` (or the single `--export` root) runs first; exit code 0 on
  normal return. No `main` and no export root is a clean "no entry point" error.
- **Dispatch.** `run()` keeps `pc`, `&program.funcs[f]`, and frame bases in
  locals; `loop { match instr { … } }` with `continue`/direct jumps. No
  `HashMap`/`String` in the loop. `#[inline]` on leaf handlers where it helps.

## 8. Runner + debugger

Standalone CLI (`lilac-vm`):

```text
lilac-vm <file.lbc> [--source=<path>] [--break=<fn>|<file>:<line>]... [--step] [--trace]
```

- Without debug flags: run to completion, print program output, exit with its
  code.
- With `--break`/`--step`/`--trace` (or when invoked as a REPL): start the
  interactive debugger.

REPL commands (whitespace-split, no external crate):

| Command | Effect |
|---|---|
| `break <fn>` / `break <file>:<line>` | set a breakpoint |
| `delete <n>` / `clear` | remove breakpoints |
| `run` / `continue` | resume |
| `step` | run to the next source line |
| `stepi` | execute one bytecode instruction |
| `next` / `finish` | source-level step-over / step-out |
| `print <reg>` / `print %<place>` | render a typed value from the current frame |
| `info regs\|stack\|funcs\|breaks` | state dumps |
| `backtrace` | call stack with `(file:line, function)` per frame |
| `disasm [pc]` | canonical disassembly around pc |
| `list [file:line]` | source lines from the loaded file |
| `quit` | exit |

- The debugger reads source from the line table's recorded path, or `--source`;
  when the file is unavailable it falls back to disassembly and still steps by
  instruction. Scripted tests feed commands on stdin and assert the transcript.

## 9. Performance strategy + benchmark gate

- Wins applied: register machine; decode-once `Vec<Instr>`; integer operands;
  `u64` registers; contiguous arena; no name/`HashMap` lookups in the loop;
  `match` compiled to a jump table; frame bases cached in locals.
- Deferred (only if the gate fails): superinstructions/fusing, inline caches for
  `Field`/`CallIndirect`, block-level `pc` threading.
- **Benchmark suite** (`crates/lilac-vm/benches/` as `.lilac` sources or inline):
  fib, mandelbrot, string/format-heavy, struct/field/drop, call-heavy.
- **Baseline:** `treewalk.rs`, a straightforward tree-walking LIR interpreter
  (name→value maps, like `lilac-const::Evaluator`), compiled only under the
  `bench-baseline` feature / `#[cfg(test)]`; never shipped.
- **Harness:** std-only (`std::time::Instant`), invoked by a `lilac-vm` bench
  binary/test; prints ns/op for VM and tree-walk and the ratio. The gate asserts
  VM ≥ 3× on every suite entry and records absolute numbers in the spec/plan.

## 10. Testing

- **Unit:** `isa`/verifier; `compile` per `LirInst`→`Instr`; `codec` round-trip
  `parse_text(to_text(p)) == p`; type sizing (including `#byte_buffer`=16 and the
  `format_args` stride agreement with `runtime.rs`); line-table extraction.
- **Integration (`crates/lilac-cli/tests/m5c.rs`):** compile a `.lilac` to VM
  target, snapshot `<stem>.lbc` (`--check`), parse+run, and assert stdout/exit
  for programs covering integers, floats, control flow, direct/indirect calls,
  byte buffers, `format_args`, structs/fields, drops, and panic. An `extern` call
  asserts the clean runtime error. `emit-vm`/`run` target-mismatch exit-2 tests.
- **Debugger:** scripted REPL transcripts (break/step/stepi/print/backtrace).
- **Benchmark:** the ≥3× gate and recorded absolute numbers.
- **No LLVM:** a grep gate proves `lilac-vm` (and `lilac-cli` via it) has no
  `inkwell`/`llvm-sys`; `cargo test --workspace` proves no `.lir`/`.ll` drift.

## 11. Ports + exit criteria

- Add `crates/lilac-vm`; add `Arch::LilacVm`/`Target::is_vm`; add
  `Backend::emit_bytecode`; wire the CLI (`emit-vm`, `run`, `disasm`).
- Exit:
  1. `lilac emit-vm` produces a canonical `.lbc` for the VM target; `--check`
     is byte-stable; a non-VM target or a mismatched backend exits 2.
  2. `lilac-vm <file.lbc>` runs every suite program correctly on the host with
     no frontend or LLVM; `lilac run` matches.
  3. The debugger supports breakpoints, `step`/`stepi`, `print`, and
     `backtrace`, driven by source-line info loaded from the recorded path.
  4. The bench gate shows ≥3× over the tree-walk baseline on every suite entry,
     with absolute ns/op recorded.
  5. `cargo fmt --check`, `clippy --workspace --all-targets -- -D warnings`,
     `cargo test --workspace` green; `unsafe_code = forbid` intact; no new
     dependency; `inkwell`/`llvm-sys` grep-clean outside `lilac-codegen-llvm`.
  6. `docs/VM_SPEC.md` documents the ISA + `.lbc`; `SPEC.md` notes the VM target;
     `AGENTS.md` notes the VM as a second backend (and the new target arch).
  7. Human review of this spec and its plan.

## 12. Risks / known divergences

- **Panic output:** the VM prints `panic: <msg>`; the LLVM backend currently
  traps silently. Intentional, documented; revisit if the LLVM side prints later.
- **`extern`:** unsupported in the VM (clean error), consistent with I5.
- **Layout parity:** the VM's packed layout need only be self-consistent, but the
  `#byte_buffer` size (16) and `format_args` piece stride (16) are hardcoded in
  the injected runtime and must match the VM's sizing — an explicit test pins it.
- **Dispatch ceiling:** on ARM the safe switch VM may trail a nightly tail-call
  VM by ~20–40%; accepted. The `become`-based core can be revisited if/when
  tail calls stabilize (~2027) without changing the ISA.
- **Text `.lbc` load cost** is one-time; if artifacts grow, a binary codec can be
  added alongside `to_text`/`parse_text` without an ISA change.

## 13. Self-review

- **Placeholders:** none — every type, instruction, command, test, and doc edit
  names its owner.
- **Consistency:** §1.1's nine decisions match §3–§9; the ISA in §4 covers every
  `LirInst`/`LirTerm` variant (with `alloca`/`drop` folded to metadata and
  `CompileError`/`FoldIntrinsic` impossible post-verify); §5's format is the
  inverse of §6's compiler; §7's semantics match §1.2's verified LIR facts.
- **Scope:** strictly M5c — no IR/language change, no LLVM change beyond adding
  the `emit_bytecode` default, no package/build work.
- **Ambiguity:** dispatch (safe switch), representation (untyped slots), target
  name (`lilacvm`), artifact (text), debug granularity (source + `stepi`),
  baseline (bench-only tree-walk), and extern/panic behavior are all decided
  here.
