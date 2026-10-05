# M5c.1 VM Performance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the VM's large `Instr` code array with a compact `u32` instruction stream, precompute all type/layout metadata, pass outputs by reference, run an inner dispatch loop, and make the bench-only tree-walk baseline fair — reaching `fib(18) < 0.2 ms` in release.

**Architecture:** The typed `Instr` enum becomes compile-time IR only; `encode.rs` lowers it to a `Vec<u32>` stream with a wide-operand escape and constant/call pools, and `decode.rs` provides a borrowed `Decoded` view used by the verifier, the disassembler, and the interpreter's inline dispatch. Per-function argument/result layouts and per-instruction widths are precomputed; outputs are passed by reference (pointer ABI); `run()` keeps hot state in locals while `step()` remains the debugger path.

**Tech Stack:** Rust (stable, edition 2021), std only, `unsafe_code = "forbid"`, `clippy::pedantic` deny, no `serde`, no LLVM in `lilac-vm`.

**Spec:** `docs/superpowers/specs/2026-10-05-lilac-vm-m5c1-performance-design.md`

## Global Constraints

- Stable Rust, edition 2021; std only; no new dependency.
- `unsafe_code = "forbid"` and workspace lints (`clippy::all`/`pedantic` deny) apply.
- No `unwrap`/`expect`/`panic!` on user input; `lilac_error::bug!` is ICE-only (`expect` in tests fine).
- `inkwell`/`llvm-sys` stay confined to `lilac-codegen-llvm`.
- TDD per task; one commit per task.
- Gates before commit: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; and the release bench gate `cargo test --release -p lilac-vm --features bench-baseline`.
- `.lbc` is canonical text, LF-only, no BOM; snapshots regenerated with the .NET `Process` + `WriteAllText UTF8Encoding($false)` method.
- Encoding: opcode in the low byte; operands `a`/`b`/`c` in bits 8/16/24; wide escape sets bit 7 of the opcode byte and follows with `ext0 = a:u16 | (b:u16 << 16)` (and `ext1 = c:u32` only when a third wide operand exists); 24-bit immediates/pool indices; `Arith` is two words (`word1 = operand_size:8 | result_size:8`).
- Perf target: `fib(18) < 0.2 ms` release; fallback levers are superinstruction fusion and redundant-copy elimination in the LIR→bytecode compiler.

## Review Focus

Each line is pinned by a test in the owning task:

1. A wide operand (register/slot index ≥ 256) must encode/decode without corruption (Task 2).
2. A malformed `.lbc` word stream (bad opcode, out-of-range operand, misaligned branch target, bad call-site range) must be rejected cleanly with exit 2, never a panic/`bug!` (Task 3).
3. An output the callee never writes must leave the caller's own storage unchanged (LLVM parity), not zeroed (Task 6).
4. A function whose out place is passed by reference and then `addr`-taken must yield the caller's storage address (Task 6).
5. `fib(18)` in release must be < 0.2 ms, with absolute ns/op recorded (Task 9).

---

### Task 1: Release bench gate and absolute numbers

**Files:**
- Modify: `crates/lilac-vm/tests/bench.rs`
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: existing `SUITE`, `time`, `compile_src`.
- Produces: the bench prints absolute ns/op per program; `AGENTS.md` Gates include the release bench command.

- [ ] **Step 1: Write the failing test/change.** In `bench.rs`, replace the ratio-only assertion with (a) `eprintln!` of `vm.as_nanos()` per entry and (b) a `const FIB_BUDGET_NS: u128` check on the `fib` entry set initially generous (e.g. `2_000_000`), asserting `vm.as_nanos() < FIB_BUDGET_NS`. The test fails today (1.19 ms > 0.2 ms is not yet asserted; set the initial budget so it passes and tighten in Task 9).

- [ ] **Step 2: Run it.**

Run: `cargo test --release -p lilac-vm --test bench --features bench-baseline -- --nocapture`
Expected: PASS, with ns/op printed.

- [ ] **Step 3: Add the gate** to `AGENTS.md`: `cargo test --release -p lilac-vm --features bench-baseline` (and `cargo clippy --release -p lilac-vm --all-targets --features bench-baseline -- -D warnings`).

- [ ] **Step 4: Gates + commit.**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --release -p lilac-vm --features bench-baseline
git add crates/lilac-vm/tests/bench.rs AGENTS.md
git commit -m "M5c.1: run the bench gate in release and record absolute ns/op"
```

---

### Task 2: Compact encode/decode module

**Files:**
- Create: `crates/lilac-vm/src/encode.rs`
- Create: `crates/lilac-vm/src/decode.rs`
- Modify: `crates/lilac-vm/src/lib.rs`
- Test: inline `#[cfg(test)]` in both files

**Interfaces:**
- Consumes: `isa::{Instr, Operand, CallSite, Func, ArithOp, ConvertOp, Builtin}`.
- Produces:
  - `encode.rs`: `pub struct CodeBuilder { pub words: Vec<u32>, pub call_sites: Vec<CallSite>, pub operands: Vec<Operand> }`; `impl CodeBuilder { pub fn new() -> Self; pub fn push(&mut self, instr: &Instr) -> Result<(), String>; }`; `pub fn opcode_mnemonic(op: u8) -> &'static str`.
  - `decode.rs`: `pub enum Decoded<'a> { /* borrowed view mirroring Instr, Call/CallIndirect/CallBuiltin carry &'a CallSite + &'a [Operand] */ }`; `pub fn decode<'a>(code: &'a [u32], pc: usize, func: &'a Func) -> Result<(Decoded<'a>, u32), String>` (returns the decoded instruction and the next pc).
  - `lib.rs`: `mod encode; mod decode;` + re-exports.

- [ ] **Step 1: Write failing tests** in `encode.rs`:

```rust
#[test]
fn every_variant_round_trips() {
    for instr in sample_instrs() {           // one of each Instr variant
        let mut b = CodeBuilder::new();
        b.push(&instr).expect("encode");
        let func = func_with(b.words.clone(), b.call_sites.clone(), b.operands.clone());
        let (decoded, next) = decode(&func.code, 0, &func).expect("decode");
        assert_eq!(next as usize, func.code.len());
        assert!(equivalent(&instr, &decoded), "{instr:?}");
    }
}

#[test]
fn wide_operand_round_trips() {
    let instr = Instr::Copy { dst: Operand::Slot(300), src: Operand::Reg(511), ty: 0 };
    // encode -> decode yields the same operands
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p lilac-vm every_variant_round_trips`
Expected: FAIL — module/types missing.

- [ ] **Step 3: Implement `CodeBuilder`/`push` and `decode`.** Opcode table: one byte per `Instr`/`ArithOp`/`ConvertOp` variant, `< 128`; operand-kind variants (register vs slot) get distinct opcodes so operands stay one byte. Narrow form one word; wide escape as in Global Constraints; `Arith` two words; 24-bit const/pool/target fields; calls append a `CallSite` + operands to the pools and emit a 24-bit `call_site` index.

- [ ] **Step 4: Run to verify pass.**

Run: `cargo test -p lilac-vm`
Expected: PASS.

- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --check
cargo clippy -p lilac-vm --all-targets -- -D warnings
cargo test -p lilac-vm
git add crates/lilac-vm/src/encode.rs crates/lilac-vm/src/decode.rs crates/lilac-vm/src/lib.rs
git commit -m "M5c.1: add compact u32 encode/decode for the bytecode ISA"
```

---

### Task 3: Migrate `Program`/`Func.code` to the word stream

**Files:**
- Modify: `crates/lilac-vm/src/isa.rs` (add `CallSite`, `Func.code: Vec<u32>`, `call_sites`, `operands`; verifier decodes)
- Modify: `crates/lilac-vm/src/compile.rs` (build typed `Instr`, then `CodeBuilder::push`; store pools)
- Modify: `crates/lilac-vm/src/exec.rs` (decode inline over `&[u32]`)
- Test: inline in `isa.rs`/`exec.rs`

**Interfaces:**
- Consumes: Task 2 `CodeBuilder`/`decode`.
- Produces: `isa::CallSite { pub func: u32, pub ins: core::ops::Range<u32>, pub outs: core::ops::Range<u32> }`; `Func { ..., pub code: Vec<u32>, pub call_sites: Vec<CallSite>, pub operands: Vec<Operand> }`; `verify` decodes each word; `Machine` executes the stream (semantics unchanged from M5c).

- [ ] **Step 1: Write failing tests** in `isa.rs`:

```rust
#[test]
fn verifier_rejects_unknown_opcode() {
    let mut program = sample_program_words(vec![0x00FF_FFFF]); // invalid opcode
    assert!(verify(&program).is_err());
}

#[test]
fn verifier_rejects_misaligned_branch_target() {
    // a Branch whose target lands inside a two-word Arith
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p lilac-vm verifier_rejects_unknown_opcode`
Expected: FAIL.

- [ ] **Step 3: Implement.** Change `Func.code` to `Vec<u32>` + pools; `compile` keeps building typed `Instr` (as today) but appends via `CodeBuilder`; `verify` iterates `decode` and checks operand/branch/pool ranges; `exec::step` and `return_from_frame` decode inline. All M5c behavior tests must still pass unchanged.

- [ ] **Step 4: Run the full VM suite.**

Run: `cargo test -p lilac-vm`
Expected: PASS (existing behavior tests green on the stream).

- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --check
cargo clippy -p lilac-vm --all-targets -- -D warnings
cargo test -p lilac-vm
git add crates/lilac-vm/src/isa.rs crates/lilac-vm/src/compile.rs crates/lilac-vm/src/exec.rs
git commit -m "M5c.1: execute the compact u32 instruction stream"
```

---

### Task 4: `.lbc` codec and debugger over the word stream

**Files:**
- Modify: `crates/lilac-vm/src/codec.rs` (disassembly of the word stream)
- Modify: `crates/lilac-vm/src/debug.rs` (disasm/`stepi` decode)
- Test: inline in `codec.rs`; `crates/lilac-cli/tests/m5c.rs`; regenerate `tests/ui/vm_hello.lbc`

**Interfaces:**
- Consumes: Task 3 `Func.code`/pools, Task 2 `decode`.
- Produces: `Program::to_text`/`parse_text` round-trip over the word stream; `--check` byte-stable; debugger disassembles the stream.

- [ ] **Step 1: Write failing test** in `codec.rs`:

```rust
#[test]
fn text_round_trips_word_stream() {
    let program = compile_src("main: function () nothing {\n    x: @bit_native = @i_add(0x1, 0x2)\n}\n");
    let text = program.to_text();
    let parsed = Program::parse_text(&text).expect("parse");
    assert_eq!(parsed.to_text(), text);
    assert_eq!(parsed.funcs[0].code, program.funcs[0].code);
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p lilac-vm text_round_trips_word_stream`
Expected: FAIL.

- [ ] **Step 3: Implement** `to_text`/`parse_text` as a disassembly/assembler of the stream (parse text → typed `Instr` → `CodeBuilder`). Update `debug.rs` disasm/`stepi`.

- [ ] **Step 4: Regenerate `tests/ui/vm_hello.lbc`** with the no-BOM/LF method, then:

Run: `cargo test --workspace`
Expected: PASS (m5c `emit-vm`/`disasm` tests green with the new snapshot).

- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add crates/lilac-vm/src/codec.rs crates/lilac-vm/src/debug.rs crates/lilac-cli/tests/m5c.rs tests/ui/vm_hello.lbc
git commit -m "M5c.1: disassemble the word stream in .lbc and the debugger"
```

---

### Task 5: Precompute widths/masks and call/return layouts

**Files:**
- Modify: `crates/lilac-vm/src/isa.rs` (`Func.in_offsets`, `in_sizes`, `out_offset`, `out_sizes`; instruction size fields)
- Modify: `crates/lilac-vm/src/compile.rs` (compute them)
- Modify: `crates/lilac-vm/src/exec.rs` (use them; remove `type_size` from the hot path)
- Test: inline in `exec.rs`

**Interfaces:**
- Consumes: Task 3 word stream.
- Produces: `Func { pub in_offsets: Vec<u32>, pub in_sizes: Vec<u32>, pub out_offset: u32, pub out_sizes: Vec<u32> }`; no `type_size(program, ty)` call inside `step`/`call`/`return_from_frame`.

- [ ] **Step 1: Write failing test** (a structural guard):

```rust
#[test]
fn hot_path_has_no_type_lookups() {
    // After the change, call/return bind using Func.in_offsets/out_sizes;
    // assert a compiled fib program's Func has populated layouts and that
    // call/return copy the right values (drive a program and check outs).
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p lilac-vm hot_path_has_no_type_lookups`
Expected: FAIL.

- [ ] **Step 3: Implement.** Compute `in_offsets`/`in_sizes`/`out_offset`/`out_sizes` in `compile`; carry `size`/`mask`/`operand_size`/`result_size` in the encoded words (already needed by the ISA); update `call`/`return_from_frame` to use them. Remove the runtime `type_size` calls.

- [ ] **Step 4: Run tests.**

Run: `cargo test -p lilac-vm`
Expected: PASS.

- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --check
cargo clippy -p lilac-vm --all-targets -- -D warnings
cargo test -p lilac-vm
git add crates/lilac-vm/src/isa.rs crates/lilac-vm/src/compile.rs crates/lilac-vm/src/exec.rs
git commit -m "M5c.1: precompute instruction widths and call/return layouts"
```

---

### Task 6: Outputs by reference (pointer ABI)

**Files:**
- Modify: `crates/lilac-vm/src/compile.rs` (out places become ref-backed; accesses indirect)
- Modify: `crates/lilac-vm/src/exec.rs` (`call` fills the out-ref region; `return` copies nothing)
- Modify: `crates/lilac-vm/src/isa.rs` (`slot_bytes` includes the out-ref region)
- Test: inline in `exec.rs`; update affected behavior tests

**Interfaces:**
- Consumes: Task 5 layouts.
- Produces: callee out places are refs supplied by the caller; `Return` copies nothing.

- [ ] **Step 1: Write failing tests.**

```rust
#[test]
fn unwritten_output_leaves_caller_storage_unchanged() {
    // caller's out place pre-set to 0x7; callee never writes it; after the call
    // the caller's place is still 0x7 (LLVM parity), not 0.
}

#[test]
fn addr_of_out_place_is_caller_storage() {
    // callee takes addr of an out place; storing through it updates the caller's place
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p lilac-vm unwritten_output_leaves_caller_storage_unchanged`
Expected: FAIL.

- [ ] **Step 3: Implement.** Reserve an out-ref region at the frame base; `call` writes each caller out place's absolute offset there; compile out-place accesses as indirect (through the ref); `return` copies nothing. Update tests/snapshots that assumed the zeroed-callee-slot behavior.

- [ ] **Step 4: Run tests.**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add crates/lilac-vm/src/compile.rs crates/lilac-vm/src/exec.rs crates/lilac-vm/src/isa.rs
git commit -m "M5c.1: pass outputs by reference (pointer ABI)"
```

---

### Task 7: Inner dispatch loop

**Files:**
- Modify: `crates/lilac-vm/src/exec.rs`
- Test: inline in `exec.rs`

**Interfaces:**
- Consumes: Task 3–6.
- Produces: `run()` loops over the word stream with `pc`/`func`/`code`/bases in locals, syncing to `self.frames` only on call/return/trap; `step()` remains the single-instruction debugger path.

- [ ] **Step 1: Write a failing test** that pins observable behavior preserved by the rewrite:

```rust
#[test]
fn run_and_step_agree_on_result() {
    // For a program with calls, run() and a step()-driven loop produce the same
    // exit code and final place values.
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p lilac-vm run_and_step_agree_on_result`
Expected: FAIL (or passes; then assert the hot loop no longer writes `last_pc` per instruction via a structural check in review).

- [ ] **Step 3: Implement** the inner loop; keep `step()`/`current_line`/`backtrace` behavior for the debugger.

- [ ] **Step 4: Run tests.**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add crates/lilac-vm/src/exec.rs
git commit -m "M5c.1: run an inner dispatch loop with hot state in locals"
```

---

### Task 8: Fair tree-walk baseline

**Files:**
- Modify: `crates/lilac-vm/src/treewalk.rs`
- Test: `crates/lilac-vm/tests/bench.rs` (fidelity must still pass)

**Interfaces:**
- Consumes: `Program`, Task 3 word stream.
- Produces: `treewalk_run` uses `Vec`-indexed registers/slots and one contiguous arena.

- [ ] **Step 1: Confirm the fidelity test** `treewalk_matches_vm_on_suite` still pins equal computation (it must after the rewrite).

- [ ] **Step 2: Rewrite** `treewalk_run` to the Vec-indexed contiguous-arena model (still tree-walking the block/word structure).

- [ ] **Step 3: Run the fidelity + ratio tests.**

Run: `cargo test --release -p lilac-vm --test bench --features bench-baseline -- --nocapture`
Expected: PASS (fidelity green; ratio re-measured).

- [ ] **Step 4: Gates + commit.**

```bash
cargo fmt --check
cargo clippy -p lilac-vm --all-targets --features bench-baseline -- -D warnings
cargo test --release -p lilac-vm --features bench-baseline
git add crates/lilac-vm/src/treewalk.rs
git commit -m "M5c.1: make the tree-walk baseline a fair reference"
```

---

### Task 9: Reach `fib(18) < 0.2 ms`

**Files:**
- Modify: `crates/lilac-vm/src/compile.rs` (superinstructions / redundant-copy elimination if needed)
- Modify: `crates/lilac-vm/src/encode.rs`/`decode.rs`/`exec.rs` (new opcodes)
- Modify: `crates/lilac-vm/tests/bench.rs` (`FIB_BUDGET_NS = 200_000`)
- Test: bench

**Interfaces:**
- Consumes: Tasks 1–8.
- Produces: `fib(18) < 0.2 ms` release, recorded.

- [ ] **Step 1: Set `FIB_BUDGET_NS = 200_000`** in `bench.rs` (fails until the rework reaches it).

- [ ] **Step 2: Measure.**

Run: `cargo test --release -p lilac-vm --test bench --features bench-baseline -- --nocapture`
Expected: report ns/op; if `fib ≥ 0.2 ms`, apply the sanctioned levers.

- [ ] **Step 3: Apply levers as needed.** Superinstruction fusion (fuse common `copy`/`const`/`arith`/`branch` sequences into single opcodes) and redundant-copy elimination in `compile.rs` (keep values in registers instead of round-tripping through slots), one lever at a time, re-measuring each.

- [ ] **Step 4: Run the gate.**

Run: `cargo test --release -p lilac-vm --features bench-baseline`
Expected: PASS with `fib < 0.2 ms`; record the absolute numbers in the commit message.

- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --release -p lilac-vm --features bench-baseline
git add crates/lilac-vm
git commit -m "M5c.1: reach fib(18) < 0.2ms via superinstructions/copy elimination"
```

---

### Task 10: Documentation

**Files:**
- Modify: `docs/VM_SPEC.md` (compact ISA word layout, opcode table, pools, by-ref outputs)
- Modify: `AGENTS.md` (release bench gate)

**Interfaces:**
- Consumes: Tasks 1–9.
- Produces: docs matching the implementation.

- [ ] **Step 1: Rewrite `VM_SPEC.md`** §3 (ISA) to the word layout/opcode table, and update the memory/calling-convention and `.lbc` sections; update the debugger section for the stream.

- [ ] **Step 2: Confirm `AGENTS.md`** documents the release bench gate.

- [ ] **Step 3: Gates.**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Commit.**

```bash
git add docs/VM_SPEC.md AGENTS.md
git commit -m "M5c.1: document the compact ISA and release bench gate"
```

---

## Self-Review

**Spec coverage:** §3 compact ISA→Tasks 2–3; §3.3 pools→Task 2; §3.4 verifier→Task 3; §4 precompute→Task 5; §5 by-ref outputs→Task 6; §6 interpreter/inner loop→Task 7; §7 `.lbc`→Task 4; §8 baseline→Task 8; §9 gate/process→Tasks 1, 9; §10 testing→each task; §11 exit criteria→Tasks 9, 10; §12 risks (semantic change→Task 6 tests; escape bugs→Task 2 wide test; 0.2 ms→Task 9). No gap.

**Step scan:** each step names a test/command/signature; the only "as needed" step is Task 9 Step 3, which is the sanctioned performance-tuning lever with a measured gate, not a placeholder.

**Type consistency:** `CodeBuilder`, `CallSite`, `Decoded`, `decode`, `Func.code: Vec<u32>`, `in_offsets`/`in_sizes`/`out_offset`/`out_sizes` are defined in Tasks 2/3/5 and referenced verbatim later. `compile_src` is the existing `#[cfg(test)] pub(crate)` helper.

**Review Focus:** the five listed failure modes map to Tasks 2, 3, 6, 6, 9 respectively.

**Proportion:** the plan pins signatures, formats, test names, and exact values; instruction bodies are left to the implementer except where the spec fixes the encoding.
