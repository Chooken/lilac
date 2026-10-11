//! Intrinsic calls: arithmetic and conversions, panics, `@array_at` and
//! atomics.

use lilac_hir::intrinsics::{AtomicOp, Intrinsic, NumOp, Ordering};
use lilac_hir::{HirExpr, HirExprKind, HirFunc, HirOld};
use lilac_ir::{
    ArithOp, AtomicOrdering, ConvertOp, LirCallee, LirConst, LirInst, LirOperand, LirTerm, LirTy,
    PlaceName, Reg, RmwOp,
};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeKind, TypeRef};

use crate::constant;
use crate::{BadDivision, BadIndex, DivisionPanic, FnState, Lower};

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
                    _ => self.native_bits(),
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
                let mut regs = self.regs(state, func, args);
                if !float && constant::is_division(&op) {
                    regs = self.check_division(state, func, &op, args, regs, span);
                }
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
            | Intrinsic::ExtendZero
            | Intrinsic::ExtendSign => {
                let op = match intrinsic {
                    Intrinsic::Truncate => ConvertOp::Trunc,
                    Intrinsic::ExtendZero => ConvertOp::ExtendZero,
                    Intrinsic::ExtendSign => ConvertOp::ExtendSign,
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
            // `@ptr_add` calls its leaf, an address computation the backend
            // keeps the pointer's provenance through (not integer arithmetic).
            Intrinsic::PtrAdd
            | Intrinsic::Memcpy
            | Intrinsic::Memmove
            | Intrinsic::Memset
            | Intrinsic::LinuxSyscall
            | Intrinsic::LinuxThreadClone
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
            Intrinsic::ArrayLen | Intrinsic::Move | Intrinsic::NeedsDrop => {
                lilac_error::bug!(span, "`{intrinsic:?}` is rewritten in typeck")
            }
            Intrinsic::Write => {
                // An assignment into an empty slot: nothing there drops.
                let target = deref(&args[0]);
                self.lower_assign(state, func, &target, &args[1], &HirOld::Empty(Vec::new()));
                LirOperand::Reg(Reg::default())
            }
            Intrinsic::Read => {
                // The bits move out, with no copy glue.
                let moved = HirExpr {
                    ty,
                    span,
                    kind: HirExprKind::Move(Box::new(deref(&args[0]))),
                };
                self.value(state, func, &moved)
            }
            Intrinsic::Drop => {
                let target = deref(&args[0]);
                let place = self.place_of(state, func, &target);
                self.drop_place(state, &place, target.ty, span);
                LirOperand::Reg(Reg::default())
            }
            // The ownership pass already turned its fields into places.
            Intrinsic::Discard => LirOperand::Reg(Reg::default()),
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

    /// Emits, before the integer division or remainder `op` of `regs` (the
    /// lowered `args`), the tests that keep it defined: a zero divisor
    /// panics with "division by zero", and for a signed `op`, `MIN` with
    /// `-1` panics with "division overflow". A test the constant operands
    /// rule out is not emitted, so the backend never divides by zero or
    /// overflows. Constant operands that make it panic are `E060`
    /// ([`BadDivision`]). Returns the operands in the block that divides
    /// (registers do not cross blocks, so a tested pair is kept in places).
    fn check_division(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        op: &ArithOp,
        args: &[HirExpr],
        regs: Vec<Reg>,
        span: Span,
    ) -> Vec<Reg> {
        let lir = &self.lir_ty(args[1].ty);
        let width = match lir {
            LirTy::Bit(bits) => u32::from(*bits),
            _ => self.native_bits(),
        };
        let mask = constant::mask_of(width);
        let min = 1u64 << (width - 1);
        let mut known = |expr: &HirExpr| match self.constant(expr) {
            Some(constant::Value::Int(bits, _)) => Some(bits & mask),
            _ => None,
        };
        let divisor = known(&args[1]);
        let dividend = known(&args[0]);
        if let Some(panic) =
            divisor.and_then(|right| constant::division_panic(op, dividend, right, width))
        {
            self.bad_divisions.push(BadDivision {
                func: Some(self.symbols[&func.key].as_str().to_owned()),
                span,
                panic,
            });
        }
        let by_zero = divisor.is_none_or(|right| right == 0);
        let overflow = matches!(op, ArithOp::IDiv | ArithOp::IMod)
            && divisor.is_none_or(|right| right == mask)
            && dividend.is_none_or(|left| left == min);
        if !by_zero && !overflow {
            return regs;
        }
        let places: Vec<PlaceName> = regs
            .iter()
            .map(|reg| {
                let place = state.tmp_lir(lir.clone(), span);
                state.emit(LirInst::CopyPlace {
                    src: LirOperand::Reg(reg.clone()),
                    dst: place.clone(),
                    span,
                });
                place
            })
            .collect();
        let reload = |state: &mut FnState| -> Vec<Reg> {
            places
                .iter()
                .map(|place| state.reg_of(LirOperand::Place(place.clone()), lir, span))
                .collect()
        };
        // `operands` are the registers valid in the current block.
        let mut operands = regs;
        if by_zero {
            let zero = equals_const(state, &operands[1], 0, lir, span);
            self.panic_when(state, func, zero, DivisionPanic::ByZero, span);
            operands = reload(state);
        }
        if overflow {
            let is_min = equals_const(state, &operands[0], min, lir, span);
            let minus_one = equals_const(state, &operands[1], mask, lir, span);
            let both = state.reg();
            state.emit(LirInst::Arith {
                op: ArithOp::And,
                lhs: is_min,
                rhs: minus_one,
                dst: both.clone(),
                ty: LirTy::Bit(8),
                span,
            });
            self.panic_when(state, func, both, DivisionPanic::Overflow, span);
            operands = reload(state);
        }
        operands
    }

    /// Panics with `panic`'s message when `cond` (a `#bit8`) is nonzero;
    /// emission continues where it is zero.
    fn panic_when(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        cond: Reg,
        panic: DivisionPanic,
        span: Span,
    ) {
        let (fail, ok) = (state.label(), state.label());
        state.end(LirTerm::BranchIf {
            cond,
            then: fail.clone(),
            els: Some(ok.clone()),
            span,
        });
        state.at(fail);
        let msg = self.byte_buffer_place(state, panic.message().as_bytes(), span);
        self.panic_with(state, func, msg, span);
        state.at(ok);
    }

    /// A fresh `#byte_buffer` place over the data constant `bytes`.
    pub(crate) fn byte_buffer_place(
        &mut self,
        state: &mut FnState,
        bytes: &[u8],
        span: Span,
    ) -> PlaceName {
        let name = self.intern_data(bytes);
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let place = self.tmp(state, Builtin::ByteBuffer.value(), span);
        let regs = (state.reg(), state.reg());
        for inst in lilac_ir::runtime::fill_byte_buffer(&place, name, len, regs, span) {
            state.emit(inst);
        }
        place
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

/// A `#bit8` register: whether `reg` (of type `lir`) equals `value`.
fn equals_const(state: &mut FnState, reg: &Reg, value: u64, lir: &LirTy, span: Span) -> Reg {
    let constant = state.reg();
    state.emit(LirInst::Const {
        dst: constant.clone(),
        value: LirConst::Int(value),
        ty: lir.clone(),
        span,
    });
    let test = state.reg();
    state.emit(LirInst::Arith {
        op: ArithOp::Eq,
        lhs: reg.clone(),
        rhs: constant,
        dst: test.clone(),
        ty: LirTy::Bit(8),
        span,
    });
    test
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

/// The place a `ref` operand points at.
fn deref(pointer: &HirExpr) -> HirExpr {
    HirExpr {
        ty: pointer.ty.deref(),
        span: pointer.span,
        kind: HirExprKind::Deref(Box::new(pointer.clone())),
    }
}
