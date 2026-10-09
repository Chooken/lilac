//! `rustc`-style human output: source snippets, underlines and notes.
//!
//! Rendering has three steps. Labels are resolved against the source map and
//! grouped into one [`Block`] per file; each block is drawn onto a [`Canvas`]
//! of styled cells; the canvas is serialized with or without ANSI colour.
//! Layout never depends on colour, so plain output is what tests compare.
//!
//! Every gutter (`-->`, `|`, `=`, line numbers) is padded to the widest line
//! number shown anywhere in the diagnostic, so the bars always line up.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use lilac_span::{FileId, SourceFile, SourceMap};

use crate::{Diagnostic, Emitter, Label, Level, CHAIN_FRAME_PREFIX};

/// Chain frames shown before [`EmitterHuman`] elides the rest (non-`verbose`).
const MAX_FRAMES: usize = 5;

/// Display width of a tab; source tabs are expanded to this many spaces.
const TAB_WIDTH: usize = 4;

/// `rustc`-style human output with snippets and underlines.
#[derive(Debug, Default)]
pub struct EmitterHuman {
    /// Emit ANSI colour codes.
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

    /// The closing line after all diagnostics: `error: aborting due to …`
    /// or `warning: N warnings emitted`, empty when there was neither.
    /// `errors` includes the `suppressed` ones the error cap dropped.
    #[must_use]
    pub fn summary(&self, errors: usize, suppressed: usize, warnings: usize) -> String {
        let plural = |n: usize, noun: &str| {
            if n == 1 {
                format!("{n} {noun}")
            } else {
                format!("{n} {noun}s")
            }
        };
        let (level, mut text) = if errors > 0 {
            let mut text = format!("aborting due to {}", plural(errors, "previous error"));
            if suppressed > 0 {
                let _ = write!(text, " ({suppressed} not shown)");
            }
            if warnings > 0 {
                let _ = write!(text, "; {} emitted", plural(warnings, "warning"));
            }
            (Level::Error, text)
        } else if warnings > 0 {
            (
                Level::Warning,
                format!("{} emitted", plural(warnings, "warning")),
            )
        } else {
            return String::new();
        };
        text.insert_str(0, ": ");
        let mut canvas = Canvas::default();
        let row = canvas.new_row();
        let col = canvas.put(row, 0, level.as_str(), Style::Level(level));
        canvas.put(row, col, &text, Style::Bold);
        canvas.render(self.color)
    }
}

impl Emitter for EmitterHuman {
    fn render(&self, diagnostic: &Diagnostic, source_map: &SourceMap) -> String {
        let mut canvas = Canvas::default();
        draw_header(&mut canvas, diagnostic);

        let blocks = blocks(diagnostic, source_map);
        let width = blocks
            .iter()
            .filter_map(|block| block.lines.last())
            .max()
            .map_or(0, |line| line.to_string().len());
        for (index, block) in blocks.iter().enumerate() {
            block.draw(&mut canvas, width, index == 0);
        }

        // Labels with no source location keep their message as a note.
        let unplaced = diagnostic
            .labels
            .iter()
            .filter(|label| !label.msg.is_empty() && resolve(label, source_map).is_none())
            .map(|label| label.msg.as_str());
        let mut footer: Vec<(Style, &str, String)> = unplaced
            .map(|msg| (Style::Note, "note", msg.to_owned()))
            .collect();
        let trace = crate::trace_text(diagnostic.trace_roots(), source_map);
        if !trace.is_empty() {
            footer.push((Style::Note, "note", trace));
        }
        footer.extend(notes(diagnostic, self.verbose));
        footer.extend(
            diagnostic
                .helps
                .iter()
                .map(|help| (Style::Help, "help", help.clone())),
        );
        let indent = if blocks.is_empty() { None } else { Some(width) };
        if indent.is_some() && !footer.is_empty() {
            gutter_row(&mut canvas, width);
        }
        for (style, kind, text) in &footer {
            draw_footer_line(&mut canvas, indent, *style, kind, text);
        }
        canvas.render(self.color)
    }
}

/// `error[E042]: message`.
fn draw_header(canvas: &mut Canvas, diagnostic: &Diagnostic) {
    let row = canvas.new_row();
    let level = Style::Level(diagnostic.level);
    let mut col = canvas.put(row, 0, diagnostic.level.as_str(), level);
    if !diagnostic.code.is_empty() {
        col = canvas.put(row, col, &format!("[{}]", diagnostic.code), level);
    }
    canvas.put(row, col, &format!(": {}", diagnostic.msg), Style::Bold);
}

/// The notes to print, with instantiation frames past [`MAX_FRAMES`] elided
/// unless `verbose`.
fn notes(diagnostic: &Diagnostic, verbose: bool) -> Vec<(Style, &'static str, String)> {
    let frames = diagnostic
        .notes
        .iter()
        .filter(|note| note.starts_with(CHAIN_FRAME_PREFIX))
        .count();
    let elide = !verbose && frames > MAX_FRAMES;
    let mut seen = 0usize;
    let mut out = Vec::new();
    for note in &diagnostic.notes {
        if note.starts_with(CHAIN_FRAME_PREFIX) {
            seen += 1;
            if elide && seen > MAX_FRAMES {
                // The marker stands where the first elided frame was.
                if seen == MAX_FRAMES + 1 {
                    let elided = frames - MAX_FRAMES;
                    let noun = if elided == 1 { "frame" } else { "frames" };
                    out.push((
                        Style::Note,
                        "note",
                        format!("... {elided} more {noun} elided (use --verbose)"),
                    ));
                }
                continue;
            }
        }
        out.push((Style::Note, "note", note.clone()));
    }
    out
}

/// `   = note: text`, with continuation lines aligned under the text.
/// Without a snippet (`indent` is `None`) the line is a bare `note: text`.
fn draw_footer_line(
    canvas: &mut Canvas,
    indent: Option<usize>,
    style: Style,
    kind: &str,
    text: &str,
) {
    let row = canvas.new_row();
    let mut col = 0;
    if let Some(width) = indent {
        col = canvas.put(row, width + 1, "=", Style::Gutter) + 1;
    }
    col = canvas.put(row, col, kind, style);
    col = canvas.put(row, col, ": ", Style::Plain);
    let mut lines = text.split('\n');
    canvas.put(row, col, lines.next().unwrap_or(""), Style::Plain);
    for line in lines {
        let row = canvas.new_row();
        canvas.put(row, col, line, Style::Plain);
    }
}

/// An empty `   |` row.
fn gutter_row(canvas: &mut Canvas, width: usize) {
    let row = canvas.new_row();
    canvas.put(row, width + 1, "|", Style::Gutter);
}

/// A label resolved against its file: byte bounds clamped to the source and
/// snapped to `char` boundaries, plus the lines it covers.
struct Mark<'a> {
    label: &'a Label,
    /// First covered byte.
    lo: usize,
    /// Start of the last covered `char` (`lo` for an empty span).
    last: usize,
    /// Whether the span covers at least one byte.
    non_empty: bool,
    start_line: usize,
    end_line: usize,
    /// Margin column for a multi-line mark.
    margin: Option<usize>,
}

impl Mark<'_> {
    fn style(&self, level: Level) -> Style {
        if self.label.primary {
            Style::Level(level)
        } else {
            Style::Gutter
        }
    }

    fn underline(&self) -> char {
        if self.label.primary {
            '^'
        } else {
            '-'
        }
    }
}

/// Resolves `label`, or `None` for a dummy span or an unknown file.
fn resolve<'a>(label: &'a Label, source_map: &'a SourceMap) -> Option<(&'a SourceFile, Mark<'a>)> {
    if label.span.is_dummy() {
        return None;
    }
    let file = source_map.file(label.span.file)?;
    let src = &file.src;
    let floor = |mut offset: usize| {
        offset = offset.min(src.len());
        while !src.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    };
    let lo = floor(label.span.lo.0 as usize);
    let hi = floor(label.span.hi.0 as usize).max(lo);
    let last = src[lo..hi]
        .char_indices()
        .last()
        .map_or(lo, |(index, _)| lo + index);
    Some((
        file,
        Mark {
            label,
            lo,
            last,
            non_empty: hi > lo,
            start_line: file.line_of(lo),
            end_line: file.line_of(last),
            margin: None,
        },
    ))
}

/// Display column of `offset` on `line`: tabs count [`TAB_WIDTH`], every
/// other `char` one. An offset past the line's text (its newline) is the
/// column just after the text.
fn display_col(file: &SourceFile, line: usize, offset: usize) -> usize {
    let text = file.line_text(line);
    let upto = offset.saturating_sub(file.line_start(line)).min(text.len());
    text.get(..upto)
        .map_or(0, |prefix| prefix.chars().map(char_width).sum())
}

/// Display width of the `char` at `offset` (one past the line's text).
fn width_at(file: &SourceFile, offset: usize) -> usize {
    file.src[offset..]
        .chars()
        .next()
        .filter(|ch| *ch != '\n' && *ch != '\r')
        .map_or(1, char_width)
}

fn char_width(ch: char) -> usize {
    if ch == '\t' {
        TAB_WIDTH
    } else {
        1
    }
}

/// All annotations in one file.
struct Block<'a> {
    file: &'a SourceFile,
    level: Level,
    marks: Vec<Mark<'a>>,
    /// Shown line numbers, ascending.
    lines: Vec<usize>,
    /// Columns reserved left of the source text for multi-line marks.
    margin_width: usize,
}

/// Groups the diagnostic's placeable labels into per-file blocks: the
/// primary label's file first, the rest in order of first appearance.
fn blocks<'a>(diagnostic: &'a Diagnostic, source_map: &'a SourceMap) -> Vec<Block<'a>> {
    let primary = diagnostic
        .labels
        .iter()
        .position(|label| label.primary)
        .unwrap_or(0);
    let mut order: Vec<usize> = (0..diagnostic.labels.len()).collect();
    if primary < order.len() {
        order.remove(primary);
        order.insert(0, primary);
    }
    let mut groups: Vec<(FileId, &SourceFile, Vec<Mark<'a>>)> = Vec::new();
    for index in order {
        let label = &diagnostic.labels[index];
        let Some((file, mark)) = resolve(label, source_map) else {
            continue;
        };
        match groups.iter_mut().find(|(id, _, _)| *id == label.span.file) {
            Some((_, _, marks)) => marks.push(mark),
            None => groups.push((label.span.file, file, vec![mark])),
        }
    }
    groups
        .into_iter()
        .map(|(_, file, marks)| Block::new(file, diagnostic.level, marks))
        .collect()
}

impl<'a> Block<'a> {
    fn new(file: &'a SourceFile, level: Level, mut marks: Vec<Mark<'a>>) -> Self {
        // Multi-line marks get margin columns, outermost (earliest, longest)
        // leftmost so nested frames do not cross.
        let mut multi: Vec<usize> = (0..marks.len())
            .filter(|&index| marks[index].start_line != marks[index].end_line)
            .collect();
        multi.sort_by_key(|&index| {
            (
                marks[index].start_line,
                std::cmp::Reverse(marks[index].end_line),
            )
        });
        for (column, &index) in multi.iter().enumerate() {
            marks[index].margin = Some(column);
        }
        let margin_width = if multi.is_empty() { 0 } else { multi.len() + 1 };

        let mut shown = BTreeSet::new();
        for mark in &marks {
            shown.insert(mark.start_line);
            shown.insert(mark.end_line);
            if mark.margin.is_some() {
                shown.insert(mark.start_line + 1);
                shown.insert(mark.end_line - 1);
            }
        }
        // A single hidden line costs as much as `...`, so show it instead.
        let gaps: Vec<usize> = shown
            .iter()
            .zip(shown.iter().skip(1))
            .filter(|(a, b)| **b == **a + 2)
            .map(|(a, _)| a + 1)
            .collect();
        shown.extend(gaps);

        Self {
            file,
            level,
            marks,
            lines: shown.into_iter().collect(),
            margin_width,
        }
    }

    fn draw(&self, canvas: &mut Canvas, width: usize, first: bool) {
        let text_col = width + 3 + self.margin_width;
        if !first {
            gutter_row(canvas, width);
        }
        // The header names the block's first mark: the primary label in its
        // own file, else the earliest label.
        let anchor = &self.marks[0];
        let (line, col) = self.file.line_col(anchor.lo);
        let row = canvas.new_row();
        let arrow = if first { "-->" } else { ":::" };
        let after = canvas.put(row, width, arrow, Style::Gutter);
        canvas.put(
            row,
            after + 1,
            &format!("{}:{line}:{col}", self.file.name),
            Style::Plain,
        );
        gutter_row(canvas, width);

        let mut previous: Option<usize> = None;
        for &line in &self.lines {
            if previous.is_some_and(|prev| line > prev + 1) {
                let row = canvas.new_row();
                canvas.put(row, 0, "...", Style::Gutter);
                for mark in &self.marks {
                    if let Some(margin) = mark.margin {
                        if mark.start_line < line && mark.end_line >= line {
                            canvas.put_char(row, width + 3 + margin, '|', mark.style(self.level));
                        }
                    }
                }
            }
            previous = Some(line);
            self.draw_line(canvas, width, text_col, line);
        }
    }

    /// The underlines on `line`: whole single-line marks, plus the opening
    /// and closing corners of multi-line marks (only the closing one carries
    /// the message).
    fn annotations(&self, line: usize, width: usize) -> Vec<Ann<'_, 'a>> {
        let margin_col = |mark: &Mark<'_>| mark.margin.map(|margin| width + 3 + margin);
        let mut anns = Vec::new();
        for mark in &self.marks {
            if mark.margin.is_none() && mark.start_line == line {
                let start = display_col(self.file, line, mark.lo);
                let end = if mark.non_empty {
                    display_col(self.file, line, mark.last) + width_at(self.file, mark.last)
                } else {
                    start + 1
                };
                anns.push(Ann {
                    mark,
                    start,
                    end: end.max(start + 1),
                    msg: &mark.label.msg,
                    underscore_from: None,
                });
            } else if let Some(col) = margin_col(mark) {
                let at = if mark.start_line == line {
                    Some((mark.lo, ""))
                } else if mark.end_line == line {
                    Some((mark.last, mark.label.msg.as_str()))
                } else {
                    None
                };
                if let Some((offset, msg)) = at {
                    let start = display_col(self.file, line, offset);
                    anns.push(Ann {
                        mark,
                        start,
                        end: start + 1,
                        msg,
                        underscore_from: Some(col + 1),
                    });
                }
            }
        }
        anns
    }

    /// One source line and the annotation rows beneath it.
    fn draw_line(&self, canvas: &mut Canvas, width: usize, text_col: usize, line: usize) {
        let row = canvas.new_row();
        canvas.put(row, 0, &format!("{line:>width$} |"), Style::Gutter);
        let text = self
            .file
            .line_text(line)
            .replace('\t', &" ".repeat(TAB_WIDTH));
        canvas.put(row, text_col, &text, Style::Plain);
        let margin_col = |mark: &Mark<'_>| mark.margin.map(|margin| width + 3 + margin);
        for mark in &self.marks {
            if let Some(col) = margin_col(mark) {
                if mark.start_line < line && line <= mark.end_line {
                    canvas.put_char(row, col, '|', mark.style(self.level));
                }
            }
        }

        let anns = self.annotations(line, width);
        if anns.is_empty() {
            return;
        }

        // The rightmost message goes inline when nothing extends past its
        // underline; the others hang below on `|` connectors, right to left.
        let max_end = anns.iter().map(|ann| ann.end).max().unwrap_or(0);
        let mut labelled: Vec<&Ann<'_, '_>> =
            anns.iter().filter(|ann| !ann.msg.is_empty()).collect();
        labelled.sort_by_key(|ann| std::cmp::Reverse((ann.start, ann.end)));
        let inline = labelled.first().filter(|ann| ann.end == max_end).copied();
        let hanging: Vec<&Ann<'_, '_>> = labelled
            .into_iter()
            .filter(|ann| !inline.is_some_and(|inline| std::ptr::eq(*ann, inline)))
            .collect();
        let rows = if hanging.is_empty() {
            1
        } else {
            2 + hanging.len()
        };
        let first = canvas.new_row();
        for _ in 1..rows {
            canvas.new_row();
        }
        for offset in 0..rows {
            canvas.put(first + offset, width + 1, "|", Style::Gutter);
        }

        // Multi-line frames: the vertical bar continues through every row of
        // a line the span crosses, starts below its first line, and ends on
        // the row with its closing underline.
        for mark in &self.marks {
            let Some(col) = margin_col(mark) else {
                continue;
            };
            let span = if mark.start_line < line && line < mark.end_line {
                0..rows
            } else if mark.start_line == line {
                1..rows
            } else if mark.end_line == line {
                0..1
            } else {
                0..0
            };
            for offset in span {
                canvas.put_char(first + offset, col, '|', mark.style(self.level));
            }
        }

        // Underlines: underscores first, then secondary carets, then primary
        // carets so the primary label wins any overlap.
        for ann in &anns {
            if let Some(from) = ann.underscore_from {
                for col in from..text_col + ann.start {
                    canvas.put_char(first, col, '_', ann.mark.style(self.level));
                }
            }
        }
        for primary in [false, true] {
            for ann in anns.iter().filter(|ann| ann.mark.label.primary == primary) {
                for col in ann.start..ann.end {
                    canvas.put_char(
                        first,
                        text_col + col,
                        ann.mark.underline(),
                        ann.mark.style(self.level),
                    );
                }
            }
        }
        if let Some(ann) = inline {
            canvas.put(
                first,
                text_col + ann.end + 1,
                ann.msg,
                ann.mark.style(self.level),
            );
        }
        for (index, ann) in hanging.iter().enumerate() {
            let col = text_col + ann.start;
            for offset in 1..=index + 1 {
                canvas.put_char(first + offset, col, '|', ann.mark.style(self.level));
            }
        }
        for (index, ann) in hanging.iter().enumerate() {
            canvas.put(
                first + index + 2,
                text_col + ann.start,
                ann.msg,
                ann.mark.style(self.level),
            );
        }
    }
}

/// One underline on one source line (display columns, `end` exclusive).
struct Ann<'m, 'a> {
    mark: &'m Mark<'a>,
    start: usize,
    end: usize,
    msg: &'a str,
    /// For a multi-line frame: the column its `___` run starts at.
    underscore_from: Option<usize>,
}

/// Colour role of a cell; serialized to ANSI only when colour is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Style {
    Plain,
    Bold,
    Level(Level),
    Note,
    Help,
    /// Line numbers, bars, arrows and secondary underlines.
    Gutter,
}

impl Style {
    fn ansi(self) -> &'static str {
        match self {
            Self::Plain => "",
            Self::Bold => "\x1b[1m",
            Self::Level(Level::Error) => "\x1b[1;31m",
            Self::Level(Level::Warning) => "\x1b[1;33m",
            Self::Note => "\x1b[1;32m",
            Self::Help => "\x1b[1;36m",
            Self::Gutter => "\x1b[1;34m",
        }
    }
}

/// A grid of styled cells; rows grow on write.
#[derive(Default)]
struct Canvas {
    rows: Vec<Vec<(char, Style)>>,
}

impl Canvas {
    fn new_row(&mut self) -> usize {
        self.rows.push(Vec::new());
        self.rows.len() - 1
    }

    /// Writes `text` at `(row, col)`, returning the column after it.
    /// Control characters (a stray newline in a message) become spaces.
    fn put(&mut self, row: usize, col: usize, text: &str, style: Style) -> usize {
        let mut col = col;
        for ch in text.chars() {
            self.put_char(row, col, ch, style);
            col += 1;
        }
        col
    }

    fn put_char(&mut self, row: usize, col: usize, ch: char, style: Style) {
        let cells = &mut self.rows[row];
        if cells.len() <= col {
            cells.resize(col + 1, (' ', Style::Plain));
        }
        let ch = if ch.is_control() { ' ' } else { ch };
        cells[col] = (ch, style);
    }

    fn render(&self, color: bool) -> String {
        let mut out = String::new();
        for cells in &self.rows {
            let used = cells
                .iter()
                .rposition(|(ch, _)| *ch != ' ')
                .map_or(0, |last| last + 1);
            let mut current = Style::Plain;
            for &(ch, style) in &cells[..used] {
                // Spaces carry no colour, so they never switch style.
                if color && ch != ' ' && style != current {
                    if current != Style::Plain {
                        out.push_str("\x1b[0m");
                    }
                    out.push_str(style.ansi());
                    current = style;
                }
                out.push(ch);
            }
            if color && current != Style::Plain {
                out.push_str("\x1b[0m");
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_span::Span;

    fn render(src: &str, build: impl FnOnce(FileId) -> Diagnostic) -> String {
        let mut sm = SourceMap::new();
        let file = sm.add_file("main.lilac", src);
        EmitterHuman::new().render(&build(file), &sm)
    }

    /// Byte span of the `nth` occurrence (0-based) of `needle` in `src`.
    fn find(src: &str, needle: &str, nth: usize) -> (u32, u32) {
        let lo = src.match_indices(needle).nth(nth).expect("needle").0;
        let at = |offset: usize| u32::try_from(offset).expect("small test source");
        (at(lo), at(lo + needle.len()))
    }

    fn span(src: &str, needle: &str, file: FileId) -> Span {
        let (lo, hi) = find(src, needle, 0);
        Span::new(lo, hi, file)
    }

    fn numbered(lines: usize) -> String {
        let mut src = String::new();
        for n in 1..=lines {
            let _ = writeln!(src, "line{n}");
        }
        src
    }

    #[test]
    fn single_digit_gutter() {
        let src = "x = 1\ny = oops\n";
        let out = render(src, |file| {
            Diagnostic::error("E001", "bad").label(span(src, "oops", file), true, "here")
        });
        assert_eq!(
            out,
            concat!(
                "error[E001]: bad\n",
                " --> main.lilac:2:5\n",
                "  |\n",
                "2 | y = oops\n",
                "  |     ^^^^ here\n",
            )
        );
    }

    #[test]
    fn gutter_width_follows_widest_line_number() {
        let src = numbered(120);
        let (lo, hi) = find(&src, "line7\n", 0);
        let out = render(&src, |file| {
            Diagnostic::error("E001", "bad")
                .label(Span::new(lo, hi - 1, file), true, "first")
                .label(span(&src, "line120", file), false, "second")
        });
        assert_eq!(
            out,
            concat!(
                "error[E001]: bad\n",
                "   --> main.lilac:7:1\n",
                "    |\n",
                "  7 | line7\n",
                "    | ^^^^^ first\n",
                "...\n",
                "120 | line120\n",
                "    | ------- second\n",
            )
        );
    }

    #[test]
    fn one_hidden_line_is_shown_instead_of_elided() {
        let src = numbered(3);
        let out = render(&src, |file| {
            Diagnostic::error("E001", "bad")
                .label(span(&src, "line1", file), true, "")
                .label(span(&src, "line3", file), false, "")
        });
        assert!(out.contains("2 | line2\n"), "{out}");
        assert!(!out.contains("..."), "{out}");
    }

    #[test]
    fn second_file_gets_its_own_block() {
        let mut sm = SourceMap::new();
        let a = sm.add_file("a.lilac", "use_it()\n");
        let b = sm.add_file("b.lilac", "fn use_it()\n");
        let diagnostic = Diagnostic::error("E001", "bad")
            .label(Span::new(0, 6, a), true, "called here")
            .label(Span::new(3, 9, b), false, "defined here");
        let out = EmitterHuman::new().render(&diagnostic, &sm);
        assert_eq!(
            out,
            concat!(
                "error[E001]: bad\n",
                " --> a.lilac:1:1\n",
                "  |\n",
                "1 | use_it()\n",
                "  | ^^^^^^ called here\n",
                "  |\n",
                " ::: b.lilac:1:4\n",
                "  |\n",
                "1 | fn use_it()\n",
                "  |    ------ defined here\n",
            )
        );
    }

    #[test]
    fn labels_on_one_line_hang_right_to_left() {
        let src = "x = foo(a, b)\n";
        let out = render(src, |file| {
            Diagnostic::error("E001", "bad")
                .label(span(src, "foo", file), true, "wrong arity")
                .label(span(src, "a", file), false, "first")
                .label(span(src, "b", file), false, "second")
        });
        assert_eq!(
            out,
            concat!(
                "error[E001]: bad\n",
                " --> main.lilac:1:5\n",
                "  |\n",
                "1 | x = foo(a, b)\n",
                "  |     ^^^ -  - second\n",
                "  |     |   |\n",
                "  |     |   first\n",
                "  |     wrong arity\n",
            )
        );
    }

    #[test]
    fn primary_wins_overlapping_underline() {
        let src = "x = foo(a)\n";
        let out = render(src, |file| {
            Diagnostic::error("E001", "bad")
                .label(span(src, "foo(a)", file), false, "")
                .label(span(src, "a", file), true, "")
        });
        assert!(out.contains("  |     ----^-\n"), "{out}");
    }

    #[test]
    fn multi_line_span_draws_a_frame() {
        let src = "fn foo() {\n    body\n}\n";
        let (lo, _) = find(src, "{", 0);
        let out = render(src, |file| {
            Diagnostic::error("E001", "bad").label(
                Span::new(lo, find(src, "}", 0).1, file),
                true,
                "this block",
            )
        });
        assert_eq!(
            out,
            concat!(
                "error[E001]: bad\n",
                " --> main.lilac:1:10\n",
                "  |\n",
                "1 |   fn foo() {\n",
                "  |  __________^\n",
                "2 | |     body\n",
                "3 | | }\n",
                "  | |_^ this block\n",
            )
        );
    }

    #[test]
    fn long_multi_line_span_elides_its_middle() {
        let src = numbered(10);
        let (lo, _) = find(&src, "line1", 0);
        let (_, hi) = find(&src, "line9", 0);
        let out = render(&src, |file| {
            Diagnostic::error("E001", "bad").label(Span::new(lo, hi, file), true, "all of it")
        });
        assert_eq!(
            out,
            concat!(
                "error[E001]: bad\n",
                " --> main.lilac:1:1\n",
                "  |\n",
                "1 |   line1\n",
                "  |  _^\n",
                "2 | | line2\n",
                "... |\n",
                "8 | | line8\n",
                "9 | | line9\n",
                "  | |_____^ all of it\n",
            )
        );
    }

    #[test]
    fn tabs_and_wide_chars_keep_carets_aligned() {
        let src = "\t\"é\" oops\n";
        let out = render(src, |file| {
            Diagnostic::error("E001", "bad").label(span(src, "oops", file), true, "")
        });
        assert!(out.contains(" --> main.lilac:1:6\n"), "{out}");
        assert!(out.contains("1 |     \"é\" oops\n"), "{out}");
        assert!(out.contains("  |         ^^^^\n"), "{out}");
    }

    #[test]
    fn empty_span_at_eof_draws_one_caret() {
        let src = "x = (";
        let out = render(src, |file| {
            Diagnostic::error("E001", "unexpected end of file").label(
                Span::new(5, 5, file),
                true,
                "",
            )
        });
        assert!(out.contains("1 | x = (\n  |      ^\n"), "{out}");
    }

    #[test]
    fn span_past_eof_is_clamped() {
        let src = "x\n";
        let out = render(src, |file| {
            Diagnostic::error("E001", "bad").label(Span::new(1, 99, file), true, "")
        });
        assert!(out.starts_with("error[E001]: bad\n"), "{out}");
    }

    #[test]
    fn dummy_label_message_becomes_a_note() {
        let sm = SourceMap::new();
        let diagnostic = Diagnostic::error("E001", "bad")
            .label(Span::dummy(), true, "somewhere")
            .help("try again");
        let out = EmitterHuman::new().render(&diagnostic, &sm);
        assert_eq!(out, "error[E001]: bad\nnote: somewhere\nhelp: try again\n");
    }

    #[test]
    fn multi_line_note_aligns_continuations() {
        let src = "x\n";
        let out = render(src, |file| {
            Diagnostic::error("E001", "bad")
                .label(Span::new(0, 1, file), true, "")
                .note("first\n  second")
        });
        assert!(
            out.ends_with("  |\n  = note: first\n            second\n"),
            "{out}"
        );
    }

    #[test]
    fn colour_changes_only_escape_codes() {
        let src = "x = foo(a, b)\n";
        let mut sm = SourceMap::new();
        let file = sm.add_file("main.lilac", src);
        let diagnostic = Diagnostic::warning("W001", "odd")
            .label(span(src, "foo", file), true, "here")
            .label(span(src, "b", file), false, "there")
            .note("n");
        let plain = EmitterHuman::new().render(&diagnostic, &sm);
        let colored = EmitterHuman {
            color: true,
            ..EmitterHuman::new()
        }
        .render(&diagnostic, &sm);
        assert!(colored.contains("\x1b[1;33mwarning[W001]"), "{colored}");
        let mut stripped = String::new();
        let mut rest = colored.as_str();
        while let Some(at) = rest.find('\x1b') {
            stripped.push_str(&rest[..at]);
            rest = &rest[at + rest[at..].find('m').expect("escape end") + 1..];
        }
        stripped.push_str(rest);
        assert_eq!(stripped, plain);
    }
}
