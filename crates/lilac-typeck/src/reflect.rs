//! The reflection tests: `@has_field`, `@has_method`, `@has_function`,
//! `@has_constant`, `@is_copyable` and `@is_same`. Each answers by the
//! lookup a use would take and folds to a `@bit8` literal per instance
//! (like `@needs_drop`); a template whose answer depends on its arguments
//! holds `HirExprKind::Unanswered`. A false test is silent: a missing
//! member, one of another type, a private one or a generic one is `0x0`,
//! never an error.

use lilac_ast::{Expr, TypeShape};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_hir::intrinsics::Intrinsic;
use lilac_hir::{HirExpr, HirExprKind};
use lilac_resolve::{DefId, DefKind};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeKind, TypeRef};

use crate::body::FnCx;
use crate::calls::Called;
use crate::Checker;

impl Checker<'_> {
    /// One of the reflection tests (see the module docs).
    pub(crate) fn check_reflect(
        &mut self,
        fcx: &mut FnCx,
        intrinsic: Intrinsic,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Called, ErrorGuaranteed> {
        let arity = match intrinsic {
            Intrinsic::IsCopyable => 1,
            Intrinsic::IsSame => 2,
            _ => 3,
        };
        self.intrinsic_arity(name, arity, args.len(), span)?;
        let answer = match intrinsic {
            Intrinsic::IsCopyable => {
                let ty = self.test_type(fcx, &args[0])?;
                if self.types.contains_param(ty.id) {
                    None
                } else {
                    Some(self.is_copyable(ty))
                }
            }
            Intrinsic::IsSame => {
                let first = self.test_type(fcx, &args[0]);
                let second = self.test_type(fcx, &args[1]);
                self.same_type(first?, second?)
            }
            _ => {
                // Every operand is checked, so each reports its own error.
                let owner = self.test_type(fcx, &args[0]);
                let member = self.member_name(&args[1]);
                let stated = self.test_type(fcx, &args[2]);
                let (owner, (member, member_span), stated) = (owner?, member?, stated?);
                self.stated_function(intrinsic, stated, args[2].span())?;
                self.has_member(intrinsic, owner, &member, member_span, stated)?
            }
        };
        let kind = answer.map_or(HirExprKind::Unanswered, |yes| {
            HirExprKind::Int(u64::from(yes))
        });
        Ok(Called::Value(HirExpr {
            ty: Builtin::Bit8.value(),
            span,
            kind,
        }))
    }

    /// A test's type operand: a type, a value standing for its type
    /// ([`Self::size_of_operand`]), or a function type, which may fail and
    /// needs no `ref` (it states a member's type, like an alias's target).
    fn test_type(&mut self, fcx: &mut FnCx, arg: &Expr) -> Result<TypeRef, ErrorGuaranteed> {
        let function = |expr: &Expr| match expr {
            Expr::TypeApp(ty) => match &ty.shape {
                TypeShape::Function(func) => Some((ty.clone(), func.clone())),
                _ => None,
            },
            _ => None,
        };
        let ty = if let Some((_, func)) = function(arg) {
            let template = TypeRef::value(self.resolve_function(&func));
            self.substitute_env(template)
        } else if let Some((syntax, func)) = match arg {
            Expr::RefOf { value, .. } => function(value),
            _ => None,
        } {
            let template = TypeRef::reference(self.resolve_function(&func));
            let ty = self.function_value(&syntax, template);
            self.substitute_env(ty)
        } else {
            self.size_of_operand(fcx, arg)?
        };
        if ty.is_error() {
            // `resolve_function` or `function_value` reported it.
            return Err(self.diags.already_reported());
        }
        self.ensure_type(ty, arg.span())?;
        Ok(ty)
    }

    /// `ty` at the arguments of the body being checked.
    fn substitute_env(&mut self, ty: TypeRef) -> TypeRef {
        if self.env.args.is_empty() || ty.is_error() {
            return ty;
        }
        let args = self.env.args.clone();
        self.types.substitute(ty, &args)
    }

    /// A test's member name: a bare identifier (`E022` otherwise).
    fn member_name(&mut self, arg: &Expr) -> Result<(String, Span), ErrorGuaranteed> {
        if let Expr::Ident(name, span) = arg {
            if !name.starts_with('@') {
                return Ok((name.clone(), *span));
            }
        }
        Err(self.diags.emit(
            Diagnostic::error(codes::E022, "expected a member name")
                .label(arg.span(), true, "write the member's name here")
                .help("a member is named by its identifier: `@has_field(T, count, Int)`"),
        ))
    }

    /// `E010` unless the type a `@has_method` or `@has_function` states is
    /// a function type (written without `ref`), so a test never stays
    /// false for a type no member can have.
    fn stated_function(
        &mut self,
        intrinsic: Intrinsic,
        stated: TypeRef,
        span: Span,
    ) -> Result<(), ErrorGuaranteed> {
        if !matches!(intrinsic, Intrinsic::HasMethod | Intrinsic::HasFunction)
            || (!stated.is_ref && matches!(self.types.kind(stated.id), TypeKind::Func { .. }))
        {
            return Ok(());
        }
        let found = self.show(stated);
        let mut diagnostic = Diagnostic::error(
            codes::E010,
            format!("expected a function type, found `{found}`"),
        )
        .label(span, true, "the member's function type goes here");
        diagnostic = if stated.is_ref {
            diagnostic.help("a test states the function's type itself: drop the `ref`")
        } else {
            diagnostic.help("state its type: `function (ref T) @bit8`")
        };
        Err(self.diags.emit(diagnostic))
    }

    /// Whether `a` and `b` are one type; `None` while a parameter in
    /// either could still make them so, or not.
    fn same_type(&self, a: TypeRef, b: TypeRef) -> Option<bool> {
        if a == b {
            return Some(true);
        }
        let open = self.types.contains_param(a.id) || self.types.contains_param(b.id);
        // A parameter may stand for a `ref`, so `ref`-ness decides nothing.
        let could = a.is_ref != b.is_ref || self.could_match(a.id, b.id);
        (!(open && could)).then_some(false)
    }

    /// `@has_field`, `@has_method`, `@has_function` or `@has_constant` on
    /// `owner` (looked through a reference): the member a use of `name`
    /// would find, if it is there, visible and of exactly the `stated`
    /// type. A found member's name is a use of it.
    fn has_member(
        &mut self,
        intrinsic: Intrinsic,
        owner: TypeRef,
        name: &str,
        name_span: Span,
        stated: TypeRef,
    ) -> Result<Option<bool>, ErrorGuaranteed> {
        let owner = owner.deref();
        let Some((def, type_args)) = self.type_parts(owner.id) else {
            // A parameter has its argument's members; any other type
            // (a builtin, an array, a function) has none.
            let param = matches!(self.types.kind(owner.id), TypeKind::Param { .. });
            return Ok((!param).then_some(false));
        };
        let Some(member) = self.res.member(def, name) else {
            return Ok(Some(false));
        };
        if self.private_hidden(member, def) {
            return Ok(Some(false));
        }
        let found = match (intrinsic, &self.res.def(member).kind) {
            (Intrinsic::HasField, DefKind::Field(_)) => {
                let field = self
                    .fields_of(owner.id)
                    .and_then(|fields| fields.into_iter().find(|(field, _)| *field == name));
                match field {
                    Some((_, ty)) => ty,
                    // A template's type may have no layout yet.
                    None if self.types.contains_param(owner.id) => return Ok(None),
                    None => return Ok(Some(false)),
                }
            }
            (Intrinsic::HasConstant, DefKind::Const { .. }) => self.const_type(member),
            (Intrinsic::HasMethod | Intrinsic::HasFunction, DefKind::Func(_)) => {
                let method = intrinsic == Intrinsic::HasMethod;
                match self.member_function_type(member, def, &type_args, method)? {
                    Some(ty) => ty,
                    None => return Ok(Some(false)),
                }
            }
            _ => return Ok(Some(false)),
        };
        if found.is_error() {
            // `const_type` or the member's signature reported it.
            return Err(self.diags.already_reported());
        }
        let answer = self.same_type(found, stated);
        if answer != Some(false) {
            self.record_use(name_span, member);
        }
        Ok(answer)
    }

    /// The function type of `member`, a function of `owner` at
    /// `type_args`: its parameters (a method's receiver first), success
    /// outs and failure outs. `None` when it is not what the test asks
    /// for: a method (`method`) or a static function, and never a generic
    /// one, whose type no test can spell.
    fn member_function_type(
        &mut self,
        member: DefId,
        owner: DefId,
        type_args: &[TypeRef],
        method: bool,
    ) -> Result<Option<TypeRef>, ErrorGuaranteed> {
        let def = self.res.def(member);
        let Some(decl) = def.func() else {
            return Ok(None);
        };
        let receiver = decl
            .params
            .first()
            .is_some_and(lilac_ast::Param::is_receiver);
        if !def.generic_params.is_empty() || receiver != method {
            return Ok(None);
        }
        let (successes, fails) = (decl.returns.len(), decl.failures.is_some());
        let c_abi = decl.is_export || decl.is_extern;
        let sig = self.sig_of(member)?;
        let at = |c: &mut Self, slots: &[TypeRef]| -> Vec<TypeRef> {
            slots
                .iter()
                .map(|ty| c.types.substitute(*ty, type_args))
                .collect()
        };
        let params = at(self, &sig.params);
        // A method's receiver is the owner itself (`ref T`, or `T` for
        // `self: T`), as `method_call` requires.
        let of_owner = |first: &TypeRef| self.type_parts(first.id).is_some_and(|(d, _)| d == owner);
        if method && !params.first().is_some_and(of_owner) {
            return Ok(None);
        }
        let returns = at(self, &sig.returns[..successes]);
        // The failure outs, then the hidden flag (`HirFunc::failure`).
        let failures = fails.then(|| at(self, &sig.returns[successes..sig.returns.len() - 1]));
        let id = self.types.intern_function(params, returns, failures, c_abi);
        Ok(Some(TypeRef::value(id)))
    }
}
