const std = @import("std");
const _tokens = @import("tokens.zig");
const Token = _tokens.Token;
const TokenType = _tokens.TokenType;
const Tokenizer = @import("lexer.zig").Tokenizer;
const untyped = @import("untyped.zig");
const Ast = untyped.Ast;
const logger = @import("logger.zig");

const Parser = struct { 
    ast: Ast,
    logger: logger.Logger,
    current_index: usize = 0,
    allocator: std.mem.Allocator,

    pub fn LogAtToken(self: *Parser, comptime fmt: []const u8, args: anytype, hint: ?[]const u8, log_level: logger.LogLevel, token_index: usize) void {
        const token = self.ast.tokens[token_index];
        self.logger.logAt(fmt, args, hint, token.start, token.end, log_level, self.ast.source);
    }
    
    pub fn current(self: *Parser) Token {

        if (self.eof()) return .{
            .start = self.current_index,
            .end = self.current_index,
            .token_type = .Invalid,
        };

        return self.ast.tokens[self.current_index];
    }

    pub fn next(self: *Parser) Token {

        if (self.current_index + 1 >= self.ast.tokens.len - 1) return .{
            .start = self.current_index,
            .end = self.current_index,
            .token_type = .Invalid,
        };

        return self.ast.tokens[self.current_index + 1];
    }

    pub fn eat(self: *Parser) Token {

        if (self.eof()) return .{
            .start = self.current_index,
            .end = self.current_index,
            .token_type = .Invalid,
        };

        self.current_index += 1;
        
        return self.ast.tokens[self.current_index - 1];
    }

    pub fn eat_expected(self: *Parser, token_type: TokenType, hint: ?[]const u8) ?Token {
        
        const prev_token = self.current();

        if (prev_token.token_type != token_type) {
            self.LogAtToken(
                "Unexpected token. Expected \x22{s}\x22 but got \x22{s}\x22 instead.", 
                .{ token_type.toString(), self.ast.source[prev_token.start..prev_token.end] },
                hint,
                .Error,
                self.current_index);
            return null;
        }

        self.current_index += 1;
        return prev_token;
    }

    pub fn eat_if(self: *Parser, token_type: TokenType) ?Token {
        const prev_token = self.current();

        if (prev_token.token_type != token_type) return null;

        self.current_index += 1;
        return prev_token;
    }

    pub fn skip(self: *Parser) void {
        self.current_index += 1;
    }

    pub fn skip_expected(self: *Parser, token_type: TokenType, hint: ?[]const u8) bool {

        const prev_token = self.current();

        if (prev_token.token_type != token_type) {
            self.LogAtToken(
                "Unexpected token. Expected \x22{s}\x22 but got \x22{s}\x22 instead.", 
                .{ token_type.toString(), self.ast.source[prev_token.start..prev_token.end]},
                hint,
                .Error,
                self.current_index);
            return true;
        }

        self.current_index += 1;
        return false;
    }

    pub fn skip_closing(self: *Parser, open_token_position: usize, token_type: TokenType) bool {
        const prev_token = self.current();

        if (prev_token.token_type != token_type) {
            self.LogAtToken(
                "Failed to find closing \x22{s}\x22 token.", .{ token_type.toString() }, 
                null,
                .Error,
                open_token_position);
            return true;
        }

        self.current_index += 1;
        return false;
    }

    pub fn skip_if(self: *Parser, token_type: TokenType) bool {

        if (self.current().token_type != token_type) return false;

        self.current_index += 1;
        return true;
    }

    pub fn eof(self: *Parser) bool {
        return self.ast.tokens.len <= self.current_index;
    }

    pub fn makeStmtNode(self: *Parser, start_token: usize, expression: untyped.Statement) untyped.Node(untyped.Statement) {
        return untyped.Node(untyped.Statement).init(
            self.allocator, 
            self.ast.tokens[start_token].start,
            self.ast.tokens[self.current_index - 1].end, 
            expression) catch @panic("Out of Memory.");
    }

    pub fn makeExprNode(self: *Parser, start_token: usize, expression: untyped.Expression) untyped.Node(untyped.Expression) {
        return untyped.Node(untyped.Expression).init(
            self.allocator, 
            self.ast.tokens[start_token].start,
            self.ast.tokens[self.current_index - 1].end, 
            expression) catch @panic("Out of Memory.");
    }
};

var _debug: bool = false;

pub fn parse(source: [:0]const u8, allocator: std.mem.Allocator, debug: bool) Ast {

    _debug = debug;

    var tokens: std.ArrayList(Token) = .empty;
    defer tokens.deinit(allocator);

    var tokenizer = Tokenizer.init(source);

    
    while (tokenizer.next()) |token| {
        tokens.append(allocator, token) catch @panic("Out of Memory.");
    }

    if (debug) {
        std.debug.print("-- Tokens --\n", .{});
        for (tokens.items) |token| {   
            std.debug.print("{s} - {s}\n", .{@tagName(token.token_type), source[token.start..token.end]});
        }
    }

    var parser = Parser { 
        .ast = Ast {
            .source = source,
            .tokens = tokens.toOwnedSlice(allocator) catch @panic("Out of Memory."),
            .root_block = undefined,
        },
        .logger = .{
            .allocator = allocator,
        },
        .allocator = allocator,
    };
    defer parser.logger.deinit();

    parser.ast.root_block = parseTopBlock(&parser);

    logger.printLogs(parser.logger);

    return parser.ast;
}

fn parseTopBlock(parser: *Parser) untyped.Block {
    var body = std.ArrayList(untyped.Node(untyped.Statement)).empty;

    while (!parser.eof()) {
        body.append(parser.allocator, parseStatement(parser)) catch @panic("Out of Memory");
    }

    return untyped.Block {
        .body = body
    };
}

fn parseBlockWithNode(parser: *Parser) untyped.Node(untyped.Block) {
    const start = parser.ast.tokens[parser.current_index];
    const block = parseBlock(parser);
    return untyped.Node(untyped.Block).init(parser.allocator, start.start, parser.ast.tokens[parser.current_index - 1].end, block) catch @panic("Out of Memory.");
}

fn parseBlock(parser: *Parser) untyped.Block {

    var body = std.ArrayList(untyped.Node(untyped.Statement)).empty;

    const open_brace_pos = parser.current_index;

    if (parser.skip_expected(.OpenBrace, null)) {
        return untyped.Block {
            .body = body
        };
    }

    while (parser.current().token_type != .CloseBrace and !parser.eof()) {
        body.append(parser.allocator, parseStatement(parser)) catch @panic("Out of Memory");
    }

    if (parser.skip_closing(open_brace_pos, .CloseBrace)) {
        return untyped.Block {
            .body = body
        };
    }
    
    return untyped.Block {
        .body = body
    };
}

fn parseStatement(parser: *Parser) untyped.Node(untyped.Statement) {

    switch (parser.current().token_type) {

        .Using => {

            parser.skip();

            const namespace = parseMember(parser);

            var alias: ?untyped.Node(untyped.Expression) = null;

            if (parser.skip_if(.As)) {
                alias = parseExpression(parser);
            }

            return parser.makeStmtNode(parser.current_index, .{ 
                .Using = .{
                    .namespace = namespace,
                    .alias = alias,
                }
            });
        },

        .Private => {
            parser.skip();

            const node = parser.makeStmtNode(parser.current_index,.{
                .Private = parseBlockWithNode(parser),
            });
            return node;
        },

        .OpenBrace => {
            const node = parser.makeStmtNode(parser.current_index,.{
                .Block = parseBlockWithNode(parser),
            });
            return node;
        },

        .Loop => {
            parser.skip();

            const node = parser.makeStmtNode(parser.current_index - 1, .{
                .Loop = parser.makeStmtNode(parser.current_index, .{
                    .Block = parseBlockWithNode(parser),
                }),
            });
            return node;
        },

        .Return => { 
            parser.skip();
            return parser.makeStmtNode(parser.current_index - 1, .Return);
        },

        .Break => { 
            parser.skip();
            return parser.makeStmtNode(parser.current_index - 1, .Break);
        },

        .Continue => { 
            parser.skip();
            return parser.makeStmtNode(parser.current_index - 1, .Continue);
        },

        .Invalid => {
            parser.LogAtToken(
                "Invalid Token", .{}, 
                null,
                .Error,
                parser.current_index);
            parser.skip();
            return parser.makeStmtNode(parser.current_index - 1, .Error);
        },

        else => { 
            const node = parser.makeStmtNode(parser.current_index, .{
                .Expression = parseExpression(parser),
            });
            return node;
        },
    }
}

fn parseExpression(parser: *Parser) untyped.Node(untyped.Expression) {
    const node = parseAssignment(parser);
    return node;
}

// =, -=, +=, *=, /=, %=, =>
fn parseAssignment(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    const lhs = parseConditionals(parser);
    
    switch (parser.current().token_type) {

        .Equals, .PlusEquals, .MinusEquals, .TimesEquals, .DivEquals, .PercentEquals, .FatRightArrow => {

            const op = parser.current_index;

            parser.skip();

            const value = parseConditionals(parser);

            return parser.makeExprNode(start_of_expression, .{
                .Assignment = .{
                    .assignee = lhs,
                    .value = value,
                    .op_token_index = op,
                }
            });
        },

        else => {
            return lhs;
        },
    }
}

fn parseConditionals(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;
    
    switch (parser.current().token_type) {

        .If => {
            parser.skip();

            var condition: untyped.Node(untyped.Expression) = undefined;
            
            if (parser.current().token_type == .OpenBrace) {
                condition = parser.makeExprNode(start_of_expression, .Error);
                parser.LogAtToken(
                    "You forgot the if condition.", .{}, 
                    "if condition {}",
                    .Error,
                    parser.current_index - 1);
            } else {
                condition = parseBinop1(parser);
            }

            var captures: ?untyped.Node(untyped.Expression) = null;

            if (parser.skip_if(.RightArrow)) {

                captures = parseDeclarationList(parser);
            }

            const body: untyped.Node(untyped.Statement) = parseStatement(parser);

            var else_body: ?untyped.Node(untyped.Else) = null;

            if (parser.skip_if(.Else)) {
                else_body = untyped.Node(untyped.Else).init(
                    parser.allocator, 
                    start_of_expression, 
                    parser.current_index, 
                    .{
                        .body = parseStatement(parser),
                    }
                ) catch @panic("Out of Memory.");
            }

            return parser.makeExprNode(start_of_expression, .{
                .If = .{
                    .condition = condition,
                    .captures = captures,
                    .body = body,
                    .else_body = else_body,
                }
            });
        },

        .Match => {
            parser.skip();

            var value: untyped.Node(untyped.Expression) = undefined;

            if (parser.current().token_type == .OpenBrace) {
                value = parser.makeExprNode(start_of_expression, .Error);
                parser.LogAtToken(
                    "You forgot the matches value.", .{}, 
                    "match value {}",
                    .Error,
                    parser.current_index);
            } else {
                value = parseMember(parser);
            }

            var body = std.ArrayList(untyped.Case).empty;
            var else_case: ?untyped.Node(untyped.Else) = null;

            const open_brace_pos = parser.current_index;

            if (parser.skip_expected(.OpenBrace, "match value {}")) {
                return parser.makeExprNode(start_of_expression, .Error);
            }

            while (parser.current().token_type != .CloseBrace and !parser.eof()) {
                if (parser.current().token_type == .Else) {
                    if (else_case != null) {
                        const other_else = parseElseCase(parser);
                        parser.logger.logError(
                            "Else has been declared twice in match expression.", .{}, 
                            "Try removing or merging your else blocks.", 
                            other_else.start, 
                            other_else.end, 
                            parser.ast.source);
                        parser.logger.logNote(
                            "This is the first else block declaration.", .{}, 
                            null, 
                            else_case.?.start, 
                            else_case.?.end, 
                            parser.ast.source);
                    } else {
                        else_case = parseElseCase(parser);
                    }
                    continue;
                }
                body.append(parser.allocator, parseCase(parser)) catch @panic("Out of Memory");
            }

            if (parser.skip_closing(open_brace_pos, .CloseBrace)) {
                return parser.makeExprNode(start_of_expression, .Error);
            }
            
            return parser.makeExprNode(start_of_expression, .{ 
                .Match = .{
                    .value = value,
                    .cases = body,
                    .else_case = else_case,
                } 
            });
        },

        else => {
            const node = parseList(parser);
            return node;
        },
    }
}

fn parseCase(parser: *Parser) untyped.Case {

    const pattern = parseMember(parser);

    var captures: ?untyped.Node(untyped.Expression) = null;

    if (parser.skip_if(.RightArrow)) {
        captures = parseDeclarationList(parser);
    }

    var body: untyped.Node(untyped.Statement) = undefined;

    switch (parser.current().token_type) {

        .FatRightArrow => {
            parser.skip();
            body = parseStatement(parser);
        },
        
        .OpenBrace => {
            body = parser.makeStmtNode(parser.current_index, .{ .Block = parseBlockWithNode(parser) });
        },

        else => {
            parser.LogAtToken(
                "Invalid case body.", .{}, 
                "pattern -> capture: type {} or pattern -> capture: type => value",
                .Error,
                parser.current_index);
            return untyped.Case {
                .body = parser.makeStmtNode(parser.current_index, .Error),
                .captures = captures,
                .pattern = pattern,
            };
        },
    }

    return untyped.Case {
        .body = body,
        .captures = captures,
        .pattern = pattern,
    };
}

fn parseElseCase(parser: *Parser) untyped.Node(untyped.Else) {

    const start_token_of_expression = parser.current_index;
    const start = parser.ast.tokens[start_token_of_expression].start;

    parser.skip();

    switch (parser.current().token_type) {

        .FatRightArrow => {
            parser.skip();
            const statement = parseStatement(parser);
            return untyped.Node(untyped.Else).init(parser.allocator, start, parser.ast.tokens[parser.current_index - 1].end, .{
                .body = statement,
            }) catch @panic("Out of Memory.");
        },
        
        .OpenBrace => {
            return untyped.Node(untyped.Else).init(parser.allocator, start, parser.ast.tokens[parser.current_index].end, .{
                .body = parser.makeStmtNode(parser.current_index, .{ .Block = parseBlockWithNode(parser) }),
            }) catch @panic("Out of Memory.");
        },

        else => {
            parser.LogAtToken(
                "Invalid else case body.", .{}, 
                "else {} or else => value",
                .Error,
                parser.current_index);
            return untyped.Node(untyped.Else).init(parser.allocator, start, parser.ast.tokens[parser.current_index].end, .{
                .body = parser.makeStmtNode(start_token_of_expression, .Error),
            }) catch @panic("Out of Memory.");
        },
    }
}

fn parseList(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    const lhs = parseDeclaration(parser);

    if (parser.current().token_type != .Comma) {
        return lhs;
    }

    var list = std.ArrayList(untyped.Node(untyped.Expression)).empty;
    list.append(parser.allocator, lhs) catch @panic("Out of Memory.");

    while (parser.skip_if(.Comma)) {
        list.append(parser.allocator, parseDeclaration(parser)) catch @panic("Out of Memory.");
    }

    return parser.makeExprNode(start_of_expression, .{
        .List = .{
            .expressions = list,
        }
    });
}

fn parseDeclaration(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseBinop1(parser);
    
    if (parser.skip_if(.Colon)) {

        lhs = parser.makeExprNode(start_of_expression, .{
            .Declaration = .{
                .name = lhs,
                .decl_type = parseUnary(parser),
            }
        });
    }

    return lhs;
}

// and, or
fn parseBinop1(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseBinop2(parser);
   
    outer: switch (parser.current().token_type) {

        .And, .Or => {
            const op = parser.current_index;
            parser.skip();
            lhs = parser.makeExprNode(start_of_expression, .{ 
                .Binop = .{ 
                    .left = lhs, 
                    .right = parseBinop2(parser), 
                    .op_token_index = op }});
            continue: outer parser.current().token_type;
        },

        else => {
            return lhs;
        },
    }
}

// ==, <, >, <=, >=
fn parseBinop2(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseBinop3(parser);
   
    outer: switch (parser.current().token_type) {

        .EqualsEquals, .LessThan, .GreaterThan, .LessThanOrEquals, .GreaterThanOrEquals => {
            const op = parser.current_index;
            parser.skip();
            lhs = parser.makeExprNode(start_of_expression, .{ 
                .Binop = .{ 
                    .left = lhs, 
                    .right = parseBinop3(parser), 
                    .op_token_index = op }});

            continue: outer parser.current().token_type;
        },

        else => {
            return lhs;
        },
    }
}

// +, -
fn parseBinop3(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseBinop4(parser);
   
    outer: switch (parser.current().token_type) {

        .Plus, .Minus => {
            const op = parser.current_index;
            parser.skip();
            lhs = parser.makeExprNode(start_of_expression, .{ 
                .Binop = .{ 
                    .left = lhs, 
                    .right = parseBinop4(parser), 
                    .op_token_index = op }});
            continue: outer parser.current().token_type;
        },

        else => {
            return lhs;
        },
    }
}

// *, /, %
fn parseBinop4(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseUnary(parser);

    outer: switch (parser.current().token_type) {

        .Asterisk, .Slash, .Percentage => {
            const op = parser.current_index;
            parser.skip();
            lhs = parser.makeExprNode(start_of_expression, .{ 
                .Binop = .{ 
                    .left = lhs, 
                    .right = parseUnary(parser), 
                    .op_token_index = op }});
            continue: outer parser.current().token_type;
        },

        else => {
            return lhs;
        },
    }
}

fn parseUnary(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    const op = parser.current_index;

    switch (parser.current().token_type) {

        .Minus, .Exclamation, .Reference => {
            parser.skip();
            const node = parser.makeExprNode(start_of_expression, .{
                .Unary = .{
                    .right = parseSetter(parser),
                    .op_token_index = op,
                }
            });
            
            return node;
        },

        else => {},
    }

    return parseSetter(parser);
}

fn parseSetter(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseMember(parser);

    if (parser.current().token_type == .OpenBrace) {

        lhs = parser.makeExprNode(start_of_expression, .{
            .Setter = .{
                .settee = lhs,
                .body = parseBlockWithNode(parser),
            }
        });
    }

    return lhs;
}

fn parseMember(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs: untyped.Node(untyped.Expression) = undefined;

    if (parser.skip_if(.Dot)) {
        lhs = parser.makeExprNode(start_of_expression, .{
            .ImplicitMember = parseCall(parser),
        });
    } else {
        lhs = parseCall(parser);
    }

    while (parser.skip_if(.Dot)) {

        lhs = parser.makeExprNode(start_of_expression, .{
            .Member = .{ 
                .parent = lhs,
                .child = parseCall(parser) 
            }
        });
    }
    return lhs;
}

fn parseCall(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseGeneric(parser);

    const open_pos = parser.current_index;

    if (parser.skip_if(.OpenParentheses)) {

        var args: ?untyped.Node(untyped.Expression) = null;

        if (parser.current().token_type != .CloseParentheses) {
            args = parseList(parser);
        }

        lhs = parser.makeExprNode(start_of_expression, .{
            .Call = .{
                .callee = lhs,
                .arguements = args,
            }
        });

        if (parser.skip_closing(open_pos, .CloseParentheses)) {
            return parser.makeExprNode(start_of_expression, .Error);
        }
    }

    return lhs;
}

fn parseGeneric(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseParentheisis(parser);

    const open_pos = parser.current_index;

    if (parser.skip_if(.OpenBracket)) {

        lhs = parser.makeExprNode(start_of_expression, .{
            .Generic =  .{
                .callee = lhs,
                .arguements = parseDeclarationList(parser),
            }
        });

        if (parser.skip_closing(open_pos, .CloseBracket)) {
            return parser.makeExprNode(start_of_expression, .Error);
        }
    }

    return lhs;
}

fn parseParentheisis(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    const open_pos = parser.current_index;
    
    if (parser.skip_if(.OpenParentheses)) {

        const rhs = parseList(parser);

        if (parser.skip_closing(open_pos, .CloseParentheses)) {
            return parser.makeExprNode(start_of_expression, .Error);
        }
        
        return rhs;
    }

    const node = parseBase(parser);
    return node;
}

fn parseBase(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;
    
    const node = switch (parser.current().token_type) {

        .Identifier => parser.makeExprNode(start_of_expression, .{
            .Identifier = .{
                .token_index = parser.current_index,
            }
        }),

        .Builtin => parser.makeExprNode(start_of_expression, .{
            .Builtin = .{
                .token_index = parser.current_index,
            }
        }),

        .String => parser.makeExprNode(start_of_expression, .{
            .Literal = .{
                .literal_type = parser.current().token_type,
                .token_index = parser.current_index,
            }
        }),

        .Char => parser.makeExprNode(start_of_expression, .{
            .Literal = .{
                .literal_type = parser.current().token_type,
                .token_index = parser.current_index,
            }
        }),

        .Bool => parser.makeExprNode(start_of_expression, .{
            .Literal = .{
                .literal_type = parser.current().token_type,
                .token_index = parser.current_index,
            }
        }),

        .Number => parser.makeExprNode(start_of_expression, .{
            .Literal = .{
                .literal_type = parser.current().token_type,
                .token_index = parser.current_index,
            }
        }),

        .Binary => parser.makeExprNode(start_of_expression, .{
            .Literal = .{
                .literal_type = parser.current().token_type,
                .token_index = parser.current_index,
            }
        }),

        .Self => parser.makeExprNode(start_of_expression, .Self),

        .Nothing => parser.makeExprNode(start_of_expression, .Nothing),

        .Unknown => parser.makeExprNode(start_of_expression, .Unknown),

        .Function => { 
            const node = parseFunctionPrototype(parser);
            return node;
        },

        .Object => { 
            parser.skip();

            const node = parser.makeExprNode(start_of_expression, .{
                .Object = parseBlockWithNode(parser)
            });
            
            return node;
        },

        .Enum => {
            parser.skip();
            
            const node = parser.makeExprNode(start_of_expression, .{
                .Enum = parseBlockWithNode(parser)
            });

            return node;
        },

        .Interface => {
            parser.skip();
            
            const node = parser.makeExprNode(start_of_expression, .{
                .Interface = parseBlockWithNode(parser)
            });
            
            return node;
        },

        else => {
            parser.LogAtToken(
                "Invalid Token \x22{s}\x22.", .{parser.current().token_type.toString()}, 
                null,
                .Error,
                parser.current_index);
            parser.skip();
            return parser.makeExprNode(start_of_expression, .Error);
        },
    };

    parser.skip();

    return node;
}

fn parseFunctionPrototype(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    const function_hint = "func (arg: type) return: type";

    if (parser.skip_expected(.Function, function_hint)) {
        return parser.makeExprNode(start_of_expression, .Error);
    }

    if (parser.skip_expected(.OpenParentheses, function_hint)) {
        return parser.makeExprNode(start_of_expression, .Error);
    }

    var left: ?untyped.Node(untyped.Expression) = null;

    if (parser.current().token_type != .CloseParentheses) {
        left = parseDeclarationList(parser);
    }

    if (parser.current().token_type != .CloseParentheses) {
        parser.LogAtToken(
            "Either Missing \x22)\x22 or \x22,\x22", .{}, 
            function_hint,
            .Error,
            parser.current_index);
    } else {
        parser.skip();
    }

    var returns: untyped.Node(untyped.Expression) = undefined;

    if (parser.current().token_type == .OpenBrace) {
        returns = parser.makeExprNode(start_of_expression, .Error);
        parser.LogAtToken(
            "You are required to have a return declarations", .{}, 
            function_hint,
            .Error,
            parser.current_index);
    } else {
        returns = parseDeclarationList(parser);
    }

    const node =  parser.makeExprNode(start_of_expression, .{ 
        .FuncPrototype = .{
            .arguments = left,
            .returns = returns,
        }
    });

    return node;
}

fn parseDeclarationList(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var node: untyped.Node(untyped.Expression) = undefined;
    
    switch (parser.current().token_type) {

        .Identifier => { 

            const name = parser.makeExprNode(start_of_expression, .{
                .Identifier = .{
                    .token_index = parser.current_index,
                }
            });

            parser.skip();

            if (parser.skip_expected(.Colon, "Declarations require a \x22:\x22 to set the type -> variable: type")) {
                if (parser.next().token_type == .Comma) {
                    parser.skip();
                    node = parser.makeExprNode(start_of_expression, .Error);
                } else {
                    return parser.makeExprNode(start_of_expression, .Error);
                }
            } else {
                node = parser.makeExprNode(start_of_expression, .{
                    .Declaration = .{
                        .name = name,
                        .decl_type = parseMember(parser),
                    }
                });
            }
        },

        .Nothing => {
            parser.skip();

            if (parser.current().token_type != .Comma) {
                return parser.makeExprNode(start_of_expression, .Nothing);
            }

            parser.skip();
            parser.LogAtToken(
                "Cannot have a list after nothing.", .{}, 
                "nothing, var: type => var: type",
                .Error,
                parser.current_index);
            return parser.makeExprNode(start_of_expression, .Error);
        },

        .Self => {
            parser.skip();
            node = parser.makeExprNode(start_of_expression, .Self);
        },

        .Unknown => {
            parser.skip();
            node = parser.makeExprNode(start_of_expression, .Unknown);
        },

        else => {
            parser.LogAtToken(
                "Invalid Token \x22{s}\x22 in declaration list.", .{parser.current().token_type.toString()},
                "decl1: type, decl2: type",
                .Error,
                parser.current_index);
            return parser.makeExprNode(start_of_expression, .Error);
        }
    }

    if (parser.current().token_type != .Comma) {
        return node;
    }

    var list = untyped.List {
        .expressions = .empty,
    };

    list.expressions.append(parser.allocator, node) catch @panic("Out of Memory.");

    while (parser.skip_if(.Comma)) {
        var node2: untyped.Node(untyped.Expression) = undefined;

        const start_pos = parser.current_index;
        
        switch (parser.current().token_type) {

            .Identifier => {
                 const name = parser.makeExprNode(start_pos, .{
                    .Identifier = .{
                        .token_index = parser.current_index,
                    }
                });

                parser.skip();

                if (parser.skip_expected(.Colon, "Declarations require a \x22:\x22 to set the type -> variable: type")) {
                    if (parser.next().token_type == .Comma) {
                        node2 = parser.makeExprNode(start_pos, .Error);
                    } else {
                        return parser.makeExprNode(start_pos, .Error);
                    }
                } else {
                    node2 = parser.makeExprNode(start_pos, .{
                        .Declaration = .{
                            .name = name,
                            .decl_type = parseMember(parser),
                        }
                    });
                }
            },

            .Nothing => {
                parser.LogAtToken(
                    "You cannot have a list with nothing in it", .{},
                    "var: type, nothing => var: type",
                    .Error,
                    parser.current_index);
                parser.skip();
                node2 = parser.makeExprNode(start_pos, .Error);
            },

            .Self => {
                parser.LogAtToken(
                    "Self is required to be first in a declaration list.", .{},
                    "func (self, decl2: type) return: type",
                    .Error,
                    parser.current_index);
                parser.skip();
                node2 = parser.makeExprNode(start_pos, .Error);
            },

            .Unknown => {
                parser.skip(); 
                node2 = parser.makeExprNode(start_pos, .Unknown);
            },

            else => {
                parser.LogAtToken(
                    "Invalid Token \x22{s}\x22 in declaration list.", .{parser.current().token_type.toString()},
                    "decl1: type, decl2: type",
                    .Error,
                    parser.current_index);
                return parser.makeExprNode(start_pos, .Error);
            },
        }

        list.expressions.append(parser.allocator, node2) catch @panic("Out of Memory.");
    }

    return parser.makeExprNode(start_of_expression, .{
        .List = list
    });
}