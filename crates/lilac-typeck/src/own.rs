//! A type's own names in its bodies.
//!
//! Inside a method, hook or member constant of a type (wherever
//! [`Env::owner`] is the type), its constants and static functions (no
//! receiver) are in scope by their bare names: after locals and generic
//! parameters, before the file's aliases and items. Fields and methods stay
//! behind `self`, and variants behind `.Variant`.
//!
//! [`Env::owner`]: crate::Env::owner

use lilac_ast::{Expr, Type as AstType};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_hir::HirExpr;
use lilac_resolve::{DefId, DefKind};
use lilac_span::Span;
use lilac_ty::TypeRef;

use crate::body::FnCx;
use crate::calls::Called;
use crate::Checker;

impl Checker<'_> {
    /// The constant or static function `name` of the type whose body is
    /// being checked. A generic parameter of that name comes first.
    pub(crate) fn own_member(&self, name: &str) -> Option<DefId> {
        let owner = self.env.owner?;
        if self.env.params.iter().any(|param| param == name) {
            return None;
        }
        let member = self.res.member(owner, name)?;
        self.res.def(member).is_own_name().then_some(member)
    }

    /// The names [`Self::own_member`] finds here, for did-you-mean.
    pub(crate) fn own_names(&self) -> Vec<String> {
        self.env
            .owner
            .map(|owner| {
                self.res
                    .def(owner)
                    .members
                    .iter()
                    .filter(|(_, id)| self.res.def(**id).is_own_name())
                    .map(|(name, _)| name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The type whose body is being checked, at the body's arguments (its
    /// abstract parameters in a template), as bare `self` is typed. `None`
    /// outside a type's body, and in a member constant of a generic type,
    /// which is one value for every instance.
    fn own_type(&mut self) -> Option<TypeRef> {
        let owner = self.env.owner?;
        let names = self.res.def(owner).generic_params.clone();
        if !self.env.params.starts_with(&names) {
            return None;
        }
        let args = names
            .iter()
            .zip(0u32..)
            .map(|(name, index)| {
                self.env
                    .args
                    .get(index as usize)
                    .copied()
                    .unwrap_or_else(|| TypeRef::value(self.types.intern_param(index, name)))
            })
            .collect();
        Some(self.type_of_def(owner, args))
    }

    /// The arguments an own static function `name` is instantiated at:
    /// the own type's. `E021` in a generic type's member constant, which
    /// has none.
    fn own_type_args(&mut self, name: &str, span: Span) -> Result<Vec<TypeRef>, ErrorGuaranteed> {
        if let Some(ty) = self.own_type() {
            return Ok(self
                .type_parts(ty.id)
                .map(|(_, args)| args)
                .unwrap_or_default());
        }
        let owner = self
            .env
            .owner
            .map_or("", |owner| self.res.def(owner).name.as_str());
        let diagnostic = Diagnostic::error(
            codes::E021,
            format!("cannot name `{name}` in a constant of generic `{owner}`"),
        )
        .label(
            span,
            true,
            format!("a function of each instance of `{owner}`"),
        );
        let diagnostic = self.owner_param_note(diagnostic, name);
        Err(self.diags.emit(diagnostic))
    }

    /// The own member `def` (from [`Self::own_member`]) named `name` at
    /// `span` as a value: a constant's value, or a static function's
    /// address (`E077` unless `behind_ref`).
    pub(crate) fn own_value(
        &mut self,
        def: DefId,
        name: &str,
        span: Span,
        behind_ref: bool,
    ) -> HirExpr {
        self.record_use(span, def);
        if let DefKind::Const { .. } = self.res.def(def).kind {
            return self.const_value(def, span);
        }
        match self.own_type_args(name, span) {
            Ok(args) => self.func_value(def, args, name, span, behind_ref),
            Err(_) => HirExpr::poison(span),
        }
    }

    /// `name(args)` where `name` is the own member `def`: a static function
    /// called at the own type's arguments, or a constant holding a function
    /// reference called through it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn own_call(
        &mut self,
        fcx: &mut FnCx,
        def: DefId,
        name: &str,
        name_span: Span,
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
    ) -> Result<Called, ErrorGuaranteed> {
        if let DefKind::Const { .. } = self.res.def(def).kind {
            if !generic_args.is_empty() {
                let error = self.generic_arity(span, name, 0, generic_args.len());
                return Err(self.callee_failed(fcx, args, error));
            }
            let callee = self.own_value(def, name, name_span, false);
            return self.value_call(fcx, callee, &format!("`{name}`"), args, span);
        }
        self.record_use(name_span, def);
        let type_args = match self.own_type_args(name, name_span) {
            Ok(type_args) => type_args,
            Err(error) => return Err(self.callee_failed(fcx, args, error)),
        };
        self.func_call(fcx, def, type_args, generic_args, args, span, None)
    }

    /// `diagnostic` (an unresolved bare `name`), with help when `name` is a
    /// field or method of the type whose body is being checked: those are
    /// reached through `self`, or through a value of the type where there
    /// is no `self`.
    pub(crate) fn own_instance_help(
        &self,
        fcx: &FnCx,
        diagnostic: Diagnostic,
        name: &str,
    ) -> Diagnostic {
        let Some(owner) = self.env.owner else {
            return diagnostic;
        };
        let Some(member) = self.res.member(owner, name) else {
            return diagnostic;
        };
        let (what, written, reach) = match self.res.def(member).kind {
            DefKind::Field(_) => ("a field", format!("self.{name}"), "read it from"),
            DefKind::Func(_) => ("a method", format!("self.{name}(…)"), "call it on"),
            _ => return diagnostic,
        };
        let owner = &self.res.def(owner).name;
        let help = if fcx.lookup("self").is_some() {
            format!("`{name}` is {what} of `{owner}`: write `{written}`")
        } else {
            format!("`{name}` is {what} of `{owner}`: {reach} a value of `{owner}`")
        };
        diagnostic.help(help)
    }
}
