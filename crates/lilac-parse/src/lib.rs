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
    failed: bool,
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
            failed: false,
            allow_struct_lit: true,
        }
    }

    /// Parses a whole file, recovering past broken items.
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
                self.failed = true;
                self.synchronize();
                if self.pos == before {
                    self.pos += 1;
                }
            }
        }
        let span = items
            .first()
            .zip(items.last())
            .map_or_else(Span::dummy, |(first, last)| {
                join_spans(first.span, last.span)
            });
        if self.failed {
            return Err(ErrorGuaranteed::new());
        }
        Ok(Program { items, span })
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
            let variants = self.parse_enum_variants(diags)?;
            Ok(Item {
                span: join_spans(name_span, self.prev_span()),
                name,
                kind: ItemKind::Enum {
                    variants,
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
            kind: ItemKind::Using { path },
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
                self.failed = true;
                self.synchronize();
                if self.pos == before {
                    self.pos += 1;
                }
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
                self.failed = true;
                self.synchronize();
                if self.pos == before {
                    self.pos += 1;
                }
            }
        }
        self.bump();
        Ok(members)
    }

    fn parse_enum_variants(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Vec<EnumVariant>, ErrorGuaranteed> {
        self.expect(diags, TokenKind::LBrace, "`{` to open enum body")?;
        let mut variants = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "unterminated enum body; add a closing `}`",
                ));
            }
            let ident = self.expect(diags, TokenKind::Ident, "enum variant name")?;
            let payload = if self.peek().kind == TokenKind::Colon {
                self.bump();
                Some(self.parse_type(diags)?)
            } else {
                None
            };
            let end = payload.as_ref().map_or(ident.span, |ty| ty.span);
            variants.push(EnumVariant {
                name: ident.text,
                payload,
                span: join_spans(ident.span, end),
            });
            if self.peek().kind == TokenKind::Comma {
                self.bump();
            }
        }
        self.bump();
        Ok(variants)
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
        let body = match self.peek().kind {
            TokenKind::LBrace => {
                if is_extern {
                    let span = self.peek().span;
                    return Err(Self::err(diags, span, "`extern` declarations have no body"));
                }
                Some(FuncBody::Block(self.parse_block(diags)?))
            }
            TokenKind::FatArrow => {
                if is_extern {
                    let span = self.peek().span;
                    return Err(Self::err(diags, span, "`extern` declarations have no body"));
                }
                self.bump();
                Some(FuncBody::Inline(self.parse_expr(diags)?))
            }
            _ => {
                if is_extern {
                    None
                } else {
                    let span = self.peek().span;
                    return Err(Self::err(
                        diags,
                        span,
                        "expected `{` or `=>` for function body",
                    ));
                }
            }
        };
        Ok(FuncDecl {
            generic_params,
            params,
            returns,
            body,
            is_extern,
        })
    }

    fn parse_param(&mut self, diags: &mut lilac_error::Diags) -> Result<Param, ErrorGuaranteed> {
        if self.peek().kind == TokenKind::At
            || Self::is_anonymous_param(self.peek(), self.peek_at(1))
        {
            let ty = self.parse_type(diags)?;
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
        let ty = self.parse_type(diags)?;
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

    fn parse_type(&mut self, diags: &mut lilac_error::Diags) -> Result<Type, ErrorGuaranteed> {
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
            return self.parse_func_type(diags, start, false);
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
                    return self.parse_func_type(diags, join_spans(start, keyword.span), true);
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
                generic_args.push(self.parse_type(diags)?);
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
                let span = join_spans(token.span, ident.span);
                Ok(Token::new(
                    TokenKind::Ident,
                    &format!("@{}", ident.text),
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
        let mut stmts = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "unterminated block; add a closing `}`",
                ));
            }
            stmts.push(self.parse_stmt(diags)?);
        }
        let close = self.bump();
        Ok(Block {
            stmts,
            span: join_spans(open.span, close.span),
        })
    }

    fn parse_stmt(&mut self, diags: &mut lilac_error::Diags) -> Result<Stmt, ErrorGuaranteed> {
        match self.peek().kind {
            TokenKind::KwReturn => {
                let keyword = self.bump();
                if starts_expr(self.peek().kind) {
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
            TokenKind::KwIf => self.parse_if_stmt(diags),
            TokenKind::KwLoop => self.parse_loop_stmt(diags),
            TokenKind::KwMatch => self.parse_match_stmt(diags),
            _ => self.parse_let_or_expr_stmt(diags),
        }
    }

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
            Some(self.parse_block(diags)?)
        } else {
            None
        };
        let end = els.as_ref().map_or(then.span, |block| block.span);
        Ok(Stmt::If {
            cond,
            capture,
            then,
            els,
            span: join_spans(keyword.span, end),
        })
    }

    fn parse_loop_stmt(&mut self, diags: &mut lilac_error::Diags) -> Result<Stmt, ErrorGuaranteed> {
        let keyword = self.bump();
        let cond = if self.peek().kind == TokenKind::KwIf {
            self.bump();
            Some(self.with_struct_lit(false, |p| p.parse_or(diags))?)
        } else {
            None
        };
        if self.peek().kind != TokenKind::LBrace {
            let span = self.peek().span;
            return Err(Self::err(diags, span, "expected `{` for loop body"));
        }
        let body = self.parse_block(diags)?;
        let els = if self.peek().kind == TokenKind::KwElse {
            self.bump();
            Some(self.parse_block(diags)?)
        } else {
            None
        };
        let end = els.as_ref().map_or(body.span, |block| block.span);
        Ok(Stmt::Loop {
            cond,
            body,
            els,
            span: join_spans(keyword.span, end),
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
                (MatchPattern::Tag(tag.text), dot.span)
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
                    "expected `.Tag` or a literal pattern for match arm",
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
            let ty = self.parse_type(diags)?;
            let mut bindings = vec![LetBinding {
                name: name.text,
                span: join_spans(name.span, ty.span),
                ty,
            }];
            while self.peek().kind == TokenKind::Comma {
                self.bump();
                let next = self.expect(diags, TokenKind::Ident, "binding name")?;
                self.expect(diags, TokenKind::Colon, "`:` after binding name")?;
                let next_ty = self.parse_type(diags)?;
                bindings.push(LetBinding {
                    name: next.text,
                    span: join_spans(next.span, next_ty.span),
                    ty: next_ty,
                });
            }
            if self.peek().kind == TokenKind::LBrace {
                let span = self.peek().span;
                return Err(Self::err(
                    diags,
                    span,
                    "expected `=` after variable type; use `= Type { ... }` to construct a struct value",
                ));
            }
            self.expect(diags, TokenKind::Eq, "`=` after variable type")?;
            let value = self.parse_expr(diags)?;
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
        let target = self.parse_expr(diags)?;
        if self.peek().kind == TokenKind::Eq {
            self.bump();
            let value = self.parse_expr(diags)?;
            let span = join_spans(target.span(), value.span());
            return Ok(Stmt::Assign {
                target,
                value,
                span,
            });
        }
        Ok(Stmt::ExprStmt(target))
    }

    fn parse_expr(&mut self, diags: &mut lilac_error::Diags) -> Result<Expr, ErrorGuaranteed> {
        self.parse_or(diags)
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
        let mut expr = self.parse_unary(diags)?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Mod,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_unary(diags)?;
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
                TokenKind::Dot => {
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
                    // Struct suffix on a path (`Type { ... }`); a bare `{`
                    // after anything else opens a block (handled by the
                    // caller). Like rustc, case never decides: the brace
                    // restriction flag does.
                    let Expr::Ident(name, span) = &expr else {
                        break;
                    };
                    if self.allow_struct_lit {
                        expr = self.parse_struct_lit(diags, name.clone(), *span)?;
                    } else if self.peek_at(1).kind == TokenKind::Ident
                        && self.peek_at(2).kind == TokenKind::Eq
                    {
                        return Err(Self::err(
                            diags,
                            join_spans(*span, self.peek().span),
                            "struct literal not allowed here; wrap it in parentheses",
                        ));
                    } else {
                        break;
                    }
                }
                TokenKind::LBracket => {
                    self.bump();
                    let mut generic_args = Vec::new();
                    loop {
                        generic_args.push(self.parse_type(diags)?);
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
                    if self.peek().kind != TokenKind::LParen {
                        let span = self.peek().span;
                        return Err(Self::err(
                            diags,
                            span,
                            "expected `(` after generic arguments",
                        ));
                    }
                    expr = self.finish_call(diags, expr, generic_args)?;
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
            args.push(self.with_struct_lit(true, |p| p.parse_expr(diags))?);
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
        }
        let close = self.bump();
        Ok(Expr::Call {
            span: join_spans(callee_span, close.span),
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
            TokenKind::StringLit => Ok(Expr::Str(token.text, token.span)),
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

    fn parse_struct_lit(
        &mut self,
        diags: &mut lilac_error::Diags,
        ty_name: String,
        ty_span: Span,
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
        Ok(Expr::StructLit {
            ty_name,
            fields,
            span: join_spans(ty_span, close.span),
        })
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
                "block `if` is a statement; expression `if` needs value branches like `if c 0x0 else 0x1`",
            ));
        }
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
                    parts.push(FStrPart::Lit(chunk.text));
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

/// Whether a token can start an expression (used for `return` validation).
fn starts_expr(kind: TokenKind) -> bool {
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
            | TokenKind::Dot
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
    fn colon_struct_literal_rejected() {
        let src =
            "main: function () nothing {\n    variable: Object {\n        member = 0x1\n    }\n}\n";
        let (_, diags, _) = parse(src);
        assert!(diags.has_errors());
        let messages: Vec<_> = diags.diagnostics().iter().map(|d| d.msg.clone()).collect();
        assert!(
            diags.diagnostics().iter().any(|d| d.code == "E001"),
            "expected E001, got {messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("use `= Type")),
            "expected `=`-form help, got {messages:?}"
        );
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
        let Expr::StructLit {
            ty_name, fields, ..
        } = value
        else {
            panic!("expected struct literal, got {value:?}");
        };
        assert_eq!(ty_name, "Object");
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
            matches!(cond, Expr::StructLit { ref ty_name, .. } if ty_name == "Foo"),
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
            matches!(value, Expr::StructLit { ref ty_name, .. } if ty_name == "foo"),
            "got {value:?}"
        );
    }

    #[test]
    fn struct_like_in_head_is_targeted_e001() {
        // `if Foo { member = ... }` meant a struct literal: say so and
        // suggest parentheses instead of failing downstream.
        let (_, diags) =
            first_stmt("main: function () nothing {\n    if Foo { member = 0x1 } {\n    }\n}\n");
        assert!(diags.has_errors());
        assert!(
            diags.diagnostics().iter().any(|d| d.code == "E001"),
            "{:?}",
            diags.diagnostics()
        );
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
}
