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
//! valid ref. Each frame's place region is appended (zeroed) on call and never
//! reclaimed; the call-depth cap bounds total growth. Out-of-range accesses are
//! reported as [`VmError::BadRef`], never a panic.
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
    arith_from_opcode, builtin_from_index, convert_from_opcode, OP_ADDR, OP_ADDRFIELD_BASE,
    OP_ADDRFIELD_LAST, OP_ADDR_FUNC, OP_BRANCH, OP_BRANCH_IF, OP_CALL, OP_CALL_BUILTIN_BASE,
    OP_CALL_BUILTIN_LAST, OP_CALL_INDIRECT, OP_CONST, OP_CONST_DATA, OP_COPY_BASE, OP_COPY_BYTES,
    OP_COPY_LAST, OP_EXTEND_ZERO, OP_FIELD_BASE, OP_FIELD_LAST, OP_LOAD, OP_PANIC, OP_RETURN,
    OP_STOREFIELD_BASE, OP_STOREFIELD_LAST, OP_STORE_BASE, OP_STORE_LAST, OP_TRUNC_BASE,
    OP_UNREACHABLE, WIDE_FLAG,
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
}

/// A bytecode interpreter over one [`Program`].
pub struct Machine<'a> {
    program: &'a Program,
    /// All frames' registers, grown on call.
    regs: Vec<u64>,
    /// Globals followed by each live frame's zeroed slot region.
    arena: Vec<u8>,
    /// The call stack; the last frame is the running one.
    frames: Vec<Frame>,
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
        let frames = vec![Frame {
            func: func_index,
            reg_base: 0,
            slot_base,
            pc: 0,
            last_pc: None,
        }];
        Ok(Self {
            program,
            regs,
            arena,
            frames,
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
        let program = self.program;
        let Some(frame) = self.frames.last() else {
            return Ok(0);
        };
        let mut func_index = frame.func;
        let mut pc = frame.pc;
        let mut reg_base = frame.reg_base;
        let mut slot_base = frame.slot_base;

        loop {
            let func = &program.funcs[func_index as usize];
            let code = &func.code;
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
            match base {
                OP_RETURN => {
                    if wide {
                        return Err(VmError::BadInstr(pc));
                    }
                    self.store_frame(func_index, pc, pc + 1);
                    match self.return_from_frame() {
                        Step::Returned => return Ok(0),
                        Step::Advanced | Step::Panicked(_) => {
                            let Some(frame) = self.frames.last() else {
                                return Ok(0);
                            };
                            func_index = frame.func;
                            pc = frame.pc;
                            reg_base = frame.reg_base;
                            slot_base = frame.slot_base;
                            continue;
                        }
                    }
                }
                OP_UNREACHABLE => {
                    if wide {
                        return Err(VmError::BadInstr(pc));
                    }
                    self.store_frame(func_index, pc, pc + 1);
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
                    self.store_frame(func_index, pc, pc + 1);
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
                    next = if self.reg(reg_base, ops[0]) == 0 {
                        ops[2]
                    } else {
                        ops[1]
                    };
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
                    self.set_reg(reg_base, dst, imm);
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
                    self.set_reg(reg_base, dst, u64::from(off));
                    next = pc + n + 1;
                }
                b @ (OP_COPY_BASE..=OP_COPY_LAST) => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let k = b - OP_COPY_BASE;
                    let dst = operate(ops[0], k & 2 != 0);
                    let src = operate(ops[1], k & 1 != 0);
                    let size = ops[2];
                    if size <= VM_POINTER_BYTES {
                        let value = self.read_scalar(reg_base, slot_base, src, size)?;
                        self.write_scalar(reg_base, slot_base, dst, value, size)?;
                    } else {
                        self.copy_aggregate(reg_base, slot_base, dst, src, size)?;
                    }
                    next = pc + n;
                }
                OP_COPY_BYTES => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let src_abs = slot_base as usize + ops[1] as usize;
                    let dst_abs = slot_base as usize + ops[0] as usize;
                    self.copy_arena(src_abs, dst_abs, ops[2] as usize)?;
                    next = pc + n;
                }
                OP_LOAD => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let ref_off = self.slot_ref(slot_base, ops[1])?;
                    if ops[2] <= VM_POINTER_BYTES {
                        let abs = usize::try_from(ref_off).map_err(|_| VmError::BadRef(ref_off))?;
                        let value = self.read_abs(abs, ops[2]).ok_or(VmError::BadRef(ref_off))?;
                        self.set_reg(reg_base, ops[0], value);
                    } else {
                        // An aggregate `Load` passes the pointer through.
                        self.set_reg(reg_base, ops[0], ref_off);
                    }
                    next = pc + n;
                }
                b @ (OP_STORE_BASE..=OP_STORE_LAST) => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let src = operate(ops[0], b == OP_STORE_LAST);
                    let ref_off = self.slot_ref(slot_base, ops[1])?;
                    let abs = usize::try_from(ref_off).map_err(|_| VmError::BadRef(ref_off))?;
                    if ops[2] <= VM_POINTER_BYTES {
                        let value = self.read_scalar(reg_base, slot_base, src, ops[2])?;
                        self.write_abs(abs, value, ops[2])?;
                    } else {
                        self.copy_operand_bytes(reg_base, slot_base, src, abs, ops[2])?;
                    }
                    next = pc + n;
                }
                OP_ADDR => {
                    let (ops, n) = read_ops2(code, pc, wide)?;
                    self.set_reg(reg_base, ops[0], u64::from(slot_base) + u64::from(ops[1]));
                    next = pc + n;
                }
                b @ (OP_ADDRFIELD_BASE..=OP_ADDRFIELD_LAST) => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let base_off = self.field_base(slot_base, ops[1], b == OP_ADDRFIELD_LAST)?;
                    let addr = base_off
                        .checked_add(ops[2] as usize)
                        .ok_or(VmError::BadRef(base_off as u64))?;
                    let addr = u64::try_from(addr).map_err(|_| VmError::BadRef(base_off as u64))?;
                    self.set_reg(reg_base, ops[0], addr);
                    next = pc + n;
                }
                OP_ADDR_FUNC => {
                    let (ops, n) = read_ops2(code, pc, wide)?;
                    self.set_reg(reg_base, ops[0], FUNC_TAG | u64::from(ops[1]));
                    next = pc + n;
                }
                b @ (OP_FIELD_BASE..=OP_FIELD_LAST) => {
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    let size = *code
                        .get(pc as usize + n as usize)
                        .ok_or(VmError::BadInstr(pc))?;
                    let base_off = self.field_base(slot_base, ops[1], b == OP_FIELD_LAST)?;
                    let addr = base_off
                        .checked_add(ops[2] as usize)
                        .ok_or(VmError::BadRef(base_off as u64))?;
                    if size <= VM_POINTER_BYTES {
                        let value = self
                            .read_abs(addr, size)
                            .ok_or(VmError::BadRef(addr as u64))?;
                        self.set_reg(reg_base, ops[0], value);
                    } else {
                        // The compiler emits `AddrField` for aggregate fields;
                        // this defensive fall-back yields the field ref.
                        let value =
                            u64::try_from(addr).map_err(|_| VmError::BadRef(addr as u64))?;
                        self.set_reg(reg_base, ops[0], value);
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
                    let base_off = self.field_base(slot_base, ops[1], k & 2 != 0)?;
                    let addr = base_off
                        .checked_add(ops[2] as usize)
                        .ok_or(VmError::BadRef(base_off as u64))?;
                    if size <= VM_POINTER_BYTES {
                        let value = self.read_scalar(reg_base, slot_base, src, size)?;
                        self.write_abs(addr, value, size)?;
                    } else {
                        self.copy_operand_bytes(reg_base, slot_base, src, addr, size)?;
                    }
                    next = pc + n + 1;
                }
                OP_CALL => {
                    if wide {
                        return Err(VmError::BadInstr(pc));
                    }
                    let (site, ins, outs) =
                        resolve_call(&func.call_sites, &func.operands, (w >> 8) as usize)
                            .ok_or(VmError::BadInstr(pc))?;
                    self.store_frame(func_index, pc, pc + 1);
                    self.call(site.func, ins, outs, reg_base, slot_base)?;
                    let Some(frame) = self.frames.last() else {
                        return Ok(0);
                    };
                    func_index = frame.func;
                    pc = frame.pc;
                    reg_base = frame.reg_base;
                    slot_base = frame.slot_base;
                    continue;
                }
                OP_CALL_INDIRECT => {
                    let (callee, n) = read_ops1(code, pc, wide)?;
                    let index = *code
                        .get(pc as usize + n as usize)
                        .ok_or(VmError::BadInstr(pc))? as usize;
                    let (_site, ins, outs) = resolve_call(&func.call_sites, &func.operands, index)
                        .ok_or(VmError::BadInstr(pc))?;
                    let value = self.reg(reg_base, callee);
                    if value & FUNC_TAG == 0 {
                        return Err(VmError::BadRef(value));
                    }
                    let target =
                        u32::try_from(value & !FUNC_TAG).map_err(|_| VmError::BadRef(value))?;
                    self.store_frame(func_index, pc, pc + n + 1);
                    self.call(target, ins, outs, reg_base, slot_base)?;
                    let Some(frame) = self.frames.last() else {
                        return Ok(0);
                    };
                    func_index = frame.func;
                    pc = frame.pc;
                    reg_base = frame.reg_base;
                    slot_base = frame.slot_base;
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
                        self.call_builtin(builtin, ins, outs, reg_base, slot_base)?
                    {
                        self.store_frame(func_index, pc, pc + 1);
                        return Ok(code);
                    }
                    next = pc + 1;
                }
                b @ (OP_TRUNC_BASE..=0x5F) => {
                    let (ops, n) = read_ops2(code, pc, wide)?;
                    let Some((_, width)) = convert_from_opcode(b) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let value = self.reg(reg_base, ops[1]);
                    self.set_reg(reg_base, ops[0], value & mask(u32::from(width) / 8));
                    next = pc + n;
                }
                OP_EXTEND_ZERO => {
                    let (ops, n) = read_ops2(code, pc, wide)?;
                    self.set_reg(reg_base, ops[0], self.reg(reg_base, ops[1]));
                    next = pc + n;
                }
                b => {
                    let Some((op, width)) = arith_from_opcode(b) else {
                        return Err(VmError::BadInstr(pc));
                    };
                    let (ops, n) = read_ops3(code, pc, wide)?;
                    self.exec_arith(op, reg_base, ops[0], ops[1], ops[2], width);
                    next = pc + n;
                }
            }
            pc = next;
        }
    }

    /// Synchronizes the running frame's `pc`/`last_pc` with the loop's locals.
    ///
    /// The inner loop keeps these in locals and only calls this on
    /// call/return/trap (spec §6.1). It is keyed on `func_index` so that a
    /// frame pushed or popped in between is never written with another frame's
    /// counters.
    fn store_frame(&mut self, func_index: u32, pc: u32, next: u32) {
        if let Some(frame) = self.frames.last_mut() {
            if frame.func == func_index {
                frame.pc = next;
                frame.last_pc = Some(pc);
            }
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
            Decoded::Call { site, ins, outs } => {
                self.call(site.func, ins, outs, reg_base, slot_base)?;
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
                self.call(target, ins, outs, reg_base, slot_base)?;
            }
            Decoded::CallBuiltin {
                builtin, ins, outs, ..
            } => {
                return self.call_builtin(builtin, ins, outs, reg_base, slot_base);
            }
            Decoded::Return => return Ok(self.return_from_frame()),
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
            Decoded::Unreachable => return Err(VmError::Unreachable),
        }
        Ok(Step::Advanced)
    }

    /// Evaluates a width-specialized arithmetic/comparison instruction.
    ///
    /// The result is masked to the operand width. For the comparison ops this
    /// is harmless (the result is `0`/`1`); for arithmetic the result type
    /// equals the operand type, matching M5c.
    fn exec_arith(&mut self, op: ArithOp, reg_base: u32, dst: u32, lhs: u32, rhs: u32, width: u8) {
        let size = u32::from(width) / 8;
        let left = self.reg(reg_base, lhs);
        let right = self.reg(reg_base, rhs);
        let value = eval_arith(op, left, right, size);
        self.set_reg(reg_base, dst, value & mask(size));
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
    ) -> Result<(), VmError> {
        let program = self.program;
        let callee = program
            .funcs
            .get(target as usize)
            .ok_or(VmError::BadRef(u64::from(target)))?;
        if callee.is_extern {
            return Err(VmError::ExternCall(callee.name.clone()));
        }
        if self.frames.len() >= MAX_CALL_DEPTH as usize {
            return Err(VmError::StackOverflow);
        }
        let reg_base = u32::try_from(self.regs.len()).unwrap_or(u32::MAX);
        let slot_base = u32::try_from(self.arena.len()).unwrap_or(u32::MAX);
        self.regs.resize(
            self.regs.len() + usize::try_from(callee.n_regs).unwrap_or(0),
            0,
        );
        self.arena.resize(
            self.arena.len() + usize::try_from(callee.slot_bytes).unwrap_or(0),
            0,
        );
        for (index, operand) in ins.iter().enumerate() {
            let (Some(&dst_off), Some(&size)) =
                (callee.in_offsets.get(index), callee.in_sizes.get(index))
            else {
                break;
            };
            let dst_abs = usize::try_from(slot_base).unwrap_or(0) + dst_off as usize;
            self.move_to_slot(caller_reg_base, caller_slot_base, *operand, size, dst_abs)?;
        }
        // Fill the out-ref region: one u64 absolute arena offset per output.
        for (index, operand) in outs.iter().enumerate() {
            let abs = match operand {
                Operand::Slot(off) => u64::from(caller_slot_base) + u64::from(*off),
                Operand::Reg(reg) => self.reg(caller_reg_base, *reg),
            };
            let cell = u32::try_from(index)
                .ok()
                .and_then(|index| index.checked_mul(VM_POINTER_BYTES))
                .and_then(|off| slot_base.checked_add(off))
                .ok_or(VmError::BadRef(u64::from(slot_base)))?;
            self.write_abs(usize::try_from(cell).unwrap_or(0), abs, VM_POINTER_BYTES)?;
        }
        self.frames.push(Frame {
            func: target,
            reg_base,
            slot_base,
            pc: 0,
            last_pc: None,
        });
        Ok(())
    }

    /// Pops the running frame.
    ///
    /// Outputs were written by reference, so there is nothing to copy back;
    /// an output the callee never wrote leaves the caller's storage unchanged.
    fn return_from_frame(&mut self) -> Step {
        if self.frames.len() <= 1 {
            self.frames.pop();
            return Step::Returned;
        }
        self.frames.pop();
        Step::Advanced
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
        match (dst, src) {
            (Operand::Slot(dst_off), Operand::Reg(src_reg)) => {
                let ref_off = self.reg(reg_base, src_reg);
                let src_abs = usize::try_from(ref_off).map_err(|_| VmError::BadRef(ref_off))?;
                let dst_abs = usize::try_from(slot_base).unwrap_or(0) + dst_off as usize;
                self.copy_arena(src_abs, dst_abs, size as usize)
            }
            (Operand::Reg(dst_reg), Operand::Slot(src_off)) => {
                let addr = u64::from(slot_base) + u64::from(src_off);
                self.set_reg(reg_base, dst_reg, addr);
                Ok(())
            }
            (Operand::Slot(dst_off), Operand::Slot(src_off)) => {
                let dst_abs = usize::try_from(slot_base).unwrap_or(0) + dst_off as usize;
                let src_abs = usize::try_from(slot_base).unwrap_or(0) + src_off as usize;
                self.copy_arena(src_abs, dst_abs, size as usize)
            }
            (Operand::Reg(dst_reg), Operand::Reg(src_reg)) => {
                let value = self.reg(reg_base, src_reg);
                self.set_reg(reg_base, dst_reg, value);
                Ok(())
            }
        }
    }

    /// Reads the pointer-sized ref stored in a frame slot.
    fn slot_ref(&self, slot_base: u32, place: u32) -> Result<u64, VmError> {
        let abs = usize::try_from(slot_base).unwrap_or(0) + place as usize;
        self.read_abs(abs, VM_POINTER_BYTES)
            .ok_or(VmError::BadRef(abs as u64))
    }

    /// Resolves a field instruction's object base to an absolute arena offset.
    ///
    /// With `indirect`, the object slot holds a ref that is dereferenced first;
    /// otherwise the frame slot itself is the aggregate base.
    fn field_base(&self, slot_base: u32, place: u32, indirect: bool) -> Result<usize, VmError> {
        let slot_abs = usize::try_from(slot_base).unwrap_or(0) + place as usize;
        if indirect {
            let ref_off = self
                .read_abs(slot_abs, VM_POINTER_BYTES)
                .ok_or(VmError::BadRef(slot_abs as u64))?;
            usize::try_from(ref_off).map_err(|_| VmError::BadRef(ref_off))
        } else {
            Ok(slot_abs)
        }
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
        let src_abs = match operand {
            Operand::Reg(reg) => {
                let ref_off = self.reg(reg_base, reg);
                usize::try_from(ref_off).map_err(|_| VmError::BadRef(ref_off))?
            }
            Operand::Slot(off) => usize::try_from(slot_base).unwrap_or(0) + off as usize,
        };
        self.copy_arena(src_abs, dst_abs, size as usize)
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
        match builtin {
            Builtin::PtrAdd => {
                let (base, offset) = match ins {
                    [base, offset] => (*base, *offset),
                    _ => {
                        lilac_error::bug!(lilac_span::Span::dummy(), "@ptr_add expects two inputs")
                    }
                };
                let base = self.read_scalar(reg_base, slot_base, base, VM_POINTER_BYTES)?;
                let offset = self.read_scalar(reg_base, slot_base, offset, VM_POINTER_BYTES)?;
                let Some(Operand::Slot(out)) = outs.first().copied() else {
                    lilac_error::bug!(lilac_span::Span::dummy(), "@ptr_add expects one output");
                };
                self.write_scalar(
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
                    _ => {
                        lilac_error::bug!(lilac_span::Span::dummy(), "@memcpy expects three inputs")
                    }
                };
                let dst = self.read_scalar(reg_base, slot_base, dst, VM_POINTER_BYTES)?;
                let src = self.read_scalar(reg_base, slot_base, src, VM_POINTER_BYTES)?;
                let len = self.read_scalar(reg_base, slot_base, len, VM_POINTER_BYTES)?;
                let dst_abs = usize::try_from(dst).map_err(|_| VmError::BadRef(dst))?;
                let src_abs = usize::try_from(src).map_err(|_| VmError::BadRef(src))?;
                let len = usize::try_from(len).map_err(|_| VmError::BadRef(len))?;
                self.copy_arena(src_abs, dst_abs, len)?;
                Ok(Step::Advanced)
            }
            Builtin::Abort => Ok(trap(b"aborted")),
        }
    }

    /// Copies a scalar or aggregate operand into an absolute slot offset.
    fn move_to_slot(
        &mut self,
        reg_base: u32,
        slot_base: u32,
        operand: Operand,
        size: u32,
        dst_abs: usize,
    ) -> Result<(), VmError> {
        if size <= VM_POINTER_BYTES {
            let value = self.read_scalar(reg_base, slot_base, operand, size)?;
            self.write_abs(dst_abs, value, size)
        } else {
            match operand {
                Operand::Reg(reg) => {
                    let ref_off = self.reg(reg_base, reg);
                    let src = usize::try_from(ref_off).map_err(|_| VmError::BadRef(ref_off))?;
                    self.copy_arena(src, dst_abs, size as usize)
                }
                Operand::Slot(off) => {
                    let src_abs = usize::try_from(slot_base).unwrap_or(0) + off as usize;
                    self.copy_arena(src_abs, dst_abs, size as usize)
                }
            }
        }
    }

    /// Reads an operand as its low `size` bytes.
    fn read_scalar(
        &self,
        reg_base: u32,
        slot_base: u32,
        operand: Operand,
        size: u32,
    ) -> Result<u64, VmError> {
        match operand {
            Operand::Reg(reg) => Ok(self.reg(reg_base, reg)),
            Operand::Slot(off) => {
                let abs = usize::try_from(slot_base).unwrap_or(0) + off as usize;
                self.read_abs(abs, size).ok_or(VmError::BadRef(abs as u64))
            }
        }
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
        match dst {
            Operand::Reg(reg) => {
                self.set_reg(reg_base, reg, value & mask(size));
                Ok(())
            }
            Operand::Slot(off) => {
                let abs = usize::try_from(slot_base).unwrap_or(0) + off as usize;
                self.write_abs(abs, value, size)
            }
        }
    }

    /// Reads `size` little-endian bytes at an absolute arena offset.
    fn read_abs(&self, abs: usize, size: u32) -> Option<u64> {
        let end = abs.checked_add(size as usize)?;
        self.arena.get(abs..end).map(le_u64)
    }

    /// Writes `size` little-endian bytes at an absolute arena offset.
    fn write_abs(&mut self, abs: usize, value: u64, size: u32) -> Result<(), VmError> {
        let end = abs
            .checked_add(size as usize)
            .ok_or(VmError::BadRef(abs as u64))?;
        let bytes = self
            .arena
            .get_mut(abs..end)
            .ok_or(VmError::BadRef(abs as u64))?;
        store_le(bytes, value);
        Ok(())
    }

    /// Copies `size` bytes within the arena (refs are byte offsets).
    fn copy_arena(&mut self, src: usize, dst: usize, size: usize) -> Result<(), VmError> {
        let src_end = src.checked_add(size).ok_or(VmError::BadRef(src as u64))?;
        let dst_end = dst.checked_add(size).ok_or(VmError::BadRef(dst as u64))?;
        if src_end > self.arena.len() {
            return Err(VmError::BadRef(src as u64));
        }
        if dst_end > self.arena.len() {
            return Err(VmError::BadRef(dst as u64));
        }
        self.arena.copy_within(src..src_end, dst);
        Ok(())
    }

    /// Reads a register of the given frame (zero when out of range).
    fn reg(&self, reg_base: u32, reg: u32) -> u64 {
        let index = usize::try_from(reg_base).unwrap_or(0) + reg as usize;
        self.regs.get(index).copied().unwrap_or(0)
    }

    /// Writes a register of the given frame (ignored when out of range).
    fn set_reg(&mut self, reg_base: u32, reg: u32, value: u64) {
        let index = usize::try_from(reg_base).unwrap_or(0) + reg as usize;
        if let Some(slot) = self.regs.get_mut(index) {
            *slot = value;
        }
    }
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
        let bytes = code[pc as usize].to_le_bytes();
        Ok((u32::from(bytes[1]), 1))
    }
}

/// Reads a two-operand instruction's operands and its word count.
#[inline]
fn read_ops2(code: &[u32], pc: u32, wide: bool) -> Result<([u32; 2], u32), VmError> {
    if wide {
        let ext0 = *code.get(pc as usize + 1).ok_or(VmError::BadInstr(pc))?;
        Ok(([ext0 & 0xFFFF, (ext0 >> 16) & 0xFFFF], 2))
    } else {
        let bytes = code[pc as usize].to_le_bytes();
        Ok(([u32::from(bytes[1]), u32::from(bytes[2])], 1))
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
        let bytes = code[pc as usize].to_le_bytes();
        Ok((
            [
                u32::from(bytes[1]),
                u32::from(bytes[2]),
                u32::from(bytes[3]),
            ],
            1,
        ))
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
    let mut value = 0_u64;
    for (index, &byte) in bytes.iter().take(8).enumerate() {
        value |= u64::from(byte) << (index * 8);
    }
    value
}

/// Writes `value`'s low bytes little-endian.
#[allow(clippy::cast_possible_truncation)]
fn store_le(bytes: &mut [u8], value: u64) {
    for (index, slot) in bytes.iter_mut().enumerate().take(8) {
        *slot = (value >> (index * 8)) as u8;
    }
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

/// Evaluates an integer arithmetic/comparison operator.
///
/// `size` is the operand width (from `Arith.operand_ty`): signed
/// division/remainder and signed comparisons sign-extend their operands at that
/// width, matching the LLVM backend's operand-typed `sdiv`/`srem`/`icmp slt`.
/// Division by zero yields zero.
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
    /// precomputed [`crate::isa::Func`] layouts, never a `type_size` lookup: a
    /// compiled `fib` carries populated `in_offsets`/`in_sizes`/`out_offset`/
    /// `out_sizes`, and driving it copies the right value through each call.
    #[test]
    fn hot_path_has_no_type_lookups() {
        let src = "fib: function (n: @bit_native) r: @bit_native {\n    if @i_less_than(n, 0x2) {\n        r = n\n    } else {\n        r = @i_add(fib(@i_sub(n, 0x1)), fib(@i_sub(n, 0x2)))\n    }\n}\n\nmain: function () nothing {\n    out: @bit_native = fib(0x12)\n}\n";
        let program = compile_src(src);
        let fib = program
            .funcs
            .iter()
            .find(|func| func.name == "fib")
            .expect("fib function");
        assert_eq!(
            fib.in_offsets,
            vec![8],
            "inputs follow the 8-byte out-ref region"
        );
        assert_eq!(fib.in_sizes, vec![8], "one pointer-wide input");
        assert_eq!(fib.out_offset, 16, "inputs end after the out-ref region");
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
                if matches!(instr, Decoded::Const { imm: 0x10, .. }) {
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
