//! Bench-only tree-walking baseline interpreter.
//!
//! This module is a deliberately **naive, correctness-first** interpreter over
//! the same compact `u32` word stream the optimized [`crate::Machine`] runs. It
//! exists only to give the benchmark a stable reference point: the VM must be
//! at least 3× faster than this baseline on every suite entry. It is never
//! compiled into a shipped build — the module is gated behind the
//! `bench-baseline` feature.
//!
//! Where [`crate::Machine`] keeps hot state in locals and dispatches over a flat
//! instruction array inside one inner loop, this baseline walks the code **a
//! block at a time**, re-deriving block bounds and decoding every instruction
//! through [`decode`]. It recurses for calls and marshals arguments/results
//! through owned `Vec<u8>` buffers, so it is still a tree-walker rather than a
//! bytecode dispatch loop.
//!
//! Its execution representation, however, is the same reasonable one the VM
//! uses (spec §8): per-frame registers live in a `Vec<u64>`, and memory is one
//! contiguous byte arena (globals followed by each frame's zeroed slot region)
//! with `ref`s as byte offsets. Earlier revisions used a per-frame `HashMap` and
//! a sparse `HashMap<u64, u8>` memory model, which made the baseline
//! pathologically slow and the ≥3× gate trivial; this is the fair reference.
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

use crate::decode::{decode, Decoded};
use crate::isa::{ArithOp, Builtin, Func, Operand, Program, TypeId, VM_POINTER_BYTES};

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
    match interp.execute_function(main, &[], &[])? {
        Outcome::Returned => Ok(0),
        Outcome::Panicked(code) => Ok(code),
    }
}

/// The result of executing one function body.
enum Outcome {
    /// The body returned; outputs were written by reference into the caller's
    /// storage, so there is nothing to return.
    Returned,
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
    /// One contiguous little-endian arena: globals followed by each frame's
    /// zeroed slot region. A `ref` is a byte offset into it.
    memory: Vec<u8>,
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
        // The arena starts with the globals, so a data constant's `global_off`
        // is directly a valid ref (matching the VM).
        let memory = program.globals.clone();
        Self {
            program,
            funcs,
            memory,
        }
    }

    /// The index of a named entry function.
    fn entry(&self, name: &str) -> Result<u32, String> {
        self.funcs
            .get(name)
            .copied()
            .ok_or_else(|| format!("no entry point `{name}`"))
    }

    /// Appends `size` fresh, zero-initialised bytes to the arena and returns
    /// their base offset.
    fn alloc(&mut self, size: u64) -> u64 {
        let base = self.memory.len() as u64;
        let owned = usize::try_from(size).unwrap_or(usize::MAX);
        let end = self.memory.len().saturating_add(owned);
        self.memory.resize(end, 0);
        base
    }

    /// Executes one function with `args` bound into its input region and the
    /// caller's `out_refs` (absolute addresses) installed in the out-ref region.
    fn execute_function(
        &mut self,
        func_index: u32,
        args: &[Vec<u8>],
        out_refs: &[u64],
    ) -> Result<Outcome, String> {
        let program = self.program;
        let func = program
            .funcs
            .get(func_index as usize)
            .ok_or_else(|| format!("function id {func_index} out of range"))?;
        if func.is_extern {
            return Err(format!("cannot call extern function `{}`", func.name));
        }
        let slot_base = self.alloc(u64::from(func.slot_bytes));
        // The frame begins with the out-ref region: one u64 ref per output.
        for (index, &addr) in out_refs.iter().enumerate() {
            let cell = slot_base + (index as u64) * u64::from(VM_POINTER_BYTES);
            self.write_abs(cell, addr, VM_POINTER_BYTES);
        }
        // Inputs are copied into the region after the out-ref cells.
        for (arg, &off) in args.iter().zip(&func.in_offsets) {
            self.write_bytes(slot_base + u64::from(off), arg);
        }

        let mut regs = vec![0_u64; usize::try_from(func.n_regs).unwrap_or(0)];
        let labels: Vec<u32> = func.labels().into_iter().collect();
        let code_len = u32::try_from(func.code.len()).map_err(|_| "function too large")?;
        let mut pc = 0_u32;
        let outcome = loop {
            match self.run_block(func, pc, &labels, code_len, &mut regs, slot_base)? {
                Flow::Goto(next) => pc = next,
                Flow::Return => break Outcome::Returned,
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
        regs: &mut [u64],
        slot_base: u64,
    ) -> Result<Flow, String> {
        let end = labels
            .iter()
            .copied()
            .find(|&label| label > start)
            .unwrap_or(code_len);
        let mut pc = start;
        while pc < end {
            let (instr, next) = decode(&func.code, pc as usize, &func.call_sites, &func.operands)
                .map_err(|err| format!("decode at pc {pc}: {err}"))?;
            if let Some(flow) = self.exec_instr(&instr, regs, slot_base, func)? {
                return Ok(flow);
            }
            pc = next;
        }
        // A block may fall through into the next one when the compiler removed
        // an explicit `branch` to the immediately following instruction.
        if end < code_len {
            Ok(Flow::Goto(end))
        } else {
            Err(format!("block at pc {start} fell off the end"))
        }
    }

    /// Executes one decoded instruction, returning a control transfer for
    /// terminators.
    #[allow(clippy::too_many_lines)]
    fn exec_instr(
        &mut self,
        instr: &Decoded<'a>,
        regs: &mut [u64],
        slot_base: u64,
        caller: &'a Func,
    ) -> Result<Option<Flow>, String> {
        let program = self.program;
        if let Some((op, dst, lhs, rhs, width)) = arith_parts(instr) {
            let size = u32::from(width) / 8;
            let left = get_reg(regs, lhs);
            let right = get_reg(regs, rhs);
            let value = eval_arith(op, left, right, size);
            set_reg(regs, dst, value & mask(size));
            return Ok(None);
        }
        match instr {
            Decoded::Const { dst, imm } => {
                set_reg(regs, *dst, *imm);
                Ok(None)
            }
            Decoded::ConstData { dst, data } => {
                let off = program
                    .data
                    .get(*data as usize)
                    .map_or(0, |entry| u64::from(entry.global_off));
                set_reg(regs, *dst, off);
                Ok(None)
            }
            Decoded::Copy { dst, src, size } => {
                if *size <= VM_POINTER_BYTES {
                    let value = self.read_scalar(regs, slot_base, *src, *size);
                    self.write_scalar(regs, slot_base, *dst, value, *size);
                } else {
                    self.copy_aggregate(regs, slot_base, *dst, *src, *size);
                }
                Ok(None)
            }
            Decoded::CopyBytes { dst, src, size } => {
                let src_abs = slot_base + u64::from(*src);
                let dst_abs = slot_base + u64::from(*dst);
                let bytes = self.read_bytes(src_abs, *size);
                self.write_bytes(dst_abs, &bytes);
                Ok(None)
            }
            Decoded::Load { dst, src, size } => {
                let ref_off = self.slot_ref(slot_base, *src);
                let value = if *size <= VM_POINTER_BYTES {
                    self.read_abs(ref_off, *size)
                } else {
                    ref_off
                };
                set_reg(regs, *dst, value);
                Ok(None)
            }
            Decoded::Store { src, dst, size } => {
                let ref_off = self.slot_ref(slot_base, *dst);
                if *size <= VM_POINTER_BYTES {
                    let value = self.read_scalar(regs, slot_base, *src, *size);
                    self.write_abs(ref_off, value, *size);
                } else {
                    self.copy_operand_bytes(regs, slot_base, *src, ref_off, *size);
                }
                Ok(None)
            }
            Decoded::Addr { dst, place } => {
                set_reg(regs, *dst, slot_base + u64::from(*place));
                Ok(None)
            }
            Decoded::AddrField {
                dst,
                place,
                off,
                indirect,
            } => {
                let base = self.field_base(slot_base, *place, *indirect);
                set_reg(regs, *dst, base + u64::from(*off));
                Ok(None)
            }
            Decoded::AddrFunc { dst, func } => {
                set_reg(regs, *dst, FUNC_TAG | u64::from(*func));
                Ok(None)
            }
            Decoded::Field {
                dst,
                place,
                off,
                size,
                indirect,
            } => {
                let base = self.field_base(slot_base, *place, *indirect);
                let address = base + u64::from(*off);
                let value = if *size <= VM_POINTER_BYTES {
                    self.read_abs(address, *size)
                } else {
                    address
                };
                set_reg(regs, *dst, value);
                Ok(None)
            }
            Decoded::StoreField {
                src,
                place,
                off,
                size,
                indirect,
            } => {
                let base = self.field_base(slot_base, *place, *indirect);
                let address = base + u64::from(*off);
                if *size <= VM_POINTER_BYTES {
                    let value = self.read_scalar(regs, slot_base, *src, *size);
                    self.write_abs(address, value, *size);
                } else {
                    self.copy_operand_bytes(regs, slot_base, *src, address, *size);
                }
                Ok(None)
            }
            Decoded::Trunc { dst, src, width } => {
                let value = get_reg(regs, *src);
                set_reg(regs, *dst, value & mask(u32::from(*width) / 8));
                Ok(None)
            }
            Decoded::ExtendZero { dst, src } => {
                let value = get_reg(regs, *src);
                set_reg(regs, *dst, value);
                Ok(None)
            }
            Decoded::ArithImm {
                dst,
                lhs,
                lhs_slot,
                op,
                width,
                imm,
            } => {
                let size = u32::from(*width) / 8;
                let left = if *lhs_slot {
                    self.read_scalar(regs, slot_base, Operand::Slot(*lhs), size)
                } else {
                    get_reg(regs, *lhs)
                };
                let value = eval_arith(*op, left, *imm, size);
                set_reg(regs, *dst, value & mask(size));
                Ok(None)
            }
            Decoded::Call { site, ins, outs } => self.call(site.func, ins, outs, regs, slot_base),
            Decoded::CallIndirect {
                callee,
                site: _,
                ins,
                outs,
            } => {
                let value = get_reg(regs, *callee);
                if value & FUNC_TAG == 0 {
                    return Err(format!("invalid function reference {value}"));
                }
                let target = u32::try_from(value & !FUNC_TAG)
                    .map_err(|_| format!("invalid function reference {value}"))?;
                self.call(target, ins, outs, regs, slot_base)
            }
            Decoded::CallBuiltin {
                builtin, ins, outs, ..
            } => self.call_builtin(*builtin, ins, outs, regs, slot_base),
            Decoded::Branch { target } => Ok(Some(Flow::Goto(*target))),
            Decoded::BranchIf { cond, then, els } => {
                let value = get_reg(regs, *cond);
                Ok(Some(Flow::Goto(if value == 0 { *els } else { *then })))
            }
            Decoded::Return => Ok(Some(Flow::Return)),
            Decoded::Panic { data } => {
                let message = program
                    .data
                    .get(*data as usize)
                    .map_or(&[][..], |entry| entry.bytes.as_slice());
                eprintln!("panic: {}", String::from_utf8_lossy(message));
                Ok(Some(Flow::Panicked(ABORT_CODE)))
            }
            Decoded::Unreachable => Err(format!(
                "executed an `unreachable` terminator in `{}`",
                caller.name
            )),
            // Every arithmetic/comparison variant is handled by `arith_parts`
            // above; this arm only satisfies exhaustiveness.
            _ => Err("unexpected decoded instruction".to_string()),
        }
    }

    /// Gathers a call's input bytes and recursively executes the callee.
    fn call(
        &mut self,
        target: u32,
        ins: &[Operand],
        outs: &[Operand],
        regs: &[u64],
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
        // Resolve each caller out place to its absolute address; the callee
        // writes them directly (pointer ABI).
        let mut out_refs = Vec::with_capacity(outs.len());
        for operand in outs {
            let addr = match operand {
                Operand::Slot(off) => slot_base + u64::from(*off),
                Operand::Reg(reg) => get_reg(regs, *reg),
            };
            out_refs.push(addr);
        }
        match self.execute_function(target, &args, &out_refs)? {
            Outcome::Returned => Ok(None),
            Outcome::Panicked(code) => Ok(Some(Flow::Panicked(code))),
        }
    }

    /// Executes a compiler leaf builtin.
    fn call_builtin(
        &mut self,
        builtin: Builtin,
        ins: &[Operand],
        outs: &[Operand],
        regs: &[u64],
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
                let out = match outs.first() {
                    Some(Operand::Slot(out)) => *out,
                    _ => return Err("@ptr_add expects one output".to_string()),
                };
                self.write_abs(
                    slot_base + u64::from(out),
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
        regs: &[u64],
        slot_base: u64,
        operand: Operand,
        dst_abs: u64,
        size: u32,
    ) {
        let src_abs = match operand {
            Operand::Reg(reg) => get_reg(regs, reg),
            Operand::Slot(off) => slot_base + u64::from(off),
        };
        let bytes = self.read_bytes(src_abs, size);
        self.write_bytes(dst_abs, &bytes);
    }

    /// Copies an aggregate value for a `Copy` instruction.
    fn copy_aggregate(
        &mut self,
        regs: &mut [u64],
        slot_base: u64,
        dst: Operand,
        src: Operand,
        size: u32,
    ) {
        match (dst, src) {
            (Operand::Slot(dst_off), Operand::Reg(src_reg)) => {
                let src_abs = get_reg(regs, src_reg);
                let bytes = self.read_bytes(src_abs, size);
                self.write_bytes(slot_base + u64::from(dst_off), &bytes);
            }
            (Operand::Reg(dst_reg), Operand::Slot(src_off)) => {
                set_reg(regs, dst_reg, slot_base + u64::from(src_off));
            }
            (Operand::Slot(dst_off), Operand::Slot(src_off)) => {
                let bytes = self.read_bytes(slot_base + u64::from(src_off), size);
                self.write_bytes(slot_base + u64::from(dst_off), &bytes);
            }
            (Operand::Reg(dst_reg), Operand::Reg(src_reg)) => {
                let value = get_reg(regs, src_reg);
                set_reg(regs, dst_reg, value);
            }
        }
    }

    /// Reads an operand as a fresh byte buffer.
    fn operand_bytes(&self, regs: &[u64], slot_base: u64, operand: Operand, size: u32) -> Vec<u8> {
        match operand {
            Operand::Reg(reg) => {
                let value = get_reg(regs, reg);
                if size <= VM_POINTER_BYTES {
                    low_bytes(value, size)
                } else {
                    self.read_bytes(value, size)
                }
            }
            Operand::Slot(off) => self.read_bytes(slot_base + u64::from(off), size),
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
    fn read_scalar(&self, regs: &[u64], slot_base: u64, operand: Operand, size: u32) -> u64 {
        match operand {
            Operand::Reg(reg) => get_reg(regs, reg),
            Operand::Slot(off) => self.read_abs(slot_base + u64::from(off), size),
        }
    }

    /// Writes an operand's low `size` bytes.
    fn write_scalar(
        &mut self,
        regs: &mut [u64],
        slot_base: u64,
        dst: Operand,
        value: u64,
        size: u32,
    ) {
        match dst {
            Operand::Reg(reg) => set_reg(regs, reg, value & mask(size)),
            Operand::Slot(off) => {
                self.write_abs(slot_base + u64::from(off), value, size);
            }
        }
    }

    /// Reads `size` little-endian bytes at an absolute address (absent is 0).
    fn read_abs(&self, address: u64, size: u32) -> u64 {
        let start = address as usize;
        let mut value = 0_u64;
        for index in 0..size.min(8) {
            let byte = self
                .memory
                .get(start + index as usize)
                .copied()
                .unwrap_or(0);
            value |= u64::from(byte) << (index * 8);
        }
        value
    }

    /// Writes `size` little-endian bytes at an absolute address.
    fn write_abs(&mut self, address: u64, value: u64, size: u32) {
        let len = size.min(8);
        let start = address as usize;
        let end = start.saturating_add(len as usize);
        if end > self.memory.len() {
            self.memory.resize(end, 0);
        }
        for index in 0..len {
            self.memory[start + index as usize] = (value >> (index * 8)) as u8;
        }
    }

    /// Reads `size` raw bytes at an absolute address.
    fn read_bytes(&self, address: u64, size: u32) -> Vec<u8> {
        let start = address as usize;
        (0..size)
            .map(|index| {
                self.memory
                    .get(start + index as usize)
                    .copied()
                    .unwrap_or(0)
            })
            .collect()
    }

    /// Writes raw `bytes` at an absolute address.
    fn write_bytes(&mut self, address: u64, bytes: &[u8]) {
        let start = address as usize;
        let end = start.saturating_add(bytes.len());
        if end > self.memory.len() {
            self.memory.resize(end, 0);
        }
        self.memory[start..start + bytes.len()].copy_from_slice(bytes);
    }
}

/// Reads a register of a frame (zero when out of range).
fn get_reg(regs: &[u64], reg: u32) -> u64 {
    regs.get(reg as usize).copied().unwrap_or(0)
}

/// Writes a register of a frame (ignored when out of range).
fn set_reg(regs: &mut [u64], reg: u32, value: u64) {
    if let Some(slot) = regs.get_mut(reg as usize) {
        *slot = value;
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

/// Extracts `(op, dst, lhs, rhs, width_bits)` from a decoded arithmetic
/// instruction, or `None` for any other instruction.
#[allow(clippy::too_many_lines)]
fn arith_parts(instr: &Decoded) -> Option<(ArithOp, u32, u32, u32, u8)> {
    let (op, dst, lhs, rhs, width) = match instr {
        Decoded::IAdd {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::IAdd, *dst, *lhs, *rhs, *width),
        Decoded::ISub {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::ISub, *dst, *lhs, *rhs, *width),
        Decoded::IMul {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::IMul, *dst, *lhs, *rhs, *width),
        Decoded::IDiv {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::IDiv, *dst, *lhs, *rhs, *width),
        Decoded::IMod {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::IMod, *dst, *lhs, *rhs, *width),
        Decoded::ILt {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::ILt, *dst, *lhs, *rhs, *width),
        Decoded::IGt {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::IGt, *dst, *lhs, *rhs, *width),
        Decoded::IDivU {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::IDivU, *dst, *lhs, *rhs, *width),
        Decoded::IModU {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::IModU, *dst, *lhs, *rhs, *width),
        Decoded::Eq {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::Eq, *dst, *lhs, *rhs, *width),
        Decoded::ILtU {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::ILtU, *dst, *lhs, *rhs, *width),
        Decoded::IGtU {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::IGtU, *dst, *lhs, *rhs, *width),
        Decoded::FAdd {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::FAdd, *dst, *lhs, *rhs, *width),
        Decoded::FSub {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::FSub, *dst, *lhs, *rhs, *width),
        Decoded::FMul {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::FMul, *dst, *lhs, *rhs, *width),
        Decoded::FDiv {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::FDiv, *dst, *lhs, *rhs, *width),
        Decoded::FMod {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::FMod, *dst, *lhs, *rhs, *width),
        Decoded::FLt {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::FLt, *dst, *lhs, *rhs, *width),
        Decoded::FGt {
            dst,
            lhs,
            rhs,
            width,
        } => (ArithOp::FGt, *dst, *lhs, *rhs, *width),
        _ => return None,
    };
    Some((op, dst, lhs, rhs, width))
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
