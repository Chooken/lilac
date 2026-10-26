//! What is at the cursor, read from the document's tokens.
//!
//! Completion and signature help run on the text being edited, before it
//! is compiled, so they cannot ask the compiler what surrounds the cursor.
//! They ask the lexer instead: it never fails, so every question here is
//! answered from tokens and comment spans, never by scanning characters.
//! A string's or comment's content is one token, so a `.`, `(` or `,`
//! inside one is never mistaken for code, and an f-string's holes are
//! code like any other.

use lilac_lex::{Comment, Lexer, Token, TokenKind};
use lilac_span::FileId;

/// The token-level view of a document around one cursor.
pub struct Context<'a> {
    text: &'a str,
    /// Every token but the end of file, in source order.
    tokens: Vec<Token>,
    cursor: u32,
    in_comment: bool,
    in_string: bool,
}

/// What precedes the name being typed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum After {
    /// An `@` right before it: an intrinsic, a builtin type or `@builtin`.
    At,
    /// A `.` on the cursor's line: a member, a tag or a path step.
    Dot,
    /// Anything else: a plain name.
    Other,
}

/// The name being completed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Completing {
    /// Where the replaced text starts (the `@` for [`After::At`]; the
    /// cursor when no name is typed yet).
    pub start: usize,
    /// What precedes the name.
    pub after: After,
}

impl<'a> Context<'a> {
    /// Lexes `text` and places `cursor` (a byte offset) in it.
    #[must_use]
    pub fn new(text: &'a str, cursor: usize) -> Self {
        let mut lexer = Lexer::new(text, FileId(0));
        let tokens: Vec<Token> = lexer
            .by_ref()
            .filter_map(Result::ok)
            .filter(|token| token.kind != TokenKind::Eof)
            .collect();
        let comments = lexer.take_comments();
        let cursor = u32::try_from(cursor).unwrap_or(u32::MAX);
        let in_comment = comments.iter().any(|comment| contains(comment, cursor));
        let in_string = !in_comment && string_at(&tokens, cursor);
        Self {
            text,
            tokens,
            cursor,
            in_comment,
            in_string,
        }
    }

    /// Whether the cursor is in a comment (after its `//`, through the end
    /// of its line).
    #[must_use]
    pub fn in_comment(&self) -> bool {
        self.in_comment
    }

    /// Whether the cursor is in a string or char literal's text, or an
    /// f-string's text outside its holes (an unterminated one runs to the
    /// end of its line).
    #[must_use]
    pub fn in_string(&self) -> bool {
        self.in_string
    }

    /// Whether the cursor is in code: neither a comment nor literal text.
    #[must_use]
    pub fn in_code(&self) -> bool {
        !self.in_comment && !self.in_string
    }

    /// The name the cursor is typing and what precedes it; `None` outside
    /// code.
    #[must_use]
    pub fn completing(&self) -> Option<Completing> {
        if !self.in_code() {
            return None;
        }
        let word = self.tokens.iter().position(|token| {
            lo(token) < self.cursor && self.cursor <= hi(token) && is_word(token)
        });
        let start = word.map_or(self.cursor, |index| lo(&self.tokens[index]));
        let before = match word {
            Some(index) => index.checked_sub(1),
            None => self.tokens.iter().rposition(|token| hi(token) <= start),
        }
        .map(|index| &self.tokens[index]);
        let completing = |start: u32, after| Completing {
            start: start as usize,
            after,
        };
        Some(match before {
            Some(at) if at.kind == TokenKind::At && hi(at) == start => {
                completing(lo(at), After::At)
            }
            Some(dot) if dot.kind == TokenKind::Dot && !self.line_break(hi(dot), start) => {
                completing(start, After::Dot)
            }
            _ => completing(start, After::Other),
        })
    }

    /// Whether the cursor follows a `.` (with maybe part of a name): only a
    /// compile of the current text knows the enum a tag expects or what a
    /// member is looked up in.
    #[must_use]
    pub fn after_dot(&self) -> bool {
        self.completing()
            .is_some_and(|completing| completing.after == After::Dot)
    }

    /// The call whose parentheses hold the cursor: its callee path
    /// (`add`, `p.sum`, `@i_add_8`) and the index of the argument the
    /// cursor is in. Brackets are matched over tokens, so strings,
    /// characters, comments, f-string holes and struct literals are
    /// skipped whole; `None` when the innermost open bracket is not a
    /// call's `(` or the cursor is in a comment.
    #[must_use]
    pub fn call(&self) -> Option<(String, u32)> {
        self.open_call()
            .map(|(callee, argument, _)| (callee, argument))
    }

    /// The first argument of the call [`Context::call`] finds, when it is
    /// a dotted path (`Point`, `Shapes.Line`) ended by a `,` before the
    /// cursor: the type operand of a reflection test.
    #[must_use]
    pub fn first_argument(&self) -> Option<String> {
        let (_, _, paren) = self.open_call()?;
        let mut path = String::new();
        let mut index = paren + 1;
        loop {
            let name = self.tokens.get(index)?;
            if !is_path_name(name) {
                return None;
            }
            path.push_str(&name.text);
            let next = self.tokens.get(index + 1)?;
            match next.kind {
                TokenKind::Dot => path.push('.'),
                TokenKind::Comma if hi(next) <= self.cursor => return Some(path),
                _ => return None,
            }
            index += 2;
        }
    }

    /// [`Context::call`], with the index of the call's `(` token.
    fn open_call(&self) -> Option<(String, u32, usize)> {
        if self.in_comment {
            return None;
        }
        let mut open: Vec<Open> = Vec::new();
        for (index, token) in self.tokens.iter().enumerate() {
            if lo(token) >= self.cursor {
                break;
            }
            match token.kind {
                TokenKind::LParen => open.push(Open::Paren {
                    callee: self.path_ending(index),
                    commas: 0,
                    at: index,
                }),
                TokenKind::LBracket => open.push(Open::Bracket),
                TokenKind::LBrace => open.push(Open::Brace),
                TokenKind::RParen => close(&mut open, |open| matches!(open, Open::Paren { .. })),
                TokenKind::RBracket => close(&mut open, |open| matches!(open, Open::Bracket)),
                TokenKind::RBrace => close(&mut open, |open| matches!(open, Open::Brace)),
                TokenKind::Comma => {
                    if let Some(Open::Paren { commas, .. }) = open.last_mut() {
                        *commas += 1;
                    }
                }
                _ => {}
            }
        }
        match open.pop()? {
            Open::Paren {
                callee: Some(callee),
                commas,
                at,
            } => Some((callee, commas, at)),
            _ => None,
        }
    }

    /// The dotted path whose last token is just before `tokens[end]`
    /// (`a.b`, `@builtin.Target.name`), if it is one.
    fn path_ending(&self, end: usize) -> Option<String> {
        let mut index = end.checked_sub(1)?;
        if !is_path_name(&self.tokens[index]) {
            return None;
        }
        let mut start = index;
        while index >= 2
            && self.tokens[index - 1].kind == TokenKind::Dot
            && is_path_name(&self.tokens[index - 2])
        {
            index -= 2;
            start = index;
        }
        if let Some(at) = start.checked_sub(1).map(|at| &self.tokens[at]) {
            if at.kind == TokenKind::At && hi(at) == lo(&self.tokens[start]) {
                start -= 1;
            }
        }
        let mut path = String::new();
        for token in &self.tokens[start..end] {
            match token.kind {
                TokenKind::At => path.push('@'),
                TokenKind::Dot => path.push('.'),
                _ => path.push_str(&token.text),
            }
        }
        Some(path)
    }

    /// Whether a line break separates `from` and `to`, two offsets with
    /// only trivia between them.
    fn line_break(&self, from: u32, to: u32) -> bool {
        self.text
            .get(from as usize..to as usize)
            .is_some_and(|between| between.contains('\n'))
    }
}

/// Whether `name` is exactly one identifier token (not a keyword).
#[must_use]
pub fn is_identifier(name: &str) -> bool {
    let (tokens, errors) = Lexer::lex_all(name, FileId(0));
    errors.is_empty()
        && matches!(
            tokens.as_slice(),
            [token, end] if token.kind == TokenKind::Ident
                && token.text == name
                && lo(token) == 0
                && end.kind == TokenKind::Eof
        )
}

/// A bracket open before the cursor.
enum Open {
    /// `(`, with the path just before it when it is a call's, and its
    /// index among the tokens.
    Paren {
        callee: Option<String>,
        commas: u32,
        at: usize,
    },
    Bracket,
    Brace,
}

/// Closes the innermost open bracket `matches` accepts, and everything
/// opened inside it; a closer with no opener is ignored.
fn close(open: &mut Vec<Open>, matches: impl Fn(&Open) -> bool) {
    if let Some(index) = open.iter().rposition(matches) {
        open.truncate(index);
    }
}

/// Whether the cursor is in literal text (see [`Context::in_string`]).
fn string_at(tokens: &[Token], cursor: u32) -> bool {
    // Whether f-string text follows each token: after its start, a text
    // chunk, or a hole's closing `}`.
    let mut holes: Vec<bool> = Vec::new();
    let mut text_follows = false;
    for token in tokens {
        if lo(token) >= cursor {
            // At a boundary, what came before decides; a token past the
            // cursor cannot.
            return text_follows && lo(token) == cursor;
        }
        if cursor < hi(token) {
            return matches!(
                token.kind,
                TokenKind::StringLit | TokenKind::CharLit | TokenKind::FStrMid
            );
        }
        // The token ends at or before the cursor. (An unterminated
        // f-string's zero-width end is at a boundary, handled above.)
        let unclosed_here = token.unclosed && hi(token) == cursor;
        text_follows = match token.kind {
            TokenKind::FStrStart | TokenKind::FStrMid => true,
            TokenKind::LBrace => {
                holes.push(text_follows);
                false
            }
            TokenKind::RBrace => holes.pop().unwrap_or(false),
            _ => false,
        };
        if unclosed_here {
            return true;
        }
    }
    text_follows
}

/// Whether `comment` holds `cursor`: after its `//`, through its line's end.
fn contains(comment: &Comment, cursor: u32) -> bool {
    comment.span.lo.0 < cursor && cursor <= comment.span.hi.0
}

/// A name being typed: an identifier or a keyword (a prefix of a longer
/// name may spell one).
fn is_word(token: &Token) -> bool {
    token.kind == TokenKind::Ident || lilac_lex::keyword(&token.text) == Some(token.kind)
}

/// A token that can be a step of a callee path.
fn is_path_name(token: &Token) -> bool {
    matches!(token.kind, TokenKind::Ident | TokenKind::KwSelf)
}

fn lo(token: &Token) -> u32 {
    token.span.lo.0
}

fn hi(token: &Token) -> u32 {
    token.span.hi.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The context at the `|` in `marked`.
    fn at(marked: &str) -> (String, usize) {
        let cursor = marked.find('|').expect("a cursor");
        (marked.replacen('|', "", 1), cursor)
    }

    fn call(marked: &str) -> Option<(String, u32)> {
        let (text, cursor) = at(marked);
        Context::new(&text, cursor).call()
    }

    fn first_argument(marked: &str) -> Option<String> {
        let (text, cursor) = at(marked);
        Context::new(&text, cursor).first_argument()
    }

    fn string(marked: &str) -> bool {
        let (text, cursor) = at(marked);
        Context::new(&text, cursor).in_string()
    }

    fn comment(marked: &str) -> bool {
        let (text, cursor) = at(marked);
        Context::new(&text, cursor).in_comment()
    }

    fn completing(marked: &str) -> Option<Completing> {
        let (text, cursor) = at(marked);
        Context::new(&text, cursor).completing()
    }

    #[test]
    fn calls_are_found_back_to_their_paren() {
        assert_eq!(call("x = add(1, f(2), |"), Some(("add".to_owned(), 2)));
        assert_eq!(call("p.sum(|"), Some(("p.sum".to_owned(), 0)));
        assert_eq!(call("self.sum(|"), Some(("self.sum".to_owned(), 0)));
        assert_eq!(call("@i_add_8(1, |"), Some(("@i_add_8".to_owned(), 1)));
        assert_eq!(
            call("@builtin.Target.f(|"),
            Some(("@builtin.Target.f".to_owned(), 0))
        );
        assert_eq!(call("f(\"a,(\", |"), Some(("f".to_owned(), 1)));
        assert_eq!(call("f('(', |"), Some(("f".to_owned(), 1)));
        assert_eq!(call("f(P { x = 1, y = 2 }, |"), Some(("f".to_owned(), 1)));
        assert_eq!(call("f($\"{a, b}\", |"), Some(("f".to_owned(), 1)));
        assert_eq!(call("f(a // (,\n, |"), Some(("f".to_owned(), 1)));
        assert_eq!(call("f(a[1, 2], |"), Some(("f".to_owned(), 1)));
        assert_eq!(call("f(1, |2)"), Some(("f".to_owned(), 1)));
        assert_eq!(call("f|(1)"), None);
        assert_eq!(call("f(1) |"), None);
        assert_eq!(call("(a + |"), None);
        assert_eq!(call("{ x }|"), None);
        assert_eq!(call("f(P { x = |"), None);
        assert_eq!(call("f(1, // |"), None);
    }

    #[test]
    fn a_path_first_argument_is_read() {
        let path = |path: &str| Some(path.to_owned());
        assert_eq!(first_argument("@has_field(Point, |"), path("Point"));
        assert_eq!(first_argument("@has_field(Point, na|"), path("Point"));
        assert_eq!(first_argument("f(A.B.Point, x, |"), path("A.B.Point"));
        assert_eq!(first_argument("f(g(Point, |"), path("Point"));
        assert_eq!(first_argument("f(Point|"), None);
        assert_eq!(first_argument("f(Box[Int], |"), None);
        assert_eq!(first_argument("f(a + b, |"), None);
        assert_eq!(first_argument("f(Point, g(|"), None);
    }

    #[test]
    fn literal_text_is_not_code() {
        assert!(string("\"ab|c\""));
        assert!(string("\"|\""));
        assert!(!string("|\"abc\""));
        assert!(!string("\"abc\"|"));
        assert!(string("s = \"abc|"));
        assert!(string("s = \"abc\\\"|"));
        assert!(!string("s = \"abc\n|"));
        assert!(string("c = '|"));
        assert!(!string("c = 'a'|"));
    }

    #[test]
    fn f_string_holes_are_code() {
        assert!(string("$\"|a {x} b\""));
        assert!(string("$\"a| {x} b\""));
        assert!(string("$\"a |{x} b\""));
        assert!(!string("$\"a {|x} b\""));
        assert!(!string("$\"a {x|} b\""));
        assert!(string("$\"a {x}| b\""));
        assert!(string("$\"{x}|{y}\""));
        assert!(string("$\"{x}|\""));
        assert!(!string("$\"{x}\"|"));
        assert!(!string("$\"{f(\"a\")|}\""));
        assert!(string("$\"{$\"in|ner\"}\""));
        assert!(string("$\"open|"));
        assert!(string("$\"open {x}|"));
        assert!(string("$\"|"));
        assert!(!string("$\"open\n|"));
    }

    #[test]
    fn comments_run_to_the_end_of_their_line() {
        assert!(comment("x // a|b"));
        assert!(comment("x //|"));
        assert!(comment("x // ab|\ny"));
        assert!(!comment("x |// ab"));
        assert!(!comment("x // ab\n|y"));
        assert!(!comment("s = \"//\" |"));
        assert!(!string("x // \"a|"));
    }

    #[test]
    fn the_name_being_typed_and_what_precedes_it() {
        let found = |start, after| Some(Completing { start, after });
        assert_eq!(completing("a.b|"), found(2, After::Dot));
        assert_eq!(completing("a.|"), found(2, After::Dot));
        assert_eq!(completing("a. |"), found(3, After::Dot));
        assert_eq!(completing("a.\n|"), found(3, After::Other));
        assert_eq!(completing("x = @i_a|"), found(4, After::At));
        assert_eq!(completing("x = @|"), found(4, After::At));
        assert_eq!(completing("x = fo|o"), found(4, After::Other));
        assert_eq!(completing("x = ma|"), found(4, After::Other));
        assert_eq!(completing("x = match|"), found(4, After::Other));
        assert_eq!(completing("x = |"), found(4, After::Other));
        assert_eq!(completing("\"a.|"), None);
        assert_eq!(completing("// a.|"), None);
        assert_eq!(completing("f(\"a//b\") point.|"), found(16, After::Dot));
    }

    #[test]
    fn identifiers_exclude_keywords() {
        assert!(is_identifier("total_2"));
        assert!(is_identifier("_x"));
        assert!(!is_identifier("2x"));
        assert!(!is_identifier("match"));
        assert!(!is_identifier("a.b"));
        assert!(!is_identifier(" a"));
        assert!(!is_identifier("a // b"));
        assert!(!is_identifier(""));
    }
}
