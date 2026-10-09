//! Did-you-mean: the closest known name to a misspelled one, offered as a
//! fix ([`Diagnostic::suggest`]).

use lilac_error::Diagnostic;
use lilac_resolve::{DefKind, Lookup};
use lilac_span::Span;

use crate::body::FnCx;
use crate::Checker;

/// The candidate closest to `name` by edit distance, if one is close
/// enough: one edit for names of up to three characters, else two.
pub(crate) fn closest<'a>(
    name: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let limit = if name.chars().count() <= 3 { 1 } else { 2 };
    candidates
        .into_iter()
        .filter(|candidate| *candidate != name)
        .map(|candidate| (levenshtein(name, candidate), candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min()
        .map(|(_, candidate)| candidate.to_owned())
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

/// Adds a fix replacing `span` with the candidate closest to `name`, if any.
pub(crate) fn suggest<'a>(
    diagnostic: Diagnostic,
    name: &str,
    span: Span,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Diagnostic {
    match closest(name, candidates) {
        Some(best) => diagnostic.suggest(span, best),
        None => diagnostic,
    }
}

impl Checker<'_> {
    /// The names a value can be spelled with here: locals in scope and the
    /// file's visible functions and constants.
    pub(crate) fn value_names(&self, fcx: &FnCx) -> Vec<String> {
        let mut names: Vec<String> = fcx
            .scopes
            .iter()
            .flat_map(|scope| scope.keys().cloned())
            .collect();
        names.extend(
            self.visible_names(|kind| matches!(kind, DefKind::Func(_) | DefKind::Const { .. })),
        );
        names
    }

    /// The names a type can be spelled with here: the file's visible
    /// objects and enums, the generic parameters, and the builtins.
    pub(crate) fn type_names(&self) -> Vec<String> {
        let mut names = self.visible_names(|kind| matches!(kind, DefKind::Object | DefKind::Enum));
        names.extend(self.env.params.iter().cloned());
        names.extend(
            lilac_ty::BUILTIN_TYPES
                .iter()
                .map(|name| (*name).to_owned()),
        );
        names
    }

    /// The file's visible item names whose definition `wanted` accepts.
    fn visible_names(&self, wanted: impl Fn(&DefKind) -> bool) -> Vec<String> {
        let Some(scope) = self.res.files.get(&self.file()) else {
            return Vec::new();
        };
        scope
            .visible
            .iter()
            .filter(|(_, lookup)| match lookup {
                Lookup::One(id) => wanted(&self.res.def(*id).kind),
                Lookup::Ambiguous(_) => false,
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Adds a fix for a misspelled value name (an undotted one).
    pub(crate) fn suggest_value(
        &self,
        fcx: &FnCx,
        diagnostic: Diagnostic,
        name: &str,
        span: Span,
    ) -> Diagnostic {
        if name.contains('.') {
            return diagnostic;
        }
        let names = self.value_names(fcx);
        suggest(diagnostic, name, span, names.iter().map(String::as_str))
    }

    /// Adds a fix for a misspelled type name (an undotted one); `span` is
    /// the name's token.
    pub(crate) fn suggest_type(
        &self,
        diagnostic: Diagnostic,
        name: &str,
        span: Span,
    ) -> Diagnostic {
        if name.contains('.') {
            return diagnostic;
        }
        let names = self.type_names();
        suggest(diagnostic, name, span, names.iter().map(String::as_str))
    }

    /// Adds a fix for a misspelled member of `owner` at `span`: a method or
    /// variant when `called`, else a field.
    pub(crate) fn suggest_member(
        &self,
        diagnostic: Diagnostic,
        owner: lilac_resolve::DefId,
        name: &str,
        span: Span,
        called: bool,
    ) -> Diagnostic {
        let names = self.member_names(owner, |kind| match kind {
            DefKind::Func(_) | DefKind::Variant(_) => called,
            DefKind::Field(_) => !called,
            _ => false,
        });
        suggest(diagnostic, name, span, names.iter().map(String::as_str))
    }

    /// The names of `owner`'s members `wanted` accepts.
    pub(crate) fn member_names(
        &self,
        owner: lilac_resolve::DefId,
        wanted: impl Fn(&DefKind) -> bool,
    ) -> Vec<String> {
        self.res
            .def(owner)
            .members
            .iter()
            .filter(|(_, id)| wanted(&self.res.def(**id).kind))
            .map(|(name, _)| name.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::closest;

    #[test]
    fn closest_scales_with_the_name() {
        assert_eq!(
            closest("lenght", ["length", "left"]),
            Some("length".to_owned())
        );
        assert_eq!(closest("ab", ["xy"]), None);
        assert_eq!(closest("ab", ["ac", "xy"]), Some("ac".to_owned()));
        assert_eq!(closest("same", ["same"]), None);
    }
}
