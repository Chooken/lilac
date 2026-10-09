//! Diagnostic and position conversion.

use std::collections::HashSet;

use lilac_error::{Diagnostic, Level, TraceFrame};
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

/// Byte offset for a 0-based line and UTF-16-code-unit column.
///
/// Returns `None` when the line does not exist; a column past the line end
/// clamps to the line end.
#[must_use]
pub fn byte_offset(src: &str, position: Position) -> Option<usize> {
    let mut line_start = 0usize;
    let mut line = 0u32;
    while line < position.line {
        let newline = src[line_start..].find('\n')?;
        line_start += newline + 1;
        line += 1;
    }
    let line_end = src[line_start..]
        .find('\n')
        .map_or(src.len(), |offset| line_start + offset);
    let mut units = 0u32;
    for (offset, ch) in src[line_start..line_end].char_indices() {
        if units >= position.character {
            return Some(line_start + offset);
        }
        units += u32::try_from(ch.len_utf16()).unwrap_or(1);
    }
    Some(line_end)
}

/// UTF-16 code-unit length of `line` in `src`, excluding the newline.
///
/// Returns `0` when the line does not exist.
#[must_use]
pub(crate) fn line_len_utf16(src: &str, line: u32) -> u32 {
    let mut start = 0usize;
    let mut current = 0u32;
    while current < line {
        let Some(newline) = src[start..].find('\n') else {
            return 0;
        };
        start += newline + 1;
        current += 1;
    }
    let end = src[start..]
        .find('\n')
        .map_or(src.len(), |offset| start + offset);
    u32::try_from(src[start..end].chars().map(char::len_utf16).sum::<usize>()).unwrap_or(u32::MAX)
}

/// LSP range for a span, or `None` when it is dummy or its file is unknown.
pub(crate) fn span_range(sources: &SourceMap, span: Span) -> Option<Range> {
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

    let mut message = diag.msg.to_string();
    for note in &diag.notes {
        message.push_str("\nnote: ");
        message.push_str(note);
    }
    for help in &diag.helps {
        message.push_str("\nhelp: ");
        message.push_str(help);
    }

    let labels = diag
        .labels
        .iter()
        .filter(|label| !label.primary)
        .map(|label| (label.span, label.msg.clone()));
    let frames = callers(diag).map(|frame| (frame.span, format!("reached from {}", frame.func)));
    let related_information = labels
        .chain(frames)
        .filter_map(|(span, message)| {
            let uri = uri_of(span.file)?;
            let range = span_range(sources, span)?;
            Some(DiagnosticRelatedInformation {
                location: Location { uri, range },
                message,
            })
        })
        .collect::<Vec<_>>();

    Some(LspDiagnostic {
        range,
        severity: Some(severity(diag.level)),
        code: Some(NumberOrString::String(diag.code.to_owned())),
        code_description: None,
        source: Some("lilac".to_owned()),
        message,
        related_information: (!related_information.is_empty()).then_some(related_information),
        tags: None,
        data: None,
    })
}

/// The trace frames past the innermost one: the call sites that lead to the
/// error (the innermost frame's site is the diagnostic's own).
fn callers(diag: &Diagnostic) -> impl Iterator<Item = &TraceFrame> {
    diag.trace_roots()
        .iter()
        .flat_map(|frame| frame.callers.iter().flat_map(TraceFrame::walk))
}

/// One diagnostic per call site in `diag`'s trace, in that site's file, so
/// the code that triggers an error is marked too, not only the error. Each
/// links back to the error; a site is reported once, and sites `uri_of`
/// cannot place are skipped.
#[must_use]
pub fn trace_diagnostics(
    diag: &Diagnostic,
    sources: &SourceMap,
    uri_of: &dyn Fn(FileId) -> Option<Url>,
) -> Vec<(Url, LspDiagnostic)> {
    let origin = diag
        .labels
        .iter()
        .find(|label| label.primary)
        .and_then(|label| Some((uri_of(label.span.file)?, span_range(sources, label.span)?)));
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for frame in callers(diag) {
        if !seen.insert(frame.span) {
            continue;
        }
        let (Some(uri), Some(range)) = (uri_of(frame.span.file), span_range(sources, frame.span))
        else {
            continue;
        };
        let what = if frame.address {
            "this function reference leads to an error"
        } else {
            "this call leads to an error"
        };
        let related_information = origin.clone().map(|(uri, range)| {
            vec![DiagnosticRelatedInformation {
                location: Location { uri, range },
                message: diag.msg.to_string(),
            }]
        });
        let lsp = LspDiagnostic {
            range,
            severity: Some(severity(diag.level)),
            code: Some(NumberOrString::String(diag.code.to_owned())),
            code_description: None,
            source: Some("lilac".to_owned()),
            message: format!("{what}: {}", diag.msg),
            related_information,
            tags: None,
            data: None,
        };
        out.push((uri, lsp));
    }
    out
}

fn severity(level: Level) -> DiagnosticSeverity {
    match level {
        Level::Error => DiagnosticSeverity::ERROR,
        Level::Warning => DiagnosticSeverity::WARNING,
    }
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
    fn byte_offset_inverts_position() {
        let src = "abc\ndef\n";
        for byte in 0..=src.len() {
            if !src.is_char_boundary(byte) {
                continue;
            }
            assert_eq!(
                byte_offset(src, position(src, byte)),
                Some(byte),
                "byte {byte}"
            );
        }
    }

    #[test]
    fn byte_offset_clamps_and_rejects_unknown_lines() {
        assert_eq!(byte_offset("é x", Position::new(0, 2)), Some(3));
        assert_eq!(byte_offset("😀x", Position::new(0, 2)), Some(4));
        assert_eq!(byte_offset("abc\n", Position::new(0, 99)), Some(3));
        assert_eq!(byte_offset("abc\n", Position::new(5, 0)), None);
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
    fn trace_marks_each_call_site_once() {
        let mut sources = SourceMap::new();
        let lib = sources.add_file("lib.lilac", "boom\n");
        let user = sources.add_file("main.lilac", "a()\nb()\n");
        let frame = |func: &str, span: Span, callers: Vec<TraceFrame>| TraceFrame {
            func: func.to_owned(),
            span,
            address: false,
            repeat: false,
            callers,
        };
        let call = Span::new(0, 3, user);
        let diag = Diagnostic::error("E031", "no")
            .label(Span::new(0, 4, lib), true, "message here")
            .trace(frame(
                "boom",
                Span::new(0, 4, lib),
                vec![
                    frame("main", call, Vec::new()),
                    frame("main", Span::new(4, 7, user), Vec::new()),
                    frame("main", call, Vec::new()),
                ],
            ));
        let lib_uri = Url::parse("file:///lib.lilac").expect("uri");
        let user_uri = Url::parse("file:///main.lilac").expect("uri");
        let uri_of = |id: FileId| {
            if id == lib {
                Some(lib_uri.clone())
            } else if id == user {
                Some(user_uri.clone())
            } else {
                None
            }
        };
        let sites = trace_diagnostics(&diag, &sources, &uri_of);
        assert_eq!(sites.len(), 2, "{sites:?}");
        let (uri, lsp) = &sites[0];
        assert_eq!(uri, &user_uri);
        assert_eq!(
            lsp.range,
            Range::new(Position::new(0, 0), Position::new(0, 3))
        );
        assert_eq!(lsp.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(lsp.message, "this call leads to an error: no");
        let back = lsp.related_information.as_ref().expect("links back");
        assert_eq!(back[0].location.uri, lib_uri);

        let origin = to_lsp_diagnostic(&diag, &sources, &uri_of).expect("mapped");
        let related = origin.related_information.expect("frames");
        assert_eq!(related.len(), 3);
        assert_eq!(related[0].message, "reached from main");
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
