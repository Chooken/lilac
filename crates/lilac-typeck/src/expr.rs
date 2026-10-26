//! Expressions, places, and the one coercion rule.
//!
//! [`Checker::coerce`] is the single place a value meets an expected type:
//! a `ref T` flowing into a `T` slot is dereferenced (an explicit `Deref`
//! node), an exact match passes, and anything else takes exactly one
//! `@conversion` (`E043` when there is none).

use std::collections::HashSet;

use lilac_ast::{
    BinOp, Expr, MatchArm, MatchArmBody, Stmt, StructField, Type as AstType, TypeShape,
};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_hir::{
    FuncKey, HirCallee, HirExpr, HirExprKind, HirValueArm, Intrinsic, MemberReceiver, SemanticKind,
};
use lilac_resolve::{DefId, DefKind, Visibility};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeId, TypeKind, TypeRef};

use crate::body::{local_expr, FnCx};
use crate::calls::Called;
use crate::literals::is_foldable_literal;
use crate::paths::{path_expr, path_tail, shadowed_alias_note};
use crate::reflect::{stand_in_field, Assumption};
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

/// How a name is used as a value: whether it is `ref`'s operand or a place
/// (`behind_ref`: a function named there is its address, no `E077`), and
/// the generic arguments naming one of a generic function's instances
/// (`ref id[@bit32]`; none for a plain name).
#[derive(Clone, Copy)]
pub(crate) struct Naming<'a> {
    /// Under `ref`, or a place.
    pub(crate) behind_ref: bool,
    /// The function's own generic arguments.
    pub(crate) generic_args: &'a [AstType],
}

impl Naming<'_> {
    /// A plain value.
    pub(crate) const VALUE: Naming<'static> = Naming {
        behind_ref: false,
        generic_args: &[],
    };
    /// `ref`'s operand, or a place.
    pub(crate) const PLACE: Naming<'static> = Naming {
        behind_ref: true,
        generic_args: &[],
    };
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
            Expr::Tag(name, span) => self.check_tag(fcx, name, *span, expected),
            Expr::Number(bytes, span) => self.check_number(bytes, *span, expected),
            Expr::Str(bytes, span) => HirExpr {
                ty: Builtin::ByteBuffer.value(),
                span: *span,
                kind: HirExprKind::Str(bytes.clone()),
            },
            Expr::Char(scalar, span) => HirExpr {
                ty: Builtin::Bit32.value(),
                span: *span,
                kind: HirExprKind::Int(u64::from(u32::from(*scalar))),
            },
            Expr::RefOf { value, span } => self.check_ref_of(fcx, value, *span),
            Expr::Not { .. } => self.check_condition(fcx, expr, "not").0,
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
                // A call used as a value is its first out; the rest drop.
                Ok(Called::Call { callee, args, outs }) => {
                    self.call_value(callee, args, &outs, *span, expected)
                }
                Ok(Called::Value(value)) => value,
                Err(_) => HirExpr::poison(*span),
            },
            Expr::Construct {
                function,
                member_span,
                generic_args,
                args,
                span,
            } => self.check_construct(
                fcx,
                function,
                *member_span,
                generic_args,
                args,
                *span,
                expected,
            ),
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
            Expr::FString { parts, span } => self.check_fstring(fcx, parts, *span, expected),
            Expr::TypeApp(ty) => self.check_type_app(fcx, ty, false),
            Expr::Instance {
                callee,
                generic_args,
                span,
            } => self.check_instance_value(fcx, callee, generic_args, *span, false),
            Expr::Error { span } => HirExpr::poison(*span),
        }
    }

    /// A call used as a value: its first out; the rest drop.
    fn call_value(
        &mut self,
        callee: HirCallee,
        args: Vec<HirExpr>,
        outs: &[TypeRef],
        span: Span,
        expected: Option<TypeId>,
    ) -> HirExpr {
        // `@panic` and `@trap` never return, so as a value they are
        // whatever is expected (`if c then @panic(m) else x`).
        if let (HirCallee::Intrinsic(Intrinsic::Panic | Intrinsic::Trap), Some(want)) =
            (&callee, expected)
        {
            return HirExpr {
                ty: self.types.value_type(want),
                span,
                kind: HirExprKind::Call {
                    callee,
                    args,
                    rest: Vec::new(),
                },
            };
        }
        let Some((&first, rest)) = outs.split_first() else {
            self.diags.emit(
                Diagnostic::error(codes::E022, "this call returns no value").label(
                    span,
                    true,
                    "a value is expected here",
                ),
            );
            return HirExpr::poison(span);
        };
        HirExpr {
            ty: first,
            span,
            kind: HirExprKind::Call {
                callee,
                args,
                rest: rest.to_vec(),
            },
        }
    }

    /// Fits `expr` into a `want` slot: deref, exact, or one conversion.
    pub(crate) fn coerce(&mut self, expr: HirExpr, want: TypeRef, want_span: Span) -> HirExpr {
        self.coerce_or_fix(expr, want, want_span, None)
    }

    /// [`Checker::coerce`]; `cast` is the span of an `as T` after a postfix
    /// operand, which a conversion method's call may replace.
    fn coerce_or_fix(
        &mut self,
        expr: HirExpr,
        want: TypeRef,
        want_span: Span,
        cast: Option<Span>,
    ) -> HirExpr {
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
        // A format slot: a literal is a format, and so is any one value.
        if let Some(writer) = self.format_writer(want.id) {
            if expr.ty.id != want.id {
                return self.format_slot(expr, writer);
            }
        }
        if expr.ty.is_ref && expr.ty.id != want.id {
            let span = expr.span;
            self.mismatch(want, expr.ty, want_span, span);
            return HirExpr::poison(span);
        }
        let expr = self.auto_deref(expr);
        if expr.ty.id == want.id {
            return expr;
        }
        if let Some(converted) = self.try_convert(expr.clone(), want.id) {
            return converted;
        }
        let from = self.show_id(expr.ty.id);
        let target = self.show_id(want.id);
        let source = expr.ty.id;
        let diagnostic = Diagnostic::error(
            codes::E043,
            format!("no conversion from {from} to {target}"),
        )
        .label(want_span, false, format!("expected `{target}` here"))
        .label(expr.span, true, "conversion attempted here");
        let diagnostic = self.conversion_methods(diagnostic, source, want.id, cast);
        self.emit_requirement(
            diagnostic,
            Requirement::Conversion { from: source },
            &[expr.ty.id, want.id],
        );
        HirExpr::poison(expr.span)
    }

    /// Names the methods of `from`'s type that take only `self` and give a
    /// `to`: the conversions it offers where no `@conversion` is exact
    /// (`Standard`'s `to_int32_truncated`, `to_float64_rounded`). Each that
    /// cannot fail is a fix replacing `cast`, when the error is an `as`.
    fn conversion_methods(
        &mut self,
        mut diagnostic: Diagnostic,
        from: TypeId,
        to: TypeId,
        cast: Option<Span>,
    ) -> Diagnostic {
        let Some((def, args)) = self.type_parts(from) else {
            return diagnostic;
        };
        if !args.is_empty() {
            return diagnostic;
        }
        let mut members: Vec<(String, DefId)> = self
            .res
            .def(def)
            .members
            .iter()
            .map(|(name, id)| (name.clone(), *id))
            .collect();
        members.sort();
        let mut methods = Vec::new();
        for (name, member) in members {
            let found = self.res.def(member);
            let (DefKind::Func(decl), Visibility::Public) = (&found.kind, found.vis) else {
                continue;
            };
            let fails = match &decl.failures {
                None => false,
                Some(outs) if outs.is_empty() => true,
                Some(_) => continue,
            };
            if !decl.generic_params.is_empty()
                || decl.params.len() != 1
                || decl.returns.len() != 1
                || !decl.params[0].is_receiver()
            {
                continue;
            }
            let Ok(sig) = self.sig_of(member) else {
                continue;
            };
            let out = sig.returns[0];
            if sig.params[0].id == from && out.id == to && !out.is_ref {
                methods.push((name, fails));
            }
        }
        if methods.is_empty() {
            return diagnostic;
        }
        let listed: Vec<String> = methods
            .iter()
            .map(|(name, fails)| {
                if *fails {
                    format!("`{name}()` (which can fail)")
                } else {
                    format!("`{name}()`")
                }
            })
            .collect();
        let (shown_from, shown_to) = (self.show_id(from), self.show_id(to));
        diagnostic = diagnostic.help(format!(
            "`{shown_from}` converts to `{shown_to}` with a method: {}",
            listed.join(", ")
        ));
        if let Some(cast) = cast {
            for (name, _) in methods.iter().filter(|(_, fails)| !fails) {
                diagnostic = diagnostic.fix(
                    cast,
                    format!(".{name}()"),
                    format!("replace the `as` with `.{name}()`"),
                );
            }
        }
        diagnostic
    }

    /// Wraps a by-value `expr` in its `@conversion` to `to`, if one exists.
    pub(crate) fn try_convert(&mut self, expr: HirExpr, to: TypeId) -> Option<HirExpr> {
        let (from, to) = (TypeRef::value(expr.ty.id), TypeRef::value(to));
        let (callee, _) = self.find_hook("@conversion", expr.span, |sig| {
            sig.params == [from] && sig.returns == [to]
        })?;
        Some(HirExpr {
            ty: to,
            span: expr.span,
            kind: HirExprKind::Call {
                callee,
                args: vec![expr],
                rest: Vec::new(),
            },
        })
    }

    /// `*expr` when `expr` is a reference, never through an opaque one
    /// (`ref unknown`, a function reference: [`lilac_ty::Types::is_opaque_ref`]).
    pub(crate) fn auto_deref(&self, expr: HirExpr) -> HirExpr {
        if !expr.ty.is_ref || self.types.is_opaque_ref(expr.ty) {
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
        self.check_name(fcx, name, span, Naming::VALUE)
    }

    /// A name or path as a value. Under `ref` (or as a place, `naming`) a
    /// function named here is its address without `E077`; `naming`'s
    /// generic arguments name a generic function's instance.
    fn check_name(&mut self, fcx: &mut FnCx, name: &str, span: Span, naming: Naming) -> HirExpr {
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
        if let Some(def) = self.own_member(name) {
            return self.own_value(def, name, span, naming);
        }
        let Ok(found) = self.lookup_item(name, span) else {
            return HirExpr::poison(span);
        };
        let Some(def) = found else {
            let diagnostic = Diagnostic::error(codes::E021, format!("unresolved name `{name}`"))
                .label(span, true, "not a local or item");
            let diagnostic = if self.is_owner_param(name) {
                self.owner_param_note(diagnostic, name)
            } else {
                let diagnostic = self.own_instance_help(fcx, diagnostic, name);
                self.suggest_value(fcx, diagnostic, name, span)
            };
            self.diags.emit(diagnostic);
            return HirExpr::poison(span);
        };
        self.record_use(path_tail(span, name), def);
        match &self.res.def(def).kind {
            DefKind::Func(_) => self.func_value(def, Vec::new(), name, span, naming),
            DefKind::Const { .. } | DefKind::Global { .. } => self.item_value(def, span),
            DefKind::Object | DefKind::Enum | DefKind::Alias(_) => {
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
                    .label(span, true, "hooks are not function values"),
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

    /// The constant or global `def` named at `span` as a value.
    pub(crate) fn item_value(&mut self, def: DefId, span: Span) -> HirExpr {
        if let DefKind::Const { .. } = self.res.def(def).kind {
            return self.const_value(def, span);
        }
        let ty = self.const_type(def);
        if ty.is_error() {
            // The type's own error was reported at the declaration.
            return HirExpr::poison(span);
        }
        HirExpr {
            ty,
            span,
            kind: HirExprKind::Global(def),
        }
    }

    /// The function `def` (of a type instantiated at `type_args`, if a
    /// member) as a value: its address, a `ref function (…)` whose
    /// parameters are the function's own, a method's receiver first. An
    /// `export` or `extern` is a C function, so its value is a `ref export
    /// function (…)`, the C symbol's address. Named without `ref`
    /// (`naming` not behind `ref`) it is `E077`, and still that value;
    /// `naming`'s generic arguments (exactly the function's own, `E044`)
    /// follow `type_args`.
    pub(crate) fn func_value(
        &mut self,
        def: DefId,
        type_args: Vec<TypeRef>,
        name: &str,
        span: Span,
        naming: Naming,
    ) -> HirExpr {
        let Some(decl) = self.res.def(def).func() else {
            lilac_error::bug!(span, "`{name}` is not a function");
        };
        let (variadic, fails) = (decl.variadic, decl.failures.is_some());
        let c_abi = decl.is_export || decl.is_extern;
        if variadic {
            self.diags.emit(
                Diagnostic::error(
                    codes::E028,
                    format!("cannot use variadic extern `{name}` as a function value"),
                )
                .label(span, true, "function types have no `...`")
                .help("call it directly, or wrap it in a function with fixed parameters"),
            );
            return HirExpr::poison(span);
        }
        if fails {
            self.diags.emit(
                Diagnostic::error(
                    codes::E057,
                    format!("`{name}` can fail, so it cannot be a function value"),
                )
                .label(span, true, "a function value cannot fail")
                .help("call it here, or wrap the call in a function that handles it"),
            );
            return HirExpr::poison(span);
        }
        let own = self.res.def(def).generic_params.len();
        let given = naming.generic_args;
        if given.len() != own {
            let noun = if own == 1 { "argument" } else { "arguments" };
            let mut diagnostic = Diagnostic::error(
                codes::E044,
                format!("wrong number of generic arguments for `{name}`"),
            )
            .label(
                span,
                true,
                format!("expected {own} generic {noun}, found {}", given.len()),
            );
            if given.is_empty() {
                diagnostic = diagnostic.help(format!(
                    "a function value is one instance: give `{name}` its arguments, `{name}[…]`"
                ));
            }
            self.diags.emit(diagnostic);
            return HirExpr::poison(span);
        }
        let mut args = type_args;
        for arg in given {
            let arg = self.resolve_type(arg);
            if arg.is_error() {
                // `resolve_type` reported it.
                return HirExpr::poison(span);
            }
            args.push(arg);
        }
        if !naming.behind_ref {
            self.diags.emit(
                Diagnostic::error(
                    codes::E077,
                    format!("`{name}` is a function, a value only behind `ref`"),
                )
                .label(span, true, "write `ref` before it")
                .fix(
                    Span::new(span.lo.0, span.lo.0, span.file),
                    "ref ",
                    "a function value is a reference to its code: add `ref`",
                ),
            );
        }
        let key = FuncKey { def, args };
        let Ok(sig) = self.request(key.clone(), span) else {
            return HirExpr::poison(span);
        };
        let id = self.types.intern_func_abi(sig.params, sig.returns, c_abi);
        HirExpr {
            ty: TypeRef::reference(id),
            span,
            kind: HirExprKind::FuncRef(key),
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

    /// Records the static member access `Type.field` ending `access` for
    /// the editor ([`Self::record_member`]).
    pub(crate) fn record_static_member(
        &mut self,
        receiver: Receiver,
        access: Span,
        field: &str,
    ) -> Result<(), ErrorGuaranteed> {
        let def = match receiver {
            Receiver::Type(ty) if !ty.is_ref => self.type_parts(ty.id).map(|(def, _)| def),
            Receiver::Type(_) => None,
            Receiver::Bare(def) => Some(def),
        };
        self.record_member(access, field, def.map(MemberReceiver::Type))
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
                if fcx.lookup(name).is_some() || self.own_member(name).is_some() {
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
                    Ok(Some(def)) if matches!(self.res.def(def).kind, DefKind::Alias(_)) => {
                        self.record_use(path_tail(*span, name), def);
                        let ty = self.alias_type(def, *span);
                        if ty.is_error() {
                            // Reported at the alias.
                            return Some(Err(self.diags.already_reported()));
                        }
                        return Some(self.ensure_type(ty, *span).map(|()| Receiver::Type(ty)));
                    }
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

    /// Checks an assignable place: a local, a global, a field chain, or
    /// what a call's returned `ref` points to.
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
                let item = self.check_name(fcx, name, *span, Naming::PLACE);
                self.item_place(item, name)
            }
            Expr::Member { obj, field, span } => {
                if let Some((path, span)) = self.qualified_name(fcx, expr) {
                    let item = self.check_name(fcx, &path, span, Naming::PLACE);
                    return self.item_place(item, &path);
                }
                // `Type.NAME`: a constant or variant, never a place.
                if let Some(receiver) = self.static_receiver(fcx, obj) {
                    let item = match receiver {
                        Ok(receiver) => {
                            self.static_member(fcx, receiver, field, *span, None, Naming::PLACE)
                        }
                        Err(_) => HirExpr::poison(*span),
                    };
                    return self.item_place(item, field);
                }
                let base = self.check_place(fcx, obj)?;
                if base.ty.is_error() {
                    return Ok(HirExpr::poison(*span));
                }
                let shadowed = self.shadowed_alias(fcx, &base);
                Ok(self.field_access(fcx, base, field, *span, None, false, shadowed))
            }
            Expr::Call { span, .. } => {
                let call = self.check_expr(fcx, expr, None);
                if call.ty.is_error() {
                    return Ok(call);
                }
                if call.ty.is_ref && !self.types.is_opaque_ref(call.ty) {
                    // What the returned `ref` points to.
                    return Ok(self.auto_deref(call));
                }
                let label = if call.ty.is_ref {
                    "this call returns an opaque `ref`, which has nothing to write through"
                } else {
                    "this call returns a value, not a `ref`"
                };
                Err(self.diags.emit(
                    Diagnostic::error(codes::E010, "cannot assign to a call's result")
                        .label(*span, true, label),
                ))
            }
            _ => Err(self.diags.emit(
                Diagnostic::error(codes::E010, "cannot assign to this expression").label(
                    expr.span(),
                    true,
                    "only variables, fields and calls returning a `ref` are assignable",
                ),
            )),
        }
    }

    /// An item named as an assignable place: only a global is one.
    fn item_place(&mut self, item: HirExpr, name: &str) -> Result<HirExpr, ErrorGuaranteed> {
        match item.kind {
            HirExprKind::Global(_) => Ok(item),
            // `check_ident` reported why the name is no value.
            HirExprKind::Poison => Err(self.diags.already_reported()),
            _ => Err(self.diags.emit(
                Diagnostic::error(codes::E010, format!("cannot assign to `{name}`")).label(
                    item.span,
                    true,
                    "only variables, globals and fields are assignable",
                ),
            )),
        }
    }

    /// `base.field` on an object (through a reference if needed).
    /// `shadowed` is the alias a local `base` hides ([`Self::shadowed_alias`]).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn field_access(
        &mut self,
        fcx: &FnCx,
        base: HirExpr,
        field: &str,
        span: Span,
        expected: Option<TypeId>,
        annotate: bool,
        shadowed: Option<(String, Span)>,
    ) -> HirExpr {
        let base = self.auto_deref(base);
        if let Some(assumed) = fcx.assumed_field(&base, field) {
            return stand_in_field(assumed, base, span);
        }
        let receiver = MemberReceiver::Value(base.ty);
        if self.record_member(span, field, Some(receiver)).is_err() {
            return HirExpr::poison(span);
        }
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
        } else if self.is_field_member(ty, field) {
            // The type has no layout: laying it out failed, which reported
            // its error.
            self.diags.already_reported();
            return HirExpr::poison(span);
        }
        let owner = self.show_id(ty);
        let mut diagnostic = Diagnostic::error(
            codes::E021,
            format!("no field `{field}` on `{owner}`"),
        )
        .label(span, true, "no such member");
        if let Some((def, _)) = self.type_parts(ty) {
            diagnostic = self.suggest_member(diagnostic, def, field, tail_span(span, field), false);
            // A constant belongs to the type, not to its values.
            if let Some(member) = self.res.member(def, field) {
                if matches!(self.res.def(member).kind, DefKind::Const { .. }) {
                    let name = &self.res.def(def).name;
                    diagnostic = diagnostic.help(format!(
                        "`{field}` is a constant of `{name}`: write `{name}.{field}`"
                    ));
                }
            }
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

    /// `E026` for a private member used outside its type's namespace. A
    /// member `private` reaches like a top-level one: the type's own
    /// namespace and those inside it (`Resolved::reaches`), so types in one
    /// folder (`Shared` and `Weak`) build and read each other.
    pub(crate) fn private_denied(&mut self, member: DefId, owner: DefId, access: Span) -> bool {
        if !self.private_hidden(member, owner) {
            return false;
        }
        let def = self.res.def(member);
        let decl = def.span;
        let path = match self.res.namespace_path(self.res.def(owner).ns) {
            "" => "the root namespace".to_owned(),
            path => format!("`{path}`"),
        };
        let diagnostic =
            Diagnostic::error(codes::E026, format!("`{}` is a private member", def.name))
                .label(
                    access,
                    true,
                    "accessed from outside its structure's namespace",
                )
                .label(decl, false, "declared private here")
                .note(format!(
                    "only code in {path} and the namespaces inside it can use it"
                ));
        self.diags.emit(diagnostic);
        true
    }

    /// Whether `member` of `owner` is private and out of reach here
    /// ([`Self::private_denied`], which reports it).
    pub(crate) fn private_hidden(&self, member: DefId, owner: DefId) -> bool {
        self.res.def(member).vis == Visibility::Private
            && self.env.owner != Some(owner)
            && !self.res.reaches(self.file_ns(), self.res.def(owner).ns)
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
            return self.static_member(fcx, receiver, field, span, expected, Naming::VALUE);
        }
        let base = self.check_expr(fcx, obj, None);
        if base.ty.is_error() {
            return base;
        }
        let shadowed = self.shadowed_alias(fcx, &base);
        self.field_access(fcx, base, field, span, expected, true, shadowed)
    }

    /// `Type.name` as a value: a constant of the type, a function of it
    /// (its address, `E077` unless `naming` is behind `ref`), or a variant.
    fn static_member(
        &mut self,
        fcx: &FnCx,
        receiver: Receiver,
        field: &str,
        span: Span,
        expected: Option<TypeId>,
        naming: Naming,
    ) -> HirExpr {
        if let Some(stand_in) = fcx.assumed_constant(receiver, field, span) {
            return stand_in;
        }
        if let Receiver::Type(ty) = receiver {
            let assumed = fcx.assumption(ty.id, field, Intrinsic::HasVariant);
            if let Some(assumed) = assumed.filter(|_| !ty.is_ref) {
                let tag = tail_span(span, field);
                return self.variant_value(ty.id, field, None, span, tag, Some(&assumed));
            }
        }
        if self.record_static_member(receiver, span, field).is_err() {
            return HirExpr::poison(span);
        }
        if let Some(constant) = self.member_constant(receiver, field, span) {
            return constant;
        }
        if let Some(function) = self.member_function(receiver, field, span, naming) {
            return function;
        }
        let ty = match receiver {
            Receiver::Type(ty) => ty,
            Receiver::Bare(def) => match self.infer_bare(def, expected, span) {
                Ok(ty) => ty,
                Err(_) => return HirExpr::poison(span),
            },
        };
        if !ty.is_error() && self.variants_of(ty.id).is_none() {
            return self.missing_constant(ty, field, span);
        }
        self.variant_value(ty.id, field, None, span, tail_span(span, field), None)
    }

    /// `Type.NAME` when `NAME` is a constant declared in the type's body
    /// (`Int.MAX`): the constant, whatever the type's arguments (one
    /// constant serves every instance). `None` when the type has no such
    /// constant.
    fn member_constant(&mut self, receiver: Receiver, field: &str, span: Span) -> Option<HirExpr> {
        let owner = match receiver {
            Receiver::Type(ty) if !ty.is_ref => self.type_parts(ty.id)?.0,
            Receiver::Type(_) => return None,
            Receiver::Bare(def) => def,
        };
        let member = self.res.member(owner, field)?;
        if !matches!(self.res.def(member).kind, DefKind::Const { .. }) {
            return None;
        }
        if self.private_denied(member, owner, span) {
            return Some(HirExpr::poison(span));
        }
        self.record_use(tail_span(span, field), member);
        Some(self.const_value(member, span))
    }

    /// `Type.f` when `f` is a function of the type: its address
    /// ([`Self::func_value`]), at the type's arguments; a method's receiver
    /// is the value's first parameter. `None` when the type has no such
    /// function.
    fn member_function(
        &mut self,
        receiver: Receiver,
        field: &str,
        span: Span,
        naming: Naming,
    ) -> Option<HirExpr> {
        let (owner, type_args) = match receiver {
            Receiver::Type(ty) if !ty.is_ref => self.type_parts(ty.id)?,
            Receiver::Type(_) => return None,
            Receiver::Bare(def) => (def, Vec::new()),
        };
        let member = self.res.member(owner, field)?;
        if !matches!(self.res.def(member).kind, DefKind::Func(_)) {
            return None;
        }
        if self.private_denied(member, owner, span) {
            return Some(HirExpr::poison(span));
        }
        self.record_use(tail_span(span, field), member);
        if let Receiver::Bare(def) = receiver {
            // `Box.make` names no instance of `Box`.
            let name = self.res.def(def).name.clone();
            let count = self.res.def(def).generic_params.len();
            self.generic_arity(span, &name, count, 0);
            return Some(HirExpr::poison(span));
        }
        Some(self.func_value(member, type_args, field, span, naming))
    }

    /// `E021` for `Type.NAME` on a type with no variants (an object, a
    /// builtin, a generic parameter) and no constant `NAME`.
    fn missing_constant(&mut self, ty: TypeRef, field: &str, span: Span) -> HirExpr {
        let owner = self.show(ty);
        let mut diagnostic = Diagnostic::error(
            codes::E021,
            format!("no constant `{field}` on `{owner}`"),
        )
        .label(span, true, "no such constant");
        let owner_def = self.type_parts(ty.id).map(|(def, _)| def);
        if let Some(def) = owner_def {
            let names = self.member_names(def, |kind| matches!(kind, DefKind::Const { .. }));
            diagnostic = crate::suggest::suggest(
                diagnostic,
                field,
                tail_span(span, field),
                names.iter().map(String::as_str),
            );
            // A field or method of that name is reached another way.
            diagnostic = match self.res.member(def, field).map(|id| &self.res.def(id).kind) {
                Some(DefKind::Field(_)) => diagnostic.help(format!(
                    "`{field}` is a field: read it from a value of `{owner}`"
                )),
                Some(DefKind::Func(_)) => diagnostic.help(format!(
                    "`{field}` is a function: call it, `{owner}.{field}(...)`"
                )),
                _ => diagnostic,
            };
        }
        let involved = if owner_def.is_some() {
            &[][..]
        } else {
            &[ty.id]
        };
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
    /// Checks a bare tag `.name` at `span` against the `expected` enum.
    fn check_tag(
        &mut self,
        fcx: &FnCx,
        name: &str,
        span: Span,
        expected: Option<TypeId>,
    ) -> HirExpr {
        self.record_tag(span, expected, &[]);
        if name.is_empty() {
            // The parser reported the `.` with no name.
            self.empty_names += 1;
            return HirExpr::poison(span);
        }
        let tag = tail_span(span, name);
        let Some(want) = expected.filter(|id| *id != TypeId::ERROR) else {
            if expected.is_none() {
                self.diags.emit(
                    Diagnostic::error(codes::E024, "tag literal needs an expected either type")
                        .label(span, true, "annotate the binding"),
                );
            }
            return HirExpr::poison(span);
        };
        let assumed = fcx.assumption(want, name, Intrinsic::HasVariant);
        self.variant_value(want, name, None, span, tag, assumed.as_ref())
    }

    /// Variant `name` of either `ty`, its `payload` checked: the stand-in
    /// a `@has_variant` states when one is `assumed`.
    pub(crate) fn variant_value(
        &mut self,
        ty: TypeId,
        name: &str,
        payload: Option<(&mut FnCx, &[Expr])>,
        span: Span,
        tag_span: Span,
        assumed: Option<&Assumption>,
    ) -> HirExpr {
        let found = match assumed {
            Some(assumed) => Ok((None, assumed.ty)),
            None => self
                .variant(ty, name, tag_span)
                .map(|(index, payload)| (Some(index), payload)),
        };
        let Ok((index, payload_ty)) = found else {
            return HirExpr::poison(span);
        };
        self.record_semantic(tag_span, SemanticKind::Variant);
        if let Some((def, _)) = self.type_parts(ty).filter(|_| index.is_some()) {
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
        let kind = match index {
            Some(index) => HirExprKind::Variant {
                index,
                payload: value,
            },
            None => HirExprKind::AssumedVariant {
                name: name.to_owned(),
                payload: value,
            },
        };
        HirExpr {
            ty: TypeRef::value(ty),
            span,
            kind,
        }
    }

    // ---- references and casts ---------------------------------------------

    /// `ref operand`: a place's address, or a function's (`ref f`, `ref
    /// A.B.f`, `ref Type.f`), which is already a `ref function (…)`.
    fn check_ref_of(&mut self, fcx: &mut FnCx, value: &Expr, span: Span) -> HirExpr {
        let Ok(place) = self.check_place_for_ref(fcx, value) else {
            return HirExpr::poison(span);
        };
        if place.ty.is_error() {
            return place;
        }
        if let HirExprKind::FuncRef(_) = place.kind {
            return HirExpr { span, ..place };
        }
        if place.ty.is_ref {
            let mut diagnostic = Diagnostic::error(codes::E010, "cannot take `ref` of a reference")
                .label(
                    span,
                    true,
                    "double references are invalid unless wrapped inside a structure",
                );
            if self.types.is_func(place.ty.id) {
                diagnostic = diagnostic.help(match value {
                    Expr::Ident(name, _) => {
                        format!("`{name}` is already a function reference: use `{name}`")
                    }
                    _ => "this is already a function reference: use it without `ref`".to_owned(),
                });
            }
            self.diags.emit(diagnostic);
            return HirExpr::poison(span);
        }
        HirExpr {
            ty: TypeRef::reference(place.ty.id),
            span,
            kind: HirExprKind::RefOf(Box::new(place)),
        }
    }

    /// What `ref value` takes the address of: a local, global or field
    /// chain, or a function named by a name, path or `Type.f`.
    fn check_place_for_ref(
        &mut self,
        fcx: &mut FnCx,
        value: &Expr,
    ) -> Result<HirExpr, ErrorGuaranteed> {
        match value {
            Expr::Ident(name, span) if fcx.lookup(name).is_none() => {
                let item = self.check_name(fcx, name, *span, Naming::PLACE);
                self.addressable_item(item, name)
            }
            Expr::Member { obj, field, span } => {
                if let Some((path, path_span)) = self.qualified_name(fcx, value) {
                    let item = self.check_name(fcx, &path, path_span, Naming::PLACE);
                    return self.addressable_item(item, &path);
                }
                if let Some(receiver) = self.static_receiver(fcx, obj) {
                    let item = match receiver {
                        Ok(receiver) => {
                            self.static_member(fcx, receiver, field, *span, None, Naming::PLACE)
                        }
                        Err(_) => HirExpr::poison(*span),
                    };
                    return self.addressable_item(item, field);
                }
                let base = self.check_place(fcx, obj)?;
                if base.ty.is_error() {
                    return Ok(HirExpr::poison(*span));
                }
                self.no_bound_method(&base, field, *span)?;
                let shadowed = self.shadowed_alias(fcx, &base);
                Ok(self.field_access(fcx, base, field, *span, None, false, shadowed))
            }
            Expr::Ident(..) => self.check_place(fcx, value),
            Expr::TypeApp(ty) => Ok(self.check_type_app(fcx, ty, true)),
            Expr::Instance {
                callee,
                generic_args,
                span,
            } => Ok(self.check_instance_value(fcx, callee, generic_args, *span, true)),
            _ => Err(self.diags.emit(
                Diagnostic::error(codes::E010, "cannot take `ref` of this expression").label(
                    value.span(),
                    true,
                    "only variables, globals, fields and functions have addresses",
                ),
            )),
        }
    }

    /// `Name[args]` as a value: a generic function's instance (`ref
    /// id[@bit32]`, [`Self::check_instance`]); a type is no value (`E021`).
    fn check_type_app(&mut self, fcx: &mut FnCx, ty: &AstType, behind_ref: bool) -> HirExpr {
        let TypeShape::Named { name, args } = &ty.shape else {
            // A function type (the parser reads one as an operand, for the
            // reflection tests) is no value.
            self.diags.emit(
                Diagnostic::error(codes::E010, "a function type is not a value")
                    .label(ty.span, true, "a type here")
                    .help("name a function instead: `ref f`"),
            );
            return HirExpr::poison(ty.span);
        };
        let callee = path_expr(name, ty.name_span);
        self.check_instance_value(fcx, &callee, args, ty.span, behind_ref)
    }

    /// `callee[generic_args]`, the instance of a generic function they name
    /// (`ref id[@bit32]`, `ref Box[@bit8].convert[@bit32]`), as a value:
    /// its address ([`Self::func_value`], `E077` unless `behind_ref`).
    /// Nothing else takes generic arguments without a call (`E044`).
    fn check_instance_value(
        &mut self,
        fcx: &mut FnCx,
        callee: &Expr,
        generic_args: &[AstType],
        span: Span,
        behind_ref: bool,
    ) -> HirExpr {
        let naming = Naming {
            behind_ref,
            generic_args,
        };
        let (item, name) = match callee {
            Expr::Ident(name, at) => (self.check_name(fcx, name, *at, naming), name.clone()),
            Expr::Member {
                obj,
                field,
                span: at,
            } => {
                if let Some((path, path_span)) = self.qualified_name(fcx, callee) {
                    (self.check_name(fcx, &path, path_span, naming), path)
                } else if let Some(receiver) = self.static_receiver(fcx, obj) {
                    let item = match receiver {
                        Ok(receiver) => self.static_member(fcx, receiver, field, *at, None, naming),
                        Err(_) => HirExpr::poison(*at),
                    };
                    (item, field.clone())
                } else {
                    // `value.f[T]`: a field, or a method, which is no value.
                    let base = self.check_expr(fcx, obj, None);
                    if base.ty.is_error() || self.no_bound_method(&base, field, *at).is_err() {
                        return HirExpr::poison(span);
                    }
                    let shadowed = self.shadowed_alias(fcx, &base);
                    let item = self.field_access(fcx, base, field, *at, None, false, shadowed);
                    (item, field.clone())
                }
            }
            _ => {
                let _ = self.check_expr(fcx, callee, None);
                self.diags.emit(
                    Diagnostic::error(
                        codes::E044,
                        "generic arguments without a call need a generic function's name",
                    )
                    .label(span, true, "not a function's name")
                    .help("a function value has no generic arguments: call it without them"),
                );
                return HirExpr::poison(span);
            }
        };
        match item.kind {
            HirExprKind::FuncRef(_) => HirExpr { span, ..item },
            // Checking the name reported it.
            HirExprKind::Poison => HirExpr::poison(span),
            _ => {
                if !item.ty.is_error() {
                    self.generic_arity(span, &name, 0, generic_args.len());
                }
                HirExpr::poison(span)
            }
        }
    }

    /// An item named under `ref`: a global or a function has an address.
    fn addressable_item(&mut self, item: HirExpr, name: &str) -> Result<HirExpr, ErrorGuaranteed> {
        match item.kind {
            HirExprKind::Global(_) | HirExprKind::FuncRef(_) => Ok(item),
            // `check_name` reported why the name is no value.
            HirExprKind::Poison => Err(self.diags.already_reported()),
            _ => Err(self.diags.emit(
                Diagnostic::error(codes::E010, format!("cannot take `ref` of `{name}`")).label(
                    item.span,
                    true,
                    "only variables, globals, fields and functions have addresses",
                ),
            )),
        }
    }

    /// `E010` for `ref value.method`: there are no bound methods, so a
    /// method's address is taken from its type, `ref Type.method`.
    fn no_bound_method(
        &mut self,
        base: &HirExpr,
        field: &str,
        span: Span,
    ) -> Result<(), ErrorGuaranteed> {
        let ty = self.auto_deref(base.clone()).ty;
        if ty.is_ref {
            return Ok(());
        }
        let Some((owner, _)) = self.type_parts(ty.id) else {
            return Ok(());
        };
        let is_field = self
            .fields_of(ty.id)
            .is_some_and(|fields| fields.iter().any(|(name, _)| name == field));
        let is_function = self
            .res
            .member(owner, field)
            .is_some_and(|member| matches!(self.res.def(member).kind, DefKind::Func(_)));
        if is_field || !is_function {
            return Ok(());
        }
        let shown = self.show_id(ty.id);
        Err(self.diags.emit(
            Diagnostic::error(
                codes::E010,
                format!("cannot take `ref` of method `{field}` of a value"),
            )
            .label(span, true, "a function value holds no receiver")
            .help(format!(
                "there are no bound methods: `ref {shown}.{field}` is the method, taking the receiver as its first parameter"
            )),
        ))
    }

    /// `operand as T`: one conversion (or a literal pinned to `T`). A cast
    /// between `ref unknown` and any `ref T` (a function reference too) is
    /// a pure retype; any other cast between two `ref` types is `E010`.
    fn check_cast(&mut self, fcx: &mut FnCx, operand: &Expr, target_ty: &AstType) -> HirExpr {
        let target = self.resolve_type(target_ty);
        let span = operand.span();
        if target.is_error() {
            let _ = self.check_expr(fcx, operand, Some(TypeId::ERROR));
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
            if value.ty.is_ref && self.types.is_func(value.ty.id) && self.types.is_func(target.id) {
                let (from, to) = (self.show(value.ty), self.show(target));
                self.emit_involving(
                    Diagnostic::error(
                        codes::E010,
                        format!("cannot cast `{from}` to `{to}`"),
                    )
                    .label(span, true, format!("a `{from}`"))
                    .label(target_ty.span, false, "a different function type")
                    .help("a function reference casts only to and from `ref unknown`; a cast through it is sound only when every slot has the same representation"),
                    &[value.ty.id, target.id],
                );
                return HirExpr::poison(span);
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
        // A method call can replace `as T` only after a postfix operand.
        let cast = matches!(
            operand,
            Expr::Ident(..) | Expr::Call { .. } | Expr::Member { .. }
        )
        .then(|| Span::new(span.hi.0, target_ty.span.hi.0, span.file));
        self.coerce_or_fix(value, target, target_ty.span, cast)
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
                return self.check_logic(fcx, lhs, op == BinOp::And, rhs, span).0;
            }
            BinOp::EqEq => return self.check_equality(fcx, lhs, rhs, span, "=="),
            BinOp::NotEq => {
                let eq = self.check_equality(fcx, lhs, rhs, span, "!=");
                if eq.ty.is_error() {
                    return eq;
                }
                return HirExpr {
                    ty: Builtin::Bit8.value(),
                    span,
                    kind: HirExprKind::Not(Box::new(eq)),
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
        let (left, right) = (self.auto_deref(left), self.auto_deref(right));
        self.operator_call(op, left, right, span, None)
    }

    /// `left op right` through the operator's hook, found by exact
    /// signature (`E043` at `label` when there is none). `compound` is the
    /// compound assignment's operator span, which the error labels instead.
    pub(crate) fn operator_call(
        &mut self,
        op: BinOp,
        left: HirExpr,
        right: HirExpr,
        span: Span,
        compound: Option<Span>,
    ) -> HirExpr {
        let hook = hook_name(op);
        let params = [left.ty, right.ty];
        let found = self.find_hook(hook, span, |sig| {
            sig.params == params && sig.returns.len() == 1
        });
        let Some((callee, sig)) = found else {
            let mut diagnostic = Diagnostic::error(
                codes::E043,
                format!(
                    "no {hook} for ({}, {})",
                    self.show_id(left.ty.id),
                    self.show_id(right.ty.id)
                ),
            )
            .label(compound.unwrap_or(span), true, "no exact operator overload");
            if compound.is_some() {
                let symbol = op_symbol(op);
                diagnostic = diagnostic.note(format!("`x {symbol}= e` is `x = x {symbol} e`"));
            }
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
            ty: sig.returns[0],
            span,
            kind: HirExprKind::Call {
                callee,
                args: vec![left, right],
                rest: Vec::new(),
            },
        }
    }

    /// `lhs == rhs` (or `!=`, named by `what`): strict bit-equality of two
    /// values of one type, never a hook.
    fn check_equality(
        &mut self,
        fcx: &mut FnCx,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
        what: &str,
    ) -> HirExpr {
        let (left, right) = self.check_sides(fcx, lhs, rhs);
        if left.ty.is_error() || right.ty.is_error() {
            return HirExpr::poison(span);
        }
        let (left, right) = (self.auto_deref(left), self.auto_deref(right));
        if left.ty.id != right.ty.id {
            let (l, r) = (self.show_id(left.ty.id), self.show_id(right.ty.id));
            self.diags.emit(
                Diagnostic::error(
                    codes::E010,
                    format!("mismatched types: `{what}` needs identical types, found {l} and {r}"),
                )
                .label(left.span, false, format!("left side `{l}` here"))
                .label(right.span, true, format!("right side `{r}` here")),
            );
            return HirExpr::poison(span);
        }
        HirExpr {
            ty: Builtin::Bit8.value(),
            span,
            kind: HirExprKind::Eq(Box::new(left), Box::new(right)),
        }
    }

    /// Checks both operands once: non-literal sides first, so a literal side
    /// takes the other side's type (an error type too, so a side that
    /// failed leaves the literal silent).
    pub(crate) fn check_sides(
        &mut self,
        fcx: &mut FnCx,
        lhs: &Expr,
        rhs: &Expr,
    ) -> (HirExpr, HirExpr) {
        let (left_lit, right_lit) = (needs_context(lhs), needs_context(rhs));
        if left_lit && right_lit {
            let left = self.check_expr(fcx, lhs, None);
            let right = self.check_expr(fcx, rhs, None);
            return (left, right);
        }
        let left = (!left_lit).then(|| self.check_expr(fcx, lhs, None));
        let right = (!right_lit).then(|| self.check_expr(fcx, rhs, None));
        let anchor = |side: &Option<HirExpr>| side.as_ref().map(|e| e.ty.id);
        let left = if let Some(left) = left {
            left
        } else {
            let want = anchor(&right);
            self.check_expr(fcx, lhs, want)
        };
        let right = if let Some(right) = right {
            right
        } else {
            self.check_expr(fcx, rhs, Some(left.ty.id))
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
        let (cond, facts) = self.check_condition(fcx, cond, "if");
        let mark = self.assume(fcx, &facts.when_true);
        let then = self.check_expr(fcx, then, expected);
        self.unassume(fcx, mark);
        // A `then` that failed leaves the error type: the `else` stays silent.
        let els_expected = expected.or(Some(self.types.read_type(then.ty).id));
        let mark = self.assume(fcx, &facts.when_false);
        let els = self.check_expr(fcx, els, els_expected);
        self.unassume(fcx, mark);
        let result = match expected {
            Some(want) => self.types.value_type(want),
            None if !then.ty.is_error() => self.types.read_type(then.ty),
            None if !els.ty.is_error() => self.types.read_type(els.ty),
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
                        self.show_id(then.ty.id),
                        self.show_id(els.ty.id)
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
            let (pattern, capture, by_ref) = self.check_pattern(fcx, arm, arms, &scrutinee);
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
                first = Some(self.types.read_type(value.ty));
            }
            checked.push((pattern, capture, by_ref, value, arm.span));
        }
        let els = self.check_expr(fcx, els, expected.or(first.map(|t| t.id)));
        let result = match (expected, first) {
            (Some(want), _) => self.types.value_type(want),
            (None, Some(first)) => first,
            (None, None) if !els.ty.is_error() => self.types.read_type(els.ty),
            _ => return HirExpr::poison(span),
        };
        if expected.is_none() && !failed {
            let disagree = checked
                .iter()
                .map(|(_, _, _, value, _)| value.ty)
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
            .map(|(pattern, capture, by_ref, value, arm_span)| HirValueArm {
                pattern,
                capture,
                value: self.coerce(value, result, arm_span),
                moves: false,
                by_ref,
                drops: Vec::new(),
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
                return self.struct_lit_failed(fcx, fields, span);
            };
            match found {
                // An alias names its type whole (a generic one with its
                // arguments), so there is nothing to infer.
                Some(def) if matches!(self.res.def(def).kind, DefKind::Alias(_)) => {
                    let tail = path_tail(ty.name_span, name);
                    self.record_use(tail, def);
                    self.record_semantic(tail, SemanticKind::UserType);
                    let target = self.alias_type(def, ty.name_span);
                    if target.is_error() || self.ensure_type(target, span).is_err() {
                        return self.struct_lit_failed(fcx, fields, span);
                    }
                    target
                }
                Some(def) if self.res.def(def).is_type() => {
                    let tail = path_tail(ty.name_span, name);
                    self.record_use(tail, def);
                    self.record_semantic(tail, SemanticKind::UserType);
                    if self.res.def(def).generic_params.is_empty() {
                        let object = self.type_of_def(def, Vec::new());
                        if self.ensure_type(object, span).is_err() {
                            return self.struct_lit_failed(fcx, fields, span);
                        }
                        object
                    } else {
                        let inferred = expected.filter(|id| {
                            matches!(self.types.kind(*id), TypeKind::Instance { base, .. } if *base == def)
                        });
                        let Some(id) = inferred else {
                            // An expected type that failed reported it.
                            if expected != Some(TypeId::ERROR) {
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
                            }
                            return self.struct_lit_failed(fcx, fields, span);
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
                    return self.struct_lit_failed(fcx, fields, span);
                }
            }
        } else {
            let resolved = self.resolve_type(ty);
            if resolved.is_error() {
                return self.struct_lit_failed(fcx, fields, span);
            }
            resolved.deref()
        };
        if matches!(self.types.kind(object.id), TypeKind::Array { .. }) {
            return self.array_literal(object, fields, span);
        }
        let display = self.show_id(object.id);
        let Some(layout) = self.fields_of(object.id) else {
            self.diags.emit(
                Diagnostic::error(codes::E010, format!("`{display}` is not a structure")).label(
                    span,
                    true,
                    "only structures construct with `{ ... }`",
                ),
            );
            return self.struct_lit_failed(fcx, fields, span);
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

    /// A struct literal whose type failed (reported): still checks each
    /// field's value, against the error type so a tag or literal does not
    /// ask for one, so errors inside the values are reported too.
    fn struct_lit_failed(&mut self, fcx: &mut FnCx, fields: &[StructField], span: Span) -> HirExpr {
        for field in fields {
            let _ = self.check_expr(fcx, &field.value, Some(TypeId::ERROR));
        }
        HirExpr::poison(span)
    }

    // ---- module constants ---------------------------------------------------

    /// A use of constant `def`: its value is inlined where it is used.
    pub(crate) fn const_value(&mut self, def: DefId, span: Span) -> HirExpr {
        self.use_builtin_const(def);
        let ty = self.const_type(def);
        HirExpr {
            ty,
            span,
            kind: HirExprKind::Const(def),
        }
    }

    /// Where a constant or global is checked: its file, and for a member
    /// constant its type, whose private members it reaches. A type's
    /// generic parameters are not in scope: one constant serves every
    /// instance.
    fn const_env(&self, def: DefId) -> Env {
        Env {
            file: Some(self.res.def(def).file),
            owner: self.res.def(def).parent,
            ..Env::default()
        }
    }

    /// The declared type of a constant or global.
    pub(crate) fn const_type(&mut self, def: DefId) -> TypeRef {
        if let Some(ty) = self.const_types.get(&def) {
            return *ty;
        }
        let (DefKind::Const { ty, .. } | DefKind::Global { ty, .. }) = &self.res.def(def).kind
        else {
            return TypeRef::ERROR;
        };
        let ty = ty.clone();
        let env = self.const_env(def);
        let mut resolved = self.with_env(env, |c| c.resolve_type(&ty));
        let what = if matches!(self.res.def(def).kind, DefKind::Global { .. }) {
            "a global"
        } else {
            "a constant"
        };
        if self.format_place(resolved, ty.span, what).is_err() {
            resolved = TypeRef::ERROR;
        }
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
        for def in self.res.ids() {
            if matches!(self.res.def(def).kind, DefKind::Global { .. }) {
                self.check_global(def);
            }
        }
    }

    /// Checks one global's type and initializer into the output. Lowering
    /// requires the initializer to be constant.
    fn check_global(&mut self, def: DefId) {
        let DefKind::Global { value, .. } = &self.res.def(def).kind else {
            return;
        };
        let value = value.clone();
        let ty = self.const_type(def);
        let span = self.res.def(def).span;
        let env = self.const_env(def);
        if ty.is_error() {
            // The type reported it. The initializer is still checked, against
            // the error type, so errors inside it are reported too; the
            // global stays out of the output, as nothing can lower it.
            if let Some(value) = value {
                self.with_env(env, |c| {
                    let mut fcx = FnCx::new(Span::dummy());
                    let _ = c.check_expr(&mut fcx, &value, Some(TypeId::ERROR));
                });
            }
            return;
        }
        let value = value.map(|value| {
            self.with_env(env, |c| {
                let mut fcx = FnCx::new(Span::dummy());
                let hir = c.check_expr(&mut fcx, &value, Some(ty.id));
                c.coerce(hir, ty, value.span())
            })
        });
        self.out
            .globals
            .insert(def, lilac_hir::HirGlobal { ty, value, span });
    }

    /// Checks one constant's initializer into the output.
    fn check_const(&mut self, def: DefId) {
        let DefKind::Const { value, .. } = &self.res.def(def).kind else {
            return;
        };
        let value = value.clone();
        let ty = self.const_type(def);
        let env = self.const_env(def);
        let hir = self.with_env(env, |c| {
            let mut fcx = FnCx::new(Span::dummy());
            let hir = c.check_expr(&mut fcx, &value, Some(ty.id));
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

/// Whether `expr` takes its type from the other side of a binary operator:
/// a number literal (or arithmetic over them) or a tag (`.None`,
/// `.Some(x)`).
fn needs_context(expr: &Expr) -> bool {
    match expr {
        Expr::Tag(..) => true,
        Expr::Call { callee, .. } => matches!(**callee, Expr::Tag(..)),
        _ => is_foldable_literal(expr),
    }
}

/// The name token of a struct-literal field (fields begin at their name).
fn leading_field(field: &StructField) -> Span {
    crate::sigs::leading(field.span, &field.name)
}

/// How a compound assignment's operator is written (`+` for `+=`).
fn op_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        _ => "%",
    }
}
