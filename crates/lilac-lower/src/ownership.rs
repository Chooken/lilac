//! Ownership: the drops and drop flags typeck decided, temporaries,
//! copies, and the drop and copy glue routines.

use lilac_hir::{HirDrop, HirExpr, HirFunc, HirPath, HirPattern, Layout};
use lilac_ir::{
    Label, LirCallee, LirConst, LirInst, LirOperand, LirTerm, LirTy, PlaceName, Symbol,
};
use lilac_span::Span;
use lilac_ty::TypeRef;

use crate::{is_fresh, FnState, Lower, Place};

impl Lower<'_> {
    pub(crate) fn needs_drop(&mut self, ty: TypeRef) -> bool {
        if ty.is_ref {
            return false;
        }
        if let Some(known) = self.needs_drop.get(&ty.id) {
            return *known;
        }
        self.needs_drop.insert(ty.id, false);
        let hooked = self
            .hir
            .hooks
            .get(&ty.id)
            .is_some_and(|hooks| hooks.drop.is_some());
        let result = hooked
            || self
                .slot_types(ty.id)
                .into_iter()
                .any(|slot| self.needs_drop(slot));
        self.needs_drop.insert(ty.id, result);
        result
    }

    pub(crate) fn needs_copy(&mut self, ty: TypeRef) -> bool {
        if ty.is_ref {
            return false;
        }
        if let Some(known) = self.needs_copy.get(&ty.id) {
            return *known;
        }
        self.needs_copy.insert(ty.id, false);
        let hooked = self
            .hir
            .hooks
            .get(&ty.id)
            .is_some_and(|hooks| hooks.copy.is_some());
        let result = hooked
            || self
                .slot_types(ty.id)
                .into_iter()
                .any(|slot| self.needs_copy(slot));
        self.needs_copy.insert(ty.id, result);
        result
    }

    /// The place an owned path names: its local, then each field.
    pub(crate) fn path_place(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        path: &HirPath,
        span: Span,
    ) -> Place {
        let Some(name) = state.places.get(&path.local).cloned() else {
            lilac_error::bug!(span, "a path through an undeclared local");
        };
        let mut place = Place::Direct(name);
        let mut ty = func.local(path.local).ty;
        for field in &path.fields {
            let (name, field_ty) = self.field_name(ty.id, *field);
            place = self.field_place(state, &place, *field, &name, field_ty, span);
            ty = field_ty;
        }
        place
    }

    /// Sets (`on`) or clears the drop flag of every place inside `path`:
    /// initializing a place sets them, moving it out clears them.
    pub(crate) fn mark(state: &mut FnState, func: &HirFunc, path: &HirPath, on: bool, span: Span) {
        for (flagged, flag) in &func.flags {
            if !flagged.within(path) {
                continue;
            }
            let Some(place) = state.places.get(flag).cloned() else {
                lilac_error::bug!(span, "an undeclared drop flag");
            };
            let value = state.constant(LirConst::Int(u64::from(on)), LirTy::Bit(8), span);
            state.emit(LirInst::CopyPlace {
                src: value,
                dst: place,
                span,
            });
        }
    }

    /// [`Self::mark`] for the place `expr` names, when it is an owned path.
    pub(crate) fn mark_expr(state: &mut FnState, func: &HirFunc, expr: &HirExpr, on: bool) {
        if func.flags.is_empty() {
            return;
        }
        if let Some(path) = HirPath::of(expr) {
            Self::mark(state, func, &path, on, expr.span);
        }
    }

    /// Drops each of `drops` (a flagged one only when its flag is set).
    pub(crate) fn lower_drops(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        drops: &[HirDrop],
        span: Span,
    ) {
        for drop in drops {
            let skip = drop.flag.map(|flag| {
                let Some(place) = state.places.get(&flag).cloned() else {
                    lilac_error::bug!(span, "an undeclared drop flag");
                };
                let operand = state.read_place(&Place::Direct(place), &LirTy::Bit(8), span);
                let cond = state.reg_of(operand, &LirTy::Bit(8), span);
                let (body, skip) = (state.label(), state.label());
                state.end(LirTerm::BranchIf {
                    cond,
                    then: body.clone(),
                    els: Some(skip.clone()),
                    span,
                });
                state.at(body);
                skip
            });
            let place = self.path_place(state, func, &drop.path, span);
            self.drop_place(state, &place, drop.ty, span);
            if let Some(skip) = skip {
                state.branch(&skip, span);
                state.at(skip);
            }
        }
    }

    /// A fresh temporary place of type `ty`.
    pub(crate) fn tmp(&mut self, state: &mut FnState, ty: TypeRef, span: Span) -> PlaceName {
        let lir = self.lir_ty(ty);
        state.tmp_lir(lir, span)
    }

    /// Registers a fresh value as owned until the end of the statement.
    pub(crate) fn own_temp(&mut self, state: &mut FnState, place: &PlaceName, ty: TypeRef) {
        if self.needs_drop(ty) {
            if let Some(scope) = state.scopes.last_mut() {
                scope.temps.push((place.clone(), ty));
            }
        }
    }

    /// Marks a fresh value as taken (it moved into a new owner).
    pub(crate) fn take_temp(state: &mut FnState, place: &PlaceName) {
        for scope in state.scopes.iter_mut().rev() {
            if let Some(index) = scope.temps.iter().position(|(name, _)| name == place) {
                scope.temps.remove(index);
                return;
            }
        }
    }

    /// Drops the temporaries scope `index` still owns, newest first; `span`
    /// is where the scope ends (its closing `}`, or the `return`/`break`
    /// leaving it). Locals drop where typeck's `HirStmt::Drop`s say.
    pub(crate) fn drop_scope(&mut self, state: &mut FnState, index: usize, span: Span) {
        let owned: Vec<(PlaceName, TypeRef)> =
            state.scopes[index].temps.iter().rev().cloned().collect();
        for (place, ty) in owned {
            self.drop_place(state, &Place::Direct(place), ty, span);
        }
    }

    /// Stores an operand into a new temp place (registered as owned).
    pub(crate) fn spill(
        &mut self,
        state: &mut FnState,
        operand: LirOperand,
        ty: TypeRef,
        span: Span,
    ) -> PlaceName {
        if let LirOperand::Place(place) = &operand {
            if is_fresh(place) {
                return place.clone();
            }
        }
        let place = self.tmp(state, ty, span);
        state.emit(LirInst::CopyPlace {
            src: operand,
            dst: place.clone(),
            span,
        });
        place
    }

    /// An operand as a fresh place the caller owns (not registered).
    pub(crate) fn fresh_place(
        &mut self,
        state: &mut FnState,
        operand: LirOperand,
        ty: TypeRef,
        span: Span,
    ) -> PlaceName {
        if let LirOperand::Place(place) = &operand {
            if is_fresh(place) {
                Self::take_temp(state, place);
                return place.clone();
            }
        }
        let place = self.tmp(state, ty, span);
        state.emit(LirInst::CopyPlace {
            src: operand,
            dst: place.clone(),
            span,
        });
        place
    }

    /// A copy of the value at `place` (copy hooks applied).
    pub(crate) fn copy_out(
        &mut self,
        state: &mut FnState,
        place: &Place,
        ty: TypeRef,
        span: Span,
    ) -> LirOperand {
        let lir = self.lir_ty(ty);
        if !self.needs_copy(ty) {
            return state.read_place(place, &lir, span);
        }
        let source = self.ref_place(state, place, ty, span);
        let out = self.tmp(state, ty, span);
        let routine = self.copy_routine(ty);
        state.emit(LirInst::Call {
            func: LirCallee::Func(routine),
            ins: vec![source],
            outs: vec![out.clone()],
            span,
        });
        LirOperand::Place(out)
    }

    /// Drops the value at `place`.
    pub(crate) fn drop_place(
        &mut self,
        state: &mut FnState,
        place: &Place,
        ty: TypeRef,
        span: Span,
    ) {
        if !self.needs_drop(ty) {
            return;
        }
        let pointer = self.ref_place(state, place, ty, span);
        let routine = self.drop_routine(ty);
        state.emit(LirInst::Call {
            func: LirCallee::Func(routine),
            ins: vec![pointer],
            outs: Vec::new(),
            span,
        });
        if let Place::Direct(name) = place {
            state.emit(LirInst::Drop {
                place: name.clone(),
                span,
            });
        }
    }

    /// The function dropping a `T` through a `ref T`: its `@on_drop` when no
    /// field needs dropping, else generated glue.
    pub(crate) fn drop_routine(&mut self, ty: TypeRef) -> Symbol {
        if let Some(name) = self.drop_glue.get(&ty.id) {
            return name.clone();
        }
        let hook = self
            .hir
            .hooks
            .get(&ty.id)
            .and_then(|hooks| hooks.drop.clone());
        let slots = self.slot_types(ty.id);
        let fields_drop = slots.iter().any(|slot| self.needs_drop(*slot));
        if let (Some(hook), false) = (&hook, fields_drop) {
            let symbol = self.symbol(hook);
            self.drop_glue.insert(ty.id, symbol.clone());
            return symbol;
        }
        let name = Symbol::new(format!(".lilac.drop.{}", self.type_name(ty.id)));
        self.drop_glue.insert(ty.id, name.clone());
        let mut state = FnState::new();
        let lir = self.lir_ty(ty);
        state.ins.push(("self".into(), lir.clone().to_ref()));
        state.place_tys.insert("self".into(), lir.clone().to_ref());
        state.used.insert("self".into());
        let span = Span::dummy();
        let this = Place::Indirect("self".into());
        if let Some(hook) = hook {
            let symbol = self.symbol(&hook);
            state.emit(LirInst::Call {
                func: LirCallee::Func(symbol),
                ins: vec!["self".into()],
                outs: Vec::new(),
                span,
            });
        }
        self.for_each_slot(&mut state, &this, ty, |lower, state, slot, slot_ty| {
            if lower.needs_drop(slot_ty) {
                lower.drop_place(state, slot, slot_ty, span);
            }
        });
        let func = state.finish(name.clone(), false, span);
        self.module.funcs.push(func);
        name
    }

    /// The function copying a `T` from a `ref T` into an out place: its
    /// `@on_copy`, else glue copying field by field.
    pub(crate) fn copy_routine(&mut self, ty: TypeRef) -> Symbol {
        if let Some(name) = self.copy_glue.get(&ty.id) {
            return name.clone();
        }
        if let Some(hook) = self
            .hir
            .hooks
            .get(&ty.id)
            .and_then(|hooks| hooks.copy.clone())
        {
            let symbol = self.symbol(&hook);
            self.copy_glue.insert(ty.id, symbol.clone());
            return symbol;
        }
        let name = Symbol::new(format!(".lilac.copy.{}", self.type_name(ty.id)));
        self.copy_glue.insert(ty.id, name.clone());
        let mut state = FnState::new();
        let lir = self.lir_ty(ty);
        for (place, slot_ty) in [("from", lir.clone().to_ref()), ("to", lir.clone())] {
            state.place_tys.insert(place.into(), slot_ty.clone());
            state.used.insert(place.into());
        }
        state.ins.push(("from".into(), lir.clone().to_ref()));
        state.outs.push(("to".into(), lir.clone()));
        let span = Span::dummy();
        let whole = state.read_place(&Place::Indirect("from".into()), &lir, span);
        state.emit(LirInst::CopyPlace {
            src: whole,
            dst: "to".into(),
            span,
        });
        let from = Place::Indirect("from".into());
        let to = Place::Direct("to".into());
        let pairs = self.slot_pairs(ty);
        for (field, name, slot_ty, variant) in pairs {
            if !self.needs_copy(slot_ty) {
                continue;
            }
            let skip =
                variant.map(|variant| self.variant_guard(&mut state, &from, ty, variant, span));
            let source = self.field_place(&mut state, &from, field, &name, slot_ty, span);
            let copied = self.copy_out(&mut state, &source, slot_ty, span);
            let target = self.field_place(&mut state, &to, field, &name, slot_ty, span);
            state.write(&target, copied, span);
            if let Some(skip) = skip {
                state.branch(&skip, span);
                state.at(skip);
            }
        }
        let func = state.finish(name.clone(), false, span);
        self.module.funcs.push(func);
        name
    }

    /// Every slot of an aggregate: `(field index, name, type, variant)` where
    /// `variant` is the enum variant that slot belongs to.
    pub(crate) fn slot_pairs(&self, ty: TypeRef) -> Vec<(u32, String, TypeRef, Option<u32>)> {
        match self.layout(ty.id) {
            Some(Layout::Object { fields }) => fields
                .iter()
                .enumerate()
                .map(|(index, (name, ty))| {
                    (
                        u32::try_from(index).unwrap_or(u32::MAX),
                        name.clone(),
                        *ty,
                        None,
                    )
                })
                .collect(),
            Some(Layout::Enum { variants, .. }) => {
                let mut out = Vec::new();
                for (variant, (name, payload)) in variants.iter().enumerate() {
                    if let Some(payload) = payload {
                        let variant = u32::try_from(variant).unwrap_or(u32::MAX);
                        let (field, _) = self.payload_field(ty.id, variant);
                        out.push((field, name.clone(), *payload, Some(variant)));
                    }
                }
                out
            }
            None => Vec::new(),
        }
    }

    /// Branches past the following code unless `place` holds `variant`;
    /// returns the label to continue at.
    pub(crate) fn variant_guard(
        &mut self,
        state: &mut FnState,
        place: &Place,
        ty: TypeRef,
        variant: u32,
        span: Span,
    ) -> Label {
        let test = self.pattern_test(state, place, ty, true, &HirPattern::Variant(variant), span);
        let (body, skip) = (state.label(), state.label());
        state.end(LirTerm::BranchIf {
            cond: test,
            then: body.clone(),
            els: Some(skip.clone()),
            span,
        });
        state.at(body);
        skip
    }

    /// Runs `f` on every slot place of the aggregate at `place` (enum payloads
    /// only when live).
    pub(crate) fn for_each_slot(
        &mut self,
        state: &mut FnState,
        place: &Place,
        ty: TypeRef,
        mut f: impl FnMut(&mut Self, &mut FnState, &Place, TypeRef),
    ) {
        for (field, name, slot_ty, variant) in self.slot_pairs(ty) {
            if !self.needs_drop(slot_ty) {
                continue;
            }
            let skip =
                variant.map(|variant| self.variant_guard(state, place, ty, variant, Span::dummy()));
            let slot = self.field_place(state, place, field, &name, slot_ty, Span::dummy());
            f(self, state, &slot, slot_ty);
            if let Some(skip) = skip {
                state.branch(&skip, Span::dummy());
                state.at(skip);
            }
        }
    }
}
