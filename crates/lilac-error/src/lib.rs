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

mod closest;
pub mod codes;
mod human;

pub use closest::closest;
pub use codes::Code;
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
    pub fn error(code: Code, msg: impl Into<String>) -> Self {
        Self {
            level: Level::Error,
            code: code.id(),
            msg: msg.into().into_boxed_str(),
            labels: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
            trace: None,
        }
    }

    /// Starts a warning diagnostic.
    #[must_use]
    pub fn warning(code: Code, msg: impl Into<String>) -> Self {
        Self {
            level: Level::Warning,
            code: code.id(),
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
///
/// [`Diags`] makes one: [`Diags::emit`], or [`Diags::already_reported`]
/// for poison passed along after an error. A diagnostic held to be reported
/// later has [`ErrorGuaranteed::held`], and the one stop without an error is
/// named: [`ErrorGuaranteed::unreported`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ErrorGuaranteed(());

/// Why a compile stops something without reporting an error
/// ([`ErrorGuaranteed::unreported`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unreported {
    /// `@entry` names something that does not exist outside an executable,
    /// so the hook is dropped rather than reported.
    DroppedEntry,
    /// The compile was cancelled (a newer one is due); it stops with
    /// whatever it reported so far, which is incomplete.
    Cancelled,
}

impl ErrorGuaranteed {
    /// The token for `diagnostic`, an error the caller holds and reports
    /// (or drops on purpose) later.
    #[must_use]
    pub fn held(diagnostic: &Diagnostic) -> Self {
        debug_assert_eq!(diagnostic.level, Level::Error, "a held warning is no error");
        Self(())
    }

    /// Stops for `reason`, which reports nothing on purpose.
    #[must_use]
    pub fn unreported(_reason: Unreported) -> Self {
        Self(())
    }
}

/// Collector: deduplicates and hands out [`ErrorGuaranteed`]. It keeps
/// every diagnostic; a terminal caps how many it shows ([`Diags::shown`]).
#[derive(Debug, Default)]
pub struct Diags {
    diagnostics: Vec<Diagnostic>,
    seen: HashSet<u64>,
}

impl Diags {
    /// Empty collector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a diagnostic (deduped by code + message + primary spans).
    pub fn emit(&mut self, diagnostic: Diagnostic) -> ErrorGuaranteed {
        if self.seen.insert(dedup_key(&diagnostic)) {
            self.diagnostics.push(diagnostic);
        }
        ErrorGuaranteed(())
    }

    /// The token for an error already reported, to pass poison along.
    /// Debug builds check that an error was recorded.
    #[must_use]
    pub fn already_reported(&self) -> ErrorGuaranteed {
        debug_assert!(self.has_errors(), "poison with no error reported");
        ErrorGuaranteed(())
    }

    /// Whether any error (not warning) was recorded.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.error_count() > 0
    }

    /// How many errors (not warnings) were recorded.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.level == Level::Error)
            .count()
    }

    /// All recorded diagnostics in emission order.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// The diagnostics to show when at most `max_errors` errors are shown:
    /// all of them in order, less the errors past the first `max_errors`.
    /// Warnings are never left out.
    #[must_use]
    pub fn shown(&self, max_errors: usize) -> Vec<&Diagnostic> {
        let mut errors = 0;
        self.diagnostics
            .iter()
            .filter(|diagnostic| {
                if diagnostic.level != Level::Error {
                    return true;
                }
                errors += 1;
                errors <= max_errors
            })
            .collect()
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
        let diagnostic = Diagnostic::error(codes::E042, "unsatisfied requirements").label(
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
        let diagnostic = Diagnostic::error(codes::E042, "unsatisfied requirements")
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
        let diagnostic = Diagnostic::error(codes::E031, "no").trace(frame(
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
    fn verbose_trace_full() {
        let mut sm = SourceMap::new();
        let file = sm.add_file("main.lilac", "x\n");
        let span = Span::new(0, 1, file);
        let mut chain = frame("frame7", span, Vec::new());
        for level in (0..7).rev() {
            chain = frame(&format!("frame{level}"), span, vec![chain]);
        }
        let diagnostic = Diagnostic::error(codes::E042, "requirements")
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
    fn shown_caps_errors_but_keeps_everything() {
        let mut diags = Diags::new();
        for index in 0..15 {
            diags.emit(Diagnostic::error(
                codes::E001,
                format!("distinct error {index}"),
            ));
        }
        diags.emit(Diagnostic::warning(codes::TEST_WARNING, "never capped"));
        assert_eq!(diags.diagnostics().len(), 16);
        assert_eq!(diags.error_count(), 15);
        let shown = diags.shown(10);
        assert_eq!(shown.len(), 11, "{shown:?}");
        assert_eq!(&*shown[9].msg, "distinct error 9");
        assert_eq!(shown[10].level, Level::Warning);
    }
}
