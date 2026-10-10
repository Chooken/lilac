//! Diagnostics for generic instances: `E042` (an instance body failed a
//! duck-typing requirement) and the instantiation chains `E042`/`E051` show.
//!
//! An instance body is checked like any other; its diagnostics are captured
//! and, when any is a recorded [`Requirement`], folded into one `E042` at the
//! use that requested the instance. Requirements are recorded when emitted, so
//! nothing here parses diagnostic text.

use std::collections::HashSet;

use lilac_error::{codes, Diagnostic, TraceFrame};
use lilac_hir::FuncKey;
use lilac_span::Span;
use lilac_ty::{TypeId, TypeRef};

use crate::sink::{has_error, Pending};
use crate::{Checker, Requirement};

impl Checker<'_> {
    /// Source name of a function for diagnostics: `name`, or `Owner.name`.
    pub(crate) fn function_name(&self, def: lilac_resolve::DefId) -> String {
        let d = self.res.def(def);
        match d.parent {
            Some(parent) => format!("{}.{}", self.res.def(parent).name, d.name),
            None => d.name.clone(),
        }
    }

    /// `name[@arg, ...]` for an instance (`name` alone when not generic).
    pub(crate) fn instance_display(&self, key: &FuncKey) -> String {
        let name = self.function_name(key.def);
        if key.args.is_empty() {
            return name;
        }
        let args: Vec<String> = key
            .args
            .iter()
            .map(|arg| self.types.display_ref(*arg))
            .collect();
        format!("{name}[{}]", args.join(", "))
    }

    /// The generic instances on `key`'s request chain, outermost first.
    pub(crate) fn frames(&self, key: &FuncKey) -> Vec<String> {
        let mut frames = Vec::new();
        let mut current = Some(key.clone());
        while let Some(key) = current {
            if key.args.is_empty() || frames.len() > 64 {
                break;
            }
            frames.push(self.instance_display(&key));
            current = self
                .instances
                .get(&key)
                .and_then(|instance| instance.origin.clone())
                .and_then(|(parent, _)| parent);
        }
        frames.reverse();
        frames
    }

    /// The trace of a failure at `span` in instance `key`: every site that
    /// requested it, through requesting generic instances, down to the
    /// non-generic functions (user code) that started each chain.
    pub(crate) fn instance_trace(&self, key: &FuncKey, span: Span) -> TraceFrame {
        let mut expanded = HashSet::from([key.clone()]);
        TraceFrame {
            func: self.instance_display(key),
            span,
            address: false,
            repeat: false,
            callers: self.origin_frames(key, &mut expanded),
        }
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

    /// Emits the held `E042`s, each traced through every origin recorded so
    /// far.
    pub(crate) fn emit_failures(&mut self) {
        for (key, diagnostic) in std::mem::take(&mut self.failures) {
            let span = diagnostic
                .labels
                .iter()
                .find(|label| label.primary)
                .map_or_else(Span::dummy, |label| label.span);
            let trace = self.instance_trace(&key, span);
            self.diags.emit(diagnostic.trace(trace));
        }
    }

    /// The `E051` chain text for `key`.
    pub(crate) fn chain_text(&self, key: &FuncKey) -> String {
        crate::types::elide_chain(&self.frames(key))
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

    /// Folds the requirement failures `held` while checking `key`'s body
    /// into one `E042` at the use that first requested `key`, held until the
    /// worklist drains ([`Self::emit_failures`]). The other diagnostics are
    /// reported unchanged.
    #[allow(clippy::too_many_lines)]
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
        if requirements.is_empty() || !self.reported.insert(key.clone()) {
            return;
        }
        let Some(use_span) = self
            .instances
            .get(key)
            .and_then(|instance| instance.origin.as_ref())
            .map(|(_, span)| *span)
        else {
            return;
        };
        let name = self.function_name(key.def);
        let params = self.scope_of(key.def).join(", ");
        let arg_list: Vec<String> = key
            .args
            .iter()
            .map(|arg| self.types.display_ref(*arg))
            .collect();
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
            for (captured, requirement) in &requirements {
                if let Some(span) = primary_label(captured) {
                    diagnostic = diagnostic.label(span, false, phrase(captured, requirement));
                }
            }
        }
        for (captured, _) in &requirements {
            for note in &captured.notes {
                diagnostic = diagnostic.note(note.clone());
            }
        }
        for help in self.requirement_helps(&requirements) {
            diagnostic = diagnostic.help(help);
        }
        self.failures.push((key.clone(), diagnostic));
    }

    /// The diagnostics of a template instance (arguments containing
    /// parameters): those about a parameter's type are dropped and the rest
    /// reported as they are, never folded into `E042`, since no argument
    /// failed, when [`Self::reports_template`].
    fn template_failure(&mut self, key: &FuncKey, held: Vec<Pending>) {
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
                            self.types.display(*from)
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
        self.types.display_ref(ty)
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
