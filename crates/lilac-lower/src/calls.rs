//! Calls: Lilac calls and `extern` calls through the C ABI.

use lilac_hir::{FuncKey, HirCallee, HirExpr, HirFunc, Layout};
use lilac_ir::{LirCallee, LirInst, LirOperand, LirTy, PlaceName, Symbol};
use lilac_span::Span;
use lilac_ty::{TypeKind, TypeRef};

use crate::symbols::c_ty;
use crate::{BadPanic, FnState, Lower};

impl Lower<'_> {
    /// The C type an `extern` slot uses ([`Self::abi_slot`] with every
    /// `ref` as the opaque `ref #unknown`: C sees only a pointer, so two
    /// declarations of one symbol may differ in pointee).
    pub(crate) fn abi_ty(&self, ty: TypeRef) -> (LirTy, bool) {
        let (slot, wrapped) = self.abi_slot(ty);
        (c_ty(slot), wrapped)
    }

    /// The Lilac type an `extern` slot carries: the type itself, or for an
    /// object wrapping exactly one scalar field (a bit integer, a float or a
    /// `ref`), that field (`true`). Any other object passes as a C struct.
    pub(crate) fn abi_slot(&self, ty: TypeRef) -> (LirTy, bool) {
        let lir = self.lir_ty(ty);
        if !ty.is_ref {
            if let Some(Layout::Object { fields }) = self.hir.layouts.get(&ty.id) {
                if let [(_, field)] = fields.as_slice() {
                    let inner = self.lir_ty(*field);
                    if inner.is_scalar() {
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
            // A `=>` body that divides by a constant argument (`a / 0` through
            // an operator hook) panics at this call: `E060` here, since the
            // body itself sees only parameters.
            if let Some(panic) = self.inline_call_panic(key, args) {
                self.bad_panics.push(BadPanic {
                    func: Some(self.symbols[&func.key].as_str().to_owned()),
                    span,
                    panic,
                });
            }
        }
        if let HirCallee::Value(value) = callee {
            // The callee is read first, into a place: the arguments may
            // branch, and a register lives in its block.
            let place = self.arg(state, func, value);
            let TypeKind::Func { returns, c_abi, .. } = self.types.kind(value.ty.id) else {
                lilac_error::bug!(
                    span,
                    "a call through a value that is not a function reference"
                );
            };
            if *c_abi {
                let returns = returns.clone();
                self.emit_c_call(
                    state,
                    func,
                    Callee::Place(place),
                    args,
                    outs,
                    &returns,
                    span,
                );
            } else {
                let ins: Vec<PlaceName> =
                    args.iter().map(|arg| self.arg(state, func, arg)).collect();
                let reg = state.reg_of(LirOperand::Place(place), &LirTy::opaque_ref(), span);
                state.emit(LirInst::Call {
                    func: LirCallee::Reg(reg),
                    ins,
                    outs: outs.to_vec(),
                    span,
                });
            }
            return;
        }
        let ins: Vec<PlaceName> = args.iter().map(|arg| self.arg(state, func, arg)).collect();
        let target = match callee {
            HirCallee::Func(key) => LirCallee::Func(self.symbol(key)),
            HirCallee::Value(_) => {
                lilac_error::bug!(span, "a value callee in the direct call path")
            }
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
        let target = Callee::Func(symbol);
        self.emit_c_call(state, func, target, args, outs, &returns, span);
    }

    /// A C ABI call to `target` returning `returns`: wrapper objects pass
    /// (and return) their single field.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_c_call(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        target: Callee,
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
        let func_target = match target {
            Callee::Func(symbol) => LirCallee::Func(symbol),
            Callee::Place(place) => {
                LirCallee::CReg(state.reg_of(LirOperand::Place(place), &LirTy::opaque_ref(), span))
            }
        };
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
            func: func_target,
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

/// What a C ABI call ([`Lower::emit_c_call`]) calls.
pub(crate) enum Callee {
    /// A function by symbol.
    Func(Symbol),
    /// The C function address in a place, read after the arguments (they
    /// may branch, and a register lives in its block).
    Place(PlaceName),
}
