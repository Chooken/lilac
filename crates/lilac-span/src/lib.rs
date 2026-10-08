//! Spans, source map, and symbol interner.
//!
//! Mirrors `rustc_span`: [`Span`] is a `(lo, hi, file)` triple, [`SourceMap`]
//! owns file contents, and [`Interner`] deduplicates identifier strings so hot
//! paths pass [`Symbol`] instead of `String`.

use std::collections::HashMap;

/// Index of a file in the [`SourceMap`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FileId(pub u32);

/// Byte offset within a file.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BytePos(pub u32);

/// A half-open byte range `[lo, hi)` in one file.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Span {
    /// Start offset (inclusive).
    pub lo: BytePos,
    /// End offset (exclusive).
    pub hi: BytePos,
    /// Owning file.
    pub file: FileId,
}

impl Span {
    /// Creates a span; panics in debug builds when `lo > hi`.
    #[must_use]
    pub fn new(lo: u32, hi: u32, file: FileId) -> Self {
        debug_assert!(lo <= hi, "span lo > hi");
        Self {
            lo: BytePos(lo),
            hi: BytePos(hi),
            file,
        }
    }

    /// Placeholder span for synthesized nodes; never points at real source.
    #[must_use]
    pub fn dummy() -> Self {
        Self {
            lo: BytePos(0),
            hi: BytePos(0),
            file: FileId(u32::MAX),
        }
    }

    /// Whether this is a [`Span::dummy`] placeholder.
    #[must_use]
    pub fn is_dummy(&self) -> bool {
        self.file == FileId(u32::MAX)
    }

    /// Byte length covered by this span.
    #[must_use]
    pub fn len(&self) -> u32 {
        self.hi.0.saturating_sub(self.lo.0)
    }

    /// Whether this span covers zero bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One source file: name, contents, and precomputed line starts.
#[derive(Debug)]
pub struct SourceFile {
    /// Path or label as passed to [`SourceMap::add_file`].
    pub name: String,
    /// Full file contents.
    pub src: String,
    line_starts: Vec<usize>,
}

impl SourceFile {
    fn new(name: String, src: String) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self {
            name,
            src,
            line_starts,
        }
    }

    /// 1-based `(line, column)` for a byte offset. Columns count `char`s,
    /// so a multi-byte character before the offset is one column.
    #[must_use]
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let mut off = offset.min(self.src.len());
        while !self.src.is_char_boundary(off) {
            off -= 1;
        }
        let line = self.line_of(off);
        let start = self.line_starts[line - 1];
        (line, self.src[start..off].chars().count() + 1)
    }

    /// 1-based line containing a byte offset (clamped to the file).
    #[must_use]
    pub fn line_of(&self, offset: usize) -> usize {
        let off = offset.min(self.src.len());
        self.line_starts.partition_point(|&start| start <= off)
    }

    /// Byte offset where a 1-based line starts (clamped to the file).
    #[must_use]
    pub fn line_start(&self, line: usize) -> usize {
        self.line_starts
            .get(line.saturating_sub(1))
            .copied()
            .unwrap_or(self.src.len())
    }

    /// Number of lines (a trailing newline opens one more, empty line).
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Text of a 1-based line without trailing newline, or `""` if out of range.
    #[must_use]
    pub fn line_text(&self, line: usize) -> &str {
        if line == 0 || line > self.line_starts.len() {
            return "";
        }
        let start = self.line_starts[line - 1];
        let end = self
            .line_starts
            .get(line)
            .copied()
            .unwrap_or(self.src.len());
        self.src[start..end].trim_end_matches(['\n', '\r'])
    }
}

/// Owns all source files for one compilation.
#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    /// Empty source map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a file, returning its [`FileId`].
    pub fn add_file(&mut self, name: &str, src: &str) -> FileId {
        let id = FileId(u32::try_from(self.files.len()).unwrap_or(u32::MAX));
        self.files
            .push(SourceFile::new(name.to_owned(), src.to_owned()));
        id
    }

    /// Looks up a file by id.
    #[must_use]
    pub fn file(&self, id: FileId) -> Option<&SourceFile> {
        self.files.get(id.0 as usize)
    }

    /// 1-based `(line, column)` for the start of a span.
    #[must_use]
    pub fn line_col(&self, span: Span) -> Option<(usize, usize)> {
        self.file(span.file)
            .map(|file| file.line_col(span.lo.0 as usize))
    }

    /// Source text covered by a span, or `None` for bad ranges.
    #[must_use]
    pub fn span_text(&self, span: Span) -> Option<&str> {
        self.file(span.file).and_then(|file| {
            let (lo, hi) = (span.lo.0 as usize, span.hi.0 as usize);
            if lo <= hi
                && hi <= file.src.len()
                && file.src.is_char_boundary(lo)
                && file.src.is_char_boundary(hi)
            {
                Some(&file.src[lo..hi])
            } else {
                None
            }
        })
    }
}

/// Interned string handle; resolve with [`Interner::resolve`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Symbol(pub u32);

/// String interner: hot paths pass [`Symbol`] instead of `String`.
#[derive(Debug, Default)]
pub struct Interner {
    map: HashMap<String, Symbol>,
    strings: Vec<String>,
}

impl Interner {
    /// Empty interner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns a string, returning its stable handle.
    pub fn intern(&mut self, text: &str) -> Symbol {
        if let Some(&sym) = self.map.get(text) {
            return sym;
        }
        let sym = Symbol(u32::try_from(self.strings.len()).unwrap_or(u32::MAX));
        self.strings.push(text.to_owned());
        self.map.insert(text.to_owned(), sym);
        sym
    }

    /// Resolves a handle back to its string.
    ///
    /// # Panics
    ///
    /// Panics for handles from a different interner (internal invariant).
    #[must_use]
    pub fn resolve(&self, sym: Symbol) -> &str {
        &self.strings[sym.0 as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_tracks_newlines() {
        let mut sm = SourceMap::new();
        let file = sm.add_file("main.lilac", "ab\ncde\n");
        assert_eq!(sm.line_col(Span::new(0, 1, file)), Some((1, 1)));
        assert_eq!(sm.line_col(Span::new(3, 4, file)), Some((2, 1)));
        assert_eq!(sm.line_col(Span::new(5, 6, file)), Some((2, 3)));
    }

    #[test]
    fn line_col_counts_chars() {
        let mut sm = SourceMap::new();
        let file = sm.add_file(
            "main.lilac",
            "\"é\" x
",
        );
        // `é` is two bytes but one column; offset 5 is `x`.
        assert_eq!(sm.line_col(Span::new(5, 6, file)), Some((1, 5)));
        // An offset inside `é` snaps back to its start.
        assert_eq!(sm.line_col(Span::new(2, 3, file)), Some((1, 2)));
    }

    #[test]
    fn interner_deduplicates() {
        let mut interner = Interner::new();
        let first = interner.intern("gen_function");
        let second = interner.intern("gen_function");
        assert_eq!(first, second);
        assert_eq!(interner.resolve(first), "gen_function");
    }

    #[test]
    fn span_text_rejects_bad_ranges() {
        let mut sm = SourceMap::new();
        let file = sm.add_file("a.lilac", "hi");
        assert_eq!(sm.span_text(Span::new(0, 2, file)), Some("hi"));
        assert_eq!(sm.span_text(Span::new(0, 99, file)), None);
    }
}
