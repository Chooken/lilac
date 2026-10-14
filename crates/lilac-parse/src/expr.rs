//! Expressions: operators by precedence, postfix chains and primaries.

use lilac_ast::{BinOp, Expr, FStrPart, StructField, Type, TypeShape};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_lex::{Token, TokenKind};
use lilac_span::Span;

use crate::types::named_type;
use crate::{join_spans, Chain, Parser};

impl Parser {
    pub(crate) fn parse_expr(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Expr, ErrorGuaranteed> {
        self.nested(
            diags,
            Self::skip_expr,
            |span| Expr::Error { span },
            |p, diags| p.parse_binary(diags, 0),
        )
    }

    /// Parses a statement/initializer expression with statement-level recovery.
    /// A *present-but-malformed* expression becomes an `Expr::Error` and the
    /// enclosing statement survives; a *missing* expression (the next token
    /// cannot start one) fails so the whole statement becomes `Stmt::Error`.
    pub(crate) fn parse_expr_recover(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Expr, ErrorGuaranteed> {
        if !starts_expression(self.peek().kind) {
            return Err(Self::expected(diags, self.peek(), "expression"));
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

    /// Parses binary operators and `as` by precedence climbing: operands
    /// bind to operators of at least `min_power` ([`binary_power`]).
    pub(crate) fn parse_binary(
        &mut self,
        diags: &mut lilac_error::Diags,
        min_power: u8,
    ) -> Result<Expr, ErrorGuaranteed> {
        self.chain(diags, |p, diags, chain| {
            let mut expr = p.parse_unary(diags)?;
            while let Some(power) = binary_power(p.peek().kind) {
                if power < min_power {
                    break;
                }
                let op = p.bump();
                let wrapped = if let Some(bin) = binary_op(op.kind) {
                    let rhs = p.parse_binary(diags, power + 1)?;
                    Expr::BinOp {
                        span: join_spans(expr.span(), rhs.span()),
                        lhs: Box::new(expr),
                        op: bin,
                        rhs: Box::new(rhs),
                    }
                } else {
                    let ty = p.parse_type(diags);
                    Expr::Cast(Box::new(expr), Box::new(ty))
                };
                expr = p.link(diags, chain, op.span, wrapped);
            }
            Ok(expr)
        })
    }

    /// Prefix operators: `-x` (`@negate(x)`) and `ref x`.
    pub(crate) fn parse_unary(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Expr, ErrorGuaranteed> {
        self.chain(diags, |p, diags, chain| {
            let mut prefixes = Vec::new();
            while matches!(
                p.peek().kind,
                TokenKind::Minus | TokenKind::KwRef | TokenKind::KwNot
            ) {
                prefixes.push(p.bump());
            }
            let mut expr = p.parse_postfix(diags)?;
            for prefix in prefixes.into_iter().rev() {
                let span = join_spans(prefix.span, expr.span());
                let wrapped = match prefix.kind {
                    TokenKind::Minus => Expr::Call {
                        callee: Box::new(Expr::Ident("@negate".to_owned(), prefix.span)),
                        generic_args: Vec::new(),
                        args: vec![expr],
                        span,
                    },
                    TokenKind::KwNot => Expr::Not {
                        value: Box::new(expr),
                        span,
                    },
                    _ => Expr::RefOf {
                        value: Box::new(expr),
                        span,
                    },
                };
                expr = p.link(diags, chain, prefix.span, wrapped);
            }
            Ok(expr)
        })
    }

    pub(crate) fn parse_postfix(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Expr, ErrorGuaranteed> {
        self.chain(diags, Self::parse_postfix_chain)
    }

    pub(crate) fn parse_postfix_chain(
        &mut self,
        diags: &mut lilac_error::Diags,
        chain: &mut Chain,
    ) -> Result<Expr, ErrorGuaranteed> {
        let mut expr = self.parse_primary(diags)?;
        loop {
            match self.peek().kind {
                // A `.` on a new line starts the next match arm (`.Tag`), not
                // a member access.
                TokenKind::Dot if !self.peek().newline_before => {
                    let dot = self.bump();
                    // A `.` with no name after it (mid-edit) accesses an
                    // empty name, so the rest still parses and the editor
                    // can complete it.
                    let field = if self.name_follows() {
                        self.bump()
                    } else {
                        self.nameless(diags, dot.span, "member name after `.`");
                        Token::new(
                            TokenKind::Ident,
                            "",
                            dot.span.hi.0 as usize,
                            dot.span.hi.0 as usize,
                            self.file,
                        )
                    };
                    let span = join_spans(expr.span(), field.span);
                    let member = Expr::Member {
                        obj: Box::new(expr),
                        field: field.text,
                        span,
                    };
                    expr = self.link(diags, chain, field.span, member);
                }
                TokenKind::LParen => {
                    let at = self.peek().span;
                    let call = self.finish_call(diags, expr, Vec::new())?;
                    expr = self.link(diags, chain, at, call);
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
                        Expr::TypeApp(ty) => (**ty).clone(),
                        _ => match path_of(&expr) {
                            Some((name, span)) => named_type(name, span, Vec::new()),
                            None => break,
                        },
                    };
                    let at = self.peek().span;
                    let literal = self.parse_struct_lit(diags, ty)?;
                    expr = self.link(diags, chain, at, literal);
                }
                TokenKind::LBracket => {
                    let open = self.peek().span;
                    let generic_args = self.parse_generic_args(diags)?;
                    if self.peek().kind == TokenKind::LParen {
                        let at = self.peek().span;
                        let call = self.finish_call(diags, expr, generic_args)?;
                        expr = self.link(diags, chain, at, call);
                        continue;
                    }
                    // `Name[args]` not followed by a call is a type in value
                    // position: a static receiver (`Box[T].make()`), a struct
                    // literal head (`Box[T] { ... }`) or a generic function's
                    // instance (`ref id[T]`). Past anything but a path it can
                    // only be an instance (`ref Box[T].convert[U]`).
                    let Some((name, name_span)) = path_of(&expr) else {
                        let span = join_spans(expr.span(), self.prev_span());
                        let instance = Expr::Instance {
                            callee: Box::new(expr),
                            generic_args,
                            span,
                        };
                        expr = self.link(diags, chain, open, instance);
                        continue;
                    };
                    let mut ty = named_type(name, name_span, generic_args);
                    ty.span = join_spans(name_span, self.prev_span());
                    expr = Expr::TypeApp(Box::new(ty));
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    pub(crate) fn finish_call(
        &mut self,
        diags: &mut lilac_error::Diags,
        callee: Expr,
        generic_args: Vec<Type>,
    ) -> Result<Expr, ErrorGuaranteed> {
        let callee_span = callee.span();
        let open = self.expect(diags, TokenKind::LParen, "`(` to open call arguments")?;
        let mut args = Vec::new();
        while self.peek().kind != TokenKind::RParen {
            if self.peek().kind == TokenKind::Eof {
                return Err(Self::unclosed(diags, &open, "call", self.peek()));
            }
            let before = self.pos;
            if let Ok(arg) = self.with_struct_lit(true, |p| p.parse_expr(diags)) {
                args.push(arg);
                if self.peek().kind == TokenKind::Comma {
                    self.bump();
                } else if self.peek().kind != TokenKind::RParen {
                    return Err(Self::expected(
                        diags,
                        self.peek(),
                        "`,` or `)` in call arguments",
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

    pub(crate) fn parse_primary(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Expr, ErrorGuaranteed> {
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
                if ident.text == "array" && self.peek().kind == TokenKind::LParen {
                    return self.parse_array_value(diags, span);
                }
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
            TokenKind::KwSelf => Ok(Expr::Ident("self".to_owned(), token.span)),
            // A `.` with no name after it (mid-edit) is a tag with an
            // empty name, so the rest still parses and the editor can
            // complete it.
            TokenKind::Dot => {
                if !self.name_follows() {
                    self.nameless(diags, token.span, "tag name after `.`");
                    return Ok(Expr::Tag(String::new(), token.span));
                }
                let tag = self.bump();
                let span = join_spans(token.span, tag.span);
                Ok(Expr::Tag(tag.text, span))
            }
            _ => Err(Self::expected(diags, &token, "expression")),
        }
    }

    /// Parses the array value `@array(T, N) {}` after `@array`.
    pub(crate) fn parse_array_value(
        &mut self,
        diags: &mut lilac_error::Diags,
        span: Span,
    ) -> Result<Expr, ErrorGuaranteed> {
        let name = Token::new(
            TokenKind::Ident,
            "@array",
            span.lo.0 as usize,
            span.hi.0 as usize,
            self.file,
        );
        let ty = self.parse_array_type(diags, &name)?;
        if self.peek().kind != TokenKind::LBrace {
            return Err(Self::err(
                diags,
                ty.span,
                "an array value is written `@array(T, N) {}`",
            ));
        }
        self.parse_struct_lit(diags, ty)
    }

    /// Parses `{ field = value, ... }` after the literal's type.
    pub(crate) fn parse_struct_lit(
        &mut self,
        diags: &mut lilac_error::Diags,
        ty: Type,
    ) -> Result<Expr, ErrorGuaranteed> {
        let open = self.bump();
        let mut fields = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                return Err(Self::unclosed(diags, &open, "struct literal", self.peek()));
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
        Ok(Expr::StructLit {
            ty: Box::new(ty),
            fields,
            span,
        })
    }

    pub(crate) fn parse_if_value(
        &mut self,
        diags: &mut lilac_error::Diags,
        keyword_span: Span,
    ) -> Result<Expr, ErrorGuaranteed> {
        let cond = self.with_struct_lit(false, |p| p.parse_expr(diags))?;
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

    pub(crate) fn parse_fstring(
        &mut self,
        diags: &mut lilac_error::Diags,
        start_span: Span,
    ) -> Result<Expr, ErrorGuaranteed> {
        let mut parts = Vec::new();
        loop {
            match self.peek().kind {
                TokenKind::FStrMid => {
                    let chunk = self.bump();
                    parts.push(FStrPart::Lit {
                        bytes: chunk.bytes,
                        span: chunk.span,
                    });
                }
                TokenKind::LBrace => {
                    let open = self.bump();
                    let expr = self.with_struct_lit(true, |p| p.parse_expr(diags))?;
                    if matches!(self.peek().kind, TokenKind::Colon | TokenKind::Comma) {
                        self.format_spec(diags);
                    }
                    let close =
                        self.expect(diags, TokenKind::RBrace, "`}` to close f-string hole")?;
                    parts.push(FStrPart::Hole {
                        expr,
                        span: join_spans(open.span, close.span),
                    });
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
                    return Err(Self::expected(
                        diags,
                        self.peek(),
                        "f-string chunk, `{hole}`, or closing `\"`",
                    ));
                }
            }
        }
    }
}

/// How tightly an infix token binds in [`Parser::parse_binary`]; higher
/// binds tighter, and every level is left-associative (`a - b - c` is
/// `(a - b) - c`). `as` binds tightest.
pub(crate) fn binary_power(kind: TokenKind) -> Option<u8> {
    match kind {
        TokenKind::KwOr => Some(1),
        TokenKind::KwAnd => Some(2),
        TokenKind::EqEq | TokenKind::BangEq => Some(3),
        TokenKind::Lt | TokenKind::Gt | TokenKind::LtEq | TokenKind::GtEq => Some(4),
        TokenKind::Plus | TokenKind::Minus => Some(5),
        TokenKind::Star | TokenKind::Slash | TokenKind::Percent => Some(6),
        TokenKind::KwAs => Some(7),
        _ => None,
    }
}

/// The binary operator an infix token spells (`as` is none).
pub(crate) fn binary_op(kind: TokenKind) -> Option<BinOp> {
    match kind {
        TokenKind::KwOr => Some(BinOp::Or),
        TokenKind::KwAnd => Some(BinOp::And),
        TokenKind::EqEq => Some(BinOp::EqEq),
        TokenKind::BangEq => Some(BinOp::NotEq),
        TokenKind::Lt => Some(BinOp::Lt),
        TokenKind::Gt => Some(BinOp::Gt),
        TokenKind::LtEq => Some(BinOp::LtEq),
        TokenKind::GtEq => Some(BinOp::GtEq),
        TokenKind::Plus => Some(BinOp::Add),
        TokenKind::Minus => Some(BinOp::Sub),
        TokenKind::Star => Some(BinOp::Mul),
        TokenKind::Slash => Some(BinOp::Div),
        TokenKind::Percent => Some(BinOp::Mod),
        _ => None,
    }
}

/// The dotted name a plain-name chain spells (`A.B.Type`), with its span.
pub(crate) fn path_of(expr: &Expr) -> Option<(String, Span)> {
    match expr {
        Expr::Ident(name, span) => Some((name.clone(), *span)),
        Expr::Member { obj, field, span } => {
            let (path, _) = path_of(obj)?;
            Some((format!("{path}.{field}"), *span))
        }
        _ => None,
    }
}

/// A named type in value position: `Name` or `Name[args]` (for `Type(args)`).
pub(crate) fn type_expr(ty: &Type) -> Expr {
    match &ty.shape {
        TypeShape::Named { name, args } if args.is_empty() => {
            Expr::Ident(name.clone(), ty.name_span)
        }
        _ => Expr::TypeApp(Box::new(ty.clone())),
    }
}

/// Whether a token can start an expression (the union of the primary forms and
/// the unary minus), used to tell a *present-but-malformed* initializer from a
/// *missing* one.
pub(crate) fn starts_expression(kind: TokenKind) -> bool {
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
            | TokenKind::KwNot
            | TokenKind::KwIf
            | TokenKind::KwMatch
            | TokenKind::KwSelf
            | TokenKind::Dot
    )
}

impl Parser {
    /// A `:` or `,` after a hole's value: format specs are not syntax, so
    /// it is `E001`, and the rest of the hole is skipped up to its `}`.
    fn format_spec(&mut self, diags: &mut lilac_error::Diags) {
        let start = self.peek().span;
        let mut open = 0usize;
        loop {
            match self.peek().kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => open += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace if open > 0 => {
                    open -= 1;
                }
                TokenKind::RBrace | TokenKind::FStrEnd | TokenKind::Eof => break,
                _ => {}
            }
            self.bump();
        }
        let span = join_spans(start, self.prev_span());
        diags.emit(
            Diagnostic::error(codes::E001, "an f-string hole takes no format spec")
                .label(span, true, "not part of the hole")
                .help(
                    "format a variation through a value with its own `@format`: \
                     `{count.hex()}`, `{pad_left($\"{name}\", 10)}`",
                ),
        );
    }
}
