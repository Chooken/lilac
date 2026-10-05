//! Bench-only tree-walking baseline interpreter.
//!
//! This module is a deliberately **naive, correctness-first** interpreter over
//! the same bytecode [`Program`] the optimized [`crate::Machine`] runs. It
//! exists only to give the benchmark a stable reference point: the VM must be
//! at least 3× faster than this baseline on every suite entry. It is never
//! compiled into a shipped build — the module is gated behind the
//! `bench-baseline` feature.
//!
//! Where [`crate::Machine`] keeps registers in one dense `Vec`, views memory as
//! a single contiguous byte arena, and dispatches over a flat instruction
//! array, this baseline mirrors `lilac-const`'s tree-walking evaluator:
//!
//! * every frame's registers live in a fresh `HashMap`;
//! * a `ref` is a byte address into a sparse `HashMap<u64, u8>` memory model
//!   (no flat arena, so aggregate copies are per-byte map operations);
//! * calls resolve by name, recurse instead of using a flat call stack, and
//!   marshal arguments/results through owned `Vec<u8>` buffers;
//! * the code is walked a block at a time, re-deriving block bounds on entry.
//!
//! The semantics match [`crate::Machine`] exactly (including the indirect
//! object-base flag, little-endian access, and the `Arith.operand_ty` width
//! rule) so the timed programs compute the same result in both engines.

// This is benchmark-only scaffolding: addresses and instruction operands are
// small by construction, so the explicit integer casts below are exact.
#![allow(
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use std::collections::HashMap;

use crate::isa::{ArithOp, Builtin, Func, Instr, Operand, Program, TypeId, VM_POINTER_BYTES};

/// High-bit tag distinguishing a function address from an arena ref.
///
/// Mirrors the VM's private `FUNC_TAG`; the baseline decodes the same
/// [`Instr::AddrFunc`] values.
const FUNC_TAG: u64 = 1 << 63;

/// Exit code reported when a program traps through `@lilac_abort` or `Panic`.
const ABORT_CODE: i32 = 101;

/// Runs `program` from its `main` entry with the tree-walking baseline.
///
/// # Errors
///
/// Returns a human-readable message when the program has no `main` or calls an
/// `extern` function.
pub fn treewalk_run(program: &Program) -> Result<i32, String> {
    let mut interp = Interp::new(program);
    let main = interp.entry("main")?;
    match interp.execute_function(main, &[])? {
        Outcome::Returned(_) => Ok(0),
        Outcome::Panicked(code) => Ok(code),
    }
}

/// The result of executing one function body.
enum Outcome {
    /// The body returned, with one byte buffer per output place.
    Returned(Vec<Vec<u8>>),
    /// The body trapped with the given exit code.
    Panicked(i32),
}

/// How a walked block terminated.
enum Flow {
    /// Control transfers to the given program counter.
    Goto(u32),
    /// The enclosing function returned.
    Return,
    /// The program trapped with the given exit code.
    Panicked(i32),
}

/// The naive interpreter state.
struct Interp<'a> {
    /// The program being interpreted.
    program: &'a Program,
    /// Functions by name (recursive resolution is a map lookup on every call).
    funcs: HashMap<&'a str, u32>,
    /// Sparse little-endian memory: address to byte, absent bytes read as zero.
    memory: HashMap<u64, u8>,
    /// Next never-used memory address (frames bump this, never reclaim).
    bump: u64,
}

impl<'a> Interp<'a> {
    /// Builds an interpreter over `program`.
    fn new(program: &'a Program) -> Self {
        let funcs = program
            .funcs
            .iter()
            .enumerate()
            .filter_map(|(index, func)| {
                u32::try_from(index)
                    .ok()
                    .map(|index| (func.name.as_str(), index))
            })
            .collect();
        let mut memory = HashMap::new();
        for (offset, &byte) in program.globals.iter().enumerate() {
            memory.insert(offset as u64, byte);
        }
        let bump = program.globals.len() as u64;
        Self {
            program,
            funcs,
            memory,
            bump,
        }
    }

    /// The index of a named entry function.
    fn entry(&self, name: &str) -> Result<u32, String> {
        self.funcs
            .get(name)
            .copied()
            .ok_or_else(|| format!("no entry point `{name}`"))
    }

    /// Reserves `size` fresh, zero-initialised bytes and returns their base.
    fn alloc(&mut self, size: u64) -> u64 {
        let base = self.bump;
        self.bump += size;
        base
    }

    /// Executes one function with `args` bound into its input slots.
    fn execute_function(&mut self, func_index: u32, args: &[Vec<u8>]) -> Result<Outcome, String> {
        let program = self.program;
        let func = program
            .funcs
            .get(func_index as usize)
            .ok_or_else(|| format!("function id {func_index} out of range"))?;
        if func.is_extern {
            return Err(format!("cannot call extern function `{}`", func.name));
        }
        let slot_base = self.alloc(u64::from(func.slot_bytes));
        let mut offset = 0_u64;
        for (arg, ty) in args.iter().zip(&func.ins) {
            let size = u64::from(type_size(program, *ty));
            self.write_bytes(slot_base + offset, arg);
            offset += size;
        }
        let out_offset = offset;

        let mut regs: HashMap<u32, u64> = HashMap::new();
        let labels: Vec<u32> = func.labels().into_iter().collect();
        let code_len = u32::try_from(func.code.len()).map_err(|_| "function too large")?;
        let mut pc = 0_u32;
        let outcome = loop {
            match self.run_block(func, pc, &labels, code_len, &mut regs, slot_base)? {
                Flow::Goto(next) => pc = next,
                Flow::Return => {
                    let mut outs = Vec::with_capacity(func.outs.len());
                    let mut out = out_offset;
                    for &ty in &func.outs {
                        let size = type_size(program, ty);
                        outs.push(self.read_bytes(slot_base + out, size));
                        out += u64::from(size);
                    }
                    break Outcome::Returned(outs);
                }
                Flow::Panicked(code) => break Outcome::Panicked(code),
            }
        };
        Ok(outcome)
    }

    /// Walks the block that begins at `start` until a terminator transfers
    /// control.
    fn run_block(
        &mut self,
        func: &'a Func,
        start: u32,
        labels: &[u32],
        code_len: u32,
        regs: &mut HashMap<u32, u64>,
        slot_base: u64,
    ) -> Result<Flow, String> {
        let end = labels
            .iter()
            .copied()
            .find(|&label| label > start)
            .unwrap_or(code_len);
        let mut pc = start;
        while pc < end {
            let instr = func
                .code
                .get(pc as usize)
                .ok_or_else(|| format!("program counter {pc} out of range"))?;
            if let Some(flow) = self.exec_instr(instr, regs, slot_base, func)? {
                return Ok(flow);
            }
            pc += 1;
        }
        Err(format!("block at pc {start} fell off the end"))
    }

    /// Executes one instruction, returning a control transfer for terminators.
    #[allow(clippy::too_many_lines)]
    fn exec_instr(
        &mut self,
        instr: &Instr,
        regs: &mut HashMap<u32, u64>,
        slot_base: u64,
        caller: &'a Func,
    ) -> Result<Option<Flow>, String> {
        let program = self.program;
        match instr {
            Instr::Const { dst, imm, ty } => {
                regs.insert(*dst, imm & mask(type_size(program, *ty)));
                Ok(None)
            }
            Instr::ConstData { dst, data, .. } => {
                let off = program
                    .data
                    .get(*data as usize)
                    .map_or(0, |entry| u64::from(entry.global_off));
                regs.insert(*dst, off);
                Ok(None)
            }
            Instr::Copy { dst, src, ty } => {
                let size = type_size(program, *ty);
                if size <= VM_POINTER_BYTES {
                    let value = self.read_scalar(regs, slot_base, *src, size);
                    self.write_scalar(regs, slot_base, *dst, value, size);
                } else {
                    self.copy_aggregate(regs, slot_base, *dst, *src, size);
                }
                Ok(None)
            }
            Instr::CopyBytes { dst, src, size } => {
                let src_abs = slot_base + u64::from(*src);
                let dst_abs = slot_base + u64::from(*dst);
                let bytes = self.read_bytes(src_abs, *size);
                self.write_bytes(dst_abs, &bytes);
                Ok(None)
            }
            Instr::Load { dst, src, ty } => {
                let size = type_size(program, *ty);
                let ref_off = self.slot_ref(slot_base, *src);
                let value = if size <= VM_POINTER_BYTES {
                    self.read_abs(ref_off, size)
                } else {
                    ref_off
                };
                regs.insert(*dst, value);
                Ok(None)
            }
            Instr::Store { src, dst, ty } => {
                let size = type_size(program, *ty);
                let ref_off = self.slot_ref(slot_base, *dst);
                if size <= VM_POINTER_BYTES {
                    let value = self.read_scalar(regs, slot_base, *src, size);
                    self.write_abs(ref_off, value, size);
                } else {
                    self.copy_operand_bytes(regs, slot_base, *src, ref_off, size);
                }
                Ok(None)
            }
            Instr::Addr { dst, place } => {
                regs.insert(*dst, slot_base + u64::from(*place));
                Ok(None)
            }
            Instr::AddrField {
                dst,
                place,
                off,
                indirect,
            } => {
                let base = self.field_base(slot_base, *place, *indirect);
                regs.insert(*dst, base + u64::from(*off));
                Ok(None)
            }
            Instr::AddrFunc { dst, func } => {
                regs.insert(*dst, FUNC_TAG | u64::from(*func));
                Ok(None)
            }
            Instr::Field {
                dst,
                place,
                off,
                ty,
                indirect,
            } => {
                let size = type_size(program, *ty);
                let base = self.field_base(slot_base, *place, *indirect);
                let address = base + u64::from(*off);
                let value = if size <= VM_POINTER_BYTES {
                    self.read_abs(address, size)
                } else {
                    address
                };
                regs.insert(*dst, value);
                Ok(None)
            }
            Instr::StoreField {
                src,
                place,
                off,
                ty,
                indirect,
            } => {
                let size = type_size(program, *ty);
                let base = self.field_base(slot_base, *place, *indirect);
                let address = base + u64::from(*off);
                if size <= VM_POINTER_BYTES {
                    let value = self.read_scalar(regs, slot_base, *src, size);
                    self.write_abs(address, value, size);
                } else {
                    self.copy_operand_bytes(regs, slot_base, *src, address, size);
                }
                Ok(None)
            }
            Instr::Arith {
                op,
                dst,
                lhs,
                rhs,
                operand_ty,
                ty,
            } => {
                let operand_size = type_size(program, *operand_ty);
                let result_size = type_size(program, *ty);
                let left = regs.get(lhs).copied().unwrap_or(0);
                let right = regs.get(rhs).copied().unwrap_or(0);
                let value = eval_arith(*op, left, right, operand_size);
                regs.insert(*dst, value & mask(result_size));
                Ok(None)
            }
            Instr::Convert { dst, src, ty, .. } => {
                let value = regs.get(src).copied().unwrap_or(0);
                regs.insert(*dst, value & mask(type_size(program, *ty)));
                Ok(None)
            }
            Instr::Call { func, ins, outs } => self.call(*func, ins, outs, regs, slot_base),
            Instr::CallIndirect { callee, ins, outs } => {
                let value = regs.get(callee).copied().unwrap_or(0);
                if value & FUNC_TAG == 0 {
                    return Err(format!("invalid function reference {value}"));
                }
                let target = u32::try_from(value & !FUNC_TAG)
                    .map_err(|_| format!("invalid function reference {value}"))?;
                self.call(target, ins, outs, regs, slot_base)
            }
            Instr::CallBuiltin { builtin, ins, outs } => {
                self.call_builtin(*builtin, ins, outs, regs, slot_base)
            }
            Instr::Branch { target } => Ok(Some(Flow::Goto(*target))),
            Instr::BranchIf { cond, then, els } => {
                let value = regs.get(cond).copied().unwrap_or(0);
                Ok(Some(Flow::Goto(if value == 0 { *els } else { *then })))
            }
            Instr::Return => Ok(Some(Flow::Return)),
            Instr::Panic { data } => {
                let message = program
                    .data
                    .get(*data as usize)
                    .map_or(&[][..], |entry| entry.bytes.as_slice());
                eprintln!("panic: {}", String::from_utf8_lossy(message));
                Ok(Some(Flow::Panicked(ABORT_CODE)))
            }
            Instr::Unreachable => Err(format!(
                "executed an `unreachable` terminator in `{}`",
                caller.name
            )),
        }
    }

    /// Gathers a call's input bytes and recursively executes the callee.
    fn call(
        &mut self,
        target: u32,
        ins: &[Operand],
        outs: &[u32],
        regs: &HashMap<u32, u64>,
        slot_base: u64,
    ) -> Result<Option<Flow>, String> {
        let program = self.program;
        let callee = program
            .funcs
            .get(target as usize)
            .ok_or_else(|| format!("function id {target} out of range"))?;
        if callee.is_extern {
            return Err(format!("cannot call extern function `{}`", callee.name));
        }
        let mut args = Vec::with_capacity(callee.ins.len());
        for (operand, ty) in ins.iter().zip(&callee.ins) {
            args.push(self.operand_bytes(regs, slot_base, *operand, type_size(program, *ty)));
        }
        match self.execute_function(target, &args)? {
            Outcome::Returned(values) => {
                self.write_out_slots(slot_base, outs, &values);
                Ok(None)
            }
            Outcome::Panicked(code) => Ok(Some(Flow::Panicked(code))),
        }
    }

    /// Executes a compiler leaf builtin.
    fn call_builtin(
        &mut self,
        builtin: Builtin,
        ins: &[Operand],
        outs: &[u32],
        regs: &HashMap<u32, u64>,
        slot_base: u64,
    ) -> Result<Option<Flow>, String> {
        match builtin {
            Builtin::PtrAdd => {
                let (base, offset) = match ins {
                    [base, offset] => (*base, *offset),
                    _ => return Err("@ptr_add expects two inputs".to_string()),
                };
                let base = self.read_scalar(regs, slot_base, base, VM_POINTER_BYTES);
                let offset = self.read_scalar(regs, slot_base, offset, VM_POINTER_BYTES);
                let out = outs
                    .first()
                    .ok_or_else(|| "@ptr_add expects one output".to_string())?;
                self.write_abs(
                    slot_base + u64::from(*out),
                    base.wrapping_add(offset),
                    VM_POINTER_BYTES,
                );
                Ok(None)
            }
            Builtin::Memcpy => {
                let (dst, src, len) = match ins {
                    [dst, src, len] => (*dst, *src, *len),
                    _ => return Err("@memcpy expects three inputs".to_string()),
                };
                let dst = self.read_scalar(regs, slot_base, dst, VM_POINTER_BYTES);
                let src = self.read_scalar(regs, slot_base, src, VM_POINTER_BYTES);
                let len = self.read_scalar(regs, slot_base, len, VM_POINTER_BYTES);
                let bytes = self.read_bytes(src, u32::try_from(len).unwrap_or(u32::MAX));
                self.write_bytes(dst, &bytes);
                Ok(None)
            }
            Builtin::Abort => Ok(Some(Flow::Panicked(ABORT_CODE))),
        }
    }

    /// Copies an aggregate operand's bytes to an absolute address.
    fn copy_operand_bytes(
        &mut self,
        regs: &HashMap<u32, u64>,
        slot_base: u64,
        operand: Operand,
        dst_abs: u64,
        size: u32,
    ) {
        let src_abs = match operand {
            Operand::Reg(reg) => regs.get(&reg).copied().unwrap_or(0),
            Operand::Slot(off) => slot_base + u64::from(off),
        };
        let bytes = self.read_bytes(src_abs, size);
        self.write_bytes(dst_abs, &bytes);
    }

    /// Copies an aggregate value for a `Copy` instruction.
    fn copy_aggregate(
        &mut self,
        regs: &mut HashMap<u32, u64>,
        slot_base: u64,
        dst: Operand,
        src: Operand,
        size: u32,
    ) {
        match (dst, src) {
            (Operand::Slot(dst_off), Operand::Reg(src_reg)) => {
                let src_abs = regs.get(&src_reg).copied().unwrap_or(0);
                let bytes = self.read_bytes(src_abs, size);
                self.write_bytes(slot_base + u64::from(dst_off), &bytes);
            }
            (Operand::Reg(dst_reg), Operand::Slot(src_off)) => {
                regs.insert(dst_reg, slot_base + u64::from(src_off));
            }
            (Operand::Slot(dst_off), Operand::Slot(src_off)) => {
                let bytes = self.read_bytes(slot_base + u64::from(src_off), size);
                self.write_bytes(slot_base + u64::from(dst_off), &bytes);
            }
            (Operand::Reg(dst_reg), Operand::Reg(src_reg)) => {
                let value = regs.get(&src_reg).copied().unwrap_or(0);
                regs.insert(dst_reg, value);
            }
        }
    }

    /// Reads an operand as a fresh byte buffer.
    fn operand_bytes(
        &self,
        regs: &HashMap<u32, u64>,
        slot_base: u64,
        operand: Operand,
        size: u32,
    ) -> Vec<u8> {
        match operand {
            Operand::Reg(reg) => {
                let value = regs.get(&reg).copied().unwrap_or(0);
                if size <= VM_POINTER_BYTES {
                    low_bytes(value, size)
                } else {
                    self.read_bytes(value, size)
                }
            }
            Operand::Slot(off) => self.read_bytes(slot_base + u64::from(off), size),
        }
    }

    /// Writes a call's returned byte buffers into the caller's output slots.
    fn write_out_slots(&mut self, slot_base: u64, outs: &[u32], values: &[Vec<u8>]) {
        for (off, bytes) in outs.iter().zip(values) {
            self.write_bytes(slot_base + u64::from(*off), bytes);
        }
    }

    /// Reads the pointer-sized ref stored in a frame slot.
    fn slot_ref(&self, slot_base: u64, place: u32) -> u64 {
        self.read_abs(slot_base + u64::from(place), VM_POINTER_BYTES)
    }

    /// Resolves a field instruction's object base.
    fn field_base(&self, slot_base: u64, place: u32, indirect: bool) -> u64 {
        let slot_abs = slot_base + u64::from(place);
        if indirect {
            self.read_abs(slot_abs, VM_POINTER_BYTES)
        } else {
            slot_abs
        }
    }

    /// Reads an operand as its low `size` bytes.
    fn read_scalar(
        &self,
        regs: &HashMap<u32, u64>,
        slot_base: u64,
        operand: Operand,
        size: u32,
    ) -> u64 {
        match operand {
            Operand::Reg(reg) => regs.get(&reg).copied().unwrap_or(0),
            Operand::Slot(off) => self.read_abs(slot_base + u64::from(off), size),
        }
    }

    /// Writes an operand's low `size` bytes.
    fn write_scalar(
        &mut self,
        regs: &mut HashMap<u32, u64>,
        slot_base: u64,
        dst: Operand,
        value: u64,
        size: u32,
    ) {
        match dst {
            Operand::Reg(reg) => {
                regs.insert(reg, value & mask(size));
            }
            Operand::Slot(off) => {
                self.write_abs(slot_base + u64::from(off), value, size);
            }
        }
    }

    /// Reads `size` little-endian bytes at an absolute address (absent is 0).
    fn read_abs(&self, address: u64, size: u32) -> u64 {
        let mut value = 0_u64;
        for index in 0..size.min(8) {
            let byte = self.memory.get(&(address + u64::from(index))).copied();
            value |= u64::from(byte.unwrap_or(0)) << (index * 8);
        }
        value
    }

    /// Writes `size` little-endian bytes at an absolute address.
    fn write_abs(&mut self, address: u64, value: u64, size: u32) {
        for index in 0..size.min(8) {
            self.memory
                .insert(address + u64::from(index), (value >> (index * 8)) as u8);
        }
    }

    /// Reads `size` raw bytes at an absolute address.
    fn read_bytes(&self, address: u64, size: u32) -> Vec<u8> {
        (0..size)
            .map(|index| {
                self.memory
                    .get(&(address + u64::from(index)))
                    .copied()
                    .unwrap_or(0)
            })
            .collect()
    }

    /// Writes raw `bytes` at an absolute address.
    fn write_bytes(&mut self, address: u64, bytes: &[u8]) {
        for (index, &byte) in bytes.iter().enumerate() {
            self.memory.insert(address + index as u64, byte);
        }
    }
}

/// The size of a type id (zero when out of range).
fn type_size(program: &Program, ty: TypeId) -> u32 {
    program.types.get(ty as usize).map_or(0, |info| info.size)
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

/// The low `size` bytes of `value`.
fn low_bytes(value: u64, size: u32) -> Vec<u8> {
    let bytes = value.to_le_bytes();
    let len = usize::try_from(size.min(8)).unwrap_or(8);
    bytes[..len].to_vec()
}

/// Sign-extends the low `size` bytes of `value` to `i64`.
fn sign_extend(value: u64, size: u32) -> i64 {
    if size == 0 {
        return 0;
    }
    let bits = size.min(8) * 8;
    let shift = 64 - bits;
    ((value << shift) as i64) >> shift
}

/// Evaluates an integer arithmetic/comparison operator.
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
