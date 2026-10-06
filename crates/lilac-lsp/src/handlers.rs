//! Editor request handlers: hover and go-to-definition.

use lilac_hir::NameSite;
use lilac_span::{FileId, Span};
use lilac_ty::{TypeRef, Types};
use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position, Range, Url};

use crate::convert::{byte_offset, span_range};
use crate::index::{AnalysisIndex, Retained};

/// Hover for the item or expression at `position`, if any.
#[must_use]
pub fn hover(retained: &Retained, uri: &Url, position: Position) -> Option<Hover> {
    let file = *retained.uri_files.get(uri)?;
    let source = retained.session.source_map.file(file)?;
    let byte = byte_offset(&source.src, position)?;
    let index = &retained.index;

    let (range, value) = match smallest_use(index, file, byte) {
        Some(site) => match (
            span_range(&retained.session.source_map, site.span),
            item_hover(retained, site, byte),
        ) {
            (Some(range), Some(text)) => (range, text),
            _ => expr_hover(retained, file, byte)?,
        },
        None => expr_hover(retained, file, byte)?,
    };

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(range),
    })
}

/// The smallest resolved use whose span contains `byte`.
fn smallest_use(index: &AnalysisIndex, file: FileId, byte: usize) -> Option<NameSite> {
    index
        .uses
        .iter()
        .filter(|site| site.span.file == file && contains(site.span, byte))
        .min_by_key(|site| (site.span.hi.0 - site.span.lo.0, site.span.lo.0))
        .copied()
}

/// The smallest checked expression whose span contains `byte`.
fn smallest_expr(index: &AnalysisIndex, file: FileId, byte: usize) -> Option<(Span, TypeRef)> {
    index
        .exprs
        .iter()
        .filter(|(span, _)| span.file == file && contains(*span, byte))
        .min_by_key(|(span, _)| (span.hi.0 - span.lo.0, span.lo.0))
        .copied()
}

/// Whether a half-open span contains `byte`.
fn contains(span: Span, byte: usize) -> bool {
    let Ok(byte) = u32::try_from(byte) else {
        return false;
    };
    span.lo.0 <= byte && byte < span.hi.0
}

/// Renders a hover for a resolved item use.
fn item_hover(retained: &Retained, site: NameSite, byte: usize) -> Option<String> {
    let info = retained.index.defs.get(&site.def)?;
    let analysis = retained.session.analysis()?;
    let text = match info.kind {
        "function" | "hook" => {
            let sig = analysis.sigs.funcs.get(&site.def).or_else(|| {
                analysis
                    .sigs
                    .hooks
                    .values()
                    .flatten()
                    .find(|(id, _)| *id == site.def)
                    .map(|(_, sig)| sig)
            })?;
            let params = join_types(&analysis.types, &sig.params);
            let returns = join_types(&analysis.types, &sig.returns);
            if returns.is_empty() {
                format!("function {}({}) nothing", info.qualified, params)
            } else {
                format!("function {}({}) -> {}", info.qualified, params, returns)
            }
        }
        "object" => format!("object {}", info.qualified),
        "enum" => format!("enum {}", info.qualified),
        _ => {
            let mut text = format!("{} {}", info.kind, info.qualified);
            if let Some((_, ty)) = smallest_expr(&retained.index, site.span.file, byte) {
                text.push('\n');
                text.push_str(&analysis.types.display_ref(ty));
            }
            text
        }
    };
    Some(format!("```lilac\n{text}\n```"))
}

/// Renders a hover for a bare expression (its type).
fn expr_hover(retained: &Retained, file: FileId, byte: usize) -> Option<(Range, String)> {
    let (span, ty) = smallest_expr(&retained.index, file, byte)?;
    let range = span_range(&retained.session.source_map, span)?;
    let analysis = retained.session.analysis()?;
    Some((
        range,
        format!("```lilac\n{}\n```", analysis.types.display_ref(ty)),
    ))
}

/// Joins rendered type refs with `, `.
fn join_types(types: &Types, refs: &[TypeRef]) -> String {
    refs.iter()
        .map(|ty| types.display_ref(*ty))
        .collect::<Vec<_>>()
        .join(", ")
}
