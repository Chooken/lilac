//! Lexer: `Iterator<Item = Result<Token, Diagnostic>>` over Lilac source.
//!
//! Implements `SPEC.md` §2: `0x`/`0b`/decimal/float numbers with `_`
//! separators, strings with `\"` `\\` `\n` `\xHH` escapes only, single-scalar
//! chars, and `$"..."` f-strings whose `{hole}` expressions are lexed with the
//! ordinary expression lexer (brace-depth tracked, so errors point inside the
//! braces). `[` and `]` are generics-only; there is no indexing syntax.

use lilac_error::{codes, Diagnostic};
use lilac_span::{FileId, Span};

/// Every keyword, for editor completion.
pub const KEYWORDS: [&str; 23] = [
    "object", "function", "enum", "extern", "export", "using", "private", "ref", "self", "unknown",
    "nothing", "loop", "if", "then", "else", "match", "break", "continue", "and", "or", "return",
    "as", "fail",
];

/// The keyword token `word` spells, if it is a keyword.
#[must_use]
pub fn keyword(word: &str) -> Option<TokenKind> {
    Some(match word {
        "object" => TokenKind::KwObject,
        "function" => TokenKind::KwFunction,
        "enum" => TokenKind::KwEnum,
        "extern" => TokenKind::KwExtern,
        "export" => TokenKind::KwExport,
        "using" => TokenKind::KwUsing,
        "private" => TokenKind::KwPrivate,
        "ref" => TokenKind::KwRef,
        "self" => TokenKind::KwSelf,
        "unknown" => TokenKind::KwUnknown,
        "nothing" => TokenKind::KwNothing,
        "loop" => TokenKind::KwLoop,
        "if" => TokenKind::KwIf,
        "then" => TokenKind::KwThen,
        "else" => TokenKind::KwElse,
        "match" => TokenKind::KwMatch,
        "break" => TokenKind::KwBreak,
        "continue" => TokenKind::KwContinue,
        "and" => TokenKind::KwAnd,
        "or" => TokenKind::KwOr,
        "return" => TokenKind::KwReturn,
        "fail" => TokenKind::KwFail,
        "as" => TokenKind::KwAs,
        _ => return None,
    })
}

/// Token category; surface text lives in [`Token::text`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    /// `foo`, `main`, `gen_function`.
    Ident,
    /// `5`, `0xFF`, `0b101`, `10.5` (source bytes; value comes from folding).
    Number,
    /// `"hi"` (decoded bytes in [`Token::bytes`]).
    StringLit,
    /// `'A'` (the scalar value is in [`Token::text`]).
    CharLit,
    /// `$"` opening an f-string.
    FStrStart,
    /// Literal chunk inside an f-string (decoded bytes in [`Token::bytes`]).
    FStrMid,
    /// Closing `"` of an f-string.
    FStrEnd,
    /// `object`.
    KwObject,
    /// `function`.
    KwFunction,
    /// `enum`.
    KwEnum,
    /// `extern`.
    KwExtern,
    /// `export`.
    KwExport,
    /// `using`.
    KwUsing,
    /// `private`.
    KwPrivate,
    /// `ref`.
    KwRef,
    /// `self`.
    KwSelf,
    /// `unknown`.
    KwUnknown,
    /// `nothing`.
    KwNothing,
    /// `loop`.
    KwLoop,
    /// `if`.
    KwIf,
    /// `then` (value `if c then x else y`).
    KwThen,
    /// `else`.
    KwElse,
    /// `match`.
    KwMatch,
    /// `break`.
    KwBreak,
    /// `continue`.
    KwContinue,
    /// `and` (short-circuit conjunction, no hook).
    KwAnd,
    /// `or` (short-circuit disjunction, no hook).
    KwOr,
    /// `return`.
    KwReturn,
    /// `fail` (leave a function that can fail through its failure outs).
    KwFail,
    /// `as` (postfix cast).
    KwAs,
    /// `:`.
    Colon,
    /// `,`.
    Comma,
    /// `=`.
    Eq,
    /// `==` (bit-equality, never overridden).
    EqEq,
    /// `=>` (inline bodies, enum constructors).
    FatArrow,
    /// `->` (multi-return capture types).
    Arrow,
    /// `.` (member access, enum tags).
    Dot,
    /// `...` (an `extern`'s variadic parameter list).
    Ellipsis,
    /// `{`.
    LBrace,
    /// `}`.
    RBrace,
    /// `(`.
    LParen,
    /// `)`.
    RParen,
    /// `[` (generics only).
    LBracket,
    /// `]` (generics only).
    RBracket,
    /// `+`.
    Plus,
    /// `-`.
    Minus,
    /// `*`.
    Star,
    /// `/`.
    Slash,
    /// `%`.
    Percent,
    /// `<`.
    Lt,
    /// `>`.
    Gt,
    /// `<=`.
    LtEq,
    /// `>=`.
    GtEq,
    /// `!` (e.g. `!const` data names).
    Bang,
    /// `@` (`@bit32`, `@add`, `@size_of`, ...).
    At,
    /// End of input (emitted once).
    Eof,
}

/// One lexical token with its span and surface text.
#[derive(Clone, Debug)]
pub struct Token {
    /// Token category.
    pub kind: TokenKind,
    /// Source range covered.
    pub span: Span,
    /// Surface text (the decoded scalar for chars; a lossy UTF-8 view of the
    /// bytes for strings and f-string chunks).
    pub text: String,
    /// Decoded bytes of a string literal or f-string chunk (`\xHH` is one raw
    /// byte); empty for every other token.
    pub bytes: Vec<u8>,
    /// Whether a line break separates this token from the previous one.
    /// Lilac has no statement terminator, so the parser uses this to tell
    /// `return x` (an error) from a `return` that ends its line.
    pub newline_before: bool,
}

impl Token {
    /// Builds a token covering `[start, end)` in `file`.
    #[must_use]
    pub fn new(kind: TokenKind, text: &str, start: usize, end: usize, file: FileId) -> Self {
        Self {
            kind,
            span: Span::new(offset_u32(start), offset_u32(end), file),
            text: text.to_owned(),
            bytes: Vec::new(),
            newline_before: false,
        }
    }

    /// Builds a string-literal or f-string-chunk token carrying decoded bytes.
    #[must_use]
    pub fn with_bytes(
        kind: TokenKind,
        bytes: Vec<u8>,
        start: usize,
        end: usize,
        file: FileId,
    ) -> Self {
        let mut token = Self::new(kind, &String::from_utf8_lossy(&bytes), start, end, file);
        token.bytes = bytes;
        token
    }

    /// Whether this token can open a top-level item (used by parse recovery).
    #[must_use]
    pub fn starts_item(&self) -> bool {
        matches!(
            self.kind,
            TokenKind::Ident
                | TokenKind::KwUsing
                | TokenKind::KwExtern
                | TokenKind::KwExport
                | TokenKind::At
                | TokenKind::Eof
        )
    }

    /// What a parse error says it found here: "`function`", "a string",
    /// "end of file".
    #[must_use]
    pub fn describe(&self) -> String {
        match self.kind {
            TokenKind::StringLit => "a string".to_owned(),
            TokenKind::CharLit => "a character".to_owned(),
            TokenKind::FStrStart | TokenKind::FStrMid | TokenKind::FStrEnd => {
                "an f-string".to_owned()
            }
            TokenKind::Eof => "end of file".to_owned(),
            _ => format!("`{}`", self.text),
        }
    }
}

/// Saturating `usize` -> `u32` for span offsets (files past 4 GiB clamp).
fn offset_u32(offset: usize) -> u32 {
    u32::try_from(offset).unwrap_or(u32::MAX)
}

/// Lexer mode: ordinary code vs. f-string literal vs. one deferred token.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LexMode {
    /// Ordinary expression/code tokens.
    Normal,
    /// Inside `$"..."` literal text (holes lex as [`LexMode::Normal`]).
    FStrLiteral,
    /// The `{` opening a hole is waiting to be emitted.
    PendingLBrace,
    /// The `"` closing an f-string is waiting to be emitted.
    PendingFStrEnd,
}

/// Lilac lexer; see the module docs for the accepted grammar.
pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    file: FileId,
    mode: LexMode,
    hole_depths: Vec<usize>,
    eof_done: bool,
}

impl<'a> Lexer<'a> {
    /// Creates a lexer over `src` (already registered as `file`).
    #[must_use]
    pub fn new(src: &'a str, file: FileId) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            file,
            mode: LexMode::Normal,
            hole_depths: Vec::new(),
            eof_done: false,
        }
    }

    /// Lexes everything, collecting recoverable errors instead of stopping.
    #[must_use]
    pub fn lex_all(src: &'a str, file: FileId) -> (Vec<Token>, Vec<Diagnostic>) {
        let mut tokens = Vec::new();
        let mut errors = Vec::new();
        for result in Self::new(src, file) {
            match result {
                Ok(token) => tokens.push(token),
                Err(diagnostic) => errors.push(diagnostic),
            }
        }
        (tokens, errors)
    }

    fn at_eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn peek(&self) -> u8 {
        self.peek_at(0)
    }

    fn peek_at(&self, ahead: usize) -> u8 {
        self.bytes.get(self.pos + ahead).copied().unwrap_or(0)
    }

    fn err(&self, start: usize, end: usize, msg: &str) -> Diagnostic {
        Diagnostic::error(codes::E000, msg).label(
            Span::new(offset_u32(start), offset_u32(end.max(start)), self.file),
            true,
            String::new(),
        )
    }

    /// Skips whitespace and comments; returns whether a line break was skipped.
    fn skip_trivia(&mut self) -> bool {
        let mut newline = false;
        while !self.at_eof() {
            match self.peek() {
                b'\n' => {
                    newline = true;
                    self.pos += 1;
                }
                b' ' | b'\t' | b'\r' => self.pos += 1,
                b'/' if self.peek_at(1) == b'/' => {
                    while !self.at_eof() && self.peek() != b'\n' {
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
        newline
    }

    fn lex_normal(&mut self) -> Result<Token, Diagnostic> {
        let byte = self.peek();
        if byte.is_ascii_alphabetic() || byte == b'_' {
            return Ok(self.lex_word());
        }
        if byte.is_ascii_digit() {
            return self.lex_number();
        }
        match byte {
            b'"' => self.lex_string(),
            b'\'' => self.lex_char(),
            b'$' => self.lex_fstr_start(),
            _ => self.lex_symbol(),
        }
    }

    fn lex_word(&mut self) -> Token {
        let start = self.pos;
        while !self.at_eof() && (self.peek().is_ascii_alphanumeric() || self.peek() == b'_') {
            self.pos += 1;
        }
        let word = &self.src[start..self.pos];
        let kind = keyword(word).unwrap_or(TokenKind::Ident);
        Token::new(kind, word, start, self.pos, self.file)
    }

    fn check_separators(&self, text: &str, start: usize) -> Result<(), Diagnostic> {
        let digits: Vec<char> = text.chars().collect();
        for (i, &ch) in digits.iter().enumerate() {
            if ch != '_' {
                continue;
            }
            let left_ok = i > 0 && digits[i - 1] != '_';
            let right_ok = i + 1 < digits.len() && digits[i + 1] != '_';
            if !(left_ok && right_ok) {
                return Err(self.err(
                    start,
                    start + text.len(),
                    "misplaced `_` separator; it must sit between digits",
                ));
            }
        }
        Ok(())
    }

    fn lex_number(&mut self) -> Result<Token, Diagnostic> {
        let start = self.pos;
        if self.peek() == b'0' && (self.peek_at(1) == b'x' || self.peek_at(1) == b'X') {
            self.pos += 2;
            let digits_start = self.pos;
            while !self.at_eof() && (self.peek().is_ascii_hexdigit() || self.peek() == b'_') {
                self.pos += 1;
            }
            let text = &self.src[start..self.pos];
            if self.pos == digits_start {
                return Err(self.err(start, self.pos, "hex literal needs digits after `0x`"));
            }
            self.check_separators(&text[2..], start)?;
            return self.finish_number(start);
        }
        if self.peek() == b'0' && (self.peek_at(1) == b'b' || self.peek_at(1) == b'B') {
            self.pos += 2;
            let digits_start = self.pos;
            while !self.at_eof()
                && (self.peek() == b'0' || self.peek() == b'1' || self.peek() == b'_')
            {
                self.pos += 1;
            }
            if self.pos == digits_start {
                return Err(self.err(start, self.pos, "binary literal needs digits after `0b`"));
            }
            let text = &self.src[start..self.pos];
            self.check_separators(&text[2..], start)?;
            if !self.at_eof() && self.peek().is_ascii_alphanumeric() {
                return Err(self.err(start, self.pos + 1, "invalid digit in binary literal"));
            }
            return Ok(Token::new(
                TokenKind::Number,
                text,
                start,
                self.pos,
                self.file,
            ));
        }
        while !self.at_eof() && (self.peek().is_ascii_digit() || self.peek() == b'_') {
            self.pos += 1;
        }
        if self.peek() == b'.' {
            if !self.peek_at(1).is_ascii_digit() {
                return Err(self.err(
                    start,
                    self.pos + 1,
                    "unexpected character after number literal",
                ));
            }
            self.pos += 1;
            while !self.at_eof() && (self.peek().is_ascii_digit() || self.peek() == b'_') {
                self.pos += 1;
            }
        }
        if self.peek() == b'e' || self.peek() == b'E' {
            self.pos += 1;
            if self.peek() == b'+' || self.peek() == b'-' {
                self.pos += 1;
            }
            if !self.peek().is_ascii_digit() {
                return Err(self.err(
                    start,
                    self.pos + 1,
                    "unexpected character after number literal",
                ));
            }
            while !self.at_eof() && (self.peek().is_ascii_digit() || self.peek() == b'_') {
                self.pos += 1;
            }
        }
        let text = &self.src[start..self.pos];
        self.check_separators(text, start)?;
        self.finish_number(start)
    }

    fn finish_number(&mut self, start: usize) -> Result<Token, Diagnostic> {
        if !self.at_eof() && self.peek().is_ascii_alphabetic() {
            return Err(self.err(
                start,
                self.pos + 1,
                "unexpected character after number literal",
            ));
        }
        let text = &self.src[start..self.pos];
        Ok(Token::new(
            TokenKind::Number,
            text,
            start,
            self.pos,
            self.file,
        ))
    }

    fn lex_string(&mut self) -> Result<Token, Diagnostic> {
        let start = self.pos;
        self.pos += 1; // opening `"`
        let mut value = Vec::new();
        loop {
            if self.at_eof() || self.peek() == b'\n' {
                return Err(self.err(
                    start,
                    self.pos,
                    "unterminated string literal; add a closing `\"`",
                ));
            }
            if self.peek() == b'"' {
                self.pos += 1;
                return Ok(Token::with_bytes(
                    TokenKind::StringLit,
                    value,
                    start,
                    self.pos,
                    self.file,
                ));
            }
            if self.peek() == b'\\' {
                let (byte, next) = self.decode_escape(self.pos)?;
                value.push(byte);
                self.pos = next;
            } else {
                value.push(self.peek());
                self.pos += 1;
            }
        }
    }

    fn lex_char(&mut self) -> Result<Token, Diagnostic> {
        let start = self.pos;
        self.pos += 1; // opening `'`
        if self.at_eof() {
            return Err(self.err(start, self.pos, "unterminated char literal"));
        }
        let value = if self.peek() == b'\\' {
            let (byte, next) = self.decode_escape(self.pos)?;
            self.pos = next;
            char::from(byte)
        } else {
            let ch = self.src[self.pos..].chars().next().unwrap_or('\u{FFFD}');
            self.pos += ch.len_utf8();
            ch
        };
        if self.at_eof() || self.peek() != b'\'' {
            return Err(self.err(
                start,
                self.pos,
                "char literal holds exactly one scalar; use a `String` for more",
            ));
        }
        self.pos += 1;
        Ok(Token::new(
            TokenKind::CharLit,
            &value.to_string(),
            start,
            self.pos,
            self.file,
        ))
    }

    /// Decodes one escape starting at `backslash`: the byte it denotes and the
    /// position after it. `\xHH` is one raw byte, never a UTF-8 encoding.
    fn decode_escape(&self, backslash: usize) -> Result<(u8, usize), Diagnostic> {
        let code = self.peek_at(backslash - self.pos + 1);
        match code {
            b'"' => Ok((b'"', backslash + 2)),
            b'\\' => Ok((b'\\', backslash + 2)),
            b'n' => Ok((b'\n', backslash + 2)),
            b'x' => {
                let hi = self.peek_at(backslash - self.pos + 2);
                let lo = self.peek_at(backslash - self.pos + 3);
                if !(hi.is_ascii_hexdigit() && lo.is_ascii_hexdigit()) {
                    return Err(self.err(
                        backslash,
                        backslash + 2,
                        "bad `\\x` escape; write exactly two hex digits like `\\x41`",
                    ));
                }
                let digits = &self.src[backslash + 2..backslash + 4];
                let byte = u8::from_str_radix(digits, 16).unwrap_or(0);
                Ok((byte, backslash + 4))
            }
            _ => Err(self.err(
                backslash,
                backslash + 2,
                "invalid escape; strings allow `\\\"`, `\\\\`, `\\n`, and `\\xHH` only",
            )),
        }
    }

    fn lex_fstr_start(&mut self) -> Result<Token, Diagnostic> {
        let start = self.pos;
        if self.peek_at(1) != b'"' {
            self.pos += 1;
            return Err(self.err(start, self.pos, "unexpected `$`; f-strings use `$\"...\"`"));
        }
        self.pos += 2;
        self.mode = LexMode::FStrLiteral;
        Ok(Token::new(
            TokenKind::FStrStart,
            "$\"",
            start,
            self.pos,
            self.file,
        ))
    }

    fn lex_fstr_literal(&mut self) -> Result<Token, Diagnostic> {
        let start = self.pos;
        let mut text = Vec::new();
        loop {
            if self.at_eof() {
                // Guarantee progress: the next call ends the iterator.
                self.mode = LexMode::Normal;
                self.eof_done = true;
                return Err(self.err(start, self.pos, "unterminated f-string; add a closing `\"`"));
            }
            match self.peek() {
                b'"' => {
                    if text.is_empty() {
                        let start_quote = self.pos;
                        self.pos += 1;
                        self.mode = LexMode::Normal;
                        return Ok(Token::new(
                            TokenKind::FStrEnd,
                            "",
                            start_quote,
                            self.pos,
                            self.file,
                        ));
                    }
                    // Leave the quote unconsumed so the next call emits `FStrEnd`.
                    self.mode = LexMode::PendingFStrEnd;
                    return Ok(Token::with_bytes(
                        TokenKind::FStrMid,
                        text,
                        start,
                        self.pos,
                        self.file,
                    ));
                }
                b'{' => {
                    if self.peek_at(1) == b'{' {
                        text.push(b'{');
                        self.pos += 2;
                    } else if text.is_empty() {
                        self.pos += 1;
                        self.mode = LexMode::Normal;
                        self.hole_depths.push(0);
                        return Ok(Token::new(
                            TokenKind::LBrace,
                            "{",
                            start,
                            self.pos,
                            self.file,
                        ));
                    } else {
                        self.mode = LexMode::PendingLBrace;
                        return Ok(Token::with_bytes(
                            TokenKind::FStrMid,
                            text,
                            start,
                            self.pos,
                            self.file,
                        ));
                    }
                }
                b'}' => {
                    if self.peek_at(1) == b'}' {
                        text.push(b'}');
                        self.pos += 2;
                    } else {
                        return Err(self.err(
                            self.pos,
                            self.pos + 1,
                            "single `}` in f-string; write `}}` for a literal brace",
                        ));
                    }
                }
                b'\\' => {
                    let (byte, next) = self.decode_escape(self.pos)?;
                    text.push(byte);
                    self.pos = next;
                }
                byte => {
                    text.push(byte);
                    self.pos += 1;
                }
            }
        }
    }

    fn lex_symbol(&mut self) -> Result<Token, Diagnostic> {
        let start = self.pos;
        if [self.peek(), self.peek_at(1), self.peek_at(2)] == [b'.'; 3] {
            self.pos += 3;
            return Ok(Token::new(
                TokenKind::Ellipsis,
                "...",
                start,
                self.pos,
                self.file,
            ));
        }
        let two = [self.peek(), self.peek_at(1)];
        let two_kind = match two {
            [b'=', b'='] => Some((TokenKind::EqEq, "==")),
            [b'=', b'>'] => Some((TokenKind::FatArrow, "=>")),
            [b'-', b'>'] => Some((TokenKind::Arrow, "->")),
            [b'<', b'='] => Some((TokenKind::LtEq, "<=")),
            [b'>', b'='] => Some((TokenKind::GtEq, ">=")),
            _ => None,
        };
        if let Some((kind, text)) = two_kind {
            self.pos += 2;
            return Ok(Token::new(kind, text, start, self.pos, self.file));
        }
        let (kind, text) = match self.peek() {
            b':' => (TokenKind::Colon, ":"),
            b',' => (TokenKind::Comma, ","),
            b'=' => (TokenKind::Eq, "="),
            b'.' => (TokenKind::Dot, "."),
            b'{' => {
                if let Some(depth) = self.hole_depths.last_mut() {
                    *depth += 1;
                }
                (TokenKind::LBrace, "{")
            }
            b'}' => {
                match self.hole_depths.last().copied() {
                    Some(0) => {
                        self.hole_depths.pop();
                        self.mode = LexMode::FStrLiteral;
                    }
                    Some(_) => {
                        if let Some(depth) = self.hole_depths.last_mut() {
                            *depth -= 1;
                        }
                    }
                    None => {}
                }
                (TokenKind::RBrace, "}")
            }
            b'(' => (TokenKind::LParen, "("),
            b')' => (TokenKind::RParen, ")"),
            b'[' => (TokenKind::LBracket, "["),
            b']' => (TokenKind::RBracket, "]"),
            b'+' => (TokenKind::Plus, "+"),
            b'-' => (TokenKind::Minus, "-"),
            b'*' => (TokenKind::Star, "*"),
            b'/' => (TokenKind::Slash, "/"),
            b'%' => (TokenKind::Percent, "%"),
            b'<' => (TokenKind::Lt, "<"),
            b'>' => (TokenKind::Gt, ">"),
            b'!' => (TokenKind::Bang, "!"),
            b'@' => (TokenKind::At, "@"),
            _ => {
                // Skip the offending char so errors always make progress.
                let bad = self.src[self.pos..].chars().next().unwrap_or('\u{FFFD}');
                self.pos += bad.len_utf8();
                return Err(self.err(
                    start,
                    self.pos,
                    "unexpected character; `[`/`]` are generics-only and indexing uses methods",
                ));
            }
        };
        self.pos += 1;
        Ok(Token::new(kind, text, start, self.pos, self.file))
    }
}

impl Iterator for Lexer<'_> {
    type Item = Result<Token, Diagnostic>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.eof_done {
            return None;
        }
        if self.mode == LexMode::PendingLBrace {
            self.mode = LexMode::Normal;
            let start = self.pos;
            self.pos += 1;
            self.hole_depths.push(0);
            return Some(Ok(Token::new(
                TokenKind::LBrace,
                "{",
                start,
                self.pos,
                self.file,
            )));
        }
        if self.mode == LexMode::PendingFStrEnd {
            self.mode = LexMode::Normal;
            let start = self.pos;
            self.pos += 1;
            return Some(Ok(Token::new(
                TokenKind::FStrEnd,
                "",
                start,
                self.pos,
                self.file,
            )));
        }
        if self.mode == LexMode::FStrLiteral {
            return Some(self.lex_fstr_literal());
        }
        let newline = self.skip_trivia();
        if self.at_eof() {
            self.eof_done = true;
            let pos = self.pos;
            let mut eof = Token::new(TokenKind::Eof, "", pos, pos, self.file);
            eof.newline_before = newline;
            return Some(Ok(eof));
        }
        Some(self.lex_normal().map(|mut token| {
            token.newline_before = newline;
            token
        }))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn keywords_are_keywords() {
        assert!(super::KEYWORDS
            .iter()
            .all(|word| super::keyword(word).is_some()));
    }

    use super::*;

    fn tokens_of(src: &str) -> Vec<Token> {
        let mut sm = lilac_span::SourceMap::new();
        let file = sm.add_file("test.lilac", src);
        Lexer::new(src, file)
            .collect::<Result<Vec<_>, _>>()
            .expect("lexing should succeed")
    }

    #[test]
    fn numbers_allow_separators() {
        let kinds: Vec<TokenKind> = tokens_of("0xFF_FF 0b10_10 10_000 10.5")
            .into_iter()
            .map(|token| token.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Number,
                TokenKind::Number,
                TokenKind::Number,
                TokenKind::Number,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn exponent_float_forms() {
        for src in ["3.14", "1e3", "1.5e-4", "2E10"] {
            let tokens = tokens_of(src);
            assert_eq!(tokens.len(), 2, "`{src}` should lex as one number and Eof");
            assert_eq!(tokens[0].kind, TokenKind::Number);
            assert_eq!(tokens[0].text, src);
        }
        for src in ["1.", "1e"] {
            let mut sm = lilac_span::SourceMap::new();
            let file = sm.add_file("test.lilac", src);
            let errors: Vec<Diagnostic> = Lexer::new(src, file).filter_map(Result::err).collect();
            assert!(!errors.is_empty(), "`{src}` should be a lex error");
            assert!(errors.iter().all(|error| error.code == "E000"));
        }
    }

    #[test]
    fn bad_escape_is_e000() {
        let mut sm = lilac_span::SourceMap::new();
        let file = sm.add_file("test.lilac", r#""bad \q""#);
        let errors: Vec<Diagnostic> = Lexer::new(r#""bad \q""#, file)
            .filter_map(Result::err)
            .collect();
        // Recovery keeps going past the typo, so every diagnostic is still E000.
        assert!(!errors.is_empty());
        assert!(errors.iter().all(|error| error.code == "E000"));
        assert!(errors[0].msg.contains("invalid escape"));
    }

    #[test]
    fn fstring_holes_use_expression_tokens() {
        let kinds: Vec<TokenKind> = tokens_of(r#"$"hi {name}!""#)
            .into_iter()
            .map(|token| token.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::FStrStart,
                TokenKind::FStrMid,
                TokenKind::LBrace,
                TokenKind::Ident,
                TokenKind::RBrace,
                TokenKind::FStrMid,
                TokenKind::FStrEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn function_header_lexes() {
        let texts: Vec<String> = tokens_of("main: function () nothing")
            .into_iter()
            .map(|token| token.text)
            .collect();
        assert_eq!(
            texts,
            vec!["main", ":", "function", "(", ")", "nothing", ""]
        );
    }

    #[test]
    fn kw_as_is_reserved() {
        let tokens = tokens_of("as");
        assert_eq!(
            tokens.iter().map(|token| token.kind).collect::<Vec<_>>(),
            vec![TokenKind::KwAs, TokenKind::Eof]
        );
        assert_eq!(tokens[0].text, "as");

        let kinds: Vec<TokenKind> = tokens_of("as = 1")
            .into_iter()
            .map(|token| token.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::KwAs,
                TokenKind::Eq,
                TokenKind::Number,
                TokenKind::Eof,
            ],
            "`as` must no longer lex as `Ident`"
        );
    }
}
