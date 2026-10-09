//! Parser: `Vec<Token>` -> untyped [`Program`] with error recovery.
//!
//! Recursive descent over [`Lexer`] output. On an item-level failure the parser
//! emits one `E001` diagnostic, skips to the next item boundary, and keeps
//! going, so a single broken declaration never hides the rest of the file.
//! Every fallible node carries a [`Span`]; `-x` desugars to `@negate(x)` per
//! `SPEC.md` §8 at parse time.

use lilac_ast::{
    BinOp, Block, EnumVariant, Expr, FStrPart, FuncBody, FuncDecl, FuncType, Item, ItemKind,
    LetBinding, MatchArm, MatchArmBody, MatchPattern, Param, Program, Stmt, StructField, Type,
};
use lilac_error::{Diagnostic, ErrorGuaranteed};
use lilac_lex::{Token, TokenKind};
use lilac_span::{FileId, Span};

/// Parser diagnostics always carry this stable code.
pub const PARSE_ERROR_CODE: &str = "E001";

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
    fn error_item(&self, start: usize) -> Item {
        Item {
            name: String::new(),
            kind: ItemKind::Error,
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
        Type {
            name: String::new(),
            name_span: span,
            is_ref: false,
            generic_args: Vec::new(),
            func: None,
            span,
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
        diags.emit(Diagnostic::error(PARSE_ERROR_CODE, msg).label(span, true, String::new()))
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
            let span = self.peek().span;
            Err(Self::err(diags, span, &format!("expected {what}")))
        }
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

    fn parse_item(
        &mut self,
        diags: &mut lilac_error::Diags,
        in_object: bool,
    ) -> Result<Item, ErrorGuaranteed> {
        if self.peek().kind == TokenKind::KwUsing {
            return self.parse_using(diags);
        }
        if self.peek().kind == TokenKind::KwPrivate {
            return self.parse_private_block(diags, in_object);
        }
        let (name, name_span) = self.parse_item_name(diags)?;
        let generic_params = self.parse_generic_params(diags)?;
        self.expect(diags, TokenKind::Colon, "`:` after declaration name")?;
        let kind_token = self.bump();
        let kind = match kind_token.kind {
            TokenKind::KwFunction => {
                ItemKind::Func(self.parse_func_rest(diags, false, generic_params)?)
            }
            TokenKind::KwExtern => {
                self.expect(diags, TokenKind::KwFunction, "`function` after `extern`")?;
                ItemKind::Func(self.parse_func_rest(diags, true, generic_params)?)
            }
            TokenKind::KwObject => {
                let members = self.parse_object_members(diags)?;
                return Ok(Item {
                    span: join_spans(name_span, self.prev_span()),
                    name,
                    kind: ItemKind::Object {
                        members,
                        generic_params,
                    },
                });
            }
            _ => {
                return self.parse_item_tail(
                    diags,
                    name,
                    name_span,
                    generic_params,
                    kind_token,
                    in_object,
                )
            }
        };
        Ok(Item {
            span: join_spans(name_span, self.prev_span()),
            name,
            kind,
        })
    }

    fn parse_item_tail(
        &mut self,
        diags: &mut lilac_error::Diags,
        name: String,
        name_span: Span,
        generic_params: Vec<String>,
        kind_token: Token,
        in_object: bool,
    ) -> Result<Item, ErrorGuaranteed> {
        if kind_token.kind == TokenKind::KwEnum {
            let (variants, members) = self.parse_enum_body(diags)?;
            Ok(Item {
                span: join_spans(name_span, self.prev_span()),
                name,
                kind: ItemKind::Enum {
                    variants,
                    members,
                    generic_params,
                },
            })
        } else {
            let ty = self.parse_type_starting(diags, kind_token)?;
            if self.peek().kind == TokenKind::FatArrow {
                self.bump();
                let value = self.parse_expr(diags)?;
                return Self::const_item(diags, name, name_span, &generic_params, ty, value, true);
            }
            if self.peek().kind == TokenKind::Eq {
                self.bump();
                let value = self.parse_expr(diags)?;
                return Self::const_item(diags, name, name_span, &generic_params, ty, value, false);
            }
            if !in_object && matches!(self.peek().kind, TokenKind::LBrace | TokenKind::LParen) {
                let value = self.parse_decl_shorthand(diags, &ty)?;
                return Self::const_item(diags, name, name_span, &generic_params, ty, value, false);
            }
            if !in_object {
                return Err(Self::err(
                    diags,
                    self.peek().span,
                    "expected `=` or `=>` after constant type",
                ));
            }
            if !generic_params.is_empty() {
                return Err(Self::err(
                    diags,
                    name_span,
                    "fields take no generic parameters",
                ));
            }
            Ok(Item {
                span: join_spans(name_span, ty.span),
                name,
                kind: ItemKind::Field { ty },
            })
        }
    }

    fn const_item(
        diags: &mut lilac_error::Diags,
        name: String,
        name_span: Span,
        generic_params: &[String],
        ty: Type,
        value: Expr,
        is_inline: bool,
    ) -> Result<Item, ErrorGuaranteed> {
        if !generic_params.is_empty() {
            return Err(Self::err(
                diags,
                name_span,
                "only `object`, `function`, and `enum` declarations take generic parameters",
            ));
        }
        Ok(Item {
            span: join_spans(name_span, value.span()),
            name,
            kind: ItemKind::Const {
                ty,
                value,
                is_inline,
            },
        })
    }

    fn parse_item_name(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<(String, Span), ErrorGuaranteed> {
        if self.peek().kind == TokenKind::At {
            let at = self.bump();
            let ident = self.expect(diags, TokenKind::Ident, "hook name after `@`")?;
            let name = format!("@{}", ident.text);
            return Ok((name, join_spans(at.span, ident.span)));
        }
        let ident = self.expect(diags, TokenKind::Ident, "declaration name")?;
        let span = ident.span;
        Ok((ident.text, span))
    }

    fn parse_generic_params(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Vec<String>, ErrorGuaranteed> {
        if self.peek().kind != TokenKind::LBracket {
            return Ok(Vec::new());
        }
        self.bump();
        let mut params = Vec::new();
        loop {
            let ident = self.expect(diags, TokenKind::Ident, "generic parameter name")?;
            params.push(ident.text);
            match self.peek().kind {
                TokenKind::Comma => {
                    self.bump();
                }
                TokenKind::RBracket => {
                    self.bump();
                    break;
                }
                _ => {
                    let span = self.peek().span;
                    return Err(Self::err(
                        diags,
                        span,
                        "expected `,` or `]` in generic parameter list",
                    ));
                }
            }
        }
        Ok(params)
    }

    fn parse_using(&mut self, diags: &mut lilac_error::Diags) -> Result<Item, ErrorGuaranteed> {
        let using = self.bump();
        let relative = self.peek().kind == TokenKind::Star;
        if relative {
            self.bump();
            self.expect(diags, TokenKind::Dot, "`.` after `*` in a relative `using`")?;
        }
        let first = self.expect(diags, TokenKind::Ident, "folder name after `using`")?;
        let mut path = first.text;
        let mut end = first.span;
        while self.peek().kind == TokenKind::Dot {
            self.bump();
            let segment = self.expect(diags, TokenKind::Ident, "folder name after `.`")?;
            path.push('.');
            path.push_str(&segment.text);
            end = segment.span;
        }
        Ok(Item {
            span: join_spans(using.span, end),
            name: path.clone(),
            kind: ItemKind::Using { path, relative },
        })
    }

    fn parse_private_block(
        &mut self,
        diags: &mut lilac_error::Diags,
        in_object: bool,
    ) -> Result<Item, ErrorGuaranteed> {
        let keyword = self.bump();
        self.expect(diags, TokenKind::LBrace, "`{` to open `private` body")?;
        let mut items = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "unterminated `private` block; add a closing `}`",
                ));
            }
            let before = self.pos;
            if let Ok(item) = self.parse_item(diags, in_object) {
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
        let close = self.bump();
        Ok(Item {
            span: join_spans(keyword.span, close.span),
            name: String::new(),
            kind: ItemKind::Private { items },
        })
    }

    fn parse_object_members(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Vec<Item>, ErrorGuaranteed> {
        self.expect(diags, TokenKind::LBrace, "`{` to open object body")?;
        let mut members = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "unterminated object body; add a closing `}`",
                ));
            }
            let before = self.pos;
            if let Ok(item) = self.parse_item(diags, true) {
                members.push(item);
            } else {
                self.recover();
                self.synchronize();
                if self.pos == before {
                    self.pos += 1;
                }
                members.push(self.error_item(before));
            }
        }
        self.bump();
        Ok(members)
    }

    /// Parses an enum body: variants (`One`, `Some: T`) interleaved with
    /// members. A member is a hook (`@name: ...`), a `private { ... }` block,
    /// a generic declaration (`name[T]: ...`), or a `function` with a body; any
    /// other `name: Type` is a payload variant.
    fn parse_enum_body(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<(Vec<EnumVariant>, Vec<Item>), ErrorGuaranteed> {
        self.expect(diags, TokenKind::LBrace, "`{` to open enum body")?;
        let mut variants = Vec::new();
        let mut members = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "unterminated enum body; add a closing `}`",
                ));
            }
            let is_member = matches!(self.peek().kind, TokenKind::At | TokenKind::KwPrivate)
                || (self.peek().kind == TokenKind::Ident
                    && self.peek_at(1).kind == TokenKind::LBracket);
            if is_member {
                let before = self.pos;
                if let Ok(item) = self.parse_item(diags, true) {
                    members.push(item);
                } else {
                    self.recover();
                    self.synchronize();
                    if self.pos == before {
                        self.pos += 1;
                    }
                    members.push(self.error_item(before));
                }
                continue;
            }
            let ident = self.expect(diags, TokenKind::Ident, "enum variant name")?;
            let payload = if self.peek().kind == TokenKind::Colon {
                self.bump();
                Some(self.parse_type(diags))
            } else {
                None
            };
            let name_span = ident.span;
            if let Some(Type {
                func: Some(func), ..
            }) = &payload
            {
                if matches!(self.peek().kind, TokenKind::LBrace | TokenKind::FatArrow) {
                    let func = func.clone();
                    let body = self.parse_func_body(diags, false)?;
                    members.push(Item {
                        name: ident.text,
                        kind: ItemKind::Func(FuncDecl {
                            generic_params: Vec::new(),
                            params: func.params,
                            returns: func.returns,
                            body,
                            is_extern: false,
                        }),
                        span: join_spans(name_span, self.prev_span()),
                    });
                    continue;
                }
            }
            let end = payload.as_ref().map_or(name_span, |ty| ty.span);
            variants.push(EnumVariant {
                name: ident.text,
                payload,
                span: join_spans(name_span, end),
                name_span,
            });
            if self.peek().kind == TokenKind::Comma {
                self.bump();
            }
        }
        self.bump();
        Ok((variants, members))
    }

    fn parse_func_rest(
        &mut self,
        diags: &mut lilac_error::Diags,
        is_extern: bool,
        generic_params: Vec<String>,
    ) -> Result<FuncDecl, ErrorGuaranteed> {
        self.expect(diags, TokenKind::LParen, "`(` to open parameter list")?;
        let mut params = Vec::new();
        while self.peek().kind != TokenKind::RParen {
            if self.peek().kind == TokenKind::Eof {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "unterminated parameter list; add a closing `)`",
                ));
            }
            params.push(self.parse_param(diags)?);
            if self.peek().kind == TokenKind::Comma {
                self.bump();
            } else if self.peek().kind != TokenKind::RParen {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "expected `,` or `)` in parameter list",
                ));
            }
        }
        self.bump();
        let returns = self.parse_returns(diags)?;
        let body = self.parse_func_body(diags, is_extern)?;
        Ok(FuncDecl {
            generic_params,
            params,
            returns,
            body,
            is_extern,
        })
    }

    /// Parses a function body (`{ ... }` or `=> expr`); `extern` declarations
    /// have none.
    fn parse_func_body(
        &mut self,
        diags: &mut lilac_error::Diags,
        is_extern: bool,
    ) -> Result<Option<FuncBody>, ErrorGuaranteed> {
        let has_body = matches!(self.peek().kind, TokenKind::LBrace | TokenKind::FatArrow);
        if is_extern {
            if has_body {
                let span = self.peek().span;
                return Err(Self::err(diags, span, "`extern` declarations have no body"));
            }
            return Ok(None);
        }
        match self.peek().kind {
            TokenKind::LBrace => Ok(Some(FuncBody::Block(self.parse_block(diags)?))),
            TokenKind::FatArrow => {
                self.bump();
                Ok(Some(FuncBody::Inline(self.parse_expr(diags)?)))
            }
            _ => {
                let span = self.peek().span;
                Err(Self::err(
                    diags,
                    span,
                    "expected `{` or `=>` for function body",
                ))
            }
        }
    }

    fn parse_param(&mut self, diags: &mut lilac_error::Diags) -> Result<Param, ErrorGuaranteed> {
        if self.peek().kind == TokenKind::At
            || Self::is_anonymous_param(self.peek(), self.peek_at(1))
        {
            let ty = self.parse_type(diags);
            let span = ty.span;
            return Ok(Param {
                name: String::new(),
                ty,
                span,
            });
        }
        let name = match self.peek().kind {
            TokenKind::Ident | TokenKind::KwSelf => self.bump(),
            _ => {
                let span = self.peek().span;
                return Err(Self::err(diags, span, "parameter name"));
            }
        };
        self.expect(diags, TokenKind::Colon, "`:` after parameter name")?;
        let ty = self.parse_type(diags);
        let span = join_spans(name.span, ty.span);
        Ok(Param {
            name: name.text,
            ty,
            span,
        })
    }

    /// Whether `first` starts an anonymous type (`@bit8`, `ref T`, `self`)
    /// rather than a `name:` slot, given the token after it.
    fn is_anonymous_param(first: &Token, second: &Token) -> bool {
        matches!(
            first.kind,
            TokenKind::Ident | TokenKind::KwRef | TokenKind::KwSelf | TokenKind::KwUnknown
        ) && second.kind != TokenKind::Colon
    }

    fn parse_returns(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Vec<Param>, ErrorGuaranteed> {
        match self.peek().kind {
            TokenKind::LBrace | TokenKind::FatArrow | TokenKind::Eof => Ok(Vec::new()),
            TokenKind::KwNothing => {
                self.bump();
                Ok(Vec::new())
            }
            _ => {
                let mut returns = Vec::new();
                loop {
                    returns.push(self.parse_param(diags)?);
                    if self.peek().kind == TokenKind::Comma {
                        self.bump();
                        continue;
                    }
                    break;
                }
                Ok(returns)
            }
        }
    }

    /// Parses a type, recovering a failure into the empty-name poison [`Type`]
    /// (recording [`Parser::had_errors`]) so the enclosing construct survives.
    fn parse_type(&mut self, diags: &mut lilac_error::Diags) -> Type {
        let start = self.pos;
        let start_span = self.peek().span;
        if let Ok(ty) = self.parse_type_inner(diags) {
            return ty;
        }
        let span = if self.pos == start {
            start_span
        } else {
            self.error_span(start)
        };
        self.poison_type(span)
    }

    /// Parses a type strictly; the caller decides whether to recover. Used for
    /// the recursive generic-argument positions so a failed inner type does not
    /// produce a cascade of follow-on diagnostics.
    fn parse_type_inner(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Type, ErrorGuaranteed> {
        if !starts_type(self.peek().kind) {
            let span = self.peek().span;
            return Err(Self::err(diags, span, "expected a type"));
        }
        let first = self.bump();
        self.parse_type_starting(diags, first)
    }

    fn parse_type_starting(
        &mut self,
        diags: &mut lilac_error::Diags,
        first: Token,
    ) -> Result<Type, ErrorGuaranteed> {
        let start = first.span;
        if first.kind == TokenKind::KwFunction {
            return self.parse_func_type(diags, start, start, false);
        }
        let (is_ref, name_token) = match first.kind {
            TokenKind::KwRef => {
                if self.peek().kind == TokenKind::KwRef {
                    let span = self.peek().span;
                    return Err(Self::err(
                        diags,
                        span,
                        "double references are invalid unless wrapped inside an object",
                    ));
                }
                if self.peek().kind == TokenKind::KwFunction {
                    let keyword = self.bump();
                    return self.parse_func_type(
                        diags,
                        join_spans(start, keyword.span),
                        keyword.span,
                        true,
                    );
                }
                (true, self.parse_type_name(diags)?)
            }
            TokenKind::At | TokenKind::Ident | TokenKind::KwUnknown | TokenKind::KwSelf => {
                (false, self.type_name_from(diags, first)?)
            }
            TokenKind::KwNothing => {
                return Err(Self::err(
                    diags,
                    first.span,
                    "`nothing` is not a type; it only marks an empty return slot",
                ));
            }
            _ => {
                return Err(Self::err(diags, first.span, "expected a type"));
            }
        };
        let mut end = name_token.span;
        let mut generic_args = Vec::new();
        if self.peek().kind == TokenKind::LBracket {
            self.bump();
            loop {
                generic_args.push(self.parse_type_inner(diags)?);
                match self.peek().kind {
                    TokenKind::Comma => {
                        self.bump();
                    }
                    TokenKind::RBracket => {
                        end = self.bump().span;
                        break;
                    }
                    _ => {
                        let span = self.peek().span;
                        return Err(Self::err(
                            diags,
                            span,
                            "expected `,` or `]` in generic argument list",
                        ));
                    }
                }
            }
        }
        Ok(Type {
            name: name_token.text,
            name_span: name_token.span,
            is_ref,
            generic_args,
            func: None,
            span: join_spans(start, end),
        })
    }

    /// Parses `function (params) returns` after the `function` keyword.
    fn parse_func_type(
        &mut self,
        diags: &mut lilac_error::Diags,
        start: Span,
        name_span: Span,
        is_ref: bool,
    ) -> Result<Type, ErrorGuaranteed> {
        self.expect(
            diags,
            TokenKind::LParen,
            "`(` to open function type parameters",
        )?;
        let mut params = Vec::new();
        while self.peek().kind != TokenKind::RParen {
            if self.peek().kind == TokenKind::Eof {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "unterminated function type; add a closing `)`",
                ));
            }
            params.push(self.parse_param(diags)?);
            if self.peek().kind == TokenKind::Comma {
                self.bump();
            } else if self.peek().kind != TokenKind::RParen {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "expected `,` or `)` in function type",
                ));
            }
        }
        self.bump();
        let returns = self.parse_returns(diags)?;
        let end = returns
            .last()
            .map_or_else(|| self.prev_span(), |param| param.span);
        Ok(Type {
            name: "function".to_owned(),
            name_span,
            is_ref,
            generic_args: Vec::new(),
            func: Some(FuncType { params, returns }),
            span: join_spans(start, end),
        })
    }

    fn parse_type_name(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Token, ErrorGuaranteed> {
        let token = self.bump();
        self.type_name_from(diags, token)
    }

    fn type_name_from(
        &mut self,
        diags: &mut lilac_error::Diags,
        token: Token,
    ) -> Result<Token, ErrorGuaranteed> {
        match token.kind {
            TokenKind::At => {
                let ident = self.expect(diags, TokenKind::Ident, "type name after `@`")?;
                let mut name = format!("@{}", ident.text);
                let mut span = join_spans(token.span, ident.span);
                // `@builtin.Os`: a type in the compiler's `@builtin` namespace.
                if name == "@builtin" {
                    while self.peek().kind == TokenKind::Dot
                        && self.peek_at(1).kind == TokenKind::Ident
                    {
                        self.bump();
                        let segment = self.bump();
                        name = format!("{name}.{}", segment.text);
                        span = join_spans(span, segment.span);
                    }
                }
                Ok(Token::new(
                    TokenKind::Ident,
                    &name,
                    span.lo.0 as usize,
                    span.hi.0 as usize,
                    self.file,
                ))
            }
            TokenKind::Ident | TokenKind::KwUnknown | TokenKind::KwSelf => Ok(token),
            _ => Err(Self::err(diags, token.span, "expected a type name")),
        }
    }

    fn parse_block(&mut self, diags: &mut lilac_error::Diags) -> Result<Block, ErrorGuaranteed> {
        let open = self.expect(diags, TokenKind::LBrace, "`{` to open block")?;
        let recoveries_at_entry = self.recoveries;
        let mut stmts = Vec::new();
        loop {
            if self.peek().kind == TokenKind::RBrace {
                break;
            }
            if self.peek().kind == TokenKind::Eof {
                // If anything inside this block recovered from a failure, the
                // error already covers the malformed tail (the failure may even
                // have consumed the closing `}`); do not cascade an
                // unterminated-block diagnostic on top of it. This covers a
                // recovered statement, a surviving statement with a poisoned
                // type, and a recovered `Expr::Error` alike.
                if self.recoveries > recoveries_at_entry {
                    break;
                }
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "unterminated block; add a closing `}`",
                ));
            }
            let before = self.pos;
            if let Ok(stmt) = self.parse_stmt(diags) {
                stmts.push(stmt);
            } else {
                self.recover();
                let span = self.error_span(before);
                if self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::Eof {
                    self.synchronize_stmt();
                    if self.pos == before {
                        self.pos += 1;
                    }
                }
                stmts.push(Stmt::Error { span });
            }
        }
        if self.peek().kind == TokenKind::RBrace {
            self.bump();
        }
        Ok(Block {
            stmts,
            span: join_spans(open.span, self.prev_span()),
        })
    }

    fn parse_stmt(&mut self, diags: &mut lilac_error::Diags) -> Result<Stmt, ErrorGuaranteed> {
        match self.peek().kind {
            TokenKind::KwReturn => {
                let keyword = self.bump();
                if !self.peek().newline_before && starts_expression(self.peek().kind) {
                    return Err(Self::err(
                        diags,
                        self.peek().span,
                        "`return` takes no arguments; assign named return slots instead",
                    ));
                }
                Ok(Stmt::Return { span: keyword.span })
            }
            TokenKind::KwBreak => {
                let keyword = self.bump();
                Ok(Stmt::Break { span: keyword.span })
            }
            TokenKind::KwContinue => {
                let keyword = self.bump();
                Ok(Stmt::Continue { span: keyword.span })
            }
            TokenKind::KwIf => self.parse_if_stmt(diags),
            TokenKind::KwLoop => self.parse_loop_stmt(diags),
            TokenKind::KwMatch => self.parse_match_stmt(diags),
            TokenKind::LBrace => Ok(Stmt::Block(self.parse_block(diags)?)),
            _ => self.parse_let_or_expr_stmt(diags),
        }
    }

    /// `if cond [-> capture] { ... } [else <stmt>]`.
    fn parse_if_stmt(&mut self, diags: &mut lilac_error::Diags) -> Result<Stmt, ErrorGuaranteed> {
        let keyword = self.bump();
        let cond = self.with_struct_lit(false, |p| p.parse_or(diags))?;
        let capture = if self.peek().kind == TokenKind::Arrow {
            self.bump();
            Some(self.parse_param(diags)?)
        } else {
            None
        };
        if self.peek().kind != TokenKind::LBrace {
            let span = self.peek().span;
            return Err(Self::err(diags, span, "expected `{` after `if` condition"));
        }
        let then = self.parse_block(diags)?;
        let els = if self.peek().kind == TokenKind::KwElse {
            self.bump();
            Some(Box::new(self.parse_stmt(diags)?))
        } else {
            None
        };
        let end = els.as_ref().map_or(then.span, |stmt| stmt.span());
        Ok(Stmt::If {
            cond,
            capture,
            then,
            els,
            span: join_spans(keyword.span, end),
        })
    }

    /// `loop <stmt>`: `loop { ... }`, `loop if c { ... } else break`, ...
    fn parse_loop_stmt(&mut self, diags: &mut lilac_error::Diags) -> Result<Stmt, ErrorGuaranteed> {
        let keyword = self.bump();
        let body = self.parse_stmt(diags)?;
        let span = join_spans(keyword.span, body.span());
        Ok(Stmt::Loop {
            body: Box::new(body),
            span,
        })
    }

    fn parse_match_stmt(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Stmt, ErrorGuaranteed> {
        let keyword = self.bump();
        let scrutinee = self.with_struct_lit(false, |p| p.parse_or(diags))?;
        self.expect(diags, TokenKind::LBrace, "`{` to open match arms")?;
        let mut arms = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "unterminated match; add a closing `}`",
                ));
            }
            arms.push(self.parse_match_arm(diags)?);
        }
        self.bump();
        let els = if self.peek().kind == TokenKind::KwElse {
            self.bump();
            Some(self.parse_block(diags)?)
        } else {
            None
        };
        let end = els
            .as_ref()
            .map_or_else(|| self.prev_span(), |block| block.span);
        Ok(Stmt::Match {
            scrutinee,
            arms,
            els,
            span: join_spans(keyword.span, end),
        })
    }

    fn parse_match_arm(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<MatchArm, ErrorGuaranteed> {
        let (pattern, start) = match self.peek().kind {
            TokenKind::Dot => {
                let dot = self.bump();
                let tag = self.expect(diags, TokenKind::Ident, "match tag after `.`")?;
                (
                    MatchPattern::Tag {
                        qualifier: None,
                        name: tag.text,
                    },
                    dot.span,
                )
            }
            TokenKind::Ident => {
                let qualifier = self.parse_type_inner(diags)?;
                self.expect(
                    diags,
                    TokenKind::Dot,
                    "`.` after the enum name in a pattern",
                )?;
                let tag = self.expect(diags, TokenKind::Ident, "match tag after `.`")?;
                let start = qualifier.span;
                (
                    MatchPattern::Tag {
                        qualifier: Some(qualifier),
                        name: tag.text,
                    },
                    start,
                )
            }
            TokenKind::Number | TokenKind::StringLit | TokenKind::CharLit => {
                let lit = self.bump();
                (MatchPattern::Const(lit.text), lit.span)
            }
            _ => {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "expected `.Tag`, `Enum.Tag`, or a literal pattern for match arm",
                ));
            }
        };
        let capture = if self.peek().kind == TokenKind::Arrow {
            self.bump();
            Some(self.parse_param(diags)?)
        } else {
            None
        };
        let (body, end) = if self.peek().kind == TokenKind::FatArrow {
            self.bump();
            if self.peek().kind == TokenKind::LBrace {
                let block = self.parse_block(diags)?;
                let end = block.span;
                (MatchArmBody::Block(block), end)
            } else {
                let stmt = self.parse_stmt(diags)?;
                let end = stmt.span();
                (MatchArmBody::Inline(Box::new(stmt)), end)
            }
        } else if self.peek().kind == TokenKind::LBrace {
            let block = self.parse_block(diags)?;
            let end = block.span;
            (MatchArmBody::Block(block), end)
        } else {
            let span = self.peek().span;
            return Err(Self::err(
                diags,
                span,
                "expected `=>` or `{` for match arm body",
            ));
        };
        Ok(MatchArm {
            pattern,
            capture,
            body,
            span: join_spans(start, end),
        })
    }

    fn parse_match_value(
        &mut self,
        diags: &mut lilac_error::Diags,
        keyword_span: Span,
    ) -> Result<Expr, ErrorGuaranteed> {
        let scrutinee = self.with_struct_lit(false, |p| p.parse_or(diags))?;
        self.expect(diags, TokenKind::LBrace, "`{` to open match arms")?;
        let mut arms = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "unterminated match; add a closing `}`",
                ));
            }
            arms.push(self.parse_match_arm(diags)?);
        }
        self.bump();
        self.expect(diags, TokenKind::KwElse, "`else` in `match` value")?;
        let els = self.parse_expr(diags)?;
        let span = join_spans(keyword_span, els.span());
        Ok(Expr::MatchValue {
            scrutinee: Box::new(scrutinee),
            arms,
            els: Box::new(els),
            span,
        })
    }

    fn parse_let_or_expr_stmt(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Stmt, ErrorGuaranteed> {
        if self.peek().kind == TokenKind::Ident && self.peek_at(1).kind == TokenKind::Colon {
            let name = self.bump();
            self.bump();
            let ty = self.parse_type(diags);
            let mut poisoned = ty.name.is_empty();
            let mut bindings = vec![LetBinding {
                name: name.text,
                span: join_spans(name.span, ty.span),
                ty,
            }];
            while self.peek().kind == TokenKind::Comma {
                if poisoned {
                    break;
                }
                self.bump();
                let next = self.expect(diags, TokenKind::Ident, "binding name")?;
                self.expect(diags, TokenKind::Colon, "`:` after binding name")?;
                let next_ty = self.parse_type(diags);
                poisoned = next_ty.name.is_empty();
                bindings.push(LetBinding {
                    name: next.text,
                    span: join_spans(next.span, next_ty.span),
                    ty: next_ty,
                });
            }
            if poisoned {
                // A poison type already reported the syntax error. Continue
                // only when a real `=` and a present initializer follow, so the
                // binding survives tolerantly; never run a follow-on
                // expectation that would cascade a second diagnostic at the
                // same span.
                if self.peek().kind == TokenKind::Eq && starts_expression(self.peek_at(1).kind) {
                    self.bump();
                    let value = self.parse_expr_recover(diags)?;
                    let span = join_spans(bindings[0].span, value.span());
                    if bindings.len() == 1 {
                        let only = bindings.remove(0);
                        return Ok(Stmt::Let {
                            name: only.name,
                            ty: only.ty,
                            value,
                            span,
                        });
                    }
                    return Ok(Stmt::MultiLet {
                        bindings,
                        value,
                        span,
                    });
                }
                return Err(ErrorGuaranteed::new());
            }
            let value = if bindings.len() == 1
                && matches!(self.peek().kind, TokenKind::LBrace | TokenKind::LParen)
            {
                let ty = bindings[0].ty.clone();
                self.parse_decl_shorthand(diags, &ty)?
            } else {
                self.expect(diags, TokenKind::Eq, "`=` after variable type")?;
                self.parse_expr_recover(diags)?
            };
            let span = join_spans(bindings[0].span, value.span());
            if bindings.len() == 1 {
                let only = bindings.remove(0);
                return Ok(Stmt::Let {
                    name: only.name,
                    ty: only.ty,
                    value,
                    span,
                });
            }
            return Ok(Stmt::MultiLet {
                bindings,
                value,
                span,
            });
        }
        let target = self.parse_expr_recover(diags)?;
        if self.peek().kind == TokenKind::Eq {
            self.bump();
            let value = self.parse_expr_recover(diags)?;
            let span = join_spans(target.span(), value.span());
            return Ok(Stmt::Assign {
                target,
                value,
                span,
            });
        }
        Ok(Stmt::ExprStmt(target))
    }

    /// The declaration shorthands after `name: Type`: `{ fields }` is
    /// `= Type { fields }` and `(args)` is `= Type(args)` (the `@init` call).
    fn parse_decl_shorthand(
        &mut self,
        diags: &mut lilac_error::Diags,
        ty: &Type,
    ) -> Result<Expr, ErrorGuaranteed> {
        if ty.is_ref || ty.func.is_some() || ty.name.is_empty() {
            let span = self.peek().span;
            return Err(Self::err(
                diags,
                span,
                "expected `=` after variable type; `Type { ... }` and `Type(...)` shorthands need a named type",
            ));
        }
        if self.peek().kind == TokenKind::LBrace {
            return self.parse_struct_lit(diags, ty.clone());
        }
        let callee = type_expr(ty);
        self.finish_call(diags, callee, Vec::new())
    }

    fn parse_expr(&mut self, diags: &mut lilac_error::Diags) -> Result<Expr, ErrorGuaranteed> {
        self.parse_or(diags)
    }

    /// Parses a statement/initializer expression with statement-level recovery.
    /// A *present-but-malformed* expression becomes an `Expr::Error` and the
    /// enclosing statement survives; a *missing* expression (the next token
    /// cannot start one) fails so the whole statement becomes `Stmt::Error`.
    fn parse_expr_recover(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Expr, ErrorGuaranteed> {
        if !starts_expression(self.peek().kind) {
            let span = self.peek().span;
            return Err(Self::err(diags, span, "expected expression"));
        }
        let before = self.pos;
        if let Ok(expr) = self.parse_expr(diags) {
            return Ok(expr);
        }
        self.recover();
        let span = if self.pos == before {
            self.peek().span
        } else {
            self.error_span(before)
        };
        Ok(Expr::Error { span })
    }

    fn parse_or(&mut self, diags: &mut lilac_error::Diags) -> Result<Expr, ErrorGuaranteed> {
        let mut expr = self.parse_and(diags)?;
        while self.peek().kind == TokenKind::KwOr {
            self.bump();
            let rhs = self.parse_and(diags)?;
            let span = join_spans(expr.span(), rhs.span());
            expr = Expr::BinOp {
                lhs: Box::new(expr),
                op: BinOp::Or,
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(expr)
    }

    fn parse_and(&mut self, diags: &mut lilac_error::Diags) -> Result<Expr, ErrorGuaranteed> {
        let mut expr = self.parse_equality(diags)?;
        while self.peek().kind == TokenKind::KwAnd {
            self.bump();
            let rhs = self.parse_equality(diags)?;
            let span = join_spans(expr.span(), rhs.span());
            expr = Expr::BinOp {
                lhs: Box::new(expr),
                op: BinOp::And,
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(expr)
    }

    fn parse_equality(&mut self, diags: &mut lilac_error::Diags) -> Result<Expr, ErrorGuaranteed> {
        let mut expr = self.parse_comparison(diags)?;
        while self.peek().kind == TokenKind::EqEq {
            self.bump();
            let rhs = self.parse_comparison(diags)?;
            let span = join_spans(expr.span(), rhs.span());
            expr = Expr::BinOp {
                lhs: Box::new(expr),
                op: BinOp::EqEq,
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(expr)
    }

    fn parse_comparison(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Expr, ErrorGuaranteed> {
        let mut expr = self.parse_additive(diags)?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::LtEq => BinOp::LtEq,
                TokenKind::GtEq => BinOp::GtEq,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_additive(diags)?;
            let span = join_spans(expr.span(), rhs.span());
            expr = Expr::BinOp {
                lhs: Box::new(expr),
                op,
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(expr)
    }

    fn parse_additive(&mut self, diags: &mut lilac_error::Diags) -> Result<Expr, ErrorGuaranteed> {
        let mut expr = self.parse_multiplicative(diags)?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_multiplicative(diags)?;
            let span = join_spans(expr.span(), rhs.span());
            expr = Expr::BinOp {
                lhs: Box::new(expr),
                op,
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(expr)
    }

    fn parse_multiplicative(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Expr, ErrorGuaranteed> {
        let mut expr = self.parse_cast(diags)?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Mod,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_cast(diags)?;
            let span = join_spans(expr.span(), rhs.span());
            expr = Expr::BinOp {
                lhs: Box::new(expr),
                op,
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(expr)
    }

    fn parse_cast(&mut self, diags: &mut lilac_error::Diags) -> Result<Expr, ErrorGuaranteed> {
        let mut expr = self.parse_unary(diags)?;
        while self.peek().kind == TokenKind::KwAs {
            self.bump();
            let ty = self.parse_type(diags);
            expr = Expr::Cast(Box::new(expr), ty);
        }
        Ok(expr)
    }

    fn parse_unary(&mut self, diags: &mut lilac_error::Diags) -> Result<Expr, ErrorGuaranteed> {
        if self.peek().kind == TokenKind::Minus {
            let minus = self.bump();
            let value = self.parse_unary(diags)?;
            let span = join_spans(minus.span, value.span());
            let callee = Expr::Ident("@negate".to_owned(), minus.span);
            return Ok(Expr::Call {
                callee: Box::new(callee),
                generic_args: Vec::new(),
                args: vec![value],
                span,
            });
        }
        self.parse_postfix(diags)
    }

    fn parse_postfix(&mut self, diags: &mut lilac_error::Diags) -> Result<Expr, ErrorGuaranteed> {
        let mut expr = self.parse_primary(diags)?;
        loop {
            match self.peek().kind {
                // A `.` on a new line starts the next match arm (`.Tag`), not
                // a member access.
                TokenKind::Dot if !self.peek().newline_before => {
                    self.bump();
                    let field = self.expect(diags, TokenKind::Ident, "field name after `.`")?;
                    let span = join_spans(expr.span(), field.span);
                    expr = Expr::Member {
                        obj: Box::new(expr),
                        field: field.text,
                        span,
                    };
                }
                TokenKind::LParen => {
                    expr = self.finish_call(diags, expr, Vec::new())?;
                }
                TokenKind::LBrace => {
                    // Struct suffix on a type (`Type { ... }`, `Name[T] { ... }`);
                    // a bare `{` after anything else, or anywhere struct
                    // literals are disabled (`if`/`loop`/`match` heads), ends
                    // the expression. Like rustc, case never decides.
                    if !self.allow_struct_lit {
                        break;
                    }
                    let ty = match &expr {
                        Expr::Ident(name, span) => named_type(name.clone(), *span, Vec::new()),
                        Expr::TypeApp(ty) => ty.clone(),
                        _ => break,
                    };
                    expr = self.parse_struct_lit(diags, ty)?;
                }
                TokenKind::LBracket => {
                    let open = self.bump();
                    let mut generic_args = Vec::new();
                    loop {
                        generic_args.push(self.parse_type_inner(diags)?);
                        match self.peek().kind {
                            TokenKind::Comma => {
                                self.bump();
                            }
                            TokenKind::RBracket => {
                                self.bump();
                                break;
                            }
                            _ => {
                                let span = self.peek().span;
                                return Err(Self::err(
                                    diags,
                                    span,
                                    "expected `,` or `]` in generic argument list",
                                ));
                            }
                        }
                    }
                    if self.peek().kind == TokenKind::LParen {
                        expr = self.finish_call(diags, expr, generic_args)?;
                        continue;
                    }
                    // `Name[args]` not followed by a call is a type in value
                    // position: a static receiver (`Box[T].make()`) or a struct
                    // literal head (`Box[T] { ... }`).
                    let Expr::Ident(name, name_span) = expr else {
                        return Err(Self::err(
                            diags,
                            open.span,
                            "expected `(` after generic arguments",
                        ));
                    };
                    let mut ty = named_type(name, name_span, generic_args);
                    ty.span = join_spans(name_span, self.prev_span());
                    expr = Expr::TypeApp(ty);
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn finish_call(
        &mut self,
        diags: &mut lilac_error::Diags,
        callee: Expr,
        generic_args: Vec<Type>,
    ) -> Result<Expr, ErrorGuaranteed> {
        let callee_span = callee.span();
        self.expect(diags, TokenKind::LParen, "`(` to open call arguments")?;
        let mut args = Vec::new();
        while self.peek().kind != TokenKind::RParen {
            if self.peek().kind == TokenKind::Eof {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "unterminated call; add a closing `)`",
                ));
            }
            let before = self.pos;
            if let Ok(arg) = self.with_struct_lit(true, |p| p.parse_expr(diags)) {
                args.push(arg);
                if self.peek().kind == TokenKind::Comma {
                    self.bump();
                } else if self.peek().kind != TokenKind::RParen {
                    let span = self.peek().span;
                    return Err(Self::err(
                        diags,
                        span,
                        "expected `,` or `)` in call arguments",
                    ));
                }
            } else {
                self.recover();
                let span = if self.pos == before {
                    self.peek().span
                } else {
                    self.error_span(before)
                };
                args.push(Expr::Error { span });
                self.synchronize_args();
                if self.pos == before {
                    self.pos += 1;
                }
                // The failed argument already reported the malformed tail;
                // continue only when a separator is actually present, never
                // emit a follow-on `,`/`)` diagnostic.
                if self.peek().kind == TokenKind::Comma {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        // Only consume an actual `)`. Recovery may have stopped at the
        // enclosing block's `}` (or a stray `]`): consuming it as the call's
        // close would corrupt every following item. Synthesize the close span
        // instead and leave the delimiter for the enclosing parser.
        let close_span = if self.peek().kind == TokenKind::RParen {
            self.bump().span
        } else {
            self.prev_span()
        };
        Ok(Expr::Call {
            span: join_spans(callee_span, close_span),
            callee: Box::new(callee),
            generic_args,
            args,
        })
    }

    fn parse_primary(&mut self, diags: &mut lilac_error::Diags) -> Result<Expr, ErrorGuaranteed> {
        let token = self.bump();
        match token.kind {
            TokenKind::Ident => Ok(Expr::Ident(token.text, token.span)),
            TokenKind::Number => Ok(Expr::Number(token.text, token.span)),
            TokenKind::StringLit => Ok(Expr::Str(token.bytes, token.span)),
            TokenKind::CharLit => Ok(Expr::Char(
                token.text.chars().next().unwrap_or('\u{FFFD}'),
                token.span,
            )),
            TokenKind::At => {
                let ident = self.expect(diags, TokenKind::Ident, "name after `@`")?;
                let span = join_spans(token.span, ident.span);
                Ok(Expr::Ident(format!("@{}", ident.text), span))
            }
            TokenKind::LParen => {
                let expr = self.with_struct_lit(true, |p| p.parse_expr(diags))?;
                self.expect(diags, TokenKind::RParen, "`)` to close group")?;
                Ok(expr)
            }
            TokenKind::KwIf => self.parse_if_value(diags, token.span),
            TokenKind::KwMatch => self.parse_match_value(diags, token.span),
            TokenKind::FStrStart => self.parse_fstring(diags, token.span),
            TokenKind::KwRef => {
                let value = self.parse_unary(diags)?;
                let span = join_spans(token.span, value.span());
                Ok(Expr::RefOf {
                    value: Box::new(value),
                    span,
                })
            }
            TokenKind::KwSelf => Ok(Expr::Ident("self".to_owned(), token.span)),
            TokenKind::Dot => {
                let tag = self.expect(diags, TokenKind::Ident, "tag name after `.`")?;
                let span = join_spans(token.span, tag.span);
                Ok(Expr::Tag(tag.text, span))
            }
            _ => Err(Self::err(diags, token.span, "expected expression")),
        }
    }

    /// Parses `{ field = value, ... }` after the literal's type.
    fn parse_struct_lit(
        &mut self,
        diags: &mut lilac_error::Diags,
        ty: Type,
    ) -> Result<Expr, ErrorGuaranteed> {
        self.bump();
        let mut fields = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "unterminated struct literal; add a closing `}`",
                ));
            }
            let name = self.expect(diags, TokenKind::Ident, "field name")?;
            self.expect(diags, TokenKind::Eq, "`=` after field name")?;
            let value = self.with_struct_lit(true, |p| p.parse_expr(diags))?;
            fields.push(StructField {
                name: name.text,
                span: join_spans(name.span, value.span()),
                value,
            });
            if self.peek().kind == TokenKind::Comma {
                self.bump();
            }
        }
        let close = self.bump();
        let span = join_spans(ty.span, close.span);
        Ok(Expr::StructLit { ty, fields, span })
    }

    fn parse_if_value(
        &mut self,
        diags: &mut lilac_error::Diags,
        keyword_span: Span,
    ) -> Result<Expr, ErrorGuaranteed> {
        let cond = self.with_struct_lit(false, |p| p.parse_or(diags))?;
        if self.peek().kind == TokenKind::LBrace {
            return Err(Self::err(
                diags,
                self.peek().span,
                "block `if` is a statement; an `if` value is written `if c then x else y`",
            ));
        }
        self.expect(diags, TokenKind::KwThen, "`then` in `if` value")?;
        let then = self.parse_expr(diags)?;
        self.expect(diags, TokenKind::KwElse, "`else` in `if` value")?;
        let els = self.parse_expr(diags)?;
        let span = join_spans(keyword_span, els.span());
        Ok(Expr::IfValue {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
            span,
        })
    }

    fn parse_fstring(
        &mut self,
        diags: &mut lilac_error::Diags,
        start_span: Span,
    ) -> Result<Expr, ErrorGuaranteed> {
        let mut parts = Vec::new();
        loop {
            match self.peek().kind {
                TokenKind::FStrMid => {
                    let chunk = self.bump();
                    parts.push(FStrPart::Lit(chunk.bytes));
                }
                TokenKind::LBrace => {
                    self.bump();
                    parts.push(FStrPart::Hole(
                        self.with_struct_lit(true, |p| p.parse_expr(diags))?,
                    ));
                    self.expect(diags, TokenKind::RBrace, "`}` to close f-string hole")?;
                }
                TokenKind::FStrEnd => {
                    let end = self.bump();
                    return Ok(Expr::FString {
                        parts,
                        span: join_spans(start_span, end.span),
                    });
                }
                TokenKind::Eof => {
                    let span = self.peek().span;
                    return Err(Self::err(diags, span, "unterminated f-string"));
                }
                _ => {
                    let span = self.peek().span;
                    return Err(Self::err(
                        diags,
                        span,
                        "expected f-string chunk, `{hole}`, or closing `\"`",
                    ));
                }
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

/// A plain named type (`Name` / `Name[args]`) built from an expression head.
fn named_type(name: String, name_span: Span, generic_args: Vec<Type>) -> Type {
    Type {
        name,
        name_span,
        is_ref: false,
        generic_args,
        func: None,
        span: name_span,
    }
}

/// A named type in value position: `Name` or `Name[args]` (for `Type(args)`).
fn type_expr(ty: &Type) -> Expr {
    if ty.generic_args.is_empty() {
        Expr::Ident(ty.name.clone(), ty.name_span)
    } else {
        Expr::TypeApp(ty.clone())
    }
}

/// Whether a token can start an expression (the union of the primary forms and
/// the unary minus), used to tell a *present-but-malformed* initializer from a
/// *missing* one.
fn starts_expression(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Ident
            | TokenKind::Number
            | TokenKind::StringLit
            | TokenKind::CharLit
            | TokenKind::LParen
            | TokenKind::Minus
            | TokenKind::FStrStart
            | TokenKind::At
            | TokenKind::KwRef
            | TokenKind::KwIf
            | TokenKind::KwMatch
            | TokenKind::KwSelf
            | TokenKind::Dot
    )
}

/// Whether a token can start a type (mirrors [`Parser::parse_type_starting`]'s
/// accepted first tokens; `nothing` is included so its targeted diagnostic is
/// preserved).
fn starts_type(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::KwFunction
            | TokenKind::KwRef
            | TokenKind::At
            | TokenKind::Ident
            | TokenKind::KwUnknown
            | TokenKind::KwSelf
            | TokenKind::KwNothing
    )
}

/// Whether a token can start a statement (a block-recovery boundary): every
/// token that can begin an expression-statement, plus the non-expression
/// statement keywords.
fn starts_stmt(kind: TokenKind) -> bool {
    starts_expression(kind)
        || matches!(
            kind,
            TokenKind::KwLoop | TokenKind::KwReturn | TokenKind::KwBreak | TokenKind::KwContinue
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_error::Diags;
    use lilac_lex::Lexer;
    use lilac_span::SourceMap;

    fn parse(src: &str) -> (Program, Diags, SourceMap) {
        let mut sm = SourceMap::new();
        let file = sm.add_file("test.lilac", src);
        let (tokens, lex_errors) = Lexer::lex_all(src, file);
        let mut diags = Diags::new();
        for error in lex_errors {
            diags.emit(error);
        }
        let mut parser = Parser::new(tokens, file);
        let program = parser.parse_program(&mut diags).unwrap_or(Program {
            items: Vec::new(),
            span: Span::dummy(),
        });
        (program, diags, sm)
    }

    #[test]
    fn hello_parses_cleanly() {
        let src = "main: function () nothing {\n    value: @bit32 = 0x1\n}\n";
        let (program, diags, _) = parse(src);
        assert!(!diags.has_errors());
        assert_eq!(program.items.len(), 1);
    }

    #[test]
    fn missing_value_is_e001() {
        let (_, diags, _) = parse("main: function () nothing {\n    value: @bit32 =\n}\n");
        assert!(diags.has_errors());
        assert!(diags.diagnostics().iter().any(|d| d.code == "E001"));
    }

    #[test]
    fn generics_and_calls_parse() {
        let src = "gen_function[T]: function (in: T) out: T {\n    out = in\n}\n\nmain: function () nothing {\n    out: @bit32 = gen_function[@bit32](0x1)\n}\n";
        let (program, diags, _) = parse(src);
        assert!(!diags.has_errors());
        assert_eq!(program.items.len(), 2);
    }

    #[test]
    fn recovery_reports_each_broken_item() {
        let src = "broken1: function ( nothing {}\nbroken2: object {\n";
        let (_, diags, _) = parse(src);
        assert!(diags.diagnostics().len() >= 2);
    }

    #[test]
    fn item_error_recovery() {
        let src = "ok: function () nothing {\n}\nbad: function (\n";
        let mut sm = SourceMap::new();
        let file = sm.add_file("test.lilac", src);
        let (tokens, lex_errors) = Lexer::lex_all(src, file);
        let mut diags = Diags::new();
        for error in lex_errors {
            diags.emit(error);
        }
        let mut parser = Parser::new(tokens, file);
        let program = parser
            .parse_program(&mut diags)
            .expect("parse_program must recover rather than return Err");
        assert_eq!(program.items.len(), 2, "one good item plus one error item");
        assert!(
            matches!(program.items[0].kind, ItemKind::Func(_)),
            "the good item survives: {:?}",
            program.items[0].kind
        );
        assert!(
            matches!(program.items[1].kind, ItemKind::Error),
            "the broken item recovers into `ItemKind::Error`: {:?}",
            program.items[1].kind
        );
        assert!(parser.had_errors(), "a failed construct is recorded");
        let parse_diag = diags
            .diagnostics()
            .iter()
            .find(|d| d.code == "E001")
            .expect("the broken item still reports E001");
        assert_eq!(
            parse_diag.msg, "unterminated parameter list; add a closing `)`",
            "same message as today"
        );
    }

    #[test]
    fn object_field_parses() {
        let src = "Object: object {\n    member: @bit8\n}\n";
        let (program, diags, _) = parse(src);
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        assert_eq!(program.items.len(), 1);
        let ItemKind::Object { members, .. } = &program.items[0].kind else {
            panic!("expected object, got {:?}", program.items[0].kind);
        };
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].name, "member");
        let ItemKind::Field { ty } = &members[0].kind else {
            panic!("expected field, got {:?}", members[0].kind);
        };
        assert_eq!(ty.name, "@bit8");
        assert!(!ty.is_ref);
    }

    fn sole_let_type(src: &str) -> (Type, SourceMap) {
        let (program, diags, sm) = parse(src);
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        let Stmt::Let { ty, .. } = &block.stmts[0] else {
            panic!("expected let, got {:?}", block.stmts[0]);
        };
        (ty.clone(), sm)
    }

    #[test]
    fn type_name_span() {
        let cases = [
            (
                "main: function () nothing {\n    x: @bit32 = 0x1\n}\n",
                "@bit32",
            ),
            (
                "main: function () nothing {\n    x: ref @bit32 = y\n}\n",
                "@bit32",
            ),
            (
                "main: function () nothing {\n    x: GenObj[@bit32] = y\n}\n",
                "GenObj",
            ),
        ];
        for (src, name) in cases {
            let (ty, sm) = sole_let_type(src);
            assert_eq!(ty.name, name, "type name in {src:?}");
            let lo = u32::try_from(src.find(name).expect("name present in source"))
                .expect("offset fits in u32");
            let hi = lo + u32::try_from(name.len()).expect("name length fits in u32");
            assert_eq!(ty.name_span.lo.0, lo, "lo for {name}");
            assert_eq!(ty.name_span.hi.0, hi, "hi for {name}");
            assert_eq!(sm.span_text(ty.name_span), Some(name), "text for {name}");
        }
    }

    #[test]
    fn tag_literal_parses() {
        let src = "main: function () nothing {\n    number: Number = .One\n}\n";
        let (program, diags, _) = parse(src);
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        assert_eq!(program.items.len(), 1);
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        let Stmt::Let { value, .. } = &block.stmts[0] else {
            panic!("expected let, got {:?}", block.stmts[0]);
        };
        let Expr::Tag(tag, _) = value else {
            panic!("expected tag literal, got {value:?}");
        };
        assert_eq!(tag, "One");
    }

    #[test]
    fn bare_top_level_decl_still_errors() {
        let (_, diags, _) = parse("x: @bit8\n");
        assert!(diags.has_errors());
        assert!(diags.diagnostics().iter().any(|d| d.code == "E001"));
    }

    #[test]
    fn and_or_parse_as_binop() {
        let src =
            "main: function () nothing {\n    x: @bit8 = 0x0 and 0x1\n    y: @bit8 = 0x1 or x\n}\n";
        let (program, diags, _) = parse(src);
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        let Stmt::Let {
            value: and_value, ..
        } = &block.stmts[0]
        else {
            panic!("expected let, got {:?}", block.stmts[0]);
        };
        let Expr::BinOp { op: and_op, .. } = and_value else {
            panic!("expected binop, got {and_value:?}");
        };
        assert_eq!(*and_op, BinOp::And);
        let Stmt::Let {
            value: or_value, ..
        } = &block.stmts[1]
        else {
            panic!("expected let, got {:?}", block.stmts[1]);
        };
        let Expr::BinOp { op: or_op, .. } = or_value else {
            panic!("expected binop, got {or_value:?}");
        };
        assert_eq!(*or_op, BinOp::Or);
    }

    #[test]
    fn match_byte_arms_parse() {
        let src = "main: function () nothing {\n    match 0x0 {\n        0x0 => {}\n        0x1 => {}\n    }\n}\n";
        let (program, diags, _) = parse(src);
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        let Stmt::Match { arms, .. } = &block.stmts[0] else {
            panic!("expected match, got {:?}", block.stmts[0]);
        };
        assert_eq!(arms.len(), 2);
        let MatchPattern::Const(first) = &arms[0].pattern else {
            panic!("expected const pattern, got {:?}", arms[0].pattern);
        };
        assert_eq!(first, "0x0");
    }

    #[test]
    fn multilet_parses() {
        let src =
            "main: function () nothing {\n    condition: @bit32, result: @bit32 = get_multi()\n}\n";
        let (program, diags, _) = parse(src);
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        let Stmt::MultiLet { bindings, .. } = &block.stmts[0] else {
            panic!("expected multilet, got {:?}", block.stmts[0]);
        };
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].name, "condition");
        assert_eq!(bindings[1].name, "result");
    }

    #[test]
    fn declaration_shorthands_desugar() {
        let stmts = main_stmts(
            "main: function () nothing {\n    a: Object {\n        member = 0x1\n    }\n    b: Box[@bit8] { value = 0x1 }\n    c: Object(0x1, 0x2)\n    d: Box[@bit8](0x1)\n}\n",
        );
        let Stmt::Let { value, .. } = &stmts[0] else {
            panic!("{:?}", stmts[0]);
        };
        assert!(
            matches!(value, Expr::StructLit { ty, fields, .. } if ty.name == "Object" && fields.len() == 1)
        );
        let Stmt::Let { value, .. } = &stmts[1] else {
            panic!("{:?}", stmts[1]);
        };
        assert!(matches!(value, Expr::StructLit { ty, .. } if ty.generic_args.len() == 1));
        let Stmt::Let { value, .. } = &stmts[2] else {
            panic!("{:?}", stmts[2]);
        };
        let Expr::Call { callee, args, .. } = value else {
            panic!("{value:?}");
        };
        assert!(matches!(callee.as_ref(), Expr::Ident(name, _) if name == "Object"));
        assert_eq!(args.len(), 2);
        let Stmt::Let { value, .. } = &stmts[3] else {
            panic!("{:?}", stmts[3]);
        };
        assert!(
            matches!(value, Expr::Call { callee, .. } if matches!(callee.as_ref(), Expr::TypeApp(ty) if ty.name == "Box"))
        );
    }

    #[test]
    fn generic_type_in_value_position() {
        let stmts = main_stmts(
            "main: function () nothing {\n    a: Box[@bit8] = Box[@bit8] { value = 0x1 }\n    b: Box[@bit8] = Box[@bit8].make(0x1)\n    c: ref @bit8 = CAllocator.create[@bit8]()\n}\n",
        );
        let Stmt::Let { value, .. } = &stmts[0] else {
            panic!("{:?}", stmts[0]);
        };
        assert!(
            matches!(value, Expr::StructLit { ty, .. } if ty.name == "Box" && ty.generic_args.len() == 1)
        );
        let Stmt::Let { value, .. } = &stmts[1] else {
            panic!("{:?}", stmts[1]);
        };
        let Expr::Call { callee, .. } = value else {
            panic!("{value:?}");
        };
        let Expr::Member { obj, field, .. } = callee.as_ref() else {
            panic!("{callee:?}");
        };
        assert_eq!(field, "make");
        assert!(matches!(obj.as_ref(), Expr::TypeApp(ty) if ty.name == "Box"));
        let Stmt::Let { value, .. } = &stmts[2] else {
            panic!("{:?}", stmts[2]);
        };
        assert!(matches!(value, Expr::Call { generic_args, .. } if generic_args.len() == 1));
    }

    #[test]
    fn continue_is_a_statement() {
        let stmts = main_stmts(
            "main: function () nothing {\n    loop if x { continue } else continue\n}\n",
        );
        let Stmt::Loop { body, .. } = &stmts[0] else {
            panic!("{:?}", stmts[0]);
        };
        let Stmt::If {
            then,
            els: Some(els),
            ..
        } = body.as_ref()
        else {
            panic!("{body:?}");
        };
        assert!(matches!(then.stmts[..], [Stmt::Continue { .. }]));
        assert!(matches!(els.as_ref(), Stmt::Continue { .. }));
    }

    #[test]
    fn builtin_type_names_take_dotted_segments() {
        let stmts = main_stmts(
            "main: function () nothing {\n    os: @builtin.Os = @builtin.Target.os\n}\n",
        );
        let Stmt::Let { ty, value, .. } = &stmts[0] else {
            panic!("{:?}", stmts[0]);
        };
        assert_eq!(ty.name, "@builtin.Os");
        assert!(matches!(value, Expr::Member { field, .. } if field == "os"));
    }

    #[test]
    fn loop_and_else_take_statements() {
        let stmts = main_stmts(
            "main: function () nothing {\n    loop if x { x = 0x0 } else break\n    if a { } else if b { } else { }\n    loop { }\n    { y: @bit8 = 0x1 }\n}\n",
        );
        let Stmt::Loop { body, .. } = &stmts[0] else {
            panic!("{:?}", stmts[0]);
        };
        let Stmt::If { els: Some(els), .. } = body.as_ref() else {
            panic!("{body:?}");
        };
        assert!(matches!(els.as_ref(), Stmt::Break { .. }));
        let Stmt::If { els: Some(els), .. } = &stmts[1] else {
            panic!("{:?}", stmts[1]);
        };
        assert!(matches!(els.as_ref(), Stmt::If { els: Some(_), .. }));
        assert!(
            matches!(&stmts[2], Stmt::Loop { body, .. } if matches!(body.as_ref(), Stmt::Block(_)))
        );
        assert!(matches!(&stmts[3], Stmt::Block(block) if block.stmts.len() == 1));
    }

    #[test]
    fn if_value_uses_then() {
        let stmts =
            main_stmts("main: function () nothing {\n    v: @bit8 = if c then -x else .A\n}\n");
        let Stmt::Let { value, .. } = &stmts[0] else {
            panic!("{:?}", stmts[0]);
        };
        let Expr::IfValue { then, els, .. } = value else {
            panic!("{value:?}");
        };
        assert!(matches!(then.as_ref(), Expr::Call { .. }));
        assert!(matches!(els.as_ref(), Expr::Tag(name, _) if name == "A"));
    }

    #[test]
    fn qualified_match_patterns() {
        let stmts = main_stmts(
            "main: function () nothing {\n    match v {\n        .A => return\n        Opt.Some -> x: @bit8 => return\n        Opt[@bit8].None => return\n    }\n}\n",
        );
        let Stmt::Match { arms, .. } = &stmts[0] else {
            panic!("{:?}", stmts[0]);
        };
        assert!(
            matches!(&arms[0].pattern, MatchPattern::Tag { qualifier: None, name } if name == "A")
        );
        assert!(
            matches!(&arms[1].pattern, MatchPattern::Tag { qualifier: Some(q), name } if q.name == "Opt" && name == "Some")
        );
        assert!(
            matches!(&arms[2].pattern, MatchPattern::Tag { qualifier: Some(q), .. } if q.generic_args.len() == 1)
        );
    }

    #[test]
    fn enum_members_parse() {
        let (program, diags, _) = parse(
            "Opt[T]: enum {\n    Some: T\n    None\n    Callback: function (x: @bit8) nothing\n    get: function (self) v: T {\n    }\n    @on_drop: function (self) nothing {}\n    map[U]: function (self) nothing {}\n    private {\n        helper: function () nothing => 0x0\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{:?}", diags.diagnostics());
        let ItemKind::Enum {
            variants, members, ..
        } = &program.items[0].kind
        else {
            panic!("{:?}", program.items[0].kind);
        };
        let names: Vec<_> = variants.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["Some", "None", "Callback"]);
        let names: Vec<_> = members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["get", "@on_drop", "map", ""]);
    }

    #[test]
    fn hex_escape_is_one_byte() {
        let stmts =
            main_stmts("main: function () nothing {\n    s: @byte_buffer = \"\\xFFa\"\n}\n");
        let Stmt::Let { value, .. } = &stmts[0] else {
            panic!("{:?}", stmts[0]);
        };
        assert!(matches!(value, Expr::Str(bytes, _) if bytes == &[0xFF, b'a']));
    }

    #[test]
    fn relative_using_parses() {
        let (program, diags, _) = parse("using *.Heap.Pool\n");
        assert!(!diags.has_errors(), "{:?}", diags.diagnostics());
        assert!(matches!(
            &program.items[0].kind,
            ItemKind::Using { path, relative: true } if path == "Heap.Pool"
        ));
    }

    #[test]
    fn structlit_equals_form_parses() {
        let src = "main: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n}\n";
        let (program, diags, _) = parse(src);
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        let Stmt::Let { value, .. } = &block.stmts[0] else {
            panic!("expected let, got {:?}", block.stmts[0]);
        };
        let Expr::StructLit { ty, fields, .. } = value else {
            panic!("expected struct literal, got {value:?}");
        };
        assert_eq!(ty.name, "Object");
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "member");
    }

    #[test]
    fn match_value_without_else_is_e001() {
        let src = "main: function () nothing {\n    variable: @bit32 = match 0x0 {\n        0x1 => 0x1\n    }\n}\n";
        let (_, diags, _) = parse(src);
        assert!(diags.has_errors());
        let messages: Vec<_> = diags.diagnostics().iter().map(|d| d.msg.clone()).collect();
        assert!(
            diags.diagnostics().iter().any(|d| d.code == "E001"),
            "expected E001, got {messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("else")),
            "expected else-mentioning message, got {messages:?}"
        );
    }

    #[test]
    fn if_value_without_else_is_e001() {
        let src = "main: function () nothing {\n    conditional: @bit32 = if 0x0 0x0\n}\n";
        let (_, diags, _) = parse(src);
        assert!(diags.has_errors());
        assert!(diags.diagnostics().iter().any(|d| d.code == "E001"));
    }

    #[test]
    fn func_type_position_parses() {
        let src = "main: function () nothing {\n    add: function (@bit32) @bit32 = addOne\n}\n";
        let (program, diags, _) = parse(src);
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        let Stmt::Let { ty, .. } = &block.stmts[0] else {
            panic!("expected let, got {:?}", block.stmts[0]);
        };
        let Some(func) = &ty.func else {
            panic!("expected function type, got {ty:?}");
        };
        assert_eq!(func.params.len(), 1);
        assert_eq!(func.params[0].name, "");
        assert_eq!(func.params[0].ty.name, "@bit32");
        assert_eq!(func.returns.len(), 1);
    }

    #[test]
    fn enum_payloads_parse() {
        let src = "OptionalBit32: enum {\n    Some: @bit32\n    None\n}\n";
        let (program, diags, _) = parse(src);
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let ItemKind::Enum { variants, .. } = &program.items[0].kind else {
            panic!("expected enum, got {:?}", program.items[0].kind);
        };
        assert_eq!(variants.len(), 2);
        assert!(variants[0].payload.is_some());
        assert!(variants[1].payload.is_none());
    }

    #[test]
    fn lowercase_ident_brace_is_not_structlit() {
        // `if x {` and `match number {` must not parse the trailing block
        // as a struct literal (struct literals name a `Type`).
        let src = "main: function () nothing {\n    x: @bit8 = 0x1\n    if x {\n    }\n}\n";
        let (program, diags, _) = parse(src);
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        assert!(
            matches!(block.stmts[1], Stmt::If { .. }),
            "expected if statement, got {:?}",
            block.stmts[1]
        );
    }

    fn first_stmt(src: &str) -> (Option<Stmt>, Diags) {
        let (program, diags, _) = parse(src);
        let stmt = program.items.iter().find_map(|item| match &item.kind {
            ItemKind::Func(decl) => decl.body.as_ref().and_then(|body| match body {
                FuncBody::Block(block) => block.stmts.first().cloned(),
                FuncBody::Inline(_) => None,
            }),
            _ => None,
        });
        (stmt, diags)
    }

    #[test]
    fn if_head_rejects_struct_literal() {
        // `if Foo {` is a condition plus a block, never a struct literal.
        let (stmt, diags) = first_stmt("main: function () nothing {\n    if Foo {\n    }\n}\n");
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let Some(Stmt::If { cond, .. }) = stmt else {
            panic!("expected if statement, got {stmt:?}");
        };
        assert!(
            matches!(&cond, Expr::Ident(name, _) if name == "Foo"),
            "got {cond:?}"
        );
    }

    #[test]
    fn parenthesized_struct_literal_in_head() {
        // Parentheses reset the restriction: a struct literal works in `if`.
        let (stmt, diags) =
            first_stmt("main: function () nothing {\n    if (Foo { member = 0x1 }) {\n    }\n}\n");
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let Some(Stmt::If { cond, .. }) = stmt else {
            panic!("expected if statement, got {stmt:?}");
        };
        assert!(
            matches!(cond, Expr::StructLit { ref ty, .. } if ty.name == "Foo"),
            "got {cond:?}"
        );
    }

    #[test]
    fn lowercase_struct_literal_in_let() {
        // Case never decides: any identifier opens a struct literal outside
        // head position (the checker rejects unknown type names later).
        let (stmt, diags) = first_stmt(
            "Object: object {\n    member: @bit8\n}\nmain: function () nothing {\n    v: Object = foo { member = 0x1 }\n}\n",
        );
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let Some(Stmt::Let { value, .. }) = stmt else {
            panic!("expected let statement, got {stmt:?}");
        };
        assert!(
            matches!(value, Expr::StructLit { ref ty, .. } if ty.name == "foo"),
            "got {value:?}"
        );
    }

    #[test]
    fn if_head_brace_always_opens_the_block() {
        // `if x { y = 0x1 }` is a condition and a block holding an assignment;
        // a struct literal in a head needs parentheses.
        let (stmt, diags) = first_stmt("main: function () nothing {\n    if x { y = 0x1 }\n}\n");
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let Some(Stmt::If { cond, then, .. }) = stmt else {
            panic!("expected if statement, got {stmt:?}");
        };
        assert!(matches!(&cond, Expr::Ident(name, _) if name == "x"));
        assert!(matches!(then.stmts[0], Stmt::Assign { .. }));
    }

    #[test]
    fn match_scrutinee_rejects_struct_literal() {
        // `match Foo {` inspects `Foo`; the `{` opens the arm block.
        let (stmt, diags) = first_stmt(
            "main: function () nothing {\n    match Foo {\n        .One {\n        }\n    }\n}\n",
        );
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let Some(Stmt::Match { scrutinee, .. }) = stmt else {
            panic!("expected match statement, got {stmt:?}");
        };
        assert!(
            matches!(&scrutinee, Expr::Ident(name, _) if name == "Foo"),
            "got {scrutinee:?}"
        );
    }

    #[test]
    fn private_block_parses_items() {
        let (program, diags, _) = parse("private {\n    helper: function () nothing {}\n}\n");
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let ItemKind::Private { items } = &program.items[0].kind else {
            panic!("expected private block, got {:?}", program.items[0].kind);
        };
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0].kind, ItemKind::Func(_)));
    }

    #[test]
    fn private_block_inside_object_parses() {
        let src = "Object: object {\n    private {\n        helper: function () nothing {}\n        secret: @bit8\n    }\n}\n";
        let (program, diags, _) = parse(src);
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let ItemKind::Object { members, .. } = &program.items[0].kind else {
            panic!("expected object, got {:?}", program.items[0].kind);
        };
        let ItemKind::Private { items } = &members[0].kind else {
            panic!("expected private block, got {:?}", members[0].kind);
        };
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn private_without_brace_is_e001() {
        let (_, diags, _) = parse("private helper: function () nothing {}\n");
        assert!(diags.has_errors());
        assert!(diags.diagnostics().iter().any(|d| d.code == "E001"));
    }

    #[test]
    fn union_keyword_is_gone() {
        // Hard removal: `union` lexes as an identifier, so a union
        // declaration fails with a generic parse error.
        let (_, diags) = first_stmt(
            "Maybe: union {\n    Some: @bit8\n    None\n}\nmain: function () nothing {\n}\n",
        );
        assert!(diags.has_errors());
        assert!(
            diags.diagnostics().iter().any(|d| d.code == "E001"),
            "{:?}",
            diags.diagnostics()
        );
    }

    /// The statements of `main`'s block body, asserting a clean parse.
    fn main_stmts(src: &str) -> Vec<Stmt> {
        let (program, diags, _) = parse(src);
        assert!(
            !diags.has_errors(),
            "unexpected errors: {:?}",
            diags.diagnostics()
        );
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        block.stmts.clone()
    }

    #[test]
    fn as_cast_parses_postfix_left_associative() {
        let src = "main: function () nothing {\n\
            a: @bit8 = x as T\n\
            b: @bit8 = o.f as T\n\
            c: @bit8 = p + q as T\n\
            d: @bit8 = x as A as B\n\
            e: ref @bit8 = x as ref @bit8\n\
            g: @bit8 = f(x) as T\n\
            h: @bit8 = -x as T\n\
        }\n";
        let stmts = main_stmts(src);

        // `x as T` -> `Cast(Ident(x), T)`.
        let Stmt::Let { value, .. } = &stmts[0] else {
            panic!("expected let, got {:?}", stmts[0]);
        };
        let Expr::Cast(inner, ty) = value else {
            panic!("expected cast, got {value:?}");
        };
        assert!(matches!(**inner, Expr::Ident(ref name, _) if name == "x"));
        assert_eq!(ty.name, "T");
        assert!(!ty.is_ref);

        // `o.f as T` -> `Cast(Member(o, f), T)`.
        let Stmt::Let { value, .. } = &stmts[1] else {
            panic!("expected let, got {:?}", stmts[1]);
        };
        let Expr::Cast(inner, ty) = value else {
            panic!("expected cast, got {value:?}");
        };
        let Expr::Member { obj, field, .. } = &**inner else {
            panic!("expected member, got {inner:?}");
        };
        assert!(matches!(**obj, Expr::Ident(ref name, _) if name == "o"));
        assert_eq!(field, "f");
        assert_eq!(ty.name, "T");

        // `p + q as T` -> `Add(p, Cast(q, T))`: `as` is postfix and binds
        // tighter than `+`.
        let Stmt::Let { value, .. } = &stmts[2] else {
            panic!("expected let, got {:?}", stmts[2]);
        };
        let Expr::BinOp { lhs, op, rhs, .. } = value else {
            panic!("expected binop, got {value:?}");
        };
        assert_eq!(*op, BinOp::Add);
        assert!(matches!(**lhs, Expr::Ident(ref name, _) if name == "p"));
        let Expr::Cast(inner, ty) = &**rhs else {
            panic!("expected cast on the right, got {rhs:?}");
        };
        assert!(matches!(**inner, Expr::Ident(ref name, _) if name == "q"));
        assert_eq!(ty.name, "T");

        // `x as A as B` -> `Cast(Cast(x, A), B)` (left-associative).
        let Stmt::Let { value, .. } = &stmts[3] else {
            panic!("expected let, got {:?}", stmts[3]);
        };
        let Expr::Cast(outer_inner, outer_ty) = value else {
            panic!("expected cast, got {value:?}");
        };
        assert_eq!(outer_ty.name, "B");
        let Expr::Cast(inner_inner, inner_ty) = &**outer_inner else {
            panic!("expected nested cast, got {outer_inner:?}");
        };
        assert_eq!(inner_ty.name, "A");
        assert!(matches!(**inner_inner, Expr::Ident(ref name, _) if name == "x"));

        // `x as ref @bit8` parses a `ref` target type.
        let Stmt::Let { value, .. } = &stmts[4] else {
            panic!("expected let, got {:?}", stmts[4]);
        };
        let Expr::Cast(inner, ty) = value else {
            panic!("expected cast, got {value:?}");
        };
        assert!(ty.is_ref);
        assert_eq!(ty.name, "@bit8");
        assert!(matches!(**inner, Expr::Ident(ref name, _) if name == "x"));

        // `f(x) as T` -> `Cast(Call(f, [x]), T)`.
        let Stmt::Let { value, .. } = &stmts[5] else {
            panic!("expected let, got {:?}", stmts[5]);
        };
        let Expr::Cast(inner, ty) = value else {
            panic!("expected cast, got {value:?}");
        };
        let Expr::Call { callee, args, .. } = &**inner else {
            panic!("expected call, got {inner:?}");
        };
        assert!(matches!(**callee, Expr::Ident(ref name, _) if name == "f"));
        assert_eq!(args.len(), 1);
        assert!(matches!(args[0], Expr::Ident(ref name, _) if name == "x"));
        assert_eq!(ty.name, "T");

        // `-x as T` -> `Cast(@negate(x), T)`: `as` is looser than unary minus,
        // so the cast applies to the negated value.
        let Stmt::Let { value, .. } = &stmts[6] else {
            panic!("expected let, got {:?}", stmts[6]);
        };
        let Expr::Cast(inner, ty) = value else {
            panic!("expected cast, got {value:?}");
        };
        assert_eq!(ty.name, "T");
        let Expr::Call { callee, args, .. } = &**inner else {
            panic!("expected negate call, got {inner:?}");
        };
        assert!(matches!(**callee, Expr::Ident(ref name, _) if name == "@negate"));
        assert_eq!(args.len(), 1);
        assert!(matches!(args[0], Expr::Ident(ref name, _) if name == "x"));
    }

    /// Parses `src` with [`Parser::parse_program`] directly, returning the
    /// program, the parser's failure signal, and the diagnostics.
    fn parse_with_status(src: &str) -> (Program, Diags, bool, SourceMap) {
        let mut sm = SourceMap::new();
        let file = sm.add_file("test.lilac", src);
        let (tokens, lex_errors) = Lexer::lex_all(src, file);
        let mut diags = Diags::new();
        for error in lex_errors {
            diags.emit(error);
        }
        let mut parser = Parser::new(tokens, file);
        let program = parser
            .parse_program(&mut diags)
            .expect("parse_program must recover rather than return Err");
        (program, diags, parser.had_errors(), sm)
    }

    #[test]
    fn stmt_error_recovery() {
        // A function body whose initializer is missing: the enclosing function
        // survives and the broken statement becomes `Stmt::Error`.
        let src = "main: function () nothing {\n    x: @bit8 = \n}\n";
        let (program, diags, had_errors, _) = parse_with_status(src);
        assert!(had_errors, "a failed statement records the failure");
        assert_eq!(program.items.len(), 1, "the function item survives");
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        assert!(
            block
                .stmts
                .iter()
                .any(|stmt| matches!(stmt, Stmt::Error { .. })),
            "the broken statement recovers into `Stmt::Error`: {:?}",
            block.stmts
        );
        assert!(
            diags.diagnostics().iter().any(|d| d.code == "E001"),
            "the syntax diagnostic is reported: {:?}",
            diags.diagnostics()
        );
    }

    #[test]
    fn broken_type_annotation_recovers_to_poison() {
        // A statement with no type annotation: the poison `Type` (empty name)
        // lets the statement survive.
        let src = "main: function () nothing {\n    x: = 0x1\n}\n";
        let (program, diags, had_errors, _) = parse_with_status(src);
        assert!(had_errors, "a failed type records the failure");
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        let Some(Stmt::Let { ty, .. }) = block.stmts.first() else {
            panic!("the statement survives, got {:?}", block.stmts);
        };
        assert_eq!(
            ty.name, "",
            "a failed type is the empty-name poison sentinel"
        );
        assert!(ty.generic_args.is_empty());
        assert!(ty.func.is_none());
        assert!(
            diags.diagnostics().iter().any(|d| d.code == "E001"),
            "the type diagnostic is reported: {:?}",
            diags.diagnostics()
        );
    }

    #[test]
    fn recovery_keeps_sibling_statements() {
        // A good statement before a broken one is preserved.
        let src = "main: function () nothing {\n    y: @bit8 = 0x1\n    x: @bit8 = \n}\n";
        let (program, _diags, had_errors, _) = parse_with_status(src);
        assert!(had_errors);
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        assert_eq!(block.stmts.len(), 2, "{:?}", block.stmts);
        assert!(
            matches!(&block.stmts[0], Stmt::Let { name, .. } if name == "y"),
            "the good sibling is parsed: {:?}",
            block.stmts[0]
        );
        assert!(
            matches!(block.stmts[1], Stmt::Error { .. }),
            "the broken statement is an error: {:?}",
            block.stmts[1]
        );
    }

    #[test]
    fn call_arg_error_recovers_to_expr_error() {
        // A malformed call argument becomes an `Expr::Error` and the statement
        // (and call) survives.
        let src = "main: function () nothing {\n    f(, 0x1)\n}\n";
        let (program, diags, had_errors, _) = parse_with_status(src);
        assert!(had_errors, "a failed expression records the failure");
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        let Stmt::ExprStmt(Expr::Call { args, .. }) = &block.stmts[0] else {
            panic!("the call survives, got {:?}", block.stmts[0]);
        };
        assert!(
            args.iter().any(|arg| matches!(arg, Expr::Error { .. })),
            "the bad argument is an `Expr::Error`: {args:?}"
        );
        assert!(diags.diagnostics().iter().any(|d| d.code == "E001"));
    }

    #[test]
    fn recovered_failure_before_missing_close_is_single_diagnostic() {
        // A recovered type failure whose statement *survives* must not be
        // followed by a spurious "unterminated block": strict mode sees exactly
        // one E001. Before the recovery, the failed statement failed the whole
        // item, so there was a single diagnostic.
        let src = "main: function () nothing {\n    x: = 0x1\n";
        let (program, diags, had_errors, _) = parse_with_status(src);
        assert!(had_errors, "the type failure is recorded");
        assert!(
            matches!(
                program.items.first().map(|item| &item.kind),
                Some(ItemKind::Func(_))
            ),
            "the function survives: {:?}",
            program.items
        );
        let errors: Vec<_> = diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E001")
            .collect();
        assert_eq!(errors.len(), 1, "exactly one E001: {errors:?}");
    }

    #[test]
    fn malformed_initializer_recovers_to_expr_error() {
        // A *present-but-malformed* initializer survives as `Expr::Error`
        // (vs. a *missing* one, which stays `Stmt::Error`).
        let src = "main: function () nothing {\n    x: @bit8 = .\n}\n";
        let (program, _diags, had_errors, _) = parse_with_status(src);
        assert!(had_errors, "the expression failure is recorded");
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        let Some(Stmt::Let { value, .. }) = block.stmts.first() else {
            panic!("the statement survives, got {:?}", block.stmts);
        };
        assert!(
            matches!(value, Expr::Error { .. }),
            "the malformed initializer is an `Expr::Error`: {value:?}"
        );
    }

    /// The E001 diagnostics in `diags`, for exact-count assertions.
    fn e001(diags: &Diags) -> Vec<&lilac_error::Diagnostic> {
        diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E001")
            .collect()
    }

    #[test]
    fn malformed_call_keeps_following_item() {
        // A call argument failure must not consume the enclosing block's `}`
        // as the call's closing paren: exactly one E001 and the following item
        // survives (Critical 1).
        let src = "main: function () nothing { f(ref) }\nfoo: function () nothing {\n}\n";
        let (program, diags, had_errors, _) = parse_with_status(src);
        assert!(had_errors, "the malformed call records the failure");
        assert_eq!(
            program.items.len(),
            2,
            "the item survives: {:?}",
            program.items
        );
        assert!(
            matches!(&program.items[1].kind, ItemKind::Func(_)),
            "the following item is intact: {:?}",
            program.items[1].kind
        );
        let errors = e001(&diags);
        assert_eq!(errors.len(), 1, "exactly one E001: {errors:?}");
    }

    #[test]
    fn poison_type_before_non_eq_is_single_diagnostic() {
        // `x: ]` leaves `]` where the `=` is expected. The recovered type must
        // not cascade a second "expected `=`" diagnostic at the same span
        // (Critical 2).
        let src = "main: function () nothing {\n    x: ]\n}\n";
        let (_, diags, had_errors, _) = parse_with_status(src);
        assert!(had_errors, "the type failure is recorded");
        let errors = e001(&diags);
        assert_eq!(errors.len(), 1, "exactly one E001: {errors:?}");
    }

    #[test]
    fn broken_statement_keeps_parenthesized_sibling() {
        // A broken statement must not skip past a following `(`-led statement:
        // the sibling survives and strict mode sees a single diagnostic
        // (Important 3).
        let src = "main: function () nothing {\n    x: ]\n    (y)\n}\n";
        let (program, diags, had_errors, _) = parse_with_status(src);
        assert!(had_errors, "the broken statement records the failure");
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function, got {:?}", program.items[0].kind);
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body, got {:?}", decl.body);
        };
        assert!(
            matches!(block.stmts.last(), Some(Stmt::ExprStmt(_))),
            "the `(y)` sibling is an expression statement: {:?}",
            block.stmts
        );
        let errors = e001(&diags);
        assert_eq!(errors.len(), 1, "exactly one E001: {errors:?}");
    }
}
