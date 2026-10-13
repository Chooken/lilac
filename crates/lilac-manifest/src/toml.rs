//! A TOML reader for `lilac.toml`: TOML 1.0, less what no setting takes
//! (numbers, dates and times, multi-line strings and arrays of tables),
//! which is `E075` like any other mistake.
//!
//! Every key and value keeps its span, so what reads the table reports
//! like the compiler. The reader recovers at the next line, so one read
//! reports every mistake; it returns a table only when it found none.

use lilac_error::{codes, Diagnostic, Diags};
use lilac_span::{FileId, Span, BYTE_ORDER_MARK};

/// How deep arrays and inline tables nest, and how many parts a key has.
/// It bounds the reader's recursion, as `lilac_parse::MAX_NESTING` bounds
/// the parser's.
pub const MAX_NESTING: usize = 32;

/// A table: its keys in source order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    /// One per key, in the order the file first names them.
    pub entries: Vec<Entry>,
}

impl Table {
    /// The entry for `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.key == key)
    }
}

/// One key of a [`Table`] and its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The key, unquoted and unescaped.
    pub key: String,
    /// The key as written: a quoted key with its quotes, and for a dotted
    /// key or a header, the part that names this entry.
    pub key_span: Span,
    /// The value.
    pub value: Value,
}

/// A value and where it is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value {
    /// What it is.
    pub kind: ValueKind,
    /// The value as written. A table a `[header]` defines spans its header;
    /// one a dotted key makes spans the `key = value` that first made it.
    pub span: Span,
}

/// The values a manifest holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueKind {
    /// A basic or literal string, its escapes applied.
    String(String),
    /// `true` or `false`.
    Bool(bool),
    /// An array.
    Array(Vec<Value>),
    /// A table: a header's, a dotted key's, or an inline one.
    Table(Table),
}

/// Reads `text`, the contents of `file`. Every mistake is reported to
/// `diags` as `E075`, and then there is no table.
pub fn read(file: FileId, text: &str, diags: &mut Diags) -> Option<Table> {
    let mut reader = Reader::new(file, text);
    reader.read_all();
    if reader.errors.is_empty() {
        return Some(reader.root.finish());
    }
    for diagnostic in reader.errors {
        diags.emit(diagnostic);
    }
    None
}

/// A statement was abandoned; its error is reported and the reader
/// recovers at the next line.
struct Stop;

type Parse<T> = Result<T, Stop>;

/// One part of a key, unquoted.
struct Key {
    name: String,
    span: Span,
}

/// How a table under construction came to be, which decides what may add
/// to it (TOML's rules).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// The document, or an inline table.
    Root,
    /// Named on the way to a header's table (`a` in `[a.b]`): a later
    /// header may still define it.
    Implicit,
    /// Defined by a `[header]`.
    Header,
    /// Made by a dotted key (`a` in `a.b = …`): only dotted keys add keys
    /// to it, and only headers add tables inside it.
    Dotted,
}

/// A table under construction.
struct Draft {
    kind: Kind,
    span: Span,
    slots: Vec<Slot>,
}

struct Slot {
    key: String,
    key_span: Span,
    node: Node,
}

enum Node {
    /// A finished value: nothing adds to it (a string, a boolean, an array,
    /// an inline table).
    Value(Value),
    /// A table later lines may add to.
    Table(Draft),
}

impl Draft {
    fn new(kind: Kind, span: Span) -> Self {
        Self {
            kind,
            span,
            slots: Vec::new(),
        }
    }

    fn slot(&self, key: &str) -> Option<usize> {
        self.slots.iter().position(|slot| slot.key == key)
    }

    /// Adds a slot, returning its index.
    fn push(&mut self, key: &Key, node: Node) -> usize {
        self.slots.push(Slot {
            key: key.name.clone(),
            key_span: key.span,
            node,
        });
        self.slots.len() - 1
    }

    fn finish(self) -> Table {
        Table {
            entries: self
                .slots
                .into_iter()
                .map(|slot| Entry {
                    key: slot.key,
                    key_span: slot.key_span,
                    value: match slot.node {
                        Node::Value(value) => value,
                        Node::Table(draft) => Value {
                            span: draft.span,
                            kind: ValueKind::Table(draft.finish()),
                        },
                    },
                })
                .collect(),
        }
    }
}

/// The table at `path` (slot indices from `root`).
fn table_at<'t>(root: &'t mut Draft, path: &[usize]) -> Option<&'t mut Draft> {
    let mut table = root;
    for &index in path {
        table = match &mut table.slots.get_mut(index)?.node {
            Node::Table(draft) => draft,
            Node::Value(_) => return None,
        };
    }
    Some(table)
}

/// Why a key cannot go where it is written.
#[derive(Clone, Copy)]
enum Conflict {
    /// The key is already defined.
    Twice,
    /// A key goes inside a string or a boolean.
    Value,
    /// A key goes inside an inline table or an array.
    Closed,
    /// A dotted key goes inside a table a header made.
    Header,
    /// A header defines a table dotted keys made.
    Dotted,
}

/// The error for writing `path` at `at`, which `first` already has.
fn conflict(conflict: Conflict, path: &[&str], at: Span, first: Span) -> Diagnostic {
    let path = display_key(path);
    let (message, here, there, note) = match conflict {
        Conflict::Twice => (
            format!("`{path}` is defined twice"),
            "defined again here",
            "first defined here".to_owned(),
            None,
        ),
        Conflict::Value => (
            format!("`{path}` is a value, not a table"),
            "used as a table here",
            "defined as a value here".to_owned(),
            None,
        ),
        Conflict::Closed => (
            format!("`{path}` cannot be added to"),
            "adds to it here",
            "written whole here".to_owned(),
            Some("an inline table or an array is complete once it is written"),
        ),
        Conflict::Header => (
            format!("a dotted key cannot add to `{path}`"),
            "adds to it here",
            format!("`[{path}]` defines it"),
            Some("a dotted key adds only to tables other dotted keys made"),
        ),
        Conflict::Dotted => (
            format!("`{path}` is defined twice"),
            "defined again here",
            "first defined by a dotted key here".to_owned(),
            Some("a `[table]` header can add a table inside a dotted key's table, but not define it again"),
        ),
    };
    let diagnostic = Diagnostic::error(codes::E075, message)
        .label(at, true, here)
        .label(first, false, there);
    match note {
        Some(note) => diagnostic.note(note),
        None => diagnostic,
    }
}

/// The conflict of a key going inside the finished value `value`.
fn inside_value(value: &Value) -> Conflict {
    match value.kind {
        ValueKind::String(_) | ValueKind::Bool(_) => Conflict::Value,
        ValueKind::Array(_) | ValueKind::Table(_) => Conflict::Closed,
    }
}

/// Adds `keys = value` to `table`, whose path is `base`. `line` spans the
/// whole `key = value`, the span of a table the dotted key makes.
fn insert(
    table: &mut Draft,
    base: &[String],
    keys: &[Key],
    value: Value,
    line: Span,
) -> Result<(), Diagnostic> {
    let mut path: Vec<&str> = base.iter().map(String::as_str).collect();
    let Some((last, parents)) = keys.split_last() else {
        return Ok(());
    };
    let mut table = table;
    for key in parents {
        path.push(&key.name);
        let index = match table.slot(&key.name) {
            Some(index) => index,
            None => table.push(key, Node::Table(Draft::new(Kind::Dotted, line))),
        };
        let slot = &mut table.slots[index];
        table = match &mut slot.node {
            Node::Table(draft) if draft.kind == Kind::Dotted => draft,
            Node::Table(_) => {
                return Err(conflict(Conflict::Header, &path, key.span, slot.key_span))
            }
            Node::Value(value) => {
                return Err(conflict(
                    inside_value(value),
                    &path,
                    key.span,
                    slot.key_span,
                ))
            }
        };
    }
    path.push(&last.name);
    if let Some(index) = table.slot(&last.name) {
        let first = table.slots[index].key_span;
        return Err(conflict(Conflict::Twice, &path, last.span, first));
    }
    table.push(last, Node::Value(value));
    Ok(())
}

/// Defines the header `[keys]` (spanning `span`), returning the path of
/// slot indices to its table.
fn define(root: &mut Draft, keys: &[Key], span: Span) -> Result<Vec<usize>, Diagnostic> {
    let mut indices = Vec::with_capacity(keys.len());
    let mut path = Vec::with_capacity(keys.len());
    let Some((last, parents)) = keys.split_last() else {
        return Ok(indices);
    };
    let mut table = root;
    for key in parents {
        path.push(key.name.as_str());
        let index = match table.slot(&key.name) {
            Some(index) => index,
            None => table.push(key, Node::Table(Draft::new(Kind::Implicit, span))),
        };
        indices.push(index);
        let slot = &mut table.slots[index];
        table = match &mut slot.node {
            Node::Table(draft) => draft,
            Node::Value(value) => {
                return Err(conflict(
                    inside_value(value),
                    &path,
                    key.span,
                    slot.key_span,
                ))
            }
        };
    }
    path.push(&last.name);
    let Some(index) = table.slot(&last.name) else {
        indices.push(table.push(last, Node::Table(Draft::new(Kind::Header, span))));
        return Ok(indices);
    };
    let slot = &mut table.slots[index];
    match &mut slot.node {
        Node::Table(draft) if draft.kind == Kind::Implicit => {
            draft.kind = Kind::Header;
            draft.span = span;
            slot.key_span = last.span;
        }
        Node::Table(draft) if draft.kind == Kind::Dotted => {
            return Err(conflict(Conflict::Dotted, &path, last.span, slot.key_span));
        }
        _ => return Err(conflict(Conflict::Twice, &path, last.span, slot.key_span)),
    }
    indices.push(index);
    Ok(indices)
}

/// A bare key's characters.
fn is_bare(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

/// A character of an unquoted value: a word, a number, a date or a time.
fn is_word(byte: u8) -> bool {
    is_bare(byte) || matches!(byte, b'+' | b'.' | b':')
}

/// A key as TOML writes it: its parts joined with `.`, each quoted unless
/// it is bare.
fn display_key(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|part| {
            if !part.is_empty() && part.bytes().all(is_bare) {
                (*part).to_owned()
            } else {
                format!("\"{}\"", part.replace('\\', "\\\\").replace('"', "\\\""))
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// A character as an error names it: in backticks, or by code point when
/// it would not show.
fn describe_char(ch: char) -> String {
    if ch.is_control() || ch.is_whitespace() || ch == '`' || ch == BYTE_ORDER_MARK {
        format!("U+{:04X}", u32::from(ch))
    } else {
        format!("`{ch}`")
    }
}

/// A control character TOML allows in no string or comment (all but tab).
fn is_forbidden_control(ch: char) -> bool {
    ch != '\t' && (ch < ' ' || ch == '\u{7F}')
}

/// What an unquoted value is.
enum Word {
    Bool(bool),
    Number,
    Date,
    Other,
}

fn classify(word: &str) -> Word {
    match word {
        "true" => return Word::Bool(true),
        "false" => return Word::Bool(false),
        _ => {}
    }
    if is_date(word) {
        return Word::Date;
    }
    let unsigned = word
        .strip_prefix('+')
        .or_else(|| word.strip_prefix('-'))
        .unwrap_or(word);
    if unsigned.starts_with(|ch: char| ch.is_ascii_digit())
        || unsigned.starts_with("inf")
        || unsigned.starts_with("nan")
    {
        Word::Number
    } else {
        Word::Other
    }
}

/// Whether `word` starts like a TOML date (`1979-05-27`) or time
/// (`07:32:00`).
fn is_date(word: &str) -> bool {
    let bytes = word.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        bytes
            .get(range)
            .is_some_and(|run| run.iter().all(u8::is_ascii_digit))
    };
    (digits(0..4) && bytes.get(4) == Some(&b'-') && digits(5..7))
        || (digits(0..2) && bytes.get(2) == Some(&b':'))
}

struct Reader<'a> {
    text: &'a str,
    file: FileId,
    pos: usize,
    errors: Vec<Diagnostic>,
    /// Arrays and inline tables open around `pos`.
    nesting: usize,
    /// Arrays open around `pos`; inline tables end with their line.
    open_arrays: usize,
    /// `open_arrays` when the last statement stopped: how many `]`
    /// recovery skips before the line can end.
    stop_depth: usize,
    root: Draft,
    /// The current header's table, or `None` after a header that failed:
    /// its keys then go to `scratch`, so they are checked but kept nowhere,
    /// and no error repeats one the header already has.
    section: Option<Vec<usize>>,
    section_path: Vec<String>,
    scratch: Draft,
}

impl<'a> Reader<'a> {
    fn new(file: FileId, text: &'a str) -> Self {
        let start = Span::new(0, 0, file);
        Self {
            text,
            file,
            pos: 0,
            errors: Vec::new(),
            nesting: 0,
            open_arrays: 0,
            stop_depth: 0,
            root: Draft::new(Kind::Root, start),
            section: Some(Vec::new()),
            section_path: Vec::new(),
            scratch: Draft::new(Kind::Root, start),
        }
    }

    fn read_all(&mut self) {
        if self.text.starts_with(BYTE_ORDER_MARK) {
            self.pos = BYTE_ORDER_MARK.len_utf8();
        }
        loop {
            self.skip_trivia();
            if self.pos >= self.text.len() {
                break;
            }
            let start = self.pos;
            if self.statement().is_err() {
                self.recover();
            }
            debug_assert!(self.pos > start, "the reader stalled at {start}");
        }
    }

    /// A header or a `key = value`, through the end of its line.
    fn statement(&mut self) -> Parse<()> {
        if self.peek() == Some(b'[') {
            self.header()
        } else {
            self.key_value()
        }
    }

    fn header(&mut self) -> Parse<()> {
        let open = self.pos;
        self.section = None;
        self.section_path.clear();
        self.scratch = Draft::new(Kind::Header, self.span(open, open + 1));
        if self.rest().starts_with("[[") {
            let diagnostic = Diagnostic::error(codes::E075, "no setting takes an array of tables")
                .label(self.span(open, open + 2), true, "")
                .help("write a `[table]` header with one `[`");
            return Err(self.fail(diagnostic));
        }
        self.pos += 1;
        self.skip_blank();
        let keys = self.key("a table name")?;
        self.skip_blank();
        if self.peek() != Some(b']') {
            return Err(self.expected("`]`"));
        }
        self.pos += 1;
        let span = self.span(open, self.pos);
        self.section_path = keys.iter().map(|key| key.name.clone()).collect();
        match define(&mut self.root, &keys, span) {
            Ok(path) => self.section = Some(path),
            Err(diagnostic) => self.report(diagnostic),
        }
        self.end_of_line()
    }

    fn key_value(&mut self) -> Parse<()> {
        let start = self.pos;
        let keys = self.key("a key or a `[table]` header")?;
        self.skip_blank();
        self.equals(&keys)?;
        self.skip_blank();
        let value = self.value()?;
        let line = Span {
            hi: value.span.hi,
            ..self.span(start, start)
        };
        let table = match &self.section {
            Some(path) => table_at(&mut self.root, path),
            None => None,
        }
        .unwrap_or(&mut self.scratch);
        if let Err(diagnostic) = insert(table, &self.section_path, &keys, value, line) {
            self.report(diagnostic);
        }
        self.end_of_line()
    }

    /// A key, its parts joined by `.`. `what` names it when the first part
    /// is missing.
    fn key(&mut self, what: &str) -> Parse<Vec<Key>> {
        let mut keys = vec![self.key_part(what)?];
        loop {
            let before = self.pos;
            self.skip_blank();
            if self.peek() != Some(b'.') {
                self.pos = before;
                return Ok(keys);
            }
            self.pos += 1;
            self.skip_blank();
            let key = self.key_part("a key after `.`")?;
            if keys.len() == MAX_NESTING {
                let diagnostic = Diagnostic::error(
                    codes::E075,
                    format!("a key has at most {MAX_NESTING} parts"),
                )
                .label(key.span, true, "");
                return Err(self.fail(diagnostic));
            }
            keys.push(key);
        }
    }

    fn key_part(&mut self, what: &str) -> Parse<Key> {
        let start = self.pos;
        let name = match self.peek() {
            Some(quote @ (b'"' | b'\'')) => {
                if self.rest().as_bytes().starts_with(&[quote; 3]) {
                    return Err(self.multi_line_string(quote));
                }
                if quote == b'"' {
                    self.basic_string()?
                } else {
                    self.literal_string()?
                }
            }
            Some(byte) if is_bare(byte) => {
                while self.peek().is_some_and(is_bare) {
                    self.pos += 1;
                }
                self.text[start..self.pos].to_owned()
            }
            _ => return Err(self.expected(what)),
        };
        Ok(Key {
            name,
            span: self.span(start, self.pos),
        })
    }

    fn equals(&mut self, keys: &[Key]) -> Parse<()> {
        if self.peek() == Some(b'=') {
            self.pos += 1;
            return Ok(());
        }
        let names: Vec<&str> = keys.iter().map(|key| key.name.as_str()).collect();
        Err(self.expected(&format!("`=` after `{}`", display_key(&names))))
    }

    fn value(&mut self) -> Parse<Value> {
        let start = self.pos;
        let kind = match self.peek() {
            Some(quote @ (b'"' | b'\'')) => {
                if self.rest().as_bytes().starts_with(&[quote; 3]) {
                    return Err(self.multi_line_string(quote));
                }
                ValueKind::String(if quote == b'"' {
                    self.basic_string()?
                } else {
                    self.literal_string()?
                })
            }
            Some(b'[') => return self.array(),
            Some(b'{') => return self.inline_table(),
            Some(byte) if is_word(byte) => return self.word(),
            _ => return Err(self.expected("a value")),
        };
        Ok(Value {
            kind,
            span: self.span(start, self.pos),
        })
    }

    /// An unquoted value: `true`, `false`, or a mistake (a number, a date,
    /// a word missing its quotes).
    fn word(&mut self) -> Parse<Value> {
        let start = self.pos;
        self.skip_word();
        let kind = classify(&self.text[start..self.pos]);
        // `1979-05-27 07:32:00`: a date, a space, then its time.
        if matches!(kind, Word::Date)
            && self.peek() == Some(b' ')
            && self
                .text
                .as_bytes()
                .get(self.pos + 1)
                .is_some_and(u8::is_ascii_digit)
        {
            self.pos += 1;
            self.skip_word();
        }
        let span = self.span(start, self.pos);
        let diagnostic = match kind {
            Word::Bool(value) => {
                return Ok(Value {
                    kind: ValueKind::Bool(value),
                    span,
                });
            }
            Word::Number => {
                Diagnostic::error(codes::E075, "no setting takes a number").label(span, true, "")
            }
            Word::Date => Diagnostic::error(codes::E075, "no setting takes a date or a time")
                .label(span, true, ""),
            Word::Other => {
                let word = &self.text[start..self.pos];
                Diagnostic::error(codes::E075, format!("expected a value, found `{word}`"))
                    .label(span, true, "")
                    .suggest(span, format!("\"{word}\""))
            }
        };
        Err(self.fail(diagnostic))
    }

    fn skip_word(&mut self) {
        while self.peek().is_some_and(is_word) {
            self.pos += 1;
        }
    }

    fn array(&mut self) -> Parse<Value> {
        let open = self.pos;
        self.enter(open)?;
        self.open_arrays += 1;
        let items = self.array_items(open);
        self.open_arrays -= 1;
        self.nesting -= 1;
        Ok(Value {
            kind: ValueKind::Array(items?),
            span: self.span(open, self.pos),
        })
    }

    fn array_items(&mut self, open: usize) -> Parse<Vec<Value>> {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            self.skip_trivia();
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(items);
            }
            if self.peek().is_none() {
                return Err(self.unclosed_array(open));
            }
            items.push(self.value()?);
            self.skip_trivia();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(items);
                }
                None => return Err(self.unclosed_array(open)),
                Some(_) => return Err(self.expected("`,` or `]`")),
            }
        }
    }

    fn inline_table(&mut self) -> Parse<Value> {
        let open = self.pos;
        self.enter(open)?;
        let table = self.inline_entries(open);
        self.nesting -= 1;
        Ok(Value {
            kind: ValueKind::Table(table?.finish()),
            span: self.span(open, self.pos),
        })
    }

    fn inline_entries(&mut self, open: usize) -> Parse<Draft> {
        self.pos += 1;
        let mut table = Draft::new(Kind::Root, self.span(open, open + 1));
        self.skip_blank();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(table);
        }
        loop {
            if self.at_line_end() {
                return Err(self.unclosed_inline_table(open));
            }
            let start = self.pos;
            let keys = self.key("a key")?;
            self.skip_blank();
            self.equals(&keys)?;
            self.skip_blank();
            let value = self.value()?;
            let line = Span {
                hi: value.span.hi,
                ..self.span(start, start)
            };
            if let Err(diagnostic) = insert(&mut table, &[], &keys, value, line) {
                self.report(diagnostic);
            }
            self.skip_blank();
            match self.peek() {
                Some(b',') => {
                    let comma = self.pos;
                    self.pos += 1;
                    self.skip_blank();
                    if self.peek() == Some(b'}') {
                        let diagnostic = Diagnostic::error(
                            codes::E075,
                            "an inline table takes no comma after its last key",
                        )
                        .label(self.span(comma, comma + 1), true, "");
                        self.report(diagnostic);
                        self.pos += 1;
                        return Ok(table);
                    }
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(table);
                }
                _ if self.at_line_end() => return Err(self.unclosed_inline_table(open)),
                _ => return Err(self.expected("`,` or `}`")),
            }
        }
    }

    /// Opens an array or inline table at `open`, within the nesting limit.
    fn enter(&mut self, open: usize) -> Parse<()> {
        if self.nesting == MAX_NESTING {
            let diagnostic = Diagnostic::error(
                codes::E075,
                format!("arrays and inline tables nest at most {MAX_NESTING} deep"),
            )
            .label(self.span(open, open + 1), true, "");
            return Err(self.fail(diagnostic));
        }
        self.nesting += 1;
        Ok(())
    }

    /// A `"basic string"`, its escapes applied. Its own mistakes are
    /// reported without stopping the statement, but one its line ends
    /// inside stops it.
    fn basic_string(&mut self) -> Parse<String> {
        let open = self.pos;
        self.pos += 1;
        let mut text = String::new();
        while let Some(ch) = self.peek_char() {
            match ch {
                '"' => {
                    self.pos += 1;
                    return Ok(text);
                }
                '\\' => self.escape(&mut text),
                _ if self.at_newline() => break,
                _ if is_forbidden_control(ch) => self.control_character(ch, "a string"),
                _ => {
                    text.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        Err(self.unclosed_string(open, '"'))
    }

    /// A `'literal string'`: no escapes.
    fn literal_string(&mut self) -> Parse<String> {
        let open = self.pos;
        self.pos += 1;
        let mut text = String::new();
        while let Some(ch) = self.peek_char() {
            match ch {
                '\'' => {
                    self.pos += 1;
                    return Ok(text);
                }
                _ if self.at_newline() => break,
                _ if is_forbidden_control(ch) => self.control_character(ch, "a string"),
                _ => {
                    text.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        Err(self.unclosed_string(open, '\''))
    }

    /// The escape at `pos` (a `\`), appended to `text`.
    fn escape(&mut self, text: &mut String) {
        let start = self.pos;
        self.pos += 1;
        let simple = match self.peek_char() {
            Some('b') => Some('\u{8}'),
            Some('t') => Some('\t'),
            Some('n') => Some('\n'),
            Some('f') => Some('\u{C}'),
            Some('r') => Some('\r'),
            Some('"') => Some('"'),
            Some('\\') => Some('\\'),
            _ => None,
        };
        if let Some(ch) = simple {
            text.push(ch);
            self.pos += 1;
            return;
        }
        let digits = match self.peek() {
            Some(b'u') => 4,
            Some(b'U') => 8,
            Some(_) if !self.at_newline() => {
                let ch = self.peek_char().unwrap_or('\\');
                self.pos += ch.len_utf8();
                return self.bad_escape(start, None);
            }
            // The string ends with its line; that error follows.
            _ => return self.bad_escape(start, None),
        };
        self.pos += 1;
        let hex = self.pos;
        while self.pos - hex < digits && self.peek().is_some_and(|byte| byte.is_ascii_hexdigit()) {
            self.pos += 1;
        }
        let code = &self.text[hex..self.pos];
        match u32::from_str_radix(code, 16).ok().and_then(char::from_u32) {
            Some(ch) if code.len() == digits => text.push(ch),
            _ => self.bad_escape(
                start,
                Some("`\\u` takes four hex digits and `\\U` eight, naming a Unicode scalar value"),
            ),
        }
    }

    fn bad_escape(&mut self, start: usize, note: Option<&str>) {
        let written = &self.text[start..self.pos];
        let diagnostic = Diagnostic::error(
            codes::E075,
            format!("invalid escape `{written}` in a string"),
        )
        .label(self.span(start, self.pos), true, "");
        let diagnostic = match note {
            Some(note) => diagnostic.note(note),
            None => diagnostic.help(
                "write `\\\\` for a backslash, or use a 'literal string', which has no escapes",
            ),
        };
        self.report(diagnostic);
    }

    fn unclosed_string(&mut self, open: usize, quote: char) -> Stop {
        let diagnostic = Diagnostic::error(codes::E075, "the line ends inside this string")
            .label(self.span(open, self.pos), true, "")
            .help(format!("add the closing `{quote}`"));
        self.fail(diagnostic)
    }

    /// `"""` or `'''`: reported, then skipped to its closing quotes so its
    /// lines are not read as statements.
    fn multi_line_string(&mut self, quote: u8) -> Stop {
        let open = self.pos;
        let delimiter = if quote == b'"' { "\"\"\"" } else { "'''" };
        let diagnostic = Diagnostic::error(codes::E075, "no setting takes a multi-line string")
            .label(self.span(open, open + 3), true, "")
            .help("write the string on one line");
        let body = open + 3;
        self.pos = self.text[body..]
            .find(delimiter)
            .map_or(self.text.len(), |end| body + end + 3);
        self.fail(diagnostic)
    }

    fn control_character(&mut self, ch: char, place: &str) {
        let start = self.pos;
        self.pos += ch.len_utf8();
        let diagnostic = Diagnostic::error(
            codes::E075,
            format!("{} is not allowed in {place}", describe_char(ch)),
        )
        .label(self.span(start, self.pos), true, "");
        self.report(diagnostic);
    }

    /// Blanks, an optional comment, then the end of the line or file.
    fn end_of_line(&mut self) -> Parse<()> {
        self.skip_blank();
        if self.peek() == Some(b'#') {
            self.comment();
        }
        if self.peek().is_none() {
            return Ok(());
        }
        if self.at_newline() {
            self.skip_newline();
            return Ok(());
        }
        Err(self.expected("the end of the line"))
    }

    /// The comment at `pos`, up to its line's end.
    fn comment(&mut self) {
        while let Some(ch) = self.peek_char() {
            if self.at_newline() {
                return;
            }
            if is_forbidden_control(ch) {
                self.control_character(ch, "a comment");
            } else {
                self.pos += ch.len_utf8();
            }
        }
    }

    /// Spaces and tabs.
    fn skip_blank(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    /// Blanks, comments and line ends.
    fn skip_trivia(&mut self) {
        loop {
            self.skip_blank();
            if self.peek() == Some(b'#') {
                self.comment();
            } else if self.at_newline() {
                self.skip_newline();
            } else {
                return;
            }
        }
    }

    /// Skips the rest of a statement that stopped: through the end of its
    /// line, once every array open where it stopped has closed.
    fn recover(&mut self) {
        let mut depth = self.stop_depth;
        while let Some(ch) = self.peek_char() {
            self.pos += ch.len_utf8();
            match ch {
                '\n' if depth == 0 => return,
                '[' => depth += 1,
                ']' => depth = depth.saturating_sub(1),
                '"' | '\'' => {
                    while let Some(inner) = self.peek_char() {
                        if inner == '\n' {
                            break;
                        }
                        self.pos += inner.len_utf8();
                        if inner == '\\' && ch == '"' {
                            if let Some(escaped) = self.peek_char().filter(|&next| next != '\n') {
                                self.pos += escaped.len_utf8();
                            }
                        } else if inner == ch {
                            break;
                        }
                    }
                }
                '#' => {
                    while self.peek().is_some_and(|byte| byte != b'\n') {
                        self.pos += 1;
                    }
                }
                _ => {}
            }
        }
    }

    fn report(&mut self, diagnostic: Diagnostic) {
        self.errors.push(diagnostic);
    }

    /// Reports an error that stops the statement.
    fn fail(&mut self, diagnostic: Diagnostic) -> Stop {
        self.stop_depth = self.open_arrays;
        self.report(diagnostic);
        Stop
    }

    /// "expected {what}, found …" at `pos`.
    fn expected(&mut self, what: &str) -> Stop {
        let (found, span) = match self.peek_char() {
            None => ("end of file".to_owned(), self.span(self.pos, self.pos)),
            Some(_) if self.at_newline() => {
                ("end of line".to_owned(), self.span(self.pos, self.pos))
            }
            Some(ch) => (
                describe_char(ch),
                self.span(self.pos, self.pos + ch.len_utf8()),
            ),
        };
        let diagnostic = Diagnostic::error(codes::E075, format!("expected {what}, found {found}"))
            .label(span, true, "");
        self.fail(diagnostic)
    }

    fn unclosed_array(&mut self, open: usize) -> Stop {
        let diagnostic = Diagnostic::error(codes::E075, "unterminated array; add a closing `]`")
            .label(self.span(open, open + 1), true, "this `[` is never closed")
            .label(self.span(self.pos, self.pos), false, "the file ends here");
        self.fail(diagnostic)
    }

    fn unclosed_inline_table(&mut self, open: usize) -> Stop {
        let diagnostic = Diagnostic::error(codes::E075, "the line ends inside this inline table")
            .label(self.span(open, open + 1), true, "this `{` is never closed")
            .note("an inline table is written on one line; a `[table]` header starts a longer one");
        self.fail(diagnostic)
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn peek_char(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }

    /// At `\n` or `\r\n` (a lone `\r` is a control character).
    fn at_newline(&self) -> bool {
        let rest = self.rest();
        rest.starts_with('\n') || rest.starts_with("\r\n")
    }

    fn skip_newline(&mut self) {
        self.pos += if self.rest().starts_with('\r') { 2 } else { 1 };
    }

    /// Where a value or key must end: a line end, a comment, or the end of
    /// the file.
    fn at_line_end(&self) -> bool {
        matches!(self.peek(), None | Some(b'#')) || self.at_newline()
    }

    fn span(&self, lo: usize, hi: usize) -> Span {
        let offset = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
        Span::new(offset(lo), offset(hi), self.file)
    }
}
