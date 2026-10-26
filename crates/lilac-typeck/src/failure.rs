//! Functions that can fail (`value: T else error: E`): `fail`, and the
//! three places a call of one is handled: a guard
//! (`a: A = f() else -> e: E { … }`), a capture `if`
//! (`if f() -> a: A { … } else -> e: E { … }`) and so a `loop if`. A call
//! anywhere else is `E057` ([`Checker::func_call`]).
//!
//! A function that can fail has one more out after its success and
//! failure outs, a `@bit8` flag (`HirFunc::failure`): set at entry and
//! cleared by `fail`. A guard or capture `if` binds every out of its call
//! to a hidden local, branches on the flag, and binds its captures from
//! those locals by position.

use lilac_ast::{Block, Expr, LetBinding, Param, Stmt};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_hir::{HirBlock, HirCallee, HirExpr, HirExprKind, HirStmt, LocalId};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeId, TypeKind, TypeRef};

use crate::body::{empty_block, local_expr, moved_local, never_finishes, set_flag, FnCx};
use crate::calls::Called;
use crate::reflect::{is_member_test, Facts};
use crate::Checker;

/// The checked call of a guard or capture `if`.
pub(crate) enum Subject {
    /// A call of a function that can fail.
    Fails(FailingCall),
    /// A call of a function that cannot.
    Call {
        /// The call, valued as its first out (the rest drop).
        call: HirExpr,
        /// Its outs.
        outs: Vec<TypeRef>,
        /// The function, for messages.
        name: String,
    },
    /// Not a call.
    Value(HirExpr),
}

/// A call of a function that can fail, with every out.
pub(crate) struct FailingCall {
    /// The call (an `HirExprKind::Call` over every out).
    call: HirExpr,
    /// The success outs, the failure outs, then the flag.
    outs: Vec<TypeRef>,
    /// How many outs are success outs.
    success: usize,
    /// The function, for messages.
    name: String,
}

impl FailingCall {
    /// How many outs are failure outs.
    fn failure(&self) -> usize {
        self.outs.len() - self.success - 1
    }
}

impl Checker<'_> {
    /// How many of a call's outs are success outs, when its callee can fail.
    pub(crate) fn success_outs(&self, callee: &HirCallee) -> Option<usize> {
        match callee {
            HirCallee::Func(key) => {
                let decl = self.res.def(key.def).func()?;
                decl.failures.as_ref().map(|_| decl.returns.len())
            }
            HirCallee::Assumed { ty, .. } => match self.types.kind(*ty) {
                TypeKind::Func {
                    returns,
                    failures: Some(_),
                    ..
                } => Some(returns.len()),
                _ => None,
            },
            HirCallee::Value(_) | HirCallee::Intrinsic(_) => None,
        }
    }

    /// `fail`: clears the flag and leaves.
    pub(crate) fn check_fail(&mut self, fcx: &FnCx, span: Span, out: &mut Vec<HirStmt>) {
        let Some(flag) = fcx.fail_flag else {
            self.diags.emit(
                Diagnostic::error(codes::E058, "`fail` outside a function that can fail")
                    .label(span, true, "this function cannot fail")
                    .help(
                        "declare failure outs after `else` in the signature: \
                         `function (...) value: T else error: E` (or `else nothing`)",
                    ),
            );
            out.push(HirStmt::Return(span));
            return;
        };
        out.push(set_flag(flag, false, span));
        out.push(HirStmt::Fail(span));
    }

    /// Checks `value`, the call of a guard or capture `if`: the one call
    /// allowed to fail here.
    fn check_subject(
        &mut self,
        fcx: &mut FnCx,
        value: &Expr,
        expected: Option<TypeId>,
    ) -> Result<Subject, ErrorGuaranteed> {
        let span = value.span();
        let outer = fcx.handled.replace(span);
        let result = match value {
            Expr::Call {
                callee,
                generic_args,
                args,
                span,
            } => self.check_call(fcx, callee, generic_args, args, *span, expected),
            Expr::Construct {
                function,
                member_span,
                generic_args,
                args,
                span,
            } => self.construct_call(
                fcx,
                function,
                *member_span,
                generic_args,
                args,
                *span,
                expected,
            ),
            _ => {
                fcx.handled = outer;
                return Ok(Subject::Value(self.check_expr(fcx, value, expected)));
            }
        };
        fcx.handled = outer;
        let (callee, args, outs) = match result? {
            Called::Call { callee, args, outs } => (callee, args, outs),
            Called::Value(value) => return Ok(Subject::Value(value)),
        };
        let name = match &callee {
            HirCallee::Func(key) => self.function_name(key.def),
            HirCallee::Value(_) => "this function reference".to_owned(),
            HirCallee::Assumed { name, .. } => name.clone(),
            HirCallee::Intrinsic(_) => match value {
                Expr::Call { callee, .. } => match &**callee {
                    Expr::Ident(name, _) => name.clone(),
                    _ => "this intrinsic".to_owned(),
                },
                _ => "this intrinsic".to_owned(),
            },
        };
        let success = self.success_outs(&callee);
        let call = HirExpr {
            ty: outs.first().copied().unwrap_or(TypeRef::ERROR),
            span,
            kind: HirExprKind::Call {
                callee,
                args,
                rest: outs.get(1..).unwrap_or_default().to_vec(),
            },
        };
        Ok(match success {
            Some(success) => Subject::Fails(FailingCall {
                call,
                outs,
                success,
                name,
            }),
            None => Subject::Call { call, outs, name },
        })
    }

    /// `if cond [-> captures] { } [else [-> captures] …]`: a capture `if`
    /// when `cond` is a call that can fail, else a plain one. As a
    /// `loop`'s statement (`FnCx::implicit_break`), a chain with no final
    /// `else` ends with `else break`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn check_if(
        &mut self,
        fcx: &mut FnCx,
        cond: &Expr,
        captures: &[Param],
        then: &Block,
        else_captures: &[Param],
        els: Option<&Stmt>,
        span: Span,
        out: &mut Vec<HirStmt>,
    ) {
        // Taken here so no `if` inside the condition or the `then` block
        // sees it, and given back to the `else` only when it continues the
        // chain.
        let implicit = std::mem::take(&mut fcx.implicit_break);
        let implicit =
            (implicit && matches!(els, None | Some(Stmt::If { .. }))).then(|| cond.span());
        let first_capture = captures.first().or(else_captures.first());
        let mut facts = Facts::default();
        let cond = if matches!(cond, Expr::Call { .. }) && !is_member_test(cond) {
            match self.check_subject(fcx, cond, None) {
                Ok(Subject::Fails(call)) => {
                    self.check_capture_if(
                        fcx,
                        &call,
                        captures,
                        then,
                        else_captures,
                        els,
                        implicit,
                        span,
                        out,
                    );
                    return;
                }
                Ok(Subject::Call { call, name, .. }) if first_capture.is_some() => {
                    self.no_captures(first_capture, &call, &name);
                    HirExpr::poison(call.span)
                }
                Ok(Subject::Call { call, outs, .. }) if outs.is_empty() => {
                    self.diags.emit(
                        Diagnostic::error(codes::E022, "this call returns no value").label(
                            call.span,
                            true,
                            "a condition is expected here",
                        ),
                    );
                    HirExpr::poison(call.span)
                }
                Ok(Subject::Call { call, .. } | Subject::Value(call)) => self.cond_of(call, "if"),
                Err(_) => HirExpr::poison(cond.span()),
            }
        } else {
            let (cond, proved) = self.check_condition(fcx, cond, "if");
            facts = proved;
            cond
        };
        if !cond.ty.is_error() {
            self.no_captures(first_capture, &cond, "the condition");
        }
        let mark = self.assume(fcx, &facts.when_true);
        let then = self.captured_block(fcx, then, captures, &[], &[]);
        self.unassume(fcx, mark);
        let mark = self.assume(fcx, &facts.when_false);
        let els = self.else_block(fcx, els, else_captures, &[], &[], implicit, span);
        self.unassume(fcx, mark);
        out.push(HirStmt::If { cond, then, els });
    }

    /// Captures on `cond`, which cannot fail, are `E062`.
    fn no_captures(&mut self, captured: Option<&Param>, cond: &HirExpr, name: &str) {
        let Some(first) = captured else {
            return;
        };
        let name = if name.starts_with("the ") {
            name.to_owned()
        } else {
            format!("`{name}`")
        };
        self.diags.emit(
            Diagnostic::error(
                codes::E062,
                format!("{name} cannot fail, so it has no captures"),
            )
            .label(first.span, true, "nothing to capture")
            .label(cond.span, false, "this cannot fail")
            .help("bind its value before the `if`: `value: T = ...`"),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn check_capture_if(
        &mut self,
        fcx: &mut FnCx,
        call: &FailingCall,
        captures: &[Param],
        then: &Block,
        else_captures: &[Param],
        els: Option<&Stmt>,
        implicit: Option<Span>,
        span: Span,
        out: &mut Vec<HirStmt>,
    ) {
        self.capture_count(captures, call.success, "success", call);
        self.capture_count(else_captures, call.failure(), "failure", call);
        let (stmts, hidden, flag) = Self::bind_outs(fcx, call);
        let mut stmts = vec![stmts];
        let (success, failure) = call.outs.split_at(call.success);
        let (successes, failures) = hidden.split_at(call.success);
        let then = self.captured_block(fcx, then, captures, successes, success);
        let els = self.else_block(fcx, els, else_captures, failures, failure, implicit, span);
        stmts.push(HirStmt::If {
            cond: flag,
            then,
            els,
        });
        out.push(HirStmt::Block(HirBlock { stmts, span }));
    }

    /// `bindings = value else [-> captures] { … }`: the bindings take the
    /// success outs in the current scope, and `els`, which must leave,
    /// runs on failure.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn check_guard(
        &mut self,
        fcx: &mut FnCx,
        bindings: &[LetBinding],
        value: &Expr,
        captures: &[Param],
        els: &Block,
        span: Span,
        out: &mut Vec<HirStmt>,
    ) {
        let declared: Vec<TypeRef> = match (bindings, value) {
            ([only], Expr::Construct { function, .. }) => {
                vec![self.construct_type(&only.ty, function)]
            }
            _ => bindings.iter().map(|b| self.resolve_type(&b.ty)).collect(),
        };
        let expected = match declared.as_slice() {
            [only] if !only.is_error() => Some(only.id),
            _ => None,
        };
        let call = match self.check_subject(fcx, value, expected) {
            Ok(Subject::Fails(call)) => Some(call),
            Ok(Subject::Call { call, name, .. }) => {
                self.diags.emit(
                    Diagnostic::error(codes::E062, format!("`{name}` cannot fail"))
                        .label(call.span, true, "this call cannot fail")
                        .help("remove the `else` block"),
                );
                None
            }
            Ok(Subject::Value(value)) => {
                if !value.ty.is_error() {
                    self.diags.emit(
                        Diagnostic::error(codes::E062, "a guard needs a call that can fail")
                            .label(value.span, true, "not a call that can fail")
                            .help("remove the `else` block"),
                    );
                }
                None
            }
            Err(_) => None,
        };
        let (successes, success) = if let Some(call) = &call {
            self.capture_count_bindings(bindings, call);
            self.capture_count(captures, call.failure(), "failure", call);
            let (stmt, hidden, flag) = Self::bind_outs(fcx, call);
            out.push(stmt);
            let (success, failure) = call.outs.split_at(call.success);
            let (successes, failures) = hidden.split_at(call.success);
            let els = self.captured_block(fcx, els, captures, failures, failure);
            out.push(HirStmt::If {
                cond: flag,
                then: empty_block(span),
                els,
            });
            (successes.to_vec(), success.to_vec())
        } else {
            let els = self.captured_block(fcx, els, captures, &[], &[]);
            out.push(HirStmt::Block(els));
            (Vec::new(), Vec::new())
        };
        if !block_leaves(&els.stmts) {
            self.diags.emit(
                Diagnostic::error(codes::E059, "a guard's `else` block must leave")
                    .label(els.span, true, "this block can finish")
                    .help(
                        "end it with `return`, `fail`, `break`, `continue`, `@panic` or `@trap`; \
                         the bindings have no value after it",
                    ),
            );
        }
        for (index, (binding, want)) in bindings.iter().zip(&declared).enumerate() {
            let value = match (successes.get(index), success.get(index)) {
                (Some(local), Some(got)) => self.coerce(
                    moved_local(*local, *got, binding.span),
                    *want,
                    binding.ty.span,
                ),
                _ => HirExpr::poison(binding.span),
            };
            let local = self.declare(fcx, &binding.name, *want, binding.span);
            out.push(HirStmt::Let { local, value });
        }
        // The success outs no binding takes drop with the current scope.
        out.extend(Self::bind_rest(fcx, &successes, &success, bindings.len()));
    }

    /// Binds every out of `call` to a hidden local: the statement, the
    /// locals, and the flag as a condition.
    fn bind_outs(fcx: &mut FnCx, call: &FailingCall) -> (HirStmt, Vec<LocalId>, HirExpr) {
        let span = call.call.span;
        let hidden: Vec<LocalId> = call.outs.iter().map(|ty| fcx.hidden(*ty, span)).collect();
        let flag = hidden.last().copied().unwrap_or(LocalId(0));
        let stmt = HirStmt::MultiLet {
            locals: hidden.clone(),
            call: call.call.clone(),
        };
        let cond = local_expr(flag, Builtin::Bit8.value(), span);
        let count = call.outs.len() - 1;
        let mut hidden = hidden;
        hidden.truncate(count);
        (stmt, hidden, cond)
    }

    /// `block` as a scope that first binds `captures` from `locals` (of
    /// types `tys`) by position; a capture with no local is poison (its
    /// error reported). Every local moves out here: the ones no capture
    /// takes move into hidden locals that drop with this scope.
    fn captured_block(
        &mut self,
        fcx: &mut FnCx,
        block: &Block,
        captures: &[Param],
        locals: &[LocalId],
        tys: &[TypeRef],
    ) -> HirBlock {
        if captures.is_empty() && locals.is_empty() {
            return self.check_block(fcx, block);
        }
        fcx.open(block.span);
        let mut stmts = self.bind_captures(fcx, captures, locals, tys);
        let inner = self.check_block(fcx, block);
        fcx.close();
        stmts.push(HirStmt::Block(inner));
        HirBlock {
            stmts,
            span: block.span,
        }
    }

    /// Binds `captures` from `locals` (of types `tys`) by position in the
    /// current scope, then moves the rest of `locals` into hidden locals.
    fn bind_captures(
        &mut self,
        fcx: &mut FnCx,
        captures: &[Param],
        locals: &[LocalId],
        tys: &[TypeRef],
    ) -> Vec<HirStmt> {
        let mut stmts = Vec::new();
        for (index, capture) in captures.iter().enumerate() {
            let declared = self.resolve_type(&capture.ty);
            let value = match (locals.get(index), tys.get(index)) {
                (Some(local), Some(ty)) => self.coerce(
                    moved_local(*local, *ty, capture.span),
                    declared,
                    capture.ty.span,
                ),
                _ => HirExpr::poison(capture.span),
            };
            let local = self.declare(fcx, &capture.name, declared, capture.span);
            stmts.push(HirStmt::Let { local, value });
        }
        stmts.extend(Self::bind_rest(fcx, locals, tys, captures.len()));
        stmts
    }

    /// Moves `locals` from index `from` on into hidden locals of the current
    /// scope, so each drops with it.
    fn bind_rest(fcx: &mut FnCx, locals: &[LocalId], tys: &[TypeRef], from: usize) -> Vec<HirStmt> {
        locals
            .iter()
            .zip(tys)
            .skip(from)
            .map(|(local, ty)| {
                let span = fcx.locals[local.0 as usize].span;
                HirStmt::Let {
                    local: fcx.hidden(*ty, span),
                    value: moved_local(*local, *ty, span),
                }
            })
            .collect()
    }

    /// An `if`'s `else`: a block binding `captures` (and moving the rest of
    /// `locals`) when there are any. `implicit` is the span of the
    /// `else break` a `loop`'s `if` chain ends with: a missing `else`
    /// breaks there, and an `else if` passes it on.
    #[allow(clippy::too_many_arguments)]
    fn else_block(
        &mut self,
        fcx: &mut FnCx,
        els: Option<&Stmt>,
        captures: &[Param],
        locals: &[LocalId],
        tys: &[TypeRef],
        implicit: Option<Span>,
        span: Span,
    ) -> HirBlock {
        fcx.implicit_break = implicit.is_some();
        let block = match (els, implicit) {
            (Some(Stmt::Block(block)), _) => self.captured_block(fcx, block, captures, locals, tys),
            (Some(els), _) if locals.is_empty() => self.check_stmt_block(fcx, els),
            (None, Some(at)) if locals.is_empty() => HirBlock {
                stmts: vec![HirStmt::Break(at)],
                span: at,
            },
            (None, None) if locals.is_empty() => empty_block(span),
            _ => {
                let span = els.map_or(implicit.unwrap_or(span), Stmt::span);
                fcx.open(span);
                let mut stmts = self.bind_captures(fcx, captures, locals, tys);
                match (els, implicit) {
                    (Some(els), _) => {
                        stmts.push(HirStmt::Block(self.check_stmt_block(fcx, els)));
                    }
                    (None, Some(at)) => stmts.push(HirStmt::Break(at)),
                    (None, None) => {}
                }
                fcx.close();
                HirBlock { stmts, span }
            }
        };
        fcx.implicit_break = false;
        block
    }

    /// More `captures` than `count` outs on this `side` is `E062`.
    fn capture_count(&mut self, captures: &[Param], count: usize, side: &str, call: &FailingCall) {
        if let Some(extra) = captures.get(count) {
            self.too_many(extra.span, captures.len(), count, side, call);
        }
    }

    /// More guard bindings than success outs is `E062`.
    fn capture_count_bindings(&mut self, bindings: &[LetBinding], call: &FailingCall) {
        if let Some(extra) = bindings.get(call.success) {
            self.too_many(extra.span, bindings.len(), call.success, "success", call);
        }
    }

    fn too_many(
        &mut self,
        extra: Span,
        found: usize,
        count: usize,
        side: &str,
        call: &FailingCall,
    ) {
        let outs = if count == 1 { "out" } else { "outs" };
        self.diags.emit(
            Diagnostic::error(
                codes::E062,
                format!(
                    "`{}` has {count} {side} {outs}, but {found} are bound",
                    call.name
                ),
            )
            .label(extra, true, "no out for this binding")
            .label(call.call.span, false, "this call"),
        );
    }
}

/// Whether running `stmts` never reaches their end: one of them leaves.
pub(crate) fn block_leaves(stmts: &[Stmt]) -> bool {
    stmts.iter().any(leaves)
}

/// Whether `stmt` never finishes: it leaves (`return`, `fail`, `break`,
/// `continue`, `@panic`, `@trap`), or every way through it does.
fn leaves(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Block(block) => block_leaves(&block.stmts),
        Stmt::If {
            then,
            els: Some(els),
            ..
        } => block_leaves(&then.stmts) && leaves(els),
        Stmt::Match {
            arms,
            els: Some(els),
            ..
        } => {
            arms.iter().all(|arm| match &arm.body {
                lilac_ast::MatchArmBody::Block(block) => block_leaves(&block.stmts),
                lilac_ast::MatchArmBody::Inline(stmt) => leaves(stmt),
            }) && block_leaves(&els.stmts)
        }
        _ => never_finishes(stmt),
    }
}
