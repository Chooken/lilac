# Lilac VM Specification (`.lbc` 0.1)

> Version: `lbc = "0.1"`. Target: `lilacvm-unknown-none`. Status: normative for
> the bytecode ISA, the `.lbc` text artifact, the interpreter's memory and
> execution model, and the debugger. `SPEC.md` is normative for language
> semantics and `IR_SPEC.md` for LIR; the M5c design
> (`docs/superpowers/specs/2026-10-05-lilac-vm-m5c-design.md`) and the M5c.1
> performance design
> (`docs/superpowers/specs/2026-10-05-lilac-vm-m5c1-performance-design.md`) are
> non-normative. The implementation lives in `crates/lilac-vm`.

The Lilac VM is the **second execution backend** for the compiler. It lowers
verified LIR to a compact **register bytecode** — an executed `Vec<u32>` word
stream (§3) — serializes it as canonical `.lbc` text, and interprets that text
with no frontend and no LLVM. The typed `Instr` enum survives only as the
compiler's intermediate representation (compile time); it is never the execution
form. The VM is implemented entirely in safe, std-only Rust
(`unsafe_code = "forbid"`).

## 1. Target and widths

The canonical VM target triple is **`lilacvm-unknown-none`** (architecture
`lilacvm`, vendor `unknown`, OS `none`). `Arch::LilacVm` is 64-bit:
`Arch::is_64_bit` returns `true`, so the single pointer-width rule in
`lilac-target` treats the VM like the other 64-bit targets. All VM pointers
(`ref` values) are 64-bit and `#bit_native` is 8 bytes.

Selecting this target selects the bytecode backend:

- `lilac emit-vm <file>` compiles to `lilacvm-unknown-none` and prints canonical
  `.lbc` text (`--check` compares against `<file>.lbc`).
- `lilac run <file>` compiles, emits `.lbc` in memory, and executes it.
- `lilac disasm <file.lbc>` reads an existing artifact, parses/verifies it, and
  prints its canonical disassembly. An unreadable file or a parse/verify failure
  is a clean data error (exit 2).
- `lilac emit-llvm --target=lilacvm-unknown-none` is a usage error (exit 2);
  `emit-vm`/`run` with any non-VM target is likewise exit 2.

Packed type sizes are used throughout (alignment and padding are ignored; the VM
only shares memory with itself):

| Type | Size (bytes) | Notes |
| --- | --- | --- |
| `#bit8` / `#bit16` / `#bit32` / `#bit64` | 1 / 2 / 4 / 8 | Raw containers. |
| `#bit_native` | 8 | 64-bit on the VM. |
| `ref T` / `#ptr` | 8 | Arena byte offset. |
| `#byte_buffer` | 16 | `{ len: #bit_native, data: ref #bit8 }`. |
| `#format_args` | 264 | `count: #bit_native` + 16 `#byte_buffer` pieces. Piece stride is 16 (`2 * ptr_size`). |
| named struct/enum layout | sum of packed field sizes | Field order from `LirModule.layouts` / builtin layouts. |

The `#byte_buffer` size (16) and the `format_args` piece stride (16) are pinned
by the injected runtime in `lilac-ir::runtime` and by a VM sizing test.

## 2. Machine model

A compiled program is a `Program`:

```rust
pub struct Program {
    pub version: String,       // "0.1"
    pub target: String,        // "lilacvm-unknown-none"
    pub types: Vec<TypeInfo>,  // { name, size, is_ref } by TypeId
    pub data: Vec<Data>,       // { bytes, global_off }
    pub globals: Vec<u8>,      // concatenated data bytes (ref targets)
    pub funcs: Vec<Func>,
}

pub struct Func {
    pub name: String,
    pub is_extern: bool,
    pub ins: Vec<TypeId>,
    pub outs: Vec<TypeId>,
    pub in_offsets: Vec<u32>,      // input byte offset within the input region
    pub in_sizes: Vec<u32>,        // input byte size (parallel to in_offsets)
    pub out_offset: u32,           // byte offset just past the input region
    pub out_sizes: Vec<u32>,       // output byte sizes (parallel to outs)
    pub n_regs: u32,               // register-file slots in a frame
    pub slot_bytes: u32,           // per-frame place region, zeroed at entry
    pub arg_reg: Option<u32>,      // register-ABI input register (§3.5)
    pub ret_reg: Option<u32>,      // register-ABI return register (§3.5)
    pub code: Vec<u32>,            // executed compact word stream; pc is a word index
    pub call_sites: Vec<CallSite>, // pool referenced by call instructions
    pub operands: Vec<Operand>,    // operand pool sliced by each call site
    pub lines: Vec<Line>,          // word pc -> (file, line, col), sorted by pc
    pub place_names: Vec<(String, u32)>, // %place name -> slot offset (debugger)
}
```

`in_sizes`/`out_sizes` are `u32` (not `u8`) because runtime values such as
`#format_args` are 264 bytes. `CallSite` is
`{ func: u32, ins: Range<u32>, outs: Range<u32> }`: the ranges index
`Func::operands`, and every output operand is stored as `Operand::Slot`.
`arg_reg`/`ret_reg` are chosen by the compiler (or read from the `.lbc` header);
`Program::rebuild_layouts` consumes them and the type table to precompute the
layout fields (`in_offsets`, `in_sizes`, `out_offset`, `out_sizes`) that `call`
and `return` read, so neither performs a runtime `type_size` lookup.

`Machine` holds three contiguous buffers: a `Vec<u64>` register file (all
frames), a `Vec<u8>` **arena** (globals followed by each live frame's slot
region), and a call stack of frames.

- **Registers.** A register is a raw `u64`. Indices are dense `u32` values
  assigned per function; LIR registers are block-local SSA, so the same name in
  two blocks maps to distinct VM registers. Because they are SSA (written before
  read), the register file is **not** re-zeroed per call: it is kept at its
  high-water length and a frame's registers may reuse stale bits. A logical
  `reg_top` tracks the live extent.
- **Places.** A place is a byte offset within a frame's slot region. Slots are
  packed in declaration order (`ins`, then `outs`, then `alloca`s in program
  order). The whole region is zeroed when the frame is entered, so `alloca` and
  `drop` emit **no instructions**; universal zero-initialization is the VM's
  default. `drop` is a marker whose destructor calls already precede it.
- **Frame reclamation.** The register file and arena are kept at high-water
  length (a logical `slot_top` tracks the live extent). On `return`, the popped
  frame's region is reclaimed by moving `reg_top`/`slot_top` back to the
  caller's bases; the next call at the same depth reuses the same storage, so a
  sequential call loop does not grow the arena without bound. Only the callee's
  slot region is re-zeroed on entry (registers need no init).
- **References.** A `ref` is a `u64` byte offset into the arena. The arena
  begins with `Program::globals`, so a data constant's `global_off` is directly a
  valid ref. Scalar loads/stores and `@memcpy` read and write little-endian
  bytes. Out-of-range accesses are reported as a VM trap, never a panic.
- **Aggregates.** A value whose packed size exceeds 8 bytes cannot sit in one
  register, so an aggregate-typed register holds a ref to its bytes; values up to
  8 bytes are held inline. `copy`/`store`/`store.field` of an aggregate register
  operand copy the operand's byte size through the ref.
- **Function values.** `addr.func` stores `FUNC_TAG | func_id` (the high bit
  tags a function address, which cannot collide with an arena offset).
  `call.indirect` decodes it.
- **Stack depth.** The call stack is capped at `MAX_CALL_DEPTH = 4096` frames;
  exceeding it is a clean `stack overflow` trap.

The program is verified by the VM's own verifier before it is serialized or run:
version and target, unique type/function names, the globals arena matching the
data constants, per-function bounds (`n_regs <= MAX_REGS = 1048576`,
`slot_bytes <= MAX_FRAME_BYTES = 67108864`), every instruction's register/slot
bounds, branch-target bounds, type/data/function indices, and call arity.

For a body loaded from an untrusted `.lbc`, a verifier failure is a **clean data
error** (the runner/CLI prints it and exits 2), never a `bug!`/panic. The verifier
decodes the word stream linearly to establish the set of valid instruction
boundaries; every instruction is checked, and every branch target must land on
one of those boundaries (so a branch cannot jump into the middle of a multi-word
instruction). Since the verifier does not do control-flow reachability, it also
requires every non-extern function to have a non-empty `code` that **ends in a
control-flow terminator** (`return`, `copy.ret`, `arith.ret`, `panic`,
`unreachable`, `branch`, `branch.if`, or `branch.if.imm`); intermediate
fall-through is legal. Together with in-range branch targets, this guarantees
`pc` can never run off the end of a verified body. The register/slot bounds are
checked *before* any frame allocation, so an untrusted artifact cannot request an
arbitrarily large register file or frame.

## 3. Bytecode ISA

The executed form is a `Vec<u32>` **word stream** (`Func::code`). The typed
`Instr` enum is compile-time IR only: `encode.rs` lowers it to words, `decode.rs`
provides a borrowed `Decoded` view, and the interpreter, verifier, and
disassembler all consume the words. `pc` is a **word index**, and branch targets
are word offsets. In `.lbc` text, registers are written `&N`, place slots `%N`,
branches `$L<pc>`, data constants `!cN`, and functions `@name`.

### 3.1 Word layout

Word 0 of every instruction carries the opcode in its low byte and, in the narrow
form, up to three byte operands:

```text
word0 = [ op:8 | a:8 | b:8 | c:8 ]        op = w & 0xFF
```

- **Narrow form.** One word. `a`/`b`/`c` are 8-bit register indices or slot byte
  offsets, so every operand must be `< 256`.
- **Wide escape.** If any operand is `>= 256` the instruction uses the wide form:
  bit 7 of the opcode byte is set (`op | 0x80`; base opcode `op & 0x7F`) and
  extension words follow:
  - `ext0 = a:u16 | (b:u16 << 16)` (both operands must fit `u16`);
  - `ext1 = c:u32`, present only when there is a third wide operand.
  A wide instruction is therefore 2 words with one or two operands and 3 words
  with three.
- **24-bit fields.** A few opcodes carry a single 24-bit field in `w >> 8` (max
  `0xFF_FFFF`) instead of `a`/`b`/`c`: `panic` (data index), `branch` (word
  target), and `call`/`builtin.*` (call-site index). These stay one word.
- **Multi-word forms.** Immediates and pool indices that do not fit the opcode
  word are appended as extra words: `const` appends the little-endian low and
  high halves of its `u64`; `const.data` appends the data index; `field` and
  `store.field` append the byte `size`; `call.indirect` appends the call-site
  index; the fused forms below have fixed layouts.

Operand **kind** (register vs place slot) is baked into the opcode for
`copy`/`store`/`store.field` and into a flag byte for the fused forms; call
outputs are always slots. No per-operand tag bit is needed, so a narrow operand
uses a full byte.

### 3.2 Opcode table

All base opcodes are `< 128`; bit 7 is reserved for the wide escape. `N` is the
instruction's word count (narrow; wide adds one or two words). Recall that
`.lbc` arithmetic text prints the operand-width type for both operands and the
result (the stream bakes only the operand width).

**Core, memory, and calls.**

| Opcodes | Canonical `.lbc` | Fields | N |
| --- | --- | --- | --- |
| `0x01` | `return` | — | 1 |
| `0x02` | `unreachable` | — | 1 |
| `0x03` | `panic !cN` | `data:24` | 1 |
| `0x04` | `branch $L<n>` | `target:24` | 1 |
| `0x05` | `branch.if &c, $L<t>, $L<e>` | `cond, then, els` | 1 |
| `0x06` | `&d: ty = const 0xImm` | `dst` + `imm_lo`, `imm_hi` | 3 |
| `0x07` | `&d: ty = const.data !cN` | `dst` + `data` word | 2 |
| `0x08` | `dst: ty = copy src` (reg→reg) | `dst, src, size` | 1 |
| `0x09` | `dst: ty = copy src` (reg←slot) | `dst, src, size` | 1 |
| `0x0A` | `dst: ty = copy src` (slot←reg) | `dst, src, size` | 1 |
| `0x0B` | `dst: ty = copy src` (slot→slot) | `dst, src, size` | 1 |
| `0x0C` | `copy.bytes %d <- %s, size` | `dst, src, size` | 1 |
| `0x0D` | `&d: ty = load %s` | `dst, src, size` | 1 |
| `0x0E` | `store src -> %d : ty` (reg source) | `src, dst, size` | 1 |
| `0x0F` | `store src -> %d : ty` (slot source) | `src, dst, size` | 1 |
| `0x10` | `&d = addr %s` | `dst, place` | 1 |
| `0x11` | `&d = addr.field %s + off direct` | `dst, place, off` | 1 |
| `0x12` | `&d = addr.field %s + off indirect` | `dst, place, off` | 1 |
| `0x13` | `&d = addr.func @f` | `dst, func` | 1 |
| `0x14` | `&d: ty = field %s + off direct` | `dst, place, off` + `size` | 2 |
| `0x15` | `&d: ty = field %s + off indirect` | `dst, place, off` + `size` | 2 |
| `0x16` | `store.field src -> %s + off direct : ty` (reg) | `src, place, off` + `size` | 2 |
| `0x17` | `store.field src -> %s + off direct : ty` (slot) | `src, place, off` + `size` | 2 |
| `0x18` | `store.field src -> %s + off indirect : ty` (reg) | `src, place, off` + `size` | 2 |
| `0x19` | `store.field src -> %s + off indirect : ty` (slot) | `src, place, off` + `size` | 2 |
| `0x1A` | `call @f (in ...) (out ...)` | `call_site:24` | 1 |
| `0x1B` | `call.indirect &c (in ...) (out ...)` | `callee` + `call_site` word | 2 |
| `0x1C` | `call.builtin @ptr_add (in ...) (out ...)` | `call_site:24` | 1 |
| `0x1D` | `call.builtin @memcpy (in ...) (out ...)` | `call_site:24` | 1 |
| `0x1E` | `call.builtin @lilac_abort (in ...) (out ...)` | `call_site:24` | 1 |

**Arithmetic and conversion** (width-specialized; one word each). Integer blocks
encode width classes `8/16/32/64` as indices `0/1/2/3`; float blocks encode
`32/64` as `0/1`:

| Opcodes | `.lbc` op | Fields | N |
| --- | --- | --- | --- |
| `0x20–0x23` | `i.div.u` | `dst, lhs, rhs` | 1 |
| `0x24–0x27` | `i.mod.u` | `dst, lhs, rhs` | 1 |
| `0x28–0x2B` | `cmp.eql` | `dst, lhs, rhs` | 1 |
| `0x2C–0x2F` | `i.cmp.lt.u` | `dst, lhs, rhs` | 1 |
| `0x30–0x33` | `i.cmp.gt.u` | `dst, lhs, rhs` | 1 |
| `0x40–0x43` | `i.add` | `dst, lhs, rhs` | 1 |
| `0x44–0x47` | `i.sub` | `dst, lhs, rhs` | 1 |
| `0x48–0x4B` | `i.mul` | `dst, lhs, rhs` | 1 |
| `0x4C–0x4F` | `i.div` | `dst, lhs, rhs` | 1 |
| `0x50–0x53` | `i.mod` | `dst, lhs, rhs` | 1 |
| `0x54–0x57` | `i.cmp.lt` | `dst, lhs, rhs` | 1 |
| `0x58–0x5B` | `i.cmp.gt` | `dst, lhs, rhs` | 1 |
| `0x5C–0x5F` | `trunc` | `dst, src` | 1 |
| `0x60–0x61` | `f.add` | `dst, lhs, rhs` | 1 |
| `0x62–0x63` | `f.sub` | `dst, lhs, rhs` | 1 |
| `0x64–0x65` | `f.mul` | `dst, lhs, rhs` | 1 |
| `0x66–0x67` | `f.div` | `dst, lhs, rhs` | 1 |
| `0x68–0x69` | `f.mod` | `dst, lhs, rhs` | 1 |
| `0x6A–0x6B` | `f.cmp.lt` | `dst, lhs, rhs` | 1 |
| `0x6C–0x6D` | `f.cmp.gt` | `dst, lhs, rhs` | 1 |
| `0x73` | `extend.zero` | `dst, src` | 1 |

**Superinstructions** (compiler fusion; spec §12). The fused arithmetic forms'
`flags`/`opcode` bytes hold a width-specialized arithmetic opcode (`< 0x80`) plus,
in bit 7, whether `lhs` is a slot offset:

| Opcodes | Canonical `.lbc` | Layout (word 0 then extra words) | N |
| --- | --- | --- | --- |
| `0x34` | `&d: ty = op.imm lhs, 0xImm : ty` | `[op][dst][lhs][flags]`, `imm_lo`, `imm_hi` | 3 |
| `0x35` | `branch.if.imm op lhs, 0xImm : ty, $L<t>, $L<e>` | `[op][lhs][flags][opcode]`, `then`, `els`, `imm_lo`, `imm_hi` | 5 |
| `0x36` | `call.arith.imm op lhs, 0xImm : ty -> @f (in ...) (out ...)` | `[op][lhs][flags][opcode]`, `call_site`, `imm_lo`, `imm_hi` | 4 |
| `0x37` | `call.out &d, size -> @f (in ...) (out ...)` | `[op][dst][size][0]`, `call_site` | 2 |
| `0x38` | `call.arith.out op lhs, 0xImm : ty, &d, size -> @f (in ...) (out ...)` | `[op][lhs][flags][opcode]`, `call_site`, `[dst][size]`, `imm_lo`, `imm_hi` | 5 |
| `0x39` | `copy.ret src, ty` | `[op][src][packed]` (`packed = kind*0x80 + size`) | 1 |
| `0x3A` | `arith.ret op &l, &r : ty` | `[op][lhs][rhs][opcode]` | 1 |

`arith.imm` (opcode `0x34`) folds a single-use `copy %place -> &r` and
`const k -> &j` into the arithmetic consuming both (`&d = op %place, k`); its
immediate is already masked to the operand width. `branch.if.imm` fuses a
comparison whose result is read only by the next branch.
`call.arith.imm`/`call.arith.out` fold the argument computation into a call;
`call.out`/`call.arith.out` additionally copy the callee's output into `&d` on
return. `copy.ret`/`arith.ret` fold the final value computation into the
`return` (the callee must use the register-return ABI). `copy.ret`'s `size`
must fit 7 bits.

### 3.3 Operands and pools

- A **register** operand is a dense `u32` index into the function's frame
  register file; a **place** operand is a `u32` byte offset into the frame's slot
  region. `decode` reattaches the kind from the opcode/flags; call inputs may be
  either kind, but call outputs are always slots.
- **Constants** live in `Program::data` (raw bytes) mapped into `Program::globals`;
  `const.data` and `panic` reference a data index.
- **Call sites.** Each call appends a `CallSite { func, ins: Range<u32>,
  outs: Range<u32> }` to the function's `call_sites` pool and its operands to the
  shared `operands` pool; the instruction carries the site index. `call` and the
  `builtin.*` forms carry a 24-bit index; the multi-word `call.indirect`/fused
  forms carry a full word. No `Vec` is present in the executed stream.

### 3.4 Values, copies, and memory

| Canonical `.lbc` | Semantics |
| --- | --- |
| `&d: ty = const 0xImm` | Truncate `imm` to the type's byte size into `&d`. |
| `&d: ty = const.data !cN` | `&d` = the arena offset of data constant `!cN` (a ref). |
| `dst: ty = copy src` | Scalar copy of the type's byte size; aggregate operands copy bytes through refs. |
| `copy.bytes %d <- %s, size` | Raw `size`-byte place-to-place copy. |
| `&d: ty = load %s` | Read the type's byte size at the ref held in `%s`. An aggregate type passes the ref through instead. |
| `store src -> %d : ty` | Write the type's byte size through the ref held in `%d`. |
| `&d = addr %s` | `&d` = address of frame slot `%s`. |
| `&d = addr.field %s + off direct\|indirect` | `&d` = field base + `off` (see indirect note). |
| `&d = addr.func @f` | `&d` = tagged function address (`FUNC_TAG | func_id`). |
| `&d: ty = field %s + off direct\|indirect` | Scalar field read at the field base + `off`. An aggregate type yields the field ref. |
| `store.field src -> %s + off direct\|indirect : ty` | Scalar field write at the field base + `off`. |

For `addr.field`/`field`/`store.field`, **`indirect`** selects the base. When
`indirect`, the object slot `%s` holds a `ref T`: the interpreter dereferences the
slot's stored ref and the field base is that pointee. Otherwise the slot itself is
the aggregate and is the base. This mirrors the LLVM backend's `field_ptr` and
makes every dereference explicit, so the interpreter needs no owner type at run
time.

### 3.5 Arithmetic, conversion, and calls

`arith`/`arith.imm`/`arith.ret`/`branch.if.imm`/`cmp` operator mnemonics:

- Integer: `i.add`, `i.sub`, `i.mul`, `i.div`, `i.div.u`, `i.mod`, `i.mod.u`.
  Signed `/`/`%` sign-extend their operands at the encoded width; division or
  remainder by zero yields zero. Results are masked to the width.
- Float (bit patterns): `f.add`, `f.sub`, `f.mul`, `f.div`, `f.mod`. Width `32`
  operands are interpreted as `f32`, width `64` as `f64`.
- Comparison: `cmp.eql` (bit-equality), `i.cmp.lt`, `i.cmp.lt.u`, `i.cmp.gt`,
  `i.cmp.gt.u`, `f.cmp.lt`, `f.cmp.gt`. Results are `0`/`1`.

`trunc` masks its source to the encoded result width; `extend.zero` is width-free
(masking to the wider type is a no-op). Width is encoded only where semantics
require it: signed div/mod and signed compares (sign extension), float ops
(f32/f64), integer `i.add`/`i.sub`/`i.mul` result masking, `trunc`, and the
unsigned ops `cmp.eql`/`i.cmp.lt.u`/`i.cmp.gt.u`/`i.div.u`/`i.mod.u` (which mask
their operands to the declared width).

**Calling.** Inputs are copied into the callee frame; outputs are placed as
described below. Calling a function pushes a frame, allocates `n_regs` registers
and `slot_bytes` zeroed arena bytes (registers are SSA and not re-zeroed), binds
the callee's inputs, and jumps to its entry `pc`.

- **Register ABI (eligible small values).** A function is marked `argreg A` when
  it has exactly one input whose type is a non-`ref` type of at most 8 bytes (a
  scalar, including an inline small aggregate) and that input place is never
  address-taken and never passed as a call output. `call` then writes the input
  value into the callee's register `A` (bypassing the input slot). A function is
  marked `retreg R` when it has exactly one output whose type is a non-`ref` type
  of at most 8 bytes, that output place is never address-taken, never read, and
  never passed as a call output, and it is definitely assigned on every returning
  path. Its result lives in register `R` and `return` yields it; a
  register-return callee has **no out-ref region**.
- **Pointer ABI (otherwise).** The callee frame begins with an **out-ref region**
  of one `u64` per output: at `call`, the caller writes the absolute arena offset
  of each of its out places there, and the callee writes the caller's storage
  directly. `return` then copies **nothing** back — an output the callee never
  writes leaves the caller's storage unchanged (matching the LLVM backend). This
  is the fallback for any `ref` or wider-than-8-byte (out-of-line) input/output,
  or an address-taken/read output.
- The `call.out`/`call.arith.out` forms record a deferred copy on the pushed
  frame; when the callee returns, `&d` is set from the return register
  (register-return callee) or from the caller's out place (by-ref). The injected
  `.lilac.*` runtime functions are ordinary functions called through `call`.

Leaf builtins and their arities:

- `@ptr_add(ref, int) -> ref`: `base + offset` (wrapping).
- `@memcpy(dst, src, len)`: copy `len` bytes within the arena.
- `@lilac_abort()`: trap with `panic: aborted` (see §5).

### 3.6 Control and termination

| Canonical `.lbc` | Semantics |
| --- | --- |
| `branch $L<n>` | Unconditional jump to word pc `n`. |
| `branch.if &c, $L<t>, $L<e>` | Jump to `e` when `&c == 0`, otherwise to `t`. |
| `branch.if.imm op lhs, 0xImm : ty, $L<t>, $L<e>` | Fused compare+branch: evaluate `op lhs, Imm` and branch on a non-zero result. |
| `return` | Pop the frame; a register-return callee yields its return register, a by-ref callee copies nothing. Returning from the entry frame ends the program with exit 0. |
| `copy.ret src, ty` | Copy `src` into the return register and return (return fusion). |
| `arith.ret op &l, &r : ty` | Compute `op &l, &r` into the return register and return. |
| `panic !cN` | Print the bytes of data constant `!cN` to stderr as `panic: <msg>` and exit 101. |
| `unreachable` | VM trap: reaching it is a clean runtime error, exit 1, never a `bug!`. |

## 4. `.lbc` text format

`.lbc` is canonical, line-oriented, LF-only, and BOM-free (like `.lir`). The
header is versioned:

```text
lbc = "0.1"
target = "lilacvm-unknown-none"
```

The body is a sequence of optional type and data declarations followed by
functions. The output is deterministic — types, data, and functions in index
order; code in word-pc order — and `Program::parse_text` is the exact inverse of
`Program::to_text`, so `parse_text(to_text(p))` reproduces both the value and
the text. The text is a **disassembly of the `u32` stream**: every `<pc>` is a
word offset, and `parse_text` rebuilds the word stream (re-verifying it).

- **Types**: `type !t<index> = "<name>" size <bytes> ref <0|1>`, one per line,
  indices strictly sequential from 0. `ref` is whether the type's values are
  arena references.
- **Data**: `data !c<index> = "<bytes>" @ 0x<global_off>`, one per line,
  indices sequential from 0. Byte strings escape `\\`, `\"`, `\n`, `\r`, `\t`,
  printable ASCII (`0x20..=0x7e`), and other bytes as `\xNN`.
- **Functions**:

  ```text
  func @<name> (<ins>) -> (<outs>) regs <n_regs> slots <slot_bytes>[ argreg A][ retreg R][ extern]
  ```

  The input list is `()` or `(in t, ...)`; the output list is `()` or
  `(t, ...)`. `argreg`/`retreg` appear for the register ABI (see §3.5); an
  `extern` declaration has no body.
- **Places** (optional): `  places %<name>=<offset> ...` on one line, mapping
  original `%place` names to slot offsets for the debugger.
- **Line table** (optional): `  line <pc>:<file>:<line>:<col>`, one per mapped
  pc, sorted ascending. A `file` containing `:` (e.g. a Windows path) is
  rejoined by the parser.
- **Labels**: `  $L<pc>:` is printed for word pc 0 and for every branch/branch-if
  target.
- **Code**: `    <pc>  <instruction>`, one per instruction, word pcs ascending
  from 0. Every instruction prints its resolved operands. Branch targets and
  function references are written symbolically (`$L<pc>`, `@name`) and resolved
  by the parser. Because the stream bakes sizes rather than type ids, the
  disassembler names a size by the first type table entry of that size (or a
  synthetic `#bitN` spelling), so e.g. an 8-byte `#bit64` value can print as
  another 8-byte type.

The runner rejects an unsupported `lbc` version, a non-VM `target`, malformed
text, an empty body, a body that does not end in a terminator, or any verifier
bound violation with a clear message and exit 2.

### 4.1 Example

The canonical artifact for `tests/ui/vm_hello.lilac`
(`x: @bit_native = @i_add(0x1, 0x2)`) is `tests/ui/vm_hello.lbc`:

```text
lbc = "0.1"
target = "lilacvm-unknown-none"

type !t0 = "#bit_native" size 8 ref 0

func @main () -> () regs 3 slots 8
  places %x=0
  line 0:../../tests/ui/vm_hello.lilac:2:29
  line 3:../../tests/ui/vm_hello.lilac:2:34
  line 6:../../tests/ui/vm_hello.lilac:2:22
  line 9:../../tests/ui/vm_hello.lilac:2:5
  $L0:
    0  &0: #bit_native = const 0x1
    3  &1: #bit_native = const 0x2
    6  &2: #bit_native = const 0x3
    9  %0: #bit_native = copy &2
    10  return
```

A larger artifact shows inputs, a reference type, a direct call, and field
indirection. Note the `retreg 0` function header (the register-return ABI) and
the word pcs: each instruction occupies as many words as its form requires
(`const` is three, `store.field` is two, the rest are one). Line file paths are
abbreviated to `demo.lilac`.

```lilac
Pair: object {
    a: @bit64
    b: @bit64
    get: function (instance: ref Pair) result: @bit8 {
        result = @truncate(instance.b)
    }
}
main: function () nothing {
    p: Pair = Pair {
        a = 0x1
        b = 0x2
    }
    r: @bit8 = p.get()
}
```

```text
lbc = "0.1"
target = "lilacvm-unknown-none"

type !t0 = "ref #Pair" size 8 ref 1
type !t1 = "#bit8" size 1 ref 0
type !t2 = "#bit64" size 8 ref 0

func @Pair.get (in ref #Pair) -> (#bit8) regs 3 slots 8 retreg 0
  places %instance=0
  line 0:demo.lilac:5:28
  line 2:demo.lilac:5:18
  line 2:demo.lilac:5:9
  $L0:
    0  &1: ref #Pair = field %0 + 8 indirect
    2  &0: #bit8 = trunc &1
    3  return

func @main () -> () regs 3 slots 42
  places %p=0 %tmp.1=16 %r=32 %tmp.2=33 %tmp.3=34
  line 0:demo.lilac:10:13
  line 3:demo.lilac:10:9
  line 5:demo.lilac:11:13
  line 8:demo.lilac:11:9
  line 10:demo.lilac:9:5
  line 11:demo.lilac:13:16
  line 12:demo.lilac:13:16
  line 12:demo.lilac:13:16
  line 13:demo.lilac:13:5
  $L0:
    0  &0: ref #Pair = const 0x1
    3  store.field &0 -> %16 + 0 direct : ref #Pair
    5  &1: ref #Pair = const 0x2
    8  store.field &1 -> %16 + 8 direct : ref #Pair
    10  copy.bytes %0 <- %16, 16
    11  &2 = addr %0
    12  call @Pair.get (in &2) (out %33)
    13  copy.bytes %32 <- %33, 1
    14  return
```

## 5. Entry, exit, panic, and `extern`

- **Entry.** The program enters at `main`. The CLI `lilac run` honors a single
  `--export=<name>`: with exactly one export root, that function is the VM entry
  point; with zero or multiple exports it falls back to `main`. The standalone
  `lilac-vm` runner always enters at `main`. No matching entry function is a
  clean `no entry point \`<name>\`` error (exit 1), never a panic, where
  `<name>` is `main` by default or the single `--export` name when one is given.
- **Normal exit.** Returning from the entry frame reports exit code 0.
- **Panic.** The `panic` terminator prints the referenced data constant's bytes
  to stderr as `panic: <message>` and exits **101**. `@lilac_abort` (used by the
  runtime for out-of-capacity or out-of-bounds conditions) prints
  `panic: aborted` and exits 101. This is more informative than the LLVM
  backend's silent trap; the divergence is intentional.
- **Runtime errors.** Other VM failures (`no entry point`, calling an `extern`,
  an invalid arena reference, a `pc` out of range, reaching an `unreachable`
  terminator, or stack overflow) print a message to stderr and exit 1 under the
  runner/CLI.
- **`extern`.** A call to an `extern` (bodyless) function is a clean runtime
  error (`cannot call extern function \`name\``), exit 1 — foreign C calls are a
  non-goal for the VM, matching the M5c design. Declaring an `extern` targeting
  the entry function name is likewise a clean error.

## 6. Runner and debugger

The standalone runner:

```text
lilac-vm <file.lbc> [--source=<path>] [--break=<loc>]... [--step] [--trace]
```

Without any debug flag the program runs to completion and the process exits
with the program's code. `--break=<fn>` or `--break=<file>:<line>` (repeatable),
`--step`, `--trace`, or `--source=<path>` enter the interactive debugger.
(`--trace` is accepted and enters the debugger; it does not currently emit a
separate trace stream.) A bad version, non-VM target, malformed artifact, or
unresolvable `--break` location exits 2; other load/runtime failures exit 1. At
end of input the debugger resumes the program to completion, so a non-interactive
invocation still terminates with the program's code.

Source text is loaded from the line table's recorded path, overridden by
`--source`; when unavailable, `list` falls back to disassembly and stepping
still works at instruction granularity. The REPL prompt is `(lilac-vm) `. The
debugger works over the decoded word stream: `stepi` executes exactly one
decoded instruction, and `disasm` renders the canonical disassembly window
around the current word `pc`.

REPL commands (whitespace-split; aliases in parentheses):

| Command | Effect |
| --- | --- |
| `break <fn>` / `break <file>:<line>` (`b`) | Set a breakpoint (function entry, or a source line). |
| `delete <n>` (`d`) | Remove breakpoint number `n` (1-based). |
| `clear` | Remove all breakpoints. |
| `run` (`r`) / `continue` (`c`) | Resume until a breakpoint hits or the program halts. |
| `step` (`s`) | Run to the next source line, entering calls. |
| `stepi` (`si`) | Execute exactly one bytecode instruction. |
| `next` (`n`) | Run to the next source line, stepping over calls. |
| `finish` (`fin`) | Run until the current frame returns to its caller. |
| `print <reg>` / `print %<place>` (`p`) | Render a register or a named place in the current frame. |
| `info regs` / `info stack` / `info funcs` / `info breaks` (`i`) | State dumps (`registers`/`functions`/`breakpoints` are accepted aliases). |
| `backtrace` (`bt`, `where`) | Call stack, one line per frame with `(file:line, function)`. |
| `disasm [pc]` (`dis`) | Canonical disassembly window around `pc` (±3/+4 instruction window). |
| `list [file:line]` / `list <line>` (`l`) | Source lines around the current location (or the requested line). |
| `quit` (`q`, `exit`) | Exit the REPL. |

A function breakpoint fires on every entry (including recursive re-entry); a
line breakpoint fires on the line's entry instruction each time execution
reaches it. Stepping reports the source line of the next instruction to execute,
while `backtrace` and `print` read the current frame's state.
