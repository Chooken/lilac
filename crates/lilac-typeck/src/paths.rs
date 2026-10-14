//! Names and qualified paths: `name`, `A.B.item`, `A.B.Type`.
//!
//! A path's first segment is a `using … as` alias, a name the file sees,
//! else a namespace by reach ([`Resolved::namespace_named`]). Each later segment steps into a
//! namespace: an item, else a child namespace. A path ends at an item;
//! `Type.member` past it is a static member access. Locals, generic
//! parameters and the own type's names (`own.rs`) come before all of this;
//! the callers check them.
//!
//! [`Resolved::namespace_named`]: lilac_resolve::Resolved::namespace_named

use lilac_ast::Expr;
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_hir::{HirExpr, HirExprKind, MemberReceiver, SemanticKind};
use lilac_resolve::{DefId, Lookup, NsId, Segment};
use lilac_span::Span;

use crate::body::FnCx;
use crate::expr::tail_span;
use crate::sigs::leading;
use crate::Checker;

/// What a path's first segment names.
pub(crate) enum Head {
    /// One namespace or definition.
    One(Segment),
    /// An import-vs-import collision (`E029` on use).
    Ambiguous(Vec<Span>),
    /// Nothing.
    Missing,
}

/// Explains a failed member access on a local that shadows an alias (see
/// [`Checker::shadowed_alias`]).
pub(crate) fn shadowed_alias_note(
    diagnostic: Diagnostic,
    shadowed: Option<(String, Span)>,
) -> Diagnostic {
    let Some((name, span)) = shadowed else {
        return diagnostic;
    };
    diagnostic
        .label(span, false, format!("alias `{name}` declared here"))
        .note(format!(
            "the local `{name}` shadows the alias `{name}`; rename the local to reach the namespace"
        ))
}

/// The span of a path's last segment (the whole span for a bare name).
pub(crate) fn path_tail(span: Span, name: &str) -> Span {
    match name.rsplit_once('.') {
        Some((_, tail)) => tail_span(span, tail),
        None => span,
    }
}

/// The span of each segment of the path `name` spelled at `span`, or `None`
/// when the span is longer than the name: the path was written with space
/// or a comment between its segments, so offsets would not find them.
/// Equal lengths mean the segments and dots fill the span exactly.
pub(crate) fn segment_spans(span: Span, name: &str) -> Option<Vec<Span>> {
    if span.is_dummy() || span.hi.0.checked_sub(span.lo.0)? as usize != name.len() {
        return None;
    }
    let mut lo = span.lo.0;
    let mut spans = Vec::new();
    for segment in name.split('.') {
        let hi = lo + u32::try_from(segment.len()).ok()?;
        spans.push(Span::new(lo, hi, span.file));
        lo = hi + 1;
    }
    Some(spans)
}

/// The name chain the path `name` spelled at `span` is (`A.B.f` as
/// `A`, `.B`, `.f`), as the parser builds one: each segment at its own
/// span ([`segment_spans`]), or at `span` when the path was written with
/// space between its segments.
pub(crate) fn path_expr(name: &str, span: Span) -> Expr {
    let spans = segment_spans(span, name).unwrap_or_default();
    let mut segments = name.split('.').enumerate().map(|(at, segment)| {
        let at = spans.get(at).copied().unwrap_or(span);
        (segment.to_owned(), at)
    });
    let (head, head_span) = segments.next().unwrap_or_else(|| (String::new(), span));
    segments.fold(Expr::Ident(head, head_span), |obj, (field, at)| {
        let start = obj.span();
        Expr::Member {
            obj: Box::new(obj),
            field,
            span: Span::new(start.lo.0, at.hi.0, at.file),
        }
    })
}

impl Checker<'_> {
    /// The namespace of the file being checked.
    pub(crate) fn file_ns(&self) -> NsId {
        self.res
            .files
            .get(&self.file())
            .map_or(NsId(0), |scope| scope.ns)
    }

    /// What `name` names as the first segment of a path.
    pub(crate) fn lookup_head(&self, name: &str) -> Head {
        // An alias shadows ancestor-reached and imported names and
        // namespaces; one named like an item of the file's own namespace is
        // `E054` in the resolver, so it never shadows those.
        if let Some(ns) = self.res.alias(self.file(), name) {
            return Head::One(Segment::Ns(ns));
        }
        match self.res.lookup(self.file(), name) {
            Some(Lookup::One(id)) => return Head::One(Segment::Def(*id)),
            Some(Lookup::Ambiguous(sites)) => return Head::Ambiguous(sites.clone()),
            None => {}
        }
        if self.in_entry() {
            if let Some(id) = self.res.root_item(name) {
                return Head::One(Segment::Def(id));
            }
        }
        match self.res.namespace_named(self.file_ns(), name) {
            Some(ns) => Head::One(Segment::Ns(ns)),
            None => Head::Missing,
        }
    }

    /// Whether the dotted `path` names a namespace (no item along it).
    pub(crate) fn names_namespace(&self, path: &str) -> bool {
        let mut parts = path.split('.');
        let Head::One(mut segment) = self.lookup_head(parts.next().unwrap_or(path)) else {
            return false;
        };
        for part in parts {
            let Segment::Ns(ns) = segment else {
                return false;
            };
            match self.res.ns_member(ns, part, self.file_ns()) {
                Some(next) => segment = next,
                None => return false,
            }
        }
        matches!(segment, Segment::Ns(_))
    }

    /// The `using … as` alias a local receiver hides: its name and the
    /// alias's span, when `receiver` is a local named like an alias of this
    /// file. Locals shadow aliases, so `Math.pow()` with a local `Math` is a
    /// method call; a failed one says why ([`shadowed_alias_note`]).
    pub(crate) fn shadowed_alias(&self, fcx: &FnCx, receiver: &HirExpr) -> Option<(String, Span)> {
        let HirExprKind::Local(id) = receiver.kind else {
            return None;
        };
        let name = &fcx.locals.get(id.0 as usize)?.name;
        let (_, span) = self.res.files.get(&self.file())?.aliases.get(name)?;
        Some((name.clone(), *span))
    }

    /// Resolves an item name or a dotted path in the current environment.
    /// `Ok(None)`: no definition (or a namespace); `Err`: ambiguous (`E029`,
    /// already reported).
    pub(crate) fn lookup_item(
        &mut self,
        name: &str,
        span: Span,
    ) -> Result<Option<DefId>, ErrorGuaranteed> {
        let mut parts = name.split('.');
        let head = parts.next().unwrap_or(name);
        if let Some(decl) = self.res.alias_span(self.file(), head) {
            self.record_alias_use(leading(span, head), decl);
        }
        // Each segment that names a namespace colors as one.
        let spans = segment_spans(span, name).unwrap_or_default();
        let span_of = |at: usize| spans.get(at).copied().unwrap_or_else(Span::dummy);
        let record = |checker: &mut Self, at: usize, segment: &Segment| {
            if let Segment::Ns(_) = segment {
                let span = span_of(at);
                if !span.is_dummy() {
                    checker.record_semantic(span, SemanticKind::Namespace);
                }
            }
        };
        let mut segment = match self.lookup_head(head) {
            Head::One(segment) => segment,
            Head::Ambiguous(sites) => {
                let mut diagnostic = Diagnostic::error(
                    codes::E029,
                    format!("`{head}` is ambiguous"),
                )
                .label(span, true, "ambiguous reference here");
                for site in sites {
                    diagnostic = diagnostic.label(site, false, "imported here");
                }
                return Err(self.diags.emit(diagnostic));
            }
            Head::Missing => return Ok(None),
        };
        record(self, 0, &segment);
        for (at, part) in parts.enumerate().map(|(at, part)| (at + 1, part)) {
            // An item ends the path.
            let Segment::Ns(ns) = segment else {
                // Only a declaration's type reads on past a type
                // (`name: Type.`, mid-edit): the shorthand's static
                // functions follow.
                let receiver = match segment {
                    Segment::Def(def) if self.res.def(def).is_type() => {
                        Some(MemberReceiver::Constructor(def))
                    }
                    // An alias of an object or enum lists that type's.
                    Segment::Def(def) => self
                        .types
                        .alias(def)
                        .and_then(|ty| self.type_parts(ty))
                        .map(|(def, _)| MemberReceiver::Constructor(def)),
                    Segment::Ns(_) => None,
                };
                self.record_member(span_of(at), part, receiver)?;
                return Ok(None);
            };
            self.record_member(span_of(at), part, Some(MemberReceiver::Namespace(ns)))?;
            match self.res.ns_member(ns, part, self.file_ns()) {
                Some(next) => segment = next,
                None => return Ok(None),
            }
            record(self, at, &segment);
        }
        match segment {
            Segment::Def(def) => Ok(Some(def)),
            Segment::Ns(_) => Ok(None),
        }
    }

    /// The dotted name a member chain spells while it walks namespaces
    /// (`A.B.foo`, `A.B.Type`, `@builtin.Target.os`), for
    /// [`Self::lookup_item`]. `None` when the chain is a value or starts at
    /// a type (`Type.method`; `A.Enum.Variant` past `A.Enum`), which are
    /// static member accesses.
    pub(crate) fn qualified_name(&self, fcx: &FnCx, expr: &Expr) -> Option<(String, Span)> {
        let Expr::Member { span, .. } = expr else {
            return None;
        };
        let (path, _) = self.path_prefix(fcx, expr)?;
        Some((path, *span))
    }

    /// The path a chain spells and what it names (`None` past a namespace
    /// member that does not exist: the path ends there, unresolved). Only a
    /// namespace continues a path.
    fn path_prefix(&self, fcx: &FnCx, expr: &Expr) -> Option<(String, Option<Segment>)> {
        match expr {
            Expr::Ident(name, _) => {
                if fcx.lookup(name).is_some()
                    || self.env.params.iter().any(|param| param == name)
                    || self.own_member(name).is_some()
                    || self.types.builtin(name).is_some()
                {
                    return None;
                }
                match self.lookup_head(name) {
                    Head::One(Segment::Ns(ns)) => Some((name.clone(), Some(Segment::Ns(ns)))),
                    _ => None,
                }
            }
            Expr::Member { obj, field, .. } => {
                let (path, segment) = self.path_prefix(fcx, obj)?;
                let Segment::Ns(ns) = segment? else {
                    return None;
                };
                let next = self.res.ns_member(ns, field, self.file_ns());
                Some((format!("{path}.{field}"), next))
            }
            _ => None,
        }
    }
}
