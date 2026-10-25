//! Diagnostics for generic instances: `E042` (an instance body failed a
//! duck-typing requirement), the instantiation limits (`E051`: a definition
//! recursing too deep, `E068`: too many instances) and the chains they show.
//!
//! An instance body is checked like any other; its diagnostics are captured
//! and, when any is a recorded [`Requirement`], folded into one `E042` at the
//! use that requested the instance. Requirements are recorded when emitted, so
//! nothing here parses diagnostic text. A requirement the generic's template
//! fails too, about no parameter, is the body's own error, reported as it is
//! ([`Checker::emit_failures`]).

use std::collections::{HashMap, HashSet};

use lilac_error::{codes, Diagnostic, ErrorGuaranteed, Level, TraceFrame};
use lilac_hir::FuncKey;
use lilac_resolve::DefId;
use lilac_span::Span;
use lilac_ty::{TypeId, TypeKind, TypeRef, DISPLAY_LIMIT};

use crate::sink::{has_error, Pending};
use crate::{Checker, Requirement, MAX_INSTANCES, MAX_RECURSION};

/// A failed generic instance's requirements, waiting for the end of the
/// check ([`Checker::emit_failures`]).
#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub key: FuncKey,
    requirements: Vec<(Diagnostic, Requirement)>,
    /// The sites that requested the instance, taken once the worklist that
    /// checked it drained ([`Checker::close_failures`]), so a template
    /// checked later adds none.
    callers: Option<Vec<TraceFrame>>,
}

impl Failure {
    /// One of the errors the failure holds, which an `E042` or its own
    /// report will cover.
    pub(crate) fn held(&self) -> Option<&Diagnostic> {
        self.requirements.first().map(|(diagnostic, _)| diagnostic)
    }
}

/// What matches an error of an instance body with its template's: its code
/// and primary span.
pub(crate) fn error_site(diagnostic: &Diagnostic) -> (&'static str, Span) {
    let span = diagnostic
        .labels
        .iter()
        .find(|label| label.primary)
        .or_else(|| diagnostic.labels.first())
        .map_or_else(Span::dummy, |label| label.span);
    (diagnostic.code, span)
}

impl Checker<'_> {
    /// Source name of a function for diagnostics: `name`, or `Owner.name`.
    pub(crate) fn function_name(&self, def: lilac_resolve::DefId) -> String {
        let d = self.res.def(def);
        match d.parent {
            Some(parent) => format!("{}.{}", self.res.def(parent).name, d.name),
            None => d.name.clone(),
        }
    }

    /// A type as diagnostics and the editor show it ([`lilac_ty::Types::display_ref`]),
    /// cut at [`DISPLAY_LIMIT`].
    pub(crate) fn show(&self, ty: TypeRef) -> String {
        self.types.display_bounded(ty, DISPLAY_LIMIT)
    }

    /// [`Self::show`] for a type by value.
    pub(crate) fn show_id(&self, id: TypeId) -> String {
        self.show(TypeRef::value(id))
    }

    /// `name[@arg, ...]` for an instance (`name` alone when not generic).
    pub(crate) fn instance_display(&self, key: &FuncKey) -> String {
        let name = self.function_name(key.def);
        if key.args.is_empty() {
            return name;
        }
        let args: Vec<String> = key.args.iter().map(|arg| self.show(*arg)).collect();
        format!("{name}[{}]", args.join(", "))
    }

    /// The generic instances on the first-origin chain that ends at `last`,
    /// outermost first. The walk ends: an instance's first origin was
    /// requested before it, so origins never form a cycle.
    pub(crate) fn chain_keys(&self, last: Option<&FuncKey>) -> Vec<FuncKey> {
        let mut keys = Vec::new();
        let mut current = last.cloned();
        while let Some(key) = current {
            if key.args.is_empty() {
                break;
            }
            current = self
                .instances
                .get(&key)
                .and_then(|instance| instance.origin.as_ref())
                .and_then(|(parent, _)| parent.clone());
            keys.push(key);
        }
        keys.reverse();
        keys
    }

    /// One frame per recorded origin of `key`, a generic requester expanded
    /// once (later frames of it are repeats).
    fn origin_frames(&self, key: &FuncKey, expanded: &mut HashSet<FuncKey>) -> Vec<TraceFrame> {
        let Some(origins) = self.origins.get(key) else {
            return Vec::new();
        };
        origins
            .iter()
            .map(|(parent, span)| {
                let Some(parent) = parent else {
                    return TraceFrame {
                        func: String::new(),
                        span: *span,
                        address: false,
                        repeat: false,
                        callers: Vec::new(),
                    };
                };
                let generic = !parent.args.is_empty();
                let repeat = generic && !expanded.insert(parent.clone());
                let callers = if generic && !repeat {
                    self.origin_frames(parent, expanded)
                } else {
                    Vec::new()
                };
                TraceFrame {
                    func: self.instance_display(parent),
                    span: *span,
                    address: false,
                    repeat,
                    callers,
                }
            })
            .collect()
    }

    /// Takes the callers of each failure the worklist produced: every site
    /// that requested its instance, also ones requested after the failing
    /// body was checked.
    pub(crate) fn close_failures(&mut self) {
        let mut failures = std::mem::take(&mut self.failures);
        for failure in &mut failures {
            if failure.callers.is_none() {
                let mut expanded = HashSet::from([failure.key.clone()]);
                failure.callers = Some(self.origin_frames(&failure.key, &mut expanded));
            }
        }
        self.failures = failures;
    }

    /// Reports the failures: a requirement the generic's template fails too,
    /// about no parameter (`template_errors`), is an error of the body,
    /// reported as the template does; the rest fold into one `E042` per instance, traced
    /// through every site that requested it.
    pub(crate) fn emit_failures(&mut self) {
        self.close_failures();
        for failure in std::mem::take(&mut self.failures) {
            let mut folded = Vec::new();
            for (diagnostic, requirement) in failure.requirements {
                let own = self
                    .template_errors
                    .get(&failure.key.def)
                    .and_then(|own| own.get(&error_site(&diagnostic)))
                    .cloned();
                match own {
                    // In the template's words (`T`, not one argument), so
                    // every instance reports it once.
                    Some(own) => {
                        self.diags.emit(own);
                    }
                    None => folded.push((diagnostic, requirement)),
                }
            }
            if folded.is_empty() || !self.reported.insert(failure.key.clone()) {
                continue;
            }
            let Some(diagnostic) = self.requirement_failure(&failure.key, &folded) else {
                continue;
            };
            let trace = TraceFrame {
                func: self.instance_display(&failure.key),
                span: error_site(&diagnostic).1,
                address: false,
                repeat: false,
                callers: failure.callers.unwrap_or_default(),
            };
            self.diags.emit(diagnostic.trace(trace));
        }
    }

    /// The `E051`/`E068` chain text for `keys` (outermost first).
    pub(crate) fn chain_text(&self, keys: &[FuncKey]) -> String {
        crate::types::elide_chain(keys.len(), |index| self.instance_display(&keys[index]))
    }

    /// `E051` for `key`, requested from `parent` at `use_span`: its
    /// definition would appear past [`MAX_RECURSION`] times on its chain.
    pub(crate) fn recursion_error(
        &mut self,
        key: &FuncKey,
        parent: Option<&FuncKey>,
        use_span: Span,
    ) -> ErrorGuaranteed {
        let mut keys = self.chain_keys(parent);
        keys.push(key.clone());
        let name = self.function_name(key.def);
        let mut diagnostic = Diagnostic::error(
            codes::E051,
            "generic instantiation is too deep (possible infinite recursion)",
        )
        .label(use_span, true, "instantiated here")
        .note(format!("instantiation chain: {}", self.chain_text(&keys)));
        let mut repeats = keys.iter().filter(|chained| chained.def == key.def);
        if let (Some(first), Some(second)) = (repeats.next(), repeats.next()) {
            if self.grew(&first.args, &second.args) {
                diagnostic = diagnostic.note(format!(
                    "the type arguments grow each time: {} -> {}",
                    self.instance_display(first),
                    self.instance_display(second)
                ));
            }
        }
        self.diags.emit(diagnostic.note(format!(
            "a generic may appear at most {MAX_RECURSION} times on one instantiation chain, and `{name}` would appear once more"
        )))
    }

    /// Whether type arguments `to` grew from `from`: they differ, and each
    /// still contains the one it replaced.
    pub(crate) fn grew(&self, from: &[TypeRef], to: &[TypeRef]) -> bool {
        from.len() == to.len()
            && from != to
            && from
                .iter()
                .zip(to)
                .all(|(from, to)| self.mentions(to.id, from.id))
    }

    /// Whether `part` is `ty` or one of the types it is made of. Each type
    /// is visited once, since a type nested `n` deep can be a tree of `2^n`
    /// nodes.
    fn mentions(&self, ty: TypeId, part: TypeId) -> bool {
        let mut seen = HashSet::new();
        let mut stack = vec![ty];
        while let Some(ty) = stack.pop() {
            if ty == part {
                return true;
            }
            if !seen.insert(ty) {
                continue;
            }
            match self.types.kind(ty) {
                TypeKind::Instance { args, .. } => stack.extend(args.iter().map(|arg| arg.id)),
                TypeKind::Array { elem, .. } => stack.push(*elem),
                TypeKind::Func {
                    params, returns, ..
                } => stack.extend(params.iter().chain(returns).map(|slot| slot.id)),
                _ => {}
            }
        }
        false
    }

    /// The `E068` that ends instantiating once the pass has built
    /// [`MAX_INSTANCES`] generic instances: `None` while some are left.
    /// Reported once, at the request that found none left; every later new
    /// instance fails with it.
    pub(crate) fn instance_budget(
        &mut self,
        key: &FuncKey,
        use_span: Span,
    ) -> Option<ErrorGuaranteed> {
        if self.instance_limit.is_some() || self.generic_instances < MAX_INSTANCES {
            return self.instance_limit;
        }
        let mut keys = self.chain_keys(self.env.key.clone().as_ref());
        keys.push(key.clone());
        let mut diagnostic = Diagnostic::error(codes::E068, "too many generic instances")
            .label(
                use_span,
                true,
                format!("`{}` would be one too many", self.instance_display(key)),
            )
            .note(format!(
                "a check builds at most {MAX_INSTANCES} generic instances"
            ));
        for note in self.busiest_definitions() {
            diagnostic = diagnostic.note(note);
        }
        let diagnostic =
            diagnostic.note(format!("instantiation chain: {}", self.chain_text(&keys)));
        let error = self.diags.report_now(diagnostic);
        self.instance_limit = Some(error);
        self.instance_limit
    }

    /// One note for each of the (up to three) definitions with the most
    /// instances: how many, and the first and the latest one built, which
    /// shows how their arguments grow.
    fn busiest_definitions(&self) -> Vec<String> {
        let mut busiest: HashMap<DefId, Built> = HashMap::new();
        for (key, instance) in &self.instances {
            if key.args.is_empty() {
                continue;
            }
            let built = (instance.serial, key);
            let entry = busiest.entry(key.def).or_insert(Built {
                count: 0,
                first: built,
                latest: built,
            });
            entry.count += 1;
            if built.0 < entry.first.0 {
                entry.first = built;
            }
            if built.0 > entry.latest.0 {
                entry.latest = built;
            }
        }
        let mut busiest: Vec<Built> = busiest.into_values().collect();
        busiest.sort_by_key(|built| (std::cmp::Reverse(built.count), built.first.0));
        busiest
            .into_iter()
            .take(3)
            .map(|built| {
                let (first, latest) = (built.first.1, built.latest.1);
                let name = self.function_name(first.def);
                if built.count == 1 {
                    return format!("`{name}` has 1 instance: {}", self.instance_display(first));
                }
                format!(
                    "`{name}` has {} instances, from {} to {}",
                    built.count,
                    self.instance_display(first),
                    self.instance_display(latest)
                )
            })
            .collect()
    }

    /// Decides what to report of the diagnostics `held` while `key`'s body
    /// was checked: a failed generic instance folds its requirements into
    /// `E042` ([`Self::instance_failure`]), a template drops what depends on
    /// its parameters ([`Self::template_failure`]), and everything else is
    /// reported as it is.
    pub(crate) fn settle(&mut self, key: &FuncKey, held: Vec<Pending>) {
        if key.args.iter().any(|arg| self.types.contains_param(arg.id)) {
            self.template_failure(key, held);
        } else if !key.args.is_empty() && has_error(&held) {
            self.instance_failure(key, held);
        } else {
            self.report(held);
        }
    }

    /// Reports `held` as it is.
    pub(crate) fn report(&mut self, held: Vec<Pending>) {
        for pending in held {
            self.diags.emit(pending.diagnostic);
        }
    }

    /// Keeps the requirement failures `held` while checking `key`'s body
    /// for [`Self::emit_failures`], which folds them into one `E042` at the
    /// use that first requested `key`. The other diagnostics are reported
    /// unchanged.
    fn instance_failure(&mut self, key: &FuncKey, held: Vec<Pending>) {
        let mut requirements = Vec::new();
        for pending in held {
            match pending.requirement {
                Some(requirement) => requirements.push((pending.diagnostic, requirement)),
                None => {
                    self.diags.emit(pending.diagnostic);
                }
            }
        }
        if !requirements.is_empty() {
            self.failures.push(Failure {
                key: key.clone(),
                requirements,
                callers: None,
            });
        }
    }

    /// The `E042` for instance `key` failing `requirements`, at the use that
    /// first requested it.
    #[allow(clippy::too_many_lines)]
    fn requirement_failure(
        &self,
        key: &FuncKey,
        requirements: &[(Diagnostic, Requirement)],
    ) -> Option<Diagnostic> {
        let use_span = self
            .instances
            .get(key)
            .and_then(|instance| instance.origin.as_ref())
            .map(|(_, span)| *span)?;
        let name = self.function_name(key.def);
        let params = self.scope_of(key.def).join(", ");
        let arg_list: Vec<String> = key.args.iter().map(|arg| self.show(*arg)).collect();
        let pretty = self.instance_display(key);
        let mut diagnostic = Diagnostic::error(
            codes::E042,
            format!(
                "type `{}` does not satisfy `{name}[{params}]` requirements",
                arg_list.join(", ")
            ),
        );
        let missing = requirements
            .iter()
            .position(|(_, requirement)| matches!(requirement, Requirement::Missing { .. }));
        let primary_label = |diagnostic: &Diagnostic| {
            diagnostic
                .labels
                .iter()
                .find(|label| label.primary)
                .or_else(|| diagnostic.labels.first())
                .map(|label| label.span)
        };
        if let Some(index) = missing {
            let (captured, requirement) = &requirements[index];
            let span = primary_label(captured).unwrap_or(use_span);
            diagnostic = diagnostic
                .label(span, true, phrase(captured, requirement))
                .label(
                    use_span,
                    false,
                    format!("required by instantiation of {pretty} here"),
                );
            for (position, (captured, requirement)) in requirements.iter().enumerate() {
                if position != index {
                    if let Some(span) = primary_label(captured) {
                        diagnostic = diagnostic.label(span, false, phrase(captured, requirement));
                    }
                }
            }
        } else {
            let def_span = self.res.def(key.def).span;
            diagnostic = diagnostic.label(use_span, true, "instantiated here").label(
                def_span,
                false,
                format!("required by `{name}[{params}]` here"),
            );
            for (captured, requirement) in requirements {
                if let Some(span) = primary_label(captured) {
                    diagnostic = diagnostic.label(span, false, phrase(captured, requirement));
                }
            }
        }
        for (captured, _) in requirements {
            for note in &captured.notes {
                // Two failures of one kind share their note.
                if !diagnostic.notes.contains(note) {
                    diagnostic = diagnostic.note(note.clone());
                }
            }
        }
        for help in self.requirement_helps(requirements) {
            diagnostic = diagnostic.help(help);
        }
        Some(diagnostic)
    }

    /// The diagnostics of a template instance (arguments containing
    /// parameters): those about a parameter's type are dropped and the rest
    /// reported as they are, never folded into `E042`, since no argument
    /// failed, when [`Self::reports_template`]. The template instance of a
    /// generic keeps where its errors about no parameter are
    /// (`template_errors`), which its instances report as they are.
    fn template_failure(&mut self, key: &FuncKey, held: Vec<Pending>) {
        if key.args == self.template_args(key.def) {
            let own = held
                .iter()
                .filter(|pending| !pending.varies && pending.diagnostic.level == Level::Error)
                .map(|pending| (error_site(&pending.diagnostic), pending.diagnostic.clone()))
                .collect();
            self.template_errors.insert(key.def, own);
        }
        if !self.reports_template(key) {
            return;
        }
        for pending in held {
            if !pending.dependent {
                self.diags.emit(pending.diagnostic);
            }
        }
    }

    /// Did-you-mean, conversion, arity and `ref` hints for the requirements.
    fn requirement_helps(&self, requirements: &[(Diagnostic, Requirement)]) -> Vec<String> {
        let mut helps = Vec::new();
        for (_, requirement) in requirements {
            match requirement {
                Requirement::Missing {
                    name,
                    owner,
                    owner_def: Some(def),
                    ..
                } => {
                    let members = self.res.def(*def).members.keys().map(String::as_str);
                    if let Some(best) = lilac_error::closest(name, members) {
                        helps.push(format!("`{owner}` has `{best}`, did you mean `{best}`?"));
                    }
                }
                Requirement::Conversion { from } => {
                    let exists = self
                        .conversion_pairs()
                        .iter()
                        .any(|(source, _)| source == from);
                    if exists {
                        helps.push(format!(
                            "a `@conversion` from `{}` exists; check its target",
                            self.show_id(*from)
                        ));
                    }
                }
                Requirement::Arity { expected, found } => helps.push(format!(
                    "the call takes {expected} arguments, but {found} were supplied"
                )),
                Requirement::Mismatch { expected_ref: true } => {
                    helps.push("try passing a reference: `ref <value>`".to_owned());
                }
                _ => {}
            }
        }
        helps
    }

    /// Whether `ty` is one of the current instance's generic arguments.
    pub(crate) fn is_generic_derived(&self, ty: TypeRef) -> bool {
        self.env
            .key
            .as_ref()
            .is_some_and(|key| !key.args.is_empty())
            && self.env.args.iter().any(|arg| arg.id == ty.id)
    }

    /// A type in the generic's own parameter form (`T`) when it is one of the
    /// current arguments, concrete otherwise.
    pub(crate) fn display_in_body(&self, ty: TypeRef) -> String {
        if let Some(index) = self.env.args.iter().position(|arg| arg.id == ty.id) {
            if let Some(name) = self.env.params.get(index) {
                if ty.is_ref && !self.env.args[index].is_ref {
                    return format!("ref {name}");
                }
                return name.clone();
            }
        }
        self.show(ty)
    }

    /// The note describing the method a generic body requires, as a Lilac
    /// declaration: `name: function (self, a: @bit32) T`. Parameters past
    /// `self` are named `a`, `b`, ... since only their types are known.
    pub(crate) fn required_method_note(
        &self,
        name: &str,
        receiver: Option<TypeRef>,
        args: &[Option<TypeRef>],
        expected: Option<TypeId>,
    ) -> String {
        let mut params = Vec::new();
        if receiver.is_some() {
            params.push("self".to_owned());
        }
        for (index, arg) in args.iter().enumerate() {
            let ty = arg.map_or_else(|| "?".to_owned(), |ty| self.display_in_body(ty));
            params.push(format!("{}: {ty}", param_name(index)));
        }
        let ret = match expected {
            Some(id) if id != TypeId::ERROR => self.display_in_body(TypeRef::value(id)),
            _ => "?".to_owned(),
        };
        format!(
            "based on this usage, the generic body requires a method matching:\n    {name}: function ({}) {ret}",
            params.join(", ")
        )
    }

    /// The note describing the field a generic body requires.
    pub(crate) fn required_field_note(&self, name: &str, expected: Option<TypeId>) -> String {
        let ty = match expected {
            Some(id) if id != TypeId::ERROR => self.display_in_body(TypeRef::value(id)),
            _ => "?".to_owned(),
        };
        format!(
            "based on this usage, the generic body requires a field matching:\n    {name}: {ty}"
        )
    }
}

/// The name the required-method note gives argument `index`: `a` ... `z`,
/// then `a26`, ...
fn param_name(index: usize) -> String {
    match u8::try_from(index) {
        Ok(letter) if letter < 26 => char::from(b'a' + letter).to_string(),
        _ => format!("a{index}"),
    }
}

/// The phrase one captured requirement contributes to `E042`.
fn phrase(diagnostic: &Diagnostic, requirement: &Requirement) -> String {
    match requirement {
        Requirement::Missing {
            method,
            name,
            owner,
            ..
        } => {
            let kind = if *method { "method" } else { "member" };
            format!("missing {kind} `{name}` on `{owner}`")
        }
        Requirement::Arity { .. } => diagnostic.msg.to_string(),
        _ => diagnostic
            .labels
            .iter()
            .find(|label| label.primary)
            .map_or_else(|| diagnostic.msg.to_string(), |label| label.msg.clone()),
    }
}

/// The instances of one definition, for `E068`: how many, and the first and
/// the latest built (by [`crate::Instance::serial`]).
struct Built<'a> {
    count: usize,
    first: (usize, &'a FuncKey),
    latest: (usize, &'a FuncKey),
}
