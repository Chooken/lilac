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
use lilac_error::Diagnostic;
use lilac_hir::{
    FuncKey, HirArm, HirBlock, HirCallee, HirExpr, HirExprKind, HirFunc, HirLocal, HirPattern,
    HirStmt, LocalId, SemanticKind,
};
use lilac_span::Span;
use lilac_ty::{FuncSig, TypeId, TypeRef};

use crate::calls::Called;
use crate::sigs::leading;
use crate::{Checker, State};

/// Per-function checking state.
pub(crate) struct FnCx {
    /// Every local, indexed by [`LocalId`].
    pub locals: Vec<HirLocal>,
    /// Block scopes, innermost last.
    pub scopes: Vec<HashMap<String, LocalId>>,
    /// Parameter locals (colored as parameters).
    pub params: HashSet<LocalId>,
    /// `loop` nesting depth.
    pub loops: u32,
}

impl FnCx {
    pub(crate) fn new() -> Self {
        Self {
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
            params: HashSet::new(),
            loops: 0,
        }
    }

    /// A fresh local not bound to any name.
    pub(crate) fn hidden(&mut self, ty: TypeRef, span: Span) -> LocalId {
        let id = LocalId(u32::try_from(self.locals.len()).unwrap_or(u32::MAX));
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

    /// The name token of `id`'s declaration (slots begin at their name).
    pub(crate) fn decl_span(&self, id: LocalId) -> Span {
        let local = &self.locals[id.0 as usize];
        leading(local.span, &local.name)
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
        let mark = self.diags.checkpoint();
        self.entry_unresolved = false;
        let func = self.with_env(env, |c| c.check_func(key, &decl, &sig, span));
        if std::mem::take(&mut self.entry_unresolved) {
            // An `@entry` naming a missing root item, outside an executable:
            // nothing enters the program, so the hook is left out.
            if let Some(instance) = self.instances.get_mut(key) {
                instance.state = State::Failed;
            }
            return;
        }
        let failed = self.diags.diagnostics()[mark.min(self.diags.checkpoint())..]
            .iter()
            .any(|d| d.level == lilac_error::Level::Error);
        if let Some(instance) = self.instances.get_mut(key) {
            instance.state = if failed { State::Failed } else { State::Done };
        }
        if failed && !key.args.is_empty() {
            self.instance_failure(key, mark);
        }
        self.out.funcs.push(func);
    }

    fn check_func(&mut self, key: &FuncKey, decl: &FuncDecl, sig: &FuncSig, span: Span) -> HirFunc {
        let mut fcx = FnCx::new();
        let slot = |c: &mut Self, fcx: &mut FnCx, param: &Param, ty: TypeRef, is_param: bool| {
            let name = if param.name.is_empty() && param.ty.name == "self" {
                "self".to_owned()
            } else {
                param.name.clone()
            };
            let id = fcx.hidden(ty, param.span);
            fcx.locals[id.0 as usize].name.clone_from(&name);
            if !name.is_empty() {
                let decl = fcx.decl_span(id);
                c.record_local(decl, decl);
                if fcx.scopes[0].insert(name.clone(), id).is_some() {
                    c.diags.emit(
                        Diagnostic::error("E020", format!("duplicate definition of `{name}`"))
                            .label(param.span, true, "redefined here"),
                    );
                }
                if is_param {
                    fcx.params.insert(id);
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
        let returns: Vec<LocalId> = decl
            .returns
            .iter()
            .zip(&sig.returns)
            .map(|(param, ty)| slot(self, &mut fcx, param, *ty, false))
            .collect();
        let body = match &decl.body {
            None => None,
            Some(FuncBody::Block(block)) => {
                let mut stmts = Vec::new();
                for stmt in &block.stmts {
                    self.check_stmt(&mut fcx, stmt, &mut stmts);
                }
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
            locals: fcx.locals,
            body,
            inline: matches!(decl.body, Some(FuncBody::Inline(_))),
            span,
        }
    }

    /// `=> expr` is `{ out = expr }` over the single return slot.
    fn check_inline_body(&mut self, fcx: &mut FnCx, expr: &Expr, returns: &[LocalId]) -> HirBlock {
        let span = expr.span();
        let [out] = returns else {
            self.diags.emit(
                Diagnostic::error("E022", "inline `=>` bodies need exactly one return type")
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
            }],
            span,
        }
    }

    /// Checks a `{ }` block as a new scope.
    pub(crate) fn check_block(&mut self, fcx: &mut FnCx, block: &Block) -> HirBlock {
        fcx.scopes.push(HashMap::new());
        let mut stmts = Vec::new();
        for stmt in &block.stmts {
            self.check_stmt(fcx, stmt, &mut stmts);
        }
        fcx.scopes.pop();
        HirBlock {
            stmts,
            span: block.span,
        }
    }

    /// Checks one statement as its own scope (`else <stmt>`, `loop <stmt>`).
    fn check_stmt_block(&mut self, fcx: &mut FnCx, stmt: &Stmt) -> HirBlock {
        if let Stmt::Block(block) = stmt {
            return self.check_block(fcx, block);
        }
        fcx.scopes.push(HashMap::new());
        let mut stmts = Vec::new();
        self.check_stmt(fcx, stmt, &mut stmts);
        fcx.scopes.pop();
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
        self.record_local(span, fcx.decl_span(local));
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
        self.record_local(leading(span, name), leading(span, name));
        let scope = fcx.scopes.last_mut().expect("a scope is always open");
        if let Some(previous) = scope.insert(name.to_owned(), id) {
            let previous = fcx.locals[previous.0 as usize].span;
            self.diags.emit(
                Diagnostic::error("E020", format!("duplicate definition of `{name}`"))
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
                let declared = self.resolve_type(ty);
                let expected = (!declared.is_error()).then_some(declared.id);
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
            Stmt::Break { span } => {
                if fcx.loops == 0 {
                    self.diags
                        .emit(Diagnostic::error("E022", "break outside `loop`").label(
                            *span,
                            true,
                            "break needs an enclosing loop",
                        ));
                }
                out.push(HirStmt::Break(*span));
            }
            Stmt::Continue { span } => {
                if fcx.loops == 0 {
                    self.diags
                        .emit(Diagnostic::error("E022", "continue outside `loop`").label(
                            *span,
                            true,
                            "continue needs an enclosing loop",
                        ));
                }
                out.push(HirStmt::Continue(*span));
            }
            Stmt::If {
                cond,
                capture: Some(capture),
                then,
                els,
                span,
            } => self.check_if_capture(fcx, cond, capture, then, els.as_deref(), *span, out),
            Stmt::If {
                cond,
                capture: None,
                then,
                els,
                span,
            } => {
                let cond = self.check_cond(fcx, cond, "if");
                let then = self.check_block(fcx, then);
                let els = match els {
                    Some(els) => self.check_stmt_block(fcx, els),
                    None => empty_block(*span),
                };
                out.push(HirStmt::If { cond, then, els });
            }
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
                        fcx.scopes.push(HashMap::new());
                        let (pattern, capture) = self.check_pattern(fcx, arm, scrutinee.ty.id);
                        let body = match &arm.body {
                            MatchArmBody::Block(block) => self.check_block(fcx, block),
                            MatchArmBody::Inline(stmt) => self.check_stmt_block(fcx, stmt),
                        };
                        fcx.scopes.pop();
                        HirArm {
                            pattern,
                            capture,
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
            let _ = self.check_expr(fcx, value, None);
            return;
        };
        if place.ty.is_error() {
            let _ = self.check_expr(fcx, value, None);
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
        });
    }

    /// An expression statement: a call's results may not be dropped silently.
    fn check_expr_stmt(&mut self, fcx: &mut FnCx, expr: &Expr, out: &mut Vec<HirStmt>) {
        let Expr::Call {
            callee,
            generic_args,
            args,
            span,
        } = expr
        else {
            let hir = self.check_expr(fcx, expr, None);
            out.push(HirStmt::Expr(hir));
            return;
        };
        match self.check_call(fcx, callee, generic_args, args, *span, None) {
            Ok(Called::Call { callee, args, outs }) => {
                if !outs.is_empty() {
                    self.diags.emit(
                        Diagnostic::error(
                            "E022",
                            format!(
                                "call returns {} value{}, but the statement drops them",
                                outs.len(),
                                if outs.len() == 1 { "" } else { "s" },
                            ),
                        )
                        .label(*span, true, "assign or drop explicitly"),
                    );
                }
                out.push(HirStmt::Expr(HirExpr {
                    ty: outs.first().copied().unwrap_or(TypeRef::ERROR),
                    span: *span,
                    kind: HirExprKind::Call { callee, args },
                }));
            }
            Ok(Called::Value(hir)) => {
                if !matches!(
                    hir.kind,
                    HirExprKind::CompileError { .. } | HirExprKind::Poison
                ) {
                    self.diags.emit(
                        Diagnostic::error("E022", "this value is computed and then dropped").label(
                            *span,
                            true,
                            "assign or drop explicitly",
                        ),
                    );
                }
                out.push(HirStmt::Expr(hir));
            }
            Err(_) => {}
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
                    "E022",
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
        if outs.len() != bindings.len() {
            self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!(
                        "wrong number of values: expected {}, call returns {}",
                        bindings.len(),
                        outs.len()
                    ),
                )
                .label(*call_span, true, "arity mismatch here"),
            );
            return;
        }
        let call = HirExpr {
            ty: outs[0],
            span: *call_span,
            kind: HirExprKind::Call { callee, args },
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
        let hidden: Vec<LocalId> = outs.iter().map(|ty| fcx.hidden(*ty, *call_span)).collect();
        out.push(HirStmt::MultiLet {
            locals: hidden.clone(),
            call,
        });
        for ((binding, want), (got, local)) in
            bindings.iter().zip(&declared).zip(outs.iter().zip(&hidden))
        {
            let value = self.coerce(
                local_expr(*local, *got, binding.span),
                *want,
                binding.ty.span,
            );
            let local = self.declare(fcx, &binding.name, *want, binding.span);
            out.push(HirStmt::Let { local, value });
        }
    }

    /// `if f() -> name: T { } else ...`: binds every out slot to a hidden
    /// local, tests the first, and exposes `name` in the `then` block.
    #[allow(clippy::too_many_arguments)]
    fn check_if_capture(
        &mut self,
        fcx: &mut FnCx,
        cond: &Expr,
        capture: &Param,
        then: &Block,
        els: Option<&Stmt>,
        span: Span,
        out: &mut Vec<HirStmt>,
    ) {
        let Expr::Call {
            callee,
            generic_args,
            args,
            span: call_span,
        } = cond
        else {
            self.diags.emit(
                Diagnostic::error("E022", "`if` capture needs a call condition").label(
                    span,
                    true,
                    "write `if f() -> name: Type`",
                ),
            );
            return;
        };
        let Ok(Called::Call { callee, args, outs }) =
            self.check_call(fcx, callee, generic_args, args, *call_span, None)
        else {
            self.diags.emit(
                Diagnostic::error("E022", "cannot capture from this call").label(
                    *call_span,
                    true,
                    "capture needs a multi-return call",
                ),
            );
            return;
        };
        if outs.is_empty() {
            self.diags.emit(
                Diagnostic::error("E022", "cannot capture from a call with no returns").label(
                    *call_span,
                    true,
                    "nothing to capture",
                ),
            );
            return;
        }
        let slot_names: Vec<String> = match &callee {
            HirCallee::Func(key) => self
                .res
                .def(key.def)
                .func()
                .map(|decl| decl.returns.iter().map(|slot| slot.name.clone()).collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let Some(position) = slot_names.iter().position(|name| *name == capture.name) else {
            self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!("no out-slot named `{}` to capture", capture.name),
                )
                .label(capture.span, true, "unknown slot"),
            );
            return;
        };
        let declared = self.resolve_type(&capture.ty);
        let hidden: Vec<LocalId> = outs.iter().map(|ty| fcx.hidden(*ty, *call_span)).collect();
        let mut stmts = vec![HirStmt::MultiLet {
            locals: hidden.clone(),
            call: HirExpr {
                ty: outs[0],
                span: *call_span,
                kind: HirExprKind::Call { callee, args },
            },
        }];
        let test = local_expr(hidden[0], outs[0], *call_span);
        let cond = self.cond_of(test, "if");
        let captured = local_expr(hidden[position], outs[position], capture.span);
        let value = self.coerce(captured, declared, capture.ty.span);
        fcx.scopes.push(HashMap::new());
        let local = self.declare(fcx, &capture.name, declared, capture.span);
        stmts.push(HirStmt::Let { local, value });
        let then = self.check_block(fcx, then);
        fcx.scopes.pop();
        let els = match els {
            Some(els) => self.check_stmt_block(fcx, els),
            None => empty_block(span),
        };
        stmts.push(HirStmt::If { cond, then, els });
        out.push(HirStmt::Block(HirBlock { stmts, span }));
    }

    /// Checks a condition: exactly `@bit8`, or one `@conversion` to it.
    pub(crate) fn check_cond(&mut self, fcx: &mut FnCx, cond: &Expr, what: &str) -> HirExpr {
        let bit8 = self.builtin("@bit8");
        let expected = matches!(cond, Expr::Number(..)).then_some(bit8);
        let hir = self.check_expr(fcx, cond, expected);
        self.cond_of(hir, what)
    }

    /// Turns a checked value into a `@bit8` condition.
    pub(crate) fn cond_of(&mut self, hir: HirExpr, what: &str) -> HirExpr {
        if hir.ty.is_error() {
            return hir;
        }
        let hir = self.auto_deref(hir);
        let bit8 = self.builtin("@bit8");
        if hir.ty.id == bit8 {
            return hir;
        }
        if let Some(converted) = self.try_convert(hir.clone(), bit8) {
            return converted;
        }
        let found = self.types.display(hir.ty.id);
        self.emit_involving(
            Diagnostic::error(
                "E010",
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
        let bit8 = self.builtin("@bit8");
        let expected = matches!(scrutinee, Expr::Number(..)).then_some(bit8);
        let hir = self.check_expr(fcx, scrutinee, expected);
        self.auto_deref(hir)
    }

    /// Checks one arm pattern and declares its capture in the current scope.
    pub(crate) fn check_pattern(
        &mut self,
        fcx: &mut FnCx,
        arm: &MatchArm,
        scrutinee: TypeId,
    ) -> (HirPattern, Option<LocalId>) {
        let poison = (HirPattern::Variant(u32::MAX), None);
        match &arm.pattern {
            MatchPattern::Tag { qualifier, name } => {
                if let Some(qualifier) = qualifier {
                    let named = self.resolve_type(qualifier);
                    if !named.is_error() && scrutinee != TypeId::ERROR && named.id != scrutinee {
                        self.diags.emit(
                            Diagnostic::error(
                                "E010",
                                format!(
                                    "pattern names `{}`, but the match inspects `{}`",
                                    self.types.display(named.id),
                                    self.types.display(scrutinee)
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
                                    "E010",
                                    format!(
                                        "mismatched types: capture holds {}, declared {}",
                                        self.types.display_ref(payload),
                                        self.types.display_ref(declared)
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
                                "E022",
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
                            "E010",
                            format!(
                                "byte patterns need a bit container scrutinee, found {}",
                                self.types.display(scrutinee)
                            ),
                        )
                        .label(arm.span, true, "not a bit container"),
                    );
                }
                if arm.capture.is_some() {
                    self.diags.emit(
                        Diagnostic::error("E022", "byte patterns capture nothing").label(
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
            return Err(lilac_error::ErrorGuaranteed::new());
        }
        let Some(variants) = self.variants_of(ty) else {
            return Err(self.diags.emit(
                Diagnostic::error("E010", format!("tag `.{name}` needs an enum type")).label(
                    span,
                    true,
                    "expected a tag here",
                ),
            ));
        };
        let Some(index) = variants.iter().position(|(variant, _)| variant == name) else {
            let owner = self.types.display(ty);
            return Err(self.diags.emit(
                Diagnostic::error("E021", format!("no variant `{name}` on `{owner}`")).label(
                    span,
                    true,
                    "no such variant",
                ),
            ));
        };
        Ok((u32::try_from(index).unwrap_or(u32::MAX), variants[index].1))
    }
}

fn empty_block(span: Span) -> HirBlock {
    HirBlock {
        stmts: Vec::new(),
        span,
    }
}
