//! Function bodies: locals and block scopes, and statements.
//!
//! Locals live in a per-function table ([`FnCx::locals`]); names resolve
//! through a stack of block scopes. Redeclaring a name in the same block is
//! `E020`; a nested block may shadow. Parameters and return slots share the
//! body's outermost scope.

use std::collections::{HashMap, HashSet};

use lilac_ast::{
    Block, Expr, FuncBody, FuncDecl, MatchArm, MatchArmBody, MatchPattern, Param, Stmt,
};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed, Unreported};
use lilac_hir::{
    FuncKey, HirArm, HirBlock, HirExpr, HirExprKind, HirFailure, HirFunc, HirLocal, HirOld,
    HirPattern, HirStmt, LocalId, LocalKind, SemanticKind,
};
use lilac_span::Span;
use lilac_ty::{Builtin, FuncSig, TypeId, TypeRef};

use crate::calls::Called;
use crate::sigs::leading;
use crate::sink::has_error;
use crate::{Checker, State};

/// Per-function checking state.
pub(crate) struct FnCx {
    /// Every local, indexed by [`LocalId`].
    pub locals: Vec<HirLocal>,
    /// Block scopes, innermost last.
    pub scopes: Vec<HashMap<String, LocalId>>,
    /// Each scope's source range, parallel to `scopes`.
    pub scope_spans: Vec<Span>,
    /// The range each local is in scope over, by [`LocalId`].
    pub local_scopes: Vec<Span>,
    /// Parameter locals (colored as parameters).
    pub params: HashSet<LocalId>,
    /// Named results, and parameters of a function without a body: names
    /// only its signature has ([`LocalKind::Signature`]).
    pub signature_only: HashSet<LocalId>,
    /// `loop` nesting depth.
    pub loops: u32,
    /// The span of the call a guard or capture `if` is checking: the one
    /// call that may fail without an error.
    pub handled: Option<Span>,
    /// The flag out of a function that can fail (`HirFunc::failure`).
    pub fail_flag: Option<LocalId>,
}

impl FnCx {
    /// A function whose outermost scope (its parameters) covers `span`.
    pub(crate) fn new(span: Span) -> Self {
        Self {
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
            scope_spans: vec![span],
            local_scopes: Vec::new(),
            params: HashSet::new(),
            signature_only: HashSet::new(),
            loops: 0,
            handled: None,
            fail_flag: None,
        }
    }

    /// A fresh local not bound to any name.
    pub(crate) fn hidden(&mut self, ty: TypeRef, span: Span) -> LocalId {
        let id = LocalId(u32::try_from(self.locals.len()).unwrap_or(u32::MAX));
        let scope = self.scope_spans.last().copied().unwrap_or_else(Span::dummy);
        self.local_scopes.push(scope);
        self.locals.push(HirLocal {
            name: String::new(),
            ty,
            span,
        });
        id
    }

    /// The local `name` denotes, innermost scope first.
    pub(crate) fn lookup(&self, name: &str) -> Option<LocalId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    pub(crate) fn ty(&self, id: LocalId) -> TypeRef {
        self.locals[id.0 as usize].ty
    }

    /// Opens a block scope over `span`.
    pub(crate) fn open(&mut self, span: Span) {
        self.scopes.push(HashMap::new());
        self.scope_spans.push(span);
    }

    /// Closes the innermost block scope.
    pub(crate) fn close(&mut self) {
        self.scopes.pop();
        self.scope_spans.pop();
    }

    /// The range `id` is in scope over.
    pub(crate) fn scope_of(&self, id: LocalId) -> Span {
        self.local_scopes
            .get(id.0 as usize)
            .copied()
            .unwrap_or_else(Span::dummy)
    }

    /// What declares `id`.
    pub(crate) fn kind_of(&self, id: LocalId) -> LocalKind {
        if self.signature_only.contains(&id) {
            LocalKind::Signature
        } else if self.params.contains(&id) {
            LocalKind::Param
        } else {
            LocalKind::Local
        }
    }

    /// The name token of `id`'s declaration (slots begin at their name).
    pub(crate) fn decl_span(&self, id: LocalId) -> Span {
        let local = &self.locals[id.0 as usize];
        leading(local.span, &local.name)
    }
}

/// `flag = 1` (or `0`): a function that can fail sets its flag out at
/// entry, and `fail` clears it.
pub(crate) fn set_flag(flag: LocalId, set: bool, span: Span) -> HirStmt {
    let ty = Builtin::Bit8.value();
    HirStmt::Assign {
        place: local_expr(flag, ty, span),
        value: HirExpr {
            ty,
            span,
            kind: HirExprKind::Number(if set { "0x1" } else { "0x0" }.to_owned()),
        },
        old: HirOld::Full,
    }
}

/// A local place expression.
pub(crate) fn local_expr(id: LocalId, ty: TypeRef, span: Span) -> HirExpr {
    HirExpr {
        ty,
        span,
        kind: HirExprKind::Local(id),
    }
}

/// A hidden local moved out (it is never read again).
pub(crate) fn moved_local(id: LocalId, ty: TypeRef, span: Span) -> HirExpr {
    HirExpr {
        ty,
        span,
        kind: HirExprKind::Move(Box::new(local_expr(id, ty, span))),
    }
}

impl Checker<'_> {
    /// Checks one queued instance's body and records its `HirFunc`.
    pub(crate) fn check_instance(&mut self, key: &FuncKey) {
        let Some(instance) = self.instances.get(key) else {
            return;
        };
        if instance.state != State::Queued {
            return;
        }
        let sig = instance.sig.clone();
        let def = self.res.def(key.def);
        let Some(decl) = def.func().cloned() else {
            return;
        };
        let span = def.span;
        let env = self.env_of(key.def, key.args.clone(), Some(key.clone()));
        let outer = self.diags.hold();
        self.entry_unresolved = false;
        let func = self.with_env(env, |c| {
            let mut func = c.check_func(key, &decl, &sig, span);
            // Ownership is decided on bodies that checked: an error left
            // poison and error types it would only repeat.
            if !c.diags.holds_error() {
                c.check_places(&mut func);
            }
            func
        });
        let held = self.diags.release(outer);
        if std::mem::take(&mut self.entry_unresolved) {
            // An `@entry` naming a missing root item, outside an executable:
            // nothing enters the program, so the hook is left out.
            if let Some(instance) = self.instances.get_mut(key) {
                instance.state =
                    State::Unusable(ErrorGuaranteed::unreported(Unreported::DroppedEntry));
            }
            self.report(held);
            return;
        }
        let failed = has_error(&held);
        if let Some(instance) = self.instances.get_mut(key) {
            instance.state = if failed {
                State::BodyFailed
            } else {
                State::Done
            };
        }
        self.settle(key, held);
        self.out.funcs.push(func);
    }

    fn check_func(&mut self, key: &FuncKey, decl: &FuncDecl, sig: &FuncSig, span: Span) -> HirFunc {
        let mut fcx = FnCx::new(span);
        let slot = |c: &mut Self, fcx: &mut FnCx, param: &Param, ty: TypeRef, is_param: bool| {
            let name = if param.name.is_empty() && param.ty.is_self() {
                "self".to_owned()
            } else {
                param.name.clone()
            };
            let id = fcx.hidden(ty, param.span);
            fcx.locals[id.0 as usize].name.clone_from(&name);
            if !name.is_empty() {
                if is_param {
                    fcx.params.insert(id);
                }
                if !is_param || decl.body.is_none() {
                    fcx.signature_only.insert(id);
                }
                let decl_span = fcx.decl_span(id);
                c.record_local(fcx, id, decl_span);
                if fcx.scopes[0].insert(name.clone(), id).is_some() {
                    c.diags.emit(
                        Diagnostic::error(codes::E020, format!("duplicate definition of `{name}`"))
                            .label(param.span, true, "redefined here"),
                    );
                }
            }
            id
        };
        let params: Vec<LocalId> = decl
            .params
            .iter()
            .zip(&sig.params)
            .map(|(param, ty)| slot(self, &mut fcx, param, *ty, true))
            .collect();
        let mut returns: Vec<LocalId> = decl
            .returns
            .iter()
            .chain(decl.failures.iter().flatten())
            .zip(&sig.returns)
            .map(|(param, ty)| slot(self, &mut fcx, param, *ty, false))
            .collect();
        let failure = match (&decl.failures, sig.returns.get(returns.len())) {
            (Some(_), Some(ty)) => {
                let flag = fcx.hidden(*ty, span);
                returns.push(flag);
                fcx.fail_flag = Some(flag);
                Some(HirFailure {
                    success: decl.returns.len(),
                    flag,
                })
            }
            _ => None,
        };
        let body = match &decl.body {
            None => None,
            Some(FuncBody::Block(block)) => {
                let mut stmts = Vec::new();
                if let Some(failure) = failure {
                    // Set until a `fail` clears it.
                    stmts.push(set_flag(failure.flag, true, block.span));
                }
                self.check_stmts(&mut fcx, &block.stmts, &mut stmts);
                Some(HirBlock {
                    stmts,
                    span: block.span,
                })
            }
            Some(FuncBody::Inline(expr)) => Some(self.check_inline_body(&mut fcx, expr, &returns)),
        };
        HirFunc {
            key: key.clone(),
            params,
            returns,
            failure,
            locals: fcx.locals,
            body,
            inline: matches!(decl.body, Some(FuncBody::Inline(_))),
            flags: Vec::new(),
            span,
        }
    }

    /// `=> expr` is `{ out = expr }` over the single return slot.
    fn check_inline_body(&mut self, fcx: &mut FnCx, expr: &Expr, returns: &[LocalId]) -> HirBlock {
        let span = expr.span();
        let [out] = returns else {
            self.diags.emit(
                Diagnostic::error(
                    codes::E022,
                    "inline `=>` bodies need exactly one return type",
                )
                .label(span, true, "here"),
            );
            return HirBlock {
                stmts: Vec::new(),
                span,
            };
        };
        let ty = fcx.ty(*out);
        let value = self.check_expr(fcx, expr, Some(ty.id));
        let value = self.coerce(value, ty, span);
        HirBlock {
            stmts: vec![HirStmt::Assign {
                place: local_expr(*out, ty, span),
                value,
                old: HirOld::Full,
            }],
            span,
        }
    }

    /// Checks a `{ }` block as a new scope.
    pub(crate) fn check_block(&mut self, fcx: &mut FnCx, block: &Block) -> HirBlock {
        fcx.open(block.span);
        let mut stmts = Vec::new();
        self.check_stmts(fcx, &block.stmts, &mut stmts);
        fcx.close();
        HirBlock {
            stmts,
            span: block.span,
        }
    }

    /// Checks a block's statements in order. The rest of the block after a
    /// statement that never finishes is still checked, and recorded as
    /// unreachable (`HirProgram::unreachable`).
    fn check_stmts(&mut self, fcx: &mut FnCx, stmts: &[Stmt], out: &mut Vec<HirStmt>) {
        let mut reached = true;
        for (index, stmt) in stmts.iter().enumerate() {
            self.check_stmt(fcx, stmt, out);
            if reached && never_finishes(stmt) {
                reached = false;
                if let (Some(first), Some(last)) = (stmts.get(index + 1), stmts.last()) {
                    let (first, last) = (first.span(), last.span());
                    self.out
                        .unreachable
                        .push(Span::new(first.lo.0, last.hi.0, first.file));
                }
            }
        }
    }

    /// Checks one statement as its own scope (`else <stmt>`, `loop <stmt>`).
    pub(crate) fn check_stmt_block(&mut self, fcx: &mut FnCx, stmt: &Stmt) -> HirBlock {
        if let Stmt::Block(block) = stmt {
            return self.check_block(fcx, block);
        }
        fcx.open(stmt.span());
        let mut stmts = Vec::new();
        self.check_stmt(fcx, stmt, &mut stmts);
        fcx.close();
        HirBlock {
            stmts,
            span: stmt.span(),
        }
    }

    /// Records a use of `local` at `span` for the editor: its token and
    /// its link to the declaration.
    pub(crate) fn record_local_use(&mut self, fcx: &FnCx, local: LocalId, span: Span) {
        let kind = if fcx.params.contains(&local) {
            SemanticKind::Param
        } else {
            SemanticKind::Local
        };
        self.record_semantic(span, kind);
        self.record_local(fcx, local, span);
    }

    /// Declares `name` in the innermost scope (`E020` when already declared
    /// there).
    pub(crate) fn declare(
        &mut self,
        fcx: &mut FnCx,
        name: &str,
        ty: TypeRef,
        span: Span,
    ) -> LocalId {
        let id = fcx.hidden(ty, span);
        name.clone_into(&mut fcx.locals[id.0 as usize].name);
        self.record_semantic(leading(span, name), SemanticKind::Local);
        let scope = fcx.scopes.last_mut().expect("a scope is always open");
        let previous = scope.insert(name.to_owned(), id);
        self.record_local(fcx, id, leading(span, name));
        if let Some(previous) = previous {
            let previous = fcx.locals[previous.0 as usize].span;
            self.diags.emit(
                Diagnostic::error(codes::E020, format!("duplicate definition of `{name}`"))
                    .label(span, true, "redefined in the same block")
                    .label(previous, false, "first defined here")
                    .help("use a nested block to shadow it, or pick another name"),
            );
        }
        id
    }

    #[allow(clippy::too_many_lines)]
    fn check_stmt(&mut self, fcx: &mut FnCx, stmt: &Stmt, out: &mut Vec<HirStmt>) {
        match stmt {
            Stmt::Let {
                name,
                ty,
                value,
                span,
            } => {
                let declared = match value {
                    Expr::Construct { function, .. } => self.construct_type(ty, function),
                    _ => self.resolve_type(ty),
                };
                // A declared type that failed stays the expectation: an error
                // type, so a tag or literal does not ask for one.
                let expected = Some(declared.id);
                let value_hir = self.check_expr(fcx, value, expected);
                let value_hir = self.coerce(value_hir, declared, ty.span);
                let local = self.declare(fcx, name, declared, *span);
                out.push(HirStmt::Let {
                    local,
                    value: value_hir,
                });
            }
            Stmt::MultiLet {
                bindings,
                value,
                span,
            } => self.check_multi_let(fcx, bindings, value, *span, out),
            Stmt::Assign {
                target,
                value,
                span,
            } => self.check_assign(fcx, target, value, *span, out),
            Stmt::ExprStmt(expr) => self.check_expr_stmt(fcx, expr, out),
            Stmt::Return { span } => out.push(HirStmt::Return(*span)),
            Stmt::Fail { span } => self.check_fail(fcx, *span, out),
            Stmt::Guard {
                bindings,
                value,
                captures,
                els,
                span,
            } => self.check_guard(fcx, bindings, value, captures, els, *span, out),
            Stmt::Break { span } => {
                if fcx.loops == 0 {
                    self.diags.emit(
                        Diagnostic::error(codes::E022, "break outside `loop`").label(
                            *span,
                            true,
                            "break needs an enclosing loop",
                        ),
                    );
                }
                out.push(HirStmt::Break(*span));
            }
            Stmt::Continue { span } => {
                if fcx.loops == 0 {
                    self.diags.emit(
                        Diagnostic::error(codes::E022, "continue outside `loop`").label(
                            *span,
                            true,
                            "continue needs an enclosing loop",
                        ),
                    );
                }
                out.push(HirStmt::Continue(*span));
            }
            Stmt::If {
                cond,
                captures,
                then,
                else_captures,
                els,
                span,
            } => self.check_if(
                fcx,
                cond,
                captures,
                then,
                else_captures,
                els.as_deref(),
                *span,
                out,
            ),
            Stmt::Loop { body, .. } => {
                fcx.loops += 1;
                let body = self.check_stmt_block(fcx, body);
                fcx.loops -= 1;
                out.push(HirStmt::Loop(body));
            }
            Stmt::Match {
                scrutinee,
                arms,
                els,
                span,
            } => {
                let scrutinee = self.check_scrutinee(fcx, scrutinee);
                let arms = arms
                    .iter()
                    .map(|arm| {
                        fcx.open(arm.span);
                        let (pattern, capture) =
                            self.check_pattern(fcx, arm, arms, scrutinee.ty.id);
                        let body = match &arm.body {
                            MatchArmBody::Block(block) => self.check_block(fcx, block),
                            MatchArmBody::Inline(stmt) => self.check_stmt_block(fcx, stmt),
                        };
                        fcx.close();
                        HirArm {
                            pattern,
                            capture,
                            moves: false,
                            body,
                        }
                    })
                    .collect();
                let els = match els {
                    Some(els) => self.check_block(fcx, els),
                    None => empty_block(*span),
                };
                out.push(HirStmt::Match {
                    scrutinee,
                    arms,
                    els,
                });
            }
            Stmt::Block(block) => {
                let block = self.check_block(fcx, block);
                out.push(HirStmt::Block(block));
            }
            Stmt::Error { .. } => {}
        }
    }

    /// `target = value`. A `ref T` target receiving a `T` value writes
    /// through the reference; receiving a `ref T` it is rebound.
    fn check_assign(
        &mut self,
        fcx: &mut FnCx,
        target: &Expr,
        value: &Expr,
        span: Span,
        out: &mut Vec<HirStmt>,
    ) {
        let Ok(place) = self.check_place(fcx, target) else {
            let _ = self.check_expr(fcx, value, Some(TypeId::ERROR));
            return;
        };
        if place.ty.is_error() {
            let _ = self.check_expr(fcx, value, Some(TypeId::ERROR));
            return;
        }
        let value_hir = self.check_expr(fcx, value, Some(place.ty.id));
        let (place, want) = if place.ty.is_ref && !value_hir.ty.is_ref {
            let ty = place.ty.deref();
            (
                HirExpr {
                    ty,
                    span: place.span,
                    kind: HirExprKind::Deref(Box::new(place)),
                },
                ty,
            )
        } else {
            let ty = place.ty;
            (place, ty)
        };
        let value_hir = self.coerce(value_hir, want, target.span());
        let _ = span;
        out.push(HirStmt::Assign {
            place,
            value: value_hir,
            old: HirOld::Full,
        });
    }

    /// An expression statement. A call's outs are dropped at its end, and a
    /// value with no effect is recorded for the editor
    /// (`HirProgram::no_effect`).
    fn check_expr_stmt(&mut self, fcx: &mut FnCx, expr: &Expr, out: &mut Vec<HirStmt>) {
        let Expr::Call {
            callee,
            generic_args,
            args,
            span,
        } = expr
        else {
            let hir = self.check_expr(fcx, expr, None);
            self.note_no_effect(&hir);
            out.push(HirStmt::Expr(hir));
            return;
        };
        match self.check_call(fcx, callee, generic_args, args, *span, None) {
            Ok(Called::Call { callee, args, outs }) => {
                let (ty, rest) = match outs.split_first() {
                    Some((&first, rest)) => (first, rest.to_vec()),
                    None => (TypeRef::ERROR, Vec::new()),
                };
                out.push(HirStmt::Expr(HirExpr {
                    ty,
                    span: *span,
                    kind: HirExprKind::Call { callee, args, rest },
                }));
            }
            Ok(Called::Value(hir)) => {
                self.note_no_effect(&hir);
                out.push(HirStmt::Expr(hir));
            }
            Err(_) => {}
        }
    }

    /// Records an expression statement whose value has no effect.
    fn note_no_effect(&mut self, hir: &HirExpr) {
        if hir.has_no_effect() && !hir.ty.is_error() {
            self.out.no_effect.push(hir.span);
        }
    }

    /// `a: T, b: U = f()`.
    fn check_multi_let(
        &mut self,
        fcx: &mut FnCx,
        bindings: &[lilac_ast::LetBinding],
        value: &Expr,
        span: Span,
        out: &mut Vec<HirStmt>,
    ) {
        let declared: Vec<TypeRef> = bindings.iter().map(|b| self.resolve_type(&b.ty)).collect();
        let Expr::Call {
            callee,
            generic_args,
            args,
            span: call_span,
        } = value
        else {
            self.diags.emit(
                Diagnostic::error(
                    codes::E022,
                    format!(
                        "multi-`let` needs a function call with {} returns",
                        bindings.len()
                    ),
                )
                .label(span, true, "not a call"),
            );
            return;
        };
        let Ok(Called::Call { callee, args, outs }) =
            self.check_call(fcx, callee, generic_args, args, *call_span, None)
        else {
            return;
        };
        // The bindings take the outs left to right; outs past them drop.
        if let Some(extra) = bindings.get(outs.len()) {
            self.diags.emit(
                Diagnostic::error(
                    codes::E022,
                    format!(
                        "{} values are bound, but the call returns {}",
                        bindings.len(),
                        outs.len()
                    ),
                )
                .label(extra.span, true, "no value for this binding")
                .label(*call_span, false, "this call"),
            );
            return;
        }
        let call = HirExpr {
            ty: outs[0],
            span: *call_span,
            kind: HirExprKind::Call {
                callee,
                args,
                rest: outs[1..].to_vec(),
            },
        };
        let exact = outs
            .iter()
            .zip(&declared)
            .all(|(got, want)| got == want || got.is_error() || want.is_error());
        if exact {
            let locals = bindings
                .iter()
                .zip(&declared)
                .map(|(binding, ty)| self.declare(fcx, &binding.name, *ty, binding.span))
                .collect();
            out.push(HirStmt::MultiLet { locals, call });
            return;
        }
        let hidden: Vec<LocalId> = outs[..bindings.len()]
            .iter()
            .map(|ty| fcx.hidden(*ty, *call_span))
            .collect();
        out.push(HirStmt::MultiLet {
            locals: hidden.clone(),
            call,
        });
        for ((binding, want), (got, local)) in
            bindings.iter().zip(&declared).zip(outs.iter().zip(&hidden))
        {
            let value = self.coerce(
                moved_local(*local, *got, binding.span),
                *want,
                binding.ty.span,
            );
            let local = self.declare(fcx, &binding.name, *want, binding.span);
            out.push(HirStmt::Let { local, value });
        }
    }

    /// Checks a condition: exactly `@bit8`, or one `@conversion` to it.
    pub(crate) fn check_cond(&mut self, fcx: &mut FnCx, cond: &Expr, what: &str) -> HirExpr {
        let bit8 = Builtin::Bit8.id();
        let expected = matches!(cond, Expr::Number(..)).then_some(bit8);
        let hir = self.check_expr(fcx, cond, expected);
        self.cond_of(hir, what)
    }

    /// Turns a checked value into a `@bit8` condition.
    pub(crate) fn cond_of(&mut self, hir: HirExpr, what: &str) -> HirExpr {
        if hir.ty.is_error() {
            return hir;
        }
        let hir = Self::auto_deref(hir);
        let bit8 = Builtin::Bit8.id();
        if hir.ty.id == bit8 {
            return hir;
        }
        if let Some(converted) = self.try_convert(hir.clone(), bit8) {
            return converted;
        }
        let found = self.show_id(hir.ty.id);
        self.emit_involving(
            Diagnostic::error(
                codes::E010,
                format!("`{what}` condition must be `@bit8`, found {found}"),
            )
            .label(hir.span, true, "not `@bit8`")
            .help(format!(
                "define `@conversion: function (from: {found}) to: @bit8` or compare explicitly"
            )),
            &[hir.ty.id],
        );
        HirExpr::poison(hir.span)
    }

    /// A `match` scrutinee: references are matched through.
    pub(crate) fn check_scrutinee(&mut self, fcx: &mut FnCx, scrutinee: &Expr) -> HirExpr {
        let bit8 = Builtin::Bit8.id();
        let expected = matches!(scrutinee, Expr::Number(..)).then_some(bit8);
        let hir = self.check_expr(fcx, scrutinee, expected);
        Self::auto_deref(hir)
    }

    /// Records the tag of `arm`, at `span`: the variants of `scrutinee`
    /// the other `arms` do not name.
    fn record_pattern_tag(
        &mut self,
        arm: &MatchArm,
        arms: &[MatchArm],
        span: Span,
        scrutinee: TypeId,
    ) {
        let taken: Vec<&str> = arms
            .iter()
            .filter(|other| !std::ptr::eq(*other, arm))
            .filter_map(|other| match &other.pattern {
                MatchPattern::Tag { name, .. } => Some(name.as_str()),
                MatchPattern::Const(_) => None,
            })
            .collect();
        self.record_tag(span, Some(scrutinee), &taken);
    }

    /// Checks one arm pattern of `arms` and declares its capture in the
    /// current scope.
    pub(crate) fn check_pattern(
        &mut self,
        fcx: &mut FnCx,
        arm: &MatchArm,
        arms: &[MatchArm],
        scrutinee: TypeId,
    ) -> (HirPattern, Option<LocalId>) {
        let poison = (HirPattern::Variant(u32::MAX), None);
        match &arm.pattern {
            MatchPattern::Tag {
                qualifier,
                name,
                span,
            } => {
                self.record_pattern_tag(arm, arms, *span, scrutinee);
                if name.is_empty() {
                    // The parser reported the `.` with no name.
                    self.empty_names += 1;
                    return poison;
                }
                if let Some(qualifier) = qualifier {
                    let named = self.resolve_type(qualifier);
                    if !named.is_error() && scrutinee != TypeId::ERROR && named.id != scrutinee {
                        self.diags.emit(
                            Diagnostic::error(
                                codes::E010,
                                format!(
                                    "pattern names `{}`, but the match inspects `{}`",
                                    self.show_id(named.id),
                                    self.show_id(scrutinee)
                                ),
                            )
                            .label(qualifier.span, true, "wrong enum"),
                        );
                        return poison;
                    }
                }
                let Ok((index, payload)) = self.variant(scrutinee, name, arm.span) else {
                    return poison;
                };
                let Some(capture) = &arm.capture else {
                    return (HirPattern::Variant(index), None);
                };
                let declared = self.resolve_type(&capture.ty);
                match payload {
                    Some(payload) => {
                        if !payload.is_error() && !declared.is_error() && payload != declared {
                            self.diags.emit(
                                Diagnostic::error(
                                    codes::E010,
                                    format!(
                                        "mismatched types: capture holds {}, declared {}",
                                        self.show(payload),
                                        self.show(declared)
                                    ),
                                )
                                .label(
                                    capture.span,
                                    true,
                                    "capture type here",
                                ),
                            );
                        }
                    }
                    None => {
                        self.diags.emit(
                            Diagnostic::error(
                                codes::E022,
                                format!("variant `.{name}` carries no payload to capture"),
                            )
                            .label(
                                capture.span,
                                true,
                                "payload-less variant",
                            ),
                        );
                    }
                }
                let local = self.declare(fcx, &capture.name, declared, capture.span);
                (HirPattern::Variant(index), Some(local))
            }
            MatchPattern::Const(bytes) => {
                if scrutinee != TypeId::ERROR && !self.is_bit(scrutinee) {
                    self.diags.emit(
                        Diagnostic::error(
                            codes::E010,
                            format!(
                                "byte patterns need a bit container scrutinee, found {}",
                                self.show_id(scrutinee)
                            ),
                        )
                        .label(arm.span, true, "not a bit container"),
                    );
                }
                if arm.capture.is_some() {
                    self.diags.emit(
                        Diagnostic::error(codes::E022, "byte patterns capture nothing").label(
                            arm.span,
                            true,
                            "captures need `.Tag` patterns",
                        ),
                    );
                }
                (HirPattern::Const(bytes.clone()), None)
            }
        }
    }

    /// Resolves variant `name` of enum type `ty`: its index and payload type.
    pub(crate) fn variant(
        &mut self,
        ty: TypeId,
        name: &str,
        span: Span,
    ) -> Result<(u32, Option<TypeRef>), lilac_error::ErrorGuaranteed> {
        if ty == TypeId::ERROR {
            // Whatever made the type an error reported it.
            return Err(self.diags.already_reported());
        }
        let Some(variants) = self.variants_of(ty) else {
            return Err(self.diags.emit(
                Diagnostic::error(codes::E010, format!("tag `.{name}` needs an enum type")).label(
                    span,
                    true,
                    "expected a tag here",
                ),
            ));
        };
        let Some(index) = variants.iter().position(|(variant, _)| variant == name) else {
            let owner = self.show_id(ty);
            let mut diagnostic = Diagnostic::error(
                codes::E021,
                format!("no variant `{name}` on `{owner}`"),
            )
            .label(span, true, "no such variant");
            // Only a span holding just the name can take the fix.
            if (span.hi.0 - span.lo.0) as usize == name.len() {
                let names = variants.iter().map(|(variant, _)| variant.as_str());
                diagnostic = crate::suggest::suggest(diagnostic, name, span, names);
            }
            return Err(self.diags.emit(diagnostic));
        };
        Ok((u32::try_from(index).unwrap_or(u32::MAX), variants[index].1))
    }
}

pub(crate) fn empty_block(span: Span) -> HirBlock {
    HirBlock {
        stmts: Vec::new(),
        span,
    }
}

/// Whether `stmt` never lets the block go on: `return`, `fail`, `break`,
/// `continue`, or a call of `@panic` or `@trap`.
pub(crate) fn never_finishes(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return { .. } | Stmt::Fail { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {
            true
        }
        Stmt::ExprStmt(Expr::Call { callee, .. }) => {
            matches!(&**callee, Expr::Ident(name, _) if name == "@panic" || name == "@trap")
        }
        _ => false,
    }
}
