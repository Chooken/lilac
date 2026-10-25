//! Editor request handlers: hover, go-to-definition, and semantic tokens.
//! Navigation (references, rename, symbols) is in [`crate::navigate`].

use lilac_hir::intrinsics::Intrinsic;
use lilac_hir::{HoleSite, SemanticKind};
use lilac_resolve::DefId;
use lilac_span::{FileId, Span};
use lilac_ty::TypeRef;
use lsp_types::{
    GotoDefinitionResponse, Hover, HoverContents, Location, MarkupContent, MarkupKind, Position,
    Range, SemanticToken, SemanticTokens, Url,
};

use crate::convert::{span_range, LineIndex};
use crate::index::{AnalysisIndex, Retained};
use crate::navigate::{declaration_at, holes_at, smallest_use, target_in, Target};
use crate::render;
use crate::roots::canonical_file_uri;

/// Legend token-type indices, in the order advertised by `initialize`.
const TOKEN_FUNCTION: u32 = 0;
const TOKEN_TYPE: u32 = 1;
const TOKEN_ENUM: u32 = 2;
const TOKEN_PROPERTY: u32 = 3;
const TOKEN_ENUM_MEMBER: u32 = 4;
const TOKEN_VARIABLE: u32 = 5;
const TOKEN_PARAMETER: u32 = 6;
const TOKEN_NAMESPACE: u32 = 7;

/// Legend modifier index 0 (`defaultLibrary`) as a bitset.
const MODIFIER_DEFAULT_LIBRARY: u32 = 1 << 0;

/// Hover for the intrinsic, item (a use or its declared name) or
/// expression at `position`, if any.
#[must_use]
pub fn hover(retained: &Retained, uri: &Url, position: Position) -> Option<Hover> {
    let file = file_for(retained, uri)?;
    let source = retained.session.source_map.file(file)?;
    let byte = LineIndex::of(source).offset(position)?;
    let sources = &retained.session.source_map;

    let item = || {
        let (span, def) =
            smallest_use(retained, file, byte).or_else(|| declaration_at(retained, file, byte))?;
        Some((span_range(sources, span)?, item_hover(retained, def)?))
    };
    let (range, value) = intrinsic_hover(retained, file, byte)
        .or_else(|| builtin_type_hover(retained, file, byte))
        .or_else(|| alias_hover(retained, file, byte))
        .or_else(item)
        .or_else(|| hole_hover(retained, file, byte))
        .or_else(|| expr_hover(retained, file, byte))?;

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(range),
    })
}

/// Go-to-definition for the item use or local at `position`, if any; on an
/// f-string hole's braces, its `@format` hooks.
#[must_use]
pub fn definition(
    retained: &Retained,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    if let Some(location) = crate::navigate::local_definition(retained, uri, position) {
        return Some(GotoDefinitionResponse::Scalar(location));
    }
    let file = file_for(retained, uri)?;
    let source = retained.session.source_map.file(file)?;
    let byte = LineIndex::of(source).offset(position)?;
    if let Some((_, def)) = smallest_use(retained, file, byte) {
        return declaration(retained, def).map(GotoDefinitionResponse::Scalar);
    }
    let mut hooks: Vec<DefId> = holes_at(retained, file, byte)
        .iter()
        .map(|site| site.hook)
        .collect();
    hooks.sort_by_key(|def| def.0);
    hooks.dedup();
    let mut locations: Vec<Location> = hooks
        .into_iter()
        .filter_map(|def| declaration(retained, def))
        .collect();
    match locations.len() {
        0 => None,
        1 => locations.pop().map(GotoDefinitionResponse::Scalar),
        _ => Some(GotoDefinitionResponse::Array(locations)),
    }
}

/// Where `def` is declared (its whole declaration).
fn declaration(retained: &Retained, def: DefId) -> Option<Location> {
    let info = retained.index.defs.get(&def)?;
    let uri = retained.file_uris.get(&info.span.file)?;
    let range = span_range(&retained.session.source_map, info.span)?;
    Some(Location {
        uri: uri.clone(),
        range,
    })
}

/// Resolves a client document URI to its `FileId`, tolerating the client's
/// `file:` spelling (`VSCode` sends `file:///c%3A/...`; the server stores
/// `file:///C:/...`). The raw key is tried first so single-file mode, whose
/// map is keyed by the `didOpen` URI, keeps working.
pub(crate) fn file_for(retained: &Retained, uri: &Url) -> Option<FileId> {
    retained
        .uri_files
        .get(uri)
        .or_else(|| retained.uri_files.get(&canonical_file_uri(uri)))
        .copied()
}

/// The smallest checked expression whose span contains `byte`.
pub(crate) fn smallest_expr(
    index: &AnalysisIndex,
    file: FileId,
    byte: usize,
) -> Option<(Span, TypeRef)> {
    index
        .exprs
        .iter()
        .filter(|(span, _)| span.file == file && contains(*span, byte))
        .min_by_key(|(span, _)| (span.hi.0 - span.lo.0, span.lo.0))
        .copied()
}

/// Whether a half-open span contains `byte`.
pub(crate) fn contains(span: Span, byte: usize) -> bool {
    let Ok(byte) = u32::try_from(byte) else {
        return false;
    };
    span.lo.0 <= byte && byte < span.hi.0
}

/// Renders a hover for a resolved item use.
fn item_hover(retained: &Retained, def: DefId) -> Option<String> {
    let analysis = retained.session.analysis()?;
    Some(render::item_markdown(&analysis.resolved, def))
}

/// Renders a hover for a bare expression (its type). A hole's whole value
/// shows the type it is formatted as (the hole borrows it) and its hook.
fn expr_hover(retained: &Retained, file: FileId, byte: usize) -> Option<(Range, String)> {
    let (span, ty) = smallest_expr(&retained.index, file, byte)?;
    let range = span_range(&retained.session.source_map, span)?;
    let analysis = retained.session.analysis()?;
    let holes: Vec<&HoleSite> = retained
        .index
        .holes
        .iter()
        .filter(|site| site.value == span)
        .collect();
    let shown = match holes.first() {
        Some(site) => analysis.types.display(site.ty),
        None => analysis.types.display_ref(ty),
    };
    let mut text = format!("```lilac\n{shown}\n```");
    let mut hooks: Vec<DefId> = holes.iter().map(|site| site.hook).collect();
    hooks.sort_by_key(|def| def.0);
    hooks.dedup();
    for hook in hooks {
        if let Some(info) = retained.index.defs.get(&hook) {
            text.push_str("\n\nformatted by `");
            text.push_str(&info.qualified);
            text.push('`');
        }
    }
    Some((range, text))
}

/// Renders a hover for an f-string hole's braces: each type it formats
/// (one per instance of a generic body that formats it differently) and
/// the hook that formats it.
fn hole_hover(retained: &Retained, file: FileId, byte: usize) -> Option<(Range, String)> {
    let sites = holes_at(retained, file, byte);
    let range = span_range(&retained.session.source_map, sites.first()?.span)?;
    let analysis = retained.session.analysis()?;
    let parts: Vec<String> = sites
        .iter()
        .map(|site| {
            let hook = render::declaration(&analysis.resolved, site.hook, 0).text;
            let shown = analysis.types.display(site.ty);
            format!("```lilac\n{shown}\n```\nformatted by\n```lilac\n{hook}\n```")
        })
        .collect();
    Some((range, parts.join("\n\n---\n\n")))
}

/// Renders a hover for a `using … as` alias (a use or its declaration): the
/// namespace it names.
fn alias_hover(retained: &Retained, file: FileId, byte: usize) -> Option<(Range, String)> {
    let (span, Target::Alias(decl)) = target_in(retained, file, byte)? else {
        return None;
    };
    let resolved = &retained.session.analysis()?.resolved;
    let (ns, _) = resolved
        .files
        .get(&decl.file)?
        .aliases
        .values()
        .find(|(_, declared)| *declared == decl)?;
    let range = span_range(&retained.session.source_map, span)?;
    let path = resolved.namespace_path(*ns);
    Some((range, format!("```lilac\nnamespace {path}\n```")))
}

/// The builtin types that take operands, with their shapes (completion and
/// hover).
pub(crate) const TYPE_CONSTRUCTORS: [(&str, &str); 2] = [
    ("@array", "@array(T, N): `N` values of `T`, in place"),
    (
        "@format_args",
        "@format_args(W): an f-string waiting to be written into a `W`",
    ),
];

/// Renders a hover for a builtin type: its name and what it is, or for one
/// that takes operands (`@array`, `@format_args`) its shape.
fn builtin_type_hover(retained: &Retained, file: FileId, byte: usize) -> Option<(Range, String)> {
    let site = retained.index.semantics.iter().find(|site| {
        site.kind == SemanticKind::BuiltinType
            && site.span.file == file
            && contains(site.span, byte)
    })?;
    let source = retained.session.source_map.file(file)?;
    let name = source
        .src
        .get(site.span.lo.0 as usize..site.span.hi.0 as usize)?;
    let range = span_range(&retained.session.source_map, site.span)?;
    if let Some(builtin) = lilac_ty::Builtin::named(name) {
        return Some((range, render::markdown(name, None, Some(builtin.doc()))));
    }
    let (_, shape) = TYPE_CONSTRUCTORS.iter().find(|(known, _)| *known == name)?;
    let (head, prose) = shape.split_once(": ")?;
    Some((range, render::markdown(head, None, Some(prose))))
}

/// Renders a hover for an intrinsic's name: its signature.
fn intrinsic_hover(retained: &Retained, file: FileId, byte: usize) -> Option<(Range, String)> {
    let site = retained.index.semantics.iter().find(|site| {
        site.kind == SemanticKind::BuiltinFunc
            && site.span.file == file
            && contains(site.span, byte)
    })?;
    let source = retained.session.source_map.file(file)?;
    let name = source
        .src
        .get(site.span.lo.0 as usize..site.span.hi.0 as usize)?;
    let intrinsic = Intrinsic::lookup(name)?;
    let range = span_range(&retained.session.source_map, site.span)?;
    let rendered = render::intrinsic(name, intrinsic);
    Some((
        range,
        render::markdown(&rendered.text, None, Some(&intrinsic.doc())),
    ))
}

/// Semantic tokens for `uri`'s document, derived from the retained analysis.
///
/// Merges the index's item declarations (`defs`), resolved item uses (`uses`),
/// and the analysis's non-item sites (`semantics`); sorts by `(line, col)`,
/// drops overlaps, and delta-encodes with UTF-16 lengths.
#[must_use]
pub fn semantic_tokens(retained: &Retained, uri: &Url) -> SemanticTokens {
    let Some(file) = file_for(retained, uri) else {
        return SemanticTokens::default();
    };
    let sources = &retained.session.source_map;

    // A span in `semantics` is the more specific source; an item use that
    // overlaps one (a `.Variant` tag, which resolves to its owning enum) must
    // not also color as the item. Overlap (not just equality) handles the tag,
    // whose semantic span is the tag token inside the larger `.Tag` use span.
    let semantic_ranges: Vec<(u32, u32)> = retained
        .index
        .semantics
        .iter()
        .filter(|site| site.span.file == file)
        .map(|site| (site.span.lo.0, site.span.hi.0))
        .collect();

    let mut raw: Vec<(Range, u32, u32)> = Vec::new();
    for info in retained.index.defs.values() {
        if info.name_span.file != file {
            continue;
        }
        if let Some((range, token_type)) = colored(sources, info.name_span, info.kind) {
            raw.push((range, token_type, 0));
        }
    }
    for site in &retained.index.uses {
        if site.span.file != file
            || semantic_ranges
                .iter()
                .any(|&(lo, hi)| site.span.lo.0 < hi && lo < site.span.hi.0)
        {
            continue;
        }
        let Some(info) = retained.index.defs.get(&site.def) else {
            continue;
        };
        if let Some((range, token_type)) = colored(sources, site.span, info.kind) {
            raw.push((range, token_type, 0));
        }
    }
    for site in &retained.index.semantics {
        if site.span.file != file {
            continue;
        }
        let (token_type, modifiers) = semantic_type(site.kind);
        if let Some(range) = span_range(sources, site.span) {
            raw.push((range, token_type, modifiers));
        }
    }

    let lines = sources
        .file(file)
        .map_or_else(|| LineIndex::new(""), LineIndex::of);
    encode(raw, &lines)
}

/// Sorts `raw` by start, drops overlaps, and delta-encodes it as LSP
/// `SemanticTokens`. Multi-line spans are clamped to their first line (a
/// semantic token may not cross a newline); lengths are UTF-16 code units.
fn encode(mut raw: Vec<(Range, u32, u32)>, lines: &LineIndex) -> SemanticTokens {
    raw.sort_by_key(|(range, token_type, modifiers)| {
        (
            range.start.line,
            range.start.character,
            range.end.line,
            range.end.character,
            *token_type,
            *modifiers,
        )
    });

    let mut data: Vec<SemanticToken> = Vec::new();
    let mut occupied: Option<(u32, u32)> = None;
    let mut last_line = 0u32;
    let mut last_start = 0u32;
    for (mut range, token_type, modifiers) in raw {
        if range.end.line != range.start.line {
            range.end = Position::new(range.start.line, lines.line_len_utf16(range.start.line));
        }
        let start = (range.start.line, range.start.character);
        let end = (range.end.line, range.end.character);
        // Keep only non-empty tokens that do not overlap the previous one.
        if start >= end || occupied.is_some_and(|previous| start < previous) {
            continue;
        }
        let length = range.end.character.saturating_sub(range.start.character);
        let delta_line = range.start.line - last_line;
        let delta_start = if delta_line == 0 {
            range.start.character - last_start
        } else {
            range.start.character
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: modifiers,
        });
        last_line = range.start.line;
        last_start = range.start.character;
        occupied = Some((range.end.line, range.end.character));
    }

    SemanticTokens {
        result_id: None,
        data,
    }
}

/// The legend `(token type, modifier bitset)` for a `DefInfo.kind`, or `None`
/// for kinds that have no token (there are none today).
fn colored(sources: &lilac_span::SourceMap, span: Span, kind: &str) -> Option<(Range, u32)> {
    let token_type = match kind {
        "function" | "hook" => TOKEN_FUNCTION,
        "structure" | "alias" => TOKEN_TYPE,
        "either" => TOKEN_ENUM,
        "field" => TOKEN_PROPERTY,
        "const" | "global" => TOKEN_VARIABLE,
        _ => return None,
    };
    span_range(sources, span).map(|range| (range, token_type))
}

/// The legend `(token type, modifier bitset)` for a non-item [`SemanticKind`].
fn semantic_type(kind: SemanticKind) -> (u32, u32) {
    match kind {
        SemanticKind::Local => (TOKEN_VARIABLE, 0),
        SemanticKind::Param => (TOKEN_PARAMETER, 0),
        SemanticKind::UserType => (TOKEN_TYPE, 0),
        SemanticKind::Variant => (TOKEN_ENUM_MEMBER, 0),
        SemanticKind::BuiltinType => (TOKEN_TYPE, MODIFIER_DEFAULT_LIBRARY),
        SemanticKind::BuiltinFunc => (TOKEN_FUNCTION, MODIFIER_DEFAULT_LIBRARY),
        SemanticKind::Namespace => (TOKEN_NAMESPACE, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_span::SourceMap;

    #[test]
    fn encode_clamps_a_newline_crossing_span() {
        let src = "abc\ndef\n";
        let mut sources = SourceMap::new();
        let file = sources.add_file("main.lilac", src);
        let (range, token_type) =
            colored(&sources, Span::new(0, 7, file), "structure").expect("range");
        let tokens = encode(vec![(range, token_type, 0)], &LineIndex::new(src));
        assert_eq!(tokens.data.len(), 1);
        assert_eq!(tokens.data[0].length, 3); // "abc", not the part past the newline
        assert_eq!(tokens.data[0].delta_line, 0);
        assert_eq!(tokens.data[0].delta_start, 0);
    }

    #[test]
    fn encode_measures_utf16_lengths() {
        let src = "éfoo\n"; // 'é' is 2 bytes but one UTF-16 code unit
        let mut sources = SourceMap::new();
        let file = sources.add_file("main.lilac", src);
        let (range, token_type) =
            colored(&sources, Span::new(2, 5, file), "structure").expect("range");
        assert_eq!(range.start, Position::new(0, 1));
        assert_eq!(range.end, Position::new(0, 4));
        let tokens = encode(vec![(range, token_type, 0)], &LineIndex::new(src));
        assert_eq!(tokens.data.len(), 1);
        assert_eq!(tokens.data[0].length, 3); // "foo"
    }

    #[test]
    fn an_alias_colors_as_a_type() {
        let mut sources = SourceMap::new();
        let file = sources.add_file("main.lilac", "Spot\n");
        let (_, token_type) = colored(&sources, Span::new(0, 4, file), "alias").expect("range");
        assert_eq!(token_type, TOKEN_TYPE);
    }
}
