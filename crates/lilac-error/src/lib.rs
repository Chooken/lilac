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

mod human;

pub use human::EmitterHuman;

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

/// A machine-applicable fix: replace `span` with `replacement` (an editor
/// quick fix).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The source range to replace.
    pub span: Span,
    /// The text to put there.
    pub replacement: String,
}

/// One `help: ...` line, with the fix it describes when it has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Help {
    /// The prose.
    pub text: String,
    /// The fix, for an editor to apply.
    pub fix: Option<Suggestion>,
}

/// One frame of a diagnostic's [`Diagnostic::trace`]: a function, the site
/// in it that leads toward the error, and the frames that reach it.
///
/// The innermost frame's site is the error itself; each caller's site is its
/// call (or address-taking) of the frame it lists under. A function's callers
/// are listed once per diagnostic: a later frame of the same function has
/// `repeat` set and no callers (it also ends recursion).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceFrame {
    /// Display name of the function.
    pub func: String,
    /// The site in `func`.
    pub span: Span,
    /// The site takes the inner function's address instead of calling it.
    pub address: bool,
    /// This function's callers are listed earlier in the trace.
    pub repeat: bool,
    /// Frames that reach `func`, each with its own call site.
    pub callers: Vec<TraceFrame>,
}

impl TraceFrame {
    /// Every frame of the tree rooted here, depth-first, this one first.
    pub fn walk(&self) -> impl Iterator<Item = &Self> {
        let mut stack = vec![self];
        std::iter::from_fn(move || {
            let frame = stack.pop()?;
            stack.extend(frame.callers.iter().rev());
            Some(frame)
        })
    }
}

/// A single user-facing diagnostic: level + stable code + message + context.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    /// Severity.
    pub level: Level,
    /// Stable code (`E0xx` errors, `W0xx` lints).
    pub code: &'static str,
    /// Headline message (boxed: see [`Self::trace`]).
    pub msg: Box<str>,
    /// Source annotations (exactly one primary in well-formed diagnostics).
    pub labels: Vec<Label>,
    /// `note: ...` follow-ups (e.g. instantiation stack frames).
    pub notes: Vec<String>,
    /// `help: ...` suggestions (e.g. did-you-mean), some with a fix.
    pub helps: Vec<Help>,
    /// Where the failing code is reached from: the innermost frame, with its
    /// callers (`E031`, `E035`, `E037`). Rendered as `reached from:`. Boxed
    /// to keep `Diagnostic` small (it is many `Result`s' error type).
    pub trace: Option<Box<TraceFrame>>,
}

impl Diagnostic {
    /// Starts an error diagnostic.
    #[must_use]
    pub fn error(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            level: Level::Error,
            code,
            msg: msg.into().into_boxed_str(),
            labels: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
            trace: None,
        }
    }

    /// Starts a warning diagnostic.
    #[must_use]
    pub fn warning(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            level: Level::Warning,
            code,
            msg: msg.into().into_boxed_str(),
            labels: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
            trace: None,
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
        self.helps.push(Help {
            text: help.into(),
            fix: None,
        });
        self
    }

    /// Adds a fix replacing `span` with `replacement`, shown as
    /// `help: did you mean `replacement`?` (builder style).
    #[must_use]
    pub fn suggest(mut self, span: Span, replacement: impl Into<String>) -> Self {
        let replacement = replacement.into();
        self.helps.push(Help {
            text: format!("did you mean `{replacement}`?"),
            fix: Some(Suggestion { span, replacement }),
        });
        self
    }

    /// The fixes among [`Self::helps`].
    pub fn suggestions(&self) -> impl Iterator<Item = &Suggestion> {
        self.helps.iter().filter_map(|help| help.fix.as_ref())
    }

    /// Sets the innermost [`TraceFrame`] (builder style).
    #[must_use]
    pub fn trace(mut self, frame: TraceFrame) -> Self {
        self.trace = Some(Box::new(frame));
        self
    }

    /// The trace's innermost frame as a slice (empty without a trace).
    #[must_use]
    pub fn trace_roots(&self) -> &[TraceFrame] {
        self.trace.as_deref().map_or(&[], std::slice::from_ref)
    }

    /// Every frame of [`Self::trace`] in display order.
    pub fn trace_frames(&self) -> impl Iterator<Item = &TraceFrame> {
        self.trace_roots().iter().flat_map(TraceFrame::walk)
    }
}

/// The `reached from:` note text for `trace`: one `at` line per frame, two
/// spaces deeper per caller level, without a trailing newline. Empty when
/// `trace` is. Levels past `max_depth` are elided with a count. A frame
/// with no function name (a use outside any function) shows only its
/// location.
#[must_use]
pub fn trace_text(
    trace: &[TraceFrame],
    source_map: &SourceMap,
    max_depth: Option<usize>,
) -> String {
    fn push(
        frame: &TraceFrame,
        depth: usize,
        max_depth: Option<usize>,
        source_map: &SourceMap,
        lines: &mut Vec<String>,
    ) {
        let indent = 2 + depth * 2;
        let location = source_map.location(frame.span);
        let mut line = match (&location, frame.func.is_empty()) {
            (Some(location), true) => format!("{:indent$}at {location}", ""),
            (Some(location), false) => format!("{:indent$}at {} ({location})", "", frame.func),
            (None, _) => format!("{:indent$}at {}", "", frame.func),
        };
        if frame.address {
            line.push_str(" [address taken]");
        }
        if frame.repeat {
            line.push_str(" [see above]");
        }
        lines.push(line);
        if max_depth.is_some_and(|max| depth + 1 >= max) && !frame.callers.is_empty() {
            let elided: usize = frame
                .callers
                .iter()
                .map(|caller| caller.walk().count())
                .sum();
            let noun = if elided == 1 { "frame" } else { "frames" };
            lines.push(format!(
                "{:width$}... {elided} more {noun} elided (use --verbose)",
                "",
                width = indent + 2
            ));
            return;
        }
        for caller in &frame.callers {
            push(caller, depth + 1, max_depth, source_map, lines);
        }
    }
    let mut lines = Vec::new();
    for frame in trace {
        push(frame, 0, max_depth, source_map, &mut lines);
    }
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "reached from:
{}",
        lines.join(
            "
"
        )
    )
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

/// Renders a [`Diagnostic`] against a [`SourceMap`].
pub trait Emitter {
    /// Renders one diagnostic to a string (no I/O performed).
    fn render(&self, diagnostic: &Diagnostic, source_map: &SourceMap) -> String;
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
            .map(|help| format!("\"{}\"", escape_json(&help.text)))
            .collect::<Vec<_>>()
            .join(",");
        let suggestions = diagnostic
            .suggestions()
            .map(|suggestion| {
                let (file, (line, col), (end_line, end_col)) =
                    source_map.file(suggestion.span.file).map_or_else(
                        || ("<unknown>".to_owned(), (0, 0), (0, 0)),
                        |source| {
                            (
                                source.name.clone(),
                                source.line_col(suggestion.span.lo.0 as usize),
                                source.line_col(suggestion.span.hi.0 as usize),
                            )
                        },
                    );
                format!(
                    "{{\"file\":\"{}\",\"line\":{line},\"col\":{col},\"end_line\":{end_line},\"end_col\":{end_col},\"replacement\":\"{}\"}}",
                    escape_json(&file),
                    escape_json(&suggestion.replacement),
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let trace = json_frames(diagnostic.trace_roots(), source_map);
        format!(
            "{{\"level\":\"{}\",\"code\":\"{}\",\"message\":\"{}\",\"spans\":{spans},\"notes\":[{notes}],\"helps\":[{helps}],\"suggestions\":[{suggestions}],\"trace\":{trace}}}\n",
            diagnostic.level.as_str(),
            diagnostic.code,
            escape_json(&diagnostic.msg),
        )
    }
}

/// `[{"func","file","line","col","address","repeat","callers":[...]}, ...]`.
fn json_frames(frames: &[TraceFrame], source_map: &SourceMap) -> String {
    let mut out = String::from("[");
    for (i, frame) in frames.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let (file, line, col) = source_map
            .file(frame.span.file)
            .filter(|_| !frame.span.is_dummy())
            .map_or_else(
                || ("<unknown>".to_owned(), 0, 0),
                |source| {
                    let (line, col) = source.line_col(frame.span.lo.0 as usize);
                    (source.name.clone(), line, col)
                },
            );
        let _ = write!(
            out,
            "{{\"func\":\"{}\",\"file\":\"{}\",\"line\":{line},\"col\":{col},\"address\":{},\"repeat\":{},\"callers\":{}}}",
            escape_json(&frame.func),
            escape_json(&file),
            frame.address,
            frame.repeat,
            json_frames(&frame.callers, source_map),
        );
    }
    out.push(']');
    out
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
    fn human_secondary_label_shares_the_snippet() {
        let mut sm = SourceMap::new();
        let file = sm.add_file(
            "main.lilac",
            "out = T.other()
    variable: @bit32 = gen_function[@bit32]()
",
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
        assert_eq!(
            rendered,
            concat!(
                "error[E042]: unsatisfied requirements
",
                " --> main.lilac:1:7
",
                "  |
",
                "1 | out = T.other()
",
                "  |       ^^^^^^^^^ missing method `other` on `@bit32`
",
                "2 |     variable: @bit32 = gen_function[@bit32]()
",
                "  |                        ---------------------- required by instantiation of gen_function[@bit32] here
",
            )
        );
    }

    #[test]
    fn json_carries_code_and_spans() {
        let (sm, diagnostic) = sample();
        let rendered = EmitterJson.render(&diagnostic, &sm);
        assert!(rendered.contains("\"code\":\"E042\""));
        assert!(rendered.contains("\"line\":1"));
    }

    fn frame(func: &str, span: Span, callers: Vec<TraceFrame>) -> TraceFrame {
        TraceFrame {
            func: func.to_owned(),
            span,
            address: false,
            repeat: false,
            callers,
        }
    }

    #[test]
    fn trace_renders_a_caller_tree() {
        let mut sm = SourceMap::new();
        let file = sm.add_file("main.lilac", "boom\na()\nboom\nb()\n");
        let mut again = frame("a", Span::new(14, 15, file), Vec::new());
        again.repeat = true;
        let diagnostic = Diagnostic::error("E031", "no").trace(frame(
            "boom",
            Span::new(0, 4, file),
            vec![
                frame("a", Span::new(5, 6, file), Vec::new()),
                frame("b", Span::new(9, 10, file), vec![again]),
            ],
        ));
        assert_eq!(
            trace_text(diagnostic.trace_roots(), &sm, None),
            "reached from:\n  at boom (main.lilac:1:1)\n    at a (main.lilac:2:1)\n    at b (main.lilac:3:1)\n      at a (main.lilac:4:1) [see above]"
        );
        let frames: Vec<&str> = diagnostic
            .trace_frames()
            .map(|frame| frame.func.as_str())
            .collect();
        assert_eq!(frames, ["boom", "a", "b", "a"]);
        let json = EmitterJson.render(&diagnostic, &sm);
        assert!(
            json.contains("\"trace\":[{\"func\":\"boom\",\"file\":\"main.lilac\",\"line\":1"),
            "{json}"
        );
        assert!(json.contains("\"repeat\":true,\"callers\":[]"), "{json}");
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
    fn verbose_trace_full() {
        let mut sm = SourceMap::new();
        let file = sm.add_file("main.lilac", "x\n");
        let span = Span::new(0, 1, file);
        let mut chain = frame("frame7", span, Vec::new());
        for level in (0..7).rev() {
            chain = frame(&format!("frame{level}"), span, vec![chain]);
        }
        let diagnostic = Diagnostic::error("E042", "requirements")
            .label(span, true, "here")
            .trace(chain);

        let short = EmitterHuman::new().render(&diagnostic, &sm);
        assert!(short.contains("at frame4 ("), "{short}");
        assert!(!short.contains("at frame5 ("), "{short}");
        assert!(
            short.contains("... 3 more frames elided (use --verbose)"),
            "{short}"
        );

        let full = EmitterHuman {
            verbose: true,
            ..EmitterHuman::new()
        }
        .render(&diagnostic, &sm);
        assert!(full.contains("at frame7 ("), "{full}");
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
