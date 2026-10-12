//! Diagnostic and position conversion.

use std::borrow::Cow;
use std::collections::HashSet;

use lilac_error::{Diagnostic, Level, TraceFrame};
use lilac_span::{FileId, SourceFile, SourceMap, Span};
use lsp_types::{
    Diagnostic as LspDiagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location,
    NumberOrString, Position, Range, TextEdit, Url,
};

/// Positions in one text: its line starts, built once per text (a
/// compiled file's are its `SourceFile`'s, an open document's are rebuilt
/// on each change), so a conversion finds its line by binary search. A
/// column is a subtraction when the line's text before it is ASCII, else
/// a count of UTF-16 units over that much of the line.
#[derive(Clone, Debug)]
pub struct LineIndex<'a> {
    text: &'a str,
    starts: Cow<'a, [usize]>,
}

impl<'a> LineIndex<'a> {
    /// Indexes `text` (see [`lilac_span::line_starts`]).
    #[must_use]
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            starts: Cow::Owned(lilac_span::line_starts(text)),
        }
    }

    /// `text` with its line starts already built (`starts` must be
    /// [`lilac_span::line_starts`] of it).
    #[must_use]
    pub fn with_starts(text: &'a str, starts: &'a [usize]) -> Self {
        debug_assert_eq!(
            starts.first().copied(),
            lilac_span::line_starts(text).first().copied()
        );
        Self {
            text,
            starts: Cow::Borrowed(starts),
        }
    }

    /// A compiled file's index.
    #[must_use]
    pub fn of(file: &'a SourceFile) -> Self {
        Self::with_starts(&file.src, file.line_starts())
    }

    /// The text indexed.
    #[must_use]
    pub fn text(&self) -> &'a str {
        self.text
    }

    /// 0-based line and UTF-16-code-unit column for a byte offset (clamped
    /// to the text, and back to a character boundary). An offset inside a
    /// byte-order mark is the start of line 0.
    #[must_use]
    pub fn position(&self, byte: usize) -> Position {
        let mut byte = byte.min(self.text.len());
        while !self.text.is_char_boundary(byte) {
            byte -= 1;
        }
        let line = self.starts.partition_point(|&start| start <= byte).max(1) - 1;
        let start = self.starts[line].min(byte);
        Position::new(
            u32::try_from(line).unwrap_or(u32::MAX),
            utf16_len(&self.text[start..byte]),
        )
    }

    /// Byte offset for a 0-based line and UTF-16-code-unit column.
    ///
    /// Returns `None` when the line does not exist; a column past the line
    /// end clamps to the line end.
    #[must_use]
    pub fn offset(&self, position: Position) -> Option<usize> {
        let (start, line) = self.line(position.line)?;
        let column = position.character as usize;
        if line.is_ascii() {
            return Some(start + column.min(line.len()));
        }
        let mut units = 0;
        for (offset, ch) in line.char_indices() {
            if units >= column {
                return Some(start + offset);
            }
            units += ch.len_utf16();
        }
        Some(start + line.len())
    }

    /// UTF-16 code-unit length of `line`, excluding the newline; `0` when
    /// the line does not exist.
    #[must_use]
    pub fn line_len_utf16(&self, line: u32) -> u32 {
        self.line(line).map_or(0, |(_, text)| utf16_len(text))
    }

    /// Where 0-based `line` starts and its text without the `\n`.
    fn line(&self, line: u32) -> Option<(usize, &'a str)> {
        let index = line as usize;
        let start = *self.starts.get(index)?;
        let end = self
            .starts
            .get(index + 1)
            .map_or(self.text.len(), |next| next - 1);
        Some((start, &self.text[start..end]))
    }
}

/// UTF-16 code units in `text` (its length when it is ASCII).
fn utf16_len(text: &str) -> u32 {
    let units = if text.is_ascii() {
        text.len()
    } else {
        text.chars().map(char::len_utf16).sum()
    };
    u32::try_from(units).unwrap_or(u32::MAX)
}

/// LSP range for a span, or `None` when it is dummy or its file is unknown.
pub(crate) fn span_range(sources: &SourceMap, span: Span) -> Option<Range> {
    if span.is_dummy() {
        return None;
    }
    let lines = LineIndex::of(sources.file(span.file)?);
    Some(Range::new(
        lines.position(span.lo.0 as usize),
        lines.position(span.hi.0 as usize),
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
        message.push_str(&help.text);
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
        data: fixes(diag, sources, uri_of),
    })
}

/// The diagnostic's fixes as `{ "fixes": [{ uri, range, newText }] }`, the
/// data `textDocument/codeAction` turns into quick fixes
/// ([`crate::actions`]); `None` without any.
fn fixes(
    diag: &Diagnostic,
    sources: &SourceMap,
    uri_of: &dyn Fn(FileId) -> Option<Url>,
) -> Option<serde_json::Value> {
    let fixes: Vec<serde_json::Value> = diag
        .suggestions()
        .filter_map(|suggestion| {
            let edit = TextEdit {
                range: span_range(sources, suggestion.span)?,
                new_text: suggestion.replacement.clone(),
            };
            Some(serde_json::json!({
                "uri": uri_of(suggestion.span.file)?,
                "edit": edit,
            }))
        })
        .collect();
    (!fixes.is_empty()).then(|| serde_json::json!({ "fixes": fixes }))
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
    use lilac_error::{codes, Diagnostic};
    use lilac_span::{SourceMap, Span};
    use lsp_types::{DiagnosticSeverity, NumberOrString, Position, Range, Url};

    fn position(src: &str, byte: usize) -> Position {
        LineIndex::new(src).position(byte)
    }

    fn byte_offset(src: &str, at: Position) -> Option<usize> {
        LineIndex::new(src).offset(at)
    }

    fn line_len_utf16(src: &str, line: u32) -> u32 {
        LineIndex::new(src).line_len_utf16(line)
    }

    #[test]
    fn non_ascii_lines_count_utf16_and_ascii_ones_subtract() {
        let src = "ab\né😀z\ncd";
        assert_eq!(position(src, 4), Position::new(1, 0));
        assert_eq!(position(src, 6), Position::new(1, 1));
        assert_eq!(position(src, 9), Position::new(1, 3));
        assert_eq!(position(src, 7), Position::new(1, 1)); // inside the emoji
        assert_eq!(byte_offset(src, Position::new(1, 3)), Some(9));
        assert_eq!(byte_offset(src, Position::new(2, 1)), Some(12));
        assert_eq!(line_len_utf16(src, 1), 4);
        assert_eq!(line_len_utf16(src, 3), 0);
    }

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

    /// A byte-order mark is no column: positions are those the editor
    /// shows, and offsets stay the file's bytes.
    #[test]
    fn byte_order_mark_is_not_a_column() {
        let src = "\u{FEFF}ab\ncd";
        assert_eq!(position(src, 0), Position::new(0, 0));
        assert_eq!(position(src, 3), Position::new(0, 0));
        assert_eq!(position(src, 4), Position::new(0, 1));
        assert_eq!(position(src, 7), Position::new(1, 1));
        assert_eq!(byte_offset(src, Position::new(0, 1)), Some(4));
        assert_eq!(byte_offset(src, Position::new(1, 0)), Some(6));
        assert_eq!(line_len_utf16(src, 0), 2);
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
        let diag =
            Diagnostic::error(codes::E001, "boom").label(Span::new(0, 3, file), true, "here");
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
        let diag = Diagnostic::error(codes::E031, "no")
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
        let diag = Diagnostic::error(codes::E000, "no location");
        let lsp = to_lsp_diagnostic(&diag, &sources, &|_| None).expect("mapped");
        assert_eq!(
            lsp.range,
            Range::new(Position::new(0, 0), Position::new(0, 0))
        );
    }
}
