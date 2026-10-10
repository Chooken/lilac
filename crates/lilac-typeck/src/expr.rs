//! Expressions, places, and the one coercion rule.
//!
//! [`Checker::coerce`] is the single place a value meets an expected type:
//! a `ref T` flowing into a `T` slot is dereferenced (an explicit `Deref`
//! node), an exact match passes, and anything else takes exactly one
//! `@conversion` (`E043` when there is none).

use std::collections::HashSet;

use lilac_ast::{
    BinOp, Expr, FStrPart, MatchArm, MatchArmBody, Stmt, StructField, Type as AstType, TypeShape,
};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_hir::{FuncKey, HirCallee, HirExpr, HirExprKind, HirValueArm, Intrinsic, SemanticKind};
use lilac_resolve::{DefId, DefKind, Visibility};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeId, TypeKind, TypeRef};

use crate::body::{local_expr, FnCx};
use crate::calls::Called;
use crate::literals::is_foldable_literal;
use crate::paths::{path_tail, shadowed_alias_note};
use crate::sigs::BINARY_HOOKS;
use crate::{Checker, Env, Requirement};

/// The trailing `name` token inside a member-access span.
pub(crate) fn tail_span(span: Span, name: &str) -> Span {
    let len = u32::try_from(name.len()).unwrap_or(u32::MAX);
    Span::new(span.hi.0.saturating_sub(len), span.hi.0, span.file)
}

/// What a static receiver (`Name.member`) names.
#[derive(Clone, Copy)]
pub(crate) enum Receiver {
    /// A concrete type.
    Type(TypeRef),
    /// A generic type written without arguments (`Optional.Some(x)`).
    Bare(DefId),
}

impl Checker<'_> {
    /// Checks an expression; `expected` guides literals, tags and generic
    /// argument inference (it never converts by itself: see `coerce`).
    pub(crate) fn check_expr(
        &mut self,
        fcx: &mut FnCx,
        expr: &Expr,
        expected: Option<TypeId>,
    ) -> HirExpr {
        match expr {
            Expr::Ident(name, span) => self.check_ident(fcx, name, *span),
            Expr::Tag(name, span) => {
                let tag = tail_span(*span, name);
                let Some(want) = expected.filter(|id| *id != TypeId::ERROR) else {
                    if expected.is_none() {
                        self.diags.emit(
                            Diagnostic::error(
                                codes::E024,
                                "tag literal needs an expected enum type",
                            )
                            .label(*span, true, "annotate the binding"),
                        );
                    }
                    return HirExpr::poison(*span);
                };
                self.variant_value(want, name, None, *span, tag)
            }
            Expr::Number(bytes, span) => self.check_number(bytes, *span, expected),
            Expr::Str(bytes, span) => HirExpr {
                ty: Builtin::ByteBuffer.value(),
                span: *span,
                kind: HirExprKind::Str(bytes.clone()),
            },
            Expr::Char(scalar, span) => HirExpr {
                ty: Builtin::Bit32.value(),
                span: *span,
                kind: HirExprKind::Number(format!("0x{:X}", u32::from(*scalar))),
            },
            Expr::RefOf { value, span } => self.check_ref_of(fcx, value, *span),
            Expr::Cast(operand, target) => self.check_cast(fcx, operand, target),
            Expr::Member { obj, field, span } => {
                if let Some((name, span)) = self.qualified_name(fcx, expr) {
                    return self.check_ident(fcx, &name, span);
                }
                self.check_member(fcx, obj, field, *span, expected)
            }
            Expr::Call {
                callee,
                generic_args,
                args,
                span,
            } => match self.check_call(fcx, callee, generic_args, args, *span, expected) {
                Ok(Called::Call { callee, args, outs }) => {
                    if outs.len() != 1 {
                        self.diags.emit(
                            Diagnostic::error(
                                codes::E022,
                                "call in value position must return exactly one value",
                            )
                            .label(
                                *span,
                                true,
                                "one value expected here",
                            ),
                        );
                        return HirExpr::poison(*span);
                    }
                    HirExpr {
                        ty: outs[0],
                        span: *span,
                        kind: HirExprKind::Call { callee, args },
                    }
                }
                Ok(Called::Value(value)) => value,
                Err(_) => HirExpr::poison(*span),
            },
            Expr::BinOp { lhs, op, rhs, span } => {
                self.check_binop(fcx, lhs, *op, rhs, *span, expected)
            }
            Expr::IfValue {
                cond,
                then,
                els,
                span,
            } => self.check_if_value(fcx, cond, then, els, *span, expected),
            Expr::MatchValue {
                scrutinee,
                arms,
                els,
                span,
            } => self.check_match_value(fcx, scrutinee, arms, els, *span, expected),
            Expr::StructLit { ty, fields, span } => {
                self.check_struct_lit(fcx, ty, fields, *span, expected)
            }
            Expr::FString { parts, span } => self.check_fstring(fcx, parts, *span),
            Expr::TypeApp(ty) => {
                self.diags.emit(
                    Diagnostic::error(
                        codes::E021,
                        format!("cannot use type `{}` as a value", ty.head()),
                    )
                    .label(ty.span, true, "a type, not a value"),
                );
                HirExpr::poison(ty.span)
            }
            Expr::Error { span } => HirExpr::poison(*span),
        }
    }

    /// Fits `expr` into a `want` slot: deref, exact, or one conversion.
    pub(crate) fn coerce(&mut self, expr: HirExpr, want: TypeRef, want_span: Span) -> HirExpr {
        if expr.ty.is_error() || want.is_error() {
            return expr;
        }
        if want.is_ref {
            if expr.ty.is_ref && expr.ty.id == want.id {
                return expr;
            }
            let span = expr.span;
            self.mismatch(want, expr.ty, want_span, span);
            return HirExpr::poison(span);
        }
        if expr.ty.is_ref && expr.ty.id != want.id {
            let span = expr.span;
            self.mismatch(want, expr.ty, want_span, span);
            return HirExpr::poison(span);
        }
        let expr = Self::auto_deref(expr);
        if expr.ty.id == want.id {
            return expr;
        }
        if let Some(converted) = self.try_convert(expr.clone(), want.id) {
            return converted;
        }
        let from = self.types.display(expr.ty.id);
        let target = self.types.display(want.id);
        let source = expr.ty.id;
        let diagnostic = Diagnostic::error(
            codes::E043,
            format!("no conversion from {from} to {target}"),
        )
        .label(want_span, false, format!("expected `{target}` here"))
        .label(expr.span, true, "conversion attempted here");
        self.emit_requirement(
            diagnostic,
            Requirement::Conversion { from: source },
            &[expr.ty.id, want.id],
        );
        HirExpr::poison(expr.span)
    }

    /// Wraps a by-value `expr` in its `@conversion` to `to`, if one exists.
    pub(crate) fn try_convert(&mut self, expr: HirExpr, to: TypeId) -> Option<HirExpr> {
        let hook = self.overloads("@conversion").into_iter().find(|hook| {
            hook.sig
                .params
                .first()
                .is_some_and(|p| p.id == expr.ty.id && !p.is_ref)
                && hook.sig.returns.first().is_some_and(|r| r.id == to)
        })?;
        Some(HirExpr {
            ty: TypeRef::value(to),
            span: expr.span,
            kind: HirExprKind::Call {
                callee: HirCallee::Func(hook.key),
                args: vec![expr],
            },
        })
    }

    /// `*expr` when `expr` is a reference (never through `ref unknown`).
    pub(crate) fn auto_deref(expr: HirExpr) -> HirExpr {
        if !expr.ty.is_ref || expr.ty.id == Builtin::Unknown.id() {
            return expr;
        }
        HirExpr {
            ty: expr.ty.deref(),
            span: expr.span,
            kind: HirExprKind::Deref(Box::new(expr)),
        }
    }

    // ---- names -----------------------------------------------------------

    fn check_ident(&mut self, fcx: &mut FnCx, name: &str, span: Span) -> HirExpr {
        if let Some(local) = fcx.lookup(name) {
            self.record_local_use(fcx, local, span);
            return local_expr(local, fcx.ty(local), span);
        }
        if self.env.params.iter().any(|param| param == name) {
            self.diags.emit(
                Diagnostic::error(codes::E021, format!("cannot use type `{name}` as a value"))
                    .label(span, true, "name a variable instead"),
            );
            return HirExpr::poison(span);
        }
        let Ok(found) = self.lookup_item(name, span) else {
            return HirExpr::poison(span);
        };
        let Some(def) = found else {
            let diagnostic = Diagnostic::error(codes::E021, format!("unresolved name `{name}`"))
                .label(span, true, "not a local or item");
            let diagnostic = self.suggest_value(fcx, diagnostic, name, span);
            self.diags.emit(diagnostic);
            return HirExpr::poison(span);
        };
        self.record_use(path_tail(span, name), def);
        match &self.res.def(def).kind {
            DefKind::Func(decl) if decl.variadic => {
                self.diags.emit(
                    Diagnostic::error(
                        codes::E028,
                        format!("cannot use variadic extern `{name}` as a value"),
                    )
                    .label(span, true, "function types have no `...`")
                    .help("call it directly, or wrap it in a function with fixed parameters"),
                );
                HirExpr::poison(span)
            }
            DefKind::Func(_) => {
                let generic = self.res.def(def).generic_params.len();
                if generic > 0 {
                    self.generic_arity(span, name, generic, 0);
                    return HirExpr::poison(span);
                }
                let key = FuncKey::plain(def);
                let Ok(sig) = self.request(key.clone(), span) else {
                    return HirExpr::poison(span);
                };
                let id = self.types.intern_func(sig.params, sig.returns);
                HirExpr {
                    ty: TypeRef::value(id),
                    span,
                    kind: HirExprKind::FuncRef(key),
                }
            }
            DefKind::Const { .. } => {
                self.use_builtin_const(def);
                let ty = self.const_type(def);
                HirExpr {
                    ty,
                    span,
                    kind: HirExprKind::Const(def),
                }
            }
            DefKind::Object | DefKind::Enum => {
                self.diags.emit(
                    Diagnostic::error(codes::E021, format!("cannot use type `{name}` as a value"))
                        .label(span, true, "name a variable instead"),
                );
                HirExpr::poison(span)
            }
            DefKind::Hook(_) => {
                self.diags.emit(
                    Diagnostic::error(
                        codes::E021,
                        format!("cannot use function `{name}` as a value"),
                    )
                    .label(span, true, "hooks are not values"),
                );
                HirExpr::poison(span)
            }
            DefKind::Field(_) | DefKind::Variant(_) => {
                self.diags.emit(
                    Diagnostic::error(codes::E021, format!("unresolved name `{name}`")).label(
                        span,
                        true,
                        "not a local or item",
                    ),
                );
                HirExpr::poison(span)
            }
        }
    }

    /// `E044` for a call-level generic-argument count mismatch.
    pub(crate) fn generic_arity(
        &mut self,
        span: Span,
        what: &str,
        expected: usize,
        found: usize,
    ) -> ErrorGuaranteed {
        let noun = if expected == 1 {
            "argument"
        } else {
            "arguments"
        };
        self.diags.emit(
            Diagnostic::error(
                codes::E044,
                format!("wrong number of generic arguments for `{what}`"),
            )
            .label(
                span,
                true,
                format!("expected {expected} generic {noun}, found {found}"),
            ),
        )
    }

    /// What `Name` names when it is the receiver of a static member access:
    /// a type (or generic parameter), or `None` when it is a value.
    pub(crate) fn static_receiver(
        &mut self,
        fcx: &FnCx,
        obj: &Expr,
    ) -> Option<Result<Receiver, ErrorGuaranteed>> {
        match obj {
            Expr::TypeApp(ty) => {
                let resolved = self.resolve_type(ty);
                Some(if resolved.is_error() {
                    // `resolve_type` reported it.
                    Err(self.diags.already_reported())
                } else {
                    Ok(Receiver::Type(resolved))
                })
            }
            Expr::Ident(name, span) => {
                if fcx.lookup(name).is_some() {
                    return None;
                }
                if let Some(index) = self.env.params.iter().position(|param| param == name) {
                    self.record_semantic(*span, SemanticKind::UserType);
                    let ty = if let Some(arg) = self.env.args.get(index) {
                        *arg
                    } else {
                        let index = u32::try_from(index).unwrap_or(u32::MAX);
                        TypeRef::value(self.types.intern_param(index, name))
                    };
                    return Some(Ok(Receiver::Type(ty)));
                }
                if let Some(id) = self.types.builtin(name) {
                    return Some(Ok(Receiver::Type(TypeRef::value(id))));
                }
                let def = match self.lookup_item(name, *span) {
                    Ok(Some(def)) if self.res.def(def).is_type() => def,
                    Ok(_) => return None,
                    Err(guaranteed) => return Some(Err(guaranteed)),
                };
                self.record_use(path_tail(*span, name), def);
                if !self.res.def(def).generic_params.is_empty() {
                    return Some(Ok(Receiver::Bare(def)));
                }
                let ty = self.type_of_def(def, Vec::new());
                Some(self.ensure_type(ty, *span).map(|()| Receiver::Type(ty)))
            }
            Expr::Member { .. } => {
                let (name, span) = self.qualified_name(fcx, obj)?;
                self.static_receiver(fcx, &Expr::Ident(name, span))
            }
            _ => None,
        }
    }

    // ---- places and members ---------------------------------------------

    /// Checks an assignable place (a local or a field chain).
    pub(crate) fn check_place(
        &mut self,
        fcx: &mut FnCx,
        expr: &Expr,
    ) -> Result<HirExpr, ErrorGuaranteed> {
        match expr {
            Expr::Ident(name, span) => {
                if let Some(local) = fcx.lookup(name) {
                    self.record_local_use(fcx, local, *span);
                    return Ok(local_expr(local, fcx.ty(local), *span));
                }
                let diagnostic = Diagnostic::error(
                    codes::E021,
                    format!("unresolved name `{name}`"),
                )
                .label(*span, true, "not a local or item");
                let diagnostic = self.suggest_value(fcx, diagnostic, name, *span);
                Err(self.diags.emit(diagnostic))
            }
            Expr::Member { obj, field, span } => {
                let base = self.check_place(fcx, obj)?;
                if base.ty.is_error() {
                    return Ok(HirExpr::poison(*span));
                }
                let shadowed = self.shadowed_alias(fcx, &base);
                Ok(self.field_access(base, field, *span, None, false, shadowed))
            }
            _ => Err(self.diags.emit(
                Diagnostic::error(codes::E010, "cannot assign to this expression").label(
                    expr.span(),
                    true,
                    "only variables and fields are assignable",
                ),
            )),
        }
    }

    /// `base.field` on an object (through a reference if needed).
    /// `shadowed` is the alias a local `base` hides ([`Self::shadowed_alias`]).
    pub(crate) fn field_access(
        &mut self,
        base: HirExpr,
        field: &str,
        span: Span,
        expected: Option<TypeId>,
        annotate: bool,
        shadowed: Option<(String, Span)>,
    ) -> HirExpr {
        let base = Self::auto_deref(base);
        let ty = base.ty.id;
        if let Some(fields) = self.fields_of(ty) {
            if let Some(index) = fields.iter().position(|(name, _)| name == field) {
                if let Some((owner, _)) = self.type_parts(ty) {
                    if let Some(member) = self.res.member(owner, field) {
                        if self.private_denied(member, owner, span) {
                            return HirExpr::poison(span);
                        }
                        self.record_use(tail_span(span, field), member);
                    }
                }
                return HirExpr {
                    ty: fields[index].1,
                    span,
                    kind: HirExprKind::Field {
                        base: Box::new(base),
                        index: u32::try_from(index).unwrap_or(u32::MAX),
                    },
                };
            }
        }
        let owner = self.types.display(ty);
        let mut diagnostic = Diagnostic::error(
            codes::E021,
            format!("no field `{field}` on `{owner}`"),
        )
        .label(span, true, "no such member");
        if let Some((def, _)) = self.type_parts(ty) {
            diagnostic = self.suggest_member(diagnostic, def, field, tail_span(span, field), false);
        }
        diagnostic = shadowed_alias_note(diagnostic, shadowed);
        if annotate && self.is_generic_derived(base.ty) {
            diagnostic = diagnostic.note(self.required_field_note(field, expected));
        }
        let owner_def = self.type_parts(ty).map(|(def, _)| def);
        let involved = if owner_def.is_some() { &[][..] } else { &[ty] };
        self.emit_requirement(
            diagnostic,
            Requirement::Missing {
                method: false,
                name: field.to_owned(),
                owner,
                owner_def,
            },
            involved,
        );
        HirExpr::poison(span)
    }

    /// `E026` for a private member used outside its type.
    pub(crate) fn private_denied(&mut self, member: DefId, owner: DefId, access: Span) -> bool {
        let def = self.res.def(member);
        if def.vis != Visibility::Private || self.env.owner == Some(owner) {
            return false;
        }
        let decl = def.span;
        self.diags.emit(
            Diagnostic::error(codes::E026, format!("`{}` is a private member", def.name))
                .label(access, true, "accessed from outside its object")
                .label(decl, false, "declared private here"),
        );
        true
    }

    /// `obj.field` as a value: a field read, or a variant of a named enum.
    fn check_member(
        &mut self,
        fcx: &mut FnCx,
        obj: &Expr,
        field: &str,
        span: Span,
        expected: Option<TypeId>,
    ) -> HirExpr {
        if let Some(receiver) = self.static_receiver(fcx, obj) {
            let Ok(receiver) = receiver else {
                return HirExpr::poison(span);
            };
            let ty = match receiver {
                Receiver::Type(ty) => ty,
                Receiver::Bare(def) => match self.infer_bare(def, expected, span) {
                    Ok(ty) => ty,
                    Err(_) => return HirExpr::poison(span),
                },
            };
            return self.variant_value(ty.id, field, None, span, tail_span(span, field));
        }
        let base = self.check_expr(fcx, obj, None);
        if base.ty.is_error() {
            return base;
        }
        let shadowed = self.shadowed_alias(fcx, &base);
        self.field_access(base, field, span, expected, true, shadowed)
    }

    /// The instance of generic `def` the expected type names (`E044` if none).
    pub(crate) fn infer_bare(
        &mut self,
        def: DefId,
        expected: Option<TypeId>,
        span: Span,
    ) -> Result<TypeRef, ErrorGuaranteed> {
        if let Some(id) = expected {
            if let TypeKind::Instance { base, .. } = self.types.kind(id) {
                if *base == def {
                    return Ok(TypeRef::value(id));
                }
            }
        }
        let name = self.res.def(def).name.clone();
        let count = self.res.def(def).generic_params.len();
        Err(self.generic_arity(span, &name, count, 0))
    }

    /// Builds variant `name` of enum `ty`, checking the payload argument.
    pub(crate) fn variant_value(
        &mut self,
        ty: TypeId,
        name: &str,
        payload: Option<(&mut FnCx, &[Expr])>,
        span: Span,
        tag_span: Span,
    ) -> HirExpr {
        let Ok((index, payload_ty)) = self.variant(ty, name, tag_span) else {
            return HirExpr::poison(span);
        };
        self.record_semantic(tag_span, SemanticKind::Variant);
        if let Some((def, _)) = self.type_parts(ty) {
            self.record_use(tag_span, def);
            if let Some(variant) = self.res.member(def, name) {
                self.record_use(tag_span, variant);
            }
        }
        let value = match (payload_ty, payload) {
            (None, None | Some((_, []))) => None,
            (None, Some(_)) => {
                self.diags.emit(
                    Diagnostic::error(codes::E022, format!("variant `.{name}` takes no payload"))
                        .label(span, true, "arity mismatch here"),
                );
                return HirExpr::poison(span);
            }
            (Some(_), None) => {
                self.diags.emit(
                    Diagnostic::error(
                        codes::E022,
                        format!("variant `.{name}` needs a payload; write `.{name}(...)`"),
                    )
                    .label(span, true, "payload missing here"),
                );
                return HirExpr::poison(span);
            }
            (Some(want), Some((fcx, args))) => {
                let [arg] = args else {
                    self.diags.emit(
                        Diagnostic::error(
                            codes::E022,
                            format!("variant `.{name}` needs exactly one payload value"),
                        )
                        .label(span, true, "arity mismatch here"),
                    );
                    return HirExpr::poison(span);
                };
                let value = self.check_expr(fcx, arg, Some(want.id));
                Some(Box::new(self.coerce(value, want, arg.span())))
            }
        };
        HirExpr {
            ty: TypeRef::value(ty),
            span,
            kind: HirExprKind::Variant {
                index,
                payload: value,
            },
        }
    }

    // ---- references and casts ---------------------------------------------

    fn check_ref_of(&mut self, fcx: &mut FnCx, value: &Expr, span: Span) -> HirExpr {
        let Ok(place) = self.check_place_for_ref(fcx, value) else {
            return HirExpr::poison(span);
        };
        if place.ty.is_error() {
            return place;
        }
        if place.ty.is_ref {
            self.diags.emit(
                Diagnostic::error(codes::E010, "cannot take `ref` of a reference").label(
                    span,
                    true,
                    "double references are invalid unless wrapped inside an object",
                ),
            );
            return HirExpr::poison(span);
        }
        HirExpr {
            ty: TypeRef::reference(place.ty.id),
            span,
            kind: HirExprKind::RefOf(Box::new(place)),
        }
    }

    /// A place to take the address of (a local or field chain).
    fn check_place_for_ref(
        &mut self,
        fcx: &mut FnCx,
        value: &Expr,
    ) -> Result<HirExpr, ErrorGuaranteed> {
        match value {
            Expr::Ident(..) | Expr::Member { .. } => self.check_place(fcx, value),
            _ => Err(self.diags.emit(
                Diagnostic::error(codes::E010, "cannot take `ref` of this expression").label(
                    value.span(),
                    true,
                    "only variables and fields have addresses",
                ),
            )),
        }
    }

    /// `operand as T`: one conversion (or a literal pinned to `T`); the
    /// `ref unknown` <-> `ref T` pointer cast is a pure retype.
    fn check_cast(&mut self, fcx: &mut FnCx, operand: &Expr, target_ty: &AstType) -> HirExpr {
        let target = self.resolve_type(target_ty);
        let span = operand.span();
        if target.is_error() {
            let _ = self.check_expr(fcx, operand, None);
            return HirExpr::poison(span);
        }
        if target.is_ref {
            let value = self.check_expr(fcx, operand, None);
            if value.ty.is_error() {
                return value;
            }
            let unknown = Builtin::Unknown.id();
            if value.ty.is_ref
                && (value.ty.id == unknown || target.id == unknown || value.ty.id == target.id)
            {
                return HirExpr {
                    ty: target,
                    span,
                    kind: HirExprKind::Retype(Box::new(value)),
                };
            }
            self.mismatch(target, value.ty, target_ty.span, span);
            return HirExpr::poison(span);
        }
        let value = self.check_expr(fcx, operand, Some(target.id));
        if value.ty.is_error() {
            return value;
        }
        if value.ty.is_ref {
            self.mismatch(target, value.ty, target_ty.span, span);
            return HirExpr::poison(span);
        }
        self.coerce(value, target, target_ty.span)
    }

    // ---- operators --------------------------------------------------------

    fn check_binop(
        &mut self,
        fcx: &mut FnCx,
        lhs: &Expr,
        op: BinOp,
        rhs: &Expr,
        span: Span,
        expected: Option<TypeId>,
    ) -> HirExpr {
        match op {
            BinOp::And | BinOp::Or => {
                let what = if op == BinOp::And { "and" } else { "or" };
                let lhs = self.check_cond(fcx, lhs, what);
                let rhs = self.check_cond(fcx, rhs, what);
                return HirExpr {
                    ty: Builtin::Bit8.value(),
                    span,
                    kind: HirExprKind::Logic {
                        and: op == BinOp::And,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                };
            }
            BinOp::EqEq => {
                let (left, right) = self.check_sides(fcx, lhs, rhs);
                if left.ty.is_error() || right.ty.is_error() {
                    return HirExpr::poison(span);
                }
                let (left, right) = (Self::auto_deref(left), Self::auto_deref(right));
                if left.ty.id != right.ty.id {
                    let (l, r) = (
                        self.types.display(left.ty.id),
                        self.types.display(right.ty.id),
                    );
                    self.diags.emit(
                        Diagnostic::error(
                            codes::E010,
                            format!(
                                "mismatched types: `==` needs identical types, found {l} and {r}"
                            ),
                        )
                        .label(left.span, false, format!("left side `{l}` here"))
                        .label(
                            right.span,
                            true,
                            format!("right side `{r}` here"),
                        ),
                    );
                    return HirExpr::poison(span);
                }
                return HirExpr {
                    ty: Builtin::Bit8.value(),
                    span,
                    kind: HirExprKind::Eq(Box::new(left), Box::new(right)),
                };
            }
            _ => {}
        }
        if let Some(folded) = self.fold_binop(lhs, op, rhs, span, expected) {
            return folded;
        }
        let (left, right) = self.check_sides(fcx, lhs, rhs);
        if left.ty.is_error() || right.ty.is_error() {
            return HirExpr::poison(span);
        }
        let (left, right) = (Self::auto_deref(left), Self::auto_deref(right));
        let hook = hook_name(op);
        let found = self.overloads(hook).into_iter().find(|candidate| {
            let sig = &candidate.sig;
            sig.params.len() == 2
                && sig.returns.len() == 1
                && sig.params[0] == left.ty
                && sig.params[1] == right.ty
        });
        let Some(found) = found else {
            let mut diagnostic = Diagnostic::error(
                codes::E043,
                format!(
                    "no {hook} for ({}, {})",
                    self.types.display(left.ty.id),
                    self.types.display(right.ty.id)
                ),
            )
            .label(span, true, "no exact operator overload");
            if left.ty == right.ty && !left.ty.is_ref {
                if let Some(help) = raw_operator_help(hook, left.ty.id) {
                    diagnostic = diagnostic.help(help);
                }
            }
            self.emit_requirement(
                diagnostic,
                Requirement::Mismatch {
                    expected_ref: false,
                },
                &[left.ty.id, right.ty.id],
            );
            return HirExpr::poison(span);
        };
        HirExpr {
            ty: found.sig.returns[0],
            span,
            kind: HirExprKind::Call {
                callee: HirCallee::Func(found.key),
                args: vec![left, right],
            },
        }
    }

    /// Checks both operands once: non-literal sides first, so a literal side
    /// takes the other side's type.
    pub(crate) fn check_sides(
        &mut self,
        fcx: &mut FnCx,
        lhs: &Expr,
        rhs: &Expr,
    ) -> (HirExpr, HirExpr) {
        let (left_lit, right_lit) = (is_foldable_literal(lhs), is_foldable_literal(rhs));
        if left_lit && right_lit {
            let left = self.check_expr(fcx, lhs, None);
            let right = self.check_expr(fcx, rhs, None);
            return (left, right);
        }
        let left = (!left_lit).then(|| self.check_expr(fcx, lhs, None));
        let right = (!right_lit).then(|| self.check_expr(fcx, rhs, None));
        let anchor = |side: &Option<HirExpr>| {
            side.as_ref()
                .map(|e| e.ty.id)
                .filter(|id| *id != TypeId::ERROR)
        };
        let left = if let Some(left) = left {
            left
        } else {
            let want = anchor(&right);
            self.check_expr(fcx, lhs, want)
        };
        let right = if let Some(right) = right {
            right
        } else {
            let want = Some(left.ty.id).filter(|id| *id != TypeId::ERROR);
            self.check_expr(fcx, rhs, want)
        };
        (left, right)
    }

    // ---- value control flow -------------------------------------------------

    fn check_if_value(
        &mut self,
        fcx: &mut FnCx,
        cond: &Expr,
        then: &Expr,
        els: &Expr,
        span: Span,
        expected: Option<TypeId>,
    ) -> HirExpr {
        let cond = self.check_cond(fcx, cond, "if");
        let then = self.check_expr(fcx, then, expected);
        let els_expected = expected.or_else(|| (!then.ty.is_error()).then_some(then.ty.deref().id));
        let els = self.check_expr(fcx, els, els_expected);
        let result = match expected {
            Some(want) => TypeRef::value(want),
            None if !then.ty.is_error() => then.ty.deref(),
            None if !els.ty.is_error() => els.ty.deref(),
            None => return HirExpr::poison(span),
        };
        if expected.is_none()
            && !then.ty.is_error()
            && !els.ty.is_error()
            && then.ty.id != els.ty.id
        {
            self.diags.emit(
                Diagnostic::error(
                    codes::E010,
                    format!(
                        "mismatched types: `if` branches produce {} and {}",
                        self.types.display(then.ty.id),
                        self.types.display(els.ty.id)
                    ),
                )
                .label(span, true, "branches disagree here"),
            );
            return HirExpr::poison(span);
        }
        let then = self.coerce(then, result, span);
        let els = self.coerce(els, result, span);
        HirExpr {
            ty: result,
            span,
            kind: HirExprKind::If {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(els),
            },
        }
    }

    fn check_match_value(
        &mut self,
        fcx: &mut FnCx,
        scrutinee: &Expr,
        arms: &[MatchArm],
        els: &Expr,
        span: Span,
        expected: Option<TypeId>,
    ) -> HirExpr {
        let scrutinee = self.check_scrutinee(fcx, scrutinee);
        let mut checked = Vec::with_capacity(arms.len());
        let mut first: Option<TypeRef> = None;
        let mut failed = false;
        for arm in arms {
            fcx.open(arm.span);
            let (pattern, capture) = self.check_pattern(fcx, arm, scrutinee.ty.id);
            let value = match &arm.body {
                MatchArmBody::Inline(stmt) => {
                    if let Stmt::ExprStmt(value) = stmt.as_ref() {
                        self.check_expr(fcx, value, expected.or(first.map(|t| t.id)))
                    } else {
                        self.diags.emit(
                            Diagnostic::error(codes::E010, "match arm does not produce a value")
                                .label(arm.span, true, "no value here"),
                        );
                        failed = true;
                        HirExpr::poison(arm.span)
                    }
                }
                MatchArmBody::Block(_) => {
                    self.diags.emit(
                        Diagnostic::error(codes::E010, "match arm block cannot produce a value")
                            .label(arm.span, true, "no value here"),
                    );
                    failed = true;
                    HirExpr::poison(arm.span)
                }
            };
            fcx.close();
            if first.is_none() && !value.ty.is_error() {
                first = Some(value.ty.deref());
            }
            checked.push((pattern, capture, value, arm.span));
        }
        let els = self.check_expr(fcx, els, expected.or(first.map(|t| t.id)));
        let result = match (expected, first) {
            (Some(want), _) => TypeRef::value(want),
            (None, Some(first)) => first,
            (None, None) if !els.ty.is_error() => els.ty.deref(),
            _ => return HirExpr::poison(span),
        };
        if expected.is_none() && !failed {
            let disagree = checked
                .iter()
                .map(|(_, _, value, _)| value.ty)
                .chain(std::iter::once(els.ty))
                .any(|ty| !ty.is_error() && ty.id != result.id);
            if disagree {
                self.diags.emit(
                    Diagnostic::error(codes::E010, "match arms produce different types").label(
                        span,
                        true,
                        "arms disagree here",
                    ),
                );
                return HirExpr::poison(span);
            }
        }
        let arms = checked
            .into_iter()
            .map(|(pattern, capture, value, arm_span)| HirValueArm {
                pattern,
                capture,
                value: self.coerce(value, result, arm_span),
            })
            .collect();
        let els = self.coerce(els, result, span);
        HirExpr {
            ty: result,
            span,
            kind: HirExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
                els: Box::new(els),
            },
        }
    }

    // ---- literals of composite types ---------------------------------------

    /// `Type { field = value, ... }` (arguments named or inferred).
    #[allow(clippy::too_many_lines)]
    fn check_struct_lit(
        &mut self,
        fcx: &mut FnCx,
        ty: &AstType,
        fields: &[StructField],
        span: Span,
        expected: Option<TypeId>,
    ) -> HirExpr {
        let plain = match &ty.shape {
            TypeShape::Named { name, args }
                if args.is_empty() && !self.env.params.contains(name) =>
            {
                Some(name.as_str())
            }
            _ => None,
        };
        let object = if let Some(name) = plain {
            let Ok(found) = self.lookup_item(name, ty.name_span) else {
                return HirExpr::poison(span);
            };
            match found {
                Some(def) if self.res.def(def).is_type() => {
                    self.record_use(ty.name_span, def);
                    self.record_semantic(ty.name_span, SemanticKind::UserType);
                    if self.res.def(def).generic_params.is_empty() {
                        let object = self.type_of_def(def, Vec::new());
                        if self.ensure_type(object, span).is_err() {
                            return HirExpr::poison(span);
                        }
                        object
                    } else {
                        let inferred = expected.filter(|id| {
                            matches!(self.types.kind(*id), TypeKind::Instance { base, .. } if *base == def)
                        });
                        let Some(id) = inferred else {
                            self.diags.emit(
                                Diagnostic::error(
                                    codes::E023,
                                    format!("cannot infer generic arguments for `{name}`"),
                                )
                                .label(
                                    span,
                                    true,
                                    format!(
                                        "write `{name}[...] {{ ... }}` or annotate the binding"
                                    ),
                                ),
                            );
                            return HirExpr::poison(span);
                        };
                        TypeRef::value(id)
                    }
                }
                _ => {
                    let diagnostic = Diagnostic::error(
                        codes::E023,
                        format!("unknown type `{name}`"),
                    )
                    .label(ty.span, true, "no such type in scope");
                    let diagnostic = self.suggest_type(diagnostic, name, ty.name_span);
                    self.diags.emit(diagnostic);
                    return HirExpr::poison(span);
                }
            }
        } else {
            let resolved = self.resolve_type(ty);
            if resolved.is_error() {
                return HirExpr::poison(span);
            }
            resolved.deref()
        };
        if matches!(self.types.kind(object.id), TypeKind::Array { .. }) {
            return self.array_literal(object, fields, span);
        }
        let display = self.types.display(object.id);
        let Some(layout) = self.fields_of(object.id) else {
            self.diags.emit(
                Diagnostic::error(codes::E010, format!("`{display}` is not an object")).label(
                    span,
                    true,
                    "only objects construct with `{ ... }`",
                ),
            );
            return HirExpr::poison(span);
        };
        let owner = self.type_parts(object.id).map(|(def, _)| def);
        let mut values: Vec<Option<HirExpr>> = vec![None; layout.len()];
        let mut seen = HashSet::new();
        let mut failed = false;
        for field in fields {
            if !seen.insert(field.name.clone()) {
                self.diags.emit(
                    Diagnostic::error(
                        codes::E020,
                        format!("duplicate field `{}` in struct literal", field.name),
                    )
                    .label(field.span, true, "redefined here"),
                );
                failed = true;
                continue;
            }
            let Some(index) = layout.iter().position(|(name, _)| *name == field.name) else {
                let is_member = owner
                    .and_then(|owner| self.res.member(owner, &field.name))
                    .is_some();
                let (code, msg) = if is_member {
                    (
                        codes::E022,
                        format!("`{}` is not a field of `{display}`", field.name),
                    )
                } else {
                    (
                        codes::E021,
                        format!("no field `{}` on `{display}`", field.name),
                    )
                };
                let mut diagnostic = Diagnostic::error(code, msg).label(
                    field.span,
                    true,
                    "only fields initialize here",
                );
                if !is_member {
                    let fields = layout.iter().map(|(name, _)| name.as_str());
                    diagnostic = crate::suggest::suggest(
                        diagnostic,
                        &field.name,
                        leading_field(field),
                        fields,
                    );
                }
                self.diags.emit(diagnostic);
                failed = true;
                continue;
            };
            if let Some(owner) = owner {
                if let Some(member) = self.res.member(owner, &field.name) {
                    if self.private_denied(member, owner, field.span) {
                        failed = true;
                        continue;
                    }
                    self.record_use(leading_field(field), member);
                }
            }
            let want = layout[index].1;
            let value = self.check_expr(fcx, &field.value, Some(want.id));
            let value = self.coerce(value, want, field.span);
            failed |= value.ty.is_error();
            values[index] = Some(value);
        }
        let mut missing: Vec<&str> = layout
            .iter()
            .zip(&values)
            .filter(|(_, value)| value.is_none())
            .map(|((name, _), _)| name.as_str())
            .filter(|name| !seen.contains(*name))
            .collect();
        missing.sort_unstable();
        if let Some(first) = missing.first() {
            self.diags.emit(
                Diagnostic::error(
                    codes::E022,
                    format!("struct literal missing field `{first}` of `{display}`"),
                )
                .label(span, true, "add the missing field"),
            );
            failed = true;
        }
        if failed {
            return HirExpr::poison(span);
        }
        HirExpr {
            ty: object,
            span,
            kind: HirExprKind::Struct(
                values
                    .into_iter()
                    .map(|value| value.unwrap_or_else(|| HirExpr::poison(span)))
                    .collect(),
            ),
        }
    }

    /// `$"..."`: each piece is a `@byte_buffer` (holes convert).
    fn check_fstring(&mut self, fcx: &mut FnCx, parts: &[FStrPart], span: Span) -> HirExpr {
        if parts.len() > 16 {
            self.diags.emit(
                Diagnostic::error(codes::E061, "too many format pieces (max 16)").label(
                    span,
                    true,
                    "too many pieces here",
                ),
            );
            return HirExpr::poison(span);
        }
        let buffer = Builtin::ByteBuffer.value();
        let pieces = parts
            .iter()
            .map(|part| match part {
                FStrPart::Lit(bytes) => HirExpr {
                    ty: buffer,
                    span,
                    kind: HirExprKind::Str(bytes.clone()),
                },
                FStrPart::Hole(expr) => {
                    let value = self.check_expr(fcx, expr, None);
                    self.coerce(value, buffer, expr.span())
                }
            })
            .collect();
        HirExpr {
            ty: Builtin::FormatArgs.value(),
            span,
            kind: HirExprKind::FormatArgs(pieces),
        }
    }

    // ---- module constants ---------------------------------------------------

    /// The declared type of a constant.
    pub(crate) fn const_type(&mut self, def: DefId) -> TypeRef {
        if let Some(ty) = self.const_types.get(&def) {
            return *ty;
        }
        let DefKind::Const { ty, .. } = &self.res.def(def).kind else {
            return TypeRef::ERROR;
        };
        let ty = ty.clone();
        let env = Env {
            file: Some(self.res.def(def).file),
            ..Env::default()
        };
        let resolved = self.with_env(env, |c| c.resolve_type(&ty));
        self.const_types.insert(def, resolved);
        resolved
    }

    /// Checks every constant's initializer, then rejects reference cycles.
    /// `@builtin` constants are checked on first use instead
    /// ([`Checker::use_builtin_const`]).
    pub(crate) fn check_consts(&mut self) {
        for def in self.res.ids() {
            if matches!(self.res.def(def).kind, DefKind::Const { .. })
                && !self.is_builtin(def)
                && !self.out.consts.contains_key(&def)
            {
                self.check_const(def);
            }
        }
        let mut done = HashSet::new();
        let consts: Vec<DefId> = self.out.consts.keys().copied().collect();
        for def in consts {
            self.const_cycle(def, &mut Vec::new(), &mut done);
        }
    }

    /// Checks one constant's initializer into the output.
    fn check_const(&mut self, def: DefId) {
        let DefKind::Const { value, .. } = &self.res.def(def).kind else {
            return;
        };
        let value = value.clone();
        let ty = self.const_type(def);
        let env = Env {
            file: Some(self.res.def(def).file),
            ..Env::default()
        };
        let hir = self.with_env(env, |c| {
            let mut fcx = FnCx::new(Span::dummy());
            let hir = c.check_expr(&mut fcx, &value, (!ty.is_error()).then_some(ty.id));
            c.coerce(hir, ty, value.span())
        });
        self.out.consts.insert(def, hir);
    }

    /// Checks a `@builtin` constant the first time code names it, so its
    /// type is laid out only in programs that use it. They never refer to
    /// each other, so this cannot recurse into itself.
    fn use_builtin_const(&mut self, def: DefId) {
        if self.is_builtin(def) && !self.out.consts.contains_key(&def) {
            self.check_const(def);
        }
    }

    /// Whether `def` is declared in the generated `@builtin` namespace.
    pub(crate) fn is_builtin(&self, def: DefId) -> bool {
        self.res
            .namespace_path(self.res.def(def).ns)
            .starts_with("@builtin")
    }

    fn const_cycle(&mut self, def: DefId, stack: &mut Vec<DefId>, done: &mut HashSet<DefId>) {
        if done.contains(&def) {
            return;
        }
        if stack.contains(&def) {
            let span = self
                .out
                .consts
                .get(&def)
                .map_or(Span::dummy(), |value| value.span);
            self.diags
                .emit(Diagnostic::error(codes::E022, "cyclic constant").label(
                    span,
                    true,
                    "constants cannot refer to themselves",
                ));
            done.insert(def);
            return;
        }
        let mut used = Vec::new();
        if let Some(value) = self.out.consts.get(&def) {
            value.walk(&mut |expr| {
                if let HirExprKind::Const(other) = expr.kind {
                    used.push(other);
                }
            });
        }
        stack.push(def);
        for other in used {
            self.const_cycle(other, stack, done);
        }
        stack.pop();
        done.insert(def);
    }
}

/// The hook a binary operator calls.
fn hook_name(op: BinOp) -> &'static str {
    let index = match op {
        BinOp::Add => 0,
        BinOp::Sub => 1,
        BinOp::Mul => 2,
        BinOp::Div => 3,
        BinOp::Mod => 4,
        BinOp::Lt => 5,
        BinOp::Gt => 6,
        BinOp::LtEq => 7,
        _ => 8,
    };
    BINARY_HOOKS[index]
}

/// The help for an operator on two raw numbers of one builtin type, which
/// have no operator hooks: the `Standard` type that has them, and the
/// intrinsic that does the operation (`@i_add_32`).
fn raw_operator_help(hook: &str, ty: TypeId) -> Option<String> {
    let builtin = Builtin::ALL
        .into_iter()
        .find(|builtin| builtin.id() == ty)?;
    let (family, width, standard) = match builtin {
        Builtin::Bit8 => ("i", "8", Some("Int8")),
        Builtin::Bit16 => ("i", "16", Some("Int16")),
        Builtin::Bit32 => ("i", "32", Some("Int32")),
        Builtin::Bit64 => ("i", "64", Some("Int64")),
        Builtin::BitNative => ("i", "native", Some("Int")),
        Builtin::FBit16 => ("f", "16", None),
        Builtin::FBit32 => ("f", "32", Some("Float32")),
        Builtin::FBit64 => ("f", "64", Some("Float64")),
        Builtin::FBit128 => ("f", "128", None),
        _ => return None,
    };
    let intrinsic = format!("@{family}_{}_{width}", hook.strip_prefix('@')?);
    Intrinsic::lookup(&intrinsic)?;
    let name = builtin.name();
    Some(match standard {
        Some(standard) => format!(
            "`{name}` has no operators; use `Standard`'s `{standard}`, or call `{intrinsic}`"
        ),
        None => format!("`{name}` has no operators; call `{intrinsic}`"),
    })
}

/// The name token of a struct-literal field (fields begin at their name).
fn leading_field(field: &StructField) -> Span {
    crate::sigs::leading(field.span, &field.name)
}
