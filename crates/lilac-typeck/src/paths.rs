//! Names and qualified paths: `name`, `A.B.item`, `A.B.Type`.
//!
//! A path's first segment is a name the file sees, else a namespace by
//! reach ([`Resolved::namespace_named`]). Each later segment steps into a
//! namespace: an item, else a child namespace. A path ends at an item;
//! `Type.member` past it is a static member access. Locals and generic
//! parameters come before all of this; the callers check them.
//!
//! [`Resolved::namespace_named`]: lilac_resolve::Resolved::namespace_named

use lilac_ast::Expr;
use lilac_error::{Diagnostic, ErrorGuaranteed};
use lilac_resolve::{DefId, Lookup, NsId, Segment};
use lilac_span::Span;

use crate::body::FnCx;
use crate::expr::tail_span;
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

/// The span of a path's last segment (the whole span for a bare name).
pub(crate) fn path_tail(span: Span, name: &str) -> Span {
    match name.rsplit_once('.') {
        Some((_, tail)) => tail_span(span, tail),
        None => span,
    }
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
        let mut segment = match self.lookup_head(head) {
            Head::One(segment) => segment,
            Head::Ambiguous(sites) => {
                let mut diagnostic = Diagnostic::error("E029", format!("`{head}` is ambiguous"))
                    .label(span, true, "ambiguous reference here");
                for site in sites {
                    diagnostic = diagnostic.label(site, false, "imported here");
                }
                return Err(self.diags.emit(diagnostic));
            }
            Head::Missing => return Ok(None),
        };
        for part in parts {
            // An item ends the path.
            let Segment::Ns(ns) = segment else {
                return Ok(None);
            };
            match self.res.ns_member(ns, part, self.file_ns()) {
                Some(next) => segment = next,
                None => return Ok(None),
            }
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
