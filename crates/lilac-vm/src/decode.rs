//! Decoder for the compact `u32` bytecode stream.
//!
//! [`decode`] turns one word (plus any extension words) at `pc` back into a
//! borrowed [`Decoded`] view of the typed [`crate::isa::Instr`]. Call variants
//! borrow their [`CallSite`] and operand slices from the caller's pools instead
//! of allocating, so the hot interpreter path never touches the heap.

use crate::encode::{
    arith_from_opcode, builtin_from_index, convert_from_opcode, OP_ADDR, OP_ADDRFIELD_BASE,
    OP_ADDRFIELD_LAST, OP_ADDR_FUNC, OP_ARITH_BASE, OP_ARITH_LAST, OP_BRANCH, OP_BRANCH_IF,
    OP_CALL, OP_CALL_BUILTIN_BASE, OP_CALL_BUILTIN_LAST, OP_CALL_INDIRECT, OP_CONST, OP_CONST_DATA,
    OP_CONVERT_BASE, OP_CONVERT_LAST, OP_COPY_BASE, OP_COPY_BYTES, OP_COPY_LAST, OP_FIELD_BASE,
    OP_FIELD_LAST, OP_LOAD, OP_PANIC, OP_RETURN, OP_STOREFIELD_BASE, OP_STOREFIELD_LAST,
    OP_STORE_BASE, OP_STORE_LAST, OP_UNREACHABLE, WIDE_FLAG,
};
use crate::isa::{ArithOp, Builtin, CallSite, ConvertOp, Operand, TypeId};

/// A borrowed view of one decoded instruction.
///
/// Mirrors [`crate::isa::Instr`] field for field, except the call variants
/// borrow their operand lists and call site rather than owning `Vec`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoded<'a> {
    /// `dst: ty = const imm`.
    Const {
        /// Destination register.
        dst: u32,
        /// Literal bits.
        imm: u64,
        /// Value type.
        ty: TypeId,
    },
    /// `dst: ty = const.data data`.
    ConstData {
        /// Destination register.
        dst: u32,
        /// Data index.
        data: u32,
        /// Value type.
        ty: TypeId,
    },
    /// Scalar copy (kind encoded in the opcode).
    Copy {
        /// Destination operand.
        dst: Operand,
        /// Source operand.
        src: Operand,
        /// Value type.
        ty: TypeId,
    },
    /// Aggregate place-to-place byte copy.
    CopyBytes {
        /// Destination slot offset.
        dst: u32,
        /// Source slot offset.
        src: u32,
        /// Byte count.
        size: u32,
    },
    /// Read `size(ty)` bytes through the reference held in `src`.
    Load {
        /// Destination register.
        dst: u32,
        /// Slot holding the reference.
        src: u32,
        /// Pointee type.
        ty: TypeId,
    },
    /// Write `size(ty)` bytes through the reference held in `dst`.
    Store {
        /// Value operand.
        src: Operand,
        /// Slot holding the reference.
        dst: u32,
        /// Value type.
        ty: TypeId,
    },
    /// `dst = addr place`.
    Addr {
        /// Destination register.
        dst: u32,
        /// Slot whose address is taken.
        place: u32,
    },
    /// `dst = addr.field place + off`.
    AddrField {
        /// Destination register.
        dst: u32,
        /// Base slot.
        place: u32,
        /// Field byte offset.
        off: u32,
        /// Whether `place` holds a ref to dereference first.
        indirect: bool,
    },
    /// `dst = addr.func func`.
    AddrFunc {
        /// Destination register.
        dst: u32,
        /// Function index.
        func: u32,
    },
    /// Scalar field read.
    Field {
        /// Destination register.
        dst: u32,
        /// Object slot.
        place: u32,
        /// Field byte offset.
        off: u32,
        /// Field type.
        ty: TypeId,
        /// Whether `place` holds a ref to dereference first.
        indirect: bool,
    },
    /// Scalar field write.
    StoreField {
        /// Value operand.
        src: Operand,
        /// Object slot.
        place: u32,
        /// Field byte offset.
        off: u32,
        /// Field type.
        ty: TypeId,
        /// Whether `place` holds a ref to dereference first.
        indirect: bool,
    },
    /// Typed arithmetic/comparison.
    Arith {
        /// Operator.
        op: ArithOp,
        /// Destination register.
        dst: u32,
        /// Left register.
        lhs: u32,
        /// Right register.
        rhs: u32,
        /// Operand width type.
        operand_ty: TypeId,
        /// Result type.
        ty: TypeId,
    },
    /// Width conversion.
    Convert {
        /// Operator.
        op: ConvertOp,
        /// Destination register.
        dst: u32,
        /// Source register.
        src: u32,
        /// Result type.
        ty: TypeId,
    },
    /// Direct call by function index.
    Call {
        /// Resolved call site.
        site: &'a CallSite,
        /// Input operands.
        ins: &'a [Operand],
        /// Output slot operands.
        outs: &'a [Operand],
    },
    /// Indirect call through a register holding a function id.
    CallIndirect {
        /// Register holding the callee.
        callee: u32,
        /// Resolved call site.
        site: &'a CallSite,
        /// Input operands.
        ins: &'a [Operand],
        /// Output slot operands.
        outs: &'a [Operand],
    },
    /// Call to a compiler leaf builtin.
    CallBuiltin {
        /// Builtin.
        builtin: Builtin,
        /// Resolved call site.
        site: &'a CallSite,
        /// Input operands.
        ins: &'a [Operand],
        /// Output slot operands.
        outs: &'a [Operand],
    },
    /// Unconditional branch.
    Branch {
        /// Target program counter.
        target: u32,
    },
    /// Conditional branch.
    BranchIf {
        /// Condition register.
        cond: u32,
        /// Taken target.
        then: u32,
        /// Untaken target.
        els: u32,
    },
    /// Return from the current function.
    Return,
    /// Print `data` bytes and abort.
    Panic {
        /// Data index.
        data: u32,
    },
    /// VM internal trap.
    Unreachable,
}

/// Decodes the instruction at `pc`.
///
/// Returns the borrowed instruction and the program counter of the next
/// instruction (advancing by the instruction's word count).
///
/// # Errors
///
/// Returns a human-readable message if `pc` is out of range, the opcode is
/// unknown, the instruction is truncated, or a call site or operand range is
/// out of range.
#[allow(clippy::too_many_lines)]
pub fn decode<'a>(
    code: &'a [u32],
    pc: usize,
    call_sites: &'a [CallSite],
    operands: &'a [Operand],
) -> Result<(Decoded<'a>, u32), String> {
    let w = *code
        .get(pc)
        .ok_or_else(|| format!("pc {pc} out of range (code length {})", code.len()))?;
    let raw = w.to_le_bytes()[0];
    let wide = raw & WIDE_FLAG != 0;
    let base = raw & !WIDE_FLAG;
    let count =
        operand_count(base).ok_or_else(|| format!("unknown opcode 0x{base:02X} at pc {pc}"))?;
    if wide && count == 0 {
        return Err(format!(
            "wide flag on zero-operand opcode 0x{base:02X} at pc {pc}"
        ));
    }
    let (decoded, words) = match base {
        OP_RETURN => (Decoded::Return, 1),
        OP_UNREACHABLE => (Decoded::Unreachable, 1),
        OP_PANIC => (Decoded::Panic { data: w >> 8 }, 1),
        OP_BRANCH => (Decoded::Branch { target: w >> 8 }, 1),
        OP_BRANCH_IF => {
            let (ops, n) = read_ops(code, pc, wide, 3)?;
            (
                Decoded::BranchIf {
                    cond: ops[0],
                    then: ops[1],
                    els: ops[2],
                },
                n,
            )
        }
        OP_CONST => {
            let (ops, n) = read_ops(code, pc, wide, 2)?;
            let lo = word_at(code, pc + n)?;
            let hi = word_at(code, pc + n + 1)?;
            let imm = u64::from(lo) | (u64::from(hi) << 32);
            (
                Decoded::Const {
                    dst: ops[0],
                    imm,
                    ty: ops[1],
                },
                n + 2,
            )
        }
        OP_CONST_DATA => {
            let (ops, n) = read_ops(code, pc, wide, 3)?;
            (
                Decoded::ConstData {
                    dst: ops[0],
                    data: ops[1],
                    ty: ops[2],
                },
                n,
            )
        }
        b @ (OP_COPY_BASE..=OP_COPY_LAST) => {
            let (ops, n) = read_ops(code, pc, wide, 3)?;
            let k = b - OP_COPY_BASE;
            let dst = operate(ops[0], k & 2 != 0);
            let src = operate(ops[1], k & 1 != 0);
            (
                Decoded::Copy {
                    dst,
                    src,
                    ty: ops[2],
                },
                n,
            )
        }
        OP_COPY_BYTES => {
            let (ops, n) = read_ops(code, pc, wide, 3)?;
            (
                Decoded::CopyBytes {
                    dst: ops[0],
                    src: ops[1],
                    size: ops[2],
                },
                n,
            )
        }
        OP_LOAD => {
            let (ops, n) = read_ops(code, pc, wide, 3)?;
            (
                Decoded::Load {
                    dst: ops[0],
                    src: ops[1],
                    ty: ops[2],
                },
                n,
            )
        }
        b @ (OP_STORE_BASE..=OP_STORE_LAST) => {
            let (ops, n) = read_ops(code, pc, wide, 3)?;
            let src = operate(ops[0], b == OP_STORE_LAST);
            (
                Decoded::Store {
                    src,
                    dst: ops[1],
                    ty: ops[2],
                },
                n,
            )
        }
        OP_ADDR => {
            let (ops, n) = read_ops(code, pc, wide, 2)?;
            (
                Decoded::Addr {
                    dst: ops[0],
                    place: ops[1],
                },
                n,
            )
        }
        b @ (OP_ADDRFIELD_BASE..=OP_ADDRFIELD_LAST) => {
            let (ops, n) = read_ops(code, pc, wide, 3)?;
            (
                Decoded::AddrField {
                    dst: ops[0],
                    place: ops[1],
                    off: ops[2],
                    indirect: b == OP_ADDRFIELD_LAST,
                },
                n,
            )
        }
        OP_ADDR_FUNC => {
            let (ops, n) = read_ops(code, pc, wide, 2)?;
            (
                Decoded::AddrFunc {
                    dst: ops[0],
                    func: ops[1],
                },
                n,
            )
        }
        b @ (OP_FIELD_BASE..=OP_FIELD_LAST) => {
            let (ops, n) = read_ops(code, pc, wide, 3)?;
            let ty = word_at(code, pc + n)?;
            (
                Decoded::Field {
                    dst: ops[0],
                    place: ops[1],
                    off: ops[2],
                    ty,
                    indirect: b == OP_FIELD_LAST,
                },
                n + 1,
            )
        }
        b @ (OP_STOREFIELD_BASE..=OP_STOREFIELD_LAST) => {
            let (ops, n) = read_ops(code, pc, wide, 3)?;
            let ty = word_at(code, pc + n)?;
            let k = b - OP_STOREFIELD_BASE;
            let src = operate(ops[0], k & 1 != 0);
            (
                Decoded::StoreField {
                    src,
                    place: ops[1],
                    off: ops[2],
                    ty,
                    indirect: k & 2 != 0,
                },
                n + 1,
            )
        }
        b @ (OP_ARITH_BASE..=OP_ARITH_LAST) => {
            let (ops, n) = read_ops(code, pc, wide, 3)?;
            let op = arith_from_opcode(b)
                .ok_or_else(|| format!("unknown arithmetic opcode 0x{b:02X} at pc {pc}"))?;
            let meta = word_at(code, pc + n)?;
            (
                Decoded::Arith {
                    op,
                    dst: ops[0],
                    lhs: ops[1],
                    rhs: ops[2],
                    operand_ty: meta & 0xFFFF,
                    ty: meta >> 16,
                },
                n + 1,
            )
        }
        b @ (OP_CONVERT_BASE..=OP_CONVERT_LAST) => {
            let (ops, n) = read_ops(code, pc, wide, 3)?;
            let op = convert_from_opcode(b)
                .ok_or_else(|| format!("unknown convert opcode 0x{b:02X} at pc {pc}"))?;
            (
                Decoded::Convert {
                    op,
                    dst: ops[0],
                    src: ops[1],
                    ty: ops[2],
                },
                n,
            )
        }
        OP_CALL => {
            let (site, ins, outs) = call_slice(call_sites, operands, (w >> 8) as usize)?;
            (Decoded::Call { site, ins, outs }, 1)
        }
        OP_CALL_INDIRECT => {
            let (ops, n) = read_ops(code, pc, wide, 1)?;
            let index = word_at(code, pc + n)? as usize;
            let (site, ins, outs) = call_slice(call_sites, operands, index)?;
            (
                Decoded::CallIndirect {
                    callee: ops[0],
                    site,
                    ins,
                    outs,
                },
                n + 1,
            )
        }
        b @ (OP_CALL_BUILTIN_BASE..=OP_CALL_BUILTIN_LAST) => {
            let builtin = builtin_from_index(b - OP_CALL_BUILTIN_BASE)
                .ok_or_else(|| format!("unknown builtin opcode 0x{b:02X} at pc {pc}"))?;
            let (site, ins, outs) = call_slice(call_sites, operands, (w >> 8) as usize)?;
            (
                Decoded::CallBuiltin {
                    builtin,
                    site,
                    ins,
                    outs,
                },
                1,
            )
        }
        _ => return Err(format!("unknown opcode 0x{base:02X} at pc {pc}")),
    };
    let next = u32::try_from(pc + words).map_err(|_| format!("pc overflow at word {pc}"))?;
    Ok((decoded, next))
}

/// The number of `a`/`b`/`c` operands a base opcode carries, or `None` if the
/// opcode is unknown. The wide escape is only legal when this is non-zero.
const fn operand_count(base: u8) -> Option<u8> {
    Some(match base {
        OP_RETURN
        | OP_UNREACHABLE
        | OP_PANIC
        | OP_BRANCH
        | OP_CALL
        | OP_CALL_BUILTIN_BASE..=OP_CALL_BUILTIN_LAST => 0,
        OP_CALL_INDIRECT => 1,
        OP_CONST | OP_ADDR | OP_ADDR_FUNC => 2,
        OP_BRANCH_IF
        | OP_CONST_DATA
        | OP_COPY_BASE..=OP_COPY_LAST
        | OP_COPY_BYTES
        | OP_LOAD
        | OP_STORE_BASE..=OP_STORE_LAST
        | OP_ADDRFIELD_BASE..=OP_ADDRFIELD_LAST
        | OP_FIELD_BASE..=OP_FIELD_LAST
        | OP_STOREFIELD_BASE..=OP_STOREFIELD_LAST
        | OP_ARITH_BASE..=OP_ARITH_LAST
        | OP_CONVERT_BASE..=OP_CONVERT_LAST => 3,
        _ => return None,
    })
}

/// Builds a register or slot [`Operand`] from a raw value and slot flag.
const fn operate(value: u32, is_slot: bool) -> Operand {
    if is_slot {
        Operand::Slot(value)
    } else {
        Operand::Reg(value)
    }
}

/// Reads the word at `index`, or reports a truncated instruction.
fn word_at(code: &[u32], index: usize) -> Result<u32, String> {
    code.get(index)
        .copied()
        .ok_or_else(|| format!("truncated instruction at word {index}"))
}

/// Reads an instruction's `n` operands, consuming wide extension words as
/// needed; returns the operands and the number of words consumed.
fn read_ops(code: &[u32], pc: usize, wide: bool, n: usize) -> Result<([u32; 3], usize), String> {
    let mut ops = [0_u32; 3];
    if wide {
        let ext0 = word_at(code, pc + 1)?;
        ops[0] = ext0 & 0xFFFF;
        ops[1] = (ext0 >> 16) & 0xFFFF;
        if n >= 3 {
            ops[2] = word_at(code, pc + 2)?;
            Ok((ops, 3))
        } else {
            Ok((ops, 2))
        }
    } else {
        let bytes = word_at(code, pc)?.to_le_bytes();
        ops[0] = u32::from(bytes[1]);
        ops[1] = u32::from(bytes[2]);
        ops[2] = u32::from(bytes[3]);
        Ok((ops, 1))
    }
}

/// Resolves a call-site index into its site and operand slices.
fn call_slice<'a>(
    call_sites: &'a [CallSite],
    operands: &'a [Operand],
    index: usize,
) -> Result<(&'a CallSite, &'a [Operand], &'a [Operand]), String> {
    let site = call_sites
        .get(index)
        .ok_or_else(|| format!("call site {index} out of range"))?;
    let ins = operand_range(operands, &site.ins)?;
    let outs = operand_range(operands, &site.outs)?;
    Ok((site, ins, outs))
}

/// Slices an operand pool by a call-site range, rejecting bad ranges cleanly.
fn operand_range<'a>(
    pool: &'a [Operand],
    range: &core::ops::Range<u32>,
) -> Result<&'a [Operand], String> {
    pool.get(range.start as usize..range.end as usize)
        .ok_or_else(|| format!("operand range {}..{} out of range", range.start, range.end))
}

#[cfg(test)]
mod tests {
    use super::{decode, Decoded};
    use crate::encode::CodeBuilder;
    use crate::isa::{ArithOp, CallSite, Instr, Operand};

    #[test]
    fn next_pc_advances_by_word_count() {
        let mut b = CodeBuilder::new();
        b.push(&Instr::Arith {
            op: ArithOp::IAdd,
            dst: 1,
            lhs: 2,
            rhs: 3,
            operand_ty: 4,
            ty: 5,
        })
        .expect("encode arith");
        b.push(&Instr::Return).expect("encode return");
        assert_eq!(b.words.len(), 3);
        let (first, next) = decode(&b.words, 0, &[], &[]).expect("decode arith");
        assert!(matches!(
            first,
            Decoded::Arith {
                op: ArithOp::IAdd,
                ..
            }
        ));
        assert_eq!(next, 2);
        let (second, next) = decode(&b.words, 2, &[], &[]).expect("decode return");
        assert!(matches!(second, Decoded::Return));
        assert_eq!(next, 3);
    }

    #[test]
    fn decodes_const_immediate() {
        let instr = Instr::Const {
            dst: 1,
            imm: 0xDEAD_BEEF_0102_0304,
            ty: 2,
        };
        let mut b = CodeBuilder::new();
        b.push(&instr).expect("encode");
        let (decoded, next) = decode(&b.words, 0, &[], &[]).expect("decode");
        assert_eq!(next as usize, b.words.len());
        assert_eq!(
            decoded,
            Decoded::Const {
                dst: 1,
                imm: 0xDEAD_BEEF_0102_0304,
                ty: 2
            }
        );
    }

    #[test]
    fn decodes_call_borrowing_pools() {
        let instr = Instr::Call {
            func: 3,
            ins: vec![Operand::Reg(0), Operand::Slot(8)],
            outs: vec![16],
        };
        let mut b = CodeBuilder::new();
        b.push(&instr).expect("encode");
        let (decoded, _) = decode(&b.words, 0, &b.call_sites, &b.operands).expect("decode");
        match decoded {
            Decoded::Call { site, ins, outs } => {
                assert_eq!(site.func, 3);
                assert_eq!(ins, &[Operand::Reg(0), Operand::Slot(8)]);
                assert_eq!(outs, &[Operand::Slot(16)]);
            }
            other => panic!("expected call, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_opcode() {
        let code = [0x0000_007F_u32];
        let err = decode(&code, 0, &[], &[]).expect_err("unknown opcode");
        assert!(err.contains("unknown opcode"), "{err}");
    }

    #[test]
    fn rejects_unknown_wide_opcode() {
        let code = [0x0000_00FF_u32];
        let err = decode(&code, 0, &[], &[]).expect_err("unknown");
        assert!(err.contains("unknown opcode"), "{err}");
    }

    #[test]
    fn rejects_wide_flag_on_zero_operand_opcode() {
        let code = [u32::from(
            crate::encode::OP_RETURN | crate::encode::WIDE_FLAG,
        )];
        let err = decode(&code, 0, &[], &[]).expect_err("wide return");
        assert!(err.contains("wide flag"), "{err}");
    }

    #[test]
    fn rejects_truncated_instruction() {
        // A wide Copy opcode with no extension word.
        let code = [u32::from(
            crate::encode::OP_COPY_BASE | crate::encode::WIDE_FLAG,
        )];
        let err = decode(&code, 0, &[], &[]).expect_err("truncated");
        assert!(err.contains("truncated"), "{err}");
    }

    #[test]
    fn rejects_out_of_range_pc() {
        let err = decode(&[], 0, &[], &[]).expect_err("bad pc");
        assert!(err.contains("out of range"), "{err}");
    }

    #[test]
    fn rejects_out_of_range_call_site() {
        let mut b = CodeBuilder::new();
        b.push(&Instr::Call {
            func: 0,
            ins: Vec::new(),
            outs: Vec::new(),
        })
        .expect("encode");
        let err = decode(&b.words, 0, &[], &b.operands).expect_err("bad call site");
        assert!(err.contains("call site"), "{err}");
    }

    #[test]
    fn rejects_out_of_range_operand_range() {
        let site = CallSite {
            func: 0,
            ins: 0..4,
            outs: 0..0,
        };
        let pool = [Operand::Reg(0)];
        let err = decode(
            &[u32::from(crate::encode::OP_CALL)],
            0,
            core::slice::from_ref(&site),
            &pool,
        )
        .expect_err("bad operand range");
        assert!(err.contains("operand range"), "{err}");
    }
}
