//! Faded hints: unused names, unreachable statements and statements whose
//! value has no effect. They are computed here from a retained analysis and
//! published beside the compiler's diagnostics as `Hint` + `Unnecessary`;
//! the compiler never reports them, so the command line prints nothing for
//! them. Discarded call results are never flagged.

use std::collections::{HashMap, HashSet};

use lilac_error::Level;
use lilac_hir::LocalKind;
use lilac_resolve::{DefKind, Visibility};
use lilac_span::Span;
use lsp_types::{Diagnostic as LspDiagnostic, DiagnosticSeverity, DiagnosticTag};

use crate::convert::span_range;
use crate::index::Retained;

/// One faded hint: where, and what it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    /// The faded range: a name, or the unreachable statements.
    pub span: Span,
    /// The hover text.
    pub message: String,
}

/// Every hint of `retained`'s compile, in source order.
///
/// Names and statements inside a scope with an error get none: what the
/// checker skipped after the error may be what uses them.
#[must_use]
pub fn hints(retained: &Retained) -> Vec<Hint> {
    let Some(analysis) = retained.session.analysis() else {
        return Vec::new();
    };
    let sources = &retained.session.source_map;
    let index = &retained.index;
    let errors: Vec<Span> = retained
        .session
        .diags
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.level == Level::Error)
        .flat_map(|diagnostic| diagnostic.labels.iter().map(|label| label.span))
        .collect();
    let has_error = |scope: Span| errors.iter().any(|error| contains(scope, *error));
    let text = |span: Span| sources.span_text(span).unwrap_or_default().to_owned();
    let mut unreachable = analysis.hir.unreachable.clone();
    unreachable.sort_by_key(|span| (span.file.0, span.lo.0, span.hi.0));
    unreachable.dedup();
    let mut hints = Vec::new();

    // Locals and parameters: a declaration that is its own only site
    // (unreachable code is faded whole instead).
    let mut used: HashSet<Span> = HashSet::new();
    for site in &index.locals {
        if site.span != site.decl {
            used.insert(site.decl);
        }
    }
    for site in &index.locals {
        let name = text(site.decl);
        if site.span != site.decl
            || used.contains(&site.decl)
            || site.kind == LocalKind::Signature
            || name == "self"
            || name.starts_with('_')
            || has_error(site.scope)
            || unreachable.iter().any(|span| contains(*span, site.decl))
        {
            continue;
        }
        let what = if site.kind == LocalKind::Param {
            "parameter"
        } else {
            "local"
        };
        hints.push(Hint {
            span: site.decl,
            message: format!("{what} `{name}` is never used"),
        });
    }

    // `using … as` aliases: a declaration no path head names.
    let mut named: HashMap<Span, bool> = HashMap::new();
    for site in &index.aliases {
        *named.entry(site.decl).or_default() |= site.span != site.decl;
    }
    for (decl, is_used) in named {
        if !is_used {
            hints.push(Hint {
                span: decl,
                message: format!("alias `{}` is never used", text(decl)),
            });
        }
    }

    unused_private_items(retained, &mut hints);

    // The rest of a block after `return`, `break`, `continue`, `@panic` or
    // `@trap`.
    for &span in &unreachable {
        if !has_error(span) {
            hints.push(Hint {
                span,
                message: "unreachable code".to_owned(),
            });
        }
    }

    // Statements whose value has no effect (`5 + 5`). Every instance of a
    // generic records its body, so a span can repeat.
    let mut no_effect = analysis.hir.no_effect.clone();
    no_effect.sort_by_key(|span| (span.file.0, span.lo.0, span.hi.0));
    no_effect.dedup();
    for span in no_effect {
        if !has_error(span) && !unreachable.iter().any(|outer| contains(*outer, span)) {
            hints.push(Hint {
                span,
                message: "this value is never used".to_owned(),
            });
        }
    }

    hints.sort_by_key(|hint| (hint.span.file.0, hint.span.lo.0, hint.span.hi.0));
    hints
}

/// Top-level `private` items nothing names.
fn unused_private_items(retained: &Retained, hints: &mut Vec<Hint>) {
    let Some(analysis) = retained.session.analysis() else {
        return;
    };
    let used: HashSet<_> = retained.index.uses.iter().map(|site| site.def).collect();
    for id in analysis.resolved.ids() {
        let def = analysis.resolved.def(id);
        let item = matches!(
            def.kind,
            DefKind::Func(_)
                | DefKind::Object
                | DefKind::Enum
                | DefKind::Const { .. }
                | DefKind::Global { .. }
        );
        if item
            && def.parent.is_none()
            && def.vis == Visibility::Private
            && !def.name_span.is_dummy()
            && !used.contains(&id)
        {
            hints.push(Hint {
                span: def.name_span,
                message: format!("`{}` is never used", def.name),
            });
        }
    }
}

/// `hint` as a faded LSP diagnostic, or `None` when its file is unknown.
#[must_use]
pub fn to_lsp_hint(retained: &Retained, hint: &Hint) -> Option<LspDiagnostic> {
    Some(LspDiagnostic {
        range: span_range(&retained.session.source_map, hint.span)?,
        severity: Some(DiagnosticSeverity::HINT),
        source: Some("lilac".to_owned()),
        message: hint.message.clone(),
        tags: Some(vec![DiagnosticTag::UNNECESSARY]),
        ..LspDiagnostic::default()
    })
}

/// Whether `outer` covers `inner` (same file).
fn contains(outer: Span, inner: Span) -> bool {
    outer.file == inner.file && outer.lo.0 <= inner.lo.0 && inner.hi.0 <= outer.hi.0
}
