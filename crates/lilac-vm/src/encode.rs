//! Compact `u32` encoder for the bytecode ISA.
//!
//! [`CodeBuilder`] lowers typed [`crate::isa::Instr`] values to the executed
//! word stream. The layout follows the M5c.1 design §3: word 0 is
//! `[op:8 | a:8 | b:8 | c:8]`, an operand `>= 256` escapes to the wide form
//! (bit 7 of the opcode set, operands moved to extension words), immediate
//! forms carry a 24-bit field in `w >> 8`, and calls append a [`CallSite`] to a
//! pool addressed by a 24-bit index.
//!
//! Type metadata is **precomputed**: `CodeBuilder` borrows the program type
//! table and bakes each `TypeId` down to its byte size (or, for arithmetic, its
//! width class) at encode time, so the interpreter never looks up a type.

use crate::isa::{ArithOp, Builtin, CallSite, ConvertOp, Instr, Operand, TypeInfo};

/// Bit 7 of the opcode byte; set when an instruction uses the wide form.
pub const WIDE_FLAG: u8 = 0x80;

/// Highest value that fits in a 24-bit field (`w >> 8`).
pub const FIELD24_MAX: u32 = 0x00FF_FFFF;

/// Highest value that fits in a narrow (one-byte) operand.
pub const NARROW_MAX: u32 = 0xFF;

// Core opcodes (all `< 128` so bit 7 is free).
/// `Return`.
pub const OP_RETURN: u8 = 0x01;
/// `Unreachable`.
pub const OP_UNREACHABLE: u8 = 0x02;
/// `Panic` (24-bit data index in `w >> 8`).
pub const OP_PANIC: u8 = 0x03;
/// `Branch` (24-bit target in `w >> 8`).
pub const OP_BRANCH: u8 = 0x04;
/// `BranchIf`.
pub const OP_BRANCH_IF: u8 = 0x05;
/// `Const` (`[op][dst]` plus two immediate words).
pub const OP_CONST: u8 = 0x06;
/// `ConstData` (`[op][dst]` plus a data-index word).
pub const OP_CONST_DATA: u8 = 0x07;
/// First `Copy` kind opcode (`Reg`→`Reg`); three more follow.
pub const OP_COPY_BASE: u8 = 0x08;
/// Last `Copy` kind opcode (`Slot`→`Slot`).
pub const OP_COPY_LAST: u8 = 0x0B;
/// `CopyBytes`.
pub const OP_COPY_BYTES: u8 = 0x0C;
/// `Load`.
pub const OP_LOAD: u8 = 0x0D;
/// First `Store` kind opcode (`Reg` source).
pub const OP_STORE_BASE: u8 = 0x0E;
/// Last `Store` kind opcode (`Slot` source).
pub const OP_STORE_LAST: u8 = 0x0F;
/// `Addr`.
pub const OP_ADDR: u8 = 0x10;
/// First `AddrField` opcode (direct); indirect follows.
pub const OP_ADDRFIELD_BASE: u8 = 0x11;
/// Last `AddrField` opcode (indirect).
pub const OP_ADDRFIELD_LAST: u8 = 0x12;
/// `AddrFunc`.
pub const OP_ADDR_FUNC: u8 = 0x13;
/// First `Field` opcode (direct); indirect follows.
pub const OP_FIELD_BASE: u8 = 0x14;
/// Last `Field` opcode (indirect).
pub const OP_FIELD_LAST: u8 = 0x15;
/// First `StoreField` opcode (`Reg` source, direct); three more follow.
pub const OP_STOREFIELD_BASE: u8 = 0x16;
/// Last `StoreField` opcode (`Slot` source, indirect).
pub const OP_STOREFIELD_LAST: u8 = 0x19;
/// `Call` (24-bit call-site index in `w >> 8`).
pub const OP_CALL: u8 = 0x1A;
/// `CallIndirect` (callee operand plus one extra call-site word).
pub const OP_CALL_INDIRECT: u8 = 0x1B;
/// First `CallBuiltin` opcode (`@ptr_add`).
pub const OP_CALL_BUILTIN_BASE: u8 = 0x1C;
/// Last `CallBuiltin` opcode (`@lilac_abort`).
pub const OP_CALL_BUILTIN_LAST: u8 = 0x1E;

// Width-specialized arithmetic/convert opcodes (one word each).
/// Base of the `IAdd` width block (8/16/32/64).
pub const OP_IADD_BASE: u8 = 0x40;
/// Base of the `ISub` width block.
pub const OP_ISUB_BASE: u8 = 0x44;
/// Base of the `IMul` width block.
pub const OP_IMUL_BASE: u8 = 0x48;
/// Base of the signed `IDiv` width block.
pub const OP_IDIV_BASE: u8 = 0x4C;
/// Base of the signed `IMod` width block.
pub const OP_IMOD_BASE: u8 = 0x50;
/// Base of the signed `ILt` width block.
pub const OP_ILT_BASE: u8 = 0x54;
/// Base of the signed `IGt` width block.
pub const OP_IGT_BASE: u8 = 0x58;
/// Base of the `Trunc` width block.
pub const OP_TRUNC_BASE: u8 = 0x5C;
/// Base of the `FAdd` width block (32/64).
pub const OP_FADD_BASE: u8 = 0x60;
/// Base of the `FSub` width block.
pub const OP_FSUB_BASE: u8 = 0x62;
/// Base of the `FMul` width block.
pub const OP_FMUL_BASE: u8 = 0x64;
/// Base of the `FDiv` width block.
pub const OP_FDIV_BASE: u8 = 0x66;
/// Base of the `FMod` width block.
pub const OP_FMOD_BASE: u8 = 0x68;
/// Base of the `FLt` width block.
pub const OP_FLT_BASE: u8 = 0x6A;
/// Base of the `FGt` width block.
pub const OP_FGT_BASE: u8 = 0x6C;
/// Base of the unsigned `IDivU` width block (8/16/32/64).
///
/// Ruling R6: the width classes are retained so operands are masked to their
/// declared width, matching M5c semantics.
pub const OP_IDIVU_BASE: u8 = 0x20;
/// Base of the unsigned `IModU` width block.
pub const OP_IMODU_BASE: u8 = 0x24;
/// Base of the bit-equality `Eq` width block.
pub const OP_EQ_BASE: u8 = 0x28;
/// Base of the unsigned `ILtU` width block.
pub const OP_ILTU_BASE: u8 = 0x2C;
/// Base of the unsigned `IGtU` width block.
pub const OP_IGTU_BASE: u8 = 0x30;
/// Width-free `ExtendZero`.
pub const OP_EXTEND_ZERO: u8 = 0x73;

/// Fused arithmetic with an immediate right operand.
///
/// Layout: `[op | dst:8 | lhs:8 | (slot:1 | op_code:7)]`, followed by the
/// little-endian low and high words of the `u64` immediate. `op_code` is a
/// width-specialized arithmetic opcode (as produced by [`arith_opcode`]); the
/// low bit of `flags` distinguishes a slot `lhs` from a register `lhs`.
pub const OP_ARITH_IMM: u8 = 0x34;

/// Fused arithmetic comparison + conditional branch.
///
/// Layout: `[op | lhs:8 | (slot:1 | op_code:7) | unused:8]`, then the `then`
/// target, the `els` target, and the little-endian low/high words of the `u64`
/// immediate (`u64`). `op_code` is a width-specialized comparison opcode.
pub const OP_BRANCH_IF_IMM: u8 = 0x35;

/// Fused `ArithImm` feeding a direct call's single input register.
///
/// Layout: `[op | lhs:8 | flags:8 | op_code:8]`, then the call-site index, then
/// the little-endian low/high words of the `u64` immediate.
pub const OP_CALL_ARITH_IMM: u8 = 0x36;

/// Direct call whose single output is copied into a register on return.
///
/// Layout: `[op | dst:8 | size:8 | 0]`, then the call-site index.
pub const OP_CALL_OUT: u8 = 0x37;

/// Fused arithmetic + direct call + output copy.
///
/// Layout: `[op | lhs:8 | flags:8 | op_code:8]`, the call-site index,
/// `[dst:8 | size:8]`, then the immediate's low/high words.
pub const OP_CALL_ARITH_OUT: u8 = 0x38;

/// Builds the compact word stream and its operand pools.
///
/// Borrows the program type table so sizes/widths are computed at encode time.
#[derive(Clone, Debug)]
pub struct CodeBuilder<'a> {
    types: &'a [TypeInfo],
    /// The executed instruction stream.
    pub words: Vec<u32>,
    /// Resolved call sites appended by `Call`/`CallIndirect`/`CallBuiltin`.
    pub call_sites: Vec<CallSite>,
    /// Shared operand pool indexed by each call site's `ins`/`outs` ranges.
    pub operands: Vec<Operand>,
}

impl<'a> CodeBuilder<'a> {
    /// Creates an empty builder over the program's type table.
    #[must_use]
    pub const fn new(types: &'a [TypeInfo]) -> Self {
        Self {
            types,
            words: Vec::new(),
            call_sites: Vec::new(),
            operands: Vec::new(),
        }
    }

    /// Appends the encoding of `instr`.
    ///
    /// # Errors
    ///
    /// Returns a message if a type id is out of range, an arithmetic operand
    /// size has no width class, a wide `a`/`b` operand exceeds `u16`, or a
    /// 24-bit index exceeds [`FIELD24_MAX`].
    #[allow(clippy::too_many_lines)]
    pub fn push(&mut self, instr: &Instr) -> Result<(), String> {
        match instr {
            Instr::Const { dst, imm, ty } => {
                let size = self.type_size(*ty)?;
                self.emit_ops(OP_CONST, &[*dst])?;
                let masked = *imm & mask_for_size(size);
                let bytes = masked.to_le_bytes();
                self.words
                    .push(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
                self.words
                    .push(u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]));
            }
            Instr::ConstData { dst, data, .. } => {
                self.emit_ops(OP_CONST_DATA, &[*dst])?;
                self.words.push(*data);
            }
            Instr::Copy { dst, src, ty } => {
                let size = self.type_size(*ty)?;
                let op = OP_COPY_BASE + kind(*dst) * 2 + kind(*src);
                self.emit_ops(op, &[operand_value(*dst), operand_value(*src), size])?;
            }
            Instr::CopyBytes { dst, src, size } => {
                self.emit_ops(OP_COPY_BYTES, &[*dst, *src, *size])?;
            }
            Instr::Load { dst, src, ty } => {
                let size = self.type_size(*ty)?;
                self.emit_ops(OP_LOAD, &[*dst, *src, size])?;
            }
            Instr::Store { src, dst, ty } => {
                let size = self.type_size(*ty)?;
                let op = OP_STORE_BASE + kind(*src);
                self.emit_ops(op, &[operand_value(*src), *dst, size])?;
            }
            Instr::Addr { dst, place } => {
                self.emit_ops(OP_ADDR, &[*dst, *place])?;
            }
            Instr::AddrField {
                dst,
                place,
                off,
                indirect,
            } => {
                let op = OP_ADDRFIELD_BASE + u8::from(*indirect);
                self.emit_ops(op, &[*dst, *place, *off])?;
            }
            Instr::AddrFunc { dst, func } => {
                self.emit_ops(OP_ADDR_FUNC, &[*dst, *func])?;
            }
            Instr::Field {
                dst,
                place,
                off,
                ty,
                indirect,
            } => {
                let size = self.type_size(*ty)?;
                let op = OP_FIELD_BASE + u8::from(*indirect);
                self.emit_ops(op, &[*dst, *place, *off])?;
                self.words.push(size);
            }
            Instr::StoreField {
                src,
                place,
                off,
                ty,
                indirect,
            } => {
                let size = self.type_size(*ty)?;
                let op = OP_STOREFIELD_BASE + u8::from(*indirect) * 2 + kind(*src);
                self.emit_ops(op, &[operand_value(*src), *place, *off])?;
                self.words.push(size);
            }
            Instr::Arith {
                op,
                dst,
                lhs,
                rhs,
                operand_ty,
                ..
            } => {
                let size = self.type_size(*operand_ty)?;
                let code = arith_opcode(*op, size)?;
                self.emit_ops(code, &[*dst, *lhs, *rhs])?;
            }
            Instr::ArithImm {
                op,
                dst,
                lhs,
                lhs_slot,
                imm,
                operand_ty,
            } => {
                let size = self.type_size(*operand_ty)?;
                let code = arith_opcode(*op, size)?;
                if *dst > NARROW_MAX || *lhs > NARROW_MAX || code >= WIDE_FLAG {
                    return Err(format!(
                        "fused arithmetic operand does not fit the narrow form (dst={dst}, lhs={lhs})"
                    ));
                }
                let flags = if *lhs_slot { code | WIDE_FLAG } else { code };
                self.words.push(
                    u32::from(OP_ARITH_IMM) | (*dst << 8) | (*lhs << 16) | (u32::from(flags) << 24),
                );
                #[allow(clippy::cast_possible_truncation)]
                let lo = (*imm & 0xFFFF_FFFF) as u32;
                #[allow(clippy::cast_possible_truncation)]
                let hi = (*imm >> 32) as u32;
                self.words.push(lo);
                self.words.push(hi);
            }
            Instr::BranchIfArithImm {
                op,
                lhs,
                lhs_slot,
                imm,
                operand_ty,
                then,
                els,
            } => {
                let size = self.type_size(*operand_ty)?;
                let code = arith_opcode(*op, size)?;
                if *lhs > NARROW_MAX || code >= WIDE_FLAG {
                    return Err(format!(
                        "fused branch operand does not fit the narrow form (lhs={lhs})"
                    ));
                }
                let flags = if *lhs_slot { u32::from(WIDE_FLAG) } else { 0 };
                self.words.push(
                    u32::from(OP_BRANCH_IF_IMM)
                        | (*lhs << 8)
                        | (flags << 16)
                        | (u32::from(code) << 24),
                );
                self.words.push(*then);
                self.words.push(*els);
                #[allow(clippy::cast_possible_truncation)]
                let lo = (*imm & 0xFFFF_FFFF) as u32;
                #[allow(clippy::cast_possible_truncation)]
                let hi = (*imm >> 32) as u32;
                self.words.push(lo);
                self.words.push(hi);
            }
            Instr::Convert { op, dst, src, ty } => {
                let size = self.type_size(*ty)?;
                let code = convert_opcode(*op, size)?;
                self.emit_ops(code, &[*dst, *src])?;
            }
            Instr::Call { func, ins, outs } => {
                let index = self.push_call_site(*func, ins, outs)?;
                self.emit_field(OP_CALL, index)?;
            }
            Instr::CallArithImm {
                op,
                lhs,
                lhs_slot,
                imm,
                operand_ty,
                func,
                ins,
                outs,
            } => {
                let size = self.type_size(*operand_ty)?;
                let code = arith_opcode(*op, size)?;
                if *lhs > NARROW_MAX || code >= WIDE_FLAG {
                    return Err(format!(
                        "fused call operand does not fit the narrow form (lhs={lhs})"
                    ));
                }
                let index = self.push_call_site(*func, ins, outs)?;
                let flags = if *lhs_slot { u32::from(WIDE_FLAG) } else { 0 };
                self.words.push(
                    u32::from(OP_CALL_ARITH_IMM)
                        | (*lhs << 8)
                        | (flags << 16)
                        | (u32::from(code) << 24),
                );
                self.words.push(index);
                #[allow(clippy::cast_possible_truncation)]
                let lo = (*imm & 0xFFFF_FFFF) as u32;
                #[allow(clippy::cast_possible_truncation)]
                let hi = (*imm >> 32) as u32;
                self.words.push(lo);
                self.words.push(hi);
            }
            Instr::CallArithOut {
                op,
                lhs,
                lhs_slot,
                imm,
                operand_ty,
                func,
                ins,
                outs,
                dst,
                size,
            } => {
                let width = self.type_size(*operand_ty)?;
                let code = arith_opcode(*op, width)?;
                if *lhs > NARROW_MAX || *dst > NARROW_MAX || *size > NARROW_MAX || code >= WIDE_FLAG
                {
                    return Err(format!(
                        "fused call.arith.out operand does not fit the narrow form (lhs={lhs}, dst={dst})"
                    ));
                }
                let index = self.push_call_site(*func, ins, outs)?;
                let flags = if *lhs_slot { u32::from(WIDE_FLAG) } else { 0 };
                self.words.push(
                    u32::from(OP_CALL_ARITH_OUT)
                        | (*lhs << 8)
                        | (flags << 16)
                        | (u32::from(code) << 24),
                );
                self.words.push(index);
                self.words.push(*dst | (*size << 8));
                #[allow(clippy::cast_possible_truncation)]
                let lo = (*imm & 0xFFFF_FFFF) as u32;
                #[allow(clippy::cast_possible_truncation)]
                let hi = (*imm >> 32) as u32;
                self.words.push(lo);
                self.words.push(hi);
            }
            Instr::CallOut {
                func,
                ins,
                outs,
                dst,
                size,
            } => {
                if *dst > NARROW_MAX || *size > NARROW_MAX {
                    return Err(format!(
                        "fused call.out operand does not fit the narrow form (dst={dst}, size={size})"
                    ));
                }
                let index = self.push_call_site(*func, ins, outs)?;
                self.words
                    .push(u32::from(OP_CALL_OUT) | (*dst << 8) | (*size << 16));
                self.words.push(index);
            }
            Instr::CallIndirect { callee, ins, outs } => {
                let index = self.push_call_site(0, ins, outs)?;
                self.emit_ops(OP_CALL_INDIRECT, &[*callee])?;
                self.words.push(index);
            }
            Instr::CallBuiltin { builtin, ins, outs } => {
                let index = self.push_call_site(0, ins, outs)?;
                self.emit_field(OP_CALL_BUILTIN_BASE + builtin_index(*builtin), index)?;
            }
            Instr::Branch { target } => self.emit_field(OP_BRANCH, *target)?,
            Instr::BranchIf { cond, then, els } => {
                self.emit_ops(OP_BRANCH_IF, &[*cond, *then, *els])?;
            }
            Instr::Return => self.words.push(u32::from(OP_RETURN)),
            Instr::Panic { data } => self.emit_field(OP_PANIC, *data)?,
            Instr::Unreachable => self.words.push(u32::from(OP_UNREACHABLE)),
        }
        Ok(())
    }

    /// Returns the byte size of `ty`, validating its id.
    fn type_size(&self, ty: u32) -> Result<u32, String> {
        self.types
            .get(ty as usize)
            .map(|info| info.size)
            .ok_or_else(|| format!("type id {ty} out of range"))
    }

    /// Emits a word 0 carrying up to three operands, using the wide escape when
    /// any operand needs more than a byte.
    fn emit_ops(&mut self, op: u8, ops: &[u32]) -> Result<(), String> {
        if ops.iter().any(|&v| v > NARROW_MAX) {
            let a = ops.first().copied().unwrap_or(0);
            let b = ops.get(1).copied().unwrap_or(0);
            if a > u32::from(u16::MAX) || b > u32::from(u16::MAX) {
                return Err(format!(
                    "wide operand does not fit in the a/b u16 field (a={a}, b={b})"
                ));
            }
            self.words.push(u32::from(op | WIDE_FLAG));
            self.words.push((a & 0xFFFF) | ((b & 0xFFFF) << 16));
            if ops.len() >= 3 {
                self.words.push(ops.get(2).copied().unwrap_or(0));
            }
        } else {
            let a = ops.first().copied().unwrap_or(0) & NARROW_MAX;
            let b = ops.get(1).copied().unwrap_or(0) & NARROW_MAX;
            let c = ops.get(2).copied().unwrap_or(0) & NARROW_MAX;
            self.words
                .push(u32::from(op) | (a << 8) | (b << 16) | (c << 24));
        }
        Ok(())
    }

    /// Emits a word 0 carrying a single 24-bit field in `w >> 8`.
    fn emit_field(&mut self, op: u8, field: u32) -> Result<(), String> {
        if field > FIELD24_MAX {
            return Err(format!("immediate {field} does not fit in a 24-bit field"));
        }
        self.words.push(u32::from(op) | (field << 8));
        Ok(())
    }

    /// Appends a call's operands and resolved call site, returning its index.
    fn push_call_site(&mut self, func: u32, ins: &[Operand], outs: &[u32]) -> Result<u32, String> {
        let ins_start = to_u32(self.operands.len(), "operand pool")?;
        self.operands.extend_from_slice(ins);
        let ins_end = to_u32(self.operands.len(), "operand pool")?;
        let outs_start = ins_end;
        self.operands
            .extend(outs.iter().map(|&off| Operand::Slot(off)));
        let outs_end = to_u32(self.operands.len(), "operand pool")?;
        let index = to_u32(self.call_sites.len(), "call-site pool")?;
        if index > FIELD24_MAX {
            return Err(format!("call-site index {index} exceeds 24 bits"));
        }
        self.call_sites.push(CallSite {
            func,
            ins: ins_start..ins_end,
            outs: outs_start..outs_end,
        });
        Ok(index)
    }
}

/// Encodes a whole typed instruction list whose branch targets are
/// **instruction indices**, rewriting them to word offsets.
///
/// The migration to the word stream makes `pc` a word index, but the compiler,
/// the `.lbc` parser, and the tests all describe branches by instruction index.
/// This helper computes each instruction's word offset (a fixpoint, because a
/// `BranchIf` target above 255 turns the instruction into the wide form) and
/// returns the encoded [`CodeBuilder`] plus the `instruction index -> word pc`
/// table (length `instrs.len() + 1`).
pub(crate) fn encode_instrs<'a>(
    types: &'a [TypeInfo],
    instrs: &[Instr],
) -> Result<(CodeBuilder<'a>, Vec<u32>), String> {
    // Start with a one-word-per-instruction guess; the loop grows offsets until
    // the encoded widths stop changing (monotone, so it terminates).
    let mut offsets: Vec<u32> = (0..=instrs.len())
        .map(|index| u32::try_from(index).unwrap_or(u32::MAX))
        .collect();
    loop {
        let mut probe = CodeBuilder::new(types);
        let mut starts = Vec::with_capacity(instrs.len());
        for instr in instrs {
            starts.push(to_u32(probe.words.len(), "word stream")?);
            let adjusted = rewrite_targets(instr, &offsets)?;
            probe.push(&adjusted)?;
        }
        starts.push(to_u32(probe.words.len(), "word stream")?);
        if starts == offsets {
            return Ok((probe, offsets));
        }
        offsets = starts;
    }
}

/// Rewrites a typed instruction's branch targets from instruction indices to
/// word offsets using `offsets`.
fn rewrite_targets(instr: &Instr, offsets: &[u32]) -> Result<Instr, String> {
    let mut out = instr.clone();
    match &mut out {
        Instr::Branch { target } => {
            *target = word_offset(offsets, *target)?;
        }
        Instr::BranchIf { then, els, .. } | Instr::BranchIfArithImm { then, els, .. } => {
            *then = word_offset(offsets, *then)?;
            *els = word_offset(offsets, *els)?;
        }
        _ => {}
    }
    Ok(out)
}

/// Maps an instruction index to its word offset.
fn word_offset(offsets: &[u32], index: u32) -> Result<u32, String> {
    offsets
        .get(index as usize)
        .copied()
        .ok_or_else(|| format!("branch target instruction {index} out of range"))
}

/// The `u64` mask for a value of `size` bytes (saturating at 8).
const fn mask_for_size(size: u32) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1_u64 << (size * 8)) - 1
    }
}

/// Converts a `usize` pool length to a `u32`, naming the pool in any error.
fn to_u32(value: usize, what: &str) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| format!("{what} length {value} exceeds u32"))
}

/// The raw payload of an [`Operand`] (register index or slot offset).
const fn operand_value(operand: Operand) -> u32 {
    match operand {
        Operand::Reg(v) | Operand::Slot(v) => v,
    }
}

/// `0` for a register operand, `1` for a slot operand.
const fn kind(operand: Operand) -> u8 {
    match operand {
        Operand::Reg(_) => 0,
        Operand::Slot(_) => 1,
    }
}

/// Maps an arithmetic operation and operand byte size to its opcode.
///
/// Width-free operations ignore `size`.
pub(crate) fn arith_opcode(op: ArithOp, size: u32) -> Result<u8, String> {
    let (base, per) = match op {
        ArithOp::IAdd => (OP_IADD_BASE, 4),
        ArithOp::ISub => (OP_ISUB_BASE, 4),
        ArithOp::IMul => (OP_IMUL_BASE, 4),
        ArithOp::IDiv => (OP_IDIV_BASE, 4),
        ArithOp::IMod => (OP_IMOD_BASE, 4),
        ArithOp::ILt => (OP_ILT_BASE, 4),
        ArithOp::IGt => (OP_IGT_BASE, 4),
        ArithOp::IDivU => (OP_IDIVU_BASE, 4),
        ArithOp::IModU => (OP_IMODU_BASE, 4),
        ArithOp::Eq => (OP_EQ_BASE, 4),
        ArithOp::ILtU => (OP_ILTU_BASE, 4),
        ArithOp::IGtU => (OP_IGTU_BASE, 4),
        ArithOp::FAdd => (OP_FADD_BASE, 2),
        ArithOp::FSub => (OP_FSUB_BASE, 2),
        ArithOp::FMul => (OP_FMUL_BASE, 2),
        ArithOp::FDiv => (OP_FDIV_BASE, 2),
        ArithOp::FMod => (OP_FMOD_BASE, 2),
        ArithOp::FLt => (OP_FLT_BASE, 2),
        ArithOp::FGt => (OP_FGT_BASE, 2),
    };
    let idx = width_index(size, per).ok_or_else(|| {
        format!(
            "operand size {size} has no {}-wide class for {op:?}",
            if per == 4 { "int" } else { "float" }
        )
    })?;
    Ok(base + idx)
}

/// Maps a conversion and result byte size to its opcode.
pub(crate) fn convert_opcode(op: ConvertOp, size: u32) -> Result<u8, String> {
    match op {
        ConvertOp::Trunc => {
            let idx = width_index(size, 4)
                .ok_or_else(|| format!("trunc result size {size} has no width class"))?;
            Ok(OP_TRUNC_BASE + idx)
        }
        ConvertOp::ExtendZero => Ok(OP_EXTEND_ZERO),
    }
}

/// The width-class index for a byte size: `0..per`. Integer classes are
/// `1/2/4/8` bytes; float classes are `4/8` bytes.
const fn width_index(size: u32, per: u8) -> Option<u8> {
    match (size, per) {
        (1, 4) | (4, 2) => Some(0),
        (2, 4) | (8, 2) => Some(1),
        (4, 4) => Some(2),
        (8, 4) => Some(3),
        _ => None,
    }
}

/// Decodes a width-specialized or width-free arithmetic opcode.
///
/// Returns `(op, width_bits)`; `width_bits` is `0` for width-free operations.
pub(crate) const fn arith_from_opcode(base: u8) -> Option<(ArithOp, u8)> {
    let (op, idx, per) = match base {
        OP_IADD_BASE..=0x5B => {
            let group = (base - OP_IADD_BASE) / 4;
            let op = match group {
                0 => ArithOp::IAdd,
                1 => ArithOp::ISub,
                2 => ArithOp::IMul,
                3 => ArithOp::IDiv,
                4 => ArithOp::IMod,
                5 => ArithOp::ILt,
                _ => ArithOp::IGt,
            };
            (op, (base - OP_IADD_BASE) % 4, 4)
        }
        OP_IDIVU_BASE..=0x23 => (ArithOp::IDivU, base - OP_IDIVU_BASE, 4),
        OP_IMODU_BASE..=0x27 => (ArithOp::IModU, base - OP_IMODU_BASE, 4),
        OP_EQ_BASE..=0x2B => (ArithOp::Eq, base - OP_EQ_BASE, 4),
        OP_ILTU_BASE..=0x2F => (ArithOp::ILtU, base - OP_ILTU_BASE, 4),
        OP_IGTU_BASE..=0x33 => (ArithOp::IGtU, base - OP_IGTU_BASE, 4),
        OP_FADD_BASE..=0x6D => {
            let group = (base - OP_FADD_BASE) / 2;
            let op = match group {
                0 => ArithOp::FAdd,
                1 => ArithOp::FSub,
                2 => ArithOp::FMul,
                3 => ArithOp::FDiv,
                4 => ArithOp::FMod,
                5 => ArithOp::FLt,
                _ => ArithOp::FGt,
            };
            (op, (base - OP_FADD_BASE) % 2, 2)
        }
        _ => return None,
    };
    let width = if per == 4 {
        match idx {
            0 => 8,
            1 => 16,
            2 => 32,
            _ => 64,
        }
    } else {
        match idx {
            0 => 32,
            _ => 64,
        }
    };
    Some((op, width))
}

/// Decodes a width-specialized or width-free convert opcode.
pub(crate) const fn convert_from_opcode(base: u8) -> Option<(ConvertOp, u8)> {
    match base {
        OP_TRUNC_BASE..=0x5F => {
            let width = match base - OP_TRUNC_BASE {
                0 => 8,
                1 => 16,
                2 => 32,
                _ => 64,
            };
            Some((ConvertOp::Trunc, width))
        }
        OP_EXTEND_ZERO => Some((ConvertOp::ExtendZero, 0)),
        _ => None,
    }
}

/// Maps a [`Builtin`] to its offset from [`OP_CALL_BUILTIN_BASE`].
pub(crate) const fn builtin_index(builtin: Builtin) -> u8 {
    match builtin {
        Builtin::PtrAdd => 0,
        Builtin::Memcpy => 1,
        Builtin::Abort => 2,
    }
}

/// Inverts [`builtin_index`].
pub(crate) const fn builtin_from_index(index: u8) -> Option<Builtin> {
    Some(match index {
        0 => Builtin::PtrAdd,
        1 => Builtin::Memcpy,
        2 => Builtin::Abort,
        _ => return None,
    })
}

/// Encodes a mnemonic for a raw opcode byte (the wide flag is ignored).
#[must_use]
pub fn opcode_mnemonic(op: u8) -> &'static str {
    let base = op & !WIDE_FLAG;
    if let Some((arith, _)) = arith_from_opcode(base) {
        return arith.as_str();
    }
    if let Some((convert, _)) = convert_from_opcode(base) {
        return convert.as_str();
    }
    match base {
        OP_RETURN => "return",
        OP_UNREACHABLE => "unreachable",
        OP_PANIC => "panic",
        OP_BRANCH => "branch",
        OP_BRANCH_IF => "branch.if",
        OP_CONST => "const",
        OP_CONST_DATA => "const.data",
        OP_COPY_BASE => "copy.reg.reg",
        0x09 => "copy.reg.slot",
        0x0A => "copy.slot.reg",
        OP_COPY_LAST => "copy.slot.slot",
        OP_COPY_BYTES => "copy.bytes",
        OP_LOAD => "load",
        OP_STORE_BASE => "store.reg",
        OP_STORE_LAST => "store.slot",
        OP_ADDR => "addr",
        OP_ADDRFIELD_BASE => "addr.field",
        OP_ADDRFIELD_LAST => "addr.field.indirect",
        OP_ADDR_FUNC => "addr.func",
        OP_FIELD_BASE => "field",
        OP_FIELD_LAST => "field.indirect",
        OP_STOREFIELD_BASE => "store.field.reg",
        0x17 => "store.field.slot",
        0x18 => "store.field.reg.indirect",
        OP_STOREFIELD_LAST => "store.field.slot.indirect",
        OP_CALL => "call",
        OP_CALL_INDIRECT => "call.indirect",
        OP_CALL_BUILTIN_BASE => "builtin.ptr_add",
        0x1D => "builtin.memcpy",
        OP_CALL_BUILTIN_LAST => "builtin.lilac_abort",
        OP_ARITH_IMM => "arith.imm",
        OP_BRANCH_IF_IMM => "branch.if.imm",
        OP_CALL_ARITH_IMM => "call.arith.imm",
        OP_CALL_OUT => "call.out",
        OP_CALL_ARITH_OUT => "call.arith.out",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::{mask_for_size, opcode_mnemonic, CodeBuilder, OP_IADD_BASE};
    use crate::decode::{decode, Decoded};
    use crate::isa::{ArithOp, Builtin, ConvertOp, Instr, Operand, TypeInfo};

    /// Type ids used by the samples: 0/1/2/3 are `#bit8/16/32/64`, 4/5 are
    /// `#f32/f64`, and 6 is a pointer-sized ref.
    fn test_types() -> Vec<TypeInfo> {
        let mut types = Vec::new();
        for (name, size) in [
            ("#bit8", 1),
            ("#bit16", 2),
            ("#bit32", 4),
            ("#bit64", 8),
            ("#f32", 4),
            ("#f64", 8),
        ] {
            types.push(TypeInfo {
                name: name.to_string(),
                size,
                is_ref: false,
            });
        }
        types.push(TypeInfo {
            name: "ref #Thing".to_string(),
            size: 8,
            is_ref: true,
        });
        types
    }

    fn size_of(types: &[TypeInfo], ty: u32) -> u32 {
        types[ty as usize].size
    }

    /// The bit width class for a byte size (`0` when unknown/width-free).
    fn width_of(size: u32) -> u8 {
        match size {
            1 => 8,
            2 => 16,
            4 => 32,
            8 => 64,
            _ => 0,
        }
    }

    fn outs_match(instr_outs: &[u32], decoded_outs: &[Operand]) -> bool {
        instr_outs.len() == decoded_outs.len()
            && instr_outs
                .iter()
                .zip(decoded_outs)
                .all(|(out, decoded)| *decoded == Operand::Slot(*out))
    }

    /// Compares a typed instruction with its decoded view: op, operands, and
    /// precomputed size/width (the typed side keeps `TypeId`s).
    #[allow(clippy::too_many_lines)]
    fn equivalent(types: &[TypeInfo], instr: &Instr, d: &Decoded<'_>) -> bool {
        match (instr, d) {
            (Instr::Const { dst, imm, ty }, Decoded::Const { dst: dd, imm: di }) => {
                dst == dd && *imm & mask_for_size(size_of(types, *ty)) == *di
            }
            (Instr::ConstData { dst, data, .. }, Decoded::ConstData { dst: dd, data: dd2 }) => {
                dst == dd && data == dd2
            }
            (
                Instr::Copy { dst, src, ty },
                Decoded::Copy {
                    dst: dd,
                    src: ds,
                    size,
                },
            ) => dst == dd && src == ds && size_of(types, *ty) == *size,
            (
                Instr::CopyBytes { dst, src, size },
                Decoded::CopyBytes {
                    dst: dd,
                    src: ds,
                    size: dsize,
                },
            ) => dst == dd && src == ds && size == dsize,
            (
                Instr::Load { dst, src, ty },
                Decoded::Load {
                    dst: dd,
                    src: ds,
                    size,
                },
            ) => dst == dd && src == ds && size_of(types, *ty) == *size,
            (
                Instr::Store { src, dst, ty },
                Decoded::Store {
                    src: ds,
                    dst: dd,
                    size,
                },
            ) => src == ds && dst == dd && size_of(types, *ty) == *size,
            (Instr::Addr { dst, place }, Decoded::Addr { dst: dd, place: dp }) => {
                dst == dd && place == dp
            }
            (
                Instr::AddrField {
                    dst,
                    place,
                    off,
                    indirect,
                },
                Decoded::AddrField {
                    dst: dd,
                    place: dp,
                    off: doff,
                    indirect: di,
                },
            ) => dst == dd && place == dp && off == doff && indirect == di,
            (Instr::AddrFunc { dst, func }, Decoded::AddrFunc { dst: dd, func: df }) => {
                dst == dd && func == df
            }
            (
                Instr::Field {
                    dst,
                    place,
                    off,
                    ty,
                    indirect,
                },
                Decoded::Field {
                    dst: dd,
                    place: dp,
                    off: doff,
                    size,
                    indirect: di,
                },
            ) => {
                dst == dd
                    && place == dp
                    && off == doff
                    && size_of(types, *ty) == *size
                    && indirect == di
            }
            (
                Instr::StoreField {
                    src,
                    place,
                    off,
                    ty,
                    indirect,
                },
                Decoded::StoreField {
                    src: ds,
                    place: dp,
                    off: doff,
                    size,
                    indirect: di,
                },
            ) => {
                src == ds
                    && place == dp
                    && off == doff
                    && size_of(types, *ty) == *size
                    && indirect == di
            }
            (
                Instr::Arith {
                    op,
                    dst,
                    lhs,
                    rhs,
                    operand_ty,
                    ..
                },
                _,
            ) => arith_equivalent(
                *op,
                width_of(size_of(types, *operand_ty)),
                *dst,
                *lhs,
                *rhs,
                d,
            ),
            (Instr::Convert { op, dst, src, ty }, _) => {
                convert_equivalent(*op, size_of(types, *ty), *dst, *src, d)
            }
            (
                Instr::ArithImm {
                    op,
                    dst,
                    lhs,
                    lhs_slot,
                    imm,
                    operand_ty,
                },
                Decoded::ArithImm {
                    dst: dd,
                    lhs: dl,
                    lhs_slot: ds,
                    op: dop,
                    width,
                    imm: di,
                },
            ) => {
                op == dop
                    && dst == dd
                    && lhs == dl
                    && lhs_slot == ds
                    && imm == di
                    && width_of(size_of(types, *operand_ty)) == *width
            }
            (
                Instr::Call { func, ins, outs },
                Decoded::Call {
                    site,
                    ins: di,
                    outs: douts,
                },
            ) => *func == site.func && ins.as_slice() == *di && outs_match(outs, douts),
            (
                Instr::CallArithImm {
                    op,
                    lhs,
                    lhs_slot,
                    imm,
                    operand_ty,
                    func,
                    ins,
                    outs,
                },
                Decoded::CallArithImm {
                    lhs: dl,
                    lhs_slot: ds,
                    op: dop,
                    width,
                    imm: di,
                    site,
                    ins: din,
                    outs: douts,
                },
            ) => {
                op == dop
                    && lhs == dl
                    && lhs_slot == ds
                    && imm == di
                    && width_of(size_of(types, *operand_ty)) == *width
                    && *func == site.func
                    && ins.as_slice() == *din
                    && outs_match(outs, douts)
            }
            (
                Instr::CallOut {
                    func,
                    ins,
                    outs,
                    dst,
                    size,
                },
                Decoded::CallOut {
                    dst: dd,
                    size: ds,
                    site,
                    ins: din,
                    outs: douts,
                },
            ) => {
                *func == site.func
                    && ins.as_slice() == *din
                    && outs_match(outs, douts)
                    && dst == dd
                    && size == ds
            }
            (
                Instr::CallArithOut {
                    op,
                    lhs,
                    lhs_slot,
                    imm,
                    operand_ty,
                    func,
                    ins,
                    outs,
                    dst,
                    size,
                },
                Decoded::CallArithOut {
                    lhs: dl,
                    lhs_slot: ds,
                    op: dop,
                    width,
                    imm: di,
                    site,
                    ins: din,
                    outs: douts,
                    dst: dd,
                    size: dz,
                },
            ) => {
                op == dop
                    && lhs == dl
                    && lhs_slot == ds
                    && imm == di
                    && width_of(size_of(types, *operand_ty)) == *width
                    && *func == site.func
                    && ins.as_slice() == *din
                    && outs_match(outs, douts)
                    && dst == dd
                    && size == dz
            }
            (
                Instr::BranchIfArithImm {
                    op,
                    lhs,
                    lhs_slot,
                    imm,
                    operand_ty,
                    then,
                    els,
                },
                Decoded::BranchIfArithImm {
                    lhs: dl,
                    lhs_slot: ds,
                    op: dop,
                    width,
                    imm: di,
                    then: dt,
                    els: de,
                },
            ) => {
                op == dop
                    && lhs == dl
                    && lhs_slot == ds
                    && imm == di
                    && width_of(size_of(types, *operand_ty)) == *width
                    && then == dt
                    && els == de
            }
            (
                Instr::CallIndirect { callee, ins, outs },
                Decoded::CallIndirect {
                    callee: dc,
                    ins: di,
                    outs: douts,
                    ..
                },
            ) => callee == dc && ins.as_slice() == *di && outs_match(outs, douts),
            (
                Instr::CallBuiltin { builtin, ins, outs },
                Decoded::CallBuiltin {
                    builtin: db,
                    ins: di,
                    outs: douts,
                    ..
                },
            ) => builtin == db && ins.as_slice() == *di && outs_match(outs, douts),
            (Instr::Branch { target }, Decoded::Branch { target: dt }) => target == dt,
            (
                Instr::BranchIf { cond, then, els },
                Decoded::BranchIf {
                    cond: dc,
                    then: dt,
                    els: de,
                },
            ) => cond == dc && then == dt && els == de,
            (Instr::Return, Decoded::Return) | (Instr::Unreachable, Decoded::Unreachable) => true,
            (Instr::Panic { data }, Decoded::Panic { data: dd }) => data == dd,
            _ => false,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn arith_equivalent(
        op: ArithOp,
        width: u8,
        dst: u32,
        lhs: u32,
        rhs: u32,
        d: &Decoded<'_>,
    ) -> bool {
        use ArithOp::{
            Eq, FAdd, FDiv, FGt, FLt, FMod, FMul, FSub, IAdd, IDiv, IDivU, IGt, IGtU, ILt, ILtU,
            IMod, IModU, IMul, ISub,
        };
        match d {
            Decoded::IAdd {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == IAdd && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::ISub {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == ISub && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::IMul {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == IMul && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::IDiv {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == IDiv && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::IMod {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == IMod && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::ILt {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == ILt && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::IGt {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == IGt && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::FAdd {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == FAdd && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::FSub {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == FSub && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::FMul {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == FMul && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::FDiv {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == FDiv && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::FMod {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == FMod && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::FLt {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == FLt && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::FGt {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == FGt && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::IDivU {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == IDivU && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::IModU {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == IModU && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::Eq {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == Eq && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::ILtU {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == ILtU && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            Decoded::IGtU {
                dst: dd,
                lhs: dl,
                rhs: dr,
                width: dw,
            } => op == IGtU && dst == *dd && lhs == *dl && rhs == *dr && width == *dw,
            _ => false,
        }
    }

    fn convert_equivalent(op: ConvertOp, size: u32, dst: u32, src: u32, d: &Decoded<'_>) -> bool {
        match d {
            Decoded::Trunc {
                dst: dd,
                src: ds,
                width,
            } => op == ConvertOp::Trunc && dst == *dd && src == *ds && width_of(size) == *width,
            Decoded::ExtendZero { dst: dd, src: ds } => {
                op == ConvertOp::ExtendZero && dst == *dd && src == *ds
            }
            _ => false,
        }
    }

    fn round_trip(types: &[TypeInfo], instr: &Instr) {
        let mut b = CodeBuilder::new(types);
        b.push(instr).expect("encode");
        let (decoded, next) = decode(&b.words, 0, &b.call_sites, &b.operands).expect("decode");
        assert_eq!(next as usize, b.words.len(), "next pc mismatch: {instr:?}");
        assert!(
            equivalent(types, instr, &decoded),
            "round trip mismatch: {instr:?} -> {decoded:?}"
        );
    }

    /// One instance of every `Instr` variant, including all operand-kind,
    /// indirect, arith-op/width, convert-op, and builtin sub-variants.
    #[allow(clippy::too_many_lines)]
    fn sample_instrs() -> Vec<Instr> {
        let mut instrs = vec![
            Instr::Const {
                dst: 3,
                imm: 0x1122_3344_5566_7788,
                ty: 0,
            },
            Instr::ConstData {
                dst: 4,
                data: 7,
                ty: 6,
            },
            Instr::Copy {
                dst: Operand::Reg(1),
                src: Operand::Reg(2),
                ty: 0,
            },
            Instr::Copy {
                dst: Operand::Slot(8),
                src: Operand::Reg(2),
                ty: 0,
            },
            Instr::Copy {
                dst: Operand::Reg(1),
                src: Operand::Slot(8),
                ty: 0,
            },
            Instr::Copy {
                dst: Operand::Slot(8),
                src: Operand::Slot(16),
                ty: 0,
            },
            Instr::CopyBytes {
                dst: 8,
                src: 16,
                size: 32,
            },
            Instr::Load {
                dst: 1,
                src: 8,
                ty: 0,
            },
            Instr::Store {
                src: Operand::Reg(1),
                dst: 8,
                ty: 0,
            },
            Instr::Store {
                src: Operand::Slot(8),
                dst: 16,
                ty: 0,
            },
            Instr::Addr { dst: 1, place: 8 },
            Instr::AddrField {
                dst: 1,
                place: 8,
                off: 12,
                indirect: false,
            },
            Instr::AddrField {
                dst: 1,
                place: 8,
                off: 12,
                indirect: true,
            },
            Instr::AddrFunc { dst: 1, func: 9 },
            Instr::Field {
                dst: 1,
                place: 8,
                off: 12,
                ty: 0,
                indirect: false,
            },
            Instr::Field {
                dst: 1,
                place: 8,
                off: 12,
                ty: 0,
                indirect: true,
            },
            Instr::StoreField {
                src: Operand::Reg(1),
                place: 8,
                off: 12,
                ty: 0,
                indirect: false,
            },
            Instr::StoreField {
                src: Operand::Slot(16),
                place: 8,
                off: 12,
                ty: 0,
                indirect: false,
            },
            Instr::StoreField {
                src: Operand::Reg(1),
                place: 8,
                off: 12,
                ty: 0,
                indirect: true,
            },
            Instr::StoreField {
                src: Operand::Slot(16),
                place: 8,
                off: 12,
                ty: 0,
                indirect: true,
            },
            Instr::Call {
                func: 3,
                ins: vec![Operand::Reg(0), Operand::Slot(8)],
                outs: vec![16, 24],
            },
            Instr::CallIndirect {
                callee: 5,
                ins: vec![Operand::Reg(0)],
                outs: vec![8],
            },
            Instr::CallBuiltin {
                builtin: Builtin::PtrAdd,
                ins: vec![Operand::Reg(0), Operand::Reg(1)],
                outs: vec![16],
            },
            Instr::CallBuiltin {
                builtin: Builtin::Memcpy,
                ins: vec![Operand::Reg(0), Operand::Reg(1), Operand::Reg(2)],
                outs: Vec::new(),
            },
            Instr::CallBuiltin {
                builtin: Builtin::Abort,
                ins: Vec::new(),
                outs: Vec::new(),
            },
            Instr::Branch { target: 42 },
            Instr::BranchIf {
                cond: 1,
                then: 2,
                els: 3,
            },
            Instr::Return,
            Instr::Panic { data: 6 },
            Instr::Unreachable,
        ];
        // Width-specialized integer ops at each integer width.
        for op in [
            ArithOp::IAdd,
            ArithOp::ISub,
            ArithOp::IMul,
            ArithOp::IDiv,
            ArithOp::IMod,
            ArithOp::ILt,
            ArithOp::IGt,
        ] {
            for ty in [0_u32, 1, 2, 3] {
                instrs.push(Instr::Arith {
                    op,
                    dst: 1,
                    lhs: 2,
                    rhs: 3,
                    operand_ty: ty,
                    ty,
                });
            }
        }
        // Float ops at each float width.
        for op in [
            ArithOp::FAdd,
            ArithOp::FSub,
            ArithOp::FMul,
            ArithOp::FDiv,
            ArithOp::FMod,
            ArithOp::FLt,
            ArithOp::FGt,
        ] {
            for ty in [4_u32, 5] {
                instrs.push(Instr::Arith {
                    op,
                    dst: 1,
                    lhs: 2,
                    rhs: 3,
                    operand_ty: ty,
                    ty,
                });
            }
        }
        // Width-free ops.
        for op in [
            ArithOp::IDivU,
            ArithOp::IModU,
            ArithOp::Eq,
            ArithOp::ILtU,
            ArithOp::IGtU,
        ] {
            instrs.push(Instr::Arith {
                op,
                dst: 1,
                lhs: 2,
                rhs: 3,
                operand_ty: 2,
                ty: 2,
            });
        }
        // Trunc at each width, and the single width-free ExtendZero.
        for ty in [0_u32, 1, 2, 3] {
            instrs.push(Instr::Convert {
                op: ConvertOp::Trunc,
                dst: 1,
                src: 2,
                ty,
            });
        }
        instrs.push(Instr::Convert {
            op: ConvertOp::ExtendZero,
            dst: 1,
            src: 2,
            ty: 0,
        });
        // Fused arithmetic with a register and a slot left operand.
        instrs.push(Instr::ArithImm {
            op: ArithOp::IAdd,
            dst: 1,
            lhs: 2,
            lhs_slot: false,
            imm: 0x10,
            operand_ty: 2,
        });
        instrs.push(Instr::ArithImm {
            op: ArithOp::ISub,
            dst: 1,
            lhs: 8,
            lhs_slot: true,
            imm: 0x1,
            operand_ty: 3,
        });
        // A signed comparison (the `fib` hot case) at 64-bit width.
        instrs.push(Instr::ArithImm {
            op: ArithOp::ILt,
            dst: 4,
            lhs: 16,
            lhs_slot: true,
            imm: 0x2,
            operand_ty: 3,
        });
        // Fused compare + branch.
        instrs.push(Instr::BranchIfArithImm {
            op: ArithOp::ILt,
            lhs: 8,
            lhs_slot: true,
            imm: 0x2,
            operand_ty: 3,
            then: 4,
            els: 5,
        });
        // Fused arith + direct call, and call with output copy.
        instrs.push(Instr::CallArithImm {
            op: ArithOp::ISub,
            lhs: 8,
            lhs_slot: true,
            imm: 0x1,
            operand_ty: 3,
            func: 3,
            ins: vec![Operand::Reg(1)],
            outs: vec![16],
        });
        instrs.push(Instr::CallOut {
            func: 3,
            ins: vec![Operand::Reg(0)],
            outs: vec![16],
            dst: 1,
            size: 8,
        });
        instrs.push(Instr::CallArithOut {
            op: ArithOp::ISub,
            lhs: 8,
            lhs_slot: true,
            imm: 0x1,
            operand_ty: 3,
            func: 3,
            ins: vec![Operand::Reg(1)],
            outs: vec![16],
            dst: 2,
            size: 8,
        });
        instrs
    }

    #[test]
    fn every_variant_round_trips() {
        let types = test_types();
        for instr in sample_instrs() {
            round_trip(&types, &instr);
        }
    }

    #[test]
    fn wide_operand_round_trips() {
        let types = test_types();
        round_trip(
            &types,
            &Instr::Copy {
                dst: Operand::Slot(300),
                src: Operand::Reg(511),
                ty: 0,
            },
        );
    }

    #[test]
    fn third_wide_operand_round_trips() {
        let types = test_types();
        round_trip(
            &types,
            &Instr::Field {
                dst: 700,
                place: 1000,
                off: 70_000,
                ty: 3,
                indirect: true,
            },
        );
    }

    #[test]
    fn arith_is_one_word() {
        let types = test_types();
        let mut b = CodeBuilder::new(&types);
        b.push(&Instr::Arith {
            op: ArithOp::IAdd,
            dst: 1,
            lhs: 2,
            rhs: 3,
            operand_ty: 2,
            ty: 2,
        })
        .expect("encode");
        assert_eq!(b.words.len(), 1);
        assert_eq!(b.words[0] & 0xFF, u32::from(OP_IADD_BASE + 2));
        assert_eq!(b.words[0] >> 8 & 0xFF, 1);
        assert_eq!(b.words[0] >> 16 & 0xFF, 2);
        assert_eq!(b.words[0] >> 24 & 0xFF, 3);
    }

    #[test]
    fn convert_is_one_word() {
        let types = test_types();
        let mut b = CodeBuilder::new(&types);
        b.push(&Instr::Convert {
            op: ConvertOp::Trunc,
            dst: 1,
            src: 2,
            ty: 3,
        })
        .expect("encode");
        assert_eq!(b.words.len(), 1);
        assert_eq!(b.words[0] & 0xFF, u32::from(super::OP_TRUNC_BASE + 3));
        assert_eq!(b.words[0] >> 8 & 0xFF, 1);
        assert_eq!(b.words[0] >> 16 & 0xFF, 2);
    }

    #[test]
    fn call_appends_to_pools() {
        let types = test_types();
        let instr = Instr::Call {
            func: 3,
            ins: vec![Operand::Reg(0), Operand::Slot(8)],
            outs: vec![16, 24],
        };
        let mut b = CodeBuilder::new(&types);
        b.push(&instr).expect("encode");
        assert_eq!(b.words.len(), 1);
        assert_eq!(b.call_sites.len(), 1);
        assert_eq!(b.operands.len(), 4);
        assert_eq!(b.call_sites[0].func, 3);
        assert_eq!(b.call_sites[0].ins, 0..2);
        assert_eq!(b.call_sites[0].outs, 2..4);
        round_trip(&types, &instr);
    }

    #[test]
    fn const_masks_immediate_to_type_width() {
        let types = test_types();
        let mut b = CodeBuilder::new(&types);
        b.push(&Instr::Const {
            dst: 1,
            imm: 0x1122_3344_5566_7788,
            ty: 0,
        })
        .expect("encode");
        // word 0, imm low, imm high
        assert_eq!(b.words.len(), 3);
        assert_eq!(b.words[1], 0x88);
        assert_eq!(b.words[2], 0);
    }

    #[test]
    fn wide_opcode_mnemonic_strips_flag() {
        assert_eq!(opcode_mnemonic(OP_IADD_BASE), "i.add");
        assert_eq!(opcode_mnemonic(OP_IADD_BASE | 0x80), "i.add");
        assert_eq!(opcode_mnemonic(0x7F), "unknown");
    }

    #[test]
    fn rejects_unfittable_operand() {
        let types = test_types();
        // `src` lands in the `b` slot of `Copy`, which the wide form stores
        // as a `u16`; a slot offset above that cannot be encoded.
        let instr = Instr::Copy {
            dst: Operand::Slot(70_000),
            src: Operand::Slot(70_000),
            ty: 0,
        };
        let mut b = CodeBuilder::new(&types);
        assert!(b.push(&instr).is_err());
    }

    #[test]
    fn rejects_bad_type_id() {
        let types = test_types();
        let mut b = CodeBuilder::new(&types);
        assert!(b
            .push(&Instr::Load {
                dst: 1,
                src: 2,
                ty: 99,
            })
            .is_err());
    }
}
