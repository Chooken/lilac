//! Diagnostic and position conversion.

use lilac_error::{Diagnostic, Level};
use lilac_span::{FileId, SourceMap, Span};
use lsp_types::{
    Diagnostic as LspDiagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location,
    NumberOrString, Position, Range, Url,
};

/// 0-based line and UTF-16-code-unit column for a byte offset.
#[must_use]
pub fn position(src: &str, byte: usize) -> Position {
    let byte = byte.min(src.len());
    let Some(before) = src.get(..byte) else {
        return Position::new(0, 0);
    };
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let line = before.matches('\n').count();
    let character = src[line_start..byte]
        .chars()
        .map(char::len_utf16)
        .sum::<usize>();
    Position::new(
        u32::try_from(line).unwrap_or(u32::MAX),
        u32::try_from(character).unwrap_or(u32::MAX),
    )
}

/// LSP range for a span, or `None` when it is dummy or its file is unknown.
fn span_range(sources: &SourceMap, span: Span) -> Option<Range> {
    if span.is_dummy() {
        return None;
    }
    let file = sources.file(span.file)?;
    Some(Range::new(
        position(&file.src, span.lo.0 as usize),
        position(&file.src, span.hi.0 as usize),
    ))
}

/// Converts a compiler diagnostic to an LSP diagnostic.
///
/// Returns `None` when the primary label points at a file the `SourceMap`
/// cannot resolve.
#[must_use]
pub fn to_lsp_diagnostic(
    diag: &Diagnostic,
    sources: &SourceMap,
    uri_of: &dyn Fn(FileId) -> Option<Url>,
) -> Option<LspDiagnostic> {
    let primary = diag
        .labels
        .iter()
        .find(|label| label.primary)
        .or_else(|| diag.labels.first());

    let unresolved = primary
        .is_some_and(|label| !label.span.is_dummy() && sources.file(label.span.file).is_none());
    if unresolved {
        return None;
    }

    let range = primary
        .and_then(|label| span_range(sources, label.span))
        .unwrap_or_else(|| Range::new(Position::new(0, 0), Position::new(0, 0)));

    let mut message = diag.msg.clone();
    for note in &diag.notes {
        message.push_str("\nnote: ");
        message.push_str(note);
    }
    for help in &diag.helps {
        message.push_str("\nhelp: ");
        message.push_str(help);
    }

    let related_information = diag
        .labels
        .iter()
        .filter(|label| !label.primary)
        .filter_map(|label| {
            let uri = uri_of(label.span.file)?;
            let range = span_range(sources, label.span)?;
            Some(DiagnosticRelatedInformation {
                location: Location { uri, range },
                message: label.msg.clone(),
            })
        })
        .collect::<Vec<_>>();

    Some(LspDiagnostic {
        range,
        severity: Some(match diag.level {
            Level::Error => DiagnosticSeverity::ERROR,
            Level::Warning => DiagnosticSeverity::WARNING,
        }),
        code: Some(NumberOrString::String(diag.code.to_owned())),
        code_description: None,
        source: Some("lilac".to_owned()),
        message,
        related_information: (!related_information.is_empty()).then_some(related_information),
        tags: None,
        data: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_error::Diagnostic;
    use lilac_span::{SourceMap, Span};
    use lsp_types::{DiagnosticSeverity, NumberOrString, Position, Range, Url};

    #[test]
    fn position_is_utf16_on_multibyte_lines() {
        assert_eq!(position("abc\ndef\n", 4), Position::new(1, 0));
        assert_eq!(position("é x", 3), Position::new(0, 2)); // 'é' is 1 UTF-16 unit
        assert_eq!(position("😀x", 4), Position::new(0, 2)); // emoji is 2 UTF-16 units
    }

    #[test]
    fn position_survives_crlf() {
        assert_eq!(position("a\r\nb", 3), Position::new(1, 0));
    }

    #[test]
    fn diagnostic_maps_code_range_and_source() {
        let mut sources = SourceMap::new();
        let file = sources.add_file("main.lilac", "abc\n");
        let diag = Diagnostic::error("E001", "boom").label(Span::new(0, 3, file), true, "here");
        let uri = Url::parse("file:///main.lilac").expect("uri");
        let lsp = to_lsp_diagnostic(&diag, &sources, &|id| (id == file).then(|| uri.clone()))
            .expect("mapped");
        assert_eq!(lsp.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(lsp.code, Some(NumberOrString::String("E001".to_owned())));
        assert_eq!(lsp.source.as_deref(), Some("lilac"));
        assert_eq!(
            lsp.range,
            Range::new(Position::new(0, 0), Position::new(0, 3))
        );
        assert!(lsp.message.contains("boom"));
    }

    #[test]
    fn diagnostic_without_primary_label_does_not_panic() {
        let mut sources = SourceMap::new();
        let _ = sources.add_file("main.lilac", "abc\n");
        let diag = Diagnostic::error("E000", "no location");
        let lsp = to_lsp_diagnostic(&diag, &sources, &|_| None).expect("mapped");
        assert_eq!(
            lsp.range,
            Range::new(Position::new(0, 0), Position::new(0, 0))
        );
    }
}
