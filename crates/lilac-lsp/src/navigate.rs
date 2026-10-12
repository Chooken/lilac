//! Navigation: references, highlights, rename, type definition and symbols.
//!
//! A name is an item ([`DefId`], through the index's `defs` and `uses`), a
//! local (its declaration's name span, through the index's `locals`), or a
//! `using … as` alias (its declaration's name span, through `aliases`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lilac_hir::HoleSite;
use lilac_resolve::DefId;
use lilac_span::{FileId, Span};
use lilac_ty::TypeKind;
use lsp_types::{
    DocumentHighlight, DocumentHighlightKind, DocumentSymbol, Location, Position, Range,
    SymbolKind, TextEdit, Url, WorkspaceEdit, WorkspaceSymbol,
};

use crate::convert::{span_range, LineIndex};
use crate::handlers::{contains, file_for, smallest_expr};
use crate::index::{DefInfo, Retained};
use crate::roots::url_to_path;

/// What a name denotes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// An item: function, hook, type, field, variant or constant.
    Item(DefId),
    /// A local or parameter, by its declaration's name span.
    Local(Span),
    /// A `using … as` alias, by its name's span in the `using`.
    Alias(Span),
}

/// The name under `position` in `uri` and what it denotes.
#[must_use]
pub fn target_at(retained: &Retained, uri: &Url, position: Position) -> Option<(Span, Target)> {
    let file = file_for(retained, uri)?;
    let source = retained.session.source_map.file(file)?;
    let byte = LineIndex::of(source).offset(position)?;
    target_in(retained, file, byte)
}

/// The name at `byte` of `file`: a local, an alias, an item use, an
/// f-string hole's braces (its `@format` hook), else an item's declared
/// name.
#[must_use]
pub fn target_in(retained: &Retained, file: FileId, byte: usize) -> Option<(Span, Target)> {
    let index = &retained.index;
    if let Some(site) = index
        .locals
        .iter()
        .filter(|site| site.span.file == file && contains(site.span, byte))
        .min_by_key(|site| site.span.hi.0 - site.span.lo.0)
    {
        return Some((site.span, Target::Local(site.decl)));
    }
    if let Some(site) = index
        .aliases
        .iter()
        .find(|site| site.span.file == file && contains(site.span, byte))
    {
        return Some((site.span, Target::Alias(site.decl)));
    }
    if let Some(site) = smallest_use(retained, file, byte) {
        return Some((site.0, Target::Item(site.1)));
    }
    if let Some(site) = holes_at(retained, file, byte).first() {
        return Some((site.span, Target::Item(site.hook)));
    }
    index
        .defs
        .iter()
        .filter(|(_, info)| info.name_span.file == file && contains(info.name_span, byte))
        .min_by_key(|(id, info)| (info.name_span.hi.0 - info.name_span.lo.0, id.0))
        .map(|(id, info)| (info.name_span, Target::Item(*id)))
}

/// The smallest resolved use containing `byte`. A span naming two items
/// (a `.Variant` tag names its enum and the variant) prefers the item whose
/// name is the span's text.
#[must_use]
pub fn smallest_use(retained: &Retained, file: FileId, byte: usize) -> Option<(Span, DefId)> {
    let index = &retained.index;
    index
        .uses
        .iter()
        .filter(|site| site.span.file == file && contains(site.span, byte))
        .min_by_key(|site| {
            let named = index
                .defs
                .get(&site.def)
                .is_some_and(|info| text(retained, site.span) == Some(info.name.as_str()));
            (site.span.hi.0 - site.span.lo.0, !named, site.span.lo.0)
        })
        .map(|site| (site.span, site.def))
}

/// The f-string holes whose braces (or the space inside them, but not
/// their value) hold `byte`: the innermost hole, once per instance of a
/// generic body that formats it differently.
#[must_use]
pub fn holes_at(retained: &Retained, file: FileId, byte: usize) -> Vec<&HoleSite> {
    let on_hole = |site: &&HoleSite| {
        site.span.file == file && contains(site.span, byte) && !contains(site.value, byte)
    };
    let holes = &retained.index.holes;
    let Some(innermost) = holes
        .iter()
        .filter(on_hole)
        .map(|site| site.span)
        .min_by_key(|span| span.hi.0 - span.lo.0)
    else {
        return Vec::new();
    };
    holes.iter().filter(|site| site.span == innermost).collect()
}

/// The source text of `span`.
fn text(retained: &Retained, span: Span) -> Option<&str> {
    let source = retained.session.source_map.file(span.file)?;
    source.src.get(span.lo.0 as usize..span.hi.0 as usize)
}

/// Every span naming `target`, its declaration first, in source order.
#[must_use]
pub fn reference_spans(retained: &Retained, target: Target) -> Vec<Span> {
    let index = &retained.index;
    let mut spans = match target {
        Target::Item(def) => {
            let mut spans: Vec<Span> = index
                .defs
                .get(&def)
                .map(|info| info.name_span)
                .filter(|span| !span.is_dummy())
                .into_iter()
                .collect();
            spans.extend(
                index
                    .uses
                    .iter()
                    .filter(|site| site.def == def)
                    .map(|site| site.span),
            );
            spans.extend(
                index
                    .holes
                    .iter()
                    .filter(|site| site.hook == def)
                    .map(|site| site.span),
            );
            spans
        }
        Target::Local(decl) => index
            .locals
            .iter()
            .filter(|site| site.decl == decl)
            .map(|site| site.span)
            .collect(),
        Target::Alias(decl) => index
            .aliases
            .iter()
            .filter(|site| site.decl == decl)
            .map(|site| site.span)
            .collect(),
    };
    let declaration = match target {
        Target::Item(def) => index.defs.get(&def).map(|info| info.name_span),
        Target::Local(decl) | Target::Alias(decl) => Some(decl),
    };
    spans.sort_by_key(|span| (Some(*span) != declaration, span.file.0, span.lo.0));
    spans.dedup();
    spans
}

/// `textDocument/references`: every location naming what is under
/// `position`.
#[must_use]
pub fn references(
    retained: &Retained,
    uri: &Url,
    position: Position,
    include_declaration: bool,
) -> Vec<Location> {
    let Some((_, target)) = target_at(retained, uri, position) else {
        return Vec::new();
    };
    let spans = reference_spans(retained, target);
    let skip = usize::from(!include_declaration && is_declared(retained, target, &spans));
    spans
        .into_iter()
        .skip(skip)
        .filter_map(|span| location(retained, span))
        .collect()
}

/// Whether `spans` (from [`reference_spans`]) starts with the declaration.
fn is_declared(retained: &Retained, target: Target, spans: &[Span]) -> bool {
    let declaration = match target {
        Target::Item(def) => retained.index.defs.get(&def).map(|info| info.name_span),
        Target::Local(decl) | Target::Alias(decl) => Some(decl),
    };
    spans.first().copied() == declaration
}

/// `textDocument/documentHighlight`: the names in `uri` denoting what is
/// under `position`. The declaration is a write, every other name a read.
#[must_use]
pub fn highlights(retained: &Retained, uri: &Url, position: Position) -> Vec<DocumentHighlight> {
    let Some(file) = file_for(retained, uri) else {
        return Vec::new();
    };
    let Some((_, target)) = target_at(retained, uri, position) else {
        return Vec::new();
    };
    let spans = reference_spans(retained, target);
    let declared = is_declared(retained, target, &spans);
    spans
        .iter()
        .enumerate()
        .filter(|(_, span)| span.file == file)
        .filter_map(|(at, span)| {
            Some(DocumentHighlight {
                range: span_range(&retained.session.source_map, *span)?,
                kind: Some(if at == 0 && declared {
                    DocumentHighlightKind::WRITE
                } else {
                    DocumentHighlightKind::READ
                }),
            })
        })
        .collect()
}

/// The location of `span`, when its file is a document.
fn location(retained: &Retained, span: Span) -> Option<Location> {
    Some(Location {
        uri: retained.file_uris.get(&span.file)?.clone(),
        range: span_range(&retained.session.source_map, span)?,
    })
}

/// Go-to-definition for a local or an alias: its declaration.
#[must_use]
pub fn local_definition(retained: &Retained, uri: &Url, position: Position) -> Option<Location> {
    match target_at(retained, uri, position)? {
        (_, Target::Local(decl) | Target::Alias(decl)) => location(retained, decl),
        (_, Target::Item(_)) => None,
    }
}

/// `textDocument/typeDefinition`: the object or enum declaring the type of
/// what is under `position`.
#[must_use]
pub fn type_definition(retained: &Retained, uri: &Url, position: Position) -> Option<Location> {
    let file = file_for(retained, uri)?;
    let source = retained.session.source_map.file(file)?;
    let byte = LineIndex::of(source).offset(position)?;
    let index = &retained.index;
    let target = target_in(retained, file, byte);
    if let Some((_, Target::Alias(_))) = target {
        return None;
    }
    let named = match target {
        Some((_, Target::Item(def))) => match index.defs.get(&def)?.kind {
            "object" | "enum" => Some(def),
            "variant" => index.defs.get(&def)?.parent,
            "alias" => {
                let ty = retained.session.analysis()?.types.alias(def)?;
                Some(type_def(retained, ty)?)
            }
            _ => None,
        },
        _ => None,
    };
    let hole = holes_at(retained, file, byte).first().map(|site| site.ty);
    let def = if let Some(def) = named {
        def
    } else if let Some(ty) = hole {
        // On a hole's braces: the type it formats.
        type_def(retained, ty)?
    } else {
        let at_cursor = || smallest_expr(index, file, byte).map(|(_, ty)| ty);
        let ty = if let Some((_, Target::Local(decl))) = target {
            // A local's declaration is no expression; any use of it is.
            index
                .locals
                .iter()
                .filter(|site| site.decl == decl)
                .find_map(|site| {
                    index
                        .exprs
                        .iter()
                        .find(|(span, _)| *span == site.span)
                        .map(|(_, ty)| *ty)
                })
                .or_else(at_cursor)
        } else {
            at_cursor()
        }?;
        type_def(retained, ty.id)?
    };
    let info = index.defs.get(&def)?;
    location(retained, info.name_span)
}

/// The object or enum a type is built from (through arrays).
fn type_def(retained: &Retained, mut ty: lilac_ty::TypeId) -> Option<DefId> {
    let types = &retained.session.analysis()?.types;
    loop {
        match types.kind(ty) {
            TypeKind::Object(def) => return Some(*def),
            TypeKind::Instance { base, .. } => return Some(*base),
            TypeKind::Array { elem, .. } => ty = *elem,
            _ => return None,
        }
    }
}

/// Why a rename is refused.
pub type Refusal = String;

/// `textDocument/prepareRename`: the name under `position` and its range, or
/// why it cannot be renamed. `libraries` are the mounted library folders.
///
/// # Errors
///
/// When the name is a hook, or an edit would fall in the builtins or in a
/// library other than the one holding `uri`.
pub fn prepare_rename(
    retained: &Retained,
    uri: &Url,
    position: Position,
    libraries: &[(String, PathBuf)],
) -> Result<Option<Range>, Refusal> {
    let Some((span, target)) = target_at(retained, uri, position) else {
        return Ok(None);
    };
    renamable(retained, uri, target, libraries)?;
    Ok(span_range(&retained.session.source_map, span))
}

/// `textDocument/rename`: renames what is under `position` to `new_name`.
///
/// # Errors
///
/// As [`prepare_rename`], or when `new_name` is no identifier.
pub fn rename(
    retained: &Retained,
    uri: &Url,
    position: Position,
    new_name: &str,
    libraries: &[(String, PathBuf)],
) -> Result<Option<WorkspaceEdit>, Refusal> {
    let Some((span, target)) = target_at(retained, uri, position) else {
        return Ok(None);
    };
    if !crate::context::is_identifier(new_name) {
        return Err(format!("`{new_name}` is not an identifier"));
    }
    let spans = renamable(retained, uri, target, libraries)?;
    let Some(old) = text(retained, span) else {
        return Ok(None);
    };
    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    for span in spans {
        // Only a name spelled out is renamed (a `.Variant` tag also records
        // its enum).
        if text(retained, span) != Some(old) {
            continue;
        }
        if let Some(location) = location(retained, span) {
            changes.entry(location.uri).or_default().push(TextEdit {
                range: location.range,
                new_text: new_name.to_owned(),
            });
        }
    }
    Ok(Some(WorkspaceEdit {
        changes: Some(changes),
        ..WorkspaceEdit::default()
    }))
}

/// The spans a rename of `target` edits, or why it is refused.
fn renamable(
    retained: &Retained,
    uri: &Url,
    target: Target,
    libraries: &[(String, PathBuf)],
) -> Result<Vec<Span>, Refusal> {
    if let Target::Item(def) = target {
        let info = retained.index.defs.get(&def).ok_or("unknown definition")?;
        if info.kind == "hook" {
            return Err(format!("`{}` is a hook; its name is fixed", info.name));
        }
    }
    let spans = reference_spans(retained, target);
    let home = url_to_path(uri).and_then(|path| library_of(&path, libraries));
    for span in &spans {
        let Some(path) = retained.file_uris.get(&span.file).and_then(url_to_path) else {
            return Err("it is defined by the compiler".to_owned());
        };
        if let Some(library) = library_of(&path, libraries) {
            if home.as_deref() != Some(library.as_str()) {
                return Err(format!("it is used in the library `{library}`"));
            }
        }
    }
    Ok(spans)
}

/// The mounted library holding `path`, by name.
fn library_of(path: &Path, libraries: &[(String, PathBuf)]) -> Option<String> {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let path = canonical(path);
    libraries
        .iter()
        .find(|(_, dir)| path.starts_with(canonical(dir)))
        .map(|(name, _)| name.clone())
}

/// `textDocument/documentSymbol`: `uri`'s items, members nested under their
/// object or enum.
#[must_use]
pub fn document_symbols(retained: &Retained, uri: &Url) -> Vec<DocumentSymbol> {
    let Some(file) = file_for(retained, uri) else {
        return Vec::new();
    };
    let mut items: Vec<(&DefId, &DefInfo)> = retained
        .index
        .defs
        .iter()
        .filter(|(_, info)| info.span.file == file && !info.span.is_dummy())
        .collect();
    items.sort_by_key(|(_, info)| info.span.lo.0);
    let mut children: HashMap<DefId, Vec<DocumentSymbol>> = HashMap::new();
    let mut top = Vec::new();
    // Members first, so their owners can take them.
    for (id, info) in items.iter().filter(|(_, info)| info.parent.is_some()) {
        if let (Some(parent), Some(symbol)) = (info.parent, symbol(retained, **id, info)) {
            children.entry(parent).or_default().push(symbol);
        }
    }
    for (id, info) in items.iter().filter(|(_, info)| info.parent.is_none()) {
        if let Some(mut symbol) = symbol(retained, **id, info) {
            symbol.children = children.remove(id);
            top.push(symbol);
        }
    }
    top
}

/// One outline entry for `info`.
fn symbol(retained: &Retained, id: DefId, info: &DefInfo) -> Option<DocumentSymbol> {
    let sources = &retained.session.source_map;
    let range = span_range(sources, info.span)?;
    let selection = span_range(sources, info.name_span)
        .filter(|name| range.start <= name.start && name.end <= range.end)
        .unwrap_or(range);
    #[allow(deprecated)]
    Some(DocumentSymbol {
        name: info.name.clone(),
        detail: crate::handlers::signature(retained, id, info),
        kind: symbol_kind(info),
        tags: None,
        deprecated: None,
        range,
        selection_range: selection,
        children: None,
    })
}

/// The LSP symbol kind of a definition.
fn symbol_kind(info: &DefInfo) -> SymbolKind {
    match info.kind {
        "function" if info.parent.is_some() => SymbolKind::METHOD,
        "function" => SymbolKind::FUNCTION,
        "hook" => SymbolKind::OPERATOR,
        "object" => SymbolKind::STRUCT,
        "enum" => SymbolKind::ENUM,
        "alias" => SymbolKind::TYPE_PARAMETER,
        "const" => SymbolKind::CONSTANT,
        "global" => SymbolKind::VARIABLE,
        "field" => SymbolKind::FIELD,
        _ => SymbolKind::ENUM_MEMBER,
    }
}

/// `workspace/symbol` over one compile: every item in a document whose name
/// contains `query`'s characters in order (ignoring case).
#[must_use]
pub fn workspace_symbols(retained: &Retained, query: &str) -> Vec<WorkspaceSymbol> {
    let query = query.to_lowercase();
    retained
        .index
        .defs
        .values()
        .filter(|info| matches_query(&info.name.to_lowercase(), &query))
        .filter_map(|info| {
            let location = location(retained, info.name_span)?;
            Some(WorkspaceSymbol {
                name: info.name.clone(),
                kind: symbol_kind(info),
                tags: None,
                container_name: info
                    .qualified
                    .rsplit_once('.')
                    .map(|(container, _)| container.to_owned()),
                location: lsp_types::OneOf::Left(location),
                data: None,
            })
        })
        .collect()
}

/// Whether `query`'s characters occur in `name` in order.
fn matches_query(name: &str, query: &str) -> bool {
    let mut name = name.chars();
    query.chars().all(|want| name.any(|ch| ch == want))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_is_a_subsequence() {
        assert!(matches_query("unwrap_or_default", "uod"));
        assert!(matches_query("anything", ""));
        assert!(!matches_query("main", "mx"));
    }
}
