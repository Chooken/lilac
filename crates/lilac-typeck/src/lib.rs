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

mod aliases;
mod arrays;
mod body;
mod calls;
mod expr;
mod failure;
mod formats;
mod generics;
mod indices;
mod layout;
mod literals;
mod memory;
mod own;
mod paths;
mod places;
mod sigs;
mod sink;
mod suggest;
mod types;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use lilac_ast::Expr;
use lilac_error::{codes, Diagnostic, Diags, ErrorGuaranteed, Unreported};
use lilac_hir::{FuncKey, HirExpr, HirFunc, HirProgram};
use lilac_resolve::{DefId, DefKind, Resolved};
use lilac_span::{FileId, Span};
use lilac_target::Target;
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
#[derive(Clone, Debug)]
pub struct Options {
    /// The target: `@bit_native`'s width (its literal range) and every
    /// type's layout, which must fit it (`E067`).
    pub target: Target,
    /// Whether the unit is an executable (`lilac build`): a name `@entry`
    /// cannot find in the root namespace is then `E032`. Otherwise an
    /// `@entry` that names a missing root item is dropped silently, since
    /// nothing enters the program.
    pub require_entry: bool,
    /// Stops the check when set (a newer compile is due): the worklist
    /// checks it before each instance and the template pass before each
    /// template, and the output is then incomplete. `None`: never.
    pub cancel: Option<Arc<AtomicBool>>,
}

/// Checks every definition in `res`, generic functions nothing instantiates
/// included (as templates).
pub fn check(res: &Resolved, types: &mut Types, diags: &mut Diags, options: Options) -> Checked {
    let mut checker = Checker::new(res, types, diags, options);
    checker.run();
    let templates = checker.check_templates();
    let sigs = checker.sigs.clone();
    Checked {
        hir: std::mem::take(&mut checker.out),
        sigs,
        templates,
    }
}

/// What [`check_selectors`] produced.
#[derive(Clone, Debug, Default)]
pub struct Selectors {
    /// What evaluating the selectors needs: constants, the bodies of the
    /// `=>` functions they call, and layouts.
    pub hir: HirProgram,
    /// Each selector checked, or `None` when checking it reported an error.
    pub values: Vec<Option<HirExpr>>,
}

/// Checks the selector of each top-level conditional: an expression in
/// its file's scope whose `@bit32` value is the index of the selected arm.
/// Only what the selectors reach is checked (plus the hooks and
/// constants), so code naming items of undecided conditionals stays out
/// of it.
pub fn check_selectors(
    res: &Resolved,
    types: &mut Types,
    diags: &mut Diags,
    options: Options,
    selectors: &[(FileId, Expr)],
) -> Selectors {
    let mut checker = Checker::new(res, types, diags, options);
    // Only what a selector reaches is checked: every hook is parked
    // (`@entry` reaches the whole program) and queued when something checked
    // uses it.
    checker.park_hooks = true;
    checker.collect_hooks();
    checker.check_consts();
    checker.drain_queue();
    let ty = TypeRef::value(Builtin::Bit32.id());
    let mut values = Vec::with_capacity(selectors.len());
    for (file, expr) in selectors {
        let before = checker.diags.error_count();
        let empty_before = checker.empty_names;
        let env = Env {
            file: Some(*file),
            ..Env::default()
        };
        let hir = checker.with_env(env, |c| {
            let mut fcx = body::FnCx::new(Span::dummy());
            let hir = c.check_expr(&mut fcx, expr, Some(ty.id));
            c.coerce(hir, ty, expr.span())
        });
        checker.drain_queue();
        // A tag or member still being typed leaves the selector undecided
        // (the parser reported it).
        let decided = checker.diags.error_count() == before && checker.empty_names == empty_before;
        values.push(decided.then_some(hir));
    }
    Selectors {
        hir: std::mem::take(&mut checker.out),
        values,
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
    /// Signature registered; the body waits for its first use
    /// ([`Checker::use_hook`]), which queues it.
    Parked,
    /// Body checked successfully.
    Done,
    /// Body has an error. Callers are still checked against the signature,
    /// so their own errors are reported too.
    BodyFailed,
    /// The instance cannot be called at all (a signature type failed, its
    /// definition recursed too deep (`E051`), the instance budget ran out
    /// (`E068`), or a dropped `@entry`); later uses fail with the same
    /// error and stay silent.
    Unusable(ErrorGuaranteed),
}

/// One function instance.
#[derive(Clone, Debug)]
pub(crate) struct Instance {
    /// Concrete signature.
    pub sig: FuncSig,
    /// The instance whose body requested this one, and the use site.
    pub origin: Option<(Option<FuncKey>, Span)>,
    /// How many times each generic definition appears on the instance's
    /// first-origin chain, its own included (`E051` past
    /// [`MAX_RECURSION`]). Empty for a non-generic instance: a chain
    /// starts again at one.
    pub recursion: Vec<(DefId, u32)>,
    /// How many generic instances the pass had built before this one (the
    /// order `E068` names them in).
    pub serial: usize,
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
    /// An ownership error (`E063`, `E064`): a type that cannot be copied
    /// was moved where it may not be.
    Ownership,
    /// A type the instance lays out is larger than the target allows
    /// (`E067`).
    Size,
    /// A hole has no `@format` for its type and the format's `W` (`E071`).
    Format,
    /// A format where only locals and parameters may hold one (`E072`).
    FormatPlace,
}

pub(crate) struct Checker<'a> {
    pub res: &'a Resolved,
    pub types: &'a mut Types,
    pub diags: Sink<'a>,
    pub target: Target,
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
    /// Whether every hook is parked until something checked uses it, not
    /// only the non-RAII hooks of generic instances ([`check_selectors`]).
    pub park_hooks: bool,
    /// Concrete object/enum types seen so far.
    pub type_states: HashMap<TypeId, TypeState>,
    /// The size and alignment of each concrete object, enum and array type
    /// laid out so far (`layout.rs`).
    pub sizes: HashMap<TypeId, lilac_layout::SizeAlign>,
    /// Types whose layout is being computed (innermost last).
    pub type_stack: Vec<TypeId>,
    /// How many times each definition is on [`Checker::type_stack`].
    pub type_counts: HashMap<DefId, u32>,
    /// Each type alias met so far ([`aliases::AliasState`]).
    pub(crate) aliases: HashMap<DefId, aliases::AliasState>,
    /// The aliases being resolved, innermost last (for `E074`'s chain).
    pub(crate) alias_stack: Vec<DefId>,
    /// Concrete array types met so far, with a span that names each, for
    /// the element hook check.
    pub arrays: Vec<(TypeId, Span)>,
    /// The types in [`Checker::arrays`].
    pub noted_arrays: HashSet<TypeId>,
    /// Function and array types `ensure_type` has made sure of (objects and
    /// enums are in [`Checker::type_states`]).
    pub ensured: HashSet<TypeId>,
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
    /// How many tags and members with no name (a `.` mid-edit, which the
    /// parser reported) were met; a selector that meets one is not decided.
    pub empty_names: usize,
    /// [`Checker::is_copyable`] per laid-out type.
    pub copyable: HashMap<TypeId, bool>,
    /// [`Checker::needs_drop`] per laid-out type.
    pub droppable: HashMap<TypeId, bool>,
    /// [`Options::cancel`].
    pub cancel: Option<Arc<AtomicBool>>,
    /// Generic instances built so far in this pass (`E068` past
    /// [`MAX_INSTANCES`]); the template pass counts its own.
    pub generic_instances: usize,
    /// The `E068` that stopped instantiating, once the budget ran out:
    /// every later new instance fails with it.
    pub instance_limit: Option<ErrorGuaranteed>,
    /// Instances recursive requests put at the front of the queue while
    /// the current body is checked ([`Checker::request`]).
    pub front: usize,
    /// Each generic definition that recursed too deep (`E051`), with whether
    /// the chain was a template's: a later recursive request of it fails
    /// rather than starting the descent again.
    pub recursion_limited: HashSet<(DefId, bool)>,
    /// Each definition whose layout recursed too deep (`E051`): a later
    /// layout of it inside itself fails.
    pub type_recursion_limited: HashSet<DefId>,
    /// The first recursion `E051`, which those later failures carry.
    pub recursion_error: Option<ErrorGuaranteed>,
}

/// How many times one generic definition may appear on an instantiation
/// chain (a function instance's first-origin chain, or the types being laid
/// out): past it is `E051`. As rustc does, recursion is counted per
/// definition, so a chain of distinct generics is never too deep.
pub(crate) const MAX_RECURSION: u32 = 64;

/// How deep types may nest while one is laid out (`E051` past it), whatever
/// they are: laying out recurses into each field's type, so this bounds the
/// native stack. Far above real programs (the limit was 16 before recursion
/// was counted per definition) and far below what
/// `lilac_driver::COMPILE_STACK_BYTES` covers in a debug build (it
/// overflows near 19,000 levels).
pub(crate) const MAX_TYPE_NESTING: usize = 1024;

/// How many generic function instances one pass of a check builds at most
/// (`E068` past it). `lib/Standard` and the test suite need at most 9 (18
/// in the template pass), and a release build reaches it in about 0.5 s
/// (1.3 s with ten-statement bodies); `docs/errors/E068.md` has the
/// measurements.
pub(crate) const MAX_INSTANCES: usize = 50_000;

impl<'a> Checker<'a> {
    fn new(
        res: &'a Resolved,
        types: &'a mut Types,
        diags: &'a mut Diags,
        options: Options,
    ) -> Self {
        Self {
            res,
            types,
            diags: Sink::new(diags),
            target: options.target,
            require_entry: options.require_entry,
            entry_unresolved: false,
            empty_names: 0,
            sigs: HashMap::new(),
            failed_sigs: HashSet::new(),
            hooks: HashMap::new(),
            instances: HashMap::new(),
            queue: VecDeque::new(),
            park_hooks: false,
            type_states: HashMap::new(),
            sizes: HashMap::new(),
            type_stack: Vec::new(),
            type_counts: HashMap::new(),
            aliases: HashMap::new(),
            alias_stack: Vec::new(),
            arrays: Vec::new(),
            noted_arrays: HashSet::new(),
            ensured: HashSet::new(),
            const_types: HashMap::new(),
            out: HirProgram::default(),
            env: Env::default(),
            instantiated: HashSet::new(),
            reported: HashSet::new(),
            origins: HashMap::new(),
            failures: Vec::new(),
            copyable: HashMap::new(),
            droppable: HashMap::new(),
            cancel: options.cancel,
            generic_instances: 0,
            instance_limit: None,
            front: 0,
            recursion_limited: HashSet::new(),
            type_recursion_limited: HashSet::new(),
            recursion_error: None,
        }
    }
}

impl Checker<'_> {
    fn run(&mut self) {
        self.resolve_aliases();
        self.collect_hooks();
        self.out.panic_message = self.panic_hook_message();
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
        if self.cancelled() {
            return;
        }
        self.finish_type_hooks();
        self.check_array_hooks();
        self.drain_queue();
    }

    /// Checks every queued instance, then emits the `E042`s they produced.
    /// A recursive request goes to the front of the queue ([`Self::request`]),
    /// so recursion is followed depth first and meets `E051` after
    /// [`MAX_RECURSION`] bodies, however many calls each level makes.
    /// A cancelled check ([`Options::cancel`]) drops the rest of the queue.
    fn drain_queue(&mut self) {
        while let Some(key) = self.queue.pop_front() {
            if self.cancelled() {
                self.queue.clear();
                return;
            }
            self.front = 0;
            self.check_instance(&key);
        }
        self.emit_failures();
    }

    /// Whether [`Options::cancel`] is set: the check stops, incomplete.
    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
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
        // A parked hook's body was never checked, so it leaves its template
        // to be.
        self.instantiated = self
            .instances
            .iter()
            .filter(|(_, instance)| instance.state != State::Parked)
            .map(|(key, _)| key.def)
            .collect();
        // A budget of its own: what the program leaves unused never takes
        // instances from what it uses, and the reverse.
        self.generic_instances = 0;
        self.instance_limit = None;
        for id in self.res.ids() {
            if self.cancelled() {
                break;
            }
            let def = self.res.def(id);
            // A member hook of a generic type too: its instances are parked
            // until used, so one no instance used is checked here.
            let member_hook = matches!(def.kind, DefKind::Hook(_))
                && def.generic_params.is_empty()
                && def
                    .parent
                    .is_some_and(|parent| !self.res.def(parent).generic_params.is_empty());
            if !(matches!(def.kind, DefKind::Func(_)) || member_hook)
                || self.instantiated.contains(&id)
            {
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
    ///
    /// Two limits keep instantiation finite and fast. A generic definition
    /// may appear [`MAX_RECURSION`] times on an instance's chain (`E051`),
    /// and a recursive instance (its definition already on the chain) is
    /// checked next, so recursion is followed depth first: it meets `E051`
    /// after [`MAX_RECURSION`] bodies even when each level requests several
    /// instances, and every later recursive request of that definition
    /// fails with the same error. A pass builds at most [`MAX_INSTANCES`]
    /// generic instances (`E068`), which bounds wide fan-out that never
    /// recurses.
    pub(crate) fn request(
        &mut self,
        key: FuncKey,
        use_span: Span,
    ) -> Result<FuncSig, ErrorGuaranteed> {
        self.instantiate(key, use_span, false)
    }

    /// Registers hook instance `key` (met at `span`) with its body
    /// [`State::Parked`]: the signature is known, so lookups find it, and
    /// the body is checked on its first use ([`Self::use_hook`]). That use,
    /// not `span`, is the instance's origin, so a body that fails is
    /// `E042` there.
    pub(crate) fn park(&mut self, key: FuncKey, span: Span) -> Result<FuncSig, ErrorGuaranteed> {
        self.instantiate(key, span, true)
    }

    /// A use of hook instance `key` at `span` (a hook call, an operator, a
    /// conversion): queues its body if it was parked, and records the use
    /// as an origin of a generic instance.
    pub(crate) fn use_hook(&mut self, key: &FuncKey, span: Span) {
        let parked = self
            .instances
            .get(key)
            .is_some_and(|instance| instance.state == State::Parked);
        if parked || !key.args.is_empty() {
            let _ = self.request(key.clone(), span);
        }
    }

    /// [`Self::request`], or [`Self::park`] when `park` is set.
    fn instantiate(
        &mut self,
        key: FuncKey,
        use_span: Span,
        park: bool,
    ) -> Result<FuncSig, ErrorGuaranteed> {
        if !park {
            let origin = (self.env.key.clone(), use_span);
            let origins = self.origins.entry(key.clone()).or_default();
            if !origins.contains(&origin) {
                origins.push(origin);
            }
        }
        if let Some(found) = self.existing(&key, use_span, park) {
            return found;
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
        let recursion = if key.args.is_empty() {
            Vec::new()
        } else {
            let mut counts = origin_key
                .as_ref()
                .and_then(|parent| self.instances.get(parent))
                .map(|parent| parent.recursion.clone())
                .unwrap_or_default();
            match counts.iter_mut().find(|(def, _)| *def == key.def) {
                Some((_, count)) => *count += 1,
                None => counts.push((key.def, 1)),
            }
            counts
        };
        let count = recursion
            .iter()
            .find(|(def, _)| *def == key.def)
            .map_or(0, |(_, count)| *count);
        let mut instance = Instance {
            sig: sig.clone(),
            origin: Some((origin_key, use_span)),
            recursion,
            serial: self.generic_instances,
            state: State::Queued,
        };
        if !key.args.is_empty() {
            if let Some(error) = self.instance_budget(&key, use_span) {
                instance.state = State::Unusable(error);
                self.instances.insert(key, instance);
                return Err(error);
            }
            self.generic_instances += 1;
        }
        let template_chain = key.args.iter().any(|arg| self.types.contains_param(arg.id));
        if count > 1 && self.recursion_limited.contains(&(key.def, template_chain)) {
            // The descent this definition started already reported `E051`.
            if let Some(error) = self.recursion_error {
                instance.state = State::Unusable(error);
                self.instances.insert(key, instance);
                return Err(error);
            }
        }
        if count > MAX_RECURSION {
            let parent = self.env.key.clone();
            let error = self.recursion_error(&key, parent.as_ref(), use_span);
            instance.state = State::Unusable(error);
            self.instances.insert(key.clone(), instance);
            self.recursion_limited.insert((key.def, template_chain));
            self.recursion_error.get_or_insert(error);
            return Err(error);
        }
        // Every type the signature mentions must exist before the body runs.
        for ty in sig.params.iter().chain(sig.returns.iter()) {
            if let Err(error) = self.ensure_type(*ty, use_span) {
                instance.state = State::Unusable(error);
                self.instances.insert(key, instance);
                return Err(error);
            }
        }
        if park {
            instance.state = State::Parked;
            self.instances.insert(key, instance);
            return Ok(sig);
        }
        self.instances.insert(key.clone(), instance);
        self.enqueue(key, count);
        Ok(sig)
    }

    /// What [`Self::instantiate`] gives for an instance already built
    /// (`None` when there is none): its signature, after queuing it when
    /// it was parked and this is a use.
    fn existing(
        &mut self,
        key: &FuncKey,
        use_span: Span,
        park: bool,
    ) -> Option<Result<FuncSig, ErrorGuaranteed>> {
        let origin = self.env.key.clone();
        let instance = self.instances.get_mut(key)?;
        Some(match instance.state {
            State::Unusable(error) => Err(error),
            State::Parked if !park => {
                instance.state = State::Queued;
                instance.origin = Some((origin, use_span));
                let sig = instance.sig.clone();
                let count = instance
                    .recursion
                    .iter()
                    .find(|(def, _)| *def == key.def)
                    .map_or(0, |(_, count)| *count);
                self.enqueue(key.clone(), count);
                Ok(sig)
            }
            _ => Ok(instance.sig.clone()),
        })
    }

    /// Queues `key`'s body: next when its definition is already on its
    /// chain (`count > 1`), so recursion runs depth first, else last.
    fn enqueue(&mut self, key: FuncKey, count: u32) {
        if count > 1 {
            self.queue.insert(self.front.min(self.queue.len()), key);
            self.front += 1;
        } else {
            self.queue.push_back(key);
        }
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
        let exp = self.show(expected);
        let got = self.show(found);
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

    /// Records an f-string hole (`span`, braces included) and the hook that
    /// formats it, for the editor.
    pub(crate) fn record_hole(&mut self, span: Span, hole: &lilac_hir::HirHole) {
        if !span.is_dummy() {
            self.out.holes.push(lilac_hir::HoleSite {
                span,
                value: hole.value.span,
                ty: hole.value.ty.id,
                hook: hole.format.def,
            });
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

    /// Records a tag at `span` for the editor: the variants of `expected`
    /// (when it is an enum) other than `taken`.
    pub(crate) fn record_tag(&mut self, span: Span, expected: Option<TypeId>, taken: &[&str]) {
        if span.is_dummy() {
            return;
        }
        let variants = expected
            .and_then(|ty| self.variants_of(ty))
            .unwrap_or_default()
            .into_iter()
            .filter(|(name, _)| !taken.contains(&name.as_str()))
            .map(|(name, payload)| (name, payload.map(|payload| self.show(payload))))
            .collect();
        self.out.tags.push(lilac_hir::TagSite { span, variants });
    }

    /// Records the member name `field` ending `access` for the editor,
    /// with what it is looked up in (`None`: nothing completion lists).
    /// `Err` when the name is empty (a `.` mid-edit, which the parser
    /// reported): the access goes no further.
    pub(crate) fn record_member(
        &mut self,
        access: Span,
        field: &str,
        receiver: Option<lilac_hir::MemberReceiver>,
    ) -> Result<(), ErrorGuaranteed> {
        if let Some(receiver) = receiver.filter(|_| !access.is_dummy()) {
            self.out.members.push(lilac_hir::MemberSite {
                span: crate::expr::tail_span(access, field),
                receiver,
            });
        }
        if field.is_empty() {
            // The parser reported the `.` with no name.
            self.empty_names += 1;
            return Err(self.diags.already_reported());
        }
        Ok(())
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
            TypeKind::BitNative => self.target.bit_native_width(),
            _ => lilac_error::bug!(Span::dummy(), "bit width of a non-bit type"),
        }
    }

    /// The IEEE format of float type `id`.
    pub(crate) fn float_format(&self, id: TypeId) -> lilac_float::FloatWidth {
        match self.types.kind(id) {
            TypeKind::FBit16 => lilac_float::FloatWidth::W16,
            TypeKind::FBit32 => lilac_float::FloatWidth::W32,
            TypeKind::FBit64 => lilac_float::FloatWidth::W64,
            TypeKind::FBit128 => lilac_float::FloatWidth::W128,
            _ => lilac_error::bug!(Span::dummy(), "float format of a non-float type"),
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
