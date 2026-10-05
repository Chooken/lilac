//! Bytecode interpreter core.
//!
//! [`Machine`] owns a program reference, a contiguous register file, one byte
//! arena for globals and per-frame slot regions, and a call stack. [`Machine::step`]
//! decodes and executes one instruction; [`Machine::run`] loops until the entry
//! frame returns.
//!
//! # Memory
//!
//! A `ref` is a `u64` byte offset into [`Machine`]'s arena. The arena starts
//! with [`Program::globals`], so a data constant's `global_off` is directly a
//! valid ref. Each frame's place region is appended (zeroed) on call and
//! reclaimed on return, so the arena and register file stay bounded by the live
//! call depth. Out-of-range accesses are reported as [`VmError::BadRef`], never
//! a panic.
//!
//! # Scope
//!
//! This task adds the memory instruction set (`Load`/`Store`/`Addr`/`AddrField`/
//! `Field`/`StoreField`), indirect calls, the `@ptr_add`/`@memcpy` builtins, the
//! `Panic` terminator, and executes the injected byte-buffer/`format_args`
//! runtime bodies. Both `@lilac_abort` and `Panic` trap through
//! `trap_message`: they print `panic: <message>` to stderr and yield
//! [`Step::Panicked`], which [`Machine::run`] maps to `Ok(101)`. `Unreachable`
//! and a `pc` running past the end of a body are clean runtime errors
//! ([`VmError::Unreachable`], [`VmError::BadPc`]), never a `bug!`.

use std::fmt;

use crate::decode::{decode, Decoded};
use crate::encode::{
    builtin_from_index, convert_from_opcode, OP_ADDR, OP_ADDRFIELD_BASE, OP_ADDRFIELD_LAST,
    OP_ADDR_FUNC, OP_ARITH_IMM, OP_ARITH_RET, OP_BRANCH, OP_BRANCH_IF, OP_BRANCH_IF_IMM, OP_CALL,
    OP_CALL_ARITH_IMM, OP_CALL_ARITH_OUT, OP_CALL_BUILTIN_BASE, OP_CALL_BUILTIN_LAST,
    OP_CALL_INDIRECT, OP_CALL_OUT, OP_CONST, OP_CONST_DATA, OP_COPY_BASE, OP_COPY_BYTES,
    OP_COPY_LAST, OP_COPY_RET, OP_EXTEND_ZERO, OP_FADD_BASE, OP_FIELD_BASE, OP_FIELD_LAST, OP_LOAD,
    OP_PANIC, OP_RETURN, OP_STOREFIELD_BASE, OP_STOREFIELD_LAST, OP_STORE_BASE, OP_STORE_LAST,
    OP_TRUNC_BASE, OP_UNREACHABLE, WIDE_FLAG,
};
use crate::isa::{ArithOp, Builtin, CallSite, Operand, Program, VM_POINTER_BYTES};

/// Maximum number of live frames before a call traps with
/// [`VmError::StackOverflow`].
pub const MAX_CALL_DEPTH: u32 = 4096;

/// Exit code reported when a program traps through `@lilac_abort`.
pub const ABORT_CODE: i32 = 101;

/// High-bit tag distinguishing a function address from an arena ref.
///
/// [`Instr::AddrFunc`] stores `FUNC_TAG | func_id`; [`Instr::CallIndirect`]
/// decodes it. Live arena refs are byte offsets bounded well below `1 << 63`
/// (globals cap at 64 MiB and the call stack at [`MAX_CALL_DEPTH`] frames of at
/// most `u32::MAX` bytes), so the tag cannot collide with a valid ref.
const FUNC_TAG: u64 = 1 << 63;

/// A VM runtime error.
#[derive(Debug)]
pub enum VmError {
    /// No function with the requested entry name exists.
    NoEntry(String),
    /// A call targeted an `extern` (bodyless) function.
    ExternCall(String),
    /// A ref/place access fell outside the arena.
    BadRef(u64),
    /// The program counter ran past the end of the current function's code.
    BadPc(u32),
    /// A malformed word stream was reached during execution (unverified
    /// program).
    BadInstr(u32),
    /// Execution reached an `unreachable` terminator.
    Unreachable,
    /// The call stack reached [`MAX_CALL_DEPTH`].
    StackOverflow,
}

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoEntry(name) => write!(f, "no entry point `{name}`"),
            Self::ExternCall(name) => write!(f, "cannot call extern function `{name}`"),
            Self::BadRef(off) => write!(f, "invalid arena reference {off}"),
            Self::BadPc(pc) => write!(f, "program counter {pc} out of range"),
            Self::BadInstr(pc) => write!(f, "malformed instruction at program counter {pc}"),
            Self::Unreachable => write!(f, "reached an `unreachable` terminator"),
            Self::StackOverflow => write!(f, "call stack overflow (limit {MAX_CALL_DEPTH})"),
        }
    }
}

impl std::error::Error for VmError {}

/// The result of executing one bytecode instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The frame advanced; execution may continue.
    Advanced,
    /// The entry frame returned; the program is complete.
    Returned,
    /// The program panicked with the given exit code.
    Panicked(i32),
}

/// One frame in [`Machine::backtrace`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameInfo {
    /// Function index.
    pub func: u32,
    /// Function name.
    pub name: String,
    /// Program counter within the function.
    pub pc: u32,
    /// Source line for `pc`, when the line table has one.
    pub line: Option<crate::isa::Line>,
}

/// One live call frame.
struct Frame {
    /// Function index.
    func: u32,
    /// Index of this frame's first register in `Machine::regs`.
    reg_base: u32,
    /// Byte offset of this frame's slot region in `Machine::arena`.
    slot_base: u32,
    /// Program counter of the next instruction to execute in this frame.
    pc: u32,
    /// Program counter of the instruction most recently executed in this frame.
    ///
    /// `pc` is advanced before an instruction runs (and overwritten to a
    /// branch target for a taken branch), so it points at the *next*
    /// instruction. Diagnostics such as [`Machine::current_line`] and
    /// [`Machine::backtrace`] instead report the instruction the frame is
    /// currently executing, which is `last_pc` once any instruction has run and
    /// the function entry otherwise.
    last_pc: Option<u32>,
    /// Reserved return register of this frame's callee (`None` for by-ref).
    ret_reg: Option<u32>,
    /// A deferred output copy applied when this frame returns (fused
    /// `call.out`/`call.arith.out`, or a register-return callee).
    out_copy: Option<PendingOut>,
}

/// A caller-side output copy to apply when the callee returns.
#[derive(Clone, Copy, Debug)]
enum PendingOut {
    /// Write the callee's return register into the caller's slot `slot_off`.
    RetToSlot { slot_off: u32, size: u32 },
    /// Write the callee's return register into the caller's register `dst`.
    RetToReg { dst: u32 },
    /// Read the caller's out slot `slot_off` into the caller's register `dst`
    /// (by-reference output).
    SlotToReg { slot_off: u32, dst: u32, size: u32 },
}

/// A bytecode interpreter over one [`Program`].
pub struct Machine<'a> {
    program: &'a Program,
    /// All frames' registers. The vector is kept at its high-water length and
    /// its live top is [`Machine::reg_top`]; registers are SSA (written before
    /// read), so frames may reuse stale bits without re-zeroing.
    regs: Vec<u64>,
    /// Globals followed by each live frame's slot region. Kept at high-water
    /// length; the live top is [`Machine::slot_top`]. Call re-zeroes only the
    /// callee's slot region (locals need `alloca` zero-init).
    arena: Vec<u8>,
    /// The call stack; the last frame is the running one.
    frames: Vec<Frame>,
    /// Logical top of the live register region in [`Machine::regs`].
    reg_top: u32,
    /// Logical top of the live slot region in [`Machine::arena`].
    slot_top: u32,
}

impl<'a> Machine<'a> {
    /// Creates a machine with entry point `main`.
    ///
    /// # Errors
    ///
    /// Returns [`VmError::NoEntry`] when the program has no `main`, and
    /// [`VmError::ExternCall`] when `main` is an `extern` declaration.
    pub fn new(program: &'a Program) -> Result<Self, VmError> {
        Self::with_entry(program, "main")
    }

    /// Creates a machine with the named entry point.
    ///
    /// # Errors
    ///
    /// Returns [`VmError::NoEntry`] when no function has the given name, and
    /// [`VmError::ExternCall`] when the entry is an `extern` declaration.
    pub fn with_entry(program: &'a Program, entry: &str) -> Result<Self, VmError> {
        let func = program
            .funcs
            .iter()
            .position(|f| f.name == entry)
            .ok_or_else(|| VmError::NoEntry(entry.to_string()))?;
        let func_index = u32::try_from(func).unwrap_or(u32::MAX);
        let entry_func = &program.funcs[func];
        if entry_func.is_extern {
            return Err(VmError::ExternCall(entry.to_string()));
        }
        let mut arena = program.globals.clone();
        let slot_base = u32::try_from(arena.len()).unwrap_or(u32::MAX);
        arena.resize(
            arena.len() + usize::try_from(entry_func.slot_bytes).unwrap_or(0),
            0,
        );
        let regs = vec![0_u64; usize::try_from(entry_func.n_regs).unwrap_or(0)];
        let slot_top = u32::try_from(arena.len()).unwrap_or(u32::MAX);
        let frames = vec![Frame {
            func: func_index,
            reg_base: 0,
            slot_base,
            pc: 0,
            last_pc: None,
            ret_reg: None,
            out_copy: None,
        }];
        Ok(Self {
            program,
            regs,
            arena,
            frames,
            reg_top: entry_func.n_regs,
            slot_top,
        })
    }

    /// Runs until the entry frame returns or the program panics.
    ///
    /// This is the hot path (spec §6.1): `func`, `code`, `pc`, `reg_base`, and
    /// `slot_base` live in locals and the raw opcode byte is matched directly,
    /// extracting operands with shifts. The frame stack is touched only on
    /// call/return/trap; the per-instruction `decode`, frame re-derivation, and
    /// `Result` of [`Machine::step`] are gone. [`Machine::step`] stays the
    /// debugger's single-instruction path over the same stream.
    ///
    /// # Errors
    ///
    /// Returns a clean [`VmError`] for a malformed (unverified) body, an
    /// out-of-range arena access, a call to a bodyless function, or call-stack
    /// overflow.
    #[allow(clippy::too_many_lines)]
    pub fn run(&mut self) -> Result<i32, VmError> {
        // Take the register file, arena, and frame stack out of `self` so the
        // dispatch loop can hold them as locals; `self` carries no aliasing
        // writes on the hot path. They are restored before returning.
        let program = self.program;
        let mut regs = core::mem::take(&mut self.regs);
        let mut arena = core::mem::take(&mut self.arena);
        let mut frames = core::mem::take(&mut self.frames);
        let mut reg_top = self.reg_top;
        let mut slot_top = self.slot_top;
        let result = Self::run_impl(
            program,
            &mut regs,
            &mut arena,
            &mut frames,
            &mut reg_top,
            &mut slot_top,
        );
        self.regs = regs;
        self.arena = arena;
        self.frames = frames;
        self.reg_top = reg_top;
        self.slot_top = slot_top;
        result
    }

    /// The inner dispatch loop over local register/arena/frame storage.
    #[allow(clippy::too_many_lines, clippy::needless_late_init)]
    fn run_impl(
        program: &Program,
        regs: &mut Vec<u64>,
        arena: &mut Vec<u8>,
        frames: &mut Vec<Frame>,
        reg_top: &mut u32,
        slot_top: &mut u32,
    ) -> Result<i32, VmError> {
        let Some(frame) = frames.last() else {
            return Ok(0);
        };
        let mut func_index = frame.func;
        let mut pc = frame.pc;
        let mut reg_base = frame.reg_base;
        let mut slot_base = frame.slot_base;
        // Hoisting the function and its code out of the loop avoids a bounds
        // check and two dependent loads per instruction; call/return arms
        // refresh them from the new `func_index`.
        let mut func = &program.funcs[func_index as usize];
        let mut code = func.code.as_slice();

        loop {
            if pc as usize >= code.len() {
                // The verifier guarantees every body ends in a terminator and
                // every branch target is in range, but a directly constructed
                // (unverified) `Program` can still run off the end.
                return Err(VmError::BadPc(pc));
            }
            let w = code[pc as usize];
            let raw = (w & 0xFF) as u8;
            let wide = raw & WIDE_FLAG != 0;
            let base = raw & !WIDE_FLAG;
            let next: u32;
            // Set by `return` and its fused variants; the shared return handling
            // runs after the match so it is written once.
            let mut return_now = false;
            match base {
                OP_RETURN => {
                    if wide {
                        return Err(VmError::BadInstr(pc));
                    }
                    next = pc + 1;
                    return_now = true;
                }
                OP_UNREACHABLE => {
                    if wide {
                        return Err(VmError::BadInstr(pc));
                    }
                    store_frame(frames, func_index, pc, pc + 1);
                    return Err(VmError::Unreachable);
                }
                OP_PANIC => {
                    if wide {
                        return Err(VmError::BadInstr(pc));
                    }
                    // The referenced data constant's bytes are the panic
                    // message; an unverified index falls back to empty.
                    let message = program
                        .data
                        .get((w >> 8) as usize)
                        .map_or(&[][..], |entry| entry.bytes.as_slice());
                    store_frame(frames, func_index, pc, pc + 1);
                    let _ = trap(message);
                    return Ok(ABORT_CODE);
                }
                OP_BRANCH => {
                    if wide {
                        return Err(VmError::BadInstr(pc));
                    }
                    next = w >> 8;
                }
                OP_BRANCH_IF => {
                    let (ops, _) = read_ops3(code, pc, wide)?;
                    next = if reg_of(regs, reg_base, ops[0]) == 0 {
                        ops[2]
                    } else {
                        ops[1]
                    };
                }
                OP_BRANCH_IF_IMM => {
                    if wide {
                        return Err(VmError::BadInstr(pc));
                    }
                    let then = *code.get(pc as usize + 1).ok_or(VmError::BadInstr(pc))?;
                    let els = *code.get(pc as usize + 2).ok_or(VmError::BadInstr(pc))?;
                    let lo = *code.get(pc as usize + 3).ok_or(VmError::BadInstr(pc))?;
                    let hi = *code.get(pc as usize + 4).ok_or(VmError::BadInstr(pc))?;
                    let imm = u64::from(lo) | (u64::from(hi) << 32);
                    let lhs = (w >> 8) & 0xFF;
                    let lhs_slot = (w >> 16) & u32::from(WIDE_FLAG) != 0;
                    let op_code = u8::try_from(w >> 24).unwrap_or(0);
                    let size = if op_code >= OP_FADD_BASE {
                        float_size(op_code & 1)
                    } else {
                        int_size(op_code & 3)
                    };
                    let left = if lhs_slot {
                        let abs = slot_base as usize + lhs as usize;
                        read_abs(arena, abs, size).ok_or(VmError::BadRef(abs as u64))?
                    } else {
                        reg_of(regs, reg_base, lhs)
                    };
                    let Some(value) = eval_arith_code(op_code, left, imm) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    next = if value == 0 { els } else { then };
                }
                OP_CONST => {
                    let (dst, n) = read_ops1(code, pc, wide)?;
                    let lo = *code
                        .get(pc as usize + n as usize)
                        .ok_or(VmError::BadInstr(pc))?;
                    let hi = *code
                        .get(pc as usize + n as usize + 1)
                        .ok_or(VmError::BadInstr(pc))?;
                    let imm = u64::from(lo) | (u64::from(hi) << 32);
                    set_reg_of(regs, reg_base, dst, imm);
                    next = pc + n + 2;
                }
                OP_CONST_DATA => {
                    let (dst, n) = read_ops1(code, pc, wide)?;
                    let data = *code
                        .get(pc as usize + n as usize)
                        .ok_or(VmError::BadInstr(pc))?;
                    let off = program
                        .data
                        .get(data as usize)
                        .map_or(0, |entry| entry.global_off);
                    set_reg_of(regs, reg_base, dst, u64::from(off));
                    next = pc + n + 1;
                }
                b @ (OP_COPY_BASE..=OP_COPY_LAST) => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let k = b - OP_COPY_BASE;
                    let dst = operate(ops[0], k & 2 != 0);
                    let src = operate(ops[1], k & 1 != 0);
                    let size = ops[2];
                    if size <= VM_POINTER_BYTES {
                        let value = read_scalar(regs, arena, reg_base, slot_base, src, size)?;
                        write_scalar(regs, arena, reg_base, slot_base, dst, value, size)?;
                    } else {
                        copy_aggregate(regs, arena, reg_base, slot_base, dst, src, size)?;
                    }
                    next = pc + n;
                }
                OP_COPY_BYTES => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let src_abs = slot_base as usize + ops[1] as usize;
                    let dst_abs = slot_base as usize + ops[0] as usize;
                    copy_arena(arena, src_abs, dst_abs, ops[2] as usize)?;
                    next = pc + n;
                }
                OP_LOAD => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let ref_off = slot_ref(arena, slot_base, ops[1])?;
                    if ops[2] <= VM_POINTER_BYTES {
                        let abs = usize::try_from(ref_off).map_err(|_| VmError::BadRef(ref_off))?;
                        let value = read_abs(arena, abs, ops[2]).ok_or(VmError::BadRef(ref_off))?;
                        set_reg_of(regs, reg_base, ops[0], value);
                    } else {
                        // An aggregate `Load` passes the pointer through.
                        set_reg_of(regs, reg_base, ops[0], ref_off);
                    }
                    next = pc + n;
                }
                b @ (OP_STORE_BASE..=OP_STORE_LAST) => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let src = operate(ops[0], b == OP_STORE_LAST);
                    let ref_off = slot_ref(arena, slot_base, ops[1])?;
                    let abs = usize::try_from(ref_off).map_err(|_| VmError::BadRef(ref_off))?;
                    if ops[2] <= VM_POINTER_BYTES {
                        let value = read_scalar(regs, arena, reg_base, slot_base, src, ops[2])?;
                        write_abs(arena, abs, value, ops[2])?;
                    } else {
                        copy_operand_bytes(regs, arena, reg_base, slot_base, src, abs, ops[2])?;
                    }
                    next = pc + n;
                }
                OP_ADDR => {
                    let (ops, n) = read_ops2(code, pc, wide)?;
                    set_reg_of(
                        regs,
                        reg_base,
                        ops[0],
                        u64::from(slot_base) + u64::from(ops[1]),
                    );
                    next = pc + n;
                }
                b @ (OP_ADDRFIELD_BASE..=OP_ADDRFIELD_LAST) => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let base_off = field_base(arena, slot_base, ops[1], b == OP_ADDRFIELD_LAST)?;
                    let addr = base_off
                        .checked_add(ops[2] as usize)
                        .ok_or(VmError::BadRef(base_off as u64))?;
                    let addr = u64::try_from(addr).map_err(|_| VmError::BadRef(base_off as u64))?;
                    set_reg_of(regs, reg_base, ops[0], addr);
                    next = pc + n;
                }
                OP_ADDR_FUNC => {
                    let (ops, n) = read_ops2(code, pc, wide)?;
                    set_reg_of(regs, reg_base, ops[0], FUNC_TAG | u64::from(ops[1]));
                    next = pc + n;
                }
                b @ (OP_FIELD_BASE..=OP_FIELD_LAST) => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let size = *code
                        .get(pc as usize + n as usize)
                        .ok_or(VmError::BadInstr(pc))?;
                    let base_off = field_base(arena, slot_base, ops[1], b == OP_FIELD_LAST)?;
                    let addr = base_off
                        .checked_add(ops[2] as usize)
                        .ok_or(VmError::BadRef(base_off as u64))?;
                    if size <= VM_POINTER_BYTES {
                        let value =
                            read_abs(arena, addr, size).ok_or(VmError::BadRef(addr as u64))?;
                        set_reg_of(regs, reg_base, ops[0], value);
                    } else {
                        // The compiler emits `AddrField` for aggregate fields;
                        // this defensive fall-back yields the field ref.
                        let value =
                            u64::try_from(addr).map_err(|_| VmError::BadRef(addr as u64))?;
                        set_reg_of(regs, reg_base, ops[0], value);
                    }
                    next = pc + n + 1;
                }
                b @ (OP_STOREFIELD_BASE..=OP_STOREFIELD_LAST) => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let size = *code
                        .get(pc as usize + n as usize)
                        .ok_or(VmError::BadInstr(pc))?;
                    let k = b - OP_STOREFIELD_BASE;
                    let src = operate(ops[0], k & 1 != 0);
                    let base_off = field_base(arena, slot_base, ops[1], k & 2 != 0)?;
                    let addr = base_off
                        .checked_add(ops[2] as usize)
                        .ok_or(VmError::BadRef(base_off as u64))?;
                    if size <= VM_POINTER_BYTES {
                        let value = read_scalar(regs, arena, reg_base, slot_base, src, size)?;
                        write_abs(arena, addr, value, size)?;
                    } else {
                        copy_operand_bytes(regs, arena, reg_base, slot_base, src, addr, size)?;
                    }
                    next = pc + n + 1;
                }
                OP_CALL => {
                    if wide {
                        return Err(VmError::BadInstr(pc));
                    }
                    let index = (w >> 8) as usize;
                    let Some(site) = func.call_sites.get(index) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let (Some(ins), Some(outs)) = (
                        func.operands
                            .get(site.ins.start as usize..site.ins.end as usize),
                        func.operands
                            .get(site.outs.start as usize..site.outs.end as usize),
                    ) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    store_pc(frames, func_index, pc + 1);
                    let callee_index = site.func;
                    let callee_reg_base = *reg_top;
                    let callee_slot_base = *slot_top;
                    do_call(
                        program,
                        regs,
                        arena,
                        frames,
                        reg_top,
                        slot_top,
                        callee_index,
                        ins,
                        outs,
                        reg_base,
                        slot_base,
                        None,
                    )?;
                    func_index = callee_index;
                    pc = 0;
                    reg_base = callee_reg_base;
                    slot_base = callee_slot_base;
                    func = &program.funcs[func_index as usize];
                    code = func.code.as_slice();
                    continue;
                }
                OP_CALL_ARITH_IMM => {
                    if wide {
                        return Err(VmError::BadInstr(pc));
                    }
                    let index = *code.get(pc as usize + 1).ok_or(VmError::BadInstr(pc))? as usize;
                    let lo = *code.get(pc as usize + 2).ok_or(VmError::BadInstr(pc))?;
                    let hi = *code.get(pc as usize + 3).ok_or(VmError::BadInstr(pc))?;
                    let imm = u64::from(lo) | (u64::from(hi) << 32);
                    let lhs = (w >> 8) & 0xFF;
                    let lhs_slot = (w >> 16) & u32::from(WIDE_FLAG) != 0;
                    let op_code = u8::try_from(w >> 24).unwrap_or(0);
                    let Some(site) = func.call_sites.get(index) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let (Some(ins), Some(outs)) = (
                        func.operands
                            .get(site.ins.start as usize..site.ins.end as usize),
                        func.operands
                            .get(site.outs.start as usize..site.outs.end as usize),
                    ) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let op_size = if op_code >= OP_FADD_BASE {
                        float_size(op_code & 1)
                    } else {
                        int_size(op_code & 3)
                    };
                    let left = if lhs_slot {
                        let abs = slot_base as usize + lhs as usize;
                        read_abs(arena, abs, op_size).ok_or(VmError::BadRef(abs as u64))?
                    } else {
                        reg_of(regs, reg_base, lhs)
                    };
                    let Some(value) = eval_arith_code(op_code, left, imm) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let Some(Operand::Reg(dst_reg)) = ins.first().copied() else {
                        return Err(VmError::BadInstr(pc));
                    };
                    set_reg_of(regs, reg_base, dst_reg, value);
                    store_pc(frames, func_index, pc + 4);
                    let callee_index = site.func;
                    let callee_reg_base = *reg_top;
                    let callee_slot_base = *slot_top;
                    do_call(
                        program,
                        regs,
                        arena,
                        frames,
                        reg_top,
                        slot_top,
                        callee_index,
                        ins,
                        outs,
                        reg_base,
                        slot_base,
                        None,
                    )?;
                    func_index = callee_index;
                    pc = 0;
                    reg_base = callee_reg_base;
                    slot_base = callee_slot_base;
                    func = &program.funcs[func_index as usize];
                    code = func.code.as_slice();
                    continue;
                }
                OP_CALL_ARITH_OUT => {
                    if wide {
                        return Err(VmError::BadInstr(pc));
                    }
                    let index = *code.get(pc as usize + 1).ok_or(VmError::BadInstr(pc))? as usize;
                    let packed = *code.get(pc as usize + 2).ok_or(VmError::BadInstr(pc))?;
                    let lo = *code.get(pc as usize + 3).ok_or(VmError::BadInstr(pc))?;
                    let hi = *code.get(pc as usize + 4).ok_or(VmError::BadInstr(pc))?;
                    let imm = u64::from(lo) | (u64::from(hi) << 32);
                    let lhs = (w >> 8) & 0xFF;
                    let lhs_slot = (w >> 16) & u32::from(WIDE_FLAG) != 0;
                    let op_code = u8::try_from(w >> 24).unwrap_or(0);
                    let Some(site) = func.call_sites.get(index) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let (Some(ins), Some(outs)) = (
                        func.operands
                            .get(site.ins.start as usize..site.ins.end as usize),
                        func.operands
                            .get(site.outs.start as usize..site.outs.end as usize),
                    ) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let op_size = if op_code >= OP_FADD_BASE {
                        float_size(op_code & 1)
                    } else {
                        int_size(op_code & 3)
                    };
                    let left = if lhs_slot {
                        let abs = slot_base as usize + lhs as usize;
                        read_abs(arena, abs, op_size).ok_or(VmError::BadRef(abs as u64))?
                    } else {
                        reg_of(regs, reg_base, lhs)
                    };
                    let Some(value) = eval_arith_code(op_code, left, imm) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let Some(Operand::Reg(arg_reg)) = ins.first().copied() else {
                        return Err(VmError::BadInstr(pc));
                    };
                    set_reg_of(regs, reg_base, arg_reg, value);
                    let Some(Operand::Slot(_)) = outs.first().copied() else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let dst = packed & 0xFF;
                    store_pc(frames, func_index, pc + 5);
                    let callee_index = site.func;
                    let callee_reg_base = *reg_top;
                    let callee_slot_base = *slot_top;
                    do_call(
                        program,
                        regs,
                        arena,
                        frames,
                        reg_top,
                        slot_top,
                        callee_index,
                        ins,
                        outs,
                        reg_base,
                        slot_base,
                        Some(dst),
                    )?;
                    func_index = callee_index;
                    pc = 0;
                    reg_base = callee_reg_base;
                    slot_base = callee_slot_base;
                    func = &program.funcs[func_index as usize];
                    code = func.code.as_slice();
                    continue;
                }
                OP_CALL_OUT => {
                    if wide {
                        return Err(VmError::BadInstr(pc));
                    }
                    let index = *code.get(pc as usize + 1).ok_or(VmError::BadInstr(pc))? as usize;
                    let Some(site) = func.call_sites.get(index) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let (Some(ins), Some(outs)) = (
                        func.operands
                            .get(site.ins.start as usize..site.ins.end as usize),
                        func.operands
                            .get(site.outs.start as usize..site.outs.end as usize),
                    ) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let Some(Operand::Slot(_)) = outs.first().copied() else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let dst = (w >> 8) & 0xFF;
                    store_pc(frames, func_index, pc + 2);
                    let callee_index = site.func;
                    let callee_reg_base = *reg_top;
                    let callee_slot_base = *slot_top;
                    do_call(
                        program,
                        regs,
                        arena,
                        frames,
                        reg_top,
                        slot_top,
                        callee_index,
                        ins,
                        outs,
                        reg_base,
                        slot_base,
                        Some(dst),
                    )?;
                    func_index = callee_index;
                    pc = 0;
                    reg_base = callee_reg_base;
                    slot_base = callee_slot_base;
                    func = &program.funcs[func_index as usize];
                    code = func.code.as_slice();
                    continue;
                }
                OP_CALL_INDIRECT => {
                    let (callee, n) = read_ops1(code, pc, wide)?;
                    let index = *code
                        .get(pc as usize + n as usize)
                        .ok_or(VmError::BadInstr(pc))? as usize;
                    let Some(site) = func.call_sites.get(index) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let (Some(ins), Some(outs)) = (
                        func.operands
                            .get(site.ins.start as usize..site.ins.end as usize),
                        func.operands
                            .get(site.outs.start as usize..site.outs.end as usize),
                    ) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let value = reg_of(regs, reg_base, callee);
                    if value & FUNC_TAG == 0 {
                        return Err(VmError::BadRef(value));
                    }
                    let target =
                        u32::try_from(value & !FUNC_TAG).map_err(|_| VmError::BadRef(value))?;
                    store_pc(frames, func_index, pc + n + 1);
                    let callee_reg_base = *reg_top;
                    let callee_slot_base = *slot_top;
                    do_call(
                        program, regs, arena, frames, reg_top, slot_top, target, ins, outs,
                        reg_base, slot_base, None,
                    )?;
                    func_index = target;
                    pc = 0;
                    reg_base = callee_reg_base;
                    slot_base = callee_slot_base;
                    func = &program.funcs[func_index as usize];
                    code = func.code.as_slice();
                    continue;
                }
                b @ (OP_CALL_BUILTIN_BASE..=OP_CALL_BUILTIN_LAST) => {
                    if wide {
                        return Err(VmError::BadInstr(pc));
                    }
                    let Some(builtin) = builtin_from_index(b - OP_CALL_BUILTIN_BASE) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let (_site, ins, outs) =
                        resolve_call(&func.call_sites, &func.operands, (w >> 8) as usize)
                            .ok_or(VmError::BadInstr(pc))?;
                    if let Step::Panicked(code) =
                        do_builtin(regs, arena, builtin, ins, outs, reg_base, slot_base)?
                    {
                        store_frame(frames, func_index, pc, pc + 1);
                        return Ok(code);
                    }
                    next = pc + 1;
                }
                b @ (OP_TRUNC_BASE..=0x5F) => {
                    let (ops, n) = read_ops2(code, pc, wide)?;
                    let Some((_, width)) = convert_from_opcode(b) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let value = reg_of(regs, reg_base, ops[1]);
                    set_reg_of(regs, reg_base, ops[0], value & mask(u32::from(width) / 8));
                    next = pc + n;
                }
                OP_EXTEND_ZERO => {
                    let (ops, n) = read_ops2(code, pc, wide)?;
                    let value = reg_of(regs, reg_base, ops[1]);
                    set_reg_of(regs, reg_base, ops[0], value);
                    next = pc + n;
                }
                OP_COPY_RET => {
                    let (ops, n) = read_ops2(code, pc, wide)?;
                    let packed = ops[1];
                    let src = operate(ops[0], packed & 0x80 != 0);
                    let size = packed & 0x7F;
                    let Some(ret_reg) = func.ret_reg else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let value = read_scalar(regs, arena, reg_base, slot_base, src, size)?;
                    set_reg_of(regs, reg_base, ret_reg, value & mask(size));
                    next = pc + n;
                    return_now = true;
                }
                OP_ARITH_RET => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let op_code = u8::try_from(ops[2] & 0x7F).unwrap_or(0);
                    let Some(ret_reg) = func.ret_reg else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let left = reg_of(regs, reg_base, ops[0]);
                    let right = reg_of(regs, reg_base, ops[1]);
                    let Some(value) = eval_arith_code(op_code, left, right) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    set_reg_of(regs, reg_base, ret_reg, value);
                    next = pc + n;
                    return_now = true;
                }
                OP_ARITH_IMM => {
                    if wide {
                        return Err(VmError::BadInstr(pc));
                    }
                    let lo = *code.get(pc as usize + 1).ok_or(VmError::BadInstr(pc))?;
                    let hi = *code.get(pc as usize + 2).ok_or(VmError::BadInstr(pc))?;
                    let imm = u64::from(lo) | (u64::from(hi) << 32);
                    let dst = (w >> 8) & 0xFF;
                    let lhs = (w >> 16) & 0xFF;
                    let flags = (w >> 24) & 0xFF;
                    let lhs_slot = flags & u32::from(WIDE_FLAG) != 0;
                    let op_code = u8::try_from(flags & 0x7F).unwrap_or(0);
                    let size = if op_code >= OP_FADD_BASE {
                        float_size(op_code & 1)
                    } else {
                        int_size(op_code & 3)
                    };
                    let left = if lhs_slot {
                        let abs = slot_base as usize + lhs as usize;
                        read_abs(arena, abs, size).ok_or(VmError::BadRef(abs as u64))?
                    } else {
                        reg_of(regs, reg_base, lhs)
                    };
                    let Some(value) = eval_arith_code(op_code, left, imm) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    set_reg_of(regs, reg_base, dst, value);
                    next = pc + 3;
                }
                b => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let left = reg_of(regs, reg_base, ops[1]);
                    let right = reg_of(regs, reg_base, ops[2]);
                    let Some(value) = eval_arith_code(b, left, right) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    set_reg_of(regs, reg_base, ops[0], value);
                    next = pc + n;
                }
            }
            if return_now {
                let (step, pending, ret) = return_frame(regs, frames, reg_top, slot_top);
                if step == Step::Returned {
                    return Ok(0);
                }
                let Some(caller) = frames.last() else {
                    return Ok(0);
                };
                match pending {
                    Some(PendingOut::RetToSlot { slot_off, size }) => {
                        let abs = caller.slot_base as usize + slot_off as usize;
                        write_abs(arena, abs, ret, size)?;
                    }
                    Some(PendingOut::RetToReg { dst }) => {
                        set_reg_of(regs, caller.reg_base, dst, ret);
                    }
                    Some(PendingOut::SlotToReg {
                        slot_off,
                        dst,
                        size,
                    }) => {
                        let abs = caller.slot_base as usize + slot_off as usize;
                        let value =
                            read_abs(arena, abs, size).ok_or(VmError::BadRef(abs as u64))?;
                        set_reg_of(regs, caller.reg_base, dst, value);
                    }
                    None => {}
                }
                func_index = caller.func;
                pc = caller.pc;
                reg_base = caller.reg_base;
                slot_base = caller.slot_base;
                func = &program.funcs[func_index as usize];
                code = func.code.as_slice();
                continue;
            }
            pc = next;
        }
    }

    /// Executes one instruction.
    ///
    /// # Errors
    ///
    /// Returns [`VmError::ExternCall`] for a call to a bodyless function,
    /// [`VmError::StackOverflow`] past [`MAX_CALL_DEPTH`], [`VmError::BadRef`]
    /// for an out-of-range arena access, and [`VmError::BadPc`] /
    /// [`VmError::Unreachable`] for a malformed (unverified) body.
    #[allow(clippy::too_many_lines)]
    pub fn step(&mut self) -> Result<Step, VmError> {
        let Some(frame) = self.frames.last() else {
            return Ok(Step::Returned);
        };
        let (func_index, pc, reg_base, slot_base) =
            (frame.func, frame.pc, frame.reg_base, frame.slot_base);
        let program = self.program;
        let func = &program.funcs[func_index as usize];
        if pc as usize >= func.code.len() {
            // The verifier guarantees every non-extern body ends in a
            // terminator and every branch target is in range, but a directly
            // constructed (unverified) `Program` can still run off the end.
            return Err(VmError::BadPc(pc));
        }
        let (instr, next) = decode(&func.code, pc as usize, &func.call_sites, &func.operands)
            .map_err(|_| VmError::BadInstr(pc))?;
        // All instructions except branches fall through; branch arms overwrite.
        if let Some(frame) = self.frames.last_mut() {
            frame.pc = next;
            frame.last_pc = Some(pc);
        }
        match instr {
            Decoded::Const { dst, imm } => self.set_reg(reg_base, dst, imm),
            Decoded::ConstData { dst, data } => {
                let off = program
                    .data
                    .get(data as usize)
                    .map_or(0, |entry| entry.global_off);
                self.set_reg(reg_base, dst, u64::from(off));
            }
            Decoded::Copy { dst, src, size } => {
                if size <= VM_POINTER_BYTES {
                    let value = self.read_scalar(reg_base, slot_base, src, size)?;
                    self.write_scalar(reg_base, slot_base, dst, value, size)?;
                } else {
                    self.copy_aggregate(reg_base, slot_base, dst, src, size)?;
                }
            }
            Decoded::CopyBytes { dst, src, size } => {
                let src_abs = usize::try_from(slot_base).unwrap_or(0) + src as usize;
                let dst_abs = usize::try_from(slot_base).unwrap_or(0) + dst as usize;
                self.copy_arena(src_abs, dst_abs, size as usize)?;
            }
            Decoded::Load { dst, src, size } => {
                let ref_off = self.slot_ref(slot_base, src)?;
                if size <= VM_POINTER_BYTES {
                    let abs = usize::try_from(ref_off).map_err(|_| VmError::BadRef(ref_off))?;
                    let value = self.read_abs(abs, size).ok_or(VmError::BadRef(ref_off))?;
                    self.set_reg(reg_base, dst, value);
                } else {
                    // An aggregate register holds a ref, not the bytes, so a
                    // `Load` of an indirect value passes the pointer through.
                    self.set_reg(reg_base, dst, ref_off);
                }
            }
            Decoded::Store { src, dst, size } => {
                let ref_off = self.slot_ref(slot_base, dst)?;
                let abs = usize::try_from(ref_off).map_err(|_| VmError::BadRef(ref_off))?;
                if size <= VM_POINTER_BYTES {
                    let value = self.read_scalar(reg_base, slot_base, src, size)?;
                    self.write_abs(abs, value, size)?;
                } else {
                    self.copy_operand_bytes(reg_base, slot_base, src, abs, size)?;
                }
            }
            Decoded::Addr { dst, place } => {
                let addr = u64::from(slot_base) + u64::from(place);
                self.set_reg(reg_base, dst, addr);
            }
            Decoded::AddrField {
                dst,
                place,
                off,
                indirect,
            } => {
                let base = self.field_base(slot_base, place, indirect)?;
                let addr = base
                    .checked_add(off as usize)
                    .ok_or(VmError::BadRef(base as u64))?;
                let addr = u64::try_from(addr).map_err(|_| VmError::BadRef(base as u64))?;
                self.set_reg(reg_base, dst, addr);
            }
            Decoded::AddrFunc { dst, func: target } => {
                self.set_reg(reg_base, dst, FUNC_TAG | u64::from(target));
            }
            Decoded::Field {
                dst,
                place,
                off,
                size,
                indirect,
            } => {
                let base = self.field_base(slot_base, place, indirect)?;
                let addr = base
                    .checked_add(off as usize)
                    .ok_or(VmError::BadRef(base as u64))?;
                if size <= VM_POINTER_BYTES {
                    let value = self
                        .read_abs(addr, size)
                        .ok_or(VmError::BadRef(addr as u64))?;
                    self.set_reg(reg_base, dst, value);
                } else {
                    // The compiler emits `AddrField` for aggregate fields; this
                    // is a defensive fall-back that yields the field ref.
                    let value = u64::try_from(addr).map_err(|_| VmError::BadRef(addr as u64))?;
                    self.set_reg(reg_base, dst, value);
                }
            }
            Decoded::StoreField {
                src,
                place,
                off,
                size,
                indirect,
            } => {
                let base = self.field_base(slot_base, place, indirect)?;
                let addr = base
                    .checked_add(off as usize)
                    .ok_or(VmError::BadRef(base as u64))?;
                if size <= VM_POINTER_BYTES {
                    let value = self.read_scalar(reg_base, slot_base, src, size)?;
                    self.write_abs(addr, value, size)?;
                } else {
                    self.copy_operand_bytes(reg_base, slot_base, src, addr, size)?;
                }
            }
            Decoded::IAdd {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::IAdd, reg_base, dst, lhs, rhs, width),
            Decoded::ISub {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::ISub, reg_base, dst, lhs, rhs, width),
            Decoded::IMul {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::IMul, reg_base, dst, lhs, rhs, width),
            Decoded::IDiv {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::IDiv, reg_base, dst, lhs, rhs, width),
            Decoded::IMod {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::IMod, reg_base, dst, lhs, rhs, width),
            Decoded::ILt {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::ILt, reg_base, dst, lhs, rhs, width),
            Decoded::IGt {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::IGt, reg_base, dst, lhs, rhs, width),
            Decoded::IDivU {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::IDivU, reg_base, dst, lhs, rhs, width),
            Decoded::IModU {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::IModU, reg_base, dst, lhs, rhs, width),
            Decoded::Eq {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::Eq, reg_base, dst, lhs, rhs, width),
            Decoded::ILtU {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::ILtU, reg_base, dst, lhs, rhs, width),
            Decoded::IGtU {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::IGtU, reg_base, dst, lhs, rhs, width),
            Decoded::FAdd {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::FAdd, reg_base, dst, lhs, rhs, width),
            Decoded::FSub {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::FSub, reg_base, dst, lhs, rhs, width),
            Decoded::FMul {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::FMul, reg_base, dst, lhs, rhs, width),
            Decoded::FDiv {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::FDiv, reg_base, dst, lhs, rhs, width),
            Decoded::FMod {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::FMod, reg_base, dst, lhs, rhs, width),
            Decoded::FLt {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::FLt, reg_base, dst, lhs, rhs, width),
            Decoded::FGt {
                dst,
                lhs,
                rhs,
                width,
            } => self.exec_arith(ArithOp::FGt, reg_base, dst, lhs, rhs, width),
            Decoded::Trunc { dst, src, width } => {
                let value = self.reg(reg_base, src);
                self.set_reg(reg_base, dst, value & mask(u32::from(width) / 8));
            }
            Decoded::ExtendZero { dst, src } => {
                // Zero-extension is already reflected in the register's low
                // bits; the result width is at least the source width, so this
                // is a register move (matching M5c's no-op mask).
                self.set_reg(reg_base, dst, self.reg(reg_base, src));
            }
            Decoded::ArithImm {
                dst,
                lhs,
                lhs_slot,
                op,
                width,
                imm,
            } => {
                let size = u32::from(width) / 8;
                let left = if lhs_slot {
                    let abs = usize::try_from(slot_base).unwrap_or(0) + lhs as usize;
                    self.read_abs(abs, size)
                        .ok_or(VmError::BadRef(abs as u64))?
                } else {
                    self.reg(reg_base, lhs)
                };
                let value = eval_arith(op, left, imm, size);
                self.set_reg(reg_base, dst, value & mask(size));
            }
            Decoded::Call { site, ins, outs } => {
                self.call(site.func, ins, outs, reg_base, slot_base, None)?;
            }
            Decoded::CallOut {
                dst,
                size: _,
                site,
                ins,
                outs,
            } => {
                self.call(site.func, ins, outs, reg_base, slot_base, Some(dst))?;
            }
            Decoded::CallArithImm {
                lhs,
                lhs_slot,
                op,
                width,
                imm,
                site,
                ins,
                outs,
            } => {
                let size = u32::from(width) / 8;
                let left = if lhs_slot {
                    let abs = usize::try_from(slot_base).unwrap_or(0) + lhs as usize;
                    self.read_abs(abs, size)
                        .ok_or(VmError::BadRef(abs as u64))?
                } else {
                    self.reg(reg_base, lhs)
                };
                let value = eval_arith(op, left, imm, size);
                let Some(Operand::Reg(dst_reg)) = ins.first().copied() else {
                    return Err(VmError::BadInstr(pc));
                };
                self.set_reg(reg_base, dst_reg, value);
                self.call(site.func, ins, outs, reg_base, slot_base, None)?;
            }
            Decoded::CallArithOut {
                lhs,
                lhs_slot,
                op,
                width,
                imm,
                site,
                ins,
                outs,
                dst,
                size: _,
            } => {
                let size = u32::from(width) / 8;
                let left = if lhs_slot {
                    let abs = usize::try_from(slot_base).unwrap_or(0) + lhs as usize;
                    self.read_abs(abs, size)
                        .ok_or(VmError::BadRef(abs as u64))?
                } else {
                    self.reg(reg_base, lhs)
                };
                let value = eval_arith(op, left, imm, size);
                if let Some(Operand::Reg(arg_reg)) = ins.first().copied() {
                    self.set_reg(reg_base, arg_reg, value);
                }
                self.call(site.func, ins, outs, reg_base, slot_base, Some(dst))?;
            }
            Decoded::CallIndirect {
                callee, ins, outs, ..
            } => {
                let value = self.reg(reg_base, callee);
                if value & FUNC_TAG == 0 {
                    return Err(VmError::BadRef(value));
                }
                let target =
                    u32::try_from(value & !FUNC_TAG).map_err(|_| VmError::BadRef(value))?;
                self.call(target, ins, outs, reg_base, slot_base, None)?;
            }
            Decoded::CallBuiltin {
                builtin, ins, outs, ..
            } => {
                return self.call_builtin(builtin, ins, outs, reg_base, slot_base);
            }
            Decoded::Return => return Ok(self.return_from_frame()),
            Decoded::CopyReturn { src, size } => {
                let value = self.read_scalar(reg_base, slot_base, src, size)?;
                if let Some(ret_reg) = func.ret_reg {
                    self.set_reg(reg_base, ret_reg, value & mask(size));
                }
                return Ok(self.return_from_frame());
            }
            Decoded::ArithReturn {
                lhs,
                rhs,
                op,
                width,
            } => {
                let size = u32::from(width) / 8;
                let left = self.reg(reg_base, lhs);
                let right = self.reg(reg_base, rhs);
                let value = eval_arith(op, left, right, size);
                if let Some(ret_reg) = func.ret_reg {
                    self.set_reg(reg_base, ret_reg, value);
                }
                return Ok(self.return_from_frame());
            }
            Decoded::Panic { data } => {
                // The referenced data constant's bytes are the panic message.
                // The verifier's `check_data` guarantees the index is in range;
                // the empty fallback only guards an unverified program.
                let message = program
                    .data
                    .get(data as usize)
                    .map_or(&[][..], |entry| entry.bytes.as_slice());
                return Ok(trap(message));
            }
            Decoded::Branch { target } => {
                if let Some(frame) = self.frames.last_mut() {
                    frame.pc = target;
                }
            }
            Decoded::BranchIf { cond, then, els } => {
                let value = self.reg(reg_base, cond);
                let target = if value == 0 { els } else { then };
                if let Some(frame) = self.frames.last_mut() {
                    frame.pc = target;
                }
            }
            Decoded::BranchIfArithImm {
                lhs,
                lhs_slot,
                op,
                width,
                imm,
                then,
                els,
            } => {
                let size = u32::from(width) / 8;
                let left = if lhs_slot {
                    let abs = usize::try_from(slot_base).unwrap_or(0) + lhs as usize;
                    self.read_abs(abs, size)
                        .ok_or(VmError::BadRef(abs as u64))?
                } else {
                    self.reg(reg_base, lhs)
                };
                let value = eval_arith(op, left, imm, size);
                let target = if value == 0 { els } else { then };
                if let Some(frame) = self.frames.last_mut() {
                    frame.pc = target;
                }
            }
            Decoded::Unreachable => return Err(VmError::Unreachable),
        }
        Ok(Step::Advanced)
    }

    /// Evaluates a width-specialized arithmetic/comparison instruction.
    ///
    /// The result is masked to the operand width. For the comparison ops this
    /// is harmless (the result is `0`/`1`); for arithmetic the result type
    /// equals the operand type, matching M5c.
    #[inline]
    fn exec_arith(&mut self, op: ArithOp, reg_base: u32, dst: u32, lhs: u32, rhs: u32, width: u8) {
        exec_arith(&mut self.regs, op, reg_base, dst, lhs, rhs, width);
    }

    /// The current frame's program counter.
    #[must_use]
    pub fn pc(&self) -> u32 {
        self.frames.last().map_or(0, |frame| frame.pc)
    }

    /// The current frame's function index.
    #[must_use]
    pub fn func_index(&self) -> u32 {
        self.frames.last().map_or(0, |frame| frame.func)
    }

    /// The source line for the current frame's current instruction, if known.
    ///
    /// This is the line of the instruction the frame most recently executed
    /// (the call site for a suspended caller), not the line of the next
    /// instruction `pc` points at.
    #[must_use]
    pub fn current_line(&self) -> Option<crate::isa::Line> {
        let frame = self.frames.last()?;
        let func = self.program.funcs.get(frame.func as usize)?;
        line_for(func, frame.last_pc.unwrap_or(frame.pc))
    }

    /// A stack trace from the outermost frame to the running one.
    ///
    /// Each frame's `line` is the source line of the instruction it is
    /// currently executing (see [`Machine::current_line`]); `pc` remains the
    /// next instruction to execute in that frame.
    #[must_use]
    pub fn backtrace(&self) -> Vec<FrameInfo> {
        self.frames
            .iter()
            .map(|frame| {
                let func = self.program.funcs.get(frame.func as usize);
                FrameInfo {
                    func: frame.func,
                    name: func.map_or_else(String::new, |f| f.name.clone()),
                    pc: frame.pc,
                    line: func.and_then(|f| line_for(f, frame.last_pc.unwrap_or(frame.pc))),
                }
            })
            .collect()
    }

    /// Reads a register of the current frame.
    #[must_use]
    pub fn read_reg(&self, r: u32) -> Option<u64> {
        let frame = self.frames.last()?;
        let index = usize::try_from(frame.reg_base).ok()? + usize::try_from(r).ok()?;
        self.regs.get(index).copied()
    }

    /// Reads the first eight bytes of a named place in the current frame.
    #[must_use]
    pub fn read_place(&self, name: &str) -> Option<u64> {
        let frame = self.frames.last()?;
        let func = self.program.funcs.get(frame.func as usize)?;
        let bare = name.strip_prefix('%').unwrap_or(name);
        let (_, off) = func
            .place_names
            .iter()
            .find(|(place, _)| place.as_str() == name || place.as_str() == bare)?;
        let abs = usize::try_from(frame.slot_base).ok()? + usize::try_from(*off).ok()?;
        let remaining = self.arena.len().checked_sub(abs)?;
        let size = u32::try_from(remaining.min(usize::try_from(VM_POINTER_BYTES).unwrap_or(8)))
            .unwrap_or(VM_POINTER_BYTES);
        self.read_abs(abs, size)
    }

    /// The program this machine executes.
    #[must_use]
    pub fn program(&self) -> &'a Program {
        self.program
    }

    /// Pushes a callee frame, binding its inputs and out-ref region from `ins`
    /// and `outs`.
    ///
    /// `outs` are the caller's output places (slot offsets into the caller's
    /// frame, or a register holding an already-absolute offset). Each callee
    /// output is bound by writing the caller place's absolute arena offset into
    /// the callee's out-ref region, so the callee writes the caller's storage
    /// directly (the pointer ABI).
    fn call(
        &mut self,
        target: u32,
        ins: &[Operand],
        outs: &[Operand],
        caller_reg_base: u32,
        caller_slot_base: u32,
        out_dst: Option<u32>,
    ) -> Result<(), VmError> {
        do_call(
            self.program,
            &mut self.regs,
            &mut self.arena,
            &mut self.frames,
            &mut self.reg_top,
            &mut self.slot_top,
            target,
            ins,
            outs,
            caller_reg_base,
            caller_slot_base,
            out_dst,
        )
    }

    /// Pops the running frame, restoring the logical region tops to the
    /// caller's.
    ///
    /// Outputs were written by reference, so there is nothing to copy back; an
    /// output the callee never wrote leaves the caller's storage unchanged. The
    /// register file and arena are not truncated (they stay at high-water);
    /// only the logical tops move, so calls reuse storage without churn.
    fn return_from_frame(&mut self) -> Step {
        let (step, pending, ret) = return_frame(
            &self.regs,
            &mut self.frames,
            &mut self.reg_top,
            &mut self.slot_top,
        );
        let Some(pending) = pending else {
            return step;
        };
        let Some(caller) = self.frames.last() else {
            return step;
        };
        let slot_abs =
            |slot_off: u32| usize::try_from(caller.slot_base).unwrap_or(0) + slot_off as usize;
        match pending {
            PendingOut::RetToSlot { slot_off, size } => {
                let abs = slot_abs(slot_off);
                let _ = write_abs(&mut self.arena, abs, ret, size);
            }
            PendingOut::RetToReg { dst, .. } => {
                let reg_base = caller.reg_base;
                set_reg_of(&mut self.regs, reg_base, dst, ret);
            }
            PendingOut::SlotToReg {
                slot_off,
                dst,
                size,
            } => {
                let abs = slot_abs(slot_off);
                if let Some(value) = read_abs(&self.arena, abs, size) {
                    let reg_base = caller.reg_base;
                    set_reg_of(&mut self.regs, reg_base, dst, value);
                }
            }
        }
        step
    }

    /// Copies an aggregate value between a register (used as a ref) and a slot.
    fn copy_aggregate(
        &mut self,
        reg_base: u32,
        slot_base: u32,
        dst: Operand,
        src: Operand,
        size: u32,
    ) -> Result<(), VmError> {
        copy_aggregate(
            &mut self.regs,
            &mut self.arena,
            reg_base,
            slot_base,
            dst,
            src,
            size,
        )
    }

    /// Reads the pointer-sized ref stored in a frame slot.
    fn slot_ref(&self, slot_base: u32, place: u32) -> Result<u64, VmError> {
        slot_ref(&self.arena, slot_base, place)
    }

    /// Resolves a field instruction's object base to an absolute arena offset.
    ///
    /// With `indirect`, the object slot holds a ref that is dereferenced first;
    /// otherwise the frame slot itself is the aggregate base.
    fn field_base(&self, slot_base: u32, place: u32, indirect: bool) -> Result<usize, VmError> {
        field_base(&self.arena, slot_base, place, indirect)
    }

    /// Copies `size` aggregate bytes from an operand to an absolute arena offset.
    ///
    /// A register operand of aggregate type is a ref to the bytes; a slot
    /// operand holds the aggregate directly.
    fn copy_operand_bytes(
        &mut self,
        reg_base: u32,
        slot_base: u32,
        operand: Operand,
        dst_abs: usize,
        size: u32,
    ) -> Result<(), VmError> {
        copy_operand_bytes(
            &self.regs,
            &mut self.arena,
            reg_base,
            slot_base,
            operand,
            dst_abs,
            size,
        )
    }

    /// Executes a compiler leaf builtin.
    ///
    /// Most builtins advance the frame; [`Builtin::Abort`] instead yields
    /// [`Step::Panicked`] so the dispatch loop unwinds to a non-zero exit.
    fn call_builtin(
        &mut self,
        builtin: Builtin,
        ins: &[Operand],
        outs: &[Operand],
        reg_base: u32,
        slot_base: u32,
    ) -> Result<Step, VmError> {
        do_builtin(
            &mut self.regs,
            &mut self.arena,
            builtin,
            ins,
            outs,
            reg_base,
            slot_base,
        )
    }

    /// Reads an operand as its low `size` bytes.
    fn read_scalar(
        &self,
        reg_base: u32,
        slot_base: u32,
        operand: Operand,
        size: u32,
    ) -> Result<u64, VmError> {
        read_scalar(&self.regs, &self.arena, reg_base, slot_base, operand, size)
    }

    /// Writes an operand's low `size` bytes.
    fn write_scalar(
        &mut self,
        reg_base: u32,
        slot_base: u32,
        dst: Operand,
        value: u64,
        size: u32,
    ) -> Result<(), VmError> {
        write_scalar(
            &mut self.regs,
            &mut self.arena,
            reg_base,
            slot_base,
            dst,
            value,
            size,
        )
    }

    /// Reads `size` little-endian bytes at an absolute arena offset.
    fn read_abs(&self, abs: usize, size: u32) -> Option<u64> {
        read_abs(&self.arena, abs, size)
    }

    /// Writes `size` little-endian bytes at an absolute arena offset.
    fn write_abs(&mut self, abs: usize, value: u64, size: u32) -> Result<(), VmError> {
        write_abs(&mut self.arena, abs, value, size)
    }

    /// Copies `size` bytes within the arena (refs are byte offsets).
    fn copy_arena(&mut self, src: usize, dst: usize, size: usize) -> Result<(), VmError> {
        copy_arena(&mut self.arena, src, dst, size)
    }

    /// Reads a register of the given frame (zero when out of range).
    #[inline]
    fn reg(&self, reg_base: u32, reg: u32) -> u64 {
        reg_of(&self.regs, reg_base, reg)
    }

    /// Writes a register of the given frame (ignored when out of range).
    #[inline]
    fn set_reg(&mut self, reg_base: u32, reg: u32, value: u64) {
        set_reg_of(&mut self.regs, reg_base, reg, value);
    }
}

/// Reads a register of a frame (zero when out of range).
#[allow(clippy::inline_always)]
#[inline(always)]
fn reg_of(regs: &[u64], reg_base: u32, reg: u32) -> u64 {
    regs.get(reg_base as usize + reg as usize)
        .copied()
        .unwrap_or(0)
}

/// Writes a register of a frame (ignored when out of range).
#[allow(clippy::inline_always)]
#[inline(always)]
fn set_reg_of(regs: &mut [u64], reg_base: u32, reg: u32, value: u64) {
    if let Some(slot) = regs.get_mut(reg_base as usize + reg as usize) {
        *slot = value;
    }
}

/// Reads `size` little-endian bytes at an absolute arena offset.
#[inline]
fn read_abs(arena: &[u8], abs: usize, size: u32) -> Option<u64> {
    let end = abs.checked_add(size as usize)?;
    arena.get(abs..end).map(le_u64)
}

/// Writes `size` little-endian bytes at an absolute arena offset.
#[inline]
fn write_abs(arena: &mut [u8], abs: usize, value: u64, size: u32) -> Result<(), VmError> {
    let end = abs
        .checked_add(size as usize)
        .ok_or(VmError::BadRef(abs as u64))?;
    let bytes = arena.get_mut(abs..end).ok_or(VmError::BadRef(abs as u64))?;
    store_le(bytes, value);
    Ok(())
}

/// Copies `size` bytes within the arena (refs are byte offsets).
fn copy_arena(arena: &mut [u8], src: usize, dst: usize, size: usize) -> Result<(), VmError> {
    let src_end = src.checked_add(size).ok_or(VmError::BadRef(src as u64))?;
    let dst_end = dst.checked_add(size).ok_or(VmError::BadRef(dst as u64))?;
    if src_end > arena.len() {
        return Err(VmError::BadRef(src as u64));
    }
    if dst_end > arena.len() {
        return Err(VmError::BadRef(dst as u64));
    }
    arena.copy_within(src..src_end, dst);
    Ok(())
}

/// Reads the pointer-sized ref stored in a frame slot.
#[inline]
fn slot_ref(arena: &[u8], slot_base: u32, place: u32) -> Result<u64, VmError> {
    let abs = slot_base as usize + place as usize;
    read_abs(arena, abs, VM_POINTER_BYTES).ok_or(VmError::BadRef(abs as u64))
}

/// Resolves a field instruction's object base to an absolute arena offset.
fn field_base(arena: &[u8], slot_base: u32, place: u32, indirect: bool) -> Result<usize, VmError> {
    let slot_abs = slot_base as usize + place as usize;
    if indirect {
        let ref_off =
            read_abs(arena, slot_abs, VM_POINTER_BYTES).ok_or(VmError::BadRef(slot_abs as u64))?;
        usize::try_from(ref_off).map_err(|_| VmError::BadRef(ref_off))
    } else {
        Ok(slot_abs)
    }
}

/// Reads an operand as its low `size` bytes.
#[inline]
fn read_scalar(
    regs: &[u64],
    arena: &[u8],
    reg_base: u32,
    slot_base: u32,
    operand: Operand,
    size: u32,
) -> Result<u64, VmError> {
    match operand {
        Operand::Reg(reg) => Ok(reg_of(regs, reg_base, reg)),
        Operand::Slot(off) => {
            let abs = slot_base as usize + off as usize;
            read_abs(arena, abs, size).ok_or(VmError::BadRef(abs as u64))
        }
    }
}

/// Writes an operand's low `size` bytes.
#[inline]
fn write_scalar(
    regs: &mut [u64],
    arena: &mut [u8],
    reg_base: u32,
    slot_base: u32,
    dst: Operand,
    value: u64,
    size: u32,
) -> Result<(), VmError> {
    match dst {
        Operand::Reg(reg) => {
            set_reg_of(regs, reg_base, reg, value & mask(size));
            Ok(())
        }
        Operand::Slot(off) => {
            let abs = slot_base as usize + off as usize;
            write_abs(arena, abs, value, size)
        }
    }
}

/// Copies an aggregate value between a register (used as a ref) and a slot.
fn copy_aggregate(
    regs: &mut [u64],
    arena: &mut [u8],
    reg_base: u32,
    slot_base: u32,
    dst: Operand,
    src: Operand,
    size: u32,
) -> Result<(), VmError> {
    match (dst, src) {
        (Operand::Slot(dst_off), Operand::Reg(src_reg)) => {
            let ref_off = reg_of(regs, reg_base, src_reg);
            let src_abs = usize::try_from(ref_off).map_err(|_| VmError::BadRef(ref_off))?;
            let dst_abs = slot_base as usize + dst_off as usize;
            copy_arena(arena, src_abs, dst_abs, size as usize)
        }
        (Operand::Reg(dst_reg), Operand::Slot(src_off)) => {
            let addr = u64::from(slot_base) + u64::from(src_off);
            set_reg_of(regs, reg_base, dst_reg, addr);
            Ok(())
        }
        (Operand::Slot(dst_off), Operand::Slot(src_off)) => {
            let dst_abs = slot_base as usize + dst_off as usize;
            let src_abs = slot_base as usize + src_off as usize;
            copy_arena(arena, src_abs, dst_abs, size as usize)
        }
        (Operand::Reg(dst_reg), Operand::Reg(src_reg)) => {
            let value = reg_of(regs, reg_base, src_reg);
            set_reg_of(regs, reg_base, dst_reg, value);
            Ok(())
        }
    }
}

/// Copies `size` aggregate bytes from an operand to an absolute arena offset.
fn copy_operand_bytes(
    regs: &[u64],
    arena: &mut [u8],
    reg_base: u32,
    slot_base: u32,
    operand: Operand,
    dst_abs: usize,
    size: u32,
) -> Result<(), VmError> {
    let src_abs = match operand {
        Operand::Reg(reg) => {
            let ref_off = reg_of(regs, reg_base, reg);
            usize::try_from(ref_off).map_err(|_| VmError::BadRef(ref_off))?
        }
        Operand::Slot(off) => slot_base as usize + off as usize,
    };
    copy_arena(arena, src_abs, dst_abs, size as usize)
}

/// Copies a scalar or aggregate operand into an absolute slot offset.
fn move_to_slot(
    regs: &[u64],
    arena: &mut [u8],
    reg_base: u32,
    slot_base: u32,
    operand: Operand,
    size: u32,
    dst_abs: usize,
) -> Result<(), VmError> {
    if size <= VM_POINTER_BYTES {
        let value = read_scalar(regs, arena, reg_base, slot_base, operand, size)?;
        write_abs(arena, dst_abs, value, size)
    } else {
        match operand {
            Operand::Reg(reg) => {
                let ref_off = reg_of(regs, reg_base, reg);
                let src = usize::try_from(ref_off).map_err(|_| VmError::BadRef(ref_off))?;
                copy_arena(arena, src, dst_abs, size as usize)
            }
            Operand::Slot(off) => {
                let src_abs = slot_base as usize + off as usize;
                copy_arena(arena, src_abs, dst_abs, size as usize)
            }
        }
    }
}

/// Writes only the running frame's return pc (used by `call`; `last_pc` is
/// debugger-only and not needed while a callee runs).
#[inline]
fn store_pc(frames: &mut [Frame], func_index: u32, next: u32) {
    if let Some(frame) = frames.last_mut() {
        if frame.func == func_index {
            frame.pc = next;
        }
    }
}

/// Synchronizes a frame's `pc`/`last_pc`, keyed on `func_index`.
fn store_frame(frames: &mut [Frame], func_index: u32, pc: u32, next: u32) {
    if let Some(frame) = frames.last_mut() {
        if frame.func == func_index {
            frame.pc = next;
            frame.last_pc = Some(pc);
        }
    }
}

/// Pushes a callee frame, binding its inputs and out-ref region.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::inline_always
)]
#[inline(always)]
fn do_call(
    program: &Program,
    regs: &mut Vec<u64>,
    arena: &mut Vec<u8>,
    frames: &mut Vec<Frame>,
    reg_top: &mut u32,
    slot_top: &mut u32,
    target: u32,
    ins: &[Operand],
    outs: &[Operand],
    caller_reg_base: u32,
    caller_slot_base: u32,
    out_dst: Option<u32>,
) -> Result<(), VmError> {
    let callee = program
        .funcs
        .get(target as usize)
        .ok_or(VmError::BadRef(u64::from(target)))?;
    if callee.is_extern {
        return Err(VmError::ExternCall(callee.name.clone()));
    }
    if frames.len() >= MAX_CALL_DEPTH as usize {
        return Err(VmError::StackOverflow);
    }
    // The callee frame begins at the current logical tops. Grow the backing
    // vectors only when the high-water mark is not yet reached; registers are
    // SSA and need no re-zeroing, so reuse their stale bits. The slot region
    // must be re-zeroed because `alloca` locals start at zero.
    let reg_base = *reg_top;
    let slot_base = *slot_top;
    let reg_end = reg_base
        .checked_add(callee.n_regs)
        .ok_or(VmError::BadRef(u64::from(reg_base)))?;
    let slot_end = slot_base
        .checked_add(callee.slot_bytes)
        .ok_or(VmError::BadRef(u64::from(slot_base)))?;
    if regs.len() < reg_end as usize {
        regs.resize(reg_end as usize, 0);
    }
    if arena.len() < slot_end as usize {
        arena.resize(slot_end as usize, 0);
    }
    arena[slot_base as usize..slot_end as usize].fill(0);
    *reg_top = reg_end;
    *slot_top = slot_end;
    if let Some(arg_reg) = callee.arg_reg {
        // Register-argument ABI: place the single input in the callee register.
        if let Some(&operand) = ins.first() {
            let size = callee.in_sizes.first().copied().unwrap_or(VM_POINTER_BYTES);
            let value = read_scalar(
                regs,
                arena,
                caller_reg_base,
                caller_slot_base,
                operand,
                size,
            )?;
            set_reg_of(regs, reg_base, arg_reg, value);
        }
    } else {
        for (index, operand) in ins.iter().enumerate() {
            let (Some(&dst_off), Some(&size)) =
                (callee.in_offsets.get(index), callee.in_sizes.get(index))
            else {
                break;
            };
            let dst_abs = slot_base as usize + dst_off as usize;
            move_to_slot(
                regs,
                arena,
                caller_reg_base,
                caller_slot_base,
                *operand,
                size,
                dst_abs,
            )?;
        }
    }
    // Fill the out-ref region: one u64 absolute arena offset per output. A
    // register-return callee has no out-ref region.
    if callee.ret_reg.is_none() {
        for (index, operand) in outs.iter().enumerate() {
            let abs = match operand {
                Operand::Slot(off) => u64::from(caller_slot_base) + u64::from(*off),
                Operand::Reg(reg) => reg_of(regs, caller_reg_base, *reg),
            };
            let cell = u32::try_from(index)
                .ok()
                .and_then(|index| index.checked_mul(VM_POINTER_BYTES))
                .and_then(|off| slot_base.checked_add(off))
                .ok_or(VmError::BadRef(u64::from(slot_base)))?;
            write_abs(
                arena,
                usize::try_from(cell).unwrap_or(0),
                abs,
                VM_POINTER_BYTES,
            )?;
        }
    }
    // Arrange the caller-side output placement applied on return.
    let out_size = callee.out_sizes.first().copied().unwrap_or(0);
    let out_slot = outs.first().and_then(|operand| match operand {
        Operand::Slot(off) => Some(*off),
        Operand::Reg(_) => None,
    });
    let pending = match (callee.ret_reg.is_some(), out_dst, out_slot) {
        (true, Some(dst), _) => Some(PendingOut::RetToReg { dst }),
        (true, None, Some(slot_off)) => Some(PendingOut::RetToSlot {
            slot_off,
            size: out_size,
        }),
        (false, Some(dst), Some(slot_off)) => Some(PendingOut::SlotToReg {
            slot_off,
            dst,
            size: out_size,
        }),
        _ => None,
    };
    frames.push(Frame {
        func: target,
        reg_base,
        slot_base,
        pc: 0,
        last_pc: None,
        ret_reg: callee.ret_reg,
        out_copy: pending,
    });
    Ok(())
}

/// Pops the running frame, restoring the logical region tops to the caller's.
///
/// The register file and arena are *not* truncated: they stay at their
/// high-water length so a later call at the same depth reuses the same storage
/// without reallocation or re-zeroing (registers are SSA; the callee re-zeroes
/// its own slot region on entry).
fn return_frame(
    regs: &[u64],
    frames: &mut Vec<Frame>,
    reg_top: &mut u32,
    slot_top: &mut u32,
) -> (Step, Option<PendingOut>, u64) {
    let Some(frame) = frames.pop() else {
        return (Step::Returned, None, 0);
    };
    let ret = frame
        .ret_reg
        .map_or(0, |reg| reg_of(regs, frame.reg_base, reg));
    if frames.is_empty() {
        return (Step::Returned, frame.out_copy, ret);
    }
    *reg_top = frame.reg_base;
    *slot_top = frame.slot_base;
    (Step::Advanced, frame.out_copy, ret)
}

/// Executes a compiler leaf builtin.
fn do_builtin(
    regs: &mut [u64],
    arena: &mut [u8],
    builtin: Builtin,
    ins: &[Operand],
    outs: &[Operand],
    reg_base: u32,
    slot_base: u32,
) -> Result<Step, VmError> {
    match builtin {
        Builtin::PtrAdd => {
            let (base, offset) = match ins {
                [base, offset] => (*base, *offset),
                _ => lilac_error::bug!(lilac_span::Span::dummy(), "@ptr_add expects two inputs"),
            };
            let base = read_scalar(regs, arena, reg_base, slot_base, base, VM_POINTER_BYTES)?;
            let offset = read_scalar(regs, arena, reg_base, slot_base, offset, VM_POINTER_BYTES)?;
            let Some(Operand::Slot(out)) = outs.first().copied() else {
                lilac_error::bug!(lilac_span::Span::dummy(), "@ptr_add expects one output");
            };
            write_scalar(
                regs,
                arena,
                reg_base,
                slot_base,
                Operand::Slot(out),
                base.wrapping_add(offset),
                VM_POINTER_BYTES,
            )?;
            Ok(Step::Advanced)
        }
        Builtin::Memcpy => {
            let (dst, src, len) = match ins {
                [dst, src, len] => (*dst, *src, *len),
                _ => lilac_error::bug!(lilac_span::Span::dummy(), "@memcpy expects three inputs"),
            };
            let dst = read_scalar(regs, arena, reg_base, slot_base, dst, VM_POINTER_BYTES)?;
            let src = read_scalar(regs, arena, reg_base, slot_base, src, VM_POINTER_BYTES)?;
            let len = read_scalar(regs, arena, reg_base, slot_base, len, VM_POINTER_BYTES)?;
            let dst_abs = usize::try_from(dst).map_err(|_| VmError::BadRef(dst))?;
            let src_abs = usize::try_from(src).map_err(|_| VmError::BadRef(src))?;
            let len = usize::try_from(len).map_err(|_| VmError::BadRef(len))?;
            copy_arena(arena, src_abs, dst_abs, len)?;
            Ok(Step::Advanced)
        }
        Builtin::Abort => Ok(trap(b"aborted")),
    }
}

/// Evaluates a width-specialized arithmetic/comparison instruction.
#[allow(clippy::inline_always)]
#[inline(always)]
fn exec_arith(
    regs: &mut [u64],
    op: ArithOp,
    reg_base: u32,
    dst: u32,
    lhs: u32,
    rhs: u32,
    width: u8,
) {
    let size = u32::from(width) / 8;
    let left = reg_of(regs, reg_base, lhs);
    let right = reg_of(regs, reg_base, rhs);
    let value = eval_arith(op, left, right, size);
    set_reg_of(regs, reg_base, dst, value & mask(size));
}

/// The source line whose `pc` is the greatest not exceeding `pc`.
fn line_for(func: &crate::isa::Func, pc: u32) -> Option<crate::isa::Line> {
    func.lines.iter().rev().find(|line| line.pc <= pc).cloned()
}

/// Renders the diagnostic line the VM prints when it traps.
///
/// Both the `Panic` terminator and `@lilac_abort` share this prefix so their
/// output is consistent (`panic: <message>`).
fn trap_message(message: &[u8]) -> String {
    format!("panic: {}", String::from_utf8_lossy(message))
}

/// Prints `message` to stderr and yields the panic step for [`Machine::run`].
fn trap(message: &[u8]) -> Step {
    eprintln!("{}", trap_message(message));
    Step::Panicked(ABORT_CODE)
}

/// Builds a register or slot [`Operand`] from a raw value and slot flag.
const fn operate(value: u32, is_slot: bool) -> Operand {
    if is_slot {
        Operand::Slot(value)
    } else {
        Operand::Reg(value)
    }
}

/// Reads a one-operand instruction's operand and its word count.
#[inline]
fn read_ops1(code: &[u32], pc: u32, wide: bool) -> Result<(u32, u32), VmError> {
    if wide {
        let ext0 = *code.get(pc as usize + 1).ok_or(VmError::BadInstr(pc))?;
        Ok((ext0 & 0xFFFF, 2))
    } else {
        Ok(((code[pc as usize] >> 8) & 0xFF, 1))
    }
}

/// Reads a two-operand instruction's operands and its word count.
#[inline]
fn read_ops2(code: &[u32], pc: u32, wide: bool) -> Result<([u32; 2], u32), VmError> {
    if wide {
        let ext0 = *code.get(pc as usize + 1).ok_or(VmError::BadInstr(pc))?;
        Ok(([ext0 & 0xFFFF, (ext0 >> 16) & 0xFFFF], 2))
    } else {
        let w = code[pc as usize];
        Ok(([(w >> 8) & 0xFF, (w >> 16) & 0xFF], 1))
    }
}

/// Reads a three-operand instruction's operands and its word count.
#[inline]
fn read_ops3(code: &[u32], pc: u32, wide: bool) -> Result<([u32; 3], u32), VmError> {
    if wide {
        let ext0 = *code.get(pc as usize + 1).ok_or(VmError::BadInstr(pc))?;
        let ext1 = *code.get(pc as usize + 2).ok_or(VmError::BadInstr(pc))?;
        Ok(([ext0 & 0xFFFF, (ext0 >> 16) & 0xFFFF, ext1], 3))
    } else {
        let w = code[pc as usize];
        Ok(([(w >> 8) & 0xFF, (w >> 16) & 0xFF, (w >> 24) & 0xFF], 1))
    }
}

/// Resolves a call-site index into its site and borrowed operand slices.
///
/// Returns `None` for an out-of-range call site or operand range, so the
/// interpreter can report a clean [`VmError::BadInstr`] instead of panicking.
fn resolve_call<'a>(
    call_sites: &'a [CallSite],
    operands: &'a [Operand],
    index: usize,
) -> Option<(&'a CallSite, &'a [Operand], &'a [Operand])> {
    let site = call_sites.get(index)?;
    let ins = operands.get(site.ins.start as usize..site.ins.end as usize)?;
    let outs = operands.get(site.outs.start as usize..site.outs.end as usize)?;
    Some((site, ins, outs))
}

/// A bit mask covering `size` bytes.
#[allow(clippy::inline_always)]
#[inline(always)]
fn mask(size: u32) -> u64 {
    if size >= 8 {
        u64::MAX
    } else if size == 0 {
        0
    } else {
        (1_u64 << (size * 8)) - 1
    }
}

/// Reads up to eight little-endian bytes.
fn le_u64(bytes: &[u8]) -> u64 {
    let mut buf = [0_u8; 8];
    let len = bytes.len().min(8);
    buf[..len].copy_from_slice(&bytes[..len]);
    u64::from_le_bytes(buf)
}

/// Writes `value`'s low bytes little-endian.
fn store_le(bytes: &mut [u8], value: u64) {
    let le = value.to_le_bytes();
    let len = bytes.len().min(8);
    bytes[..len].copy_from_slice(&le[..len]);
}

/// Sign-extends the low `size` bytes of `value` to `i64`.
#[allow(clippy::cast_possible_wrap)]
fn sign_extend(value: u64, size: u32) -> i64 {
    if size == 0 {
        return 0;
    }
    let bits = size.min(8) * 8;
    let shift = 64 - bits;
    ((value << shift) as i64) >> shift
}

/// The byte size for an integer width-class index (`0..4`).
const fn int_size(idx: u8) -> u32 {
    match idx {
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 8,
    }
}

/// The byte size for a float width-class index (`0..2`).
const fn float_size(idx: u8) -> u32 {
    if idx == 0 {
        4
    } else {
        8
    }
}

/// Evaluates a width-specialized arithmetic opcode directly from its opcode
/// byte, returning the final (already masked) result.
///
/// This is the flattened dispatch (spec §3.3): one match, no intermediate
/// `ArithOp` sub-match. `None` means the byte is not an arithmetic opcode.
#[allow(clippy::inline_always, clippy::too_many_lines)]
#[inline(always)]
fn eval_arith_code(code: u8, left: u64, right: u64) -> Option<u64> {
    let value = match code {
        // Signed integer, widths 8/16/32/64.
        0x40..=0x43 => {
            let m = mask(int_size(code & 3));
            left.wrapping_add(right) & m
        }
        0x44..=0x47 => {
            let m = mask(int_size(code & 3));
            left.wrapping_sub(right) & m
        }
        0x48..=0x4B => {
            let m = mask(int_size(code & 3));
            left.wrapping_mul(right) & m
        }
        0x4C..=0x4F => {
            let size = int_size(code & 3);
            let divisor = sign_extend(right, size);
            if divisor == 0 {
                0
            } else {
                sign_extend(left, size)
                    .wrapping_div(divisor)
                    .cast_unsigned()
                    & mask(size)
            }
        }
        0x50..=0x53 => {
            let size = int_size(code & 3);
            let divisor = sign_extend(right, size);
            if divisor == 0 {
                0
            } else {
                sign_extend(left, size)
                    .wrapping_rem(divisor)
                    .cast_unsigned()
                    & mask(size)
            }
        }
        0x54..=0x57 => {
            let size = int_size(code & 3);
            u64::from(sign_extend(left, size) < sign_extend(right, size))
        }
        0x58..=0x5B => {
            let size = int_size(code & 3);
            u64::from(sign_extend(left, size) > sign_extend(right, size))
        }
        // Unsigned/bit-equality, widths 8/16/32/64 (operands are masked).
        0x20..=0x23 => {
            let m = mask(int_size(code & 3));
            (left & m).checked_div(right & m).unwrap_or(0)
        }
        0x24..=0x27 => {
            let m = mask(int_size(code & 3));
            (left & m).checked_rem(right & m).unwrap_or(0)
        }
        0x28..=0x2B => {
            let m = mask(int_size(code & 3));
            u64::from((left & m) == (right & m))
        }
        0x2C..=0x2F => {
            let m = mask(int_size(code & 3));
            u64::from((left & m) < (right & m))
        }
        0x30..=0x33 => {
            let m = mask(int_size(code & 3));
            u64::from((left & m) > (right & m))
        }
        // Float, widths 32/64.
        0x60..=0x61 => eval_float_arith(ArithOp::FAdd, left, right, float_size(code & 1)),
        0x62..=0x63 => eval_float_arith(ArithOp::FSub, left, right, float_size(code & 1)),
        0x64..=0x65 => eval_float_arith(ArithOp::FMul, left, right, float_size(code & 1)),
        0x66..=0x67 => eval_float_arith(ArithOp::FDiv, left, right, float_size(code & 1)),
        0x68..=0x69 => eval_float_arith(ArithOp::FMod, left, right, float_size(code & 1)),
        0x6A..=0x6B => eval_float_arith(ArithOp::FLt, left, right, float_size(code & 1)),
        0x6C..=0x6D => eval_float_arith(ArithOp::FGt, left, right, float_size(code & 1)),
        _ => return None,
    };
    Some(value)
}

/// Evaluates an integer arithmetic/comparison operator.
///
/// `size` is the operand width (from `Arith.operand_ty`): signed
/// division/remainder and signed comparisons sign-extend their operands at that
/// width, matching the LLVM backend's operand-typed `sdiv`/`srem`/`icmp slt`.
/// Division by zero yields zero.
#[allow(clippy::inline_always)]
#[inline(always)]
fn eval_int_arith(op: ArithOp, left: u64, right: u64, size: u32) -> u64 {
    let m = mask(size);
    match op {
        ArithOp::IAdd => left.wrapping_add(right) & m,
        ArithOp::ISub => left.wrapping_sub(right) & m,
        ArithOp::IMul => left.wrapping_mul(right) & m,
        ArithOp::IDiv => {
            let divisor = sign_extend(right, size);
            if divisor == 0 {
                0
            } else {
                sign_extend(left, size)
                    .wrapping_div(divisor)
                    .cast_unsigned()
                    & m
            }
        }
        ArithOp::IDivU => (left & m).checked_div(right & m).unwrap_or(0),
        ArithOp::IMod => {
            let divisor = sign_extend(right, size);
            if divisor == 0 {
                0
            } else {
                sign_extend(left, size)
                    .wrapping_rem(divisor)
                    .cast_unsigned()
                    & m
            }
        }
        ArithOp::IModU => (left & m).checked_rem(right & m).unwrap_or(0),
        ArithOp::Eq => u64::from((left & m) == (right & m)),
        ArithOp::ILt => u64::from(sign_extend(left, size) < sign_extend(right, size)),
        ArithOp::ILtU => u64::from((left & m) < (right & m)),
        ArithOp::IGt => u64::from(sign_extend(left, size) > sign_extend(right, size)),
        ArithOp::IGtU => u64::from((left & m) > (right & m)),
        ArithOp::FAdd
        | ArithOp::FSub
        | ArithOp::FMul
        | ArithOp::FDiv
        | ArithOp::FMod
        | ArithOp::FLt
        | ArithOp::FGt => 0,
    }
}

/// Evaluates a floating-point arithmetic/comparison operator over bit patterns.
///
/// `size` is the operand width (from `Arith.operand_ty`): `#bit32` operands are
/// interpreted as `f32` and `#bit64`/`#bit_native` as `f64`, matching the LLVM
/// backend. Any other width falls back to `f64`.
#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::inline_always)]
#[inline(always)]
fn eval_float_arith(op: ArithOp, left: u64, right: u64, size: u32) -> u64 {
    if size == 4 {
        let a = f32::from_bits(left as u32);
        let b = f32::from_bits(right as u32);
        match op {
            ArithOp::FLt => u64::from(a < b),
            ArithOp::FGt => u64::from(a > b),
            ArithOp::FAdd => u64::from((a + b).to_bits()),
            ArithOp::FSub => u64::from((a - b).to_bits()),
            ArithOp::FMul => u64::from((a * b).to_bits()),
            ArithOp::FDiv => u64::from((a / b).to_bits()),
            _ => u64::from((a % b).to_bits()),
        }
    } else {
        let a = f64::from_bits(left);
        let b = f64::from_bits(right);
        match op {
            ArithOp::FLt => u64::from(a < b),
            ArithOp::FGt => u64::from(a > b),
            ArithOp::FAdd => (a + b).to_bits(),
            ArithOp::FSub => (a - b).to_bits(),
            ArithOp::FMul => (a * b).to_bits(),
            ArithOp::FDiv => (a / b).to_bits(),
            _ => (a % b).to_bits(),
        }
    }
}

/// Dispatches to the integer or floating-point evaluator for `op`.
#[allow(clippy::inline_always)]
#[inline(always)]
fn eval_arith(op: ArithOp, left: u64, right: u64, size: u32) -> u64 {
    if matches!(
        op,
        ArithOp::FAdd
            | ArithOp::FSub
            | ArithOp::FMul
            | ArithOp::FDiv
            | ArithOp::FMod
            | ArithOp::FLt
            | ArithOp::FGt
    ) {
        eval_float_arith(op, left, right, size)
    } else {
        eval_int_arith(op, left, right, size)
    }
}

#[cfg(test)]
mod tests {
    use super::{trap_message, Machine, Step, VmError};
    use crate::compile::compile_src;
    use crate::decode::{decode, Decoded};
    use crate::isa::{self, Instr, Operand};

    fn run_src(src: &str) -> Result<i32, VmError> {
        let program = compile_src(src);
        Machine::new(&program)?.run()
    }

    /// Runs `program` to completion.
    fn run(program: &isa::Program) -> Result<i32, VmError> {
        Machine::new(program)?.run()
    }

    /// Runs `src`, capturing the last value seen in `place` while its frame is
    /// live (the frame is popped by the terminating `Return`).
    fn run_and_capture_place(src: &str, place: &str) -> Result<Option<u64>, VmError> {
        let program = compile_src(src);
        let mut machine = Machine::new(&program)?;
        let mut captured = None;
        loop {
            if let Some(value) = machine.read_place(place) {
                captured = Some(value);
            }
            match machine.step()? {
                Step::Advanced => {}
                Step::Returned | Step::Panicked(_) => break,
            }
        }
        Ok(captured)
    }

    #[test]
    fn runs_empty_main() {
        assert_eq!(run_src("main: function () nothing {\n}\n").expect("run"), 0);
    }

    #[test]
    fn runs_direct_call_and_return_value() {
        let src =
            "helper: function (a: @bit_native) ret: @bit_native {\n    ret = @i_add(a, 0x1)\n}\n\
                   main: function () nothing {\n    x: @bit_native = helper(0x41)\n}\n";
        let program = compile_src(src);
        let mut machine = Machine::new(&program).expect("entry");
        // Drive the call and capture the caller's out place while its frame is
        // still live: `x` must hold `0x41 + 1` only if the argument binding and
        // the `Return` out-copy are both correct.
        let mut x = None;
        loop {
            if let Some(value) = machine.read_place("x") {
                x = Some(value);
            }
            match machine.step().expect("step") {
                Step::Returned => break,
                Step::Advanced => {}
                Step::Panicked(code) => unreachable!("unexpected panic {code}"),
            }
        }
        assert_eq!(x, Some(0x42), "argument binding / return out-copy");
    }

    /// The inner dispatch loop ([`Machine::run`]) and the debugger's
    /// single-instruction path ([`Machine::step`]) must agree on both the exit
    /// code and the values written through call/return.
    ///
    /// Recursion is not constant-folded, so `fib` survives compilation and the
    /// step-driven loop exercises real call/return frames; `out` is captured
    /// while its frame is still live (the terminating `Return` pops it).
    #[test]
    fn run_and_step_agree_on_result() {
        // `fib(18)` is beyond the constant folder's effort budget, so `fib`
        // survives compilation (matching `hot_path_has_no_type_lookups`).
        let src = "fib: function (n: @bit_native) r: @bit_native {\n    if @i_less_than(n, 0x2) {\n        r = n\n    } else {\n        r = @i_add(fib(@i_sub(n, 0x1)), fib(@i_sub(n, 0x2)))\n    }\n}\n\nmain: function () nothing {\n    out: @bit_native = fib(0x12)\n}\n";
        let program = compile_src(src);
        assert!(
            program.funcs.iter().any(|func| func.name == "fib"),
            "fib must survive folding to exercise call/return"
        );

        // The hot inner loop.
        let run_code = Machine::new(&program).expect("entry").run().expect("run");

        // The debugger's step()-driven path, capturing `out` while live.
        let mut machine = Machine::new(&program).expect("entry");
        let mut out = None;
        let step_code = loop {
            if let Some(value) = machine.read_place("out") {
                out = Some(value);
            }
            match machine.step().expect("step") {
                Step::Advanced => {}
                Step::Returned => break 0,
                Step::Panicked(code) => break code,
            }
        };

        assert_eq!(
            run_code, step_code,
            "run() and step() agree on the exit code"
        );
        assert_eq!(out, Some(2584), "fib(18) copied through call/return");
    }

    /// The call/return path binds arguments and places results from the
    /// precomputed [`crate::isa::Func`] metadata, never a `type_size` lookup.
    /// A compiled `fib` has one scalar input and one scalar output, so it uses
    /// the register ABI: the input and output live in reserved registers and
    /// there is no slot input/out-ref region.
    #[test]
    fn hot_path_has_no_type_lookups() {
        let src = "fib: function (n: @bit_native) r: @bit_native {\n    if @i_less_than(n, 0x2) {\n        r = n\n    } else {\n        r = @i_add(fib(@i_sub(n, 0x1)), fib(@i_sub(n, 0x2)))\n    }\n}\n\nmain: function () nothing {\n    out: @bit_native = fib(0x12)\n}\n";
        let program = compile_src(src);
        let fib = program
            .funcs
            .iter()
            .find(|func| func.name == "fib")
            .expect("fib function");
        assert!(fib.arg_reg.is_some(), "single scalar input uses a register");
        assert!(
            fib.ret_reg.is_some(),
            "single scalar output uses a register"
        );
        assert!(
            fib.in_offsets.is_empty(),
            "register-arg inputs are not in the slot region"
        );
        assert_eq!(fib.out_sizes, vec![8], "one pointer-wide output");
        let out = run_and_capture_place(src, "out").expect("run");
        assert_eq!(out, Some(2584), "fib(18) copied through call/return");
    }

    /// Builds a two-function program for the out-by-reference tests: `main`
    /// (its `%x` at slot byte 8, pre-set to 0x7) calls function 1 with `%x` as
    /// its out place, supplying `callee_code` as the callee body.
    fn out_ref_program(callee_code: &[Instr], callee_slots: u32) -> isa::Program {
        let types = vec![isa::TypeInfo {
            name: "#bit_native".to_string(),
            size: 8,
            is_ref: false,
        }];
        let (main_code, main_calls, main_ops) = isa::words_of(
            &types,
            &[
                Instr::Const {
                    dst: 0,
                    imm: 0x7,
                    ty: 0,
                },
                Instr::Copy {
                    dst: Operand::Slot(8),
                    src: Operand::Reg(0),
                    ty: 0,
                },
                Instr::Call {
                    func: 1,
                    ins: Vec::new(),
                    outs: vec![8],
                },
                Instr::Return,
            ],
        );
        let (callee_code, callee_calls, callee_ops) = isa::words_of(&types, callee_code);
        let mut program = isa::Program {
            version: isa::LBC_VERSION.to_string(),
            target: isa::VM_TARGET.to_string(),
            types,
            data: Vec::new(),
            globals: Vec::new(),
            funcs: vec![
                isa::Func {
                    name: "main".to_string(),
                    is_extern: false,
                    ins: Vec::new(),
                    outs: Vec::new(),
                    in_offsets: Vec::new(),
                    in_sizes: Vec::new(),
                    out_offset: 0,
                    out_sizes: Vec::new(),
                    n_regs: 1,
                    slot_bytes: 16,
                    arg_reg: None,
                    ret_reg: None,
                    code: main_code,
                    call_sites: main_calls,
                    operands: main_ops,
                    lines: Vec::new(),
                    place_names: vec![("x".to_string(), 8)],
                },
                isa::Func {
                    name: "callee".to_string(),
                    is_extern: false,
                    ins: Vec::new(),
                    outs: vec![0],
                    in_offsets: Vec::new(),
                    in_sizes: Vec::new(),
                    out_offset: 0,
                    out_sizes: Vec::new(),
                    n_regs: 3,
                    slot_bytes: callee_slots,
                    arg_reg: None,
                    ret_reg: None,
                    code: callee_code,
                    call_sites: callee_calls,
                    operands: callee_ops,
                    lines: Vec::new(),
                    place_names: Vec::new(),
                },
            ],
        };
        program.rebuild_layouts();
        program.verify().expect("verify");
        program
    }

    /// Drives `program` and captures the last value seen in `main`'s `%x`.
    fn run_and_capture_x(program: &isa::Program) -> Option<u64> {
        let mut machine = Machine::new(program).expect("entry");
        let mut captured = None;
        loop {
            if let Some(value) = machine.read_place("x") {
                captured = Some(value);
            }
            match machine.step().expect("step") {
                Step::Advanced => {}
                Step::Returned | Step::Panicked(_) => break,
            }
        }
        captured
    }

    /// An output the callee never writes leaves the caller's own storage
    /// unchanged (LLVM parity), not the callee's zeroed slot.
    #[test]
    fn unwritten_output_leaves_caller_storage_unchanged() {
        // `callee` has one out place but its body is a bare `return`.
        let program = out_ref_program(&[Instr::Return], 8);
        assert_eq!(
            run_and_capture_x(&program),
            Some(0x7),
            "unwritten output must not overwrite the caller"
        );
    }

    /// Reading an output before a branch must not corrupt later block starts.
    ///
    /// `x = r` (an output read) expands to `Load` + `Copy`, so the entry block
    /// emits more instructions than one per LIR instruction. Block starts must
    /// come from real emission or the `branch.if` targets land wrong (on the
    /// branch itself). `f` is reached with a runtime-derived argument so the
    /// constant folder cannot fold it away. The loop is step-capped so a
    /// corrupt target fails as a hang instead of hanging the suite.
    #[test]
    fn output_read_before_branch_targets_correctly() {
        let src = "f: function (n: @bit_native) r: @bit_native {\n    r = n\n    x: @bit_native = r\n    if @i_less_than(x, 0x5) {\n        r = 0x1\n    } else {\n        r = 0x2\n    }\n}\nmain: function () nothing {\n    part: @byte_buffer = \"x\"\n    msg: @format_args = $\"a{part}b\"\n    n: @bit_native = @format_args_count(msg)\n    a: @bit_native = f(n)\n}\n";
        let program = compile_src(src);
        assert!(
            program.funcs.iter().any(|func| func.name == "f"),
            "`f` must survive folding to exercise the branch layout"
        );
        let mut machine = Machine::new(&program).expect("entry");
        let mut a = None;
        let mut steps = 0_u32;
        loop {
            if let Some(value) = machine.read_place("a") {
                a = Some(value);
            }
            steps += 1;
            assert!(steps < 100_000, "did not terminate: corrupt branch target?");
            match machine.step().expect("step") {
                Step::Advanced => {}
                Step::Returned | Step::Panicked(_) => break,
            }
        }
        assert_eq!(a, Some(0x1), "f(3) takes the then-branch");
    }

    /// Runtime-level aliasing: `addr %out` is the caller's storage address, so
    /// a store through the taken address updates the caller's place.
    #[test]
    fn out_place_address_aliases_caller_storage() {
        // callee: r0 = ref %out; p = r0; r1 = 0x2a; *p = r1.
        let callee = [
            Instr::Copy {
                dst: Operand::Reg(0),
                src: Operand::Slot(0),
                ty: 0,
            },
            Instr::Copy {
                dst: Operand::Slot(8),
                src: Operand::Reg(0),
                ty: 0,
            },
            Instr::Const {
                dst: 1,
                imm: 0x2a,
                ty: 0,
            },
            Instr::Store {
                src: Operand::Reg(1),
                dst: 8,
                ty: 0,
            },
            Instr::Return,
        ];
        let program = out_ref_program(&callee, 16);
        assert_eq!(
            run_and_capture_x(&program),
            Some(0x2a),
            "store through the out place's address reaches the caller"
        );
    }

    /// The compiler's `Addr %out` branch: source that takes `addr` of an out
    /// place (`ref out`) and stores through it reaches the caller's storage.
    #[test]
    fn addr_of_out_place_is_caller_storage() {
        let src = "set: function () out: @bit8 {\n    out = 0x0\n    p: ref @bit8 = ref out\n    @store_u8(p, 0x0, 0x2a)\n}\nmain: function () nothing {\n    b: @bit8 = set()\n    x: @bit_native = @extend_zero(b)\n}\n";
        let program = compile_src(src);
        assert!(
            program.funcs.iter().any(|func| func.name == "set"),
            "`set` must survive folding to exercise `addr %out`"
        );
        assert_eq!(run_and_capture_place(src, "x").expect("run"), Some(0x2a));
        assert_eq!(run_src(src).expect("run"), 0);
    }

    #[test]
    fn missing_entry_is_clean_error() {
        let program = compile_src("f: function () nothing {\n}\n");
        assert!(matches!(Machine::new(&program), Err(VmError::NoEntry(_))));
    }

    #[test]
    fn recursion_is_depth_capped() {
        let src = "f: function (n: @bit_native) ret: @bit_native {\n    ret = f(n)\n}\n\
                   main: function () nothing {\n    x: @bit_native = f(0x1)\n}\n";
        assert!(matches!(run_src(src), Err(VmError::StackOverflow)));
    }

    #[test]
    fn with_entry_selects_named_function() {
        let program = compile_src("foo: function () nothing {\n}\n");
        assert!(Machine::with_entry(&program, "foo").is_ok());
        assert!(matches!(
            Machine::with_entry(&program, "bar"),
            Err(VmError::NoEntry(_))
        ));
    }

    #[test]
    fn executes_const_into_register() {
        let program = isa::sample_program(vec![
            Instr::Const {
                dst: 0,
                imm: 0x2a,
                ty: 0,
            },
            Instr::Return,
        ]);
        let mut machine = Machine::new(&program).expect("entry");
        assert_eq!(machine.step().expect("step"), Step::Advanced);
        assert_eq!(machine.read_reg(0), Some(0x2a));
        assert_eq!(machine.pc(), 3, "a `const` is three words");
        assert_eq!(machine.step().expect("step"), Step::Returned);
    }

    #[test]
    fn running_past_end_is_clean_error() {
        let program = isa::sample_program(vec![Instr::Const {
            dst: 0,
            imm: 1,
            ty: 0,
        }]);
        let mut machine = Machine::new(&program).expect("entry");
        assert_eq!(machine.step().expect("const"), Step::Advanced);
        assert!(matches!(machine.step(), Err(VmError::BadPc(3))));
    }

    #[test]
    fn unreachable_is_clean_error() {
        let program = isa::sample_program(vec![Instr::Unreachable]);
        let mut machine = Machine::new(&program).expect("entry");
        assert!(matches!(machine.step(), Err(VmError::Unreachable)));
    }

    #[test]
    fn branch_if_selects_fallthrough() {
        let program = isa::sample_program(vec![
            Instr::Const {
                dst: 0,
                imm: 0,
                ty: 0,
            },
            Instr::BranchIf {
                cond: 0,
                then: 3,
                els: 2,
            },
            Instr::Const {
                dst: 0,
                imm: 0xaa,
                ty: 0,
            },
            Instr::Return,
        ]);
        let mut machine = Machine::new(&program).expect("entry");
        machine.step().expect("const");
        machine.step().expect("branch");
        assert_eq!(
            machine.pc(),
            4,
            "else target is the second `const`'s word pc"
        );
        machine.step().expect("const");
        assert_eq!(machine.read_reg(0), Some(0xaa));
    }

    #[test]
    fn extern_direct_call_is_clean_error() {
        let mut program = isa::sample_program(vec![
            Instr::Call {
                func: 1,
                ins: Vec::new(),
                outs: Vec::new(),
            },
            Instr::Return,
        ]);
        program.funcs.push(isa::Func {
            name: "ext".to_string(),
            is_extern: true,
            ins: Vec::new(),
            outs: Vec::new(),
            in_offsets: Vec::new(),
            in_sizes: Vec::new(),
            out_offset: 0,
            out_sizes: Vec::new(),
            n_regs: 0,
            slot_bytes: 0,
            arg_reg: None,
            ret_reg: None,
            code: Vec::new(),
            call_sites: Vec::new(),
            operands: Vec::new(),
            lines: Vec::new(),
            place_names: Vec::new(),
        });
        program.verify().expect("verify");
        let mut machine = Machine::new(&program).expect("entry");
        assert!(matches!(
            machine.run(),
            Err(VmError::ExternCall(name)) if name == "ext"
        ));
    }

    /// A `Panic` terminator returns exit code 101, so `run` yields `Ok(101)`.
    /// M5c has no source-level panic (the `LirTerm::Panic` terminator is not
    /// produced by the frontend), so the program is built directly.
    #[test]
    fn panic_returns_101() {
        assert_eq!(run(&panic_program(b"boom")).expect("run"), 101);
    }

    /// The trap message line is `panic: ` followed by the data constant's bytes.
    #[test]
    fn panic_trap_message_has_prefix_and_data_bytes() {
        assert_eq!(trap_message(b"boom"), "panic: boom");
    }

    /// A call to an `extern` (bodyless) function is a clean VM runtime error,
    /// not a trap. The declaration spelling matches `tests/ui/extern_decl.lilac`.
    #[test]
    fn extern_call_is_clean_error() {
        let src = "puts: extern function (s: @byte_buffer) nothing\n\
                   main: function () nothing {\n    puts(\"hi\")\n}\n";
        assert!(matches!(run_src(src), Err(VmError::ExternCall(name)) if name == "puts"));
    }

    /// A one-function `main` program whose `Panic` references a data constant
    /// holding `message`.
    fn panic_program(message: &[u8]) -> isa::Program {
        let mut program = isa::sample_program(vec![Instr::Panic { data: 0 }]);
        program.data = vec![isa::Data {
            bytes: message.to_vec(),
            global_off: 0,
        }];
        program.globals = message.to_vec();
        program
    }

    #[test]
    fn backtrace_reports_entry_frame() {
        let program = compile_src("main: function () nothing {\n}\n");
        let machine = Machine::new(&program).expect("entry");
        let trace = machine.backtrace();
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].name, "main");
        assert_eq!(trace[0].pc, 0);
    }

    /// `current_line` must report the line of the instruction just executed,
    /// not the fall-through `pc` (the *next* instruction). Executing the last
    /// instruction of source line 2 must still report line 2, not line 3.
    #[test]
    fn current_line_tracks_last_executed_instruction() {
        let mut program = isa::sample_program(vec![
            Instr::Const {
                dst: 0,
                imm: 0x1,
                ty: 0,
            },
            Instr::Const {
                dst: 0,
                imm: 0x2,
                ty: 0,
            },
            Instr::Return,
        ]);
        program.funcs[0].lines = vec![
            isa::Line {
                pc: 0,
                file: "t.lilac".to_string(),
                line: 2,
                col: 1,
            },
            isa::Line {
                pc: 3,
                file: "t.lilac".to_string(),
                line: 3,
                col: 1,
            },
            isa::Line {
                pc: 6,
                file: "t.lilac".to_string(),
                line: 4,
                col: 1,
            },
        ];
        let mut machine = Machine::new(&program).expect("entry");
        assert_eq!(machine.current_line().map(|l| l.line), Some(2));
        machine.step().expect("const");
        assert_eq!(
            machine.current_line().map(|l| l.line),
            Some(2),
            "after executing word 0, still on line 2"
        );
        assert_eq!(machine.pc(), 3);
        machine.step().expect("const");
        assert_eq!(machine.current_line().map(|l| l.line), Some(3));
    }

    /// A suspended caller frame must report its call-site line, not the return
    /// address's line. `main` calls `f` at pc 0 (line 5); the fall-through
    /// return address pc 1 is line 6, so the buggy line would be 6.
    #[test]
    fn backtrace_reports_caller_call_site_line() {
        let mut program = isa::sample_program(vec![
            Instr::Call {
                func: 1,
                ins: Vec::new(),
                outs: Vec::new(),
            },
            Instr::Return,
        ]);
        program.funcs[0].lines = vec![
            isa::Line {
                pc: 0,
                file: "t.lilac".to_string(),
                line: 5,
                col: 1,
            },
            isa::Line {
                pc: 1,
                file: "t.lilac".to_string(),
                line: 6,
                col: 1,
            },
        ];
        let (fcode, fcall, fops) = isa::words_of(&program.types, &[Instr::Return]);
        program.funcs.push(isa::Func {
            name: "f".to_string(),
            is_extern: false,
            ins: Vec::new(),
            outs: Vec::new(),
            in_offsets: Vec::new(),
            in_sizes: Vec::new(),
            out_offset: 0,
            out_sizes: Vec::new(),
            n_regs: 0,
            slot_bytes: 0,
            arg_reg: None,
            ret_reg: None,
            code: fcode,
            call_sites: fcall,
            operands: fops,
            lines: vec![isa::Line {
                pc: 0,
                file: "t.lilac".to_string(),
                line: 10,
                col: 1,
            }],
            place_names: Vec::new(),
        });
        program.verify().expect("verify");
        let mut machine = Machine::new(&program).expect("entry");
        machine.step().expect("call");
        let trace = machine.backtrace();
        assert_eq!(trace.len(), 2);
        assert_eq!(trace[0].name, "main");
        assert_eq!(trace[0].line.as_ref().map(|l| l.line), Some(5));
        assert_eq!(trace[1].name, "f");
        assert_eq!(trace[1].line.as_ref().map(|l| l.line), Some(10));
    }

    /// Builds a one-function `main` program with explicit types and code.
    #[allow(clippy::needless_pass_by_value)]
    fn arith_program(types: Vec<isa::TypeInfo>, code: Vec<Instr>, n_regs: u32) -> isa::Program {
        let (words, call_sites, operands) = isa::words_of(&types, &code);
        isa::Program {
            version: isa::LBC_VERSION.to_string(),
            target: isa::VM_TARGET.to_string(),
            types,
            data: Vec::new(),
            globals: Vec::new(),
            funcs: vec![isa::Func {
                name: "main".to_string(),
                is_extern: false,
                ins: Vec::new(),
                outs: Vec::new(),
                in_offsets: Vec::new(),
                in_sizes: Vec::new(),
                out_offset: 0,
                out_sizes: Vec::new(),
                n_regs,
                slot_bytes: 0,
                arg_reg: None,
                ret_reg: None,
                code: words,
                call_sites,
                operands,
                lines: Vec::new(),
                place_names: Vec::new(),
            }],
        }
    }

    fn bit32_and_bit8() -> Vec<isa::TypeInfo> {
        vec![
            isa::TypeInfo {
                name: "#bit32".to_string(),
                size: 4,
                is_ref: false,
            },
            isa::TypeInfo {
                name: "#bit8".to_string(),
                size: 1,
                is_ref: false,
            },
        ]
    }

    /// The fused `arith.imm` reads a register or a slot left operand and masks
    /// the immediate to the operand width, matching `Arith` semantics.
    #[test]
    fn fused_arith_imm_reads_register_and_slot() {
        let code = vec![
            Instr::Const {
                dst: 0,
                imm: 10,
                ty: 0,
            },
            // &1 = 10 + 5 (register left operand).
            Instr::ArithImm {
                op: isa::ArithOp::IAdd,
                dst: 1,
                lhs: 0,
                lhs_slot: false,
                imm: 5,
                operand_ty: 0,
            },
            // &2 = (slot8 < 100) (slot left operand).
            Instr::ArithImm {
                op: isa::ArithOp::ILt,
                dst: 2,
                lhs: 8,
                lhs_slot: true,
                imm: 100,
                operand_ty: 0,
            },
            // &3 = slot8 + 1.
            Instr::ArithImm {
                op: isa::ArithOp::IAdd,
                dst: 3,
                lhs: 8,
                lhs_slot: true,
                imm: 1,
                operand_ty: 0,
            },
            Instr::Return,
        ];
        let mut program = arith_program(bit32_and_bit8(), code, 4);
        program.funcs[0].slot_bytes = 16;
        program.verify().expect("verify");
        let mut machine = Machine::new(&program).expect("entry");
        machine.write_abs(8, 5, 4).expect("seed slot");
        machine.step().expect("const");
        machine.step().expect("fused add reg");
        assert_eq!(machine.read_reg(1), Some(15), "register left operand");
        machine.step().expect("fused cmp slot");
        assert_eq!(machine.read_reg(2), Some(1), "slot left operand");
        machine.step().expect("fused add slot");
        assert_eq!(machine.read_reg(3), Some(6), "slot left operand add");
    }

    /// A comparison's LIR result is `#bit8`, but `operand_ty` drives the
    /// operand interpretation: `0xffff_ffff < 0x1` is true as `i32` (`-1 < 1`)
    /// and false if the full 64-bit register is read as `i64`.
    #[test]
    fn signed_32bit_comparison_uses_operand_width() {
        let code = vec![
            Instr::Const {
                dst: 0,
                imm: 0xffff_ffff,
                ty: 0,
            },
            Instr::Const {
                dst: 1,
                imm: 0x1,
                ty: 0,
            },
            Instr::Arith {
                op: isa::ArithOp::ILt,
                dst: 2,
                lhs: 0,
                rhs: 1,
                operand_ty: 0,
                ty: 1,
            },
            Instr::Return,
        ];
        let program = arith_program(bit32_and_bit8(), code, 3);
        program.verify().expect("verify");
        let mut machine = Machine::new(&program).expect("entry");
        machine.step().expect("const lhs");
        machine.step().expect("const rhs");
        machine.step().expect("cmp");
        assert_eq!(machine.read_reg(2), Some(1), "0xffff_ffff < 0x1 as i32");
    }

    /// `@f_less_than` on `#bit32` operands must reinterpret them as `f32`, not
    /// `f64`: `-1.0f32 < 1.0f32` is true, while the same 32-bit patterns read
    /// as `f64` are two subnormals where the first is larger.
    #[test]
    fn f32_comparison_uses_operand_width() {
        let code = vec![
            Instr::Const {
                dst: 0,
                imm: 0xbf80_0000,
                ty: 0,
            },
            Instr::Const {
                dst: 1,
                imm: 0x3f80_0000,
                ty: 0,
            },
            Instr::Arith {
                op: isa::ArithOp::FLt,
                dst: 2,
                lhs: 0,
                rhs: 1,
                operand_ty: 0,
                ty: 1,
            },
            Instr::Return,
        ];
        let program = arith_program(bit32_and_bit8(), code, 3);
        program.verify().expect("verify");
        let mut machine = Machine::new(&program).expect("entry");
        machine.step().expect("const lhs");
        machine.step().expect("const rhs");
        machine.step().expect("cmp");
        assert_eq!(machine.read_reg(2), Some(1), "-1.0f32 < 1.0f32");
    }

    /// Unsigned comparisons mask their operands to `operand_ty`'s width, so the
    /// result does not depend on producers having zero-extended the operands.
    #[test]
    fn unsigned_compare_masks_to_operand_width() {
        let types = vec![
            isa::TypeInfo {
                name: "#bit_native".to_string(),
                size: 8,
                is_ref: false,
            },
            isa::TypeInfo {
                name: "#bit8".to_string(),
                size: 1,
                is_ref: false,
            },
        ];
        // 0x100 vs 0x1 at `#bit8`: masked `0x00 < 0x01` is true; unmasked
        // `0x100 < 0x01` is false.
        let less_than = vec![
            Instr::Const {
                dst: 0,
                imm: 0x100,
                ty: 0,
            },
            Instr::Const {
                dst: 1,
                imm: 0x1,
                ty: 0,
            },
            Instr::Arith {
                op: isa::ArithOp::ILtU,
                dst: 2,
                lhs: 0,
                rhs: 1,
                operand_ty: 1,
                ty: 1,
            },
            Instr::Return,
        ];
        let program = arith_program(types.clone(), less_than, 3);
        program.verify().expect("verify");
        let mut machine = Machine::new(&program).expect("entry");
        machine.step().expect("const lhs");
        machine.step().expect("const rhs");
        machine.step().expect("cmp");
        assert_eq!(machine.read_reg(2), Some(1), "0x100 < 0x1 masked to u8");

        // 0x100 vs 0x0 at `#bit8`: masked `0x00 == 0x00` is true; unmasked is
        // false.
        let equal = vec![
            Instr::Const {
                dst: 0,
                imm: 0x100,
                ty: 0,
            },
            Instr::Const {
                dst: 1,
                imm: 0x0,
                ty: 0,
            },
            Instr::Arith {
                op: isa::ArithOp::Eq,
                dst: 2,
                lhs: 0,
                rhs: 1,
                operand_ty: 1,
                ty: 1,
            },
            Instr::Return,
        ];
        let program = arith_program(types, equal, 3);
        program.verify().expect("verify");
        let mut machine = Machine::new(&program).expect("entry");
        machine.step().expect("const lhs");
        machine.step().expect("const rhs");
        machine.step().expect("cmp");
        assert_eq!(machine.read_reg(2), Some(1), "0x100 == 0x0 masked to u8");
    }

    /// `Point { x, y }` stores both fields into an aggregate place; `p.x` reads
    /// the first field back. Exercises `Addr`/`StoreField`/`Field` plus the
    /// aggregate `Copy` that materializes the literal into `%p`.
    #[test]
    fn struct_field_round_trips() {
        let src = "Point: object {\n    x: @bit_native\n    y: @bit_native\n}\n\
                   main: function () nothing {\n    p: Point = Point {\n        x = 0x1\n        y = 0x2\n    }\n    a: @bit_native = p.x\n}\n";
        let a = run_and_capture_place(src, "a").expect("run");
        assert_eq!(a, Some(0x1), "first field round-trips");
    }

    /// `@ptr_add` on a frame ref with a large byte offset produces a ref beyond
    /// the arena; the following `load` must trap with `BadRef`, never panic.
    #[test]
    fn ptr_add_out_of_range_traps() {
        let src = "main: function () nothing {\n    p: @bit8 = 0x0\n    r: ref @bit8 = ref p\n    v: @bit8 = @load_u8(r, 0x100000)\n}\n";
        assert!(matches!(run_src(src), Err(VmError::BadRef(_))));
    }

    /// A `store` through an out-of-range ref traps with `BadRef`.
    #[test]
    fn store_u8_out_of_range_traps() {
        let src = "main: function () nothing {\n    p: @bit8 = 0x0\n    r: ref @bit8 = ref p\n    @store_u8(r, 0x100000, 0x1)\n}\n";
        assert!(matches!(run_src(src), Err(VmError::BadRef(_))));
    }

    /// The `format_args` runtime bodies run as ordinary bytecode: `new`, three
    /// `push`es through a `ref #format_args`, `count`, and a valid `piece`.
    /// (`$"...{part}..."` lowers to those calls; the count is the piece count.)
    const FORMAT_ARGS_SRC: &str = "main: function () nothing {\n    part: @byte_buffer = \"x\"\n    msg: @format_args = $\"a{part}b\"\n    n: @bit_native = @format_args_count(msg)\n    first: @byte_buffer = @format_args_piece(msg, 0x0)\n}\n";

    /// `@byte_buffer_from(ref #bit8, #bit_native)` runs the injected body (an
    /// `alloca`, two `store_field`s, and a `copy`) and `@byte_buffer_len` reads
    /// the stored length back. The pointer is not foldable, so the call stays.
    #[test]
    fn byte_buffer_from_and_len() {
        let src = "main: function () nothing {\n    s: @byte_buffer = \"hi\"\n    p: ref @bit8 = @byte_buffer_ptr(s)\n    b: @byte_buffer = @byte_buffer_from(p, 0x2)\n    n: @bit_native = @byte_buffer_len(b)\n}\n";
        let n = run_and_capture_place(src, "n").expect("run");
        assert_eq!(n, Some(0x2), "buffer length round-trips");
        assert_eq!(run_src(src).expect("run"), 0);
    }

    /// A `format_args` table built by the runtime round-trips its piece count.
    #[test]
    fn format_args_round_trip() {
        let n = run_and_capture_place(FORMAT_ARGS_SRC, "n").expect("run");
        assert_eq!(n, Some(0x3), "three pieces: \"a\", part, \"b\"");
        assert_eq!(run_src(FORMAT_ARGS_SRC).expect("run"), 0);
    }

    /// An out-of-range `format_args_piece` routes through the injected
    /// `@lilac_abort` trap: exit code 101, never a VM internal error.
    #[test]
    fn format_args_piece_out_of_range_aborts() {
        let src = "main: function () nothing {\n    args: @format_args = @format_args_new()\n    x: @byte_buffer = @format_args_piece(args, 0x0)\n}\n";
        assert_eq!(run_src(src).expect("run"), 101);
    }

    /// Pins the two hardcoded agreements in `lilac-ir/src/runtime.rs`: a
    /// `#byte_buffer` is 16 bytes and the `format_args` piece stride is 16.
    #[test]
    fn byte_buffer_size_and_format_args_stride_are_sixteen() {
        let program = compile_src(FORMAT_ARGS_SRC);
        let byte_buffer = program
            .types
            .iter()
            .find(|info| info.name == "#byte_buffer")
            .expect("#byte_buffer type");
        assert_eq!(byte_buffer.size, 16, "#byte_buffer size pins runtime.rs");
        for name in [".lilac.format_args_push", ".lilac.format_args_piece"] {
            let func = program
                .funcs
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("runtime function {name}"));
            let mut pc = 0_usize;
            let mut has_stride = false;
            while pc < func.code.len() {
                let (instr, next) = decode(&func.code, pc, &func.call_sites, &func.operands)
                    .unwrap_or_else(|err| panic!("decode {name} at {pc}: {err}"));
                if matches!(
                    instr,
                    Decoded::Const { imm: 0x10, .. } | Decoded::ArithImm { imm: 0x10, .. }
                ) {
                    has_stride = true;
                }
                pc = next as usize;
            }
            assert!(has_stride, "{name} must embed the 16-byte piece stride");
        }
    }

    /// A data buffer's pointer is read with `Field`, `@load_u8` addresses it
    /// with `@ptr_add`, and the resulting byte is loaded back.
    #[test]
    fn byte_buffer_load_uses_addr_field_and_ptr_add() {
        let src = "main: function () nothing {\n    s: @byte_buffer = \"A\"\n    p: ref @bit8 = @byte_buffer_ptr(s)\n    c: @bit8 = @load_u8(p, 0x0)\n}\n";
        let c = run_and_capture_place(src, "c").expect("run");
        assert_eq!(c, Some(0x41), "first buffer byte is `A`");
    }

    /// `@memcpy` copies raw bytes through two refs; the copied byte is read
    /// back through the destination ref.
    #[test]
    fn memcpy_builtin_copies_bytes() {
        let src = "main: function () nothing {\n    cell: @bit8 = 0x0\n    dp: ref @bit8 = ref cell\n    s: @byte_buffer = \"A\"\n    sp: ref @bit8 = @byte_buffer_ptr(s)\n    @memcpy(dp, sp, 0x1)\n    c: @bit8 = @load_u8(dp, 0x0)\n}\n";
        let c = run_and_capture_place(src, "c").expect("run");
        assert_eq!(c, Some(0x41), "copied byte is `A`");
    }

    /// `AddrFunc` yields a decodable function address that `CallIndirect`
    /// dispatches.
    #[test]
    fn addr_func_and_call_indirect_round_trip() {
        let mut program = isa::sample_program(vec![
            Instr::AddrFunc { dst: 0, func: 1 },
            Instr::CallIndirect {
                callee: 0,
                ins: Vec::new(),
                outs: Vec::new(),
            },
            Instr::Return,
        ]);
        let (hcode, hcall, hops) = isa::words_of(&program.types, &[Instr::Return]);
        program.funcs.push(isa::Func {
            name: "helper".to_string(),
            is_extern: false,
            ins: Vec::new(),
            outs: Vec::new(),
            in_offsets: Vec::new(),
            in_sizes: Vec::new(),
            out_offset: 0,
            out_sizes: Vec::new(),
            n_regs: 0,
            slot_bytes: 0,
            arg_reg: None,
            ret_reg: None,
            code: hcode,
            call_sites: hcall,
            operands: hops,
            lines: Vec::new(),
            place_names: Vec::new(),
        });
        program.verify().expect("verify");
        assert_eq!(run(&program).expect("run"), 0);
    }

    /// A `CallIndirect` through a value with no function tag is not a valid
    /// function address and traps with `BadRef`.
    #[test]
    fn call_indirect_non_function_traps() {
        let program = isa::sample_program(vec![
            Instr::Const {
                dst: 0,
                imm: 0x1,
                ty: 0,
            },
            Instr::CallIndirect {
                callee: 0,
                ins: Vec::new(),
                outs: Vec::new(),
            },
            Instr::Return,
        ]);
        program.verify().expect("verify");
        let mut machine = Machine::new(&program).expect("entry");
        machine.step().expect("const");
        assert!(matches!(machine.step(), Err(VmError::BadRef(_))));
    }

    /// An `indirect` `Field` read through an out-of-range ref traps with
    /// `BadRef`, never a panic.
    #[test]
    fn field_out_of_range_traps() {
        let program = arith_program(
            vec![isa::TypeInfo {
                name: "#bit8".to_string(),
                size: 1,
                is_ref: false,
            }],
            vec![
                Instr::Field {
                    dst: 0,
                    place: 0,
                    off: 0,
                    ty: 0,
                    indirect: true,
                },
                Instr::Return,
            ],
            1,
        );
        // `arith_program` has no slots; give it an 8-byte ref cell.
        let mut program = program;
        program.funcs[0].slot_bytes = 8;
        program.verify().expect("verify");
        let mut machine = Machine::new(&program).expect("entry");
        // Seed the object slot with a huge ref before the field read.
        machine
            .write_abs(0, u64::MAX - 1, isa::VM_POINTER_BYTES)
            .expect("seed ref");
        assert!(matches!(machine.step(), Err(VmError::BadRef(_))));
    }
}
