//! Did-you-mean: the closest known name to a misspelled one, offered as a
//! fix ([`Diagnostic::suggest`]).

use lilac_error::Diagnostic;
use lilac_resolve::{DefKind, Lookup, NsId, Segment, Visibility};
use lilac_span::Span;

use crate::body::FnCx;
use crate::paths::Head;
use crate::Checker;
use lilac_error::closest;

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
        names.extend(self.visible_names(|kind| {
            matches!(
                kind,
                DefKind::Func(_) | DefKind::Const { .. } | DefKind::Global { .. }
            )
        }));
        names
    }

    /// The names a type can be spelled with here: the file's visible
    /// objects and enums, the generic parameters, and the builtins.
    pub(crate) fn type_names(&self) -> Vec<String> {
        let mut names = self.visible_names(|kind| matches!(kind, DefKind::Object | DefKind::Enum));
        names.extend(self.env.params.iter().cloned());
        names.extend(lilac_ty::Builtin::ALL.map(|builtin| builtin.name().to_owned()));
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

    /// Adds a fix for a misspelled value name, or for the misspelled
    /// segment of a path (`span` starts at its head).
    pub(crate) fn suggest_value(
        &self,
        fcx: &FnCx,
        diagnostic: Diagnostic,
        name: &str,
        span: Span,
    ) -> Diagnostic {
        if name.contains('.') {
            return self.suggest_path(diagnostic, name, span, |kind| {
                matches!(
                    kind,
                    DefKind::Func(_) | DefKind::Const { .. } | DefKind::Global { .. }
                )
            });
        }
        // A misspelled bare name may be a path's head (`Mth.pow()`).
        let mut names = self.value_names(fcx);
        names.extend(self.head_names(|_| false));
        suggest(diagnostic, name, span, names.iter().map(String::as_str))
    }

    /// Adds a fix for a misspelled type name, or for the misspelled segment
    /// of a type path; `span` is the whole name (`A.B.Type`).
    pub(crate) fn suggest_type(
        &self,
        diagnostic: Diagnostic,
        name: &str,
        span: Span,
    ) -> Diagnostic {
        if name.contains('.') {
            return self.suggest_path(diagnostic, name, span, |kind| {
                matches!(kind, DefKind::Object | DefKind::Enum)
            });
        }
        let names = self.type_names();
        suggest(diagnostic, name, span, names.iter().map(String::as_str))
    }

    /// Adds a fix for the first segment of the path `name` that names
    /// nothing, from what the previous segment's namespace holds: its
    /// child namespaces and items (those `wanted` accepts for the last
    /// segment, types before it). A head is matched against the file's
    /// aliases, the namespaces within reach and the visible types. `span`
    /// starts at the head; a path that reaches an item before failing is a
    /// member access and gets no fix here.
    fn suggest_path(
        &self,
        diagnostic: Diagnostic,
        name: &str,
        span: Span,
        wanted: impl Fn(&DefKind) -> bool,
    ) -> Diagnostic {
        let segments: Vec<&str> = name.split('.').collect();
        let mut offset = 0u32;
        let mut current: Option<NsId> = None;
        for (index, segment) in segments.iter().enumerate() {
            let last = index + 1 == segments.len();
            let found = match current {
                None => match self.lookup_head(segment) {
                    Head::One(found) => Some(found),
                    Head::Ambiguous(_) => return diagnostic,
                    Head::Missing => None,
                },
                Some(ns) => self.res.ns_member(ns, segment, self.file_ns()),
            };
            match found {
                Some(Segment::Ns(ns)) if !last => current = Some(ns),
                Some(_) => return diagnostic,
                None => {
                    let accepts = |kind: &DefKind| {
                        if last {
                            wanted(kind)
                        } else {
                            matches!(kind, DefKind::Object | DefKind::Enum)
                        }
                    };
                    let names = match current {
                        None => self.head_names(accepts),
                        Some(ns) => self.namespace_names(ns, accepts),
                    };
                    let len = u32::try_from(segment.len()).unwrap_or(u32::MAX);
                    let lo = span.lo.0.saturating_add(offset);
                    let at = Span::new(lo, lo.saturating_add(len), span.file);
                    return suggest(diagnostic, segment, at, names.iter().map(String::as_str));
                }
            }
            offset = offset
                .saturating_add(u32::try_from(segment.len()).unwrap_or(u32::MAX))
                .saturating_add(1);
        }
        diagnostic
    }

    /// The names a path can start with here: the file's aliases, the
    /// namespaces within reach, and its visible items `wanted` accepts.
    fn head_names(&self, wanted: impl Fn(&DefKind) -> bool) -> Vec<String> {
        let mut names = self.visible_names(wanted);
        if let Some(scope) = self.res.files.get(&self.file()) {
            names.extend(scope.aliases.keys().cloned());
        }
        let mut ns = Some(self.file_ns());
        while let Some(current) = ns {
            names.extend(
                self.res
                    .child_namespaces(current)
                    .map(|(name, _)| name.to_owned()),
            );
            ns = self
                .res
                .namespaces
                .get(current.0 as usize)
                .and_then(|data| data.parent);
        }
        names
    }

    /// What a path can step to inside `ns`: its child namespaces and the
    /// items `wanted` accepts (a `private` one only within reach).
    fn namespace_names(&self, ns: NsId, wanted: impl Fn(&DefKind) -> bool) -> Vec<String> {
        let mut names: Vec<String> = self
            .res
            .child_namespaces(ns)
            .map(|(name, _)| name.to_owned())
            .collect();
        let reaches = self.res.reaches(self.file_ns(), ns);
        if let Some(data) = self.res.namespaces.get(ns.0 as usize) {
            names.extend(
                data.items
                    .iter()
                    .filter(|(_, id)| {
                        let def = self.res.def(**id);
                        (def.vis == Visibility::Public || reaches) && wanted(&def.kind)
                    })
                    .map(|(name, _)| name.clone()),
            );
        }
        names
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
