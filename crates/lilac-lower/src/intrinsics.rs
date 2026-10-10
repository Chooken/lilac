//! Intrinsic calls: arithmetic and conversions, panics, `@array_at` and
//! atomics.

use lilac_hir::intrinsics::{AtomicOp, Intrinsic, NumOp, Ordering};
use lilac_hir::{HirExpr, HirFunc};
use lilac_ir::{
    ArithOp, AtomicOrdering, ConvertOp, LirCallee, LirConst, LirInst, LirOperand, LirTerm, LirTy,
    PlaceName, Reg, RmwOp,
};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeKind, TypeRef};

use crate::constant;
use crate::{BadIndex, FnState, Lower};

impl Lower<'_> {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn lower_intrinsic(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        intrinsic: Intrinsic,
        args: &[HirExpr],
        ty: TypeRef,
        span: Span,
    ) -> LirOperand {
        let lir = self.lir_ty(ty);
        match intrinsic {
            Intrinsic::Int(NumOp::Negate, _) | Intrinsic::Float(NumOp::Negate, _) => {
                let float = matches!(intrinsic, Intrinsic::Float(..));
                let operand = self.value_reg(state, func, &args[0]);
                let zero = state.reg();
                state.emit(LirInst::Const {
                    dst: zero.clone(),
                    value: if float {
                        LirConst::Float(0.0)
                    } else {
                        LirConst::Int(0)
                    },
                    ty: lir.clone(),
                    span,
                });
                let reg = state.reg();
                state.emit(LirInst::Arith {
                    op: if float { ArithOp::FSub } else { ArithOp::ISub },
                    lhs: zero,
                    rhs: operand,
                    dst: reg.clone(),
                    ty: lir,
                    span,
                });
                LirOperand::Reg(reg)
            }
            Intrinsic::Int(NumOp::Not, _) => {
                // `not x` is `x xor <all ones>`.
                let operand = self.value_reg(state, func, &args[0]);
                let ones = state.reg();
                let width = match lir {
                    LirTy::Bit(bits) => u32::from(bits),
                    _ => u32::try_from(self.ptr_bytes * 8).unwrap_or(64),
                };
                state.emit(LirInst::Const {
                    dst: ones.clone(),
                    value: LirConst::Int(constant::mask_of(width)),
                    ty: lir.clone(),
                    span,
                });
                let reg = state.reg();
                state.emit(LirInst::Arith {
                    op: ArithOp::Xor,
                    lhs: operand,
                    rhs: ones,
                    dst: reg.clone(),
                    ty: lir,
                    span,
                });
                LirOperand::Reg(reg)
            }
            Intrinsic::Int(op, _) | Intrinsic::Float(op, _) => {
                let float = matches!(intrinsic, Intrinsic::Float(..));
                let Some(op) = constant::arith_op(op, float) else {
                    lilac_error::bug!(span, "intrinsic without an IR operation");
                };
                let regs = self.regs(state, func, args);
                let reg = state.reg();
                state.emit(LirInst::Arith {
                    op,
                    lhs: regs[0].clone(),
                    rhs: regs[1].clone(),
                    dst: reg.clone(),
                    ty: lir,
                    span,
                });
                LirOperand::Reg(reg)
            }
            Intrinsic::FbitToBits(_)
            | Intrinsic::BitsToFbit(_)
            | Intrinsic::Truncate
            | Intrinsic::ExtendZero => {
                let op = match intrinsic {
                    Intrinsic::Truncate => ConvertOp::Trunc,
                    Intrinsic::ExtendZero => ConvertOp::ExtendZero,
                    _ => ConvertOp::Bitcast,
                };
                let src = self.value_reg(state, func, &args[0]);
                let reg = state.reg();
                state.emit(LirInst::Convert {
                    op,
                    src,
                    dst: reg.clone(),
                    ty: lir,
                    span,
                });
                LirOperand::Reg(reg)
            }
            Intrinsic::PtrToAddress | Intrinsic::AddressToPtr => {
                let src = self.value_reg(state, func, &args[0]);
                LirOperand::Reg(state.bitcast(src, lir, span))
            }
            Intrinsic::PtrAdd => {
                // `ptrtoint`, add, `inttoptr`: what the `@ptr_add` leaf does,
                // inline, so the operand keeps its `ref #unknown` type.
                let base = self.value_reg(state, func, &args[0]);
                let address = state.bitcast(base, LirTy::BitNative, span);
                let offset = self.value_reg(state, func, &args[1]);
                let sum = state.reg();
                state.emit(LirInst::Arith {
                    op: ArithOp::IAdd,
                    lhs: address,
                    rhs: offset,
                    dst: sum.clone(),
                    ty: LirTy::BitNative,
                    span,
                });
                LirOperand::Reg(state.bitcast(sum, lir, span))
            }
            Intrinsic::Memcpy
            | Intrinsic::Memmove
            | Intrinsic::Memset
            | Intrinsic::Syscall
            | Intrinsic::Trap => {
                let Some(name) = intrinsic.leaf_builtin() else {
                    lilac_error::bug!(span, "intrinsic `{intrinsic:?}` has no leaf");
                };
                let ins = args.iter().map(|arg| self.arg(state, func, arg)).collect();
                let out = (!ty.is_error()).then(|| self.tmp(state, ty, span));
                state.emit(LirInst::Call {
                    func: LirCallee::Builtin(name.to_owned()),
                    ins,
                    outs: out.iter().cloned().collect(),
                    span,
                });
                if intrinsic == Intrinsic::Trap {
                    state.end(LirTerm::Unreachable { span });
                }
                out.map_or(LirOperand::Reg(Reg::default()), LirOperand::Place)
            }
            Intrinsic::Atomic(op, ordering, _) => {
                self.lower_atomic(state, func, op, atomic_ordering(ordering), args, span)
            }
            Intrinsic::Fence(ordering) => {
                state.emit(LirInst::Fence {
                    order: atomic_ordering(ordering),
                    span,
                });
                LirOperand::Reg(Reg::default())
            }
            Intrinsic::Panic => {
                let msg = self.arg(state, func, &args[0]);
                self.panic_with(state, func, msg, span);
                LirOperand::Reg(Reg::default())
            }
            Intrinsic::ArrayAt => self.lower_array_at(state, func, args, span),
            Intrinsic::ArrayLen => {
                lilac_error::bug!(span, "`@array_len` folds to a literal in typeck")
            }
            _ => {
                let Some(symbol) = intrinsic.runtime_symbol() else {
                    lilac_error::bug!(span, "intrinsic `{intrinsic:?}` has no lowering");
                };
                let ins = args.iter().map(|arg| self.arg(state, func, arg)).collect();
                if ty.is_error() {
                    state.emit(LirInst::Call {
                        func: LirCallee::Func(symbol.into()),
                        ins,
                        outs: Vec::new(),
                        span,
                    });
                    return LirOperand::Reg(Reg::default());
                }
                let out = self.tmp(state, ty, span);
                state.emit(LirInst::Call {
                    func: LirCallee::Func(symbol.into()),
                    ins,
                    outs: vec![out.clone()],
                    span,
                });
                LirOperand::Place(out)
            }
        }
    }

    /// Ends the block with `.lilac.panic(msg, site)` and `unreachable`,
    /// where `msg` is a `#byte_buffer` place and `site` is this call's
    /// stack-trace line.
    pub(crate) fn panic_with(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        msg: PlaceName,
        span: Span,
    ) {
        let location = self.sources.location(span);
        let site = lilac_ir::trace::site_text(&self.display_of(func), location.as_deref());
        let site = self.byte_buffer_place(state, site.as_bytes(), span);
        state.emit(LirInst::Call {
            func: LirCallee::Func(".lilac.panic".into()),
            ins: vec![msg, site],
            outs: Vec::new(),
            span,
        });
        state.end(LirTerm::Unreachable { span });
    }

    /// A fresh `#byte_buffer` place over the data constant `bytes`.
    pub(crate) fn byte_buffer_place(
        &mut self,
        state: &mut FnState,
        bytes: &[u8],
        span: Span,
    ) -> PlaceName {
        let name = self.intern_data(bytes);
        let reg = state.reg();
        state.emit(LirInst::Const {
            dst: reg.clone(),
            value: LirConst::Data(name),
            ty: LirTy::byte_buffer(),
            span,
        });
        self.fresh_place(
            state,
            LirOperand::Reg(reg),
            Builtin::ByteBuffer.value(),
            span,
        )
    }

    /// Lowers `@array_at(ref array, index)`: the element address, kept in a
    /// place across an unsigned bounds check that panics with "index out
    /// of bounds". A constant index in bounds needs no check.
    pub(crate) fn lower_array_at(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        args: &[HirExpr],
        span: Span,
    ) -> LirOperand {
        let TypeKind::Array { elem, len } = *self.types.kind(args[0].ty.id) else {
            lilac_error::bug!(span, "`@array_at` of a non-array");
        };
        let index = match self.constant(&args[1]) {
            Some(constant::Value::Int(index, _)) => Some(index),
            _ => None,
        };
        let in_bounds = index.is_some_and(|index| index < len);
        if let Some(index) = index.filter(|_| !in_bounds) {
            self.bad_indices.push(BadIndex {
                func: self.symbols[&func.key].as_str().to_owned(),
                span: args[1].span,
                index,
                len,
            });
        }
        let operands = self.regs(state, func, args);
        let ty = self.lir_ty(TypeRef::reference(elem));
        let addr = state.reg();
        if in_bounds {
            state.emit(LirInst::ElemAddr {
                array: operands[0].clone(),
                index: operands[1].clone(),
                dst: addr.clone(),
                ty,
                span,
            });
            return LirOperand::Reg(addr);
        }
        state.emit(LirInst::ElemAddr {
            array: operands[0].clone(),
            index: operands[1].clone(),
            dst: addr.clone(),
            ty: ty.clone(),
            span,
        });
        let result = state.tmp_lir(ty, span);
        state.emit(LirInst::CopyPlace {
            src: LirOperand::Reg(addr),
            dst: result.clone(),
            span,
        });
        let count = state.reg();
        state.emit(LirInst::Const {
            dst: count.clone(),
            value: LirConst::Int(len),
            ty: LirTy::BitNative,
            span,
        });
        let in_bounds = state.reg();
        state.emit(LirInst::Arith {
            op: ArithOp::ILtU,
            lhs: operands[1].clone(),
            rhs: count,
            dst: in_bounds.clone(),
            ty: LirTy::Bit(8),
            span,
        });
        let (ok, fail) = (state.label(), state.label());
        state.end(LirTerm::BranchIf {
            cond: in_bounds,
            then: ok.clone(),
            els: Some(fail.clone()),
            span,
        });
        state.at(fail);
        let msg = self.byte_buffer_place(state, b"index out of bounds", span);
        self.panic_with(state, func, msg, span);
        state.at(ok);
        LirOperand::Place(result)
    }

    /// Lowers `@atomic_<op>_W(ref place, operands…)`: the operands become
    /// registers of the element type and the address a `ref` register.
    pub(crate) fn lower_atomic(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        op: AtomicOp,
        order: AtomicOrdering,
        args: &[HirExpr],
        span: Span,
    ) -> LirOperand {
        let mut operands = self.regs(state, func, args);
        let ptr = operands.remove(0);
        let ty = self.lir_ty(TypeRef::value(args[0].ty.id));
        if op == AtomicOp::Store {
            state.emit(LirInst::AtomicStore {
                value: operands[0].clone(),
                ptr,
                ty,
                order,
                span,
            });
            return LirOperand::Reg(Reg::default());
        }
        let dst = state.reg();
        let inst = match op {
            AtomicOp::Load => LirInst::AtomicLoad {
                ptr,
                dst: dst.clone(),
                ty,
                order,
                span,
            },
            AtomicOp::CompareSwap => LirInst::CmpXchg {
                ptr,
                expected: operands[0].clone(),
                desired: operands[1].clone(),
                dst: dst.clone(),
                ty,
                order,
                span,
            },
            _ => LirInst::AtomicRmw {
                op: rmw_op(op),
                ptr,
                value: operands[0].clone(),
                dst: dst.clone(),
                ty,
                order,
                span,
            },
        };
        state.emit(inst);
        LirOperand::Reg(dst)
    }
}

/// The LIR ordering of an atomic intrinsic's ordering.
pub(crate) fn atomic_ordering(ordering: Ordering) -> AtomicOrdering {
    match ordering {
        Ordering::Relaxed => AtomicOrdering::Relaxed,
        Ordering::Acquire => AtomicOrdering::Acquire,
        Ordering::Release => AtomicOrdering::Release,
        Ordering::AcqRel => AtomicOrdering::AcqRel,
        Ordering::SeqCst => AtomicOrdering::SeqCst,
    }
}

/// The `atomic.<op>` of a read-modify-write intrinsic.
pub(crate) fn rmw_op(op: AtomicOp) -> RmwOp {
    match op {
        AtomicOp::Add => RmwOp::Add,
        AtomicOp::Sub => RmwOp::Sub,
        AtomicOp::And => RmwOp::And,
        AtomicOp::Or => RmwOp::Or,
        AtomicOp::Xor => RmwOp::Xor,
        AtomicOp::Max => RmwOp::Max,
        AtomicOp::Min => RmwOp::Min,
        AtomicOp::MaxU => RmwOp::MaxU,
        AtomicOp::MinU => RmwOp::MinU,
        AtomicOp::Swap => RmwOp::Swap,
        AtomicOp::Load | AtomicOp::Store | AtomicOp::CompareSwap => {
            lilac_error::bug!(Span::dummy(), "`{op:?}` is not a read-modify-write")
        }
    }
}
