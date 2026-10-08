//! Editor request handlers: hover, go-to-definition, and semantic tokens.

use lilac_hir::{NameSite, SemanticKind};
use lilac_span::{FileId, Span};
use lilac_ty::{TypeRef, Types};
use lsp_types::{
    GotoDefinitionResponse, Hover, HoverContents, Location, MarkupContent, MarkupKind, Position,
    Range, SemanticToken, SemanticTokens, Url,
};

use crate::convert::{byte_offset, line_len_utf16, span_range};
use crate::index::{AnalysisIndex, Retained};
use crate::roots::canonical_file_uri;

/// Legend token-type indices, in the order advertised by `initialize`.
const TOKEN_FUNCTION: u32 = 0;
const TOKEN_TYPE: u32 = 1;
const TOKEN_ENUM: u32 = 2;
const TOKEN_PROPERTY: u32 = 3;
const TOKEN_ENUM_MEMBER: u32 = 4;
const TOKEN_VARIABLE: u32 = 5;
const TOKEN_PARAMETER: u32 = 6;

/// Legend modifier index 0 (`defaultLibrary`) as a bitset.
const MODIFIER_DEFAULT_LIBRARY: u32 = 1 << 0;

/// Hover for the item or expression at `position`, if any.
#[must_use]
pub fn hover(retained: &Retained, uri: &Url, position: Position) -> Option<Hover> {
    let file = file_for(retained, uri)?;
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

/// Go-to-definition for the item use at `position`, if any.
#[must_use]
pub fn definition(
    retained: &Retained,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let file = file_for(retained, uri)?;
    let source = retained.session.source_map.file(file)?;
    let byte = byte_offset(&source.src, position)?;
    let site = smallest_use(&retained.index, file, byte)?;
    let info = retained.index.defs.get(&site.def)?;
    let target = retained.file_uris.get(&info.span.file)?;
    let range = span_range(&retained.session.source_map, info.span)?;
    Some(GotoDefinitionResponse::Scalar(Location {
        uri: target.clone(),
        range,
    }))
}

/// Resolves a client document URI to its `FileId`, tolerating the client's
/// `file:` spelling (`VSCode` sends `file:///c%3A/...`; the server stores
/// `file:///C:/...`). The raw key is tried first so single-file mode, whose
/// map is keyed by the `didOpen` URI, keeps working.
fn file_for(retained: &Retained, uri: &Url) -> Option<FileId> {
    retained
        .uri_files
        .get(uri)
        .or_else(|| retained.uri_files.get(&canonical_file_uri(uri)))
        .copied()
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
            let sig = analysis.sigs.get(&site.def)?;
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

    let source = sources.file(file).map_or("", |known| known.src.as_str());
    encode(raw, source)
}

/// Sorts `raw` by start, drops overlaps, and delta-encodes it as LSP
/// `SemanticTokens`. Multi-line spans are clamped to their first line (a
/// semantic token may not cross a newline); lengths are UTF-16 code units.
fn encode(mut raw: Vec<(Range, u32, u32)>, src: &str) -> SemanticTokens {
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
            range.end = Position::new(range.start.line, line_len_utf16(src, range.start.line));
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
        "object" => TOKEN_TYPE,
        "enum" => TOKEN_ENUM,
        "field" => TOKEN_PROPERTY,
        "const" => TOKEN_VARIABLE,
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
            colored(&sources, Span::new(0, 7, file), "object").expect("range");
        let tokens = encode(vec![(range, token_type, 0)], src);
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
            colored(&sources, Span::new(2, 5, file), "object").expect("range");
        assert_eq!(range.start, Position::new(0, 1));
        assert_eq!(range.end, Position::new(0, 4));
        let tokens = encode(vec![(range, token_type, 0)], src);
        assert_eq!(tokens.data.len(), 1);
        assert_eq!(tokens.data[0].length, 3); // "foo"
    }
}
