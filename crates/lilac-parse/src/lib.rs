//! Parser: `Vec<Token>` -> untyped [`Program`] with error recovery.
//!
//! Recursive descent over [`Lexer`] output. On an item-level failure the parser
//! emits one `E001` diagnostic, skips to the next item boundary, and keeps
//! going, so a single broken declaration never hides the rest of the file.
//! Every fallible node carries a [`Span`]; `-x` desugars to `@negate(x)` per
//! `SPEC.md` §8 at parse time.

mod expr;
mod items;
mod stmt;
mod types;

use lilac_ast::{Expr, Item, ItemKind, Program, Type};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_lex::{Token, TokenKind};
use lilac_span::{FileId, Span};

use crate::stmt::starts_stmt;
use crate::types::error_type;

/// How deeply expressions, statements and types may nest; past it is `E001`.
///
/// Every pass recurses over the tree, so this bounds the stack a compile
/// needs (`lilac_driver::COMPILE_STACK_BYTES`). Each link of a chain built
/// left to right nests one level too: an operator of `a + b + c`, an `as`, a
/// `.b` or call of `a.b().c`, and a prefix `-` or `ref`.
pub const MAX_NESTING: usize = 256;

/// Recursive-descent parser over an owned token buffer.
pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    file: FileId,
    eof: Token,
    had_errors: bool,
    /// Count of recovered construct failures. Blocks snapshot it to tell whether
    /// anything inside them recovered, so an unterminated-block diagnostic is
    /// not emitted on top of an already-reported failure.
    recoveries: usize,
    /// Whether `Ident { ... }` parses as a struct literal (Rust-style
    /// brace restriction: `if`/`loop` heads and `match` scrutinees parse
    /// with this off, so a trailing `{` opens a block; parentheses and
    /// other complete subexpressions reset it on).
    allow_struct_lit: bool,
    /// How deep the node being parsed nests (see [`MAX_NESTING`]).
    depth: usize,
    /// Whether this item already reported passing [`MAX_NESTING`]; one
    /// `E001` covers every place it nests too deeply.
    too_deep_reported: bool,
    /// The header (`name: kind`) of the item that just failed after
    /// reading it: the name, and whether it declared a function. The
    /// recovered [`ItemKind::Error`] keeps both.
    failed_header: Option<(String, bool)>,
}

/// A chain being built left to right in a loop (`a + b + c`, `a.b().c`,
/// `- -x`): each link nests the tree one level deeper ([`Parser::link`]).
struct Chain {
    /// The depth before the chain, restored after it.
    base: usize,
    /// Whether the chain passed [`MAX_NESTING`] and is now one `Expr::Error`.
    collapsed: bool,
}

impl Parser {
    /// Creates a parser; `file` must be the id the tokens were lexed with.
    #[must_use]
    pub fn new(tokens: Vec<Token>, file: FileId) -> Self {
        Self {
            tokens,
            pos: 0,
            file,
            eof: Token::new(
                TokenKind::Eof,
                "",
                u32::MAX as usize,
                u32::MAX as usize,
                file,
            ),
            had_errors: false,
            recoveries: 0,
            allow_struct_lit: true,
            depth: 0,
            too_deep_reported: false,
            failed_header: None,
        }
    }

    /// Records a recovered construct failure (statement, expression, or type);
    /// sets the failure signal and bumps the recovery counter.
    fn recover(&mut self) {
        self.had_errors = true;
        self.recoveries = self.recoveries.saturating_add(1);
    }

    /// Whether any construct failed to parse (the "a construct failed" signal).
    /// Distinct from [`lilac_error::Diags::has_errors`]: a lex error may parse
    /// cleanly and must not flip this flag.
    #[must_use]
    pub fn had_errors(&self) -> bool {
        self.had_errors
    }

    /// Parses a whole file, recovering past broken items. Always returns `Ok`;
    /// a failed item becomes an [`ItemKind::Error`] and [`Parser::had_errors`]
    /// records that a construct failed.
    pub fn parse_program(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Program, ErrorGuaranteed> {
        let mut items = Vec::new();
        while self.peek().kind != TokenKind::Eof {
            let before = self.pos;
            self.too_deep_reported = false;
            if let Ok(item) = self.parse_item(diags, false) {
                items.push(item);
            } else {
                self.recover();
                self.synchronize();
                if self.pos == before {
                    self.pos += 1;
                }
                items.push(self.error_item(before));
            }
        }
        let span = items
            .first()
            .zip(items.last())
            .map_or_else(Span::dummy, |(first, last)| {
                join_spans(first.span, last.span)
            });
        Ok(Program { items, span })
    }

    /// An [`ItemKind::Error`] item spanning the tokens consumed for a failed
    /// item, from `start` through the current position.
    fn error_item(&mut self, start: usize) -> Item {
        let (name, function) = self.failed_header.take().unwrap_or_default();
        Item {
            doc: None,
            name,
            kind: ItemKind::Error { function },
            span: self.error_span(start),
        }
    }

    /// The span from token `start` through the last consumed token, used as an
    /// error node's span.
    fn error_span(&self, start: usize) -> Span {
        let first = self.tokens.get(start).map(|token| token.span);
        let last = self
            .pos
            .checked_sub(1)
            .and_then(|end| self.tokens.get(end))
            .map(|token| token.span);
        match (first, last) {
            (Some(first), Some(last)) => join_spans(first, last),
            _ => Span::dummy(),
        }
    }

    /// The empty-name poison [`Type`]: the sentinel for a type that failed to
    /// parse (a user type name is never empty). Records the failure.
    fn poison_type(&mut self, span: Span) -> Type {
        self.recover();
        error_type(span)
    }

    /// Enters one more level of nesting, or returns `false` when that would
    /// pass [`MAX_NESTING`], reporting `E001` at `span` (once per item).
    fn descend(&mut self, diags: &mut lilac_error::Diags, span: Span) -> bool {
        if self.depth >= MAX_NESTING {
            self.recover();
            if std::mem::replace(&mut self.too_deep_reported, true) {
                return false;
            }
            diags.emit(
                Diagnostic::error(
                    codes::E001,
                    format!("nested too deeply; the limit is {MAX_NESTING} levels"),
                )
                .label(span, true, String::new())
                .help("move part of it into a variable or a function"),
            );
            return false;
        }
        self.depth += 1;
        true
    }

    /// Parses one more level of nesting with `parse`. Past [`MAX_NESTING`] it
    /// reports `E001`, skips the construct with `skip` and returns `poison`
    /// over the skipped tokens instead, so the tree stays shallow and the
    /// rest of the file still parses.
    fn nested<T>(
        &mut self,
        diags: &mut lilac_error::Diags,
        skip: fn(&mut Self),
        poison: fn(Span) -> T,
        parse: impl FnOnce(&mut Self, &mut lilac_error::Diags) -> Result<T, ErrorGuaranteed>,
    ) -> Result<T, ErrorGuaranteed> {
        let start = self.pos;
        let span = self.peek().span;
        if !self.descend(diags, span) {
            skip(self);
            let span = if self.pos == start {
                span
            } else {
                self.error_span(start)
            };
            return Ok(poison(span));
        }
        let result = parse(self, diags);
        self.depth -= 1;
        result
    }

    /// Runs `parse`, which builds a chain one [`Parser::link`] at a time, and
    /// restores the depth after it.
    fn chain<T>(
        &mut self,
        diags: &mut lilac_error::Diags,
        parse: impl FnOnce(&mut Self, &mut lilac_error::Diags, &mut Chain) -> T,
    ) -> T {
        let mut chain = Chain {
            base: self.depth,
            collapsed: false,
        };
        let result = parse(self, diags, &mut chain);
        self.depth = chain.base;
        result
    }

    /// Takes `expr`, the chain's newest link, one level deeper. Past
    /// [`MAX_NESTING`] the chain collapses into one `Expr::Error`, reported
    /// once at `at`, and later links fold into it without nesting.
    fn link(
        &mut self,
        diags: &mut lilac_error::Diags,
        chain: &mut Chain,
        at: Span,
        expr: Expr,
    ) -> Expr {
        if !chain.collapsed {
            if self.descend(diags, at) {
                return expr;
            }
            chain.collapsed = true;
            self.depth = chain.base;
        }
        Expr::Error { span: expr.span() }
    }

    /// Skips a bracketed group (`(…)`, `[…]` or `{…}`) whole, when the next
    /// token opens one.
    fn skip_group(&mut self) {
        let mut open = 0usize;
        loop {
            match self.peek().kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => open += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace if open > 0 => {
                    open -= 1;
                }
                TokenKind::Eof => break,
                _ if open > 0 => {}
                _ => break,
            }
            self.pos += 1;
            if open == 0 {
                break;
            }
        }
    }

    /// Skips an expression whole: up to a `,` or closing bracket at bracket
    /// depth zero, a later line, or a `{` that opens the block after an
    /// `if`, `loop` or `match` head.
    fn skip_expr(&mut self) {
        self.skip_to_end(!self.allow_struct_lit);
    }

    /// Skips a type whole, like [`Parser::skip_expr`]; a type never holds a
    /// `{`, so one always ends it.
    fn skip_type(&mut self) {
        self.skip_to_end(true);
    }

    /// Skips to a `,` or closing bracket at bracket depth zero, the first
    /// token of a later line, or (when `brace_ends`) a `{` at depth zero.
    fn skip_to_end(&mut self, brace_ends: bool) {
        let start = self.pos;
        let mut open = 0usize;
        loop {
            let token = self.peek();
            if open == 0 {
                let ends = match token.kind {
                    TokenKind::Eof
                    | TokenKind::Comma
                    | TokenKind::RParen
                    | TokenKind::RBracket
                    | TokenKind::RBrace => true,
                    TokenKind::LBrace => brace_ends,
                    _ => self.pos > start && token.newline_before,
                };
                if ends {
                    break;
                }
            }
            match token.kind {
                TokenKind::Eof => break,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => open += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => open -= 1,
                _ => {}
            }
            self.pos += 1;
        }
    }

    /// Skips a statement whole: up to the first token of a later line at
    /// bracket depth zero (except an `else`, which continues it), or the
    /// enclosing block's `}`.
    fn skip_stmt(&mut self) {
        let start = self.pos;
        let mut open = 0usize;
        loop {
            let token = self.peek();
            if open == 0
                && self.pos > start
                && token.newline_before
                && token.kind != TokenKind::KwElse
            {
                break;
            }
            match token.kind {
                TokenKind::Eof => break,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => open += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    if open == 0 {
                        break;
                    }
                    open -= 1;
                }
                _ => {}
            }
            self.pos += 1;
        }
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&self.eof)
    }

    fn peek_at(&self, ahead: usize) -> &Token {
        self.tokens.get(self.pos + ahead).unwrap_or(&self.eof)
    }

    fn bump(&mut self) -> Token {
        let token = self.peek().clone();
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    fn err(diags: &mut lilac_error::Diags, span: Span, msg: &str) -> ErrorGuaranteed {
        diags.emit(Diagnostic::error(codes::E001, msg).label(span, true, String::new()))
    }

    /// Runs `f` with struct literals allowed or disallowed, restoring the
    /// previous setting after (mirrors rustc's `with_res` for restrictions).
    fn with_struct_lit<T>(&mut self, allowed: bool, f: impl FnOnce(&mut Self) -> T) -> T {
        let previous = std::mem::replace(&mut self.allow_struct_lit, allowed);
        let result = f(self);
        self.allow_struct_lit = previous;
        result
    }

    fn expect(
        &mut self,
        diags: &mut lilac_error::Diags,
        kind: TokenKind,
        what: &str,
    ) -> Result<Token, ErrorGuaranteed> {
        if self.peek().kind == kind {
            Ok(self.bump())
        } else {
            Err(Self::expected(diags, self.peek(), what))
        }
    }

    /// An `open` delimiter the file ends inside: reported at `open`, with the
    /// end of file as a secondary label.
    fn unclosed(
        diags: &mut lilac_error::Diags,
        open: &Token,
        what: &str,
        eof: &Token,
    ) -> ErrorGuaranteed {
        let close = if open.kind == TokenKind::LParen {
            ")"
        } else {
            "}"
        };
        diags.emit(
            Diagnostic::error(
                codes::E001,
                format!("unterminated {what}; add a closing `{close}`"),
            )
            .label(
                open.span,
                true,
                format!("this `{}` is never closed", open.text),
            )
            .label(eof.span, false, "the file ends here"),
        )
    }

    /// Whether a name follows the `.` on its line (a tag's, a member's or
    /// a path segment's), so a `.` ending a line does not take the next
    /// line's first name.
    fn name_follows(&self) -> bool {
        let next = self.peek();
        next.kind == TokenKind::Ident && !next.newline_before
    }

    /// A `.` at `dot` with no name after it (mid-edit): "expected {what},
    /// found {token}", at the `.`, so the error lies inside what holds the
    /// name. The caller goes on with an empty name.
    fn nameless(&mut self, diags: &mut lilac_error::Diags, dot: Span, what: &str) {
        let msg = format!("expected {what}, found {}", self.peek().describe());
        Self::err(diags, dot, &msg);
        self.recover();
    }

    /// "expected {what}, found {token}", at `found`.
    fn expected(diags: &mut lilac_error::Diags, found: &Token, what: &str) -> ErrorGuaranteed {
        let msg = format!("expected {what}, found {}", found.describe());
        Self::err(diags, found.span, &msg)
    }

    fn synchronize(&mut self) {
        // Leaves a depth-0 `}` unconsumed so the enclosing block/object parser
        // sees its own terminator; the caller guarantees progress instead.
        let mut depth = 0;
        while self.peek().kind != TokenKind::Eof {
            match self.peek().kind {
                TokenKind::LBrace => {
                    depth += 1;
                    self.pos += 1;
                }
                TokenKind::RBrace => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.pos += 1;
                }
                _ => {
                    if depth == 0 && self.peek().starts_item() {
                        break;
                    }
                    self.pos += 1;
                }
            }
        }
    }

    /// Skips to the next statement boundary inside a block: the closing `}` or
    /// a token that can start a statement. Leaves the boundary unconsumed.
    fn synchronize_stmt(&mut self) {
        let mut depth = 0;
        while self.peek().kind != TokenKind::Eof {
            match self.peek().kind {
                TokenKind::LBrace => {
                    depth += 1;
                    self.pos += 1;
                }
                TokenKind::RBrace => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.pos += 1;
                }
                _ => {
                    if depth == 0 && starts_stmt(self.peek().kind) {
                        break;
                    }
                    self.pos += 1;
                }
            }
        }
    }

    /// Skips to the next `,` or `)` at bracket depth zero (leaving it), for
    /// recovering a failed call argument.
    fn synchronize_args(&mut self) {
        let mut depth = 0usize;
        while self.peek().kind != TokenKind::Eof {
            match self.peek().kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                    depth += 1;
                    self.pos += 1;
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.pos += 1;
                }
                TokenKind::Comma if depth == 0 => break,
                _ => self.pos += 1,
            }
        }
    }

    fn prev_span(&self) -> Span {
        self.tokens
            .get(self.pos.saturating_sub(1))
            .map_or_else(Span::dummy, |token| token.span)
    }
}

impl Default for Parser {
    fn default() -> Self {
        Self::new(Vec::new(), FileId(u32::MAX))
    }
}

/// Joins two spans from the same file into their hull.
fn join_spans(first: Span, last: Span) -> Span {
    if first.is_dummy() {
        return last;
    }
    if last.is_dummy() {
        return first;
    }
    Span {
        lo: first.lo.min(last.lo),
        hi: first.hi.max(last.hi),
        file: first.file,
    }
}

#[cfg(test)]
mod tests;
