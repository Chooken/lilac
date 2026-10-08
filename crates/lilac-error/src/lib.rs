//! Diagnostics: `Diagnostic`, emitters, and `ErrorGuaranteed` poison.
//!
//! Mirrors `rustc_errors`: passes never print directly; they return
//! `Result<_, ErrorGuaranteed>` and the driver renders at the end with an
//! [`Emitter`]. Emitted diagnostics are deduplicated so one bad generic
//! instantiation produces one error, never note spam.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

use lilac_span::{SourceMap, Span};

/// Severity of a [`Diagnostic`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    /// Hard error; compilation fails.
    Error,
    /// Lint-style advisory; compilation continues.
    Warning,
}

impl Level {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// One annotated source range inside a [`Diagnostic`].
#[derive(Clone, Debug)]
pub struct Label {
    /// Annotated range.
    pub span: Span,
    /// Primary labels render as `^^^`; secondary ones as notes.
    pub primary: bool,
    /// Message shown next to the carets.
    pub msg: String,
}

/// A single user-facing diagnostic: level + stable code + message + context.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    /// Severity.
    pub level: Level,
    /// Stable code (`E0xx` errors, `W0xx` lints).
    pub code: &'static str,
    /// Headline message.
    pub msg: String,
    /// Source annotations (exactly one primary in well-formed diagnostics).
    pub labels: Vec<Label>,
    /// `note: ...` follow-ups (e.g. instantiation stack frames).
    pub notes: Vec<String>,
    /// `help: ...` suggestions (e.g. did-you-mean).
    pub helps: Vec<String>,
}

impl Diagnostic {
    /// Starts an error diagnostic.
    #[must_use]
    pub fn error(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            level: Level::Error,
            code,
            msg: msg.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
        }
    }

    /// Starts a warning diagnostic.
    #[must_use]
    pub fn warning(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            level: Level::Warning,
            code,
            msg: msg.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
        }
    }

    /// Adds a source annotation (builder style).
    #[must_use]
    pub fn label(mut self, span: Span, primary: bool, msg: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            primary,
            msg: msg.into(),
        });
        self
    }

    /// Adds a `note: ...` line (builder style).
    #[must_use]
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Adds a `help: ...` line (builder style).
    #[must_use]
    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.helps.push(help.into());
        self
    }
}

/// Proof that an error was already emitted; forces callers to stop normally.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ErrorGuaranteed(());

impl ErrorGuaranteed {
    /// Creates the poison token (call only after emitting a diagnostic).
    #[must_use]
    pub fn new() -> Self {
        Self(())
    }
}

impl Default for ErrorGuaranteed {
    fn default() -> Self {
        Self::new()
    }
}

/// Collector: deduplicates, caps output, and hands out [`ErrorGuaranteed`].
#[derive(Debug)]
pub struct Diags {
    diagnostics: Vec<Diagnostic>,
    seen: HashSet<u64>,
    /// Maximum primary errors rendered; the rest are dropped with a note.
    pub max_errors: usize,
    /// Error-level diagnostics currently in `diagnostics`.
    error_count: usize,
    /// Error-level diagnostics dropped because the cap was reached.
    suppressed_errors: usize,
}

impl Diags {
    /// Empty collector with the default cap of 10 primary errors.
    #[must_use]
    pub fn new() -> Self {
        Self {
            diagnostics: Vec::new(),
            seen: HashSet::new(),
            max_errors: 10,
            error_count: 0,
            suppressed_errors: 0,
        }
    }

    /// Records a diagnostic (deduped by code + message + primary spans).
    ///
    /// Once [`Self::max_errors`] error-level diagnostics are live, further
    /// errors are dropped and counted in [`Self::suppressed_errors`] instead.
    pub fn emit(&mut self, diagnostic: Diagnostic) -> ErrorGuaranteed {
        if diagnostic.level == Level::Error && self.error_count >= self.max_errors {
            self.suppressed_errors += 1;
            return ErrorGuaranteed::new();
        }
        if self.seen.insert(dedup_key(&diagnostic)) {
            if diagnostic.level == Level::Error {
                self.error_count += 1;
            }
            self.diagnostics.push(diagnostic);
        }
        ErrorGuaranteed::new()
    }

    /// Whether any error (not warning) was recorded.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.level == Level::Error)
    }

    /// Error-level diagnostics dropped by the [`Self::max_errors`] cap.
    #[must_use]
    pub fn suppressed_errors(&self) -> usize {
        self.suppressed_errors
    }

    /// All recorded diagnostics in emission order.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Marks the current end of the diagnostic stream so a later pass can
    /// capture everything emitted since (generic instantiation capture).
    #[must_use]
    pub fn checkpoint(&self) -> usize {
        self.diagnostics.len()
    }

    /// Removes and returns every diagnostic emitted after `mark`.
    ///
    /// Used by instantiation to capture a specialized body's diagnostics and
    /// re-attribute them to the call site; a stale `mark` past the end yields
    /// an empty vector.
    pub fn take_since(&mut self, mark: usize) -> Vec<Diagnostic> {
        if mark >= self.diagnostics.len() {
            return Vec::new();
        }
        let taken = self.diagnostics.split_off(mark);
        // Dropping the dedup hashes lets the same diagnostic be emitted again
        // after a capture (a failing instance is otherwise swallowed).
        for diagnostic in &taken {
            self.seen.remove(&dedup_key(diagnostic));
            if diagnostic.level == Level::Error {
                self.error_count = self.error_count.saturating_sub(1);
            }
        }
        taken
    }
}

/// Dedup identity of a diagnostic: code + message + primary label spans.
fn dedup_key(diagnostic: &Diagnostic) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    diagnostic.code.hash(&mut hasher);
    diagnostic.msg.hash(&mut hasher);
    for label in &diagnostic.labels {
        if label.primary {
            label.span.hash(&mut hasher);
        }
    }
    hasher.finish()
}

impl Default for Diags {
    fn default() -> Self {
        Self::new()
    }
}

/// Stable prefix on instantiation-stack notes so [`EmitterHuman`] can elide
/// them past [`EMITTER_MAX_FRAMES`] unless `verbose`.
pub const CHAIN_FRAME_PREFIX: &str = "in instantiation of ";

/// Chain frames shown before [`EmitterHuman`] elides the rest (non-`verbose`).
const EMITTER_MAX_FRAMES: usize = 5;

/// Renders a [`Diagnostic`] against a [`SourceMap`].
pub trait Emitter {
    /// Renders one diagnostic to a string (no I/O performed).
    fn render(&self, diagnostic: &Diagnostic, source_map: &SourceMap) -> String;
}

/// `rustc`-style human output with snippets and carets.
#[derive(Debug, Default)]
pub struct EmitterHuman {
    /// Reserved for color support; currently always plain text.
    pub color: bool,
    /// Show every instantiation-chain frame (default elides past 5).
    pub verbose: bool,
}

impl EmitterHuman {
    /// Plain-text emitter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Emitter for EmitterHuman {
    fn render(&self, diagnostic: &Diagnostic, source_map: &SourceMap) -> String {
        let mut out = format!(
            "{}[{}]: {}\n",
            diagnostic.level.as_str(),
            diagnostic.code,
            diagnostic.msg
        );
        let primary = diagnostic
            .labels
            .iter()
            .find(|label| label.primary)
            .or_else(|| diagnostic.labels.first());
        if let Some(label) = primary {
            if let Some(file) = source_map.file(label.span.file) {
                let (line, col) = file.line_col(label.span.lo.0 as usize);
                let _ = writeln!(out, "  --> {}:{line}:{col}", file.name);
                let text = file.line_text(line);
                let width = (label.span.len() as usize).clamp(1, text.len().max(1));
                let pad = " ".repeat(col.saturating_sub(1));
                out.push_str("   |\n");
                let _ = writeln!(out, "{line} | {text}");
                let _ = writeln!(out, "   | {pad}{}", "^".repeat(width));
                if !label.msg.is_empty() {
                    let _ = writeln!(out, "   = {}", label.msg);
                }
            }
        }
        for label in &diagnostic.labels {
            let is_primary = primary.is_some_and(|first| std::ptr::eq(first, label));
            if is_primary || label.span.is_dummy() {
                continue;
            }
            // Every span-bearing secondary label renders as its own rustc-style
            // snippet block; its message trails the carets (E060.md's shape).
            if let Some(file) = source_map.file(label.span.file) {
                let (line, col) = file.line_col(label.span.lo.0 as usize);
                let _ = writeln!(out, "  --> {}:{line}:{col}", file.name);
                let text = file.line_text(line);
                let width = (label.span.len() as usize).clamp(1, text.len().max(1));
                let pad = " ".repeat(col.saturating_sub(1));
                out.push_str("   |\n");
                let _ = writeln!(out, "{line} | {text}");
                let carets = "^".repeat(width);
                if label.msg.is_empty() {
                    let _ = writeln!(out, "   | {pad}{carets}");
                } else {
                    let _ = writeln!(out, "   | {pad}{carets} {}", label.msg);
                }
            }
        }
        let frame_count = diagnostic
            .notes
            .iter()
            .filter(|note| note.starts_with(CHAIN_FRAME_PREFIX))
            .count();
        let elide = !self.verbose && frame_count > EMITTER_MAX_FRAMES;
        let mut frames_seen = 0usize;
        let mut frames_elided = 0usize;
        for note in &diagnostic.notes {
            if note.starts_with(CHAIN_FRAME_PREFIX) {
                frames_seen += 1;
                if elide && frames_seen > EMITTER_MAX_FRAMES {
                    frames_elided += 1;
                    continue;
                }
            }
            let _ = writeln!(out, "note: {note}");
        }
        if frames_elided > 0 {
            let noun = if frames_elided == 1 {
                "frame"
            } else {
                "frames"
            };
            let _ = writeln!(
                out,
                "note: ... {frames_elided} more {noun} elided (use --verbose)"
            );
        }
        for help in &diagnostic.helps {
            let _ = writeln!(out, "help: {help}");
        }
        out
    }
}

/// Newline-delimited JSON, one object per diagnostic (`--error-format=json`).
#[derive(Debug, Default)]
pub struct EmitterJson;

impl EmitterJson {
    /// JSON emitter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Emitter for EmitterJson {
    fn render(&self, diagnostic: &Diagnostic, source_map: &SourceMap) -> String {
        let mut spans = String::from("[");
        for (i, label) in diagnostic.labels.iter().enumerate() {
            let (file, line, col) = source_map.file(label.span.file).map_or_else(
                || ("<unknown>".to_owned(), 0, 0),
                |source| {
                    let (line, col) = source.line_col(label.span.lo.0 as usize);
                    (source.name.clone(), line, col)
                },
            );
            if i > 0 {
                spans.push(',');
            }
            let _ = write!(
                spans,
                "{{\"file\":\"{}\",\"line\":{line},\"col\":{col},\"primary\":{},\"label\":\"{}\"}}",
                escape_json(&file),
                label.primary,
                escape_json(&label.msg),
            );
        }
        spans.push(']');
        let notes = diagnostic
            .notes
            .iter()
            .map(|note| format!("\"{}\"", escape_json(note)))
            .collect::<Vec<_>>()
            .join(",");
        let helps = diagnostic
            .helps
            .iter()
            .map(|help| format!("\"{}\"", escape_json(help)))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"level\":\"{}\",\"code\":\"{}\",\"message\":\"{}\",\"spans\":{spans},\"notes\":[{notes}],\"helps\":[{helps}]}}\n",
            diagnostic.level.as_str(),
            diagnostic.code,
            escape_json(&diagnostic.msg),
        )
    }
}

/// Minimal JSON string escaper (the compiler crates stay `serde`-free).
#[must_use]
pub fn escape_json(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            other if (other as u32) < 0x20 => {
                let _ = write!(escaped, "\\u{:04x}", other as u32);
            }
            other => escaped.push(other),
        }
    }
    escaped
}

/// Internal compiler error: prints `ICE + span + message` and panics.
///
/// Only for broken invariants, never for user input.
#[macro_export]
macro_rules! bug {
    ($span:expr, $($arg:tt)*) => {{
        panic!("ICE at {:?}: {}", $span, format!($($arg)*))
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_span::SourceMap;

    fn sample() -> (SourceMap, Diagnostic) {
        let mut sm = SourceMap::new();
        let file = sm.add_file("main.lilac", "out = T.other()\n");
        let diagnostic = Diagnostic::error("E042", "unsatisfied requirements").label(
            Span::new(7, 8, file),
            true,
            "missing method `other`",
        );
        (sm, diagnostic)
    }

    #[test]
    fn human_points_at_use_site() {
        let (sm, diagnostic) = sample();
        let rendered = EmitterHuman::new().render(&diagnostic, &sm);
        assert!(rendered.contains("error[E042]: unsatisfied requirements"));
        assert!(rendered.contains("--> main.lilac:1:8"));
        assert!(rendered.contains('^'));
    }

    #[test]
    fn human_secondary_label_renders_as_snippet() {
        let mut sm = SourceMap::new();
        let file = sm.add_file(
            "main.lilac",
            "out = T.other()\n    variable: @bit32 = gen_function[@bit32]()\n",
        );
        let primary = Span::new(6, 15, file);
        let secondary = Span::new(39, 61, file);
        let diagnostic = Diagnostic::error("E042", "unsatisfied requirements")
            .label(primary, true, "missing method `other` on `@bit32`")
            .label(
                secondary,
                false,
                "required by instantiation of gen_function[@bit32] here",
            );
        let rendered = EmitterHuman::new().render(&diagnostic, &sm);
        assert!(
            rendered.contains("  --> main.lilac:2:24\n"),
            "secondary lacks a snippet header: {rendered}"
        );
        assert!(
            rendered.contains(
                "   |                        ^^^^^^^^^^^^^^^^^^^^^^ required by instantiation of gen_function[@bit32] here\n"
            ),
            "secondary message not on the caret line: {rendered}"
        );
        assert!(
            !rendered.contains("note: main.lilac:2:24"),
            "old flat note form survived: {rendered}"
        );
    }

    #[test]
    fn json_carries_code_and_spans() {
        let (sm, diagnostic) = sample();
        let rendered = EmitterJson.render(&diagnostic, &sm);
        assert!(rendered.contains("\"code\":\"E042\""));
        assert!(rendered.contains("\"line\":1"));
    }

    #[test]
    fn dedup_kills_repeats() {
        let (sm, diagnostic) = sample();
        let _ = sm;
        let mut diags = Diags::new();
        diags.emit(diagnostic.clone());
        diags.emit(diagnostic);
        assert_eq!(diags.diagnostics().len(), 1);
        assert!(diags.has_errors());
    }

    #[test]
    fn take_since_allows_reemission() {
        let mut diags = Diags::new();
        let diagnostic = Diagnostic::error("E001", "boom");
        diags.emit(diagnostic.clone());
        assert_eq!(diags.take_since(0).len(), 1);
        diags.emit(diagnostic);
        assert_eq!(diags.diagnostics().len(), 1);
    }

    #[test]
    fn checkpoint_and_take_since_capture_a_tail() {
        let mut diags = Diags::new();
        let mark = diags.checkpoint();
        diags.emit(Diagnostic::error("E001", "first"));
        diags.emit(Diagnostic::error("E002", "second"));
        assert_eq!(diags.checkpoint(), 2);
        let taken = diags.take_since(mark);
        assert_eq!(taken.len(), 2);
        assert!(diags.diagnostics().is_empty());
        assert!(diags.take_since(mark).is_empty());
        assert_eq!(diags.checkpoint(), 0);
    }

    #[test]
    fn verbose_stack_full() {
        let mut sm = SourceMap::new();
        let file = sm.add_file("main.lilac", "x\n");
        let mut diagnostic = Diagnostic::error("E042", "requirements").label(
            Span::new(0, 1, file),
            true,
            "instantiated here",
        );
        for frame in 0..7 {
            diagnostic = diagnostic.note(format!("{CHAIN_FRAME_PREFIX}frame{frame}"));
        }

        let short = EmitterHuman::new().render(&diagnostic, &sm);
        assert!(short.contains("in instantiation of frame0"), "{short}");
        assert!(!short.contains("in instantiation of frame6"), "{short}");
        assert!(short.contains("more frames elided"), "{short}");

        let full = EmitterHuman {
            verbose: true,
            ..EmitterHuman::new()
        }
        .render(&diagnostic, &sm);
        assert!(full.contains("in instantiation of frame6"), "{full}");
        assert!(!full.contains("elided"), "{full}");
    }

    #[test]
    fn caps_primary_errors_at_max() {
        let mut diags = Diags::new();
        for index in 0..15 {
            diags.emit(Diagnostic::error("E001", format!("distinct error {index}")));
        }
        let errors = diags
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.level == Level::Error)
            .count();
        assert_eq!(errors, diags.max_errors, "{:?}", diags.diagnostics());
        assert_eq!(diags.suppressed_errors(), 5);
        assert!(diags.has_errors());

        // Warnings are never capped.
        diags.emit(Diagnostic::warning("W001", "counted separately"));
        assert_eq!(
            diags
                .diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.level == Level::Warning)
                .count(),
            1
        );
    }

    #[test]
    fn take_since_frees_the_error_budget() {
        let mut diags = Diags::new();
        diags.max_errors = 1;
        diags.emit(Diagnostic::error("E001", "captured"));
        assert_eq!(diags.take_since(0).len(), 1);
        // The drained error no longer occupies the budget.
        diags.emit(Diagnostic::error("E002", "after capture"));
        assert_eq!(diags.suppressed_errors(), 0);
        assert_eq!(diags.diagnostics().len(), 1);
    }
}
