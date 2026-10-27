//! Items: functions, objects, enums, constants, `using` and `private`
//! blocks, and function signatures.

use lilac_ast::{
    ArmTest, Conditional, EnumVariant, Expr, FuncBody, FuncDecl, Item, ItemArm, ItemKind,
    MatchPattern, Param, Type,
};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_lex::{Token, TokenKind};
use lilac_span::Span;

use crate::{join_spans, Parser};

impl Parser {
    pub(crate) fn parse_item(
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
        if matches!(self.peek().kind, TokenKind::KwIf | TokenKind::KwMatch) {
            if in_object {
                return Err(Self::err(
                    diags,
                    self.peek().span,
                    "a conditional `if`/`match` over items is top-level only",
                ));
            }
            return self.parse_conditional_item(diags);
        }
        if self.at_test() {
            if in_object {
                return Err(Self::err(
                    diags,
                    self.peek().span,
                    "a `test` is declared only at the top level of a file",
                ));
            }
            let doc = self.peek().doc.as_deref().cloned();
            let mut item = self.parse_test(diags)?;
            item.doc = doc;
            return Ok(item);
        }
        self.failed_header = None;
        let doc = self.peek().doc.as_deref().cloned();
        let mut item = self.parse_declaration(diags, in_object)?;
        item.doc = doc;
        Ok(item)
    }

    /// Whether a test starts here: `test` is a keyword only before a
    /// string, so it stays an ordinary name (`test: function …`).
    pub(crate) fn at_test(&self) -> bool {
        let token = self.peek();
        token.kind == TokenKind::Ident
            && token.text == "test"
            && self.peek_at(1).kind == TokenKind::StringLit
    }

    /// `test "name" { … }` (the doc comment is the caller's).
    fn parse_test(&mut self, diags: &mut lilac_error::Diags) -> Result<Item, ErrorGuaranteed> {
        let keyword = self.bump();
        let literal = self.bump();
        let name = match String::from_utf8(literal.bytes) {
            Ok(name) if !name.trim().is_empty() && !name.contains(char::is_control) => name,
            _ => {
                return Err(diags.emit(
                    Diagnostic::error(
                        codes::E001,
                        "a test's name is text on one line: not empty, no control characters",
                    )
                    .label(literal.span, true, String::new()),
                ));
            }
        };
        let body = self.parse_block(diags)?;
        Ok(Item {
            doc: None,
            span: join_spans(keyword.span, body.span),
            name,
            kind: ItemKind::Test {
                name_span: literal.span,
                body,
            },
        })
    }

    /// `name: …`, an item, member or field (the doc comment is the caller's).
    fn parse_declaration(
        &mut self,
        diags: &mut lilac_error::Diags,
        in_object: bool,
    ) -> Result<Item, ErrorGuaranteed> {
        let (name, name_span) = self.parse_item_name(diags)?;
        let (generic_params, generic_spans) = self.parse_generic_params(diags)?;
        self.expect(diags, TokenKind::Colon, "`:` after declaration name")?;
        let kind_token = self.bump();
        let function = matches!(
            kind_token.kind,
            TokenKind::KwFunction | TokenKind::KwExtern | TokenKind::KwExport
        );
        let mut item = self.parse_declaration_body(
            diags,
            in_object,
            name.clone(),
            name_span,
            generic_params,
            kind_token,
        );
        match &mut item {
            Ok(Item {
                kind:
                    ItemKind::Func(FuncDecl {
                        generic_spans: spans,
                        ..
                    })
                    | ItemKind::Object {
                        generic_spans: spans,
                        ..
                    }
                    | ItemKind::Enum {
                        generic_spans: spans,
                        ..
                    },
                ..
            }) => *spans = generic_spans,
            Ok(_) => {}
            Err(_) => self.failed_header = Some((name, function)),
        }
        item
    }

    /// What follows a declaration's `name: kind`.
    fn parse_declaration_body(
        &mut self,
        diags: &mut lilac_error::Diags,
        in_object: bool,
        name: String,
        name_span: Span,
        generic_params: Vec<String>,
        mut kind_token: Token,
    ) -> Result<Item, ErrorGuaranteed> {
        if let Some(kind) = self.old_type_keyword(&kind_token) {
            let keyword = if kind == TokenKind::KwStructure {
                "structure"
            } else {
                "either"
            };
            diags.emit(
                Diagnostic::error(
                    codes::E001,
                    format!(
                        "`{}` is not a keyword; a type is declared with `{keyword}`",
                        kind_token.text
                    ),
                )
                .label(kind_token.span, true, String::new())
                .suggest(kind_token.span, keyword),
            );
            self.recover();
            // The body still parses, so the rest of the file does too.
            kind_token.kind = kind;
        }
        let kind = match kind_token.kind {
            TokenKind::KwFunction => {
                ItemKind::Func(self.parse_func_rest(diags, false, generic_params)?)
            }
            TokenKind::KwExtern | TokenKind::KwExport => {
                let is_export = kind_token.kind == TokenKind::KwExport;
                let keyword = if is_export { "export" } else { "extern" };
                let link_name = if self.peek().kind == TokenKind::StringLit {
                    let token = self.bump();
                    match String::from_utf8(token.bytes) {
                        Ok(name) if !name.is_empty() && !name.contains('\0') => Some(name),
                        _ => {
                            return Err(Self::err(
                                diags,
                                token.span,
                                &format!(
                                    "an `{keyword}` link name must be non-empty UTF-8 with no NUL"
                                ),
                            ))
                        }
                    }
                } else {
                    None
                };
                let after = format!("`function` after `{keyword}`");
                self.expect(diags, TokenKind::KwFunction, &after)?;
                let mut decl = self.parse_func_rest(diags, !is_export, generic_params)?;
                decl.is_export = is_export;
                decl.link_name = link_name;
                ItemKind::Func(decl)
            }
            TokenKind::KwAlias => {
                self.parse_alias_rest(diags, &name, name_span, &generic_params)?
            }
            TokenKind::KwStructure => {
                let members = self.parse_object_members(diags)?;
                return Ok(Item {
                    doc: None,
                    span: join_spans(name_span, self.prev_span()),
                    name,
                    kind: ItemKind::Object {
                        members,
                        generic_params,
                        generic_spans: Vec::new(),
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
            doc: None,
            span: join_spans(name_span, self.prev_span()),
            name,
            kind,
        })
    }

    /// The keyword `token` meant if it is `object` or `enum` (the keywords
    /// before `structure` and `either`) opening a body.
    fn old_type_keyword(&self, token: &Token) -> Option<TokenKind> {
        if token.kind != TokenKind::Ident || self.peek().kind != TokenKind::LBrace {
            return None;
        }
        match token.text.as_str() {
            "object" => Some(TokenKind::KwStructure),
            "enum" => Some(TokenKind::KwEither),
            _ => None,
        }
    }

    /// The type after `name: alias`. An alias is not generic, is not a
    /// hook, and names a type rather than a reference: `ref` goes where
    /// the alias is used.
    fn parse_alias_rest(
        &mut self,
        diags: &mut lilac_error::Diags,
        name: &str,
        name_span: Span,
        generic_params: &[String],
    ) -> Result<ItemKind, ErrorGuaranteed> {
        // The type is read first, so the next item parses after an error.
        let ty = self.parse_type(diags);
        if name.starts_with('@') {
            return Err(Self::err(
                diags,
                name_span,
                "a hook is a function, not an alias",
            ));
        }
        if !generic_params.is_empty() {
            return Err(Self::err(
                diags,
                name_span,
                "an alias takes no generic parameters; it may name an instance (`alias List[@bit8]`)",
            ));
        }
        if ty.is_ref {
            return Err(Self::err(
                diags,
                ty.span,
                "an alias names a type, not a reference; write `ref` where the alias is used",
            ));
        }
        Ok(ItemKind::Alias { ty })
    }

    pub(crate) fn parse_item_tail(
        &mut self,
        diags: &mut lilac_error::Diags,
        name: String,
        name_span: Span,
        generic_params: Vec<String>,
        kind_token: Token,
        in_object: bool,
    ) -> Result<Item, ErrorGuaranteed> {
        if kind_token.kind == TokenKind::KwEither {
            let (variants, members) = self.parse_enum_body(diags)?;
            Ok(Item {
                doc: None,
                span: join_spans(name_span, self.prev_span()),
                name,
                kind: ItemKind::Enum {
                    variants,
                    members,
                    generic_params,
                    generic_spans: Vec::new(),
                },
            })
        } else {
            let mut ty = self.parse_type_starting(diags, kind_token)?;
            if self.peek().kind == TokenKind::FatArrow {
                self.bump();
                let value = self.parse_expr(diags)?;
                return Self::const_item(diags, name, name_span, &generic_params, ty, value, true);
            }
            if self.peek().kind == TokenKind::Eq {
                let eq = self.bump();
                let value = self.parse_expr(diags)?;
                if in_object {
                    return Err(diags.emit(
                        Diagnostic::error(codes::E001, "a type's body declares no globals")
                            .label(eq.span, true, "`=` declares a global only at the top level")
                            .help(format!(
                                "write `{name}: Type => value` for a constant, or move the global to the top level"
                            )),
                    ));
                }
                return Self::const_item(diags, name, name_span, &generic_params, ty, value, false);
            }
            if !in_object && self.at_decl_shorthand() {
                let value = self.parse_decl_shorthand(diags, &mut ty)?;
                return Self::const_item(diags, name, name_span, &generic_params, ty, value, false);
            }
            if !in_object {
                // `name: Type` at the top level: a zeroed global.
                if !generic_params.is_empty() {
                    return Err(Self::err(
                        diags,
                        name_span,
                        "only `structure`, `function`, and `either` declarations take generic parameters",
                    ));
                }
                return Ok(Item {
                    doc: None,
                    span: join_spans(name_span, ty.span),
                    name,
                    kind: ItemKind::Global { ty, value: None },
                });
            }
            if !generic_params.is_empty() {
                return Err(Self::err(
                    diags,
                    name_span,
                    "fields take no generic parameters",
                ));
            }
            Ok(Item {
                doc: None,
                span: join_spans(name_span, ty.span),
                name,
                kind: ItemKind::Field { ty },
            })
        }
    }

    pub(crate) fn const_item(
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
                "only `structure`, `function`, and `either` declarations take generic parameters",
            ));
        }
        let span = join_spans(name_span, value.span());
        let kind = if is_inline {
            ItemKind::Const { ty, value }
        } else {
            ItemKind::Global {
                ty,
                value: Some(value),
            }
        };
        Ok(Item {
            span,
            name,
            kind,
            doc: None,
        })
    }

    pub(crate) fn parse_item_name(
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

    pub(crate) fn parse_generic_params(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<(Vec<String>, Vec<Span>), ErrorGuaranteed> {
        if self.peek().kind != TokenKind::LBracket {
            return Ok((Vec::new(), Vec::new()));
        }
        self.bump();
        let mut params = Vec::new();
        let mut spans = Vec::new();
        loop {
            let ident = self.expect(diags, TokenKind::Ident, "generic parameter name")?;
            params.push(ident.text);
            spans.push(ident.span);
            match self.peek().kind {
                TokenKind::Comma => {
                    self.bump();
                }
                TokenKind::RBracket => {
                    self.bump();
                    break;
                }
                _ => {
                    return Err(Self::expected(
                        diags,
                        self.peek(),
                        "`,` or `]` in generic parameter list",
                    ));
                }
            }
        }
        Ok((params, spans))
    }

    pub(crate) fn parse_using(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Item, ErrorGuaranteed> {
        let using = self.bump();
        let relative = self.peek().kind == TokenKind::Star;
        if relative {
            self.bump();
            self.expect(diags, TokenKind::Dot, "`.` after `*` in a relative `using`")?;
        }
        let first = self.expect(diags, TokenKind::Ident, "folder name after `using`")?;
        let mut path = first.text;
        let mut end = first.span;
        let mut segments = vec![first.span];
        while self.peek().kind == TokenKind::Dot {
            self.bump();
            let segment = self.expect(diags, TokenKind::Ident, "folder name after `.`")?;
            path.push('.');
            path.push_str(&segment.text);
            end = segment.span;
            segments.push(segment.span);
        }
        let alias = if self.peek().kind == TokenKind::KwAs {
            self.bump();
            let name = self.expect(diags, TokenKind::Ident, "alias name after `as`")?;
            end = name.span;
            Some((name.text, name.span))
        } else {
            None
        };
        Ok(Item {
            doc: None,
            span: join_spans(using.span, end),
            name: path.clone(),
            kind: ItemKind::Using {
                path,
                relative,
                alias,
                segments,
            },
        })
    }

    pub(crate) fn parse_private_block(
        &mut self,
        diags: &mut lilac_error::Diags,
        in_object: bool,
    ) -> Result<Item, ErrorGuaranteed> {
        let keyword = self.bump();
        let items = self.parse_item_block(diags, in_object, "`private` block")?;
        Ok(Item {
            doc: None,
            span: join_spans(keyword.span, self.prev_span()),
            name: String::new(),
            kind: ItemKind::Private { items },
        })
    }

    /// `{ items }`, recovering past broken items; `what` names the block
    /// for errors.
    fn parse_item_block(
        &mut self,
        diags: &mut lilac_error::Diags,
        in_object: bool,
        what: &str,
    ) -> Result<Vec<Item>, ErrorGuaranteed> {
        let open = self.expect(diags, TokenKind::LBrace, &format!("`{{` to open {what}"))?;
        let mut items = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                return Err(Self::unclosed(diags, &open, what, self.peek()));
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
        self.bump();
        Ok(items)
    }

    /// A top-level `if` chain or `match` over items, one nesting level.
    fn parse_conditional_item(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Item, ErrorGuaranteed> {
        let start = self.peek().span;
        self.nested(
            diags,
            Self::skip_stmt,
            |span| Item {
                doc: None,
                name: String::new(),
                kind: ItemKind::Error { function: false },
                span,
            },
            |p, diags| {
                let conditional = if p.peek().kind == TokenKind::KwIf {
                    p.parse_item_if(diags)?
                } else {
                    p.parse_item_match(diags)?
                };
                Ok(Item {
                    doc: None,
                    name: String::new(),
                    kind: ItemKind::Conditional(conditional),
                    span: join_spans(start, p.prev_span()),
                })
            },
        )
    }

    /// `if a { items } else if b { items } else { items }`.
    fn parse_item_if(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Conditional, ErrorGuaranteed> {
        let mut arms = Vec::new();
        let mut els = None;
        loop {
            let keyword = self.bump();
            let cond = self.with_struct_lit(false, |p| p.parse_expr(diags))?;
            let items = self.parse_item_block(diags, false, "`if` items")?;
            arms.push(ItemArm {
                test: ArmTest::If(cond),
                items,
                span: join_spans(keyword.span, self.prev_span()),
            });
            if self.peek().kind != TokenKind::KwElse {
                break;
            }
            self.bump();
            if self.peek().kind != TokenKind::KwIf {
                els = Some(self.parse_item_block(diags, false, "`else` items")?);
                break;
            }
        }
        Ok(Conditional {
            scrutinee: None,
            arms,
            els,
        })
    }

    /// `match s { .A { items } .B => item } else { items }`.
    fn parse_item_match(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Conditional, ErrorGuaranteed> {
        self.bump();
        let scrutinee = self.with_struct_lit(false, |p| p.parse_expr(diags))?;
        let open = self.expect(diags, TokenKind::LBrace, "`{` to open match arms")?;
        let mut arms = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                return Err(Self::unclosed(diags, &open, "match", self.peek()));
            }
            let (pattern, start) = self.parse_match_pattern(diags)?;
            let pattern_span = join_spans(start, self.prev_span());
            let items = match self.peek().kind {
                TokenKind::FatArrow if self.peek_at(1).kind == TokenKind::LBrace => {
                    self.bump();
                    self.parse_item_block(diags, false, "match arm items")?
                }
                TokenKind::FatArrow => {
                    self.bump();
                    vec![self.parse_item(diags, false)?]
                }
                TokenKind::LBrace => self.parse_item_block(diags, false, "match arm items")?,
                // A tag still being typed: an arm with no items (an empty
                // tag already reported).
                _ if matches!(&pattern, MatchPattern::Tag { .. }) => {
                    if matches!(&pattern, MatchPattern::Tag { name, .. } if !name.is_empty()) {
                        Self::expected(diags, self.peek(), "`=>` or `{` for match arm items");
                        self.recover();
                    }
                    Vec::new()
                }
                _ => {
                    return Err(Self::expected(
                        diags,
                        self.peek(),
                        "`=>` or `{` for match arm items",
                    ))
                }
            };
            arms.push(ItemArm {
                test: ArmTest::Pattern(pattern, pattern_span),
                items,
                span: join_spans(start, self.prev_span()),
            });
        }
        self.bump();
        let els = if self.peek().kind == TokenKind::KwElse {
            self.bump();
            Some(self.parse_item_block(diags, false, "`else` items")?)
        } else {
            None
        };
        Ok(Conditional {
            scrutinee: Some(scrutinee),
            arms,
            els,
        })
    }

    pub(crate) fn parse_object_members(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Vec<Item>, ErrorGuaranteed> {
        let open = self.expect(diags, TokenKind::LBrace, "`{` to open structure body")?;
        let mut members = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                return Err(Self::unclosed(diags, &open, "structure body", self.peek()));
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
    /// a generic declaration (`name[T]: ...`), a nested `structure`/`either`
    /// (rejected by the resolver, `E036`), a `function` with a body, or a
    /// constant (`name: Type => value`); any other `name: Type` is a
    /// payload variant.
    pub(crate) fn parse_enum_body(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<(Vec<EnumVariant>, Vec<Item>), ErrorGuaranteed> {
        let open = self.expect(diags, TokenKind::LBrace, "`{` to open either body")?;
        let mut variants = Vec::new();
        let mut members = Vec::new();
        while self.peek().kind != TokenKind::RBrace {
            if self.peek().kind == TokenKind::Eof {
                return Err(Self::unclosed(diags, &open, "either body", self.peek()));
            }
            let is_member = matches!(self.peek().kind, TokenKind::At | TokenKind::KwPrivate)
                || (self.peek().kind == TokenKind::Ident
                    && self.peek_at(1).kind == TokenKind::LBracket)
                || (self.peek().kind == TokenKind::Ident
                    && self.peek_at(1).kind == TokenKind::Colon
                    && matches!(
                        self.peek_at(2).kind,
                        TokenKind::KwStructure | TokenKind::KwEither | TokenKind::KwAlias
                    ))
                || (self.peek().kind == TokenKind::Ident
                    && self.peek_at(1).kind == TokenKind::Colon
                    && matches!(self.peek_at(2).text.as_str(), "object" | "enum")
                    && self.peek_at(3).kind == TokenKind::LBrace);
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
            let doc = self.peek().doc.as_deref().cloned();
            let ident = self.expect(diags, TokenKind::Ident, "variant name")?;
            let name_span = ident.span;
            // `name: function (…) … { … }` is a method, never a payload: a
            // function payload is a `ref function`.
            if self.peek().kind == TokenKind::Colon && self.peek_at(1).kind == TokenKind::KwFunction
            {
                self.bump();
                self.bump();
                let decl = self.parse_func_rest(diags, false, Vec::new())?;
                members.push(Item {
                    doc,
                    name: ident.text,
                    kind: ItemKind::Func(decl),
                    span: join_spans(name_span, self.prev_span()),
                });
                continue;
            }
            let payload = if self.peek().kind == TokenKind::Colon {
                self.bump();
                Some(self.parse_type(diags))
            } else {
                None
            };
            // `NAME: Type => value` is a constant, not a variant.
            if let Some(ty) = payload
                .as_ref()
                .filter(|_| self.peek().kind == TokenKind::FatArrow)
            {
                self.bump();
                let value = self.parse_expr(diags)?;
                let mut item =
                    Self::const_item(diags, ident.text, name_span, &[], ty.clone(), value, true)?;
                item.doc = doc;
                members.push(item);
                continue;
            }
            let end = payload.as_ref().map_or(name_span, |ty| ty.span);
            variants.push(EnumVariant {
                doc,
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

    pub(crate) fn parse_func_rest(
        &mut self,
        diags: &mut lilac_error::Diags,
        is_extern: bool,
        generic_params: Vec<String>,
    ) -> Result<FuncDecl, ErrorGuaranteed> {
        let open = self.expect(diags, TokenKind::LParen, "`(` to open parameter list")?;
        let mut params = Vec::new();
        let mut variadic = false;
        while self.peek().kind != TokenKind::RParen {
            if self.peek().kind == TokenKind::Ellipsis {
                let dots = self.bump();
                if !is_extern {
                    return Err(Self::err(
                        diags,
                        dots.span,
                        "only `extern` functions can take `...`",
                    ));
                }
                if self.peek().kind != TokenKind::RParen {
                    let span = self.peek().span;
                    return Err(Self::err(diags, span, "`...` must be the last parameter"));
                }
                variadic = true;
                break;
            }
            if self.peek().kind == TokenKind::Eof {
                return Err(Self::unclosed(diags, &open, "parameter list", self.peek()));
            }
            params.push(self.parse_param(diags)?);
            if self.peek().kind == TokenKind::Comma {
                self.bump();
            } else if self.peek().kind != TokenKind::RParen {
                return Err(Self::expected(
                    diags,
                    self.peek(),
                    "`,` or `)` in parameter list",
                ));
            }
        }
        self.bump();
        let (returns, failures) = self.parse_returns(diags)?;
        let body = self.parse_func_body(diags, is_extern)?;
        Ok(FuncDecl {
            generic_params,
            generic_spans: Vec::new(),
            params,
            returns,
            failures,
            body,
            is_extern,
            is_export: false,
            link_name: None,
            variadic,
        })
    }

    /// Parses a function body (`{ ... }` or `=> expr`); `extern` declarations
    /// have none.
    pub(crate) fn parse_func_body(
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
            // `name: function (…) …` with no body is most likely meant to
            // hold a function, which is a `ref function` (`E077`).
            _ => {
                let found = self.peek();
                Err(diags.emit(
                    Diagnostic::error(
                        codes::E001,
                        format!(
                            "expected `{{` or `=>` for function body, found {}",
                            found.describe()
                        ),
                    )
                    .label(found.span, true, String::new())
                    .help(
                        "a function needs a body; a field or global that holds a function \
                         is a `ref function (…) …`",
                    ),
                ))
            }
        }
    }

    pub(crate) fn parse_param(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Param, ErrorGuaranteed> {
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
    pub(crate) fn is_anonymous_param(first: &Token, second: &Token) -> bool {
        matches!(
            first.kind,
            TokenKind::Ident | TokenKind::KwRef | TokenKind::KwSelf | TokenKind::KwUnknown
        ) && second.kind != TokenKind::Colon
    }

    /// A signature's outs: the success outs, then, for a function that can
    /// fail, `else` and the failure outs (`else nothing` for none).
    pub(crate) fn parse_returns(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<(Vec<Param>, Option<Vec<Param>>), ErrorGuaranteed> {
        let returns = match self.peek().kind {
            TokenKind::LBrace | TokenKind::FatArrow | TokenKind::Eof | TokenKind::KwElse => {
                Vec::new()
            }
            _ => self.parse_out_slots(diags)?,
        };
        if self.peek().kind != TokenKind::KwElse {
            return Ok((returns, None));
        }
        self.bump();
        if matches!(
            self.peek().kind,
            TokenKind::LBrace | TokenKind::FatArrow | TokenKind::Eof
        ) {
            return Err(Self::expected(
                diags,
                self.peek(),
                "failure outs or `nothing` after `else`",
            ));
        }
        let failures = self.parse_out_slots(diags)?;
        Ok((returns, Some(failures)))
    }

    /// `nothing`, `name: Type` slots separated by commas, or the slots in
    /// parentheses as a function type writes them (no type starts with
    /// `(`, so the two never meet).
    fn parse_out_slots(
        &mut self,
        diags: &mut lilac_error::Diags,
    ) -> Result<Vec<Param>, ErrorGuaranteed> {
        if matches!(self.peek().kind, TokenKind::KwNothing | TokenKind::LParen) {
            return self.parse_type_outs(diags);
        }
        let mut slots = Vec::new();
        loop {
            slots.push(self.parse_param(diags)?);
            if self.peek().kind == TokenKind::Comma {
                self.bump();
                continue;
            }
            break;
        }
        Ok(slots)
    }
}
