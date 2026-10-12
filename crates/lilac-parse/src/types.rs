//! Types: names with generic arguments, `ref`, arrays and function types.

use lilac_ast::{FuncType, Type, TypeShape};
use lilac_error::ErrorGuaranteed;
use lilac_lex::{Token, TokenKind};
use lilac_span::Span;

use crate::{join_spans, Parser};

impl Parser {
    /// Parses a type, recovering a failure into [`TypeShape::Error`]
    /// (recording [`Parser::had_errors`]) so the enclosing construct survives.
    pub(crate) fn parse_type(&mut self, diags: &mut lilac_error::Diags) -> Type {
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
    pub(crate) fn parse_type_inner(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Type, ErrorGuaranteed> {
        self.nested(diags, Self::skip_type, error_type, Self::parse_type_body)
    }

    pub(crate) fn parse_type_body(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Type, ErrorGuaranteed> {
        if !starts_type(self.peek().kind) {
            return Err(Self::expected(diags, self.peek(), "a type"));
        }
        let first = self.bump();
        self.parse_type_starting(diags, first)
    }

    pub(crate) fn parse_type_starting(
        &mut self,
        diags: &mut lilac_error::Diags,
        first: Token,
    ) -> Result<Type, ErrorGuaranteed> {
        let start = first.span;
        if first.kind == TokenKind::KwFunction {
            return self.parse_func_type(diags, start, start, false);
        }
        if first.kind == TokenKind::KwExport {
            // `export function (…)`: a C function's address.
            let keyword = self.expect(diags, TokenKind::KwFunction, "`function` after `export`")?;
            let mut ty = self.parse_func_type(diags, start, keyword.span, false)?;
            if let TypeShape::Function(func) = &mut ty.shape {
                func.is_export = true;
            }
            return Ok(ty);
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
                return Err(Self::expected(diags, &first, "a type"));
            }
        };
        if name_token.text == "@array" && self.peek().kind == TokenKind::LParen {
            let mut ty = self.parse_array_type(diags, &name_token)?;
            ty.is_ref = is_ref;
            ty.span = join_spans(start, ty.span);
            return Ok(ty);
        }
        if name_token.text == "@format_args" && self.peek().kind == TokenKind::LParen {
            self.bump();
            let writer = self.parse_type_inner(diags)?;
            let close = self.expect(diags, TokenKind::RParen, "`)` to close `@format_args`")?;
            return Ok(Type {
                shape: TypeShape::FormatArgs {
                    writer: Box::new(writer),
                },
                name_span: name_token.span,
                is_ref,
                span: join_spans(start, close.span),
            });
        }
        let mut end = name_token.span;
        let mut generic_args = Vec::new();
        if self.peek().kind == TokenKind::LBracket {
            generic_args = self.parse_generic_args(diags)?;
            end = self.prev_span();
        }
        let shape = if name_token.kind == TokenKind::KwSelf {
            if !generic_args.is_empty() {
                return Err(Self::err(
                    diags,
                    join_spans(name_token.span, end),
                    "`self` takes no generic arguments",
                ));
            }
            TypeShape::SelfType
        } else {
            TypeShape::Named {
                name: name_token.text,
                args: generic_args,
            }
        };
        Ok(Type {
            shape,
            name_span: name_token.span,
            is_ref,
            span: join_spans(start, end),
        })
    }

    /// `[T, U]`: generic arguments, from the `[` through the `]`.
    pub(crate) fn parse_generic_args(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Vec<Type>, ErrorGuaranteed> {
        self.expect(diags, TokenKind::LBracket, "`[` to open generic arguments")?;
        let mut args = Vec::new();
        loop {
            args.push(self.parse_type_inner(diags)?);
            match self.peek().kind {
                TokenKind::Comma => {
                    self.bump();
                }
                TokenKind::RBracket => {
                    self.bump();
                    return Ok(args);
                }
                _ => {
                    return Err(Self::expected(
                        diags,
                        self.peek(),
                        "`,` or `]` in generic argument list",
                    ));
                }
            }
        }
    }

    /// Parses `(Type, length)` after `@array`; the length is a number
    /// literal.
    pub(crate) fn parse_array_type(
        &mut self,
        diags: &mut lilac_error::Diags,
        name: &Token,
    ) -> Result<Type, ErrorGuaranteed> {
        self.expect(diags, TokenKind::LParen, "`(` after `@array`")?;
        let elem = self.parse_type_inner(diags)?;
        self.expect(diags, TokenKind::Comma, "`,` before the array length")?;
        let len = self.expect(diags, TokenKind::Number, "an array length literal")?;
        let close = self.expect(diags, TokenKind::RParen, "`)` to close `@array`")?;
        Ok(Type {
            shape: TypeShape::Array {
                element: Box::new(elem),
                len: len.text,
                len_span: len.span,
            },
            name_span: name.span,
            is_ref: false,
            span: join_spans(name.span, close.span),
        })
    }

    /// Parses `function (params) returns` after the `function` keyword.
    pub(crate) fn parse_func_type(
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
                return Err(Self::expected(
                    diags,
                    self.peek(),
                    "`,` or `)` in function type",
                ));
            }
        }
        self.bump();
        let (returns, failures) = self.parse_returns(diags)?;
        let end = self.prev_span();
        Ok(Type {
            shape: TypeShape::Function(FuncType {
                params,
                returns,
                failures,
                is_export: false,
            }),
            name_span,
            is_ref,
            span: join_spans(start, end),
        })
    }

    pub(crate) fn parse_type_name(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Token, ErrorGuaranteed> {
        let token = self.bump();
        self.type_name_from(diags, token)
    }

    pub(crate) fn type_name_from(
        &mut self,
        diags: &mut lilac_error::Diags,
        token: Token,
    ) -> Result<Token, ErrorGuaranteed> {
        let (mut name, mut span) = match token.kind {
            TokenKind::At => {
                let ident = self.expect(diags, TokenKind::Ident, "type name after `@`")?;
                (
                    format!("@{}", ident.text),
                    join_spans(token.span, ident.span),
                )
            }
            TokenKind::Ident => (token.text, token.span),
            TokenKind::KwUnknown | TokenKind::KwSelf => return Ok(token),
            _ => return Err(Self::expected(diags, &token, "a type name")),
        };
        // A path (`A.B.Type`, `@builtin.Os`). A `.` on a new line starts a
        // match arm (`.Tag`), not a segment.
        while self.peek().kind == TokenKind::Dot && !self.peek().newline_before {
            let dot = self.bump();
            if !self.name_follows() {
                // Mid-edit: an empty last segment, which the editor can
                // complete.
                self.nameless(diags, dot.span, "a name after `.`");
                name.push('.');
                span = join_spans(span, dot.span);
                break;
            }
            let segment = self.bump();
            name = format!("{name}.{}", segment.text);
            span = join_spans(span, segment.span);
        }
        Ok(Token::new(
            TokenKind::Ident,
            &name,
            span.lo.0 as usize,
            span.hi.0 as usize,
            self.file,
        ))
    }
}

/// The [`TypeShape::Error`] stand-in (see [`Parser::poison_type`]).
pub(crate) fn error_type(span: Span) -> Type {
    Type {
        shape: TypeShape::Error,
        name_span: span,
        is_ref: false,
        span,
    }
}

/// A plain named type (`Name` / `Name[args]`) built from an expression head.
pub(crate) fn named_type(name: String, name_span: Span, args: Vec<Type>) -> Type {
    Type {
        shape: TypeShape::Named { name, args },
        name_span,
        is_ref: false,
        span: name_span,
    }
}

/// Whether a token can start a type (mirrors [`Parser::parse_type_starting`]'s
/// accepted first tokens; `nothing` is included so its targeted diagnostic is
/// preserved).
pub(crate) fn starts_type(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::KwFunction
            | TokenKind::KwExport
            | TokenKind::KwRef
            | TokenKind::At
            | TokenKind::Ident
            | TokenKind::KwUnknown
            | TokenKind::KwSelf
            | TokenKind::KwNothing
    )
}
