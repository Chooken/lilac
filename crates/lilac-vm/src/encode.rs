//! Compact `u32` encoder for the bytecode ISA.
//!
//! [`CodeBuilder`] lowers typed [`crate::isa::Instr`] values to the executed
//! word stream. The layout is documented in `docs/superpowers/specs/
//! 2026-10-05-lilac-vm-m5c1-performance-design.md` §3: word 0 is
//! `[op:8 | a:8 | b:8 | c:8]`, an operand `>= 256` escapes to the wide form
//! (bit 7 of the opcode set, operands moved to extension words), immediate
//! forms carry a 24-bit field in `w >> 8`, and calls append a [`CallSite`] to
//! a pool addressed by a 24-bit index.

use crate::isa::{ArithOp, Builtin, CallSite, ConvertOp, Instr, Operand};

/// Bit 7 of the opcode byte; set when an instruction uses the wide form.
pub const WIDE_FLAG: u8 = 0x80;

/// Highest value that fits in a 24-bit field (`w >> 8`).
pub const FIELD24_MAX: u32 = 0x00FF_FFFF;

/// Highest value that fits in a narrow (one-byte) operand.
pub const NARROW_MAX: u32 = 0xFF;

// Core opcodes (base opcodes are all `< 128` so bit 7 is free).
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
/// `Const` (two immediate words follow).
pub const OP_CONST: u8 = 0x06;
/// `ConstData`.
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
/// First `Arith` opcode (`IAdd`); one per [`ArithOp`].
pub const OP_ARITH_BASE: u8 = 0x20;
/// Last `Arith` opcode (`FGt`).
pub const OP_ARITH_LAST: u8 = 0x32;
/// First `Convert` opcode (`Trunc`); one per [`ConvertOp`].
pub const OP_CONVERT_BASE: u8 = 0x33;
/// Last `Convert` opcode (`ExtendZero`).
pub const OP_CONVERT_LAST: u8 = 0x34;

/// Builds the compact word stream and its operand pools.
#[derive(Clone, Debug, Default)]
pub struct CodeBuilder {
    /// The executed instruction stream.
    pub words: Vec<u32>,
    /// Resolved call sites appended by `Call`/`CallIndirect`/`CallBuiltin`.
    pub call_sites: Vec<CallSite>,
    /// Shared operand pool indexed by each call site's `ins`/`outs` ranges.
    pub operands: Vec<Operand>,
}

impl CodeBuilder {
    /// Creates an empty builder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            words: Vec::new(),
            call_sites: Vec::new(),
            operands: Vec::new(),
        }
    }

    /// Appends the encoding of `instr`.
    ///
    /// # Errors
    ///
    /// Returns a message if a value does not fit its encoded field (a wide
    /// `a`/`b` operand above `u16`, a 24-bit index above [`FIELD24_MAX`], or an
    /// arithmetic type id above `u16`).
    #[allow(clippy::too_many_lines)]
    pub fn push(&mut self, instr: &Instr) -> Result<(), String> {
        match instr {
            Instr::Const { dst, imm, ty } => {
                self.emit_ops(OP_CONST, &[*dst, *ty])?;
                let bytes = imm.to_le_bytes();
                self.words
                    .push(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
                self.words
                    .push(u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]));
            }
            Instr::ConstData { dst, data, ty } => {
                self.emit_ops(OP_CONST_DATA, &[*dst, *data, *ty])?;
            }
            Instr::Copy { dst, src, ty } => {
                let op = OP_COPY_BASE + kind(*dst) * 2 + kind(*src);
                self.emit_ops(op, &[operand_value(*dst), operand_value(*src), *ty])?;
            }
            Instr::CopyBytes { dst, src, size } => {
                self.emit_ops(OP_COPY_BYTES, &[*dst, *src, *size])?;
            }
            Instr::Load { dst, src, ty } => {
                self.emit_ops(OP_LOAD, &[*dst, *src, *ty])?;
            }
            Instr::Store { src, dst, ty } => {
                let op = OP_STORE_BASE + kind(*src);
                self.emit_ops(op, &[operand_value(*src), *dst, *ty])?;
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
                let op = OP_FIELD_BASE + u8::from(*indirect);
                self.emit_ops(op, &[*dst, *place, *off])?;
                self.words.push(*ty);
            }
            Instr::StoreField {
                src,
                place,
                off,
                ty,
                indirect,
            } => {
                let op = OP_STOREFIELD_BASE + u8::from(*indirect) * 2 + kind(*src);
                self.emit_ops(op, &[operand_value(*src), *place, *off])?;
                self.words.push(*ty);
            }
            Instr::Arith {
                op,
                dst,
                lhs,
                rhs,
                operand_ty,
                ty,
            } => {
                self.emit_ops(arith_opcode(*op), &[*dst, *lhs, *rhs])?;
                if *operand_ty > u32::from(u16::MAX) || *ty > u32::from(u16::MAX) {
                    return Err("arithmetic type id exceeds the 16-bit word-1 field".to_string());
                }
                self.words
                    .push((*operand_ty & 0xFFFF) | ((*ty & 0xFFFF) << 16));
            }
            Instr::Convert { op, dst, src, ty } => {
                self.emit_ops(convert_opcode(*op), &[*dst, *src, *ty])?;
            }
            Instr::Call { func, ins, outs } => {
                let index = self.push_call_site(*func, ins, outs)?;
                self.emit_field(OP_CALL, index)?;
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

/// Maps an [`ArithOp`] to its flattened opcode.
pub(crate) const fn arith_opcode(op: ArithOp) -> u8 {
    OP_ARITH_BASE
        + match op {
            ArithOp::IAdd => 0x00,
            ArithOp::ISub => 0x01,
            ArithOp::IMul => 0x02,
            ArithOp::IDiv => 0x03,
            ArithOp::IDivU => 0x04,
            ArithOp::IMod => 0x05,
            ArithOp::IModU => 0x06,
            ArithOp::FAdd => 0x07,
            ArithOp::FSub => 0x08,
            ArithOp::FMul => 0x09,
            ArithOp::FDiv => 0x0A,
            ArithOp::FMod => 0x0B,
            ArithOp::Eq => 0x0C,
            ArithOp::ILt => 0x0D,
            ArithOp::ILtU => 0x0E,
            ArithOp::IGt => 0x0F,
            ArithOp::IGtU => 0x10,
            ArithOp::FLt => 0x11,
            ArithOp::FGt => 0x12,
        }
}

/// Inverts [`arith_opcode`].
pub(crate) const fn arith_from_opcode(op: u8) -> Option<ArithOp> {
    Some(match op.wrapping_sub(OP_ARITH_BASE) {
        0x00 => ArithOp::IAdd,
        0x01 => ArithOp::ISub,
        0x02 => ArithOp::IMul,
        0x03 => ArithOp::IDiv,
        0x04 => ArithOp::IDivU,
        0x05 => ArithOp::IMod,
        0x06 => ArithOp::IModU,
        0x07 => ArithOp::FAdd,
        0x08 => ArithOp::FSub,
        0x09 => ArithOp::FMul,
        0x0A => ArithOp::FDiv,
        0x0B => ArithOp::FMod,
        0x0C => ArithOp::Eq,
        0x0D => ArithOp::ILt,
        0x0E => ArithOp::ILtU,
        0x0F => ArithOp::IGt,
        0x10 => ArithOp::IGtU,
        0x11 => ArithOp::FLt,
        0x12 => ArithOp::FGt,
        _ => return None,
    })
}

/// Maps a [`ConvertOp`] to its flattened opcode.
pub(crate) const fn convert_opcode(op: ConvertOp) -> u8 {
    OP_CONVERT_BASE
        + match op {
            ConvertOp::Trunc => 0,
            ConvertOp::ExtendZero => 1,
        }
}

/// Inverts [`convert_opcode`].
pub(crate) const fn convert_from_opcode(op: u8) -> Option<ConvertOp> {
    Some(match op.wrapping_sub(OP_CONVERT_BASE) {
        0 => ConvertOp::Trunc,
        1 => ConvertOp::ExtendZero,
        _ => return None,
    })
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
    match op & !WIDE_FLAG {
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
        OP_ARITH_BASE..=OP_ARITH_LAST => arith_mnemonic(op),
        OP_CONVERT_BASE..=OP_CONVERT_LAST => convert_mnemonic(op),
        _ => "unknown",
    }
}

/// Mnemonic for an arithmetic opcode byte.
fn arith_mnemonic(op: u8) -> &'static str {
    match arith_from_opcode(op & !WIDE_FLAG) {
        Some(arith) => arith.as_str(),
        None => "unknown",
    }
}

/// Mnemonic for a convert opcode byte.
fn convert_mnemonic(op: u8) -> &'static str {
    match convert_from_opcode(op & !WIDE_FLAG) {
        Some(convert) => convert.as_str(),
        None => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::{opcode_mnemonic, CodeBuilder};
    use crate::decode::{decode, Decoded};
    use crate::isa::{ArithOp, Builtin, ConvertOp, Instr, Operand, TypeId};

    /// Rebuilds a typed instruction from a decoded view, allocating the call
    /// operand lists. Test-only; the interpreter uses the borrowed view.
    fn to_instr(d: &Decoded<'_>) -> Instr {
        match *d {
            Decoded::Const { dst, imm, ty } => Instr::Const { dst, imm, ty },
            Decoded::ConstData { dst, data, ty } => Instr::ConstData { dst, data, ty },
            Decoded::Copy { dst, src, ty } => Instr::Copy { dst, src, ty },
            Decoded::CopyBytes { dst, src, size } => Instr::CopyBytes { dst, src, size },
            Decoded::Load { dst, src, ty } => Instr::Load { dst, src, ty },
            Decoded::Store { src, dst, ty } => Instr::Store { src, dst, ty },
            Decoded::Addr { dst, place } => Instr::Addr { dst, place },
            Decoded::AddrField {
                dst,
                place,
                off,
                indirect,
            } => Instr::AddrField {
                dst,
                place,
                off,
                indirect,
            },
            Decoded::AddrFunc { dst, func } => Instr::AddrFunc { dst, func },
            Decoded::Field {
                dst,
                place,
                off,
                ty,
                indirect,
            } => Instr::Field {
                dst,
                place,
                off,
                ty,
                indirect,
            },
            Decoded::StoreField {
                src,
                place,
                off,
                ty,
                indirect,
            } => Instr::StoreField {
                src,
                place,
                off,
                ty,
                indirect,
            },
            Decoded::Arith {
                op,
                dst,
                lhs,
                rhs,
                operand_ty,
                ty,
            } => Instr::Arith {
                op,
                dst,
                lhs,
                rhs,
                operand_ty,
                ty,
            },
            Decoded::Convert { op, dst, src, ty } => Instr::Convert { op, dst, src, ty },
            Decoded::Call { site, ins, outs } => Instr::Call {
                func: site.func,
                ins: ins.to_vec(),
                outs: outs_to_u32(outs),
            },
            Decoded::CallIndirect {
                callee,
                site: _,
                ins,
                outs,
            } => Instr::CallIndirect {
                callee,
                ins: ins.to_vec(),
                outs: outs_to_u32(outs),
            },
            Decoded::CallBuiltin {
                builtin, ins, outs, ..
            } => Instr::CallBuiltin {
                builtin,
                ins: ins.to_vec(),
                outs: outs_to_u32(outs),
            },
            Decoded::Branch { target } => Instr::Branch { target },
            Decoded::BranchIf { cond, then, els } => Instr::BranchIf { cond, then, els },
            Decoded::Return => Instr::Return,
            Decoded::Panic { data } => Instr::Panic { data },
            Decoded::Unreachable => Instr::Unreachable,
        }
    }

    fn outs_to_u32(outs: &[Operand]) -> Vec<u32> {
        outs.iter()
            .map(|o| match *o {
                Operand::Slot(s) => s,
                Operand::Reg(r) => r,
            })
            .collect()
    }

    fn round_trip(instr: &Instr) {
        let mut b = CodeBuilder::new();
        b.push(instr).expect("encode");
        let (decoded, next) = decode(&b.words, 0, &b.call_sites, &b.operands).expect("decode");
        assert_eq!(next as usize, b.words.len(), "next pc mismatch: {instr:?}");
        assert_eq!(to_instr(&decoded), *instr, "round trip mismatch");
    }

    /// One instance of every `Instr` variant, including all operand-kind,
    /// indirect, arith-op, and builtin sub-variants.
    #[allow(clippy::too_many_lines)]
    fn sample_instrs() -> Vec<Instr> {
        let ty: TypeId = 0;
        let mut instrs = vec![
            Instr::Const {
                dst: 3,
                imm: 0x1122_3344_5566_7788,
                ty,
            },
            Instr::ConstData {
                dst: 4,
                data: 7,
                ty,
            },
            Instr::Copy {
                dst: Operand::Reg(1),
                src: Operand::Reg(2),
                ty,
            },
            Instr::Copy {
                dst: Operand::Slot(8),
                src: Operand::Reg(2),
                ty,
            },
            Instr::Copy {
                dst: Operand::Reg(1),
                src: Operand::Slot(8),
                ty,
            },
            Instr::Copy {
                dst: Operand::Slot(8),
                src: Operand::Slot(16),
                ty,
            },
            Instr::CopyBytes {
                dst: 8,
                src: 16,
                size: 32,
            },
            Instr::Load { dst: 1, src: 8, ty },
            Instr::Store {
                src: Operand::Reg(1),
                dst: 8,
                ty,
            },
            Instr::Store {
                src: Operand::Slot(8),
                dst: 16,
                ty,
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
                ty,
                indirect: false,
            },
            Instr::Field {
                dst: 1,
                place: 8,
                off: 12,
                ty,
                indirect: true,
            },
            Instr::StoreField {
                src: Operand::Reg(1),
                place: 8,
                off: 12,
                ty,
                indirect: false,
            },
            Instr::StoreField {
                src: Operand::Slot(16),
                place: 8,
                off: 12,
                ty,
                indirect: false,
            },
            Instr::StoreField {
                src: Operand::Reg(1),
                place: 8,
                off: 12,
                ty,
                indirect: true,
            },
            Instr::StoreField {
                src: Operand::Slot(16),
                place: 8,
                off: 12,
                ty,
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
        for op in arith_ops() {
            instrs.push(Instr::Arith {
                op,
                dst: 1,
                lhs: 2,
                rhs: 3,
                operand_ty: 4,
                ty: 5,
            });
        }
        for op in [ConvertOp::Trunc, ConvertOp::ExtendZero] {
            instrs.push(Instr::Convert {
                op,
                dst: 1,
                src: 2,
                ty: 3,
            });
        }
        instrs
    }

    fn arith_ops() -> Vec<ArithOp> {
        vec![
            ArithOp::IAdd,
            ArithOp::ISub,
            ArithOp::IMul,
            ArithOp::IDiv,
            ArithOp::IDivU,
            ArithOp::IMod,
            ArithOp::IModU,
            ArithOp::FAdd,
            ArithOp::FSub,
            ArithOp::FMul,
            ArithOp::FDiv,
            ArithOp::FMod,
            ArithOp::Eq,
            ArithOp::ILt,
            ArithOp::ILtU,
            ArithOp::IGt,
            ArithOp::IGtU,
            ArithOp::FLt,
            ArithOp::FGt,
        ]
    }

    #[test]
    fn every_variant_round_trips() {
        for instr in sample_instrs() {
            round_trip(&instr);
        }
    }

    #[test]
    fn wide_operand_round_trips() {
        let instr = Instr::Copy {
            dst: Operand::Slot(300),
            src: Operand::Reg(511),
            ty: 0,
        };
        round_trip(&instr);
    }

    #[test]
    fn third_wide_operand_round_trips() {
        let instr = Instr::Field {
            dst: 700,
            place: 1000,
            off: 70_000,
            ty: 900,
            indirect: true,
        };
        round_trip(&instr);
    }

    #[test]
    fn arith_is_two_words() {
        let mut b = CodeBuilder::new();
        b.push(&Instr::Arith {
            op: ArithOp::IAdd,
            dst: 1,
            lhs: 2,
            rhs: 3,
            operand_ty: 7,
            ty: 9,
        })
        .expect("encode");
        assert_eq!(b.words.len(), 2);
        assert_eq!(b.words[0] & 0xFF, u32::from(super::OP_ARITH_BASE));
        assert_eq!(b.words[0] >> 8 & 0xFF, 1);
        assert_eq!(b.words[0] >> 16 & 0xFF, 2);
        assert_eq!(b.words[0] >> 24 & 0xFF, 3);
        assert_eq!(b.words[1] & 0xFFFF, 7);
        assert_eq!(b.words[1] >> 16, 9);
    }

    #[test]
    fn call_appends_to_pools() {
        let instr = Instr::Call {
            func: 3,
            ins: vec![Operand::Reg(0), Operand::Slot(8)],
            outs: vec![16, 24],
        };
        let mut b = CodeBuilder::new();
        b.push(&instr).expect("encode");
        assert_eq!(b.words.len(), 1);
        assert_eq!(b.call_sites.len(), 1);
        assert_eq!(b.operands.len(), 4);
        assert_eq!(b.call_sites[0].func, 3);
        assert_eq!(b.call_sites[0].ins, 0..2);
        assert_eq!(b.call_sites[0].outs, 2..4);
        round_trip(&instr);
    }

    #[test]
    fn wide_opcode_mnemonic_strips_flag() {
        assert_eq!(opcode_mnemonic(super::OP_ARITH_BASE), "i.add");
        assert_eq!(opcode_mnemonic(super::OP_ARITH_BASE | 0x80), "i.add");
        assert_eq!(opcode_mnemonic(0x7F), "unknown");
    }

    #[test]
    fn rejects_unfittable_operand() {
        // `src` lands in the `b` slot of `Copy`, which the wide form stores
        // as a `u16`; a slot offset above that cannot be encoded.
        let instr = Instr::Copy {
            dst: Operand::Slot(70_000),
            src: Operand::Slot(70_000),
            ty: 0,
        };
        let mut b = CodeBuilder::new();
        assert!(b.push(&instr).is_err());
    }
}
