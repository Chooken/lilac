# Lilac — M5c.1 Bytecode VM Performance Design

> Scope: rework the `lilac-vm` execution representation and calling convention so
> the interpreter is genuinely fast. The milestone replaces the large `Instr`
> enum code array with a **compact 32-bit instruction stream**, precomputes every
> size/offset/mask the compiler already knows, passes **outputs by reference**
> (matching the LIR/LLVM pointer ABI), runs an **inner dispatch loop** with the
> hot state in locals, and rewrites the bench-only tree-walk baseline to be a
> fair reference. Work stays on branch `rust-rewrite-m0`. Stable Rust,
> `unsafe_code = "forbid"`, std-only. No LLVM is linked.

## 1. Outcome / constraints / success

- **Outcome:** `fib(18)` in release drops from **1.19 ms** to **under 0.2 ms**
  (≈6×, and ~4× faster than CPython's ~0.8 ms) — with proportional wins on the
  rest of the suite — and the executed artifact is a compact `u32` stream
  (≈16 instructions per cache line vs ≈1 today).
- **Constraints:** (a) same gates as M5c plus a **release** bench gate
  (`cargo test --release -p lilac-vm --features bench-baseline`); (b) std-only,
  no new dependency; (c) no `unwrap`/`expect`/`panic!` on user input
  (`lilac_error::bug!` for ICEs); (d) `unsafe_code = "forbid"` and workspace
  lints apply; (e) TDD per task, one commit per task; (f) `inkwell`/`llvm-sys`
  stay confined to `lilac-codegen-llvm`.
- **Success:** §12 exit criteria green.
- **Non-goals:** JIT/AOT (that is the LLVM backend; ~50 µs fib is LuaJIT
  territory and out of reach for a switch interpreter), GC, async, `extern`.

### 1.1 Decisions locked during brainstorming

1. **Compact `u32` instruction stream.** The executed code is `Vec<u32>`; the
   typed `Instr` enum survives only as a compile-time intermediate (compiler and
   verifier), never as the execution representation.
2. **8-bit operands with an escape word.** The narrow form packs opcode + up to
   three byte operands in one word; operands that do not fit use a wide escape
   form (extension word(s)). No register spilling is required for the common
   case.
3. **Flattened opcodes.** Each arithmetic/comparison/conversion operation is its
   own opcode — one `match` per instruction, no nested `ArithOp` match.
4. **Precompute all type metadata.** Instruction widths/masks and per-function
   argument/result layouts are computed at compile time; the interpreter never
   calls `type_size`.
5. **Outputs by reference (pointer ABI).** The callee writes directly into the
   caller's out places; there is no copy on return. This restores parity with the
   LLVM backend (`call void @f(ptr %in…, ptr %out…)`, `lower.rs:505-563`).
6. **Inputs copied in, with precomputed offsets.** Input values are copied into
   the callee frame (preserving today's snapshot semantics) using precomputed
   `in_offsets`/`in_sizes`; no runtime `type_size`.
7. **Inner dispatch loop.** `run()` keeps `pc`/function/bases in locals and only
   touches the frame stack on call/return; `step()` remains the debugger's
   single-instruction path.
8. **Fair baseline.** The bench-only tree-walker uses `Vec`-indexed
   registers/slots and a contiguous arena (still a tree-walker), so the ≥3× gate
   compares against a reasonable reference.
9. **Release gate.** The bench runs in release mode and records absolute ns/op;
   a perf-budget assertion replaces the ratio-only check as the primary signal.

### 1.2 Evidence (measured before design)

Release (`cargo test --release -p lilac-vm --test bench --features bench-baseline`):

| program | tree-walk | VM | notes |
|---|---|---|---|
| fib | 25.0 ms | **1.19 ms** | ~8,361 calls × ~20 instructions ≈ 170k instr ≈ 7 ns/instr |
| mandelbrot | 900 µs | 65 µs | |
| strings | 4.14 ms | 171 µs | |
| structs | 264 µs | 14.3 µs | |
| calls | 453 µs | 25.9 µs | |

Root causes (read from `exec.rs`/`compile.rs`):

- `type_size(program, ty)` is called inside the loop for `Const` (`:229`),
  `Copy` (`:240`), `Arith` twice (`:261-262`), `Convert` (`:269`), `Load`
  (`:289`), `Store` (`:302`), `Field` (`:339`), `StoreField` (`:363`), per
  call argument (`:518`), and per return input/output (`:545,565`).
- `Instr::Arith` dispatches `op` through a second `match` (`eval_arith`).
- `Instr` is a large enum (the `Call { ins: Vec<Operand>, outs: Vec<u32> }`
  variants dominate) — poor code-array density and a heap deref per call.
- `step()` re-derives `frame`/`func`/`code.get(pc)` and writes
  `frame.pc`/`frame.last_pc` through `frames.last_mut()` every instruction,
  returning a `Result` each time (`:209-226`).
- `return_from_frame` re-sums the callee's input sizes to find the out offset
  **every return** (`:543-546`) and re-reads the caller's call instruction
  (`:550-560`).
- The VM does **not** allocate per instruction; the tree-walker does (per-frame
  `HashMap`, sparse byte-addressed memory).

## 2. Architecture

```text
crates/lilac-vm/
  isa.rs        # typed Instr (compile-time IR), Program, pools, verifier
  compile.rs    # LIR -> typed Instr
  encode.rs     # typed Instr -> compact u32 stream (NEW)
  decode.rs     # compact u32 -> typed Instr / disassembly (NEW)
  exec.rs       # Machine: decode-and-execute over Vec<u32>, inner loop
  codec.rs      # .lbc text = disassembly of the u32 stream
  debug.rs      # debugger decodes for disasm/stepi
```

Dependency edges unchanged. No new crate, no new dependency.

## 3. Compact ISA encoding

### 3.1 Word layout

The executed code is `Func.code: Vec<u32>`. Word 0 of every instruction carries
the opcode in the low byte; operands occupy the upper three bytes:

```text
word0 = [ op:8 | a:8 | b:8 | c:8 ]        // op = w & 0xFF; a = (w>>8)&0xFF, ...
```

- **Narrow form:** one word, operands are 8-bit register indices or slot byte
  offsets (both `< 256`).
- **Wide escape:** if any operand is `>= 256`, the instruction uses the wide
  form: the opcode byte's high bit is set (`op | 0x80`, base opcode
  `op & 0x7F`) and one or two extension words follow:
  - `ext0 = a:u16 | (b:u16 << 16)`
  - `ext1 = c:u32` (only when a third wide operand exists)
- **Immediates/pool indices:** a 24-bit field (`w >> 8`) for branch targets and
  constant-pool indices (up to 16M each), which is ample.
- **Width-specialized arithmetic, one word.** Each `ArithOp`/`ConvertOp` is
  encoded as one opcode per width class where the width matters (e.g.
  `i.add.32`), so arithmetic is a **single word** `[op][dst][lhs][rhs]` and the
  interpreter matches the opcode directly — no `ArithOp` sub-match and no second
  type word. Width is chosen at encode time from the type table. Width is
  encoded only where semantics require it: signed div/mod and signed compares
  (sign extension), float ops (f32/f64), integer arithmetic result masking
  (`IAdd`/`ISub`/`IMul`), and `Trunc`. Operations whose operands are
  zero-extended to their declared width (`Eq`, unsigned compares, unsigned
  div/mod, `ExtendZero`) need no width class.
- **Precomputed sizes, not type ids.** Every instruction that today carries a
  `TypeId` only to look up a size/width (`Copy`, `Load`, `Store`, `Field`,
  `StoreField`, `Convert`, `Arith`, `Const`) encodes the **precomputed byte
  size/width** instead. `CodeBuilder` takes the program type table so it can
  compute them at encode time; the interpreter never looks up a type.
  `Const64` remains two words for a 64-bit immediate that is not pooled.

Operand **kind** (register vs slot vs immediate) is encoded in the opcode, so no
per-operand tag bit is needed and operands stay a full byte.

### 3.2 Opcodes

Flattened, `< 128` base opcodes:

- **Core:** `Const`, `ConstData`, `Copy` (register/slot kind variants),
  `CopyBytes`, `Load`, `Store`, `Addr`, `AddrField`, `AddrFunc`, `Field`,
  `StoreField`, `Call`, `CallIndirect`, `CallBuiltin`, `Branch`, `BranchIf`,
  `Return`, `Panic`, `Unreachable`.
- **Arith/Convert:** one opcode per `(operation, width class)` where the width
  matters (see §3.1) — `IAdd`/`ISub`/`IMul`/`IDiv`/`IMod`/`ILt`/`IGt`/`Trunc` at
  widths 8/16/32/64, `FAdd`/`FSub`/`FMul`/`FDiv`/`FMod`/`FLt`/`FGt` at 32/64 —
  and a single width-free opcode for `IDivU`/`IModU`/`Eq`/`ILtU`/`IGtU`/
  `ExtendZero`. One word each; the interpreter matches the opcode directly.

The exact opcode table is enumerated in `docs/VM_SPEC.md` at implementation; the
constraint is `< 128` base opcodes so the wide bit is available.

### 3.3 Pools

- **Constants:** the existing `Program.data`; `Const`/`ConstData`/`Panic` carry a
  24-bit index.
- **Call operands:** a per-function `call_sites: Vec<CallSite>` where
  `CallSite { func: u32, ins: Range<u32>, outs: Range<u32> }` indexes a shared
  `Func.operands: Vec<Operand>` pool. A call instruction carries a 24-bit
  `call_site` index. No `Vec` in the executed stream.

### 3.4 Verifier

`verify` decodes the stream (via `decode.rs`) and checks: opcode validity,
operand bounds (register `< n_regs`, slot `< slot_bytes`, pool indices in range),
branch targets `< code.len()` and landing on an instruction boundary, call-site
ranges in range, and the existing structural rules (globals invariant, final
terminator, `MAX_REGS`/`MAX_FRAME_BYTES`). A malformed stream is a clean error
(exit 2 via the runner), never a `bug!`.

## 4. Precomputed metadata

- `Func` gains:
  - `in_offsets: Vec<u32>`, `in_sizes: Vec<u8>` — where each input lands in the
    callee's input region.
  - `out_offset: u32`, `out_sizes: Vec<u8>` — the result layout (used to place
    the out-ref region).
  - `code: Vec<u32>`, `call_sites: Vec<CallSite>`, `operands: Vec<Operand>`.
- Instructions carry precomputed `size`/`mask`/`operand_size`/`result_size`
  where they need them; the interpreter never calls `type_size`.
- `Func.is_extern` is read once at call; the frame caches the call site's out
  descriptors so `return` does not re-read the call instruction.

## 5. Calling convention (pointer ABI for outputs)

- **Outputs by reference.** The callee frame begins with an **out-ref region**
  of `outs.len()` `u64` refs. At `call`, the caller writes the absolute arena
  offset of each of its out places into that region. The compiler compiles
  accesses to an out place as **indirect** (through the ref), exactly as it does
  for a `ref`-typed place; `addr %out` is the ref itself. `Return` copies
  nothing.
- **Inputs copied in.** At `call`, each input value is copied from the caller's
  place into the callee's input region using `in_offsets`/`in_sizes`; the callee
  accesses input places directly. This preserves the current snapshot semantics
  (a place passed as both input and output is read before the call writes it).
- **Semantics.** By-ref outputs match the LLVM backend: an output the callee
  never writes leaves the caller's own storage unchanged. This is a deliberate
  correction of a latent divergence (the current VM copies the callee's
  zero-initialized slot); affected VM tests/snapshots are updated to the LLVM
  behavior.
- The verifier's flow-insensitive "each out assigned ≥ once" rule is unchanged.

## 6. Interpreter

### 6.1 Inner dispatch loop

`run()` holds `func`, `code: &[u32]`, `pc`, `reg_base`, and `slot_base` in
locals and loops over words, decoding inline:

```rust
let w = code[pc];
match (w & 0xFF) as u8 { /* ... */ }
```

It only synchronizes to `self.frames` on `call`/`return` (and on trap). This
removes the per-instruction `frames.last_mut()` writes, the per-instruction
re-derivation, and the per-instruction `Result`. Out-of-range `pc`, `Unreachable`,
and malformed operands return clean `VmError`s (never `bug!`).

### 6.2 Debugger path

`step()` remains a single-instruction path over the same stream for the debugger
(breakpoints, `stepi`), decoding one word and executing it. It may be slower; it
is not the hot path. `current_line`/`backtrace` continue to work off
`Func.lines`.

### 6.3 Call/return

- `call` uses precomputed `in_offsets`/`in_sizes`, fills the out-ref region from
  the call site, and pushes a frame recording the call site (so `return` needs no
  lookup).
- `return` pops and, for by-ref outputs, copies nothing.

## 7. `.lbc` text format

`.lbc` remains canonical text, LF-only, no BOM, and becomes a **disassembly of
the `u32` stream** produced by `decode.rs`. `Program::to_text` encodes/decodes as
needed; `parse_text` rebuilds the word stream and re-verifies. The format stays
deterministic and the existing `--check` byte comparison is preserved. Snapshots
are regenerated; `docs/VM_SPEC.md` documents the word layout and opcode table.

## 8. Baseline rewrite

`treewalk.rs` (bench-only, `bench-baseline` feature) is rewritten to use
`Vec`-indexed registers/slots and one contiguous memory arena instead of a
per-frame `HashMap` and sparse byte memory. It stays a tree-walker (block
structure, no bytecode), so it is a fair reference. The fidelity test
(conditional-trap checksums) and the ≥3× gate are retained; the ratio is
re-measured after the VM rework.

## 9. Gate and process

- The bench runs in **release**: add
  `cargo test --release -p lilac-vm --features bench-baseline` (and the matching
  clippy) to `AGENTS.md`'s Gates.
- The bench records absolute ns/op per program and adds a **perf budget** check
  with generous headroom (e.g. fib under a release budget) so absolute
  regressions are caught, not only the tree-walk ratio.

## 10. Testing

- **Encoder/decoder round-trip:** `decode(encode(instr)) == instr` for every
  `Instr` variant, including wide operands and multi-word arith/const.
- **Verifier:** malformed streams (bad opcode, out-of-range operand, misaligned
  branch target, bad call-site range) rejected cleanly.
- **Execution parity:** the M5c integration tests (byte buffers, `format_args`,
  structs, drops, panic, extern, indirect calls) still pass on the new stream.
- **Calling convention:** by-ref outputs — a caller's out place is written
  directly by the callee; an unwritten out leaves the caller's storage unchanged
  (LLVM parity). Inputs remain snapshotted.
- **Debugger:** `stepi`/breakpoints/`disasm` over the decoded stream.
- **Benchmark:** release gate with absolute numbers and the budget assertion.
- **No LLVM:** `lilac-vm` remains `inkwell`/`llvm-sys`-free.

## 11. Ports + exit criteria

- Rework `isa.rs`/`compile.rs`; add `encode.rs`/`decode.rs`; rewrite the `exec.rs`
  hot loop; update `codec.rs`/`debug.rs`; rewrite `treewalk.rs`; regenerate
  snapshots; update `VM_SPEC.md`/`AGENTS.md`.
- Exit:
  1. Executed code is a `Vec<u32>` stream; the typed `Instr` is compile-time only.
  2. `fib(18)` in release is **< 0.2 ms** (≈1.2 ns/instruction over ~170k
     instructions); mandelbrot/structs/calls improve proportionally. If the base
     rework falls short, the sanctioned levers are superinstruction fusion and
     redundant-copy elimination in the LIR→bytecode compiler (see §12), applied
     within this milestone rather than deferred.
  3. No runtime `type_size` call remains on the hot path; no copy on return for
     by-ref outputs; the inner loop holds hot state in locals.
  4. All M5c behavior tests pass; the debugger works over the decoded stream;
     `.lbc` snapshots are regenerated and `--check`-stable.
  5. `cargo fmt --check`, `clippy --workspace --all-targets -- -D warnings`,
     `cargo test --workspace`, and the release bench gate are green;
     `unsafe_code = forbid` intact; no new dependency.
  6. `docs/VM_SPEC.md` documents the compact ISA; `AGENTS.md` notes the release
     bench gate.
  7. Human review of this spec and its plan.

## 12. Risks / known divergences

- **Semantic change from by-ref outputs.** An unwritten output now leaves the
  caller's storage unchanged (LLVM parity) instead of the callee's zeroed slot.
  Affected tests are updated; this is a correction, not a regression.
- **Escape-form bugs.** Wide operands are rare; the encoder/decoder round-trip
  tests must cover them explicitly.
- **Density vs decode cost.** For very small programs (fib) the u32 stream may be
  roughly neutral vs a compact enum; the structural win is cache behavior on
  larger programs. The precompute + by-ref + inner-loop changes are the primary
  fib wins.
- **0.2 ms is aggressive.** It is ≈1.2 ns/instruction over fib's ~170k
  instructions, near the limit of a switch interpreter. If the base rework
  (compact stream + precompute + by-ref outputs + inner loop) does not reach it,
  the sanctioned levers — applied within M5c.1 — are: superinstruction fusion
  (e.g. fuse `copy`/`const`/`arith` sequences into one opcode) and
  redundant-copy elimination in the LIR→bytecode compiler (keep values in
  registers instead of round-tripping through slots), which cut the instruction
  count directly. 50 µs remains LuaJIT territory (JIT/AOT) and is out of reach.
- **Snapshot churn.** `.lbc` text changes shape; all VM snapshots regenerate.

## 13. Self-review

- **Placeholders:** none — every type, field, format, and test names its owner;
  the exact opcode table is delegated to `VM_SPEC.md` at implementation, which is
  a concrete deliverable, not a TBD.
- **Consistency:** §1.1's nine decisions match §3–§9; the compact encoding (§3)
  is the execution form the interpreter (§6) decodes; by-ref outputs (§5) match
  the LLVM ABI cited in §1.2; the release gate (§9) matches exit criterion §11.2.
- **Scope:** strictly the VM performance rework — no language or LIR change, no
  LLVM change (the LLVM backend already uses the pointer ABI).
- **Ambiguity:** word layout, escape form, operand-kind encoding, pools,
  precomputed metadata, calling convention, inner-loop/debugger split, baseline,
  and gate are all decided here.
