//! Statements and control flow: blocks, `if`, `match`, loops and
//! assignment, and the value forms of `if` and `match`.

use lilac_hir::intrinsics::Intrinsic;
use lilac_hir::{
    HirArm, HirBlock, HirCallee, HirExpr, HirExprKind, HirFunc, HirLocal, HirPattern, HirStmt,
    HirValueArm, Layout, LocalId,
};
use lilac_ir::{ArithOp, LirCallee, LirConst, LirInst, LirOperand, LirTerm, LirTy, PlaceName, Reg};
use lilac_span::Span;
use lilac_ty::TypeRef;

use crate::constant::number_const;
use crate::{end_of, stmt_span, FnState, Lower, Place, Scope};

impl Lower<'_> {
    pub(crate) fn lower_block(&mut self, state: &mut FnState, func: &HirFunc, block: &HirBlock) {
        state.scopes.push(Scope::default());
        for stmt in &block.stmts {
            self.lower_stmt(state, func, stmt);
        }
        if state.open {
            let index = state.scopes.len() - 1;
            self.drop_scope(state, index, end_of(block.span));
        }
        state.scopes.pop();
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn lower_stmt(&mut self, state: &mut FnState, func: &HirFunc, stmt: &HirStmt) {
        let scope = state.scopes.len() - 1;
        let mark = state.scopes[scope].temps.len();
        match stmt {
            HirStmt::Let { local, value } => {
                let place = self.declare(state, func, *local);
                let operand = self.read(state, func, value);
                state.emit(LirInst::CopyPlace {
                    src: operand,
                    dst: place.clone(),
                    span: value.span,
                });
                self.own(state, &place, func.local(*local).ty);
            }
            HirStmt::MultiLet { locals, call } => {
                let places: Vec<PlaceName> = locals
                    .iter()
                    .map(|id| self.declare(state, func, *id))
                    .collect();
                if let HirExprKind::Call {
                    callee: callee @ (HirCallee::Func(_) | HirCallee::Local(_)),
                    args,
                } = &call.kind
                {
                    self.emit_call(state, func, callee, args, &places, call.span);
                } else {
                    let operand = self.read(state, func, call);
                    state.emit(LirInst::CopyPlace {
                        src: operand,
                        dst: places[0].clone(),
                        span: call.span,
                    });
                }
                for (place, id) in places.iter().zip(locals) {
                    self.own(state, place, func.local(*id).ty);
                }
            }
            HirStmt::Assign { place, value } => self.lower_assign(state, func, place, value),
            HirStmt::Expr(expr) => match &expr.kind {
                HirExprKind::Call { callee, args } if expr.ty.is_error() => {
                    if let HirCallee::Intrinsic(intrinsic) = callee {
                        let _ =
                            self.lower_intrinsic(state, func, *intrinsic, args, expr.ty, expr.span);
                    } else {
                        self.emit_call(state, func, callee, args, &[], expr.span);
                    }
                }
                HirExprKind::CompileError { msg, msg_span } => {
                    state.emit(LirInst::CompileError {
                        msg: msg.clone(),
                        msg_span: *msg_span,
                        call_span: expr.span,
                    });
                }
                _ => {
                    let _ = self.value(state, func, expr);
                }
            },
            HirStmt::Return(span) => {
                for index in (0..state.scopes.len()).rev() {
                    self.drop_scope(state, index, *span);
                }
                state.end(LirTerm::Return { span: *span });
            }
            HirStmt::Break(span) | HirStmt::Continue(span) => {
                let Some((head, exit, depth)) = state.loops.last().cloned() else {
                    lilac_error::bug!(*span, "break or continue outside a loop");
                };
                for index in (depth..state.scopes.len()).rev() {
                    self.drop_scope(state, index, *span);
                }
                let target = if matches!(stmt, HirStmt::Break(_)) {
                    exit
                } else {
                    head
                };
                state.branch(&target, *span);
            }
            HirStmt::If { cond, then, els } => {
                // A constant condition lowers only its live arm.
                if let Some(taken) = self.constant(cond).and_then(|value| value.truthy()) {
                    self.lower_block(state, func, if taken { then } else { els });
                } else {
                    self.lower_if(state, func, cond, then, els);
                }
            }
            HirStmt::Loop(body) => {
                let (head, exit) = (state.label(), state.label());
                state.branch(&head, body.span);
                state.at(head.clone());
                state
                    .loops
                    .push((head.clone(), exit.clone(), state.scopes.len()));
                self.lower_block(state, func, body);
                state.loops.pop();
                state.branch(&head, body.span);
                state.at(exit);
            }
            HirStmt::Match {
                scrutinee,
                arms,
                els,
            } => self.lower_match(state, func, scrutinee, arms, els),
            HirStmt::Block(block) => self.lower_block(state, func, block),
        }
        if state.open {
            let temps: Vec<(PlaceName, TypeRef)> = state.scopes[scope].temps[mark..]
                .iter()
                .rev()
                .cloned()
                .collect();
            let span = stmt_span(stmt);
            for (place, ty) in temps {
                self.drop_place(state, &Place::Direct(place), ty, span);
            }
        }
        state.scopes[scope].temps.truncate(mark);
    }

    pub(crate) fn lower_if(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        cond: &HirExpr,
        then: &HirBlock,
        els: &HirBlock,
    ) {
        let cond = self.value_reg(state, func, cond);
        let (then_label, els_label, join) = (state.label(), state.label(), state.label());
        state.end(LirTerm::BranchIf {
            cond,
            then: then_label.clone(),
            els: Some(els_label.clone()),
            span: then.span,
        });
        state.at(then_label);
        self.lower_block(state, func, then);
        state.branch(&join, then.span);
        state.at(els_label);
        self.lower_block(state, func, els);
        state.branch(&join, els.span);
        state.at(join);
    }

    pub(crate) fn lower_assign(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        place: &HirExpr,
        value: &HirExpr,
    ) {
        let ty = place.ty;
        let operand = self.read(state, func, value);
        let target = self.place_of(state, func, place);
        let override_ = self
            .hir
            .hooks
            .get(&ty.id)
            .and_then(|hooks| hooks.override_.clone());
        let drops = self.needs_drop(ty);
        if override_.is_none() && !drops {
            state.write(&target, operand, value.span);
            return;
        }
        let new = self.fresh_place(state, operand, ty, value.span);
        // Assigning an out slot initializes the result, so `@on_override` is
        // skipped (a guard type could otherwise never be returned); the old
        // value is still dropped.
        let is_out = matches!(place.kind, HirExprKind::Local(id) if func.returns.contains(&id));
        if let Some(hook) = override_.filter(|_| !ty.is_ref && !is_out) {
            let old_ref = self.ref_place(state, &target, ty, place.span);
            let new_ref = self.ref_place(state, &Place::Direct(new.clone()), ty, place.span);
            let symbol = self.symbol(&hook);
            state.emit(LirInst::Call {
                func: LirCallee::Func(symbol),
                ins: vec![old_ref, new_ref],
                outs: Vec::new(),
                span: place.span,
            });
        }
        if drops {
            self.drop_place(state, &target, ty, place.span);
        }
        state.write(&target, LirOperand::Place(new), value.span);
    }

    pub(crate) fn lower_match(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        scrutinee: &HirExpr,
        arms: &[HirArm],
        els: &HirBlock,
    ) {
        if let Some((choice, _)) =
            self.constant_choice(scrutinee, arms.iter().map(|arm| &arm.pattern))
        {
            // A constant scrutinee lowers only the chosen arm.
            let Some(arm) = choice.map(|index| &arms[index]) else {
                self.lower_block(state, func, els);
                return;
            };
            state.scopes.push(Scope::default());
            if let Some(capture) = arm.capture {
                let (subject, _) = self.subject(state, func, scrutinee);
                self.bind_capture(state, func, &subject, scrutinee.ty, &arm.pattern, capture);
            }
            for stmt in &arm.body.stmts {
                self.lower_stmt(state, func, stmt);
            }
            if state.open {
                let index = state.scopes.len() - 1;
                self.drop_scope(state, index, end_of(arm.body.span));
            }
            state.scopes.pop();
            return;
        }
        let (subject, is_enum) = self.subject(state, func, scrutinee);
        let join = state.label();
        let els_label = state.label();
        let mut next = state.label();
        state.branch(&next, scrutinee.span);
        for (index, arm) in arms.iter().enumerate() {
            state.at(next);
            let body = state.label();
            next = if index + 1 == arms.len() {
                els_label.clone()
            } else {
                state.label()
            };
            let test = self.pattern_test(
                state,
                &subject,
                scrutinee.ty,
                is_enum,
                &arm.pattern,
                scrutinee.span,
            );
            state.end(LirTerm::BranchIf {
                cond: test,
                then: body.clone(),
                els: Some(next.clone()),
                span: arm.body.span,
            });
            state.at(body);
            state.scopes.push(Scope::default());
            if let Some(capture) = arm.capture {
                self.bind_capture(state, func, &subject, scrutinee.ty, &arm.pattern, capture);
            }
            for stmt in &arm.body.stmts {
                self.lower_stmt(state, func, stmt);
            }
            if state.open {
                let index = state.scopes.len() - 1;
                self.drop_scope(state, index, end_of(arm.body.span));
            }
            state.scopes.pop();
            state.branch(&join, arm.body.span);
        }
        if arms.is_empty() {
            state.at(next);
            state.branch(&els_label, els.span);
        }
        state.at(els_label);
        self.lower_block(state, func, els);
        state.branch(&join, els.span);
        state.at(join);
    }

    /// The place a `match` inspects (fresh values spill into a temp).
    pub(crate) fn subject(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        scrutinee: &HirExpr,
    ) -> (Place, bool) {
        let is_enum = matches!(self.layout(scrutinee.ty.id), Some(Layout::Enum { .. }));
        if scrutinee.is_place() {
            return (self.place_of(state, func, scrutinee), is_enum);
        }
        let operand = self.value(state, func, scrutinee);
        let place = self.spill(state, operand, scrutinee.ty, scrutinee.span);
        (Place::Direct(place), is_enum)
    }

    /// `&test: #bit8` for one arm pattern.
    pub(crate) fn pattern_test(
        &mut self,
        state: &mut FnState,
        subject: &Place,
        ty: TypeRef,
        is_enum: bool,
        pattern: &HirPattern,
        span: Span,
    ) -> Reg {
        let (value, constant, lir) = if is_enum {
            let HirPattern::Variant(index) = pattern else {
                lilac_error::bug!(span, "byte pattern on an enum");
            };
            let Some(Layout::Enum { tag, .. }) = self.layout(ty.id) else {
                lilac_error::bug!(span, "enum without a layout");
            };
            let tag = self.lir_ty(TypeRef::value(*tag));
            let value = state.reg();
            state.emit(LirInst::Field {
                obj: subject.name().to_owned(),
                name: "tag".to_owned(),
                idx: 0,
                dst: value.clone(),
                ty: tag.clone(),
                span,
            });
            (value, LirConst::Int(u64::from(*index)), tag)
        } else {
            let HirPattern::Const(bytes) = pattern else {
                lilac_error::bug!(span, "tag pattern on a bit scrutinee");
            };
            let lir = self.lir_ty(ty);
            let value = state.read_place(subject, &lir, span);
            let value = state.reg_of(value, &lir, span);
            (value, number_const(bytes, &lir, span), lir)
        };
        let constant_reg = state.reg();
        state.emit(LirInst::Const {
            dst: constant_reg.clone(),
            value: constant,
            ty: lir,
            span,
        });
        let test = state.reg();
        state.emit(LirInst::Arith {
            op: ArithOp::Eq,
            lhs: value,
            rhs: constant_reg,
            dst: test.clone(),
            ty: LirTy::Bit(8),
            span,
        });
        test
    }

    /// Initializes a capture local with a copy of the matched payload.
    pub(crate) fn bind_capture(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        subject: &Place,
        ty: TypeRef,
        pattern: &HirPattern,
        capture: LocalId,
    ) {
        let HirPattern::Variant(index) = pattern else {
            lilac_error::bug!(Span::dummy(), "capture on a byte pattern");
        };
        let place = self.declare(state, func, capture);
        let HirLocal {
            ty: capture_ty,
            span,
            ..
        } = *func.local(capture);
        let (field, name) = self.payload_field(ty.id, *index);
        let payload = self.field_place(state, subject, field, &name, capture_ty, span);
        let value = self.copy_out(state, &payload, capture_ty, span);
        state.emit(LirInst::CopyPlace {
            src: value,
            dst: place.clone(),
            span,
        });
        self.own(state, &place, capture_ty);
    }

    /// Whether lowering `expr` creates blocks.
    pub(crate) fn branches(&self, expr: &HirExpr) -> bool {
        let mut found = false;
        expr.walk(&mut |e| match &e.kind {
            HirExprKind::If { .. }
            | HirExprKind::Match { .. }
            | HirExprKind::Logic { .. }
            | HirExprKind::Call {
                callee: HirCallee::Intrinsic(Intrinsic::ArrayAt),
                ..
            } => {
                found = true;
            }
            HirExprKind::Const(def) => {
                if let Some(value) = self.hir.consts.get(def) {
                    found |= self.branches(value);
                }
            }
            _ => {}
        });
        found
    }

    /// Evaluates `value` in its own scope and moves it into `result`.
    pub(crate) fn region(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        value: &HirExpr,
        result: &PlaceName,
    ) {
        state.scopes.push(Scope::default());
        let operand = self.read(state, func, value);
        state.emit(LirInst::CopyPlace {
            src: operand,
            dst: result.clone(),
            span: value.span,
        });
        let index = state.scopes.len() - 1;
        self.drop_scope(state, index, value.span);
        state.scopes.pop();
    }

    pub(crate) fn lower_match_value(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        expr: &HirExpr,
        scrutinee: &HirExpr,
        arms: &[HirValueArm],
        els: &HirExpr,
    ) -> LirOperand {
        let span = expr.span;
        let result = self.tmp(state, expr.ty, span);
        if let Some((choice, _)) =
            self.constant_choice(scrutinee, arms.iter().map(|arm| &arm.pattern))
        {
            // A constant scrutinee lowers only the chosen arm.
            match choice.map(|index| &arms[index]) {
                Some(arm) => {
                    state.scopes.push(Scope::default());
                    if let Some(capture) = arm.capture {
                        let (subject, _) = self.subject(state, func, scrutinee);
                        self.bind_capture(
                            state,
                            func,
                            &subject,
                            scrutinee.ty,
                            &arm.pattern,
                            capture,
                        );
                    }
                    let operand = self.read(state, func, &arm.value);
                    state.emit(LirInst::CopyPlace {
                        src: operand,
                        dst: result.clone(),
                        span,
                    });
                    let scope = state.scopes.len() - 1;
                    self.drop_scope(state, scope, arm.value.span);
                    state.scopes.pop();
                }
                None => self.region(state, func, els, &result),
            }
            self.own_temp(state, &result, expr.ty);
            return LirOperand::Place(result);
        }
        let (subject, is_enum) = self.subject(state, func, scrutinee);
        let join = state.label();
        let els_label = state.label();
        let mut next = state.label();
        state.branch(&next, span);
        for (index, arm) in arms.iter().enumerate() {
            state.at(next);
            let body = state.label();
            next = if index + 1 == arms.len() {
                els_label.clone()
            } else {
                state.label()
            };
            let test =
                self.pattern_test(state, &subject, scrutinee.ty, is_enum, &arm.pattern, span);
            state.end(LirTerm::BranchIf {
                cond: test,
                then: body.clone(),
                els: Some(next.clone()),
                span,
            });
            state.at(body);
            state.scopes.push(Scope::default());
            if let Some(capture) = arm.capture {
                self.bind_capture(state, func, &subject, scrutinee.ty, &arm.pattern, capture);
            }
            let operand = self.read(state, func, &arm.value);
            state.emit(LirInst::CopyPlace {
                src: operand,
                dst: result.clone(),
                span,
            });
            let scope = state.scopes.len() - 1;
            self.drop_scope(state, scope, arm.value.span);
            state.scopes.pop();
            state.branch(&join, span);
        }
        if arms.is_empty() {
            state.at(next);
            state.branch(&els_label, span);
        }
        state.at(els_label);
        self.region(state, func, els, &result);
        state.branch(&join, span);
        state.at(join);
        self.own_temp(state, &result, expr.ty);
        LirOperand::Place(result)
    }

    /// The value of an `if` expression: each branch writes the result; a
    /// constant condition lowers only the branch it takes.
    pub(crate) fn if_value(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        expr: &HirExpr,
        cond: &HirExpr,
        then: &HirExpr,
        els: &HirExpr,
    ) -> LirOperand {
        let span = expr.span;
        let result = self.tmp(state, expr.ty, span);
        if let Some(taken) = self.constant(cond).and_then(|value| value.truthy()) {
            self.region(state, func, if taken { then } else { els }, &result);
            self.own_temp(state, &result, expr.ty);
            return LirOperand::Place(result);
        }
        let cond = self.value_reg(state, func, cond);
        let (then_label, els_label, join) = (state.label(), state.label(), state.label());
        state.end(LirTerm::BranchIf {
            cond,
            then: then_label.clone(),
            els: Some(els_label.clone()),
            span,
        });
        for (label, branch) in [(then_label, then), (els_label, els)] {
            state.at(label);
            self.region(state, func, branch, &result);
            state.branch(&join, span);
        }
        state.at(join);
        self.own_temp(state, &result, expr.ty);
        LirOperand::Place(result)
    }

    /// The value of `and`/`or`: the right side runs only when the left does
    /// not decide; a constant left side lowers only what it leaves.
    pub(crate) fn logic_value(
        &mut self,
        state: &mut FnState,
        func: &HirFunc,
        expr: &HirExpr,
        and: bool,
        lhs: &HirExpr,
        rhs: &HirExpr,
    ) -> LirOperand {
        let span = expr.span;
        let result = self.tmp(state, expr.ty, span);
        if let Some(left) = self.constant(lhs).and_then(|value| value.truthy()) {
            if left == and {
                self.region(state, func, rhs, &result);
            } else {
                let reg = state.reg();
                state.emit(LirInst::Const {
                    dst: reg.clone(),
                    value: LirConst::Int(u64::from(!and)),
                    ty: LirTy::Bit(8),
                    span,
                });
                state.emit(LirInst::CopyPlace {
                    src: LirOperand::Reg(reg),
                    dst: result.clone(),
                    span,
                });
            }
            return LirOperand::Place(result);
        }
        let left = self.value_reg(state, func, lhs);
        let (rhs_label, short_label, join) = (state.label(), state.label(), state.label());
        let (then, els) = if and {
            (rhs_label.clone(), short_label.clone())
        } else {
            (short_label.clone(), rhs_label.clone())
        };
        state.end(LirTerm::BranchIf {
            cond: left,
            then,
            els: Some(els),
            span,
        });
        state.at(rhs_label);
        self.region(state, func, rhs, &result);
        state.branch(&join, span);
        state.at(short_label);
        let reg = state.reg();
        state.emit(LirInst::Const {
            dst: reg.clone(),
            value: LirConst::Int(u64::from(!and)),
            ty: LirTy::Bit(8),
            span,
        });
        state.emit(LirInst::CopyPlace {
            src: LirOperand::Reg(reg),
            dst: result.clone(),
            span,
        });
        state.branch(&join, span);
        state.at(join);
        LirOperand::Place(result)
    }
}
