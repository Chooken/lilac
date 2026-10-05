# Lilac VM Specification (`.lbc` 0.1)

> Version: `lbc = "0.1"`. Target: `lilacvm-unknown-none`. Status: normative for
> the bytecode ISA, the `.lbc` text artifact, the interpreter's memory and
> execution model, and the debugger. `SPEC.md` is normative for language
> semantics and `IR_SPEC.md` for LIR; the M5c design document
> (`docs/superpowers/specs/2026-10-05-lilac-vm-m5c-design.md`) is
> non-normative. The implementation lives in `crates/lilac-vm`.

The Lilac VM is the **second execution backend** for the compiler. It lowers
verified LIR to a register bytecode, serializes it as canonical `.lbc` text, and
interprets that text with no frontend and no LLVM. It is implemented entirely in
safe, std-only Rust (`unsafe_code = "forbid"`).

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
    pub n_regs: u32,           // register-file slots in a frame
    pub slot_bytes: u32,       // per-frame place region, zeroed at entry
    pub code: Vec<Instr>,
    pub lines: Vec<Line>,      // pc -> (file, line, col), sorted by pc
    pub place_names: Vec<(String, u32)>, // %place name -> slot offset (debugger)
}
```

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
  operand copy `size(ty)` bytes through the ref.
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
error** (the runner/CLI prints it and exits 2), never a `bug!`/panic. Since the
verifier does not do control-flow reachability, it also requires every non-extern
function to have a non-empty `code` that **ends in a control-flow terminator**
(`return`, `panic`, `unreachable`, `branch`, `branch.if`, or `branch.if.imm`);
intermediate
fall-through is legal. Together with in-range branch targets, this guarantees
`pc` can never run off the end of a verified body. The register/slot bounds are
checked *before* any frame allocation, so an untrusted artifact cannot request an
arbitrarily large register file or frame.

## 3. Bytecode ISA

`Instr` is decode-once: one variant per LIR operation, with integer operands and
no per-step string decoding. Registers are written `&N`, place slots `%N`,
branches `$L<pc>`, data constants `!cN`, and functions `@name`.

### 3.1 Values, copies, and memory

| Variant | Canonical `.lbc` | Operands | Semantics |
| --- | --- | --- | --- |
| `Const` | `&d: ty = const 0xImm` | `dst`, `imm: u64`, `ty` | Truncate `imm` to `size(ty)` bytes into `&d`. |
| `ConstData` | `&d: ty = const.data !cN` | `dst`, `data`, `ty` | `&d` = `globals[data.global_off]` (a ref). |
| `Copy` | `dst: ty = copy src` | `dst`, `src: Operand`, `ty` | Scalar copy of `size(ty)` bytes; aggregate operands copy bytes through refs. |
| `CopyBytes` | `copy.bytes %d <- %s, size` | `dst`, `src`, `size` | Raw `size`-byte place-to-place copy. |
| `Load` | `&d: ty = load %s` | `dst`, `src`, `ty` | Read `size(ty)` bytes at the ref held in `%s`. An aggregate `ty` passes the ref through instead. |
| `Store` | `store src -> %d : ty` | `src: Operand`, `dst`, `ty` | Write `size(ty)` bytes through the ref held in `%d`. |
| `Addr` | `&d = addr %s` | `dst`, `place` | `&d` = address of frame slot `%s`. |
| `AddrField` | `&d = addr.field %s + off direct\|indirect` | `dst`, `place`, `off`, `indirect` | `&d` = field base + `off` (see indirect note). |
| `AddrFunc` | `&d = addr.func @f` | `dst`, `func` | `&d` = tagged function address. |
| `Field` | `&d: ty = field %s + off direct\|indirect` | `dst`, `place`, `off`, `ty`, `indirect` | Scalar field read at the field base + `off`. An aggregate `ty` yields the field ref. |
| `StoreField` | `store.field src -> %s + off direct\|indirect : ty` | `src`, `place`, `off`, `ty`, `indirect` | Scalar field write at the field base + `off`. |

For `AddrField`/`Field`/`StoreField`, **`indirect`** selects the base. When
`true`, the object slot `%s` holds a `ref T`: the interpreter dereferences the
slot's stored ref and the field base is that pointee. When `false`, the slot
itself is the aggregate and is the base. This mirrors the LLVM backend's
`field_ptr` and makes every dereference explicit, so the interpreter needs no
owner type at run time.

### 3.2 Arithmetic and conversion

| Variant | Canonical `.lbc` | Fields | Semantics |
| --- | --- | --- | --- |
| `Arith` | `&d: ty = op &l, &r : operand_ty` | `op`, `dst`, `lhs`, `rhs`, `operand_ty`, `ty` | Typed arithmetic/comparison (see below). |
| `ArithImm` | `&d: ty = op.imm lhs, 0xImm : operand_ty` | `op`, `dst`, `lhs`, `lhs_slot`, `imm`, `operand_ty` | Fused `copy`+`const`+`arith`: `lhs` is a register or slot, `imm` the right operand. |
| `Convert` | `&d: ty = trunc\|extend.zero &s` | `op`, `dst`, `src`, `ty` | Mask the source to `size(ty)` bytes. |

`ArithImm` is a compiler superinstruction (spec §12): it folds a single-use
`copy %place -> &r` and a `const k -> &j` into the arithmetic that consumes both
(`&d = op %place, k`), cutting the instruction count directly. Its immediate is
already masked to the operand width; `lhs` is a register or frame slot.

**`operand_ty` vs `ty`.** `operand_ty` is the operands' declared type and `ty`
is the result type. They differ for comparisons, whose LIR result is always
`#bit8` regardless of operand width. The interpreter uses `operand_ty` for the
operand width, signedness, and float format (exactly as the LLVM backend reads
the operand register types), and `ty` only for the result width.

Operator mnemonics (`ArithOp`):

- Integer: `i.add`, `i.sub`, `i.mul`, `i.div`, `i.div.u`, `i.mod`, `i.mod.u`.
  Signed `/`/`%` sign-extend their operands at `operand_ty` width; division or
  remainder by zero yields zero. Results are masked to `size(ty)`.
- Float (bit patterns): `f.add`, `f.sub`, `f.mul`, `f.div`, `f.mod`.
  `#bit32` operands are interpreted as `f32`, other widths as `f64`.
- Comparison: `cmp.eql` (bit-equality over `operand_ty` width), `i.cmp.lt`,
  `i.cmp.lt.u`, `i.cmp.gt`, `i.cmp.gt.u`, `f.cmp.lt`, `f.cmp.gt`. Results are
  `0`/`1`.

`ConvertOp` is `trunc` or `extend.zero`; both currently mask the source to the
result width (`value & mask(size(ty))`).

### 3.3 Calls

| Variant | Canonical `.lbc` | Semantics |
| --- | --- | --- |
| `Call` | `call @f (in ...) (out ...)` | Direct call by function index. |
| `CallArithImm` | `call.arith.imm op lhs, 0xImm : ty -> @f (in ...) (out ...)` | Fused `ArithImm` feeding the call's single input register. |
| `CallOut` | `call.out &d, size -> @f (in ...) (out ...)` | Direct call whose single output is copied into `&d` on return. |
| `CallIndirect` | `call.indirect &c (in ...) (out ...)` | Call the function id (from `addr.func`) held in `&c`. |
| `CallBuiltin` | `call.builtin @b (in ...) (out ...)` | Leaf builtin: `ptr_add`, `memcpy`, or `lilac_abort`. |

Inputs are operands (`&reg` or `%slot`); outputs are slots (`%slot`). Calling a
function pushes a frame, allocates `n_regs` registers and `slot_bytes` zeroed
arena bytes (registers are SSA and not re-zeroed), binds the callee's `ins` into
its input slots, and jumps to its entry pc.

**Outputs are by reference (pointer ABI).** The callee's frame begins with an
out-ref region: at `call`, the caller writes the absolute arena offset of each
of its out places there, and the callee writes the caller's storage directly.
`return` therefore copies **nothing** back — an output the callee never writes
leaves the caller's storage unchanged. `CallArithImm` folds the argument
computation into the call. `CallOut` records a deferred copy on the pushed
frame; when the callee returns, the value at the caller's out place is copied
into `&d`, removing the separate read. The injected `.lilac.*` runtime functions
are ordinary functions called through `Call`.

Leaf builtins and their arities:

- `@ptr_add(ref, int) -> ref`: `base + offset` (wrapping).
- `@memcpy(dst, src, len)`: copy `len` bytes within the arena.
- `@lilac_abort()`: trap with `panic: aborted` (see §5).

### 3.4 Control and termination

| Variant | Canonical `.lbc` | Semantics |
| --- | --- | --- |
| `Branch` | `branch $L<n>` | Unconditional jump to pc `n`. |
| `BranchIf` | `branch.if &c, $L<t>, $L<e>` | Jump to `e` when `&c == 0`, otherwise to `t`. |
| `BranchIfArithImm` | `branch.if.imm op lhs, 0xImm : ty, $L<t>, $L<e>` | Fused compare+branch: evaluate `op lhs, Imm` and branch on a non-zero result. |
| `Return` | `return` | Copy outs to the caller's out slots and pop the frame; returning from the entry frame ends the program with exit 0. |
| `Panic` | `panic !cN` | Print the bytes of data constant `!cN` to stderr as `panic: <msg>` and exit 101. |
| `Unreachable` | `unreachable` | VM trap: reaching it is a clean runtime error (`unreachable terminator`), exit 1, never a `bug!`. |

## 4. `.lbc` text format

`.lbc` is canonical, line-oriented, LF-only, and BOM-free (like `.lir`). The
header is versioned:

```text
lbc = "0.1"
target = "lilacvm-unknown-none"
```

The body is a sequence of optional type and data declarations followed by
functions. The output is deterministic — types, data, and functions in index
order; code in pc order — and `Program::parse_text` is the exact inverse of
`Program::to_text`, so `parse_text(to_text(p))` reproduces both the value and
the text.

- **Types**: `type !t<index> = "<name>" size <bytes> ref <0|1>`, one per line,
  indices strictly sequential from 0. `ref` is whether the type's values are
  arena references.
- **Data**: `data !c<index> = "<bytes>" @ 0x<global_off>`, one per line,
  indices sequential from 0. Byte strings escape `\\`, `\"`, `\n`, `\r`, `\t`,
  printable ASCII (`0x20..=0x7e`), and other bytes as `\xNN`.
- **Functions**:

  ```text
  func @<name> (<ins>) -> (<outs>) regs <n_regs> slots <slot_bytes>[ extern]
  ```

  The input list is `()` or `(in t, ...)`; the output list is `()` or
  `(t, ...)`. An `extern` declaration has no body.
- **Places** (optional): `  places %<name>=<offset> ...` on one line, mapping
  original `%place` names to slot offsets for the debugger.
- **Line table** (optional): `  line <pc>:<file>:<line>:<col>`, one per mapped
  pc, sorted ascending. A `file` containing `:` (e.g. a Windows path) is
  rejoined by the parser.
- **Labels**: `  $L<pc>:` is printed for pc 0 and for every branch/branch-if
  target.
- **Code**: `    <pc>  <instruction>`, one per instruction, pcs ascending from
  0. Every instruction prints its resolved operands. Branch targets and function
  references are written symbolically (`$L<pc>`, `@name`) and resolved by the
  parser.

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
  line 1:../../tests/ui/vm_hello.lilac:2:34
  line 2:../../tests/ui/vm_hello.lilac:2:22
  line 3:../../tests/ui/vm_hello.lilac:2:5
  $L0:
    0  &0: #bit_native = const 0x1
    1  &1: #bit_native = const 0x2
    2  &2: #bit_native = const 0x3
    3  %0: #bit_native = copy &2
    4  return
```

A larger artifact shows inputs, a reference type, a direct call, and field
indirection (`line` file paths abbreviated to `demo.lilac`):

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

func @Pair.get (in ref #Pair) -> (#bit8) regs 2 slots 9
  places %instance=0 %result=8
  line 0:demo.lilac:5:28
  line 1:demo.lilac:5:18
  line 2:demo.lilac:5:9
  $L0:
    0  &0: #bit64 = field %0 + 8 indirect
    1  &1: #bit8 = trunc &0
    2  %8: #bit8 = copy &1
    3  return

func @main () -> () regs 3 slots 42
  places %p=0 %tmp.1=16 %r=32 %tmp.2=33 %tmp.3=34
  line 0:demo.lilac:10:13
  line 1:demo.lilac:10:9
  line 2:demo.lilac:11:13
  line 3:demo.lilac:11:9
  line 4:demo.lilac:9:5
  line 5:demo.lilac:13:16
  line 6:demo.lilac:13:16
  line 7:demo.lilac:13:16
  line 8:demo.lilac:13:5
  $L0:
    0  &0: #bit64 = const 0x1
    1  store.field &0 -> %16 + 0 direct : #bit64
    2  &1: #bit64 = const 0x2
    3  store.field &1 -> %16 + 8 direct : #bit64
    4  copy.bytes %0 <- %16, 16
    5  &2 = addr %0
    6  %34: ref #Pair = copy &2
    7  call @Pair.get (in %34) (out %33)
    8  copy.bytes %32 <- %33, 1
    9  return
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
still works at instruction granularity. The REPL prompt is `(lilac-vm) `.

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
