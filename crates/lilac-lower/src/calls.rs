//! Calls: Lilac calls and `extern` calls through the C ABI.

use lilac_hir::{FuncKey, HirCallee, HirExpr, HirFunc, Layout, LocalId};
use lilac_ir::{LirCallee, LirInst, LirOperand, LirTy, PlaceName, Reg};
use lilac_span::Span;
use lilac_ty::{TypeKind, TypeRef};

use crate::symbols::c_ty;
use crate::{FnState, Lower};

impl Lower<'_> {
    /// The C type an `extern` slot uses ([`Self::abi_slot`] with every
    /// `ref` as the opaque `ref #unknown`: C sees only a pointer, so two
    /// declarations of one symbol may differ in pointee).
    pub(crate) fn abi_ty(&self, ty: TypeRef) -> (LirTy, bool) {
        let (slot, wrapped) = self.abi_slot(ty);
        (c_ty(slot), wrapped)
    }

    /// The Lilac type an `extern` slot carries: the type itself, or for an
    /// object wrapping exactly one C-representable field, that field
    /// (`true`).
    pub(crate) fn abi_slot(&self, ty: TypeRef) -> (LirTy, bool) {
        let lir = self.lir_ty(ty);
        if !ty.is_ref {
            if let Some(Layout::Object { fields }) = self.hir.layouts.get(&ty.id) {
                if let [(_, field)] = fields.as_slice() {
                    let inner = self.lir_ty(*field);
                    if lilac_ir::is_c_abi_type(&inner) {
                        return (inner, true);
                    }
                }
            }
        }
        (lir, false)
    }

    pub(crate) fn emit_call(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        callee: &HirCallee,
        args: &[HirExpr],
        outs: &[PlaceName],
        span: Span,
    ) {
        if let HirCallee::Func(key) = callee {
            if self.externs.contains(key) {
                self.emit_extern_call(state, func, key, args, outs, span);
                return;
            }
        }
        if let HirCallee::Local(id) = callee {
            if let TypeKind::Func {
                returns,
                c_abi: true,
                ..
            } = self.types.kind(func.local(*id).ty.id)
            {
                let returns = returns.clone();
                let reg = callee_reg(state, *id, span);
                self.emit_c_call(
                    state,
                    func,
                    LirCallee::CReg(reg),
                    args,
                    outs,
                    &returns,
                    span,
                );
                return;
            }
        }
        let ins: Vec<PlaceName> = args.iter().map(|arg| self.arg(state, func, arg)).collect();
        let target = match callee {
            HirCallee::Func(key) => LirCallee::Func(self.symbol(key)),
            HirCallee::Local(id) => LirCallee::Reg(callee_reg(state, *id, span)),
            HirCallee::Intrinsic(_) => {
                lilac_error::bug!(span, "intrinsic in the general call path")
            }
        };
        state.emit(LirInst::Call {
            func: target,
            ins,
            outs: outs.to_vec(),
            span,
        });
    }

    /// A call to an `extern`, with the C ABI ([`Self::emit_c_call`]).
    pub(crate) fn emit_extern_call(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        key: &FuncKey,
        args: &[HirExpr],
        outs: &[PlaceName],
        span: Span,
    ) {
        let symbol = self.symbol(key);
        let returns: Vec<TypeRef> = self
            .hir
            .funcs
            .iter()
            .find(|candidate| candidate.key == *key)
            .map(|callee| {
                callee
                    .returns
                    .iter()
                    .map(|id| callee.local(*id).ty)
                    .collect()
            })
            .unwrap_or_default();
        let target = LirCallee::Func(symbol);
        self.emit_c_call(state, func, target, args, outs, &returns, span);
    }

    /// A C ABI call to `target` returning `returns`: wrapper objects pass
    /// (and return) their single field.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_c_call(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        target: LirCallee,
        args: &[HirExpr],
        outs: &[PlaceName],
        returns: &[TypeRef],
        span: Span,
    ) {
        let mut ins = Vec::with_capacity(args.len());
        for arg in args {
            let place = self.arg(state, func, arg);
            let (slot, wrapped) = self.abi_slot(arg.ty);
            let abi = c_ty(slot.clone());
            if !wrapped && abi == slot {
                ins.push(place);
                continue;
            }
            let mut reg = if wrapped {
                let (name, _) = self.field_name(arg.ty.id, 0);
                let reg = state.reg();
                state.emit(LirInst::Field {
                    obj: place,
                    name,
                    idx: 0,
                    dst: reg.clone(),
                    ty: slot.clone(),
                    span,
                });
                reg
            } else {
                state.reg_of(LirOperand::Place(place), &slot, span)
            };
            if abi != slot {
                reg = state.bitcast(reg, abi.clone(), span);
            }
            let inner = state.tmp_lir(abi, span);
            state.emit(LirInst::CopyPlace {
                src: LirOperand::Reg(reg),
                dst: inner.clone(),
                span,
            });
            ins.push(inner);
        }
        let mut call_outs = Vec::with_capacity(outs.len());
        let mut rewrap = Vec::new();
        for (out, ty) in outs.iter().zip(returns) {
            let (slot, wrapped) = self.abi_slot(*ty);
            let abi = c_ty(slot.clone());
            if wrapped || abi != slot {
                let inner = state.tmp_lir(abi.clone(), span);
                rewrap.push((out.clone(), inner.clone(), *ty, slot, abi, wrapped));
                call_outs.push(inner);
            } else {
                call_outs.push(out.clone());
            }
        }
        state.emit(LirInst::Call {
            func: target,
            ins,
            outs: call_outs,
            span,
        });
        for (out, inner, ty, slot, abi, wrapped) in rewrap {
            let value = if abi == slot {
                LirOperand::Place(inner)
            } else {
                let reg = state.reg_of(LirOperand::Place(inner), &abi, span);
                LirOperand::Reg(state.bitcast(reg, slot, span))
            };
            if wrapped {
                let (name, _) = self.field_name(ty.id, 0);
                state.emit(LirInst::StoreField {
                    value,
                    obj: out,
                    name,
                    idx: 0,
                    span,
                });
            } else {
                state.emit(LirInst::CopyPlace {
                    src: value,
                    dst: out,
                    span,
                });
            }
        }
    }
}

/// The function address a local holds, in a register.
fn callee_reg(state: &mut FnState, id: LocalId, span: Span) -> Reg {
    let Some(place) = state.places.get(&id).cloned() else {
        lilac_error::bug!(span, "call through an undeclared local");
    };
    let reg = state.reg();
    state.emit(LirInst::CopyReg {
        src: LirOperand::Place(place),
        dst: reg.clone(),
        ty: LirTy::opaque_ref(),
        span,
    });
    reg
}
