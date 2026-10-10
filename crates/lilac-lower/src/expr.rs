//! Expressions: places and the values they read.

use lilac_hir::{HirCallee, HirExpr, HirExprKind, HirFunc, Layout};
use lilac_ir::{
    ArithOp, LirCallee, LirConst, LirData, LirInst, LirOperand, LirTy, PlaceName, Reg, Symbol,
};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeRef};

use crate::constant::number_const;
use crate::{is_fresh, FnState, Lower, Place};

impl Lower<'_> {
    pub(crate) fn place_of(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        expr: &HirExpr,
    ) -> Place {
        match &expr.kind {
            HirExprKind::Local(id) => match state.places.get(id) {
                Some(place) => Place::Direct(place.clone()),
                None => lilac_error::bug!(expr.span, "use of an undeclared local"),
            },
            HirExprKind::Deref(inner) => {
                if let HirExprKind::Local(id) = inner.kind {
                    if let Some(place) = state.places.get(&id) {
                        return Place::Indirect(place.clone());
                    }
                }
                let pointer = self.value(state, func, inner);
                Place::Indirect(self.fresh_place(state, pointer, inner.ty, expr.span))
            }
            HirExprKind::Field { base, index } => {
                let base_place = if base.is_place() {
                    self.place_of(state, func, base)
                } else {
                    let operand = self.value(state, func, base);
                    Place::Direct(self.spill(state, operand, base.ty, base.span))
                };
                let (name, _) = self.field_name(base.ty.id, *index);
                self.field_place(state, &base_place, *index, &name, expr.ty, expr.span)
            }
            _ => {
                let operand = self.value(state, func, expr);
                Place::Direct(self.spill(state, operand, expr.ty, expr.span))
            }
        }
    }

    /// The place of field `index` of the aggregate at `base`.
    pub(crate) fn field_place(
        &mut self,
        state: &mut FnState,
        base: &Place,
        index: u32,
        name: &str,
        ty: TypeRef,
        span: Span,
    ) -> Place {
        // A field that is itself a reference has a pointer-to-pointer address,
        // which LIR spells as the opaque `ref #unknown`.
        let pointer_ty = if ty.is_ref {
            LirTy::opaque_ref()
        } else {
            self.lir_ty(ty).to_ref()
        };
        let reg = state.reg();
        state.emit(LirInst::FieldAddr {
            obj: base.name().to_owned(),
            name: name.to_owned(),
            idx: index,
            dst: reg.clone(),
            ty: pointer_ty.clone(),
            span,
        });
        let holder = state.tmp_lir(pointer_ty, span);
        state.emit(LirInst::CopyPlace {
            src: LirOperand::Reg(reg),
            dst: holder.clone(),
            span,
        });
        Place::Indirect(holder)
    }

    /// A fresh `ref T` place holding the address of `place`.
    pub(crate) fn ref_place(
        &mut self,
        state: &mut FnState,
        place: &Place,
        ty: TypeRef,
        span: Span,
    ) -> PlaceName {
        let pointer = self.lir_ty(ty).to_ref();
        let reg = state.reg();
        match place {
            Place::Direct(name) => state.emit(LirInst::Addr {
                place: name.clone(),
                dst: reg.clone(),
                ty: pointer.clone(),
                span,
            }),
            Place::Indirect(name) => state.emit(LirInst::CopyReg {
                src: LirOperand::Place(name.clone()),
                dst: reg.clone(),
                ty: pointer.clone(),
                span,
            }),
        }
        let holder = state.tmp_lir(pointer, span);
        state.emit(LirInst::CopyPlace {
            src: LirOperand::Reg(reg),
            dst: holder.clone(),
            span,
        });
        holder
    }

    /// Evaluates an expression into a value that moves into a new owner:
    /// places are copied (hooks applied), fresh values are taken.
    pub(crate) fn read(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        expr: &HirExpr,
    ) -> LirOperand {
        if expr.is_place() && self.needs_copy(expr.ty) {
            let place = self.place_of(state, func, expr);
            return self.copy_out(state, &place, expr.ty, expr.span);
        }
        let operand = self.value(state, func, expr);
        if let LirOperand::Place(place) = &operand {
            if is_fresh(place) {
                Self::take_temp(state, place);
            }
        }
        operand
    }

    /// `read` into a fresh place (call arguments).
    pub(crate) fn arg(&mut self, state: &mut FnState, func: &HirFunc, expr: &HirExpr) -> PlaceName {
        let operand = self.read(state, func, expr);
        self.fresh_place(state, operand, expr.ty, expr.span)
    }

    pub(crate) fn value_reg(&mut self, state: &mut FnState, func: &HirFunc, expr: &HirExpr) -> Reg {
        let operand = self.value(state, func, expr);
        let lir = self.lir_ty(expr.ty);
        state.reg_of(operand, &lir, expr.span)
    }

    /// Registers for several operands, spilling earlier ones when a later one
    /// branches (registers do not cross blocks).
    pub(crate) fn regs(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        exprs: &[HirExpr],
    ) -> Vec<Reg> {
        let mut operands = Vec::with_capacity(exprs.len());
        for (index, expr) in exprs.iter().enumerate() {
            let operand = self.value(state, func, expr);
            let later_branches = exprs[index + 1..].iter().any(|later| self.branches(later));
            let operand = match operand {
                LirOperand::Reg(_) if later_branches => {
                    let place = self.tmp(state, expr.ty, expr.span);
                    state.emit(LirInst::CopyPlace {
                        src: operand,
                        dst: place.clone(),
                        span: expr.span,
                    });
                    LirOperand::Place(place)
                }
                other => other,
            };
            operands.push(operand);
        }
        operands
            .into_iter()
            .zip(exprs)
            .map(|(operand, expr)| {
                let lir = self.lir_ty(expr.ty);
                state.reg_of(operand, &lir, expr.span)
            })
            .collect()
    }

    /// Evaluates an expression without taking ownership: a fresh aggregate
    /// value stays registered for its statement's drop.
    pub(crate) fn value(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        expr: &HirExpr,
    ) -> LirOperand {
        let span = expr.span;
        let lir = self.lir_ty(expr.ty);
        match &expr.kind {
            HirExprKind::Local(id) => match state.places.get(id) {
                Some(place) => LirOperand::Place(place.clone()),
                None => lilac_error::bug!(span, "use of an undeclared local"),
            },
            HirExprKind::Field { base, index } if base.is_place() => {
                self.field_value(state, func, base, *index, lir, span)
            }
            HirExprKind::Field { .. } | HirExprKind::Deref(_) => {
                let place = self.place_of(state, func, expr);
                state.read_place(&place, &lir, span)
            }
            HirExprKind::Number(text) => state.constant(number_const(text, &lir, span), lir, span),
            HirExprKind::Str(bytes) => {
                let name = self.intern_data(bytes);
                state.constant(LirConst::Data(name), LirTy::byte_buffer(), span)
            }
            HirExprKind::Struct(values) => self.struct_value(state, func, expr, values),
            HirExprKind::Variant { index, payload } => {
                self.variant_value(state, func, expr, *index, payload.as_deref())
            }
            HirExprKind::Call { callee, args } => self.call_value(state, func, expr, callee, args),
            HirExprKind::FuncRef(key) => {
                let reg = state.reg();
                state.emit(LirInst::AddrFunc {
                    func: self.symbol(key),
                    dst: reg.clone(),
                    ty: LirTy::opaque_ref(),
                    span,
                });
                LirOperand::Reg(reg)
            }
            HirExprKind::RefOf(inner) => self.ref_value(state, func, inner, lir, span),
            HirExprKind::Retype(inner) => {
                let operand = self.value(state, func, inner);
                let reg = state.reg();
                state.emit(LirInst::CopyReg {
                    src: operand,
                    dst: reg.clone(),
                    ty: lir,
                    span,
                });
                LirOperand::Reg(reg)
            }
            HirExprKind::Eq(lhs, rhs) => {
                let regs = self.regs(state, func, &[(**lhs).clone(), (**rhs).clone()]);
                let reg = state.reg();
                state.emit(LirInst::Arith {
                    op: ArithOp::Eq,
                    lhs: regs[0].clone(),
                    rhs: regs[1].clone(),
                    dst: reg.clone(),
                    ty: LirTy::Bit(8),
                    span,
                });
                LirOperand::Reg(reg)
            }
            HirExprKind::If { cond, then, els } => {
                self.if_value(state, func, expr, cond, then, els)
            }
            HirExprKind::Match {
                scrutinee,
                arms,
                els,
            } => self.lower_match_value(state, func, expr, scrutinee, arms, els),
            HirExprKind::Logic { and, lhs, rhs } => {
                self.logic_value(state, func, expr, *and, lhs, rhs)
            }
            HirExprKind::FormatArgs(pieces) => self.format_args_value(state, func, expr, pieces),
            HirExprKind::Const(def) => match self.hir.consts.get(def) {
                Some(value) => self.value(state, func, value),
                None => lilac_error::bug!(span, "use of an unchecked constant"),
            },
            HirExprKind::SizeOf(ty) => {
                let (size, _) = self.size_align(*ty);
                state.constant(LirConst::Int(size), lir, span)
            }
            HirExprKind::AlignOf(ty) => {
                let (_, align) = self.size_align(*ty);
                state.constant(LirConst::Int(align), lir, span)
            }
            HirExprKind::CompileError { msg, msg_span } => {
                state.emit(LirInst::CompileError {
                    msg: msg.clone(),
                    msg_span: *msg_span,
                    call_span: span,
                });
                let ty = if expr.ty.is_error() {
                    Builtin::Bit8.value()
                } else {
                    expr.ty
                };
                LirOperand::Place(self.tmp(state, ty, span))
            }
            HirExprKind::Poison => lilac_error::bug!(span, "lowering poisoned HIR"),
        }
    }

    /// The size and alignment of `ty`, in bytes.
    fn size_align(&self, ty: TypeRef) -> (u64, u64) {
        let lir = self.lir_ty(ty);
        lilac_ir::size_align(&self.module.layouts, &lir, self.ptr_bytes)
    }

    /// A field of a place, read straight out of it.
    fn field_value(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        base: &HirExpr,
        index: u32,
        lir: LirTy,
        span: Span,
    ) -> LirOperand {
        let base_place = self.place_of(state, func, base);
        let (name, _) = self.field_name(base.ty.id, index);
        let reg = state.reg();
        state.emit(LirInst::Field {
            obj: base_place.name().to_owned(),
            name,
            idx: index,
            dst: reg.clone(),
            ty: lir,
            span,
        });
        LirOperand::Reg(reg)
    }

    /// A call's result: an intrinsic's value, or a temporary the call
    /// writes (none for a call of error type).
    fn call_value(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        expr: &HirExpr,
        callee: &HirCallee,
        args: &[HirExpr],
    ) -> LirOperand {
        let span = expr.span;
        if let HirCallee::Intrinsic(intrinsic) = callee {
            return self.lower_intrinsic(state, func, *intrinsic, args, expr.ty, span);
        }
        if expr.ty.is_error() {
            self.emit_call(state, func, callee, args, &[], span);
            return LirOperand::Reg(Reg::default());
        }
        let out = self.tmp(state, expr.ty, span);
        self.emit_call(state, func, callee, args, std::slice::from_ref(&out), span);
        self.own_temp(state, &out, expr.ty);
        LirOperand::Place(out)
    }

    /// An object literal: a zeroed temporary with each field stored.
    fn struct_value(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        expr: &HirExpr,
        values: &[HirExpr],
    ) -> LirOperand {
        let span = expr.span;
        let place = self.tmp(state, expr.ty, span);
        state.emit(LirInst::Zero {
            place: place.clone(),
            span,
        });
        for (index, value) in values.iter().enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            let (name, _) = self.field_name(expr.ty.id, index);
            let operand = self.read(state, func, value);
            state.emit(LirInst::StoreField {
                value: operand,
                obj: place.clone(),
                name,
                idx: index,
                span: value.span,
            });
        }
        self.own_temp(state, &place, expr.ty);
        LirOperand::Place(place)
    }

    /// A variant: a zeroed temporary with its tag, and payload if any.
    fn variant_value(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        expr: &HirExpr,
        index: u32,
        payload: Option<&HirExpr>,
    ) -> LirOperand {
        let span = expr.span;
        let place = self.tmp(state, expr.ty, span);
        state.emit(LirInst::Zero {
            place: place.clone(),
            span,
        });
        let Some(Layout::Enum { tag, .. }) = self.layout(expr.ty.id) else {
            lilac_error::bug!(span, "variant of a non-enum");
        };
        let tag_ty = self.lir_ty(TypeRef::value(*tag));
        let reg = state.reg();
        state.emit(LirInst::Const {
            dst: reg.clone(),
            value: LirConst::Int(u64::from(index)),
            ty: tag_ty,
            span,
        });
        state.emit(LirInst::StoreField {
            value: LirOperand::Reg(reg),
            obj: place.clone(),
            name: "tag".to_owned(),
            idx: 0,
            span,
        });
        if let Some(payload) = payload {
            let (field, name) = self.payload_field(expr.ty.id, index);
            let operand = self.read(state, func, payload);
            state.emit(LirInst::StoreField {
                value: operand,
                obj: place.clone(),
                name,
                idx: field,
                span,
            });
        }
        self.own_temp(state, &place, expr.ty);
        LirOperand::Place(place)
    }

    /// `ref place`: the place's address.
    fn ref_value(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        inner: &HirExpr,
        lir: LirTy,
        span: Span,
    ) -> LirOperand {
        let place = self.place_of(state, func, inner);
        let reg = state.reg();
        match &place {
            Place::Direct(name) => state.emit(LirInst::Addr {
                place: name.clone(),
                dst: reg.clone(),
                ty: lir,
                span,
            }),
            Place::Indirect(name) => state.emit(LirInst::CopyReg {
                src: LirOperand::Place(name.clone()),
                dst: reg.clone(),
                ty: lir,
                span,
            }),
        }
        LirOperand::Reg(reg)
    }

    /// A `format_args` table: created, then each piece pushed.
    fn format_args_value(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        expr: &HirExpr,
        pieces: &[HirExpr],
    ) -> LirOperand {
        let span = expr.span;
        let place = self.tmp(state, expr.ty, span);
        state.emit(LirInst::Call {
            func: LirCallee::Func(".lilac.format_args_new".into()),
            ins: Vec::new(),
            outs: vec![place.clone()],
            span,
        });
        for piece in pieces {
            let piece_place = self.arg(state, func, piece);
            let table = self.ref_place(state, &Place::Direct(place.clone()), expr.ty, span);
            state.emit(LirInst::Call {
                func: LirCallee::Func(".lilac.format_args_push".into()),
                ins: vec![table, piece_place],
                outs: Vec::new(),
                span: piece.span,
            });
        }
        LirOperand::Place(place)
    }

    pub(crate) fn intern_data(&mut self, bytes: &[u8]) -> Symbol {
        if let Some(existing) = self.module.data.iter().find(|data| data.bytes == bytes) {
            return existing.name.clone();
        }
        let name = Symbol::new(format!("c{}", self.module.data.len()));
        self.module.data.push(LirData {
            name: name.clone(),
            bytes: bytes.to_vec(),
            span: Span::dummy(),
        });
        name
    }
}
