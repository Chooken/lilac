//! Body checking: statements and expressions with expected types.

use std::collections::{HashMap, HashSet};

use lilac_ast::{BinOp, Expr, Item, ItemKind, Program, Stmt, Type as AstType};
use lilac_error::{Diagnostic, Diags, ErrorGuaranteed};
use lilac_resolve::{DefId, Module, ResItem, ResKind};
use lilac_span::Span;
use lilac_ty::{FuncSig, TypeId, TypeRef, Types};

use super::{lookup_intrinsic, resolve_type, Intrinsic, Sigs};

/// Checks every non-generic body. Generic items belong to M4; `E052` for
/// generic hooks already fired during signature collection.
pub fn check_bodies(
    program: &Program,
    module: &Module,
    sigs: &Sigs,
    types: &mut Types,
    diags: &mut Diags,
) {
    let mut checker = Checker {
        program,
        module,
        sigs,
        types,
        diags,
        checked: HashSet::new(),
        fields: HashMap::new(),
    };
    checker.collect_fields(&program.items);
    for item in &program.items {
        if let Some(resolved) = checker.module.items.get(&item.name) {
            let resolved = resolved.clone();
            checker.check_item(item, &resolved);
        }
    }
}

struct Checker<'a> {
    program: &'a Program,
    module: &'a Module,
    sigs: &'a Sigs,
    types: &'a mut Types,
    diags: &'a mut Diags,
    checked: HashSet<DefId>,
    fields: HashMap<(String, String), AstType>,
}

struct FuncCtx {
    locals: HashMap<String, TypeRef>,
    slots: Vec<(String, TypeRef, Span)>,
    assigned: HashSet<String>,
}

fn is_poison(ty: TypeRef) -> bool {
    ty.id == TypeId::ERROR
}

fn is_bit(types: &Types, id: TypeId) -> bool {
    matches!(
        types.kind(id),
        lilac_ty::TypeKind::Bit8
            | lilac_ty::TypeKind::Bit16
            | lilac_ty::TypeKind::Bit32
            | lilac_ty::TypeKind::Bit64
            | lilac_ty::TypeKind::BitNative
    )
}

fn deferred(diags: &mut Diags, span: Span, what: &str, milestone: &str) -> ErrorGuaranteed {
    diags.emit(
        Diagnostic::error(
            "E030",
            format!("{what} is not implemented yet ({milestone})"),
        )
        .label(span, true, "lands in a later milestone".to_owned()),
    )
}

impl Checker<'_> {
    fn collect_fields(&mut self, items: &[Item]) {
        for item in items {
            if let ItemKind::Object { members, .. } = &item.kind {
                for member in members {
                    if let ItemKind::Field { ty } = &member.kind {
                        self.fields
                            .insert((item.name.clone(), member.name.clone()), ty.clone());
                    }
                    self.collect_fields(std::slice::from_ref(member));
                }
            }
        }
    }

    fn mismatch(
        &mut self,
        expected: TypeRef,
        found: TypeRef,
        expected_span: Span,
        found_span: Span,
    ) -> ErrorGuaranteed {
        let exp = self.types.display_ref(expected);
        let got = self.types.display_ref(found);
        self.diags.emit(
            Diagnostic::error(
                "E010",
                format!("mismatched types: expected {exp}, found {got}"),
            )
            .label(expected_span, false, format!("expected `{exp}` here"))
            .label(found_span, true, format!("found `{got}` here")),
        )
    }

    /// Unifies an actual value against an expected type: exact first (with
    /// silent auto-deref), then one directed `@conversion` step for non-`ref`
    /// mismatches. `ref` shape mismatches stay `E010`; failed searches are
    /// `E043`. Never chains.
    fn expect_type(
        &mut self,
        actual: TypeRef,
        expected: TypeId,
        expected_span: Span,
        actual_span: Span,
    ) -> Result<(), ErrorGuaranteed> {
        if is_poison(actual) || expected == TypeId::ERROR {
            return Ok(());
        }
        if actual.id == expected && !actual.is_ref {
            return Ok(());
        }
        if actual.is_ref {
            if actual.id == expected {
                return Ok(());
            }
            return Err(self.mismatch(
                TypeRef {
                    id: expected,
                    is_ref: false,
                },
                actual,
                expected_span,
                actual_span,
            ));
        }
        match super::ops::convert(
            self.types,
            self.diags,
            self.sigs,
            actual,
            expected,
            expected_span,
            actual_span,
        ) {
            Ok(_) => Ok(()),
            Err(guaranteed) => Err(guaranteed),
        }
    }

    fn check_item(&mut self, item: &Item, resolved: &ResItem) {
        match &item.kind {
            ItemKind::Func(decl) => self.check_func(item, resolved, decl),
            ItemKind::Object { members, .. } => {
                if let ResKind::Object {
                    members: resolved_members,
                } = &resolved.kind
                {
                    for member in members {
                        if let Some(member_resolved) = resolved_members.get(&member.name) {
                            self.check_item(member, member_resolved);
                        }
                    }
                }
            }
            ItemKind::Const { ty, value, .. } => {
                if !self.checked.insert(resolved.id) {
                    return;
                }
                let declared = resolve_type(ty, self.types, self.module, self.diags);
                let mut fctx = FuncCtx {
                    locals: HashMap::new(),
                    slots: Vec::new(),
                    assigned: HashSet::new(),
                };
                let got = self.check_expr(&mut fctx, value, Some(declared.id));
                let _ = self.expect_type(got, declared.id, ty.span, value.span());
            }
            ItemKind::Enum { .. }
            | ItemKind::Union { .. }
            | ItemKind::Using { .. }
            | ItemKind::Field { .. } => {}
        }
    }

    fn func_sig(&self, def: DefId) -> Option<FuncSig> {
        if let Some(sig) = self.sigs.funcs.get(&def) {
            return Some(sig.clone());
        }
        for overloads in self.sigs.hooks.values() {
            for (id, sig) in overloads {
                if *id == def {
                    return Some(sig.clone());
                }
            }
        }
        None
    }

    fn check_func(&mut self, item: &Item, resolved: &ResItem, decl: &lilac_ast::FuncDecl) {
        if !self.checked.insert(resolved.id) {
            return;
        }
        if matches!(
            &resolved.kind,
            ResKind::Func { generic: true, .. } | ResKind::Hook { generic: true, .. }
        ) {
            return;
        }
        let Some(sig) = self.func_sig(resolved.id) else {
            lilac_error::bug!(item.span, "missing signature for checked function");
        };
        let errors_before = self.diags.diagnostics().len();
        let mut fctx = FuncCtx {
            locals: HashMap::new(),
            slots: Vec::new(),
            assigned: HashSet::new(),
        };
        for (param, ty) in decl.params.iter().zip(sig.params.iter()) {
            if !param.name.is_empty() {
                fctx.locals.insert(param.name.clone(), *ty);
            }
        }
        for (slot, ty) in decl.returns.iter().zip(sig.returns.iter()) {
            if !slot.name.is_empty() {
                fctx.locals.insert(slot.name.clone(), *ty);
                fctx.slots.push((slot.name.clone(), *ty, slot.span));
            }
        }
        match &decl.body {
            None => return,
            Some(lilac_ast::FuncBody::Block(block)) => {
                for stmt in &block.stmts {
                    self.check_stmt(&mut fctx, stmt);
                }
            }
            Some(lilac_ast::FuncBody::Inline(expr)) => {
                if sig.returns.len() != 1 {
                    self.diags.emit(
                        Diagnostic::error(
                            "E022",
                            "inline `=>` bodies need exactly one return type",
                        )
                        .label(expr.span(), true, "here".to_owned()),
                    );
                    return;
                }
                let got = self.check_expr(&mut fctx, expr, Some(sig.returns[0].id));
                let span = expr.span();
                let _ = self.expect_type(got, sig.returns[0].id, span, span);
                if let Some((name, _, _)) = fctx.slots.first() {
                    fctx.assigned.insert(name.clone());
                }
            }
        }
        if self.diags.diagnostics().len() == errors_before {
            for (name, _, span) in &fctx.slots {
                if !fctx.assigned.contains(name) {
                    self.diags.emit(
                        Diagnostic::error(
                            "E027",
                            format!("return slot `{name}` is never assigned"),
                        )
                        .label(
                            *span,
                            true,
                            "assign it before `return`".to_owned(),
                        ),
                    );
                }
            }
        }
    }

    fn place_type(
        &mut self,
        fctx: &mut FuncCtx,
        target: &Expr,
    ) -> Result<TypeRef, ErrorGuaranteed> {
        match target {
            Expr::Ident(name, span) => {
                if let Some(ty) = fctx.locals.get(name) {
                    Ok(*ty)
                } else {
                    Err(self.diags.emit(
                        Diagnostic::error("E021", format!("unresolved name `{name}`")).label(
                            *span,
                            true,
                            "not a local or item".to_owned(),
                        ),
                    ))
                }
            }
            Expr::Member { obj, field, span } => {
                let obj_ty = self.check_expr(fctx, obj, None);
                if is_poison(obj_ty) {
                    return Ok(TypeRef {
                        id: TypeId::ERROR,
                        is_ref: false,
                    });
                }
                self.field_type(obj_ty.id, field, *span)
            }
            _ => Err(self.diags.emit(
                Diagnostic::error("E010", "cannot assign to this expression".to_owned()).label(
                    target.span(),
                    true,
                    "only variables and fields are assignable".to_owned(),
                ),
            )),
        }
    }

    fn field_type(
        &mut self,
        obj: TypeId,
        field: &str,
        span: Span,
    ) -> Result<TypeRef, ErrorGuaranteed> {
        let name = self.types.display(obj);
        if let Some(ty) = self.fields.get(&(name.clone(), field.to_owned())).cloned() {
            Ok(resolve_type(&ty, self.types, self.module, self.diags))
        } else {
            Err(self.diags.emit(
                Diagnostic::error("E021", format!("no field `{field}` on `{name}`")).label(
                    span,
                    true,
                    "unknown member (M4 refines this to E040)".to_owned(),
                ),
            ))
        }
    }

    fn find_by_def(&self, def: DefId) -> Option<ResKind> {
        for item in self.module.items.values() {
            if item.id == def {
                return Some(item.kind.clone());
            }
            if let ResKind::Object { members } = &item.kind {
                for member in members.values() {
                    if member.id == def {
                        return Some(member.kind.clone());
                    }
                }
            }
        }
        None
    }

    fn check_stmt(&mut self, fctx: &mut FuncCtx, stmt: &Stmt) {
        match stmt {
            Stmt::Let {
                name,
                ty,
                value,
                span: _,
            } => {
                let declared = resolve_type(ty, self.types, self.module, self.diags);
                let expected = if is_poison(declared) {
                    None
                } else {
                    Some(declared.id)
                };
                let got = self.check_expr(fctx, value, expected);
                if !is_poison(declared) && !is_poison(got) {
                    let _ = self.expect_type(got, declared.id, ty.span, value.span());
                }
                fctx.locals.insert(name.clone(), declared);
            }
            Stmt::Assign { target, value, .. } => {
                let Ok(target_ty) = self.place_type(fctx, target) else {
                    return;
                };
                if is_poison(target_ty) {
                    let _ = self.check_expr(fctx, value, None);
                    return;
                }
                let base = target_ty.id;
                let got = self.check_expr(fctx, value, Some(base));
                if !is_poison(got) {
                    let _ = self.expect_type(got, base, target.span(), value.span());
                }
                if let Expr::Ident(name, _) = target {
                    if fctx.slots.iter().any(|(slot, _, _)| slot == name) {
                        fctx.assigned.insert(name.clone());
                    }
                }
            }
            Stmt::ExprStmt(expr) => {
                if let Expr::Call {
                    callee,
                    generic_args,
                    args,
                    span,
                } = expr
                {
                    let Ok(outs) = self.check_call(fctx, callee, generic_args, args, *span, None)
                    else {
                        return;
                    };
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
                            .label(
                                *span,
                                true,
                                "assign or drop explicitly".to_owned(),
                            ),
                        );
                    }
                    return;
                }
                let _ = self.check_expr(fctx, expr, None);
            }
            Stmt::Return { .. } | Stmt::Break { .. } => {}
            Stmt::MultiLet { span, .. } => {
                let _ = deferred(self.diags, *span, "multi-`let`", "M2");
            }
            Stmt::If { span, .. } => {
                let _ = deferred(self.diags, *span, "`if` statements", "M2");
            }
            Stmt::Loop { span, .. } => {
                let _ = deferred(self.diags, *span, "`loop` statements", "M2");
            }
            Stmt::Match { span, .. } => {
                let _ = deferred(self.diags, *span, "`match` statements", "M2");
            }
        }
    }

    fn check_expr(&mut self, fctx: &mut FuncCtx, expr: &Expr, expected: Option<TypeId>) -> TypeRef {
        match expr {
            Expr::Ident(name, span) => self.ident_type(fctx, name, *span).unwrap_or(TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            }),
            Expr::Tag(name, span) => self.tag_type(name, *span, expected).unwrap_or(TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            }),
            Expr::Number(_, _) | Expr::Str(_, _) | Expr::Char(_, _) => {
                self.check_literal(expr, expected)
            }
            Expr::RefOf { value, span } => match self.place_type(fctx, value) {
                Ok(place) if place.is_ref => {
                    self.diags.emit(
                        Diagnostic::error("E010", "cannot take `ref` of a reference").label(
                            *span,
                            true,
                            "double references are invalid".to_owned(),
                        ),
                    );
                    TypeRef {
                        id: TypeId::ERROR,
                        is_ref: false,
                    }
                }
                Ok(place) => TypeRef {
                    id: place.id,
                    is_ref: true,
                },
                Err(_) => TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                },
            },
            Expr::Member { obj, field, span } => {
                let obj_ty = self.check_expr(fctx, obj, None);
                if is_poison(obj_ty) {
                    return TypeRef {
                        id: TypeId::ERROR,
                        is_ref: false,
                    };
                }
                self.field_type(obj_ty.id, field, *span).unwrap_or(TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                })
            }
            Expr::Call {
                callee,
                generic_args,
                args,
                span,
            } => self.check_call_expr(fctx, callee, generic_args, args, *span, expected),
            Expr::BinOp { lhs, op, rhs, span } => self.check_binop_expr(fctx, lhs, *op, rhs, *span),
            Expr::IfValue { span, .. } => {
                deferred(self.diags, *span, "`if` values", "M2");
                TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                }
            }
            Expr::MatchValue { span, .. } => {
                deferred(self.diags, *span, "`match` values", "M2");
                TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                }
            }
            Expr::StructLit { span, .. } => {
                deferred(self.diags, *span, "struct literals", "M2");
                TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                }
            }
            Expr::FString { span, .. } => {
                deferred(self.diags, *span, "f-strings", "M3");
                TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                }
            }
        }
    }

    fn check_literal(&mut self, expr: &Expr, expected: Option<TypeId>) -> TypeRef {
        let poison = TypeRef {
            id: TypeId::ERROR,
            is_ref: false,
        };
        match expr {
            Expr::Number(_, span) => match expected {
                Some(t) if is_bit(self.types, t) => TypeRef {
                    id: t,
                    is_ref: false,
                },
                _ => {
                    self.diags.emit(
                        Diagnostic::error("E024", "number literal needs an expected bit type")
                            .label(*span, true, "annotate the binding".to_owned()),
                    );
                    poison
                }
            },
            Expr::Str(_, _) => TypeRef {
                id: self.builtin_ty("@byte_buffer"),
                is_ref: false,
            },
            Expr::Char(_, _) => TypeRef {
                id: self.builtin_ty("@bit32"),
                is_ref: false,
            },
            _ => {
                lilac_error::bug!(expr.span(), "check_literal on non-literal");
            }
        }
    }

    fn check_call_expr(
        &mut self,
        fctx: &mut FuncCtx,
        callee: &Expr,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> TypeRef {
        let poison = TypeRef {
            id: TypeId::ERROR,
            is_ref: false,
        };
        match self.check_call(fctx, callee, generic_args, args, span, expected) {
            Ok(outs) if outs.len() == 1 => outs[0],
            Ok(_) => {
                self.diags.emit(
                    Diagnostic::error(
                        "E022",
                        "call in value position must return exactly one value",
                    )
                    .label(span, true, "one value expected here".to_owned()),
                );
                poison
            }
            Err(_) => poison,
        }
    }

    fn check_binop_expr(
        &mut self,
        fctx: &mut FuncCtx,
        lhs: &Expr,
        op: BinOp,
        rhs: &Expr,
        span: Span,
    ) -> TypeRef {
        let poison = TypeRef {
            id: TypeId::ERROR,
            is_ref: false,
        };
        if matches!(op, BinOp::And | BinOp::Or) {
            deferred(self.diags, span, "`and`/`or`", "M2");
            return poison;
        }
        if op != BinOp::EqEq {
            let Some(hook) = super::ops::binop_hook_name(op) else {
                lilac_error::bug!(span, "equality handled above");
            };
            let (left, right) = self.check_binop_sides(fctx, lhs, rhs);
            if is_poison(left) || is_poison(right) {
                return poison;
            }
            match super::ops::resolve_binop(self.sigs, hook, left.id, right.id) {
                Ok(sig) => {
                    return TypeRef {
                        id: sig.returns[0].id,
                        is_ref: false,
                    };
                }
                Err(miss) => {
                    self.diags.emit(
                        Diagnostic::error(
                            "E043",
                            format!(
                                "no {} for ({}, {}); no conversion from {} to {}",
                                miss.hook,
                                self.types.display(miss.lhs),
                                self.types.display(miss.rhs),
                                self.types.display(miss.rhs),
                                self.types.display(miss.lhs),
                            ),
                        )
                        .label(
                            span,
                            true,
                            "operator and conversion search failed".to_owned(),
                        ),
                    );
                    return poison;
                }
            }
        }
        let (left, right) = self.check_binop_sides(fctx, lhs, rhs);
        if is_poison(left) || is_poison(right) {
            return poison;
        }
        if left.id == right.id {
            return TypeRef {
                id: self.builtin_ty("@bit8"),
                is_ref: false,
            };
        }
        self.diags.emit(
            Diagnostic::error(
                "E010",
                format!(
                    "mismatched types: `==` needs identical types, found {} and {}",
                    self.types.display(left.id),
                    self.types.display(right.id),
                ),
            )
            .label(
                lhs.span(),
                false,
                format!("left side `{}` here", self.types.display(left.id)),
            )
            .label(
                rhs.span(),
                true,
                format!("right side `{}` here", self.types.display(right.id)),
            ),
        );
        poison
    }

    /// Checks both binop sides exactly once: non-literals first to anchor the
    /// width, literals take the anchor (or report `E024` without one).
    fn check_binop_sides(
        &mut self,
        fctx: &mut FuncCtx,
        lhs: &Expr,
        rhs: &Expr,
    ) -> (TypeRef, TypeRef) {
        let poison = TypeRef {
            id: TypeId::ERROR,
            is_ref: false,
        };
        let left_lit = matches!(lhs, Expr::Number(..));
        let right_lit = matches!(rhs, Expr::Number(..));
        if left_lit && right_lit {
            let _ = self.check_expr(fctx, lhs, None);
            return (poison, poison);
        }
        let left = if left_lit {
            None
        } else {
            Some(self.check_expr(fctx, lhs, None))
        };
        let right = if right_lit {
            None
        } else {
            Some(self.check_expr(fctx, rhs, None))
        };
        let anchor = left
            .iter()
            .chain(right.iter())
            .map(|ty| ty.id)
            .find(|id| is_bit(self.types, *id));
        let left = match left {
            Some(ty) => ty,
            None => self.check_expr(fctx, lhs, anchor),
        };
        let right = match right {
            Some(ty) => ty,
            None => self.check_expr(fctx, rhs, anchor),
        };
        (left, right)
    }

    fn check_call(
        &mut self,
        fctx: &mut FuncCtx,
        callee: &Expr,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<Vec<TypeRef>, ErrorGuaranteed> {
        if !generic_args.is_empty() {
            return Err(deferred(self.diags, span, "generic calls", "M4"));
        }
        match callee {
            Expr::Ident(name, name_span) => {
                if fctx.locals.contains_key(name) {
                    return Err(self.diags.emit(
                        Diagnostic::error("E022", format!("`{name}` is not callable")).label(
                            *name_span,
                            true,
                            "only functions can be called".to_owned(),
                        ),
                    ));
                }
                if let Some(item) = self.module.items.get(name).cloned() {
                    return self.check_named_call(fctx, name, &item, args, span);
                }
                if let Some(intrinsic) = lookup_intrinsic(name) {
                    return self.check_intrinsic(fctx, intrinsic, args, span, expected);
                }
                Err(self.diags.emit(
                    Diagnostic::error("E021", format!("unresolved name `{name}`")).label(
                        *name_span,
                        true,
                        "not a local, item, or intrinsic".to_owned(),
                    ),
                ))
            }
            Expr::Member { span, .. } => Err(deferred(self.diags, *span, "member calls", "M2")),
            _ => {
                let _ = self.check_expr(fctx, callee, None);
                Err(self.diags.emit(
                    Diagnostic::error("E022", "this expression is not callable").label(
                        span,
                        true,
                        "only functions can be called".to_owned(),
                    ),
                ))
            }
        }
    }

    fn check_named_call(
        &mut self,
        fctx: &mut FuncCtx,
        name: &str,
        item: &ResItem,
        args: &[Expr],
        span: Span,
    ) -> Result<Vec<TypeRef>, ErrorGuaranteed> {
        match &item.kind {
            ResKind::Func { generic, .. } => {
                if *generic {
                    return Err(deferred(self.diags, span, "generic calls", "M4"));
                }
                let Some(sig) = self.func_sig(item.id) else {
                    lilac_error::bug!(span, "missing signature for called function");
                };
                self.check_args(fctx, &sig, args, span)?;
                return Ok(sig.returns.clone());
            }
            ResKind::Hook { generic, .. } => {
                if *generic {
                    return Err(ErrorGuaranteed::new());
                }
            }
            ResKind::Object { .. } | ResKind::Enum { .. } | ResKind::Union { .. } => {
                return Err(deferred(self.diags, span, "constructors (`@init`)", "M2"));
            }
            ResKind::Const | ResKind::Field => {
                return Err(self.diags.emit(
                    Diagnostic::error("E022", "this item is not callable").label(
                        span,
                        true,
                        "only functions can be called".to_owned(),
                    ),
                ));
            }
        }
        let overloads = self.sigs.hooks.get(name).cloned().unwrap_or_default();
        let arity_ok: Vec<FuncSig> = overloads
            .iter()
            .filter(|(_, sig)| sig.params.len() == args.len())
            .map(|(_, sig)| sig.clone())
            .collect();
        let Some(first) = arity_ok.first().cloned() else {
            return Err(self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!("no overload of `{name}` takes {} arguments", args.len()),
                )
                .label(span, true, "arity mismatch here".to_owned()),
            ));
        };
        if arity_ok.len() == 1 {
            self.check_args(fctx, &first, args, span)?;
            return Ok(first.returns.clone());
        }
        let mut probed: Vec<Option<TypeRef>> = Vec::with_capacity(args.len());
        for arg in args {
            if matches!(arg, Expr::Number(..)) {
                probed.push(None);
                continue;
            }
            let got = self.check_expr(fctx, arg, None);
            probed.push(if is_poison(got) { None } else { Some(got) });
        }
        let winner = arity_ok
            .iter()
            .find(|sig| {
                sig.params
                    .iter()
                    .zip(probed.iter())
                    .all(|(param, got)| match got {
                        None => true,
                        Some(ty) if param.is_ref => ty.is_ref && ty.id == param.id,
                        Some(ty) => ty.id == param.id,
                    })
            })
            .cloned()
            .unwrap_or(first);
        self.check_args(fctx, &winner, args, span)?;
        Ok(winner.returns.clone())
    }

    fn check_args(
        &mut self,
        fctx: &mut FuncCtx,
        sig: &FuncSig,
        args: &[Expr],
        span: Span,
    ) -> Result<(), ErrorGuaranteed> {
        if args.len() != sig.params.len() {
            return Err(self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!(
                        "wrong number of arguments: expected {}, found {}",
                        sig.params.len(),
                        args.len(),
                    ),
                )
                .label(span, true, "arity mismatch here".to_owned()),
            ));
        }
        let mut failed = false;
        for (arg, param) in args.iter().zip(sig.params.iter()) {
            let got = self.check_expr(fctx, arg, Some(param.id));
            if is_poison(got) {
                failed = true;
                continue;
            }
            if param.is_ref && !got.is_ref {
                let want = self.types.display_ref(*param);
                self.diags.emit(
                    Diagnostic::error(
                        "E010",
                        format!(
                            "mismatched types: expected {want}, found {}",
                            self.types.display_ref(got),
                        ),
                    )
                    .label(span, false, format!("expected `{want}` here"))
                    .label(
                        arg.span(),
                        true,
                        "needs an explicit `ref`".to_owned(),
                    ),
                );
                failed = true;
                continue;
            }
            if self.expect_type(got, param.id, span, arg.span()).is_err() {
                failed = true;
            }
        }
        if failed {
            return Err(ErrorGuaranteed::new());
        }
        Ok(())
    }

    fn builtin_ty(&mut self, name: &str) -> TypeId {
        match self.types.builtin(name) {
            Some(id) => id,
            None => lilac_error::bug!(Span::dummy(), "missing builtin `{name}`"),
        }
    }

    fn expect_arity(
        &mut self,
        span: Span,
        what: &str,
        args: &[Expr],
        want: usize,
    ) -> Result<(), ErrorGuaranteed> {
        if args.len() != want {
            return Err(self.diags.emit(
                Diagnostic::error(
                    "E022",
                    format!(
                        "wrong number of arguments to `{what}`: expected {want}, found {}",
                        args.len()
                    ),
                )
                .label(span, true, "arity mismatch here".to_owned()),
            ));
        }
        Ok(())
    }

    /// Unifies same-width bit operands (literals take the width from siblings
    /// or the expected type); returns the unified width.
    fn unify_width(
        &mut self,
        fctx: &mut FuncCtx,
        args: &[Expr],
        expected: Option<TypeId>,
        span: Span,
    ) -> Result<TypeId, ErrorGuaranteed> {
        let mut width = match expected {
            Some(t) if is_bit(self.types, t) => Some(t),
            _ => None,
        };
        let mut checked: Vec<Option<TypeRef>> = vec![None; args.len()];
        let mut failed = false;
        for (index, arg) in args.iter().enumerate() {
            if matches!(arg, Expr::Number(..)) {
                continue;
            }
            let got = self.check_expr(fctx, arg, None);
            if is_poison(got) {
                failed = true;
                continue;
            }
            if !is_bit(self.types, got.id) {
                self.diags.emit(
                    Diagnostic::error(
                        "E010",
                        format!(
                            "mismatched types: expected a bit container, found {}",
                            self.types.display(got.id),
                        ),
                    )
                    .label(arg.span(), true, "not a bit container".to_owned()),
                );
                failed = true;
                continue;
            }
            if width.is_none() {
                width = Some(got.id);
            }
            checked[index] = Some(got);
        }
        let Some(width) = width else {
            self.diags.emit(
                Diagnostic::error("E024", "cannot infer the width here").label(
                    span,
                    true,
                    "annotate the binding".to_owned(),
                ),
            );
            return Err(ErrorGuaranteed::new());
        };
        for (index, arg) in args.iter().enumerate() {
            if let Some(got) = checked[index] {
                if got.id != width {
                    self.diags.emit(
                        Diagnostic::error(
                            "E010",
                            format!(
                                "mismatched widths: expected {}, found {}",
                                self.types.display(width),
                                self.types.display(got.id),
                            ),
                        )
                        .label(
                            arg.span(),
                            true,
                            "width mismatch here".to_owned(),
                        ),
                    );
                    failed = true;
                }
            } else {
                let got = self.check_expr(fctx, arg, Some(width));
                if !is_poison(got) && got.id != width {
                    failed = true;
                }
            }
        }
        if failed {
            return Err(ErrorGuaranteed::new());
        }
        Ok(width)
    }

    fn check_intrinsic(
        &mut self,
        fctx: &mut FuncCtx,
        intrinsic: Intrinsic,
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<Vec<TypeRef>, ErrorGuaranteed> {
        let value = |id: TypeId| TypeRef { id, is_ref: false };
        match intrinsic {
            Intrinsic::Arith | Intrinsic::Cmp | Intrinsic::Negate => {
                self.arith_out(fctx, intrinsic, args, span, expected)
            }
            Intrinsic::AsInt | Intrinsic::AsFloat | Intrinsic::Truncate | Intrinsic::ExtendZero => {
                self.converting_out(fctx, intrinsic, args, span, expected)
            }
            Intrinsic::LoadU8 | Intrinsic::StoreU8 | Intrinsic::Memcpy => {
                self.mem_intrinsic(fctx, intrinsic, args, span)
            }
            Intrinsic::ByteBufferPtr
            | Intrinsic::ByteBufferLen
            | Intrinsic::ByteBufferFrom
            | Intrinsic::FormatArgsNew
            | Intrinsic::FormatArgsPush
            | Intrinsic::FormatArgsCount
            | Intrinsic::FormatArgsPiece => self.buffer_intrinsic(fctx, intrinsic, args, span),
            Intrinsic::SizeOf => {
                self.expect_arity(span, "@size_of", args, 1)?;
                let _ = self.check_expr(fctx, &args[0], None);
                Ok(vec![value(self.builtin_ty("@bitNative"))])
            }
            Intrinsic::CompileError => {
                Err(deferred(self.diags, span, "`@compile_error` folding", "M2"))
            }
        }
    }

    fn arith_out(
        &mut self,
        fctx: &mut FuncCtx,
        intrinsic: Intrinsic,
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<Vec<TypeRef>, ErrorGuaranteed> {
        let value = |id: TypeId| TypeRef { id, is_ref: false };
        if intrinsic == Intrinsic::Negate {
            self.expect_arity(span, "@negate", args, 1)?;
        }
        let width = self.unify_width(fctx, args, expected, span)?;
        if args.is_empty() {
            let what = if intrinsic == Intrinsic::Cmp {
                "comparison"
            } else {
                "arithmetic"
            };
            return Err(self.diags.emit(
                Diagnostic::error("E022", format!("{what} needs operand values")).label(
                    span,
                    true,
                    "arity mismatch here".to_owned(),
                ),
            ));
        }
        if intrinsic == Intrinsic::Cmp {
            return Ok(vec![value(self.builtin_ty("@bit8"))]);
        }
        Ok(vec![value(width)])
    }

    fn converting_out(
        &mut self,
        fctx: &mut FuncCtx,
        intrinsic: Intrinsic,
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<Vec<TypeRef>, ErrorGuaranteed> {
        let value = |id: TypeId| TypeRef { id, is_ref: false };
        let (name, want_buffer) = match intrinsic {
            Intrinsic::AsInt => ("@as_int", true),
            Intrinsic::AsFloat => ("@as_float", true),
            Intrinsic::Truncate => ("@truncate", false),
            _ => ("@extend_zero", false),
        };
        self.expect_arity(span, name, args, 1)?;
        let got = self.check_expr(fctx, &args[0], None);
        if !is_poison(got) {
            let ok = if want_buffer {
                got.id == self.builtin_ty("@byte_buffer")
            } else {
                is_bit(self.types, got.id)
            };
            if !ok {
                let (want, note) = if want_buffer {
                    ("@byte_buffer".to_owned(), "digit buffer expected")
                } else {
                    ("a bit container".to_owned(), "not a bit container")
                };
                self.diags.emit(
                    Diagnostic::error(
                        "E010",
                        format!(
                            "mismatched types: expected {want}, found {}",
                            self.types.display(got.id),
                        ),
                    )
                    .label(args[0].span(), true, note.to_owned()),
                );
                return Err(ErrorGuaranteed::new());
            }
        }
        match expected {
            Some(t) if is_bit(self.types, t) => Ok(vec![value(t)]),
            _ => {
                Err(self.diags.emit(
                    Diagnostic::error("E024", format!("`{name}` needs an expected bit type"))
                        .label(span, true, "annotate the binding".to_owned()),
                ))
            }
        }
    }

    fn mem_intrinsic(
        &mut self,
        fctx: &mut FuncCtx,
        intrinsic: Intrinsic,
        args: &[Expr],
        span: Span,
    ) -> Result<Vec<TypeRef>, ErrorGuaranteed> {
        let value = |id: TypeId| TypeRef { id, is_ref: false };
        match intrinsic {
            Intrinsic::LoadU8 => {
                self.expect_arity(span, "@load_u8", args, 2)?;
                self.check_ref_bit8_arg(fctx, &args[0], span)?;
                self.check_fixed_arg(fctx, &args[1], "@bitNative")?;
                Ok(vec![value(self.builtin_ty("@bit8"))])
            }
            Intrinsic::StoreU8 => {
                self.expect_arity(span, "@store_u8", args, 3)?;
                self.check_ref_bit8_arg(fctx, &args[0], span)?;
                self.check_fixed_arg(fctx, &args[1], "@bitNative")?;
                self.check_fixed_arg(fctx, &args[2], "@bit8")?;
                Ok(Vec::new())
            }
            _ => {
                self.expect_arity(span, "@memcpy", args, 3)?;
                self.check_ref_bit8_arg(fctx, &args[0], span)?;
                self.check_ref_bit8_arg(fctx, &args[1], span)?;
                self.check_fixed_arg(fctx, &args[2], "@bitNative")?;
                Ok(Vec::new())
            }
        }
    }

    fn buffer_intrinsic(
        &mut self,
        fctx: &mut FuncCtx,
        intrinsic: Intrinsic,
        args: &[Expr],
        span: Span,
    ) -> Result<Vec<TypeRef>, ErrorGuaranteed> {
        let value = |id: TypeId| TypeRef { id, is_ref: false };
        match intrinsic {
            Intrinsic::ByteBufferPtr => {
                self.expect_arity(span, "@byte_buffer_ptr", args, 1)?;
                self.check_fixed_arg(fctx, &args[0], "@byte_buffer")?;
                Ok(vec![TypeRef {
                    id: self.builtin_ty("@bit8"),
                    is_ref: true,
                }])
            }
            Intrinsic::ByteBufferLen => {
                self.expect_arity(span, "@byte_buffer_len", args, 1)?;
                self.check_fixed_arg(fctx, &args[0], "@byte_buffer")?;
                Ok(vec![value(self.builtin_ty("@bitNative"))])
            }
            Intrinsic::ByteBufferFrom => {
                self.expect_arity(span, "@byte_buffer_from", args, 2)?;
                self.check_ref_bit8_arg(fctx, &args[0], span)?;
                self.check_fixed_arg(fctx, &args[1], "@bitNative")?;
                Ok(vec![value(self.builtin_ty("@byte_buffer"))])
            }
            Intrinsic::FormatArgsNew => {
                self.expect_arity(span, "@format_args_new", args, 0)?;
                Ok(vec![value(self.builtin_ty("@format_args"))])
            }
            Intrinsic::FormatArgsPush => {
                self.expect_arity(span, "@format_args_push", args, 2)?;
                let got = self.check_expr(fctx, &args[0], None);
                if !is_poison(got) && (!got.is_ref || got.id != self.builtin_ty("@format_args")) {
                    self.diags.emit(
                        Diagnostic::error(
                            "E010",
                            "mismatched types: expected ref @format_args".to_owned(),
                        )
                        .label(span, false, "expected `ref @format_args` here".to_owned())
                        .label(
                            args[0].span(),
                            true,
                            "needs an explicit `ref`".to_owned(),
                        ),
                    );
                    return Err(ErrorGuaranteed::new());
                }
                self.check_fixed_arg(fctx, &args[1], "@byte_buffer")?;
                Ok(Vec::new())
            }
            Intrinsic::FormatArgsCount => {
                self.expect_arity(span, "@format_args_count", args, 1)?;
                self.check_fixed_arg(fctx, &args[0], "@format_args")?;
                Ok(vec![value(self.builtin_ty("@bitNative"))])
            }
            _ => {
                self.expect_arity(span, "@format_args_piece", args, 2)?;
                self.check_fixed_arg(fctx, &args[0], "@format_args")?;
                self.check_fixed_arg(fctx, &args[1], "@bitNative")?;
                Ok(vec![value(self.builtin_ty("@byte_buffer"))])
            }
        }
    }

    fn check_ref_bit8_arg(
        &mut self,
        fctx: &mut FuncCtx,
        arg: &Expr,
        span: Span,
    ) -> Result<(), ErrorGuaranteed> {
        let got = self.check_expr(fctx, arg, None);
        if is_poison(got) {
            return Err(ErrorGuaranteed::new());
        }
        if got.is_ref && got.id == self.builtin_ty("@bit8") {
            return Ok(());
        }
        Err(self.diags.emit(
            Diagnostic::error("E010", "mismatched types: expected ref @bit8".to_owned())
                .label(span, false, "expected `ref @bit8` here".to_owned())
                .label(arg.span(), true, "needs an explicit `ref`".to_owned()),
        ))
    }

    fn check_fixed_arg(
        &mut self,
        fctx: &mut FuncCtx,
        arg: &Expr,
        want: &str,
    ) -> Result<(), ErrorGuaranteed> {
        let want_id = self.builtin_ty(want);
        let got = self.check_expr(fctx, arg, Some(want_id));
        if is_poison(got) {
            return Err(ErrorGuaranteed::new());
        }
        self.expect_type(got, want_id, arg.span(), arg.span())?;
        Ok(())
    }

    fn ident_type(
        &mut self,
        fctx: &mut FuncCtx,
        name: &str,
        span: Span,
    ) -> Result<TypeRef, ErrorGuaranteed> {
        if let Some(ty) = fctx.locals.get(name) {
            return Ok(*ty);
        }
        if let Some(item) = self.module.items.get(name) {
            let kind = item.kind.clone();
            return match kind {
                ResKind::Func { .. } | ResKind::Hook { .. } => Err(self.diags.emit(
                    Diagnostic::error("E021", format!("cannot use function `{name}` as a value"))
                        .label(span, true, "functions are not values in M1".to_owned()),
                )),
                ResKind::Const => self.const_type(name, span),
                ResKind::Object { .. } | ResKind::Enum { .. } | ResKind::Union { .. } => {
                    Err(self.diags.emit(
                        Diagnostic::error("E021", format!("cannot use type `{name}` as a value"))
                            .label(span, true, "name a variable instead".to_owned()),
                    ))
                }
                ResKind::Field => Err(self.diags.emit(
                    Diagnostic::error("E021", format!("unresolved name `{name}`")).label(
                        span,
                        true,
                        "fields need a receiver in M1".to_owned(),
                    ),
                )),
            };
        }
        Err(self.diags.emit(
            Diagnostic::error("E021", format!("unresolved name `{name}`")).label(
                span,
                true,
                "not a local or item".to_owned(),
            ),
        ))
    }

    fn const_type(&mut self, name: &str, span: Span) -> Result<TypeRef, ErrorGuaranteed> {
        let found = self.program.items.iter().find_map(|item| match &item.kind {
            ItemKind::Const { ty, .. } if item.name == name => Some(ty.clone()),
            _ => None,
        });
        match found {
            Some(ty) => Ok(resolve_type(&ty, self.types, self.module, self.diags)),
            None => Err(self.diags.emit(
                Diagnostic::error("E021", format!("unresolved name `{name}`")).label(
                    span,
                    true,
                    "not a local or item".to_owned(),
                ),
            )),
        }
    }

    fn tag_type(
        &mut self,
        tag: &str,
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<TypeRef, ErrorGuaranteed> {
        let Some(want) = expected else {
            return Err(self.diags.emit(
                Diagnostic::error("E024", "tag literal needs an expected enum type").label(
                    span,
                    true,
                    "annotate the binding".to_owned(),
                ),
            ));
        };
        if want == TypeId::ERROR {
            return Ok(TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            });
        }
        let lilac_ty::TypeKind::Object(def) = self.types.kind(want) else {
            return Err(self.diags.emit(
                Diagnostic::error("E010", format!("tag `.{tag}` needs an enum type")).label(
                    span,
                    true,
                    "expected a tag here".to_owned(),
                ),
            ));
        };
        let found = self.find_by_def(*def);
        match found {
            Some(ResKind::Enum { variants }) => {
                if variants.iter().any(|v| v.name == *tag) {
                    Ok(TypeRef {
                        id: want,
                        is_ref: false,
                    })
                } else {
                    Err(self.diags.emit(
                        Diagnostic::error("E010", format!("no variant `.{tag}` here")).label(
                            span,
                            true,
                            "unknown tag for this enum".to_owned(),
                        ),
                    ))
                }
            }
            Some(ResKind::Union { .. }) => Err(deferred(self.diags, span, "union tags", "M2")),
            _ => Err(self.diags.emit(
                Diagnostic::error("E010", format!("tag `.{tag}` needs an enum type")).label(
                    span,
                    true,
                    "expected a tag here".to_owned(),
                ),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::check_bodies;
    use lilac_error::Diags;
    use lilac_lex::Lexer;
    use lilac_parse::Parser;
    use lilac_span::SourceMap;
    use lilac_ty::Types;

    fn check_src(src: &str) -> Diags {
        let mut sm = SourceMap::new();
        let file = sm.add_file("test.lilac", src);
        let (tokens, lex_errors) = Lexer::lex_all(src, file);
        let mut diags = Diags::new();
        for error in lex_errors {
            diags.emit(error);
        }
        let mut parser = Parser::new(tokens, file);
        let program = parser
            .parse_program(&mut diags)
            .expect("test source parses");
        let module = lilac_resolve::collect(&program, &mut diags);
        let mut types = Types::new();
        let sigs = super::super::collect_sigs(&module, &program, &mut types, &mut diags);
        check_bodies(&program, &module, &sigs, &mut types, &mut diags);
        diags
    }

    fn has_code(diags: &Diags, code: &str) -> bool {
        diags.diagnostics().iter().any(|d| d.code == code)
    }

    fn messages(diags: &Diags) -> Vec<String> {
        diags.diagnostics().iter().map(|d| d.msg.clone()).collect()
    }

    #[test]
    fn mismatch_without_conversion_is_e043() {
        // Task 5: without a binding, a type mismatch is a failed conversion
        // search. The diagnostic still shows both sides.
        let diags =
            check_src("main: function () nothing {\n    y: @bit32 = 0x1\n    x: @bit8 = y\n}\n");
        assert!(has_code(&diags, "E043"), "{diags:?}");
        let msgs = messages(&diags);
        assert!(
            msgs.iter()
                .any(|m| m.contains("@bit8") && m.contains("@bit32")),
            "{msgs:?}"
        );
    }

    #[test]
    fn unresolved_name_is_e021() {
        let diags = check_src("main: function () nothing {\n    x: @bit8 = nope\n}\n");
        assert!(has_code(&diags, "E021"), "{diags:?}");
    }

    #[test]
    fn arity_mismatch_is_e022() {
        let diags = check_src(
            "get: function () result: @bit32 {\n    result = 0x1\n}\nmain: function () nothing {\n    x: @bit8 = get(0x1)\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
    }

    #[test]
    fn bare_literal_is_e024() {
        let diags = check_src("main: function () nothing {\n    0x1\n}\n");
        assert!(has_code(&diags, "E024"), "{diags:?}");
    }

    #[test]
    fn unassigned_slot_is_e027() {
        let diags = check_src("get: function () result: @bit32 {\n}\n");
        assert!(has_code(&diags, "E027"), "{diags:?}");
    }

    #[test]
    fn locals_shadow_globals() {
        let diags = check_src(
            "get: function () result: @bit32 {\n    result = 0x1\n}\nmain: function () nothing {\n    g: @bit32 = get()\n    x: @bit8 = g()\n}\n",
        );
        assert!(has_code(&diags, "E022"), "{diags:?}");
        assert_eq!(diags.diagnostics().len(), 1, "{diags:?}");
    }

    #[test]
    fn equality_needs_same_width() {
        let diags = check_src(
            "main: function () nothing {\n    a: @bit8 = 0x1\n    b: @bit32 = 0x2\n    c: @bit8 = a == b\n}\n",
        );
        assert!(has_code(&diags, "E010"), "{diags:?}");
        let e010: Vec<_> = diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E010")
            .collect();
        assert_eq!(e010.len(), 1, "{diags:?}");
        assert_eq!(e010[0].labels.len(), 2, "{diags:?}");
    }

    #[test]
    fn ref_mismatch_labels_both_sides() {
        let diags = check_src(
            "set: function (r: ref @bit8) nothing {\n}\nmain: function () nothing {\n    v: @bit8 = 0x1\n    set(v)\n}\n",
        );
        assert!(has_code(&diags, "E010"), "{diags:?}");
        let e010: Vec<_> = diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E010")
            .collect();
        assert_eq!(e010.len(), 1, "{diags:?}");
        assert_eq!(e010[0].labels.len(), 2, "{diags:?}");
        assert!(
            e010[0].msg.contains("ref @bit8") && e010[0].msg.contains("@bit8"),
            "{diags:?}"
        );
    }

    #[test]
    fn union_tag_defers_with_e030() {
        let diags = check_src(
            "Maybe: union {\n    Some: @bit8\n    None\n}\nmain: function () nothing {\n    x: Maybe = .None\n}\n",
        );
        assert!(has_code(&diags, "E030"), "{diags:?}");
    }

    #[test]
    fn overflowing_literal_waits_for_m3() {
        // M1 does not range-check literals; M3 `E060` tightens this.
        let diags = check_src("main: function () nothing {\n    x: @bit8 = 0x1FF\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn poison_types_never_panic() {
        // Unknown parameter types poison the signature; calling with a
        // literal must report E023, never crash the compiler.
        let diags = check_src(
            "foo: function (x: Nope) nothing {\n}\nmain: function () nothing {\n    foo(0x1)\n}\n",
        );
        assert!(has_code(&diags, "E023"), "{diags:?}");
    }
}
