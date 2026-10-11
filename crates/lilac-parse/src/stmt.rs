//! Blocks and statements.

use lilac_ast::{
    Block, Expr, LetBinding, MatchArm, MatchArmBody, MatchPattern, Stmt, Type, TypeShape,
};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_lex::TokenKind;
use lilac_span::Span;

use crate::expr::{starts_expression, type_expr};
use crate::{join_spans, Parser};

impl Parser {
    pub(crate) fn parse_block(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Block, ErrorGuaranteed> {
        self.nested(
            diags,
            Self::skip_group,
            |span| Block {
                stmts: Vec::new(),
                span,
            },
            Self::parse_block_body,
        )
    }

    pub(crate) fn parse_block_body(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Block, ErrorGuaranteed> {
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
                return Err(Self::unclosed(diags, &open, "block", self.peek()));
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

    pub(crate) fn parse_stmt(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Stmt, ErrorGuaranteed> {
        self.nested(
            diags,
            Self::skip_stmt,
            |span| Stmt::Error { span },
            Self::parse_stmt_body,
        )
    }

    pub(crate) fn parse_stmt_body(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Stmt, ErrorGuaranteed> {
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
    pub(crate) fn parse_if_stmt(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Stmt, ErrorGuaranteed> {
        let keyword = self.bump();
        let cond = self.with_struct_lit(false, |p| p.parse_expr(diags))?;
        let capture = if self.peek().kind == TokenKind::Arrow {
            self.bump();
            Some(self.parse_param(diags)?)
        } else {
            None
        };
        if self.peek().kind != TokenKind::LBrace {
            return Err(Self::expected(
                diags,
                self.peek(),
                "`{` after `if` condition",
            ));
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
            capture: capture.map(Box::new),
            then,
            els,
            span: join_spans(keyword.span, end),
        })
    }

    /// `loop <stmt>`: `loop { ... }`, `loop if c { ... } else break`, ...
    pub(crate) fn parse_loop_stmt(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Stmt, ErrorGuaranteed> {
        let keyword = self.bump();
        let body = self.parse_stmt(diags)?;
        let span = join_spans(keyword.span, body.span());
        Ok(Stmt::Loop {
            body: Box::new(body),
            span,
        })
    }

    pub(crate) fn parse_match_stmt(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Stmt, ErrorGuaranteed> {
        let keyword = self.bump();
        let scrutinee = self.with_struct_lit(false, |p| p.parse_expr(diags))?;
        let open = self.expect(diags, TokenKind::LBrace, "`{` to open match arms")?;
        let mut arms = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                return Err(Self::unclosed(diags, &open, "match", self.peek()));
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

    pub(crate) fn parse_match_arm(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<MatchArm, ErrorGuaranteed> {
        let (pattern, start) = self.parse_match_pattern(diags)?;
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
        } else if let MatchPattern::Tag { name, span, .. } = &pattern {
            // A tag still being typed: an arm with no body, so the rest of
            // the `match` still parses (an empty tag already reported).
            if !name.is_empty() {
                Self::expected(diags, self.peek(), "`=>` or `{` for match arm body");
                self.recover();
            }
            let end = *span;
            let block = Block {
                stmts: Vec::new(),
                span: end,
            };
            (MatchArmBody::Block(block), end)
        } else {
            return Err(Self::expected(
                diags,
                self.peek(),
                "`=>` or `{` for match arm body",
            ));
        };
        Ok(MatchArm {
            pattern,
            capture,
            body,
            span: join_spans(start, end),
        })
    }

    /// A match arm's pattern (`.Tag`, `Enum.Tag` or a literal) and the span
    /// it starts at.
    pub(crate) fn parse_match_pattern(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<(MatchPattern, Span), ErrorGuaranteed> {
        Ok(match self.peek().kind {
            TokenKind::Dot => {
                let dot = self.bump();
                // A `.` with no name (mid-edit) is an empty tag.
                let (name, span) = if self.name_follows() {
                    let tag = self.bump();
                    (tag.text, join_spans(dot.span, tag.span))
                } else {
                    self.nameless(diags, dot.span, "match tag after `.`");
                    (String::new(), dot.span)
                };
                (
                    MatchPattern::Tag {
                        qualifier: None,
                        name,
                        span,
                    },
                    dot.span,
                )
            }
            TokenKind::Ident | TokenKind::At => {
                let mut qualifier = self.parse_type_inner(diags)?;
                let start = qualifier.span;
                let swallowed = if self.peek().kind == TokenKind::Dot {
                    None
                } else {
                    split_tag(&mut qualifier)
                };
                let tag = if let Some(tag) = swallowed {
                    tag
                } else {
                    self.expect(
                        diags,
                        TokenKind::Dot,
                        "`.` after the enum name in a pattern",
                    )?;
                    self.expect(diags, TokenKind::Ident, "match tag after `.`")?
                        .text
                };
                (
                    MatchPattern::Tag {
                        qualifier: Some(qualifier),
                        name: tag,
                        span: join_spans(start, self.prev_span()),
                    },
                    start,
                )
            }
            TokenKind::Number | TokenKind::StringLit | TokenKind::CharLit => {
                let lit = self.bump();
                (MatchPattern::Const(lit.text), lit.span)
            }
            _ => {
                return Err(Self::expected(
                    diags,
                    self.peek(),
                    "`.Tag`, `Enum.Tag`, or a literal pattern for match arm",
                ));
            }
        })
    }

    pub(crate) fn parse_match_value(
        &mut self,
        diags: &mut lilac_error::Diags,
        keyword_span: Span,
    ) -> Result<Expr, ErrorGuaranteed> {
        let scrutinee = self.with_struct_lit(false, |p| p.parse_expr(diags))?;
        let open = self.expect(diags, TokenKind::LBrace, "`{` to open match arms")?;
        let mut arms = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                return Err(Self::unclosed(diags, &open, "match", self.peek()));
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

    pub(crate) fn parse_let_or_expr_stmt(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Stmt, ErrorGuaranteed> {
        if self.peek().kind == TokenKind::Ident && self.peek_at(1).kind == TokenKind::Colon {
            let name = self.bump();
            self.bump();
            let ty = self.parse_type(diags);
            let mut poisoned = ty.is_error();
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
                poisoned = next_ty.is_error();
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
                            ty: Box::new(only.ty),
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
                return Err(diags.already_reported());
            }
            let value = if bindings.len() == 1 && self.at_decl_shorthand() {
                self.parse_decl_shorthand(diags, &mut bindings[0].ty)?
            } else if bindings.len() == 1
                && self.peek().kind != TokenKind::Eq
                && ends_with_empty_segment(&bindings[0].ty)
            {
                // `name: Type.` mid-edit (the `.` is reported): keep the
                // declaration, so its type's path completes.
                Expr::Error {
                    span: bindings[0].ty.span,
                }
            } else {
                self.expect(diags, TokenKind::Eq, "`=` after variable type")?;
                self.parse_expr_recover(diags)?
            };
            let span = join_spans(bindings[0].span, value.span());
            if bindings.len() == 1 {
                let only = bindings.remove(0);
                return Ok(Stmt::Let {
                    name: only.name,
                    ty: Box::new(only.ty),
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

    /// Whether a declaration shorthand follows `name: Type`: `{`, `(`, or
    /// the `.` of `Box[T].create(args)` on the same line.
    pub(crate) fn at_decl_shorthand(&self) -> bool {
        let next = self.peek();
        match next.kind {
            TokenKind::LBrace | TokenKind::LParen => true,
            TokenKind::Dot => !next.newline_before,
            _ => false,
        }
    }

    /// The declaration shorthands after `name: Type`: `{ fields }` is
    /// `= Type { fields }`, and `.function(args)` is
    /// `= Type.function(args)` (an [`Expr::Construct`]). A dotted type
    /// name read before `(` loses its last segment to the function, which
    /// takes the name's generic arguments (`Type.make[T](args)`); a type
    /// with generic arguments is followed by its own `.function`
    /// (`Box[T].create(args)`). `Type(args)` with no function stays a call
    /// of the type, which typeck rejects.
    pub(crate) fn parse_decl_shorthand(
        &mut self,
        diags: &mut lilac_error::Diags,
        ty: &mut Type,
    ) -> Result<Expr, ErrorGuaranteed> {
        if ty.is_ref || matches!(ty.shape, TypeShape::Function(_) | TypeShape::Error) {
            let found = self.peek();
            let msg = format!(
                "expected `=` after variable type, found {}",
                found.describe()
            );
            return Err(diags.emit(
                Diagnostic::error(codes::E001, msg)
                    .label(found.span, true, String::new())
                    .help("`Type { ... }` and `Type.function(...)` shorthands need a named type"),
            ));
        }
        match self.peek().kind {
            TokenKind::LBrace => self.parse_struct_lit(diags, ty.clone()),
            TokenKind::Dot => {
                let dot = self.bump();
                let (function, function_span) = if self.name_follows() {
                    let name = self.bump();
                    (name.text, name.span)
                } else {
                    // Mid-edit: an empty name, which the editor completes
                    // with the type's static functions.
                    self.nameless(diags, dot.span, "a static function name after `.`");
                    (
                        String::new(),
                        Span::new(dot.span.hi.0, dot.span.hi.0, dot.span.file),
                    )
                };
                let generic_args = if self.peek().kind == TokenKind::LBracket {
                    self.parse_generic_args(diags)?
                } else {
                    Vec::new()
                };
                self.finish_construct(diags, ty, function, function_span, generic_args)
            }
            _ => {
                if let Some((function, function_span, generic_args)) = split_function(ty) {
                    return self.finish_construct(diags, ty, function, function_span, generic_args);
                }
                let callee = type_expr(ty);
                self.finish_call(diags, callee, Vec::new())
            }
        }
    }

    /// `(args)` after `Type.function` in the declaration shorthand.
    fn finish_construct(
        &mut self,
        diags: &mut lilac_error::Diags,
        ty: &Type,
        function: String,
        function_span: Span,
        generic_args: Vec<Type>,
    ) -> Result<Expr, ErrorGuaranteed> {
        let member_span = join_spans(ty.span, function_span);
        if function.is_empty() && self.peek().kind != TokenKind::LParen {
            // Mid-edit (`Box[T].`; the `.` is reported): no call yet.
            return Ok(Expr::Construct {
                function,
                member_span,
                generic_args,
                args: Vec::new(),
                span: member_span,
            });
        }
        let callee = Expr::Ident(function.clone(), member_span);
        let Expr::Call { args, span, .. } = self.finish_call(diags, callee, Vec::new())? else {
            unreachable!("`finish_call` builds a call");
        };
        Ok(Expr::Construct {
            function,
            member_span,
            generic_args,
            args,
            span,
        })
    }
}

/// Whether a type's dotted name ends in a `.` with no name (`Type.`).
fn ends_with_empty_segment(ty: &Type) -> bool {
    matches!(&ty.shape, TypeShape::Named { name, .. } if name.ends_with('.'))
}

/// Splits the function off a dotted type name read before `(` in the
/// declaration shorthand (`A.Type.make[T]`), leaving `A.Type`; the
/// function takes the generic arguments. `None` for an undotted name.
fn split_function(ty: &mut Type) -> Option<(String, Span, Vec<Type>)> {
    let TypeShape::Named { name, args } = &mut ty.shape else {
        return None;
    };
    let (path, function) = name.rsplit_once('.')?;
    let function = function.to_owned();
    let path_len = u32::try_from(path.len()).unwrap_or(u32::MAX);
    let lo = ty.name_span.lo.0;
    let function_span = Span::new(
        ty.name_span
            .hi
            .0
            .saturating_sub(u32::try_from(function.len()).unwrap_or(u32::MAX)),
        ty.name_span.hi.0,
        ty.name_span.file,
    );
    name.truncate(path.len());
    let generic_args = std::mem::take(args);
    ty.name_span = Span::new(lo, lo.saturating_add(path_len), ty.name_span.file);
    ty.span = ty.name_span;
    Some((function, function_span, generic_args))
}

/// Splits the tag off a dotted qualifier that swallowed it (`A.Enum.Tag`),
/// leaving `A.Enum`.
fn split_tag(qualifier: &mut Type) -> Option<String> {
    if qualifier.is_ref {
        return None;
    }
    let TypeShape::Named { name, args } = &mut qualifier.shape else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let (path, tag) = name.rsplit_once('.')?;
    let tag = tag.to_owned();
    let len = u32::try_from(path.len()).unwrap_or(u32::MAX);
    name.truncate(path.len());
    qualifier.name_span = Span::new(
        qualifier.name_span.lo.0,
        qualifier.name_span.lo.0.saturating_add(len),
        qualifier.name_span.file,
    );
    qualifier.span = qualifier.name_span;
    Some(tag)
}

/// Whether a token can start a statement (a block-recovery boundary): every
/// token that can begin an expression-statement, plus the non-expression
/// statement keywords.
pub(crate) fn starts_stmt(kind: TokenKind) -> bool {
    starts_expression(kind)
        || matches!(
            kind,
            TokenKind::KwLoop | TokenKind::KwReturn | TokenKind::KwBreak | TokenKind::KwContinue
        )
}
