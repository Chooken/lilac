//! Calls: one resolution path for every callee shape.
//!
//! `f(...)`, `Type.f(...)`, `value.f(...)`, `name: Type.f(...)`,
//! `@hook(...)`, `.Variant(...)` and intrinsics all end in either a call of a
//! [`FuncKey`] (generic arguments resolved, instance requested) or a value
//! (variant construction, folded literal, `@compile_error`).

use lilac_ast::{Expr, Type as AstType};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_hir::intrinsics::{Intrinsic, Slot};
use lilac_hir::{FuncKey, HirCallee, HirExpr, HirExprKind, MemberReceiver, SemanticKind};
use lilac_resolve::{DefId, DefKind};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeId, TypeKind, TypeRef};

use crate::body::{local_expr, FnCx};
use crate::expr::{tail_span, Receiver};
use crate::literals::is_foldable_literal;
use crate::paths::shadowed_alias_note;
use crate::{Checker, Hook, Requirement};

/// A checked call.
pub(crate) enum Called {
    /// A call with its out-slot types.
    Call {
        /// Target.
        callee: HirCallee,
        /// Checked arguments.
        args: Vec<HirExpr>,
        /// Out-slot types (empty for `nothing`).
        outs: Vec<TypeRef>,
    },
    /// The call shape produced a plain value.
    Value(HirExpr),
}

impl Checker<'_> {
    /// `.Variant(payload)`: the expected enum's variant.
    fn tag_call(
        &mut self,
        fcx: &mut FnCx,
        name: &str,
        tag_span: Span,
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<Called, ErrorGuaranteed> {
        self.record_tag(tag_span, expected, &[]);
        if name.is_empty() {
            // The parser reported the `.` with no name.
            self.empty_names += 1;
            return Err(self.callee_failed(fcx, args, self.diags.already_reported()));
        }
        let Some(want) = expected else {
            let error =
                self.diags.emit(
                    Diagnostic::error(codes::E024, "tag construction needs an expected type")
                        .label(tag_span, true, "annotate the binding"),
                );
            return Err(self.callee_failed(fcx, args, error));
        };
        if want == TypeId::ERROR {
            // The expected type reported it.
            return Err(self.callee_failed(fcx, args, self.diags.already_reported()));
        }
        let tag = tail_span(tag_span, name);
        Ok(Called::Value(self.variant_value(
            want,
            name,
            Some((fcx, args)),
            span,
            tag,
        )))
    }

    pub(crate) fn check_call(
        &mut self,
        fcx: &mut FnCx,
        callee: &Expr,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<Called, ErrorGuaranteed> {
        match callee {
            Expr::Ident(name, name_span) => {
                self.check_named_call(fcx, name, *name_span, generic_args, args, span, expected)
            }
            Expr::Member {
                obj,
                field,
                span: member_span,
            } => {
                if let Some((name, name_span)) = self.qualified_name(fcx, callee) {
                    return self.check_named_call(
                        fcx,
                        &name,
                        name_span,
                        generic_args,
                        args,
                        span,
                        expected,
                    );
                }
                if let Some(receiver) = self.static_receiver(fcx, obj) {
                    let receiver = self.callee(fcx, args, receiver)?;
                    let recorded = self.record_static_member(receiver, *member_span, field);
                    self.callee(fcx, args, recorded)?;
                    return self.static_call(
                        fcx,
                        receiver,
                        field,
                        generic_args,
                        args,
                        span,
                        *member_span,
                        expected,
                    );
                }
                let receiver = self.check_expr(fcx, obj, None);
                if receiver.ty.is_error() {
                    // The receiver reported it.
                    return Err(self.callee_failed(fcx, args, self.diags.already_reported()));
                }
                self.method_call(
                    fcx,
                    receiver,
                    field,
                    generic_args,
                    args,
                    span,
                    *member_span,
                    expected,
                )
            }
            Expr::Tag(name, tag_span) => self.tag_call(fcx, name, *tag_span, args, span, expected),
            Expr::TypeApp(ty) => {
                let resolved = self.resolve_type(ty);
                if resolved.is_error() {
                    // `resolve_type` reported it.
                    return Err(self.callee_failed(fcx, args, self.diags.already_reported()));
                }
                let name = self.show(resolved);
                Err(self.type_call(fcx, &name, args, span))
            }
            _ => {
                let checked = self.check_expr(fcx, callee, None);
                if !generic_args.is_empty() && !checked.ty.is_error() {
                    let error = self.diags.emit(
                        Diagnostic::error(
                            codes::E044,
                            "a function reference takes no generic arguments",
                        )
                        .label(
                            span,
                            true,
                            "its function is already one instance",
                        ),
                    );
                    return Err(self.callee_failed(fcx, args, error));
                }
                self.value_call(fcx, checked, "this expression", args, span)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn check_named_call(
        &mut self,
        fcx: &mut FnCx,
        name: &str,
        name_span: Span,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<Called, ErrorGuaranteed> {
        if name == "@negate" && generic_args.is_empty() && args.len() == 1 {
            if let Some(folded) = self.fold_negative(&args[0], span, expected) {
                return Ok(Called::Value(folded));
            }
        }
        if let Some(local) = fcx.lookup(name) {
            self.record_local_use(fcx, local, name_span);
            if !generic_args.is_empty() {
                let error = self.generic_arity(span, name, 0, generic_args.len());
                return Err(self.callee_failed(fcx, args, error));
            }
            let callee = local_expr(local, fcx.ty(local), name_span);
            return self.value_call(fcx, callee, &format!("`{name}`"), args, span);
        }
        if name.starts_with('@') && !name.contains('.') {
            if let Some(intrinsic) = Intrinsic::lookup(name) {
                self.record_semantic(name_span, SemanticKind::BuiltinFunc);
                if !generic_args.is_empty() {
                    let error = self.generic_arity(span, name, 0, generic_args.len());
                    return Err(self.callee_failed(fcx, args, error));
                }
                return self.check_intrinsic(fcx, intrinsic, name, args, span, expected);
            }
            let overloads = self.overloads(name);
            if overloads.is_empty() {
                let mut diagnostic = Diagnostic::error(
                    codes::E021,
                    format!("unresolved name `{name}`"),
                )
                .label(name_span, true, "no hook or intrinsic with this name");
                if let Some(note) = Intrinsic::replaced(name) {
                    diagnostic = diagnostic.note(note);
                }
                let error = self.diags.emit(diagnostic);
                return Err(self.callee_failed(fcx, args, error));
            }
            return self.hook_call(fcx, name, &overloads, args, span);
        }
        if let Some(def) = self.own_member(name) {
            return self.own_call(fcx, def, name, name_span, generic_args, args, span);
        }
        let receiver = Expr::Ident(name.to_owned(), name_span);
        if let Some(receiver) = self.static_receiver(fcx, &receiver) {
            self.callee(fcx, args, receiver)?;
            return Err(self.type_call(fcx, name, args, span));
        }
        let found = self.lookup_item(name, name_span);
        let Some(def) = self.callee(fcx, args, found)? else {
            let error = if self.in_entry() {
                self.entry_unresolved(name, name_span)
            } else {
                let diagnostic = Diagnostic::error(
                    codes::E021,
                    format!("unresolved name `{name}`"),
                )
                .label(name_span, true, "not a local, item, or intrinsic");
                let diagnostic = self.own_instance_help(fcx, diagnostic, name);
                let diagnostic = self.suggest_value(fcx, diagnostic, name, name_span);
                self.diags.emit(diagnostic)
            };
            return Err(self.callee_failed(fcx, args, error));
        };
        self.record_use(crate::paths::path_tail(name_span, name), def);
        match self.res.def(def).kind {
            DefKind::Func(_) => {
                return self.func_call(fcx, def, Vec::new(), generic_args, args, span, None);
            }
            // A constant or global holding a function reference.
            DefKind::Const { .. } | DefKind::Global { .. } => {
                if !generic_args.is_empty() {
                    let error = self.generic_arity(span, name, 0, generic_args.len());
                    return Err(self.callee_failed(fcx, args, error));
                }
                let callee = self.item_value(def, name_span);
                return self.value_call(fcx, callee, &format!("`{name}`"), args, span);
            }
            _ => {}
        }
        let error = self.diags.emit(
            Diagnostic::error(codes::E022, "this item is not callable").label(
                span,
                true,
                "only functions can be called",
            ),
        );
        Err(self.callee_failed(fcx, args, error))
    }

    /// A call through `callee`, a function reference (`ref function (…)`),
    /// described as `what` when it is not one (`E022`). No `self` is
    /// passed: the arguments are the function type's parameters. The callee
    /// is evaluated before the arguments.
    pub(crate) fn value_call(
        &mut self,
        fcx: &mut FnCx,
        callee: HirExpr,
        what: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Called, ErrorGuaranteed> {
        if callee.ty.is_error() {
            // Checking the callee reported it.
            return Err(self.callee_failed(fcx, args, self.diags.already_reported()));
        }
        let function = if callee.ty.is_ref {
            match self.types.kind(callee.ty.id) {
                TypeKind::Func {
                    params, returns, ..
                } => Some((params.clone(), returns.clone())),
                _ => None,
            }
        } else {
            None
        };
        let Some((params, returns)) = function else {
            let shown = self.show(callee.ty);
            let error = self.emit_involving(
                Diagnostic::error(
                    codes::E022,
                    format!("{what} is a `{shown}`, not a function"),
                )
                .label(callee.span, true, "not callable")
                .help("only functions and function references (`ref function (…)`) can be called"),
                &[callee.ty.id],
            );
            return Err(self.callee_failed(fcx, args, error));
        };
        let args = self.check_args(fcx, &params, args, span)?;
        Ok(Called::Call {
            callee: HirCallee::Value(Box::new(callee)),
            args,
            outs: returns,
        })
    }

    /// A call whose callee failed (`error` reports it): still checks each
    /// argument, against the error type so a tag or literal does not ask
    /// for one, so errors inside the arguments are reported too.
    pub(crate) fn callee_failed(
        &mut self,
        fcx: &mut FnCx,
        args: &[Expr],
        error: ErrorGuaranteed,
    ) -> ErrorGuaranteed {
        for arg in args {
            let _ = self.check_expr(fcx, arg, Some(TypeId::ERROR));
        }
        error
    }

    /// `resolved`, a step of finding the callee, or
    /// [`Self::callee_failed`] when it failed.
    pub(crate) fn callee<T>(
        &mut self,
        fcx: &mut FnCx,
        args: &[Expr],
        resolved: Result<T, ErrorGuaranteed>,
    ) -> Result<T, ErrorGuaranteed> {
        resolved.map_err(|error| self.callee_failed(fcx, args, error))
    }

    /// Calls function `def` (of a type instantiated at `type_args`, if a
    /// member), with an optional `ref` receiver as the first argument.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn func_call(
        &mut self,
        fcx: &mut FnCx,
        def: DefId,
        type_args: Vec<TypeRef>,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        receiver: Option<HirExpr>,
    ) -> Result<Called, ErrorGuaranteed> {
        let own = self.res.def(def).generic_params.len();
        if generic_args.len() != own {
            let name = self.function_name(def);
            let error = self.generic_arity(span, &name, own, generic_args.len());
            return Err(self.callee_failed(fcx, args, error));
        }
        let mut key_args = type_args;
        for arg in generic_args {
            let arg = self.resolve_type(arg);
            if arg.is_error() {
                // `resolve_type` reported it.
                return Err(self.callee_failed(fcx, args, self.diags.already_reported()));
            }
            key_args.push(arg);
        }
        let key = FuncKey {
            def,
            args: key_args,
        };
        let sig = self.request(key.clone(), span);
        let sig = self.callee(fcx, args, sig)?;
        let skip = usize::from(receiver.is_some());
        if receiver.is_some() && sig.params.is_empty() {
            // `method_call` checked the method takes `self`: only an error
            // type in its signature (reported) gets here.
            return Err(self.callee_failed(fcx, args, self.diags.already_reported()));
        }
        let mut hir_args: Vec<HirExpr> = receiver.into_iter().collect();
        let fixed = &sig.params[skip..];
        let variadic = self.res.def(def).func().is_some_and(|decl| decl.variadic);
        let (args, extra) = if variadic && args.len() > fixed.len() {
            args.split_at(fixed.len())
        } else {
            (args, &[][..])
        };
        hir_args.extend(self.check_args(fcx, fixed, args, span)?);
        let mut failed = false;
        for arg in extra {
            match self.check_variadic_arg(fcx, arg) {
                Ok(value) => hir_args.push(value),
                Err(_) => failed = true,
            }
        }
        if failed {
            // `check_variadic_arg` reported it.
            return Err(self.diags.already_reported());
        }
        let can_fail = self
            .res
            .def(def)
            .func()
            .is_some_and(|decl| decl.failures.is_some());
        if can_fail && fcx.handled != Some(span) {
            let name = self.function_name(def);
            return Err(self.diags.emit(
                Diagnostic::error(
                    codes::E057,
                    format!("`{name}` can fail, and the failure is not handled"),
                )
                .label(span, true, "this call can fail")
                .help(
                    "handle it where it happens: `value: T = call else { return }`, \
                     `if call -> value: T { ... }`, or `loop if call -> value: T { ... }`",
                ),
            ));
        }
        Ok(Called::Call {
            callee: HirCallee::Func(key),
            args: hir_args,
            outs: sig.returns,
        })
    }

    /// `Type.member(...)`: a static function, or a variant with a payload.
    #[allow(clippy::too_many_arguments)]
    fn static_call(
        &mut self,
        fcx: &mut FnCx,
        receiver: Receiver,
        field: &str,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        member_span: Span,
        expected: Option<TypeId>,
    ) -> Result<Called, ErrorGuaranteed> {
        if let Receiver::Type(ty) = receiver {
            let assumed = fcx.assumption(ty.id, field, Intrinsic::HasFunction);
            if let Some(assumed) = assumed.filter(|_| !ty.is_ref) {
                return self.assumed_call(fcx, assumed, None, generic_args.len(), args, span);
            }
        }
        let ty = match receiver {
            Receiver::Type(ty) => ty,
            Receiver::Bare(def) => {
                let is_variant = self
                    .res
                    .member(def, field)
                    .is_some_and(|member| matches!(self.res.def(member).kind, DefKind::Variant(_)));
                if !is_variant {
                    let name = self.res.def(def).name.clone();
                    let count = self.res.def(def).generic_params.len();
                    let error = self.generic_arity(span, &name, count, 0);
                    return Err(self.callee_failed(fcx, args, error));
                }
                let ty = self.infer_bare(def, expected, span);
                self.callee(fcx, args, ty)?
            }
        };
        let owner = self.show(ty);
        let Some((def, type_args)) = self.type_parts(ty.id).filter(|_| !ty.is_ref) else {
            let mut diagnostic = Diagnostic::error(
                codes::E010,
                format!("cannot call static member `{field}` on `{owner}`"),
            )
            .label(span, true, "static calls need a structure type");
            if self.is_generic_derived(ty) {
                let probed = self.probe_args(fcx, args);
                diagnostic =
                    diagnostic.note(self.required_method_note(field, None, &probed, expected));
            }
            let error = self.missing(diagnostic, true, field, owner, None, &[ty.id]);
            return Err(self.callee_failed(fcx, args, error));
        };
        let Some(member) = self.res.member(def, field) else {
            let mut diagnostic = Diagnostic::error(
                codes::E021,
                format!("no member `{field}` on `{owner}`"),
            )
            .label(span, true, "no such member");
            diagnostic =
                self.suggest_member(diagnostic, def, field, tail_span(member_span, field), true);
            if self.is_generic_derived(ty) {
                let probed = self.probe_args(fcx, args);
                diagnostic =
                    diagnostic.note(self.required_method_note(field, None, &probed, expected));
            }
            let error = self.missing(diagnostic, true, field, owner, Some(def), &[]);
            return Err(self.callee_failed(fcx, args, error));
        };
        let member_name = tail_span(member_span, field);
        match &self.res.def(member).kind {
            DefKind::Variant(_) => {
                if !generic_args.is_empty() {
                    let error = self.generic_arity(span, field, 0, generic_args.len());
                    return Err(self.callee_failed(fcx, args, error));
                }
                Ok(Called::Value(self.variant_value(
                    ty.id,
                    field,
                    Some((fcx, args)),
                    span,
                    member_name,
                )))
            }
            DefKind::Func(_) => {
                if self.private_denied(member, def, span) {
                    // `private_denied` reported `E026`.
                    return Err(self.callee_failed(fcx, args, self.diags.already_reported()));
                }
                self.record_use(member_name, member);
                self.func_call(fcx, member, type_args, generic_args, args, span, None)
            }
            _ => {
                let error = self.diags.emit(
                    Diagnostic::error(codes::E022, format!("`{field}` is not callable")).label(
                        span,
                        true,
                        "only functions can be called",
                    ),
                );
                Err(self.callee_failed(fcx, args, error))
            }
        }
    }

    /// `diagnostic` (a method `field` the receiver lacks), with the note
    /// naming the method a generic-derived receiver must have.
    fn method_requirement(
        &mut self,
        fcx: &mut FnCx,
        diagnostic: Diagnostic,
        receiver: &HirExpr,
        field: &str,
        args: &[Expr],
        expected: Option<TypeId>,
    ) -> Diagnostic {
        if !self.is_generic_derived(receiver.ty.deref()) {
            return diagnostic;
        }
        let probed = self.probe_args(fcx, args);
        diagnostic.note(self.required_method_note(field, Some(receiver.ty), &probed, expected))
    }

    /// `value.method(...)`: the receiver is passed as `ref Self`.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn method_call(
        &mut self,
        fcx: &mut FnCx,
        receiver: HirExpr,
        field: &str,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        member_span: Span,
        expected: Option<TypeId>,
    ) -> Result<Called, ErrorGuaranteed> {
        let base = receiver.ty.deref();
        let assumed = fcx
            .assumption(base.id, field, Intrinsic::HasMethod)
            .or_else(|| fcx.assumption(base.id, field, Intrinsic::HasField));
        if let Some(assumed) = assumed {
            let receiver = Some(receiver);
            return self.assumed_call(fcx, assumed, receiver, generic_args.len(), args, span);
        }
        let recorded = self.record_member(member_span, field, Some(MemberReceiver::Value(base)));
        self.callee(fcx, args, recorded)?;
        let owner = self.show_id(base.id);
        let shadowed = self.shadowed_alias(fcx, &receiver);
        let Some((def, type_args)) = self.type_parts(base.id) else {
            let diagnostic = Diagnostic::error(
                codes::E010,
                format!("cannot call method `{field}` on `{owner}`"),
            )
            .label(span, true, "methods need a structure");
            let diagnostic = shadowed_alias_note(diagnostic, shadowed);
            let diagnostic =
                self.method_requirement(fcx, diagnostic, &receiver, field, args, expected);
            let error = self.missing(diagnostic, true, field, owner, None, &[base.id]);
            return Err(self.callee_failed(fcx, args, error));
        };
        let Some(member) = self.res.member(def, field) else {
            let mut diagnostic = Diagnostic::error(
                codes::E021,
                format!("no member `{field}` on `{owner}`"),
            )
            .label(member_span, true, "no such member");
            diagnostic =
                self.suggest_member(diagnostic, def, field, tail_span(member_span, field), true);
            diagnostic = shadowed_alias_note(diagnostic, shadowed);
            diagnostic = self.method_requirement(fcx, diagnostic, &receiver, field, args, expected);
            let error = self.missing(diagnostic, true, field, owner, Some(def), &[]);
            return Err(self.callee_failed(fcx, args, error));
        };
        if let DefKind::Field(_) = self.res.def(member).kind {
            return self.field_call(
                fcx,
                receiver,
                (def, member),
                field,
                generic_args,
                args,
                span,
                member_span,
            );
        }
        if !matches!(self.res.def(member).kind, DefKind::Func(_)) {
            let error = self.diags.emit(
                Diagnostic::error(codes::E022, format!("`{field}` is not callable")).label(
                    member_span,
                    true,
                    "only functions can be called",
                ),
            );
            return Err(self.callee_failed(fcx, args, error));
        }
        if self.private_denied(member, def, member_span) {
            // `private_denied` reported `E026`.
            return Err(self.callee_failed(fcx, args, self.diags.already_reported()));
        }
        self.record_use(tail_span(member_span, field), member);
        let template = self.sig_of(member);
        let template = self.callee(fcx, args, template)?;
        let of_owner = |c: &Self, ty: &TypeRef| c.type_parts(ty.id).is_some_and(|(d, _)| d == def);
        let first = template.params.first();
        let takes_ref = first.is_some_and(|first| first.is_ref && of_owner(self, first));
        // `self: T` takes the receiver by value: it moves (or copies) in.
        let takes_value = !takes_ref
            && first.is_some_and(|first| !first.is_ref && of_owner(self, first))
            && self
                .res
                .def(member)
                .func()
                .and_then(|decl| decl.params.first())
                .is_some_and(|param| param.name == "self");
        if !takes_ref && !takes_value {
            let error =
                self.diags.emit(
                    Diagnostic::error(codes::E022, format!("method `{field}` takes no `self`"))
                        .label(member_span, true, "first parameter must be `self`"),
                );
            return Err(self.callee_failed(fcx, args, error));
        }
        let receiver = if takes_value {
            self.auto_deref(receiver)
        } else if receiver.ty.is_ref {
            receiver
        } else {
            HirExpr {
                ty: TypeRef::reference(base.id),
                span: receiver.span,
                kind: HirExprKind::RefOf(Box::new(receiver)),
            }
        };
        self.func_call(
            fcx,
            member,
            type_args,
            generic_args,
            args,
            span,
            Some(receiver),
        )
    }

    /// `value.field(...)` where `field` (the member `owner.member`) is a
    /// field: a call through the function reference it holds, which passes
    /// no `self`. The field is read before the arguments are evaluated.
    #[allow(clippy::too_many_arguments)]
    fn field_call(
        &mut self,
        fcx: &mut FnCx,
        receiver: HirExpr,
        (owner, member): (DefId, DefId),
        field: &str,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        member_span: Span,
    ) -> Result<Called, ErrorGuaranteed> {
        if self.private_denied(member, owner, member_span) {
            // `private_denied` reported `E026`.
            return Err(self.callee_failed(fcx, args, self.diags.already_reported()));
        }
        self.record_use(tail_span(member_span, field), member);
        if !generic_args.is_empty() {
            let error = self.generic_arity(span, field, 0, generic_args.len());
            return Err(self.callee_failed(fcx, args, error));
        }
        let base = self.auto_deref(receiver);
        let fields = self.fields_of(base.ty.id).unwrap_or_default();
        let Some(index) = fields.iter().position(|(name, _)| name == field) else {
            lilac_error::bug!(member_span, "`{field}` is a member but no field");
        };
        let callee = HirExpr {
            ty: fields[index].1,
            span: member_span,
            kind: HirExprKind::Field {
                base: Box::new(base),
                index: u32::try_from(index).unwrap_or(u32::MAX),
            },
        };
        self.value_call(fcx, callee, &format!("field `{field}`"), args, span)
    }

    /// Emits a missing-member diagnostic and records the requirement.
    /// `involved` is the receiver type when it has no members of its own
    /// (a type's member set does not depend on its arguments).
    fn missing(
        &mut self,
        diagnostic: Diagnostic,
        method: bool,
        name: &str,
        owner: String,
        owner_def: Option<DefId>,
        involved: &[TypeId],
    ) -> ErrorGuaranteed {
        self.emit_requirement(
            diagnostic,
            Requirement::Missing {
                method,
                name: name.to_owned(),
                owner,
                owner_def,
            },
            involved,
        )
    }

    /// Argument types for a required-signature note (diagnostics discarded).
    fn probe_args(&mut self, fcx: &mut FnCx, args: &[Expr]) -> Vec<Option<TypeRef>> {
        args.iter().map(|arg| self.probe(fcx, arg)).collect()
    }

    /// The type an expression would have, without keeping its diagnostics.
    fn probe(&mut self, fcx: &mut FnCx, expr: &Expr) -> Option<TypeRef> {
        let outer = self.diags.hold();
        let ty = self.check_expr(fcx, expr, None).ty;
        let _ = self.diags.release(outer);
        (!ty.is_error()).then_some(ty)
    }

    /// `Type(args)`: a type is not callable; its static functions are.
    fn type_call(
        &mut self,
        fcx: &mut FnCx,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> ErrorGuaranteed {
        let error = self.diags.emit(
            Diagnostic::error(codes::E056, format!("type `{name}` is not callable"))
                .label(span, true, "a type, not a function")
                .help(format!(
                    "call a static function: `{name}.new(...)`, or declare `variable: {name}.new(...)`"
                )),
        );
        self.callee_failed(fcx, args, error)
    }

    /// The declared type of `name: Type.function(args)`: `Type`, which a
    /// namespace is not.
    pub(crate) fn construct_type(&mut self, ty: &AstType, function: &str) -> TypeRef {
        let path = match &ty.shape {
            lilac_ast::TypeShape::Named { name, args } if args.is_empty() => name.as_str(),
            _ => return self.resolve_type(ty),
        };
        if !self.names_namespace(path) {
            return self.resolve_type(ty);
        }
        self.diags.emit(
            Diagnostic::error(codes::E056, format!("`{path}` is a namespace, not a type"))
                .label(ty.span, true, "the declaration shorthand needs a type here")
                .help(format!(
                    "the name before `(` is a static function of the type before it; \
                     to call `{path}.{function}`, write `variable: Type = {path}.{function}(...)`"
                )),
        );
        TypeRef::ERROR
    }

    /// `name: Type.function(args)` as a value.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn check_construct(
        &mut self,
        fcx: &mut FnCx,
        function: &str,
        member_span: Span,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> HirExpr {
        let called = self.construct_call(
            fcx,
            function,
            member_span,
            generic_args,
            args,
            span,
            expected,
        );
        match called {
            // It cannot fail here (`func_call`), so its one out is the value.
            Ok(Called::Call { callee, args, outs }) => HirExpr {
                ty: outs.first().copied().unwrap_or(TypeRef::ERROR),
                span,
                kind: HirExprKind::Call {
                    callee,
                    args,
                    rest: outs.get(1..).unwrap_or_default().to_vec(),
                },
            },
            Ok(Called::Value(value)) => value,
            Err(_) => HirExpr::poison(span),
        }
    }

    /// `name: Type.function(args)`: a call of `function`, a static function
    /// of the declared type `expected` (no `self`), whose success outs must
    /// be exactly one value of that type. It may fail when a guard handles
    /// it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn construct_call(
        &mut self,
        fcx: &mut FnCx,
        function: &str,
        member_span: Span,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<Called, ErrorGuaranteed> {
        let Some(want) = expected.filter(|want| *want != TypeId::ERROR) else {
            // The declared type reported it.
            return Err(self.callee_failed(fcx, args, self.diags.already_reported()));
        };
        let ty = TypeRef::value(want);
        let receiver = Receiver::Type(ty);
        let owner = self
            .type_parts(want)
            .map(|(def, _)| MemberReceiver::Constructor(def));
        if let Err(error) = self.record_member(member_span, function, owner) {
            return Err(self.callee_failed(fcx, args, error));
        }
        let name = tail_span(member_span, function);
        if let Some(error) = self.construct_member_error(ty, function, name) {
            return Err(self.callee_failed(fcx, args, error));
        }
        let called = self.static_call(
            fcx,
            receiver,
            function,
            generic_args,
            args,
            span,
            member_span,
            expected,
        );
        // `static_call` reported an error (a variant was ruled out above).
        let Called::Call { callee, args, outs } = called? else {
            lilac_error::bug!(span, "the shorthand's function is a variant");
        };
        let success = self.success_outs(&callee).unwrap_or(outs.len());
        let successes = &outs[..success];
        if successes != [ty] {
            let owner = self.show(ty);
            let returns = if successes.is_empty() {
                "nothing".to_owned()
            } else {
                let names: Vec<String> = successes.iter().map(|out| self.show(*out)).collect();
                format!("`{}`", names.join(", "))
            };
            let mut involved = vec![want];
            involved.extend(successes.iter().map(|out| out.id));
            return Err(self.emit_involving(
                Diagnostic::error(
                    codes::E056,
                    format!("`{owner}.{function}` returns {returns}, not one `{owner}`"),
                )
                .label(name, true, format!("must return exactly one `{owner}`"))
                .help(format!(
                    "the shorthand needs a function that makes a `{owner}`; otherwise write `variable: {owner} = ...`"
                )),
                &involved,
            ));
        }
        Ok(Called::Call { callee, args, outs })
    }

    /// Why member `function` of `ty` cannot be the shorthand's function: it
    /// is a variant, or a method taking `self`. A missing member is left to
    /// [`Self::static_call`].
    fn construct_member_error(
        &mut self,
        ty: TypeRef,
        function: &str,
        name: Span,
    ) -> Option<ErrorGuaranteed> {
        let (def, _) = self.type_parts(ty.id)?;
        let member = self.res.member(def, function)?;
        let owner = self.show(ty);
        let (message, label, help) = match &self.res.def(member).kind {
            DefKind::Variant(_) => (
                format!("`{function}` is a variant of `{owner}`, not a static function"),
                "a variant",
                format!("write `variable: {owner} = .{function}`, with its payload if it has one"),
            ),
            DefKind::Func(decl)
                if decl
                    .params
                    .first()
                    .is_some_and(lilac_ast::Param::is_receiver) =>
            {
                (
                    format!("`{owner}.{function}` takes `self`"),
                    "a method",
                    "the shorthand calls a static function, one without `self`".to_owned(),
                )
            }
            _ => return None,
        };
        Some(
            self.diags.emit(
                Diagnostic::error(codes::E056, message)
                    .label(name, true, label)
                    .help(help),
            ),
        )
    }

    /// `@hook(args)`: exact-match overload selection.
    fn hook_call(
        &mut self,
        fcx: &mut FnCx,
        name: &str,
        overloads: &[Hook],
        args: &[Expr],
        span: Span,
    ) -> Result<Called, ErrorGuaranteed> {
        let Some(hook) = self.select_overload(fcx, overloads, args) else {
            let error = self.diags.emit(
                Diagnostic::error(
                    codes::E022,
                    format!("no overload of `{name}` takes {} arguments", args.len()),
                )
                .label(span, true, "arity mismatch here"),
            );
            return Err(self.callee_failed(fcx, args, error));
        };
        self.use_hook(&hook.key, span);
        let args = self.check_exact_args(fcx, &hook.sig.params, args, span)?;
        Ok(Called::Call {
            callee: HirCallee::Func(hook.key),
            args,
            outs: hook.sig.returns,
        })
    }

    /// The overload whose arity and (probed) argument types match.
    fn select_overload(
        &mut self,
        fcx: &mut FnCx,
        overloads: &[Hook],
        args: &[Expr],
    ) -> Option<Hook> {
        let candidates: Vec<&Hook> = overloads
            .iter()
            .filter(|hook| hook.sig.params.len() == args.len())
            .collect();
        let first = (*candidates.first()?).clone();
        if candidates.len() == 1 {
            return Some(first);
        }
        let probed: Vec<Option<TypeRef>> = args
            .iter()
            .map(|arg| {
                if matches!(arg, Expr::Number(..)) {
                    None
                } else {
                    self.probe(fcx, arg)
                }
            })
            .collect();
        let found = candidates.into_iter().find(|hook| {
            hook.sig
                .params
                .iter()
                .zip(&probed)
                .all(|(param, got)| match got {
                    None => true,
                    Some(got) if param.is_ref => got.is_ref && got.id == param.id,
                    Some(got) => got.id == param.id,
                })
        });
        Some(found.cloned().unwrap_or(first))
    }

    /// Arguments of an ordinary call: each converts to its parameter type.
    pub(crate) fn check_args(
        &mut self,
        fcx: &mut FnCx,
        params: &[TypeRef],
        args: &[Expr],
        span: Span,
    ) -> Result<Vec<HirExpr>, ErrorGuaranteed> {
        if let Err(error) = self.arity(params.len(), args.len(), span) {
            return Err(self.callee_failed(fcx, args, error));
        }
        let mut checked = Vec::with_capacity(args.len());
        let mut failed = false;
        for (arg, param) in args.iter().zip(params) {
            let value = self.check_expr(fcx, arg, Some(param.id));
            if value.ty.is_error() {
                failed = true;
                continue;
            }
            if param.is_ref && !value.ty.is_ref {
                self.mismatch(*param, value.ty, span, arg.span());
                failed = true;
                continue;
            }
            let value = self.coerce(value, *param, span);
            failed |= value.ty.is_error();
            checked.push(value);
        }
        if failed {
            // Each failed argument reported it.
            Err(self.diags.already_reported())
        } else {
            Ok(checked)
        }
    }

    /// Arguments of a hook call: exact types only (references deref).
    fn check_exact_args(
        &mut self,
        fcx: &mut FnCx,
        params: &[TypeRef],
        args: &[Expr],
        span: Span,
    ) -> Result<Vec<HirExpr>, ErrorGuaranteed> {
        if let Err(error) = self.arity(params.len(), args.len(), span) {
            return Err(self.callee_failed(fcx, args, error));
        }
        let mut checked = Vec::with_capacity(args.len());
        let mut failed = false;
        for (arg, param) in args.iter().zip(params) {
            let value = self.check_expr(fcx, arg, Some(param.id));
            if value.ty.is_error() {
                failed = true;
                continue;
            }
            let fits = if param.is_ref {
                value.ty == *param
            } else {
                value.ty.id == param.id
            };
            if !fits {
                self.mismatch(*param, value.ty, span, arg.span());
                failed = true;
                continue;
            }
            let value = if param.is_ref {
                value
            } else {
                self.auto_deref(value)
            };
            checked.push(value);
        }
        if failed {
            // Each failed argument reported it.
            Err(self.diags.already_reported())
        } else {
            Ok(checked)
        }
    }

    /// One argument in a variadic extern's `...`: it has no parameter type, so
    /// it must carry its own, and C must not promote it (`E028`).
    fn check_variadic_arg(
        &mut self,
        fcx: &mut FnCx,
        arg: &Expr,
    ) -> Result<HirExpr, ErrorGuaranteed> {
        if is_foldable_literal(arg) {
            return Err(self.diags.emit(
                Diagnostic::error(codes::E028, "a variadic argument needs a type")
                    .label(arg.span(), true, "this literal has no type to pass")
                    .help("give it one with `as`, e.g. `0x1b6 as @bit32`"),
            ));
        }
        let value = self.check_expr(fcx, arg, None);
        if value.ty.is_error() {
            // The argument reported it.
            return Err(self.diags.already_reported());
        }
        let passes = value.ty.is_ref
            || ["@bit32", "@bit64", "@bit_native", "@fbit64"]
                .iter()
                .any(|name| self.types.builtin(name) == Some(value.ty.id));
        if passes {
            return Ok(value);
        }
        let ty = self.show(value.ty);
        Err(self.diags.emit(
            Diagnostic::error(codes::E028, format!("`{ty}` cannot be a variadic argument"))
                .label(arg.span(), true, "not passable through `...`")
                .help("C promotes narrow integers and floats; pass a `@bit32`, `@bit64`, `@bit_native`, `@fbit64` or a `ref`, widening explicitly with `as`"),
        ))
    }

    fn arity(&mut self, expected: usize, found: usize, span: Span) -> Result<(), ErrorGuaranteed> {
        if expected == found {
            return Ok(());
        }
        let diagnostic = Diagnostic::error(
            codes::E022,
            format!("wrong number of arguments: expected {expected}, found {found}"),
        )
        .label(span, true, "arity mismatch here");
        Err(self.emit_requirement(diagnostic, Requirement::Arity { expected, found }, &[]))
    }

    // ---- intrinsics -------------------------------------------------------

    fn check_intrinsic(
        &mut self,
        fcx: &mut FnCx,
        intrinsic: Intrinsic,
        name: &str,
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<Called, ErrorGuaranteed> {
        if intrinsic.is_special() {
            return self.check_special_intrinsic(fcx, intrinsic, name, args, span, expected);
        }
        let signature = intrinsic.signature();
        let arity = self.intrinsic_arity(name, signature.params.len(), args.len(), span);
        self.callee(fcx, args, arity)?;
        let exact = matches!(
            intrinsic,
            Intrinsic::Int(..)
                | Intrinsic::Float(..)
                | Intrinsic::FbitToBits(_)
                | Intrinsic::BitsToFbit(_)
                | Intrinsic::Atomic(..)
        );
        let mut checked = Vec::with_capacity(args.len());
        let mut failed = false;
        for (arg, slot) in args.iter().zip(signature.slots()) {
            match self.intrinsic_arg(fcx, arg, slot, exact, span) {
                Some(value) => checked.push(value),
                None => failed = true,
            }
        }
        if failed {
            // `intrinsic_arg` reported it.
            return Err(self.diags.already_reported());
        }
        let outs = match signature.result {
            None => Vec::new(),
            Some(Slot::Builtin(ty)) => vec![ty.value()],
            Some(Slot::Ref(ty)) => vec![TypeRef::reference(ty.id())],
            Some(Slot::ExpectedBit | Slot::AnyBit) => match expected {
                Some(want) if self.is_bit(want) => vec![TypeRef::value(want)],
                _ => {
                    return Err(self.diags.emit(
                        Diagnostic::error(
                            codes::E024,
                            format!("`{name}` needs an expected bit type"),
                        )
                        .label(span, true, "annotate the binding"),
                    ));
                }
            },
            Some(Slot::ExpectedFloat | Slot::AnyFloat) => match expected {
                Some(want) if self.is_float(want) => vec![TypeRef::value(want)],
                _ => {
                    return Err(self.diags.emit(
                        Diagnostic::error(
                            codes::E024,
                            format!("`{name}` needs an expected float type"),
                        )
                        .label(span, true, "annotate the binding"),
                    ));
                }
            },
            Some(slot) => {
                lilac_error::bug!(
                    span,
                    "`{name}` returns `{slot}`, which is checked specially"
                );
            }
        };
        Ok(Called::Call {
            callee: HirCallee::Intrinsic(intrinsic),
            args: checked,
            outs,
        })
    }

    /// An intrinsic typeck checks specially (`Intrinsic::is_special`):
    /// its signature only describes its operands.
    fn check_special_intrinsic(
        &mut self,
        fcx: &mut FnCx,
        intrinsic: Intrinsic,
        name: &str,
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<Called, ErrorGuaranteed> {
        match intrinsic {
            Intrinsic::SizeOf | Intrinsic::AlignOf => {
                self.intrinsic_arity(name, 1, args.len(), span)?;
                let ty = self.size_of_operand(fcx, &args[0])?;
                let kind = if intrinsic == Intrinsic::SizeOf {
                    HirExprKind::SizeOf(ty)
                } else {
                    HirExprKind::AlignOf(ty)
                };
                Ok(Called::Value(HirExpr {
                    ty: Builtin::BitNative.value(),
                    span,
                    kind,
                }))
            }
            Intrinsic::ArrayAt => self.check_array_at(fcx, args, span),
            Intrinsic::ArrayLen => self.check_array_len(fcx, args, span),
            Intrinsic::FormatArgsHoles | Intrinsic::FormatArgsPiece | Intrinsic::FormatArgsHole => {
                self.check_format_consumer(fcx, intrinsic, name, args, span)
            }
            _ if intrinsic.is_reflection() => self.check_reflect(fcx, intrinsic, name, args, span),
            _ if intrinsic.is_memory() => {
                self.check_memory(fcx, intrinsic, name, args, span, expected)
            }
            Intrinsic::Panic => self.check_panic(fcx, name, args, span),
            Intrinsic::CompileError => {
                self.intrinsic_arity(name, 1, args.len(), span)?;
                let Expr::Str(msg, msg_span) = &args[0] else {
                    return Err(self.diags.emit(
                        Diagnostic::error(
                            codes::E010,
                            "`@compile_error` needs a string literal message",
                        )
                        .label(args[0].span(), true, "message goes here"),
                    ));
                };
                Ok(Called::Value(HirExpr {
                    ty: expected.map_or(TypeRef::ERROR, TypeRef::value),
                    span,
                    kind: HirExprKind::CompileError {
                        msg: String::from_utf8_lossy(msg).into_owned(),
                        msg_span: *msg_span,
                    },
                }))
            }
            _ => lilac_error::bug!(span, "`{name}` is not checked specially"),
        }
    }

    /// `@panic(message)`: the message is what `@compiler_print` takes
    /// (`@byte_buffer` without a hook), slot rules included, so with a
    /// format hook a literal, an f-string or one value is a message.
    fn check_panic(
        &mut self,
        fcx: &mut FnCx,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Called, ErrorGuaranteed> {
        let arity = self.intrinsic_arity(name, 1, args.len(), span);
        self.callee(fcx, args, arity)?;
        let want = TypeRef::value(
            self.panic_hook_message()
                .unwrap_or_else(|| Builtin::ByteBuffer.id()),
        );
        let value = self.check_expr(fcx, &args[0], Some(want.id));
        let value = self.coerce(value, want, args[0].span());
        if value.ty.is_error() {
            // `check_expr` or `coerce` reported it.
            return Err(self.diags.already_reported());
        }
        Ok(Called::Call {
            callee: HirCallee::Intrinsic(Intrinsic::Panic),
            args: vec![value],
            outs: Vec::new(),
        })
    }

    pub(crate) fn intrinsic_arity(
        &mut self,
        name: &str,
        want: usize,
        found: usize,
        span: Span,
    ) -> Result<(), ErrorGuaranteed> {
        if want == found {
            return Ok(());
        }
        Err(self.diags.emit(
            Diagnostic::error(
                codes::E022,
                format!("wrong number of arguments to `{name}`: expected {want}, found {found}"),
            )
            .label(span, true, "arity mismatch here"),
        ))
    }

    pub(crate) fn intrinsic_arg(
        &mut self,
        fcx: &mut FnCx,
        arg: &Expr,
        slot: Slot,
        exact: bool,
        span: Span,
    ) -> Option<HirExpr> {
        match slot {
            Slot::Builtin(ty) => {
                let id = ty.id();
                let value = self.check_expr(fcx, arg, Some(id));
                if value.ty.is_error() {
                    return None;
                }
                if !exact {
                    let value = self.coerce(value, TypeRef::value(id), arg.span());
                    return (!value.ty.is_error()).then_some(value);
                }
                let value = self.auto_deref(value);
                if value.ty.id != id {
                    let diagnostic = Diagnostic::error(
                        codes::E010,
                        format!(
                            "mismatched types: expected {}, found {}",
                            self.show_id(id),
                            self.show_id(value.ty.id)
                        ),
                    )
                    .label(arg.span(), true, "wrong operand type here");
                    let requirement = Requirement::Mismatch {
                        expected_ref: false,
                    };
                    self.emit_requirement(diagnostic, requirement, &[value.ty.id]);
                    return None;
                }
                Some(value)
            }
            Slot::Ref(ty) => {
                let id = ty.id();
                let name = ty.name();
                let value = self.check_expr(fcx, arg, None);
                if value.ty.is_error() {
                    return None;
                }
                if value.ty.is_ref && value.ty.id == id {
                    return Some(value);
                }
                let diagnostic = Diagnostic::error(
                    codes::E010,
                    format!("mismatched types: expected ref {name}"),
                )
                .label(span, false, format!("expected `ref {name}` here"))
                .label(arg.span(), true, "needs an explicit `ref`");
                let requirement = Requirement::Mismatch { expected_ref: true };
                self.emit_requirement(diagnostic, requirement, &[value.ty.id]);
                None
            }
            Slot::AnyBit | Slot::ExpectedBit | Slot::AnyFloat | Slot::ExpectedFloat => {
                let float = matches!(slot, Slot::AnyFloat | Slot::ExpectedFloat);
                let value = self.check_expr(fcx, arg, None);
                if value.ty.is_error() {
                    return None;
                }
                let value = self.auto_deref(value);
                let fits = if float {
                    self.is_float(value.ty.id)
                } else {
                    self.is_bit(value.ty.id)
                };
                if fits {
                    return Some(value);
                }
                let container = if float {
                    "a float container"
                } else {
                    "a bit container"
                };
                let diagnostic = Diagnostic::error(
                    codes::E010,
                    format!(
                        "mismatched types: expected {container}, found {}",
                        self.show_id(value.ty.id)
                    ),
                )
                .label(arg.span(), true, format!("not {container}"));
                let requirement = Requirement::Mismatch {
                    expected_ref: false,
                };
                self.emit_requirement(diagnostic, requirement, &[value.ty.id]);
                None
            }
            _ => lilac_error::bug!(span, "slot `{slot}` is checked specially"),
        }
    }

    /// `@size_of`'s and `@align_of`'s operand: a type (`@bit32`, `T`, `Box[T]`, `ref T`) or a
    /// value (its type).
    pub(crate) fn size_of_operand(
        &mut self,
        fcx: &mut FnCx,
        arg: &Expr,
    ) -> Result<TypeRef, ErrorGuaranteed> {
        let as_type = match arg {
            Expr::RefOf { value, .. } => self.static_receiver(fcx, value).map(|receiver| {
                receiver.and_then(|receiver| match receiver {
                    Receiver::Type(ty) => Ok(TypeRef::reference(ty.id)),
                    Receiver::Bare(def) => Err(self.bare_arity(def, arg.span())),
                })
            }),
            _ => self.static_receiver(fcx, arg).map(|receiver| {
                receiver.and_then(|receiver| match receiver {
                    Receiver::Type(ty) => Ok(ty),
                    Receiver::Bare(def) => Err(self.bare_arity(def, arg.span())),
                })
            }),
        };
        if let Some(ty) = as_type {
            return ty;
        }
        let value = self.check_expr(fcx, arg, None);
        if value.ty.is_error() {
            // The argument reported it.
            return Err(self.diags.already_reported());
        }
        Ok(value.ty)
    }

    fn bare_arity(&mut self, def: DefId, span: Span) -> ErrorGuaranteed {
        let name = self.res.def(def).name.clone();
        let count = self.res.def(def).generic_params.len();
        self.generic_arity(span, &name, count, 0)
    }
}
