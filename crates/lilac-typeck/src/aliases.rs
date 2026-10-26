//! Type aliases: `Name: alias Type` is another name for `Type`, the same
//! `TypeId`. An alias is resolved once, in its own file's scope, and
//! memoized; every use is then the type itself, so hooks, layouts and
//! symbols never see the alias. Only [`Types::display`] does
//! ([`Types::add_alias`]).
//!
//! [`Types::display`]: lilac_ty::Types::display
//! [`Types::add_alias`]: lilac_ty::Types::add_alias

use lilac_ast::TypeShape;
use lilac_error::{codes, Diagnostic};
use lilac_resolve::{DefId, DefKind};
use lilac_span::Span;
use lilac_ty::TypeRef;

use crate::Checker;

/// Where an alias's resolution stands.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AliasState {
    /// Being resolved: meeting it again is a cycle (`E074`).
    Active,
    /// Resolved; [`TypeRef::ERROR`] when it failed (already reported).
    Done(TypeRef),
}

impl Checker<'_> {
    /// Resolves every alias up front, outside any instance, so an error in
    /// one is reported plainly at the alias (never folded into the `E042`
    /// of the generic that first named it) and every alias is known to
    /// [`lilac_ty::Types::display`] before the first diagnostic. `@builtin`
    /// declares none.
    pub(crate) fn resolve_aliases(&mut self) {
        for id in self.res.ids() {
            if matches!(self.res.def(id).kind, DefKind::Alias(_)) {
                let span = self.res.def(id).name_span;
                let _ = self.alias_type(id, span);
            }
        }
    }

    /// The type the alias `def` names, named at `span`. A cycle is `E074`
    /// at the use that closes it.
    pub(crate) fn alias_type(&mut self, def: DefId, span: Span) -> TypeRef {
        match self.aliases.get(&def) {
            Some(AliasState::Done(ty)) => return *ty,
            Some(AliasState::Active) => {
                self.alias_cycle(def, span);
                return TypeRef::ERROR;
            }
            None => {}
        }
        let DefKind::Alias(syntax) = &self.res.def(def).kind else {
            lilac_error::bug!(span, "`{}` is not an alias", self.res.def(def).name);
        };
        let syntax = syntax.clone();
        self.aliases.insert(def, AliasState::Active);
        self.alias_stack.push(def);
        let env = self.env_of(def, Vec::new(), None);
        // An alias's target is the one place a bare function type may be
        // written: each use then writes `ref Name`.
        let resolved = self.with_env(env, |c| match &syntax.shape {
            TypeShape::Function(func) if !syntax.is_ref => TypeRef::value(c.resolve_function(func)),
            TypeShape::Named { name, args } => c.resolve_named(&syntax, name, args, true),
            _ => c.resolve_syntax(&syntax),
        });
        self.alias_stack.pop();
        // A cycle through this alias already failed it.
        let ty = match self.aliases.get(&def) {
            Some(AliasState::Done(failed)) => *failed,
            _ => resolved,
        };
        self.aliases.insert(def, AliasState::Done(ty));
        if !ty.is_error() {
            let name = self.res.qualified_name(def);
            self.types.add_alias(def, &name, ty.id);
        }
        ty
    }

    /// `E074`: resolving `def` needs `def`. Every alias on the loop fails
    /// with it, so it is reported once.
    fn alias_cycle(&mut self, def: DefId, span: Span) {
        let start = self
            .alias_stack
            .iter()
            .position(|alias| *alias == def)
            .unwrap_or(0);
        let chain: Vec<DefId> = self.alias_stack[start..].to_vec();
        for alias in &chain {
            self.aliases
                .insert(*alias, AliasState::Done(TypeRef::ERROR));
        }
        let name = self.res.def(def).name.clone();
        let mut diagnostic =
            Diagnostic::error(codes::E074, format!("alias `{name}` refers to itself")).label(
                span,
                true,
                format!("`{name}` is needed to know what `{name}` names"),
            );
        if chain.len() > 1 {
            let mut names: Vec<String> = chain
                .iter()
                .map(|alias| format!("`{}`", self.res.def(*alias).name))
                .collect();
            names.push(format!("`{name}`"));
            diagnostic = diagnostic
                .label(self.res.def(def).name_span, false, "declared here")
                .note(format!(
                    "the aliases name each other: {}",
                    names.join(" → ")
                ));
        }
        self.diags.emit(diagnostic);
    }
}
