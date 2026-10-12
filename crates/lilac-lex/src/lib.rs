//! Lexer: `Iterator<Item = Result<Token, Diagnostic>>` over Lilac source.
//!
//! Implements `SPEC.md` §2: `0x`/`0b`/decimal/float numbers with `_`
//! separators, strings with `\"` `\\` `\n` `\xHH` escapes only, single-scalar
//! chars, and `$"..."` f-strings whose `{hole}` expressions are lexed with the
//! ordinary expression lexer (brace-depth tracked, so errors point inside the
//! braces). A string's or f-string's text ends at its line. A UTF-8
//! byte-order mark at the start of the file is trivia.
//!
//! Errors never stop lexing, and every item consumes input (see
//! [`Lexer`]): a bad literal is reported and still yields its token.

use lilac_error::{codes, Diagnostic};
use lilac_span::{FileId, Span, BYTE_ORDER_MARK};

/// Every keyword, for editor completion.
pub const KEYWORDS: [&str; 24] = [
    "object", "function", "enum", "alias", "extern", "export", "using", "private", "ref", "self",
    "unknown", "nothing", "loop", "if", "then", "else", "match", "break", "continue", "and", "or",
    "return", "as", "fail",
];

/// The keyword token `word` spells, if it is a keyword.
#[must_use]
pub fn keyword(word: &str) -> Option<TokenKind> {
    Some(match word {
        "object" => TokenKind::KwObject,
        "function" => TokenKind::KwFunction,
        "enum" => TokenKind::KwEnum,
        "alias" => TokenKind::KwAlias,
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
    /// `alias` (a type alias: `Name: alias Type`).
    KwAlias,
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
    /// The f-string's line or file ended before its `"`: a zero-width
    /// `FStrEnd` is waiting to be emitted, so the parser sees it closed.
    UnclosedFStrEnd,
}

/// Lilac lexer; see the module docs for the accepted grammar.
///
/// It yields `Err` only for bytes that form no token (a character the
/// language does not use, a `$` that starts no f-string). An error inside
/// a token (a bad escape, a lone `}` in f-string text, an unterminated or
/// malformed literal) is side output
/// ([`Lexer::take_diagnostics`]): the token is still yielded, so the
/// parser sees no hole.
///
/// Every item consumes at least one byte, except the end of file and the
/// zero-width `FStrEnd` that closes an unterminated f-string. `next`
/// checks this ([`Lexer::progressed`]), so no input makes it loop.
pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    file: FileId,
    mode: LexMode,
    hole_depths: Vec<usize>,
    /// Where each open f-string's `$"` starts (innermost last): the span of
    /// an unterminated one.
    fstrings: Vec<usize>,
    eof_done: bool,
    /// Errors inside tokens, in source order, not yet taken.
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Lexer<'a> {
    /// Creates a lexer over `src` (already registered as `file`). A UTF-8
    /// byte-order mark at the start is skipped as trivia; offsets stay
    /// those of `src`.
    #[must_use]
    pub fn new(src: &'a str, file: FileId) -> Self {
        let pos = if src.starts_with(BYTE_ORDER_MARK) {
            BYTE_ORDER_MARK.len_utf8()
        } else {
            0
        };
        Self {
            src,
            bytes: src.as_bytes(),
            pos,
            file,
            mode: LexMode::Normal,
            hole_depths: Vec::new(),
            fstrings: Vec::new(),
            eof_done: false,
            diagnostics: Vec::new(),
        }
    }

    /// Lexes everything, collecting recoverable errors instead of stopping.
    /// The errors are in source order: each item's own, then those found
    /// inside it.
    #[must_use]
    pub fn lex_all(src: &'a str, file: FileId) -> (Vec<Token>, Vec<Diagnostic>) {
        let mut lexer = Self::new(src, file);
        let mut tokens = Vec::new();
        let mut errors = Vec::new();
        while let Some(result) = lexer.next() {
            match result {
                Ok(token) => tokens.push(token),
                Err(diagnostic) => errors.push(diagnostic),
            }
            errors.append(&mut lexer.diagnostics);
        }
        (tokens, errors)
    }

    /// The errors found inside tokens since the last call, in source order
    /// (side output: the tokens themselves were still yielded).
    pub fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
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

    /// The character at the current position, if any.
    fn peek_char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn err(&self, start: usize, end: usize, msg: &str) -> Diagnostic {
        Diagnostic::error(codes::E000, msg).label(
            Span::new(offset_u32(start), offset_u32(end.max(start)), self.file),
            true,
            String::new(),
        )
    }

    /// Records an error inside the token being lexed.
    fn report(&mut self, start: usize, end: usize, msg: &str) {
        let diagnostic = self.err(start, end, msg);
        self.diagnostics.push(diagnostic);
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

    /// Consumes letters, digits and `_`.
    fn skip_word_chars(&mut self) {
        while !self.at_eof() && (self.peek().is_ascii_alphanumeric() || self.peek() == b'_') {
            self.pos += 1;
        }
    }

    fn lex_normal(&mut self) -> Result<Token, Diagnostic> {
        let byte = self.peek();
        if byte.is_ascii_alphabetic() || byte == b'_' {
            return Ok(self.lex_word());
        }
        if byte.is_ascii_digit() {
            return Ok(self.lex_number());
        }
        match byte {
            b'"' => Ok(self.lex_string()),
            b'\'' => Ok(self.lex_char()),
            b'$' => self.lex_fstr_start(),
            _ => self.lex_symbol(),
        }
    }

    fn lex_word(&mut self) -> Token {
        let start = self.pos;
        self.skip_word_chars();
        let word = &self.src[start..self.pos];
        let kind = keyword(word).unwrap_or(TokenKind::Ident);
        Token::new(kind, word, start, self.pos, self.file)
    }

    /// Whether every `_` in a literal's digits sits between two digits.
    fn separators_ok(digits: &str) -> bool {
        let digits = digits.as_bytes();
        digits.iter().enumerate().all(|(i, &byte)| {
            byte != b'_'
                || (i > 0
                    && digits[i - 1] != b'_'
                    && digits.get(i + 1).is_some_and(|&next| next != b'_'))
        })
    }

    /// A malformed number literal from `start`: it consumes the rest of the
    /// literal (letters, digits and `_`), reports `msg` over all of it, and
    /// is still a number token, so the parser sees no hole. Its text is
    /// `recovered`, a well-formed literal (the part read before the error),
    /// so later passes find nothing more to report.
    fn malformed_number(&mut self, start: usize, recovered: &str, msg: &str) -> Token {
        self.skip_word_chars();
        self.report(start, self.pos, msg);
        Token::new(TokenKind::Number, recovered, start, self.pos, self.file)
    }

    fn lex_number(&mut self) -> Token {
        let start = self.pos;
        let radix = match [self.peek(), self.peek_at(1)] {
            [b'0', b'x' | b'X'] => Some(("hex", 16)),
            [b'0', b'b' | b'B'] => Some(("binary", 2)),
            _ => None,
        };
        if let Some((base, radix)) = radix {
            self.pos += 2;
            self.skip_word_chars();
            let prefix = &self.src[start..start + 2];
            let digits = &self.src[start + 2..self.pos];
            let valid: String = digits
                .chars()
                .take_while(|&ch| ch == '_' || ch.is_digit(radix))
                .filter(|&ch| ch != '_')
                .collect();
            let recovered = format!("{prefix}{}", if valid.is_empty() { "0" } else { &valid });
            let problem = if digits.is_empty() {
                Some(format!("{base} literal needs digits after `{prefix}`"))
            } else if let Some(bad) = digits.chars().find(|&ch| ch != '_' && !ch.is_digit(radix)) {
                Some(format!("invalid digit `{bad}` in {base} literal"))
            } else if !Self::separators_ok(digits) {
                Some(MISPLACED_SEPARATOR.to_owned())
            } else {
                None
            };
            return match problem {
                Some(msg) => self.malformed_number(start, &recovered, &msg),
                None => self.number_token(start),
            };
        }
        // `valid_end`: where the well-formed part read so far ends.
        self.skip_digits();
        let mut valid_end = self.pos;
        if self.peek() == b'.' {
            if !self.peek_at(1).is_ascii_digit() {
                self.pos += 1;
                let recovered = self.recovered_decimal(start, valid_end);
                return self.malformed_number(
                    start,
                    &recovered,
                    "expected a digit after `.` in number literal",
                );
            }
            self.pos += 1;
            self.skip_digits();
            valid_end = self.pos;
        }
        if self.peek() == b'e' || self.peek() == b'E' {
            self.pos += 1;
            if self.peek() == b'+' || self.peek() == b'-' {
                self.pos += 1;
            }
            if !self.peek().is_ascii_digit() {
                let recovered = self.recovered_decimal(start, valid_end);
                return self.malformed_number(
                    start,
                    &recovered,
                    "expected a digit in the exponent of number literal",
                );
            }
            self.skip_digits();
            valid_end = self.pos;
        }
        if let Some(bad) = self.peek_char().filter(|&ch| ch.is_ascii_alphabetic()) {
            let recovered = self.recovered_decimal(start, valid_end);
            return self.malformed_number(
                start,
                &recovered,
                &format!("invalid digit `{bad}` in decimal literal"),
            );
        }
        if !Self::separators_ok(&self.src[start..self.pos]) {
            let recovered = self.recovered_decimal(start, valid_end);
            return self.malformed_number(start, &recovered, MISPLACED_SEPARATOR);
        }
        self.number_token(start)
    }

    /// The well-formed decimal literal `[start, end)` reads as, without its
    /// separators (so a misplaced one is gone too).
    fn recovered_decimal(&self, start: usize, end: usize) -> String {
        self.src[start..end].replace('_', "")
    }

    /// Consumes decimal digits and `_`.
    fn skip_digits(&mut self) {
        while !self.at_eof() && (self.peek().is_ascii_digit() || self.peek() == b'_') {
            self.pos += 1;
        }
    }

    fn number_token(&self, start: usize) -> Token {
        Token::new(
            TokenKind::Number,
            &self.src[start..self.pos],
            start,
            self.pos,
            self.file,
        )
    }

    /// A string literal. A bad escape or a missing closing `"` is reported,
    /// and the string still ends at its `"` (or its line): one error, one
    /// token holding the bytes decoded.
    fn lex_string(&mut self) -> Token {
        let start = self.pos;
        self.pos += 1; // opening `"`
        let mut value = Vec::new();
        loop {
            if self.at_eof() || self.peek() == b'\n' {
                self.report(
                    start,
                    self.pos,
                    "unterminated string literal; add a closing `\"`",
                );
                break;
            }
            match self.peek() {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => value.extend(self.escape()),
                byte => {
                    value.push(byte);
                    self.pos += 1;
                }
            }
        }
        Token::with_bytes(TokenKind::StringLit, value, start, self.pos, self.file)
    }

    /// A char literal: scanned to its closing `'` on the same line, and
    /// reported once when it does not hold exactly one scalar (a bad escape
    /// counts as one, and is reported itself). The token holds its first
    /// scalar, or U+FFFD when it has none.
    fn lex_char(&mut self) -> Token {
        let start = self.pos;
        self.pos += 1; // opening `'`
        let mut first = None;
        let mut scalars = 0usize;
        let closed = loop {
            if self.at_eof() || self.peek() == b'\n' {
                break false;
            }
            if self.peek() == b'\'' {
                self.pos += 1;
                break true;
            }
            let scalar = if self.peek() == b'\\' {
                self.escape().map(char::from)
            } else {
                let ch = self.peek_char().unwrap_or(char::REPLACEMENT_CHARACTER);
                self.pos += ch.len_utf8();
                Some(ch)
            };
            if scalars == 0 {
                first = scalar;
            }
            scalars += 1;
        };
        if !closed {
            self.report(
                start,
                self.pos,
                "unterminated char literal; add a closing `'`",
            );
        } else if scalars == 0 {
            self.report(
                start,
                self.pos,
                "empty char literal; it holds exactly one scalar",
            );
        } else if scalars > 1 {
            self.report(
                start,
                self.pos,
                "char literal holds exactly one scalar; use a `String` for more",
            );
        }
        let value = first.unwrap_or(char::REPLACEMENT_CHARACTER);
        Token::new(
            TokenKind::CharLit,
            &value.to_string(),
            start,
            self.pos,
            self.file,
        )
    }

    /// Consumes the escape at the current position (a `\`) and returns the
    /// byte it denotes; `\xHH` is one raw byte, never a UTF-8 encoding. A
    /// bad escape is reported and denotes nothing: it consumes the `\` and
    /// the character after it (unless the line ends there), or `\x` and
    /// the hex digits present.
    fn escape(&mut self) -> Option<u8> {
        let start = self.pos;
        let simple = match self.peek_at(1) {
            b'"' => Some(b'"'),
            b'\\' => Some(b'\\'),
            b'n' => Some(b'\n'),
            _ => None,
        };
        if let Some(byte) = simple {
            self.pos += 2;
            return Some(byte);
        }
        if self.peek_at(1) == b'x' {
            let digits = (2..4)
                .take_while(|&ahead| self.peek_at(ahead).is_ascii_hexdigit())
                .count();
            self.pos += 2 + digits;
            if digits == 2 {
                return u8::from_str_radix(&self.src[start + 2..self.pos], 16).ok();
            }
            self.report(
                start,
                self.pos,
                "bad `\\x` escape; write exactly two hex digits like `\\x41`",
            );
            return None;
        }
        self.pos += 1;
        if let Some(escaped) = self.peek_char().filter(|&ch| ch != '\n') {
            self.pos += escaped.len_utf8();
        }
        self.report(
            start,
            self.pos,
            "invalid escape; strings allow `\\\"`, `\\\\`, `\\n`, and `\\xHH` only",
        );
        None
    }

    fn lex_fstr_start(&mut self) -> Result<Token, Diagnostic> {
        let start = self.pos;
        if self.peek_at(1) != b'"' {
            self.pos += 1;
            return Err(self.err(start, self.pos, "unexpected `$`; f-strings use `$\"...\"`"));
        }
        self.pos += 2;
        self.mode = LexMode::FStrLiteral;
        self.fstrings.push(start);
        Ok(Token::new(
            TokenKind::FStrStart,
            "$\"",
            start,
            self.pos,
            self.file,
        ))
    }

    /// The `FStrEnd` of the innermost f-string, covering `[start, end)`;
    /// lexing continues in ordinary code.
    fn fstr_end(&mut self, start: usize, end: usize) -> Token {
        self.mode = LexMode::Normal;
        self.fstrings.pop();
        Token::new(TokenKind::FStrEnd, "", start, end, self.file)
    }

    /// F-string text up to its closing `"`, a hole's `{`, or the end of
    /// its line or file (an unterminated f-string, closed by a zero-width
    /// `FStrEnd`). A lone `}` and a bad escape are reported, and the text
    /// goes on after them.
    fn lex_fstr_literal(&mut self) -> Token {
        let start = self.pos;
        let mut text = Vec::new();
        loop {
            if self.at_eof() || self.peek() == b'\n' {
                let open = self.fstrings.last().copied().unwrap_or(start);
                self.report(open, self.pos, "unterminated f-string; add a closing `\"`");
                if text.is_empty() {
                    return self.fstr_end(self.pos, self.pos);
                }
                self.mode = LexMode::UnclosedFStrEnd;
                return Token::with_bytes(TokenKind::FStrMid, text, start, self.pos, self.file);
            }
            match self.peek() {
                b'"' => {
                    if text.is_empty() {
                        let quote = self.pos;
                        self.pos += 1;
                        return self.fstr_end(quote, self.pos);
                    }
                    // Leave the quote unconsumed so the next call emits `FStrEnd`.
                    self.mode = LexMode::PendingFStrEnd;
                    return Token::with_bytes(TokenKind::FStrMid, text, start, self.pos, self.file);
                }
                b'{' => {
                    if self.peek_at(1) == b'{' {
                        text.push(b'{');
                        self.pos += 2;
                    } else if text.is_empty() {
                        self.pos += 1;
                        self.mode = LexMode::Normal;
                        self.hole_depths.push(0);
                        return Token::new(TokenKind::LBrace, "{", start, self.pos, self.file);
                    } else {
                        self.mode = LexMode::PendingLBrace;
                        return Token::with_bytes(
                            TokenKind::FStrMid,
                            text,
                            start,
                            self.pos,
                            self.file,
                        );
                    }
                }
                b'}' => {
                    text.push(b'}');
                    if self.peek_at(1) == b'}' {
                        self.pos += 2;
                    } else {
                        // A lone `}` reads as the literal brace the fix
                        // writes, and the text goes on.
                        let brace =
                            Span::new(offset_u32(self.pos), offset_u32(self.pos + 1), self.file);
                        let diagnostic = self
                            .err(self.pos, self.pos + 1, "single `}` in f-string text")
                            .suggest(brace, "}}");
                        self.diagnostics.push(diagnostic);
                        self.pos += 1;
                    }
                }
                b'\\' => text.extend(self.escape()),
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
                let bad = self.peek_char().unwrap_or(char::REPLACEMENT_CHARACTER);
                self.pos += bad.len_utf8();
                return Err(self.err(
                    start,
                    self.pos,
                    &format!("unexpected character {}", describe_char(bad)),
                ));
            }
        };
        self.pos += 1;
        Ok(Token::new(kind, text, start, self.pos, self.file))
    }

    /// The next item, before the progress check.
    fn lex_item(&mut self) -> Result<Token, Diagnostic> {
        match self.mode {
            LexMode::PendingLBrace => {
                self.mode = LexMode::Normal;
                let start = self.pos;
                self.pos += 1;
                self.hole_depths.push(0);
                return Ok(Token::new(
                    TokenKind::LBrace,
                    "{",
                    start,
                    self.pos,
                    self.file,
                ));
            }
            LexMode::PendingFStrEnd => {
                let quote = self.pos;
                self.pos += 1;
                return Ok(self.fstr_end(quote, self.pos));
            }
            LexMode::UnclosedFStrEnd => return Ok(self.fstr_end(self.pos, self.pos)),
            LexMode::FStrLiteral => return Ok(self.lex_fstr_literal()),
            LexMode::Normal => {}
        }
        let newline = self.skip_trivia();
        if self.at_eof() {
            self.eof_done = true;
            let pos = self.pos;
            let mut eof = Token::new(TokenKind::Eof, "", pos, pos, self.file);
            eof.newline_before = newline;
            return Ok(eof);
        }
        self.lex_normal().map(|mut token| {
            token.newline_before = newline;
            token
        })
    }

    /// Whether an item lexed from `entry` (in `entry_mode`) made progress:
    /// it consumed a byte, or it is the end of file, or the zero-width
    /// `FStrEnd` closing an unterminated f-string (which leaves f-string
    /// text, entered only by consuming a `$"` or a hole's `}`).
    fn progressed(
        &self,
        entry: usize,
        entry_mode: LexMode,
        item: &Result<Token, Diagnostic>,
    ) -> bool {
        let closed_in_place = matches!(
            item,
            Ok(token) if token.kind == TokenKind::FStrEnd && token.span.lo == token.span.hi
        );
        self.pos > entry
            || self.eof_done
            || (matches!(entry_mode, LexMode::FStrLiteral | LexMode::UnclosedFStrEnd)
                && self.mode == LexMode::Normal
                && closed_in_place)
    }

    /// Recovery from a lexer bug ([`Lexer::progressed`] failed): consumes
    /// one character (or, at the end of the file, leaves f-string text so
    /// the next item is the end of file) and reports it. `item`, the item
    /// that made no progress, is dropped; an error it carried is kept.
    fn force_progress(&mut self, item: Result<Token, Diagnostic>) -> Diagnostic {
        if let Err(diagnostic) = item {
            self.diagnostics.push(diagnostic);
        }
        let start = self.pos;
        if let Some(ch) = self.peek_char() {
            self.pos += ch.len_utf8();
        } else {
            self.mode = LexMode::Normal;
            self.hole_depths.clear();
            self.fstrings.clear();
        }
        self.err(
            start,
            self.pos,
            "internal lexer error: no progress here, so this character was skipped \
             (please report this bug)",
        )
    }
}

/// The error for a `_` that is not between two digits.
const MISPLACED_SEPARATOR: &str = "misplaced `_` separator; it must sit between digits";

/// How an error names a character it found: "`#`", or, for one that does
/// not show (a control, a space other than ASCII's, a format character)
/// and for a backtick, its code point ("U+FEFF").
fn describe_char(ch: char) -> String {
    let invisible = ch.is_control()
        || ch.is_whitespace()
        || matches!(
            ch,
            '`' | '\u{AD}'
                | '\u{34F}'
                | '\u{61C}'
                | '\u{115F}'
                | '\u{1160}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF0}'..='\u{FFFB}'
                | '\u{E0000}'..='\u{E0FFF}'
        );
    if invisible {
        format!("U+{:04X}", u32::from(ch))
    } else {
        format!("`{ch}`")
    }
}

impl Iterator for Lexer<'_> {
    type Item = Result<Token, Diagnostic>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.eof_done {
            return None;
        }
        let (entry, entry_mode) = (self.pos, self.mode);
        let item = self.lex_item();
        let progressed = self.progressed(entry, entry_mode, &item);
        debug_assert!(
            progressed,
            "the lexer made no progress at byte {entry}: {item:?}"
        );
        if !progressed {
            return Some(Err(self.force_progress(item)));
        }
        Some(item)
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
    }

    /// Every token and every error (items' and side output) of `src`.
    fn lex(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
        let mut sm = lilac_span::SourceMap::new();
        let file = sm.add_file("test.lilac", src);
        Lexer::lex_all(src, file)
    }

    fn kinds(tokens: &[Token]) -> Vec<TokenKind> {
        tokens.iter().map(|token| token.kind).collect()
    }

    /// The `[lo, hi)` byte range of a diagnostic's primary label.
    fn range(diagnostic: &Diagnostic) -> (u32, u32) {
        let label = diagnostic
            .labels
            .iter()
            .find(|label| label.primary)
            .expect("a primary label");
        (label.span.lo.0, label.span.hi.0)
    }

    /// Asserts `src` reports exactly one error, containing `msg`, over
    /// `[lo, hi)`, and returns the tokens.
    fn one_error(src: &str, msg: &str, lo: u32, hi: u32) -> Vec<Token> {
        let (tokens, errors) = lex(src);
        let [error] = errors.as_slice() else {
            panic!("{src:?}: one error, found {errors:#?}");
        };
        assert_eq!(error.code, "E000", "{src:?}");
        assert!(error.msg.contains(msg), "{src:?}: {}", error.msg);
        assert_eq!(range(error), (lo, hi), "{src:?}: {}", error.msg);
        tokens
    }

    #[test]
    fn bad_string_escape_is_one_error_and_one_string() {
        let tokens = one_error(r#""bad \q""#, "invalid escape", 5, 7);
        assert_eq!(kinds(&tokens), [TokenKind::StringLit, TokenKind::Eof]);
        assert_eq!(tokens[0].bytes, b"bad ");
        assert_eq!((tokens[0].span.lo.0, tokens[0].span.hi.0), (0, 8));

        let tokens = one_error(r#""a\qb\n""#, "invalid escape", 2, 4);
        assert_eq!(tokens[0].bytes, b"ab\n");
        // The escaped character is a whole scalar, not a byte.
        let tokens = one_error("\"\\é\"", "invalid escape", 1, 4);
        assert_eq!(kinds(&tokens), [TokenKind::StringLit, TokenKind::Eof]);
        // `\x` with too few hex digits consumes the digits it has.
        let tokens = one_error(r#""\x4z""#, "bad `\\x` escape", 1, 4);
        assert_eq!(tokens[0].bytes, b"z");
        let tokens = one_error(r#""\x4""#, "bad `\\x`", 1, 4);
        assert_eq!(tokens[0].bytes, b"");
        let (tokens, errors) = lex(r#""\x41\xff""#);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(tokens[0].bytes, [0x41, 0xFF]);
    }

    #[test]
    fn unterminated_string_is_still_a_string() {
        let tokens = one_error("\"abc\nx", "unterminated string", 0, 4);
        assert_eq!(
            kinds(&tokens),
            [TokenKind::StringLit, TokenKind::Ident, TokenKind::Eof]
        );
        assert_eq!(tokens[0].bytes, b"abc");
        // A `\` that ends the line escapes nothing; the line still ends.
        let (tokens, errors) = lex("\"a\\\nx");
        assert_eq!(errors.len(), 2, "{errors:#?}");
        assert!(errors[0].msg.contains("invalid escape"));
        assert_eq!(range(&errors[0]), (2, 3));
        assert!(errors[1].msg.contains("unterminated string"));
        assert_eq!(
            kinds(&tokens),
            [TokenKind::StringLit, TokenKind::Ident, TokenKind::Eof]
        );
    }

    #[test]
    fn char_literals_report_once() {
        let tokens = one_error("'ab' x", "exactly one scalar", 0, 4);
        assert_eq!(
            kinds(&tokens),
            [TokenKind::CharLit, TokenKind::Ident, TokenKind::Eof]
        );
        assert_eq!(tokens[0].text, "a");
        let tokens = one_error("''", "empty char literal", 0, 2);
        assert_eq!(kinds(&tokens), [TokenKind::CharLit, TokenKind::Eof]);
        let tokens = one_error(r"'\q'", "invalid escape", 1, 3);
        assert_eq!(kinds(&tokens), [TokenKind::CharLit, TokenKind::Eof]);
        let tokens = one_error("'a b\nc", "unterminated char literal", 0, 4);
        assert_eq!(
            kinds(&tokens),
            [TokenKind::CharLit, TokenKind::Ident, TokenKind::Eof]
        );
        let (tokens, errors) = lex(r"'é' '\x41' '\\'");
        assert!(errors.is_empty(), "{errors:?}");
        let texts: Vec<&str> = tokens.iter().map(|token| token.text.as_str()).collect();
        assert_eq!(texts, ["é", "A", "\\", ""]);
    }

    #[test]
    fn malformed_numbers_are_one_error_and_one_number() {
        for (src, msg, recovered) in [
            ("0x1g2", "invalid digit `g` in hex literal", "0x1"),
            ("0b102", "invalid digit `2` in binary literal", "0b10"),
            ("0xzz", "invalid digit `z` in hex literal", "0x0"),
            ("0x_1", "misplaced `_` separator", "0x1"),
            ("12abc", "invalid digit `a` in decimal literal", "12"),
            ("1.foo", "expected a digit after `.`", "1"),
            ("1.", "expected a digit after `.`", "1"),
            ("1e5x", "invalid digit `x` in decimal literal", "1e5"),
            ("1ex", "expected a digit in the exponent", "1"),
            ("1e", "expected a digit in the exponent", "1"),
            ("1.5e+", "expected a digit in the exponent", "1.5"),
            ("1__0", "misplaced `_` separator", "10"),
        ] {
            let end = u32::try_from(src.len()).expect("short");
            let tokens = one_error(src, msg, 0, end);
            assert_eq!(
                kinds(&tokens),
                [TokenKind::Number, TokenKind::Eof],
                "{src:?}"
            );
            assert_eq!(tokens[0].text, recovered, "{src:?}");
            assert_eq!((tokens[0].span.lo.0, tokens[0].span.hi.0), (0, end));
        }
        let tokens = one_error("0x+", "hex literal needs digits after `0x`", 0, 2);
        assert_eq!(
            kinds(&tokens),
            [TokenKind::Number, TokenKind::Plus, TokenKind::Eof]
        );
        assert_eq!(tokens[0].text, "0x0");
    }

    #[test]
    fn lone_fstring_brace_is_reported_and_kept() {
        let tokens = one_error(r#"$"a}b""#, "single `}` in f-string text", 3, 4);
        assert_eq!(
            kinds(&tokens),
            [
                TokenKind::FStrStart,
                TokenKind::FStrMid,
                TokenKind::FStrEnd,
                TokenKind::Eof,
            ]
        );
        assert_eq!(tokens[1].bytes, b"a}b");
        let (_, errors) = lex(r#"$"a}b""#);
        let fixes: Vec<&str> = errors[0]
            .suggestions()
            .map(|fix| fix.replacement.as_str())
            .collect();
        assert_eq!(fixes, ["}}"]);
    }

    #[test]
    fn bad_fstring_escape_continues_the_text() {
        let tokens = one_error(r#"$"\q""#, "invalid escape", 2, 4);
        assert_eq!(
            kinds(&tokens),
            [TokenKind::FStrStart, TokenKind::FStrEnd, TokenKind::Eof]
        );
        let tokens = one_error(r#"$"a\qb{x}c""#, "invalid escape", 3, 5);
        assert_eq!(
            kinds(&tokens),
            [
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
        assert_eq!(tokens[1].bytes, b"ab");

        let (tokens, errors) = lex(r#"$"" x"#);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            kinds(&tokens),
            [
                TokenKind::FStrStart,
                TokenKind::FStrEnd,
                TokenKind::Ident,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn fstring_text_ends_at_its_line() {
        // `\"` is a valid escape, so the literal runs to the line's end; the
        // `}` on the next line is code again.
        let tokens = one_error("$\"a\\\"\n}\n", "unterminated f-string", 0, 5);
        assert_eq!(
            kinds(&tokens),
            [
                TokenKind::FStrStart,
                TokenKind::FStrMid,
                TokenKind::FStrEnd,
                TokenKind::RBrace,
                TokenKind::Eof,
            ]
        );
        assert_eq!(tokens[1].bytes, b"a\"");
        assert_eq!((tokens[2].span.lo.0, tokens[2].span.hi.0), (5, 5));
        for src in ["$\"", "$\"ab", "$\"{x}"] {
            let (tokens, errors) = lex(src);
            assert_eq!(errors.len(), 1, "{src:?}: {errors:#?}");
            assert!(errors[0].msg.contains("unterminated f-string"));
            assert_eq!(range(&errors[0]).0, 0, "{src:?}");
            let kinds = kinds(&tokens);
            assert_eq!(kinds.first(), Some(&TokenKind::FStrStart), "{src:?}");
            assert_eq!(
                &kinds[kinds.len() - 2..],
                [TokenKind::FStrEnd, TokenKind::Eof],
                "{src:?}"
            );
        }
        // A nested f-string that ends with its line leaves the outer hole.
        let (tokens, errors) = lex("$\"{$\"ab\n}c\"");
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(range(&errors[0]), (3, 7));
        assert_eq!(
            kinds(&tokens),
            [
                TokenKind::FStrStart,
                TokenKind::LBrace,
                TokenKind::FStrStart,
                TokenKind::FStrMid,
                TokenKind::FStrEnd,
                TokenKind::RBrace,
                TokenKind::FStrMid,
                TokenKind::FStrEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn unexpected_characters_are_named() {
        one_error("a # b", "unexpected character `#`", 2, 3);
        one_error("é", "unexpected character `é`", 0, 2);
        one_error("a\u{200B}b", "unexpected character U+200B", 1, 4);
        one_error("\u{7}", "unexpected character U+0007", 0, 1);
        one_error("\u{A0}", "unexpected character U+00A0", 0, 2);
        one_error("`", "unexpected character U+0060", 0, 1);
        let (_, errors) = lex("#");
        assert!(!errors[0].msg.contains("generics"), "{}", errors[0].msg);
    }

    #[test]
    fn byte_order_mark_is_trivia_at_the_start_only() {
        let (tokens, errors) = lex("\u{FEFF}main");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(kinds(&tokens), [TokenKind::Ident, TokenKind::Eof]);
        // Offsets stay the file's bytes.
        assert_eq!((tokens[0].span.lo.0, tokens[0].span.hi.0), (3, 7));
        one_error("main\u{FEFF}", "unexpected character U+FEFF", 4, 7);
    }

    #[test]
    fn errors_come_in_source_order() {
        let (_, errors) = lex("# \"\\q\" $ '' @");
        let starts: Vec<u32> = errors.iter().map(|error| range(error).0).collect();
        assert_eq!(starts, [0, 3, 7, 9]);
    }

    /// The recovery `next` uses when an item made no progress (a lexer
    /// bug, which debug builds assert against): one character is skipped
    /// and reported, and an error the item carried is kept.
    #[test]
    fn a_stalled_lexer_skips_one_character() {
        let mut sm = lilac_span::SourceMap::new();
        let file = sm.add_file("test.lilac", "é#");
        let mut lexer = Lexer::new("é#", file);
        let stalled = lexer.err(0, 0, "stalled");
        let error = lexer.force_progress(Err(stalled));
        assert_eq!(lexer.pos, 2);
        assert_eq!(range(&error), (0, 2));
        assert!(error.msg.contains("internal lexer error"));
        assert_eq!(lexer.take_diagnostics().len(), 1);

        // At the end of the file it leaves f-string text, so the end of
        // file comes next.
        let src = "$\"";
        let mut lexer = Lexer::new(src, file);
        assert!(matches!(lexer.next(), Some(Ok(token)) if token.kind == TokenKind::FStrStart));
        lexer.pos = src.len();
        let _ = lexer.force_progress(Ok(Token::new(TokenKind::Dot, ".", 2, 2, file)));
        assert!(matches!(lexer.next(), Some(Ok(token)) if token.kind == TokenKind::Eof));
        assert!(lexer.next().is_none());
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
