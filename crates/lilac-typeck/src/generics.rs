//! Diagnostics for generic instances: `E042` (an instance body failed a
//! duck-typing requirement) and the instantiation chains `E042`/`E051` show.
//!
//! An instance body is checked like any other; its diagnostics are captured
//! and, when any is a recorded [`Requirement`], folded into one `E042` at the
//! use that requested the instance. Requirements are recorded when emitted, so
//! nothing here parses diagnostic text.

use lilac_error::{Diagnostic, CHAIN_FRAME_PREFIX};
use lilac_hir::FuncKey;
use lilac_ty::{TypeId, TypeRef};

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

    /// The `E051` chain text for `key`.
    pub(crate) fn chain_text(&self, key: &FuncKey) -> String {
        crate::types::elide_chain(&self.frames(key))
    }

    /// Folds the requirement failures captured since `mark` (while checking
    /// `key`'s body) into one `E042` at the use that requested `key`. Other
    /// captured diagnostics are re-emitted unchanged.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn instance_failure(&mut self, key: &FuncKey, mark: usize) {
        let captured = self.diags.take_since(mark);
        let mut recorded = Vec::new();
        self.requirements.retain(|(index, requirement)| {
            if *index >= mark {
                recorded.push((*index - mark, requirement.clone()));
                false
            } else {
                true
            }
        });
        let mut requirements = Vec::new();
        for (position, diagnostic) in captured.into_iter().enumerate() {
            match recorded.iter().find(|(index, _)| *index == position) {
                Some((_, requirement)) => requirements.push((diagnostic, requirement.clone())),
                None => {
                    self.diags.emit(diagnostic);
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
            "E042",
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
        let frames = self.frames(key);
        if !(missing.is_some() && frames.len() <= 1) {
            for frame in frames {
                diagnostic = diagnostic.note(format!("{CHAIN_FRAME_PREFIX}{frame}"));
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
        self.diags.emit(diagnostic);
    }

    /// Did-you-mean, conversion, arity and `ref` hints for the requirements.
    fn requirement_helps(&self, requirements: &[(Diagnostic, Requirement)]) -> Vec<String> {
        let mut helps = Vec::new();
        for (diagnostic, requirement) in requirements {
            match requirement {
                Requirement::Missing {
                    name,
                    owner,
                    owner_def: Some(def),
                    ..
                } => {
                    let members: Vec<&String> = self.res.def(*def).members.keys().collect();
                    if let Some(best) = closest(name, &members) {
                        helps.push(format!("`{owner}` has `{best}`, did you mean `{best}`?"));
                    }
                }
                Requirement::Conversion { from } => {
                    let exists = self
                        .conversion_pairs()
                        .iter()
                        .any(|(source, _)| self.types.display(*source) == *from);
                    if exists {
                        helps.push(format!(
                            "a `@conversion` from `{from}` exists; check its target"
                        ));
                    }
                }
                Requirement::Arity { expected, found } => helps.push(format!(
                    "the call takes {expected} arguments, but {found} were supplied"
                )),
                Requirement::Mismatch if diagnostic.msg.contains("expected ref ") => {
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

    /// The note describing the method a generic body requires.
    pub(crate) fn required_method_note(
        &self,
        name: &str,
        receiver: Option<TypeRef>,
        args: &[Option<TypeRef>],
        expected: Option<TypeId>,
    ) -> String {
        let mut params = Vec::new();
        if let Some(ty) = receiver {
            params.push(format!("self: ref {}", self.display_in_body(ty.deref())));
        }
        for arg in args {
            params.push(arg.map_or_else(|| "?".to_owned(), |ty| self.display_in_body(ty)));
        }
        let ret = match expected {
            Some(id) if id != TypeId::ERROR => self.display_in_body(TypeRef::value(id)),
            _ => "?".to_owned(),
        };
        format!(
            "based on this usage, the generic body requires a method matching:\n    fn {name}({}) -> {ret}",
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

/// The closest member by edit distance (at most 2), if any.
fn closest(name: &str, members: &[&String]) -> Option<String> {
    members
        .iter()
        .map(|member| (levenshtein(name, member), *member))
        .filter(|(distance, member)| *distance <= 2 && member.as_str() != name)
        .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)))
        .map(|(_, member)| member.clone())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}
