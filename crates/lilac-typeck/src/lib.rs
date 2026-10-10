//! Type checking: resolved definitions in, concrete typed HIR out.
//!
//! The checker works through a queue of function instances. Every non-generic
//! function and hook is queued up front; a generic function (or a method of a
//! generic type) is queued the first time a concrete instance is used, and its
//! body is then checked under that instance's arguments (duck typing:
//! requirements are proven per instance). A failure inside an instance body is
//! reported once as `E042` at the use that requested it (see `generics.rs`).
//!
//! Module map: `sigs` (signatures and the hook table), `types` (type syntax,
//! layouts, generic type instances), `body` (statements and scopes), `expr`
//! (expressions and places), `calls` (call and member resolution), `literals`
//! (number literals), `generics` (instance diagnostics).

mod arrays;
mod body;
mod calls;
mod expr;
mod generics;
mod indices;
mod literals;
mod paths;
mod sigs;
mod sink;
mod suggest;
mod types;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};

use lilac_error::{codes, Diagnostic, Diags, ErrorGuaranteed, Unreported};
use lilac_hir::{FuncKey, HirFunc, HirProgram};
use lilac_resolve::{DefId, DefKind, Resolved};
use lilac_span::{FileId, Span};
use lilac_ty::{Builtin, FuncSig, TypeId, TypeKind, TypeRef, Types};
use sink::{Pending, Sink};

/// The result of checking a unit.
#[derive(Clone, Debug, Default)]
pub struct Checked {
    /// Concrete typed HIR (usable only when no errors were emitted).
    pub hir: HirProgram,
    /// Declared signature of every function and hook (generic ones as
    /// templates), for editor hovers.
    pub sigs: HashMap<DefId, FuncSig>,
    /// Bodies of generic functions that no instance reached, checked with
    /// their parameters left abstract ([`Checker::check_templates`]). Never
    /// lowered; they exist for semantic tokens and hovers.
    pub templates: Vec<HirFunc>,
}

/// What a unit is checked for.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// The target's pointer width (`@bit_native` literal range).
    pub native_bits: u32,
    /// Whether the unit is an executable (`lilac build`): a name `@entry`
    /// cannot find in the root namespace is then `E032`. Otherwise an
    /// `@entry` that names a missing root item is dropped silently, since
    /// nothing enters the program.
    pub require_entry: bool,
}

/// Checks every definition in `res`, generic functions nothing instantiates
/// included (as templates).
pub fn check(res: &Resolved, types: &mut Types, diags: &mut Diags, options: Options) -> Checked {
    let mut checker = Checker {
        res,
        types,
        diags: Sink::new(diags),
        native_bits: options.native_bits,
        require_entry: options.require_entry,
        entry_unresolved: false,
        sigs: HashMap::new(),
        failed_sigs: HashSet::new(),
        hooks: HashMap::new(),
        instances: HashMap::new(),
        queue: VecDeque::new(),
        type_states: HashMap::new(),
        type_stack: Vec::new(),
        arrays: Vec::new(),
        const_types: HashMap::new(),
        out: HirProgram::default(),
        env: Env::default(),
        instantiated: HashSet::new(),
        reported: HashSet::new(),
        origins: HashMap::new(),
        failures: Vec::new(),
    };
    checker.run();
    let templates = checker.check_templates();
    let sigs = checker.sigs.clone();
    Checked {
        hir: std::mem::take(&mut checker.out),
        sigs,
        templates,
    }
}

/// One entry of the concrete hook table.
#[derive(Clone, Debug)]
pub(crate) struct Hook {
    /// The instance implementing it.
    pub key: FuncKey,
    /// Its concrete signature.
    pub sig: FuncSig,
    /// Declaration span (for `E020`).
    pub span: Span,
}

/// Progress of one function instance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum State {
    /// Queued; body not checked yet.
    Queued,
    /// Body checked successfully.
    Done,
    /// Body has an error. Callers are still checked against the signature,
    /// so their own errors are reported too.
    BodyFailed,
    /// The instance cannot be called at all (a signature type failed, the
    /// `E051` depth cap, or a dropped `@entry`); later uses stay silent.
    Unusable,
}

/// One function instance.
#[derive(Clone, Debug)]
pub(crate) struct Instance {
    /// Concrete signature.
    pub sig: FuncSig,
    /// The instance whose body requested this one, and the use site.
    pub origin: Option<(Option<FuncKey>, Span)>,
    /// Number of generic instances on the request chain (`E051` bound).
    pub depth: usize,
    /// Progress.
    pub state: State,
}

/// Progress of one concrete type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TypeState {
    /// Layout being computed (a by-value cycle if reached again).
    Active,
    /// Layout and hooks registered.
    Done,
    /// Failed; later uses stay silent.
    Failed,
}

/// The environment a body or signature is checked in.
#[derive(Clone, Debug, Default)]
pub(crate) struct Env {
    /// File whose scope resolves names.
    pub file: Option<FileId>,
    /// Generic parameter names in scope (owning type's first).
    pub params: Vec<String>,
    /// Their concrete arguments (empty for templates).
    pub args: Vec<TypeRef>,
    /// The type whose member is being checked (private-member access).
    pub owner: Option<DefId>,
    /// The instance being checked.
    pub key: Option<FuncKey>,
}

/// A duck-typing requirement failure recorded alongside its diagnostic, so
/// `E042` can be assembled without re-reading message text.
#[derive(Clone, Debug)]
pub(crate) enum Requirement {
    /// `value.member` / `T.member` found nothing.
    Missing {
        /// Method (call) or member (field read).
        method: bool,
        /// Member name.
        name: String,
        /// Display of the receiver type.
        owner: String,
        /// The receiver's base definition, for did-you-mean.
        owner_def: Option<DefId>,
    },
    /// A one-step conversion was missing.
    Conversion {
        /// The source type.
        from: TypeId,
    },
    /// Wrong argument count.
    Arity {
        /// Expected count.
        expected: usize,
        /// Supplied count.
        found: usize,
    },
    /// Any other type mismatch.
    Mismatch {
        /// Whether a `ref` was expected (the help suggests passing one).
        expected_ref: bool,
    },
}

pub(crate) struct Checker<'a> {
    pub res: &'a Resolved,
    pub types: &'a mut Types,
    pub diags: Sink<'a>,
    pub native_bits: u32,
    /// Declared signatures by definition (templates for generic ones).
    pub sigs: HashMap<DefId, FuncSig>,
    /// Definitions whose signature failed (callers stay silent).
    pub failed_sigs: HashSet<DefId>,
    /// The concrete hook table: name to overloads.
    pub hooks: HashMap<String, Vec<Hook>>,
    /// Every requested function instance.
    pub instances: HashMap<FuncKey, Instance>,
    /// Instances whose bodies are still to be checked.
    pub queue: VecDeque<FuncKey>,
    /// Concrete object/enum types seen so far.
    pub type_states: HashMap<TypeId, TypeState>,
    /// Types whose layout is being computed (innermost last).
    pub type_stack: Vec<TypeId>,
    /// Concrete array types met so far, with a span that names each, for
    /// the element hook check.
    pub arrays: Vec<(TypeId, Span)>,
    /// Declared types of module constants.
    pub const_types: HashMap<DefId, TypeRef>,
    /// Output being built.
    pub out: HirProgram,
    /// Current environment.
    pub env: Env,
    /// Generic functions with a concrete instance, whose errors that
    /// instance reports (so a template check of them stays silent).
    pub instantiated: HashSet<DefId>,
    /// `E042`s already reported, by instance (one per instance).
    pub reported: HashSet<FuncKey>,
    /// Every site that requested each instance: the requesting instance
    /// (`None` outside a function body) and the use. `E042` traces them.
    pub origins: HashMap<FuncKey, Vec<(Option<FuncKey>, Span)>>,
    /// `E042`s waiting for the worklist to drain, so their traces list every
    /// origin, also ones requested after the failing body was checked.
    pub failures: Vec<(FuncKey, Diagnostic)>,
    /// [`Options::require_entry`].
    pub require_entry: bool,
    /// Set when the `@entry` body being checked names a missing root item
    /// outside an executable, so the hook is dropped rather than reported.
    pub entry_unresolved: bool,
}

/// Production instantiation depth cap; the test cap keeps `E051` cheap.
#[cfg(not(test))]
pub(crate) const MAX_DEPTH: usize = 16;
#[cfg(test)]
pub(crate) const MAX_DEPTH: usize = 4;

impl Checker<'_> {
    fn run(&mut self) {
        self.collect_hooks();
        self.check_types();
        self.check_consts();
        for id in self.res.ids() {
            let def = self.res.def(id);
            let generic = !def.generic_params.is_empty()
                || def
                    .parent
                    .is_some_and(|parent| !self.res.def(parent).generic_params.is_empty());
            if matches!(def.kind, DefKind::Func(_)) && !generic {
                let span = def.span;
                let _ = self.request(FuncKey::plain(id), span);
            }
        }
        self.drain_queue();
        self.finish_type_hooks();
        self.check_array_hooks();
        self.drain_queue();
    }

    /// Checks every queued instance, then emits the `E042`s they produced.
    fn drain_queue(&mut self) {
        while let Some(key) = self.queue.pop_front() {
            self.check_instance(&key);
        }
        self.emit_failures();
    }

    /// Checks each generic function no instance reached, with its generic
    /// parameters as abstract `T`s: a best-effort check of code nothing
    /// uses yet, which also gives the editor its locals, uses and expression
    /// types. An error about a type containing a parameter is dropped (a
    /// duck-typed `T.member` cannot be proven without arguments); every
    /// other error is reported ([`Self::template_failure`]). The output is
    /// kept apart from the lowered program.
    fn check_templates(&mut self) -> Vec<HirFunc> {
        let funcs = self.out.funcs.len();
        let layouts: HashSet<TypeId> = self.out.layouts.keys().copied().collect();
        self.instantiated = self.instances.keys().map(|key| key.def).collect();
        for id in self.res.ids() {
            let def = self.res.def(id);
            if !matches!(def.kind, DefKind::Func(_)) || self.instantiated.contains(&id) {
                continue;
            }
            let args = self.template_args(id);
            if args.is_empty() {
                continue;
            }
            let span = def.span;
            let _ = self.request(FuncKey { def: id, args }, span);
            self.drain_queue();
        }
        self.out.layouts.retain(|ty, _| layouts.contains(ty));
        let templates = self.out.funcs.split_off(funcs);
        for func in &templates {
            if self.reports_template(&func.key) {
                self.check_template_indices(func);
            }
        }
        templates
    }

    /// Whether template instance `key` reports its errors: it is the
    /// template instance of a generic nothing instantiates. An instance at
    /// other parameters (another template's call), or of an instantiated
    /// generic, would repeat errors reported elsewhere.
    pub(crate) fn reports_template(&mut self, key: &FuncKey) -> bool {
        !key.args.is_empty()
            && !self.instantiated.contains(&key.def)
            && key.args == self.template_args(key.def)
    }

    /// `def`'s generic parameters as abstract types: the arguments of its
    /// template instance (empty when `def` is not generic).
    pub(crate) fn template_args(&mut self, def: DefId) -> Vec<TypeRef> {
        self.scope_of(def)
            .iter()
            .zip(0u32..)
            .map(|(name, index)| TypeRef::value(self.types.intern_param(index, name)))
            .collect()
    }

    // ---- environment ---------------------------------------------------

    /// The file the current environment resolves names in.
    pub(crate) fn file(&self) -> FileId {
        self.env.file.unwrap_or(FileId(u32::MAX))
    }

    /// Runs `f` in `env`, restoring the previous environment after.
    pub(crate) fn with_env<T>(&mut self, env: Env, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = std::mem::replace(&mut self.env, env);
        let result = f(self);
        self.env = saved;
        result
    }

    /// The environment of definition `def` instantiated at `args`.
    pub(crate) fn env_of(&self, def: DefId, args: Vec<TypeRef>, key: Option<FuncKey>) -> Env {
        let d = self.res.def(def);
        let owner = if d.is_type() { Some(def) } else { d.parent };
        Env {
            file: Some(d.file),
            params: self.scope_of(def),
            args,
            owner,
            key,
        }
    }

    /// The generic parameter names in scope for `def`: its owning type's,
    /// then its own.
    pub(crate) fn scope_of(&self, def: DefId) -> Vec<String> {
        let d = self.res.def(def);
        let mut scope = match d.parent {
            Some(parent) if !d.is_type() => self.res.def(parent).generic_params.clone(),
            _ => Vec::new(),
        };
        scope.extend(d.generic_params.iter().cloned());
        scope
    }

    // ---- instances -----------------------------------------------------

    /// Requests instance `key` (from the instance being checked, at
    /// `use_span`): computes its concrete signature and queues its body.
    /// `Err` when the signature failed, now (reported) or before (silent: it
    /// was reported then); a body that failed still gives its signature.
    pub(crate) fn request(
        &mut self,
        key: FuncKey,
        use_span: Span,
    ) -> Result<FuncSig, ErrorGuaranteed> {
        let origin = (self.env.key.clone(), use_span);
        let origins = self.origins.entry(key.clone()).or_default();
        if !origins.contains(&origin) {
            origins.push(origin);
        }
        if let Some(instance) = self.instances.get(&key) {
            return match instance.state {
                // Reported when it became unusable.
                State::Unusable => Err(self.diags.already_reported()),
                _ => Ok(instance.sig.clone()),
            };
        }
        let template = self.sig_of(key.def)?;
        let sig = FuncSig {
            params: template
                .params
                .iter()
                .map(|ty| self.types.substitute(*ty, &key.args))
                .collect(),
            returns: template
                .returns
                .iter()
                .map(|ty| self.types.substitute(*ty, &key.args))
                .collect(),
        };
        let origin_key = self.env.key.clone();
        let parent_depth = origin_key
            .as_ref()
            .and_then(|parent| self.instances.get(parent))
            .map_or(0, |parent| parent.depth);
        let depth = parent_depth + usize::from(!key.args.is_empty());
        let mut instance = Instance {
            sig: sig.clone(),
            origin: Some((origin_key, use_span)),
            depth,
            state: State::Queued,
        };
        if depth > MAX_DEPTH {
            instance.state = State::Unusable;
            self.instances.insert(key.clone(), instance);
            let chain = self.chain_text(&key);
            return Err(self.diags.emit(
                Diagnostic::error(
                    codes::E051,
                    "generic instantiation is too deep (possible infinite recursion)",
                )
                .label(use_span, true, "instantiated here")
                .note(format!("instantiation chain: {chain}")),
            ));
        }
        // Every type the signature mentions must exist before the body runs.
        for ty in sig.params.iter().chain(sig.returns.iter()) {
            if let Err(error) = self.ensure_type(*ty, use_span) {
                instance.state = State::Unusable;
                self.instances.insert(key, instance);
                return Err(error);
            }
        }
        self.instances.insert(key.clone(), instance);
        self.queue.push_back(key);
        Ok(sig)
    }

    /// The declared signature of a function or hook (a template when generic).
    pub(crate) fn sig_of(&mut self, def: DefId) -> Result<FuncSig, ErrorGuaranteed> {
        if let Some(sig) = self.sigs.get(&def) {
            return Ok(sig.clone());
        }
        if self.failed_sigs.contains(&def) {
            // Reported when it first failed.
            return Err(self.diags.already_reported());
        }
        if let Some(sig) = self.collect_sig(def) {
            self.sigs.insert(def, sig.clone());
            Ok(sig)
        } else {
            // `collect_sig` reported each unresolved type.
            self.failed_sigs.insert(def);
            Err(self.diags.already_reported())
        }
    }

    // ---- name lookup ---------------------------------------------------

    /// Whether the body being checked is the top-level `@entry` hook, whose
    /// unfound names fall back to the program's root namespace (so a
    /// runtime library's `@entry` can call the program's `main`).
    pub(crate) fn in_entry(&self) -> bool {
        self.env.key.as_ref().is_some_and(|key| {
            let def = self.res.def(key.def);
            def.name == "@entry" && def.parent.is_none() && matches!(def.kind, DefKind::Hook(_))
        })
    }

    /// The error for a name `@entry` finds nowhere: `E032` in an
    /// executable; otherwise nothing, and the hook is dropped
    /// ([`Options::require_entry`]).
    pub(crate) fn entry_unresolved(&mut self, name: &str, span: Span) -> ErrorGuaranteed {
        if !self.require_entry {
            self.entry_unresolved = true;
            return ErrorGuaranteed::unreported(Unreported::DroppedEntry);
        }
        self.diags.emit(
            Diagnostic::error(
                codes::E032,
                format!("the program has no `{name}`, which `@entry` calls"),
            )
            .label(span, true, format!("`@entry` calls `{name}` here"))
            .note(format!(
                "`@entry` looks names up in the program's root namespace; define `{name}` in a file at the project root"
            )),
        )
    }

    // ---- diagnostics ---------------------------------------------------

    /// Emits a diagnostic that also records a duck-typing requirement.
    /// `involved` are the types the failure is about ([`Self::emit_involving`]).
    pub(crate) fn emit_requirement(
        &mut self,
        diagnostic: Diagnostic,
        requirement: Requirement,
        involved: &[TypeId],
    ) -> ErrorGuaranteed {
        let dependent = self.involves_param(involved);
        self.diags.push(Pending {
            diagnostic,
            requirement: Some(requirement),
            dependent,
        })
    }

    /// Emits a diagnostic about `involved` types, marked dependent when one
    /// contains a generic parameter (so a template check, which cannot know
    /// the parameter, drops it).
    pub(crate) fn emit_involving(
        &mut self,
        diagnostic: Diagnostic,
        involved: &[TypeId],
    ) -> ErrorGuaranteed {
        let dependent = self.involves_param(involved);
        self.diags.push(Pending {
            diagnostic,
            requirement: None,
            dependent,
        })
    }

    fn involves_param(&self, involved: &[TypeId]) -> bool {
        involved.iter().any(|ty| self.types.contains_param(*ty))
    }

    /// `E010` with both sides labelled.
    pub(crate) fn mismatch(
        &mut self,
        expected: TypeRef,
        found: TypeRef,
        expected_span: Span,
        found_span: Span,
    ) -> ErrorGuaranteed {
        let exp = self.types.display_ref(expected);
        let got = self.types.display_ref(found);
        let diagnostic = Diagnostic::error(
            codes::E010,
            format!("mismatched types: expected {exp}, found {got}"),
        )
        .label(expected_span, false, format!("expected `{exp}` here"))
        .label(found_span, true, format!("found `{got}` here"));
        self.emit_requirement(
            diagnostic,
            Requirement::Mismatch {
                expected_ref: expected.is_ref,
            },
            &[expected.id, found.id],
        )
    }

    /// Records an item use for the editor.
    pub(crate) fn record_use(&mut self, span: Span, def: DefId) {
        if def != DefId::ERROR && !span.is_dummy() {
            self.out.uses.push(lilac_hir::NameSite { span, def });
        }
    }

    /// Records a use of a `using … as` alias for the editor: the path head at
    /// `span`, declared at `decl`.
    pub(crate) fn record_alias_use(&mut self, span: Span, decl: Span) {
        if !span.is_dummy() && !decl.is_dummy() {
            self.out
                .alias_uses
                .push(lilac_hir::AliasSite { span, decl });
        }
    }

    /// Records a semantic-token site for the editor.
    pub(crate) fn record_semantic(&mut self, span: Span, kind: lilac_hir::SemanticKind) {
        if !span.is_dummy() {
            self.out
                .semantics
                .push(lilac_hir::SemanticSite { span, kind });
        }
    }

    /// Records a local name for the editor: a use at `span` of `local`
    /// (its declaration when `span` is the declared name).
    pub(crate) fn record_local(
        &mut self,
        fcx: &crate::body::FnCx,
        local: lilac_hir::LocalId,
        span: Span,
    ) {
        let decl = fcx.decl_span(local);
        if !span.is_dummy() && !decl.is_dummy() {
            self.out.locals.push(lilac_hir::LocalSite {
                span,
                decl,
                ty: fcx.ty(local),
                scope: fcx.scope_of(local),
                kind: fcx.kind_of(local),
            });
        }
    }

    // ---- small type helpers --------------------------------------------

    pub(crate) fn is_bit(&self, id: TypeId) -> bool {
        matches!(
            self.types.kind(id),
            TypeKind::Bit8
                | TypeKind::Bit16
                | TypeKind::Bit32
                | TypeKind::Bit64
                | TypeKind::BitNative
        )
    }

    pub(crate) fn is_float(&self, id: TypeId) -> bool {
        matches!(
            self.types.kind(id),
            TypeKind::FBit16 | TypeKind::FBit32 | TypeKind::FBit64 | TypeKind::FBit128
        )
    }

    pub(crate) fn bit_width(&self, id: TypeId) -> u32 {
        match self.types.kind(id) {
            TypeKind::Bit8 => 8,
            TypeKind::Bit16 => 16,
            TypeKind::Bit32 => 32,
            TypeKind::Bit64 => 64,
            TypeKind::BitNative => self.native_bits,
            _ => lilac_error::bug!(Span::dummy(), "bit width of a non-bit type"),
        }
    }

    pub(crate) fn float_width(&self, id: TypeId) -> u32 {
        match self.types.kind(id) {
            TypeKind::FBit16 => 16,
            TypeKind::FBit32 => 32,
            TypeKind::FBit64 => 64,
            TypeKind::FBit128 => 128,
            _ => lilac_error::bug!(Span::dummy(), "float width of a non-float type"),
        }
    }

    pub(crate) fn bit_of_width(width: u32) -> TypeId {
        match width {
            8 => Builtin::Bit8,
            16 => Builtin::Bit16,
            32 => Builtin::Bit32,
            _ => Builtin::Bit64,
        }
        .id()
    }

    /// Every concrete `@conversion` as `(from, to)`.
    pub(crate) fn conversion_pairs(&self) -> Vec<(TypeId, TypeId)> {
        self.hooks
            .get("@conversion")
            .map(|hooks| {
                hooks
                    .iter()
                    .filter(|hook| hook.sig.params.len() == 1 && hook.sig.returns.len() == 1)
                    .map(|hook| (hook.sig.params[0].id, hook.sig.returns[0].id))
                    .collect()
            })
            .unwrap_or_default()
    }
}
