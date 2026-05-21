const std = @import("std");
const _tokens = @import("tokens.zig");
const Token = _tokens.Token;
const TokenType = _tokens.TokenType;
const Tokenizer = @import("lexer.zig").Tokenizer;
const ast = @import("ast.zig");
const Ast = ast.Ast;
const logger = @import("logger.zig");

const Parser = struct { 
    ast: Ast,
    logs: std.ArrayList(logger.Log),
    current_index: usize = 0,
    allocator: std.mem.Allocator,

    pub fn AddErrorAt(self: *Parser, comptime fmt: []const u8, args: anytype, hint: ?[]const u8, token_index: usize) void {
        const message = std.fmt.allocPrint(self.allocator, fmt, args) catch return;

        const token = self.ast.tokens[token_index];
        
        self.logs.append(self.allocator, .{
            .start = token.start,
            .end = token.end,
            .source = self.ast.source,
            .message = message,
            .hint = hint,
            .level = .Error,
        }) catch @panic("Out of Memory.");
    }

    pub fn AddError(self: *Parser, comptime fmt: []const u8, args: anytype, hint: ?[]const u8) void {
        self.AddErrorAt(fmt, args, hint, self.current_index);
    }

    pub fn AddNoteAt(self: *Parser, comptime fmt: []const u8, args: anytype, hint: ?[]const u8, token_index: usize) void {
        const message = std.fmt.allocPrint(self.allocator, fmt, args) catch return;

        const token = self.ast.tokens[token_index];
        
        self.logs.append(self.allocator, .{
            .start = token.start,
            .end = token.end,
            .source = self.ast.source,
            .message = message,
            .hint = hint,
            .level = .Note,
        }) catch @panic("Out of Memory.");
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
            self.AddError(
                "Unexpected token. Expected \x22{s}\x22 but got \x22{s}\x22 instead.", 
                .{ token_type.toString(), self.ast.source[prev_token.start..prev_token.end] },
                hint);
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
            self.AddError(
                "Unexpected token. Expected \x22{s}\x22 but got \x22{s}\x22 instead.", 
                .{ token_type.toString(), self.ast.source[prev_token.start..prev_token.end]},
                hint);
            return true;
        }

        self.current_index += 1;
        return false;
    }

    pub fn skip_closing(self: *Parser, open_token_position: usize, token_type: TokenType) bool {
        const prev_token = self.current();

        if (prev_token.token_type != token_type) {
            self.AddErrorAt("Failed to find closing \x22{s}\x22 token.", .{ token_type.toString() }, null, open_token_position);
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

    pub fn makeStmtNode(self: *Parser, start_token: usize, expression: ast.Statement) ast.Node(ast.Statement) {
        return ast.Node(ast.Statement).init(
            self.allocator, 
            start_token,
            self.current_index - 1, 
            expression) catch @panic("Out of Memory.");
    }

    pub fn makeExprNode(self: *Parser, start_token: usize, expression: ast.Expression) ast.Node(ast.Expression) {
        return ast.Node(ast.Expression).init(
            self.allocator, 
            start_token,
            self.current_index - 1, 
            expression) catch @panic("Out of Memory.");
    }
};

var _debug: bool = false;

pub fn parse(source: [:0]const u8, allocator: std.mem.Allocator, debug: bool) !Ast {

    _debug = debug;

    var tokens: std.ArrayList(Token) = .empty;
    defer tokens.deinit(allocator);

    var tokenizer = Tokenizer.init(source);

    
    while (tokenizer.next()) |token| {
        try tokens.append(allocator, token);
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
            .tokens = try tokens.toOwnedSlice(allocator),
            .root_block = undefined,
        },
        .logs = .empty,
        .allocator = allocator,
    };
    defer parser.logs.deinit(allocator);

    parser.ast.root_block = parseTopBlock(&parser);

    logger.printLogs(parser.logs);

    return parser.ast;
}

fn parseTopBlock(parser: *Parser) ast.Block {
    Enter(parser, "TopBlock");
    var body = std.ArrayList(ast.Node(ast.Statement)).empty;

    while (!parser.eof()) {
        body.append(parser.allocator, parseStatement(parser)) catch @panic("Out of Memory");
    }

    Exit(false);
    return ast.Block {
        .body = body
    };
}

fn parseBlockWithNode(parser: *Parser) ast.Node(ast.Block) {
    const start = parser.current_index;
    const block = parseBlock(parser);
    return ast.Node(ast.Block).init(parser.allocator, start, parser.current_index, block) catch @panic("Out of Memory.");
}

fn parseBlock(parser: *Parser) ast.Block {

    Enter(parser, "Block");
    var body = std.ArrayList(ast.Node(ast.Statement)).empty;

    const open_brace_pos = parser.current_index;

    if (parser.skip_expected(.OpenBrace, null)) {
        Exit(true);
        return ast.Block {
            .body = body
        };
    }

    while (parser.current().token_type != .CloseBrace and !parser.eof()) {
        body.append(parser.allocator, parseStatement(parser)) catch @panic("Out of Memory");
    }

    if (parser.skip_closing(open_brace_pos, .CloseBrace)) {
        Exit(true);
        return ast.Block {
            .body = body
        };
    }
    
    Exit(false);
    return ast.Block {
        .body = body
    };
}

fn parseStatement(parser: *Parser) ast.Node(ast.Statement) {

    Enter(parser, "Statement");

    switch (parser.current().token_type) {

        .Using => {

            parser.skip();

            const namespace = parseMember(parser);

            var alias: ?ast.Node(ast.Expression) = null;

            if (parser.skip_if(.As)) {
                alias = parseExpression(parser);
            }

            Exit(false);
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
            Exit(false);
            return node;
        },

        .OpenBrace => {
            const node = parser.makeStmtNode(parser.current_index,.{
                .Block = parseBlockWithNode(parser),
            });
            Exit(false);
            return node;
        },

        .Loop => {
            parser.skip();

            const node = parser.makeStmtNode(parser.current_index - 1, .{
                .Loop = parser.makeStmtNode(parser.current_index, .{
                    .Block = parseBlockWithNode(parser),
                }),
            });
            Exit(false);
            return node;
        },

        .Return => { 
            parser.skip();
            Exit(false);
            return parser.makeStmtNode(parser.current_index - 1, .Return);
        },

        .Break => { 
            parser.skip();
            Exit(false);
            return parser.makeStmtNode(parser.current_index - 1, .Break);
        },

        .Continue => { 
            parser.skip();
            Exit(false);
            return parser.makeStmtNode(parser.current_index - 1, .Continue);
        },

        .Invalid => {
            parser.AddError("Invalid Token", .{}, null);
            parser.skip();
            Exit(true);
            return parser.makeStmtNode(parser.current_index - 1, .Error);
        },

        else => { 
            const node = parser.makeStmtNode(parser.current_index, .{
                .Expression = parseExpression(parser),
            });
            Exit(false);
            return node;
        },
    }
}

fn parseExpression(parser: *Parser) ast.Node(ast.Expression) {
    Enter(parser, "Expression");
    const node = parseAssignment(parser);
    Exit(false);
    return node;
}

// =, -=, +=, *=, /=, %=, =>
fn parseAssignment(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Assignment");

    const start_of_expression = parser.current_index;

    const lhs = parseConditionals(parser);
    
    switch (parser.current().token_type) {

        .Equals, .PlusEquals, .MinusEquals, .TimesEquals, .DivEquals, .PercentEquals, .FatRightArrow => {

            const op = parser.current_index;

            parser.skip();

            const value = parseConditionals(parser);

            Exit(false);
            return parser.makeExprNode(start_of_expression, .{
                .Assignment = .{
                    .assignee = lhs,
                    .value = value,
                    .op_token_index = op,
                }
            });
        },

        else => {
            Exit(false);
            return lhs;
        },
    }
}

fn parseConditionals(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Conditionals");

    const start_of_expression = parser.current_index;
    
    switch (parser.current().token_type) {

        .If => {
            parser.skip();

            var condition: ast.Node(ast.Expression) = undefined;
            
            if (parser.current().token_type == .OpenBrace) {
                condition = parser.makeExprNode(start_of_expression, .Error);
                parser.AddErrorAt(
                    "You forgot the if condition.", .{}, 
                    "if condition {}",
                    parser.current_index - 1);
            } else {
                condition = parseBinop1(parser);
            }

            var captures: ?ast.Node(ast.Expression) = null;

            if (parser.skip_if(.RightArrow)) {

                captures = parseDeclarationList(parser);
            }

            const body: ast.Node(ast.Statement) = parseStatement(parser);

            var else_body: ?ast.Node(ast.Else) = null;

            if (parser.skip_if(.Else)) {
                else_body = ast.Node(ast.Else).init(
                    parser.allocator, 
                    start_of_expression, 
                    parser.current_index, 
                    .{
                        .body = parseStatement(parser),
                    }
                ) catch @panic("Out of Memory.");
            }

            Exit(false);
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

            var value: ast.Node(ast.Expression) = undefined;

            if (parser.current().token_type == .OpenBrace) {
                value = parser.makeExprNode(start_of_expression, .Error);
                parser.AddErrorAt(
                    "You forgot the matches value.", .{}, 
                    "match value {}",
                    parser.current_index - 1);
            } else {
                value = parseMember(parser);
            }

            var body = std.ArrayList(ast.Case).empty;
            var else_case: ?ast.Node(ast.Else) = null;

            const open_brace_pos = parser.current_index;

            if (parser.skip_expected(.OpenBrace, "match value {}")) {
                Exit(true);
                return parser.makeExprNode(start_of_expression, .Error);
            }

            while (parser.current().token_type != .CloseBrace and !parser.eof()) {
                if (parser.current().token_type == .Else) {
                    if (else_case != null) {
                        parser.AddError(
                            "Else has been declared twice in match expression.", .{}, 
                            "Try removing or merging your else blocks.");
                        _ = parseElseCase(parser);
                        parser.AddNoteAt(
                            "This is the first else block declaration.", .{}, 
                            null, else_case.?.start_token);
                    } else {
                        else_case = parseElseCase(parser);
                    }
                    continue;
                }
                body.append(parser.allocator, parseCase(parser)) catch @panic("Out of Memory");
            }

            if (parser.skip_closing(open_brace_pos, .CloseBrace)) {
                Exit(true);
                return parser.makeExprNode(start_of_expression, .Error);
            }
            
            Exit(false);
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
            Exit(false);
            return node;
        },
    }
}

fn parseCase(parser: *Parser) ast.Case {
    Enter(parser, "Case");

    const pattern = parseMember(parser);

    var captures: ?ast.Node(ast.Expression) = null;

    if (parser.skip_if(.RightArrow)) {
        captures = parseDeclarationList(parser);
    }

    var body: ast.Node(ast.Statement) = undefined;

    switch (parser.current().token_type) {

        .FatRightArrow => {
            parser.skip();
            body = parseStatement(parser);
        },
        
        .OpenBrace => {
            body = parser.makeStmtNode(parser.current_index, .{ .Block = parseBlockWithNode(parser) });
        },

        else => {
            parser.AddError(
                "Invalid case body.", .{}, 
                "pattern -> capture: type {} or pattern -> capture: type => value");
            Exit(true);
            return ast.Case {
                .body = parser.makeStmtNode(parser.current_index, .Error),
                .captures = captures,
                .pattern = pattern,
            };
        },
    }

    return ast.Case {
        .body = body,
        .captures = captures,
        .pattern = pattern,
    };
}

fn parseElseCase(parser: *Parser) ast.Node(ast.Else) {

    const start_of_expression = parser.current_index;

    parser.skip();

    switch (parser.current().token_type) {

        .FatRightArrow => {
            parser.skip();
            const statement = parseStatement(parser);
            return ast.Node(ast.Else).init(parser.allocator, start_of_expression, parser.current_index, .{
                .body = statement,
            }) catch @panic("Out of Memory.");
        },
        
        .OpenBrace => {
            return ast.Node(ast.Else).init(parser.allocator, start_of_expression, parser.current_index, .{
                .body = parser.makeStmtNode(parser.current_index, .{ .Block = parseBlockWithNode(parser) }),
            }) catch @panic("Out of Memory.");
        },

        else => {
            parser.AddError(
                "Invalid else case body.", .{}, 
                "else {} or else => value");
            Exit(true);
            return ast.Node(ast.Else).init(parser.allocator, start_of_expression, parser.current_index, .{
                .body = parser.makeStmtNode(start_of_expression, .Error),
            }) catch @panic("Out of Memory.");
        },
    }
}

fn parseList(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "List");

    const start_of_expression = parser.current_index;

    const lhs = parseDeclaration(parser);

    if (parser.current().token_type != .Comma) {
        Exit(false);
        return lhs;
    }

    var list = std.ArrayList(ast.Node(ast.Expression)).empty;
    list.append(parser.allocator, lhs) catch @panic("Out of Memory.");

    while (parser.skip_if(.Comma)) {
        list.append(parser.allocator, parseDeclaration(parser)) catch @panic("Out of Memory.");
    }

    Exit(false);
    return parser.makeExprNode(start_of_expression, .{
        .List = .{
            .expressions = list,
        }
    });
}

fn parseDeclaration(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Declaration");

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

    Exit(false);
    return lhs;
}

// and, or
fn parseBinop1(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Binop 1");

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
            Exit(false);
            return lhs;
        },
    }
}

// ==, <, >, <=, >=
fn parseBinop2(parser: *Parser) ast.Node(ast.Expression) {
    
    Enter(parser, "Binop 2");

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
            Exit(false);
            return lhs;
        },
    }
}

// +, -
fn parseBinop3(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Binop 3");

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
            Exit(false);
            return lhs;
        },
    }
}

// *, /, %
fn parseBinop4(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Binop 4");

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
            Exit(false);
            return lhs;
        },
    }
}

fn parseUnary(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Unary");

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
            
            Exit(false);
            return node;
        },

        else => {},
    }

    Exit(false);
    return parseSetter(parser);
}

fn parseSetter(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Setter");

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

    Exit(false);
    return lhs;
}

fn parseMember(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Member");

    const start_of_expression = parser.current_index;

    var lhs: ast.Node(ast.Expression) = undefined;

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

    Exit(false);
    return lhs;
}

fn parseCall(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Call");

    const start_of_expression = parser.current_index;

    var lhs = parseGeneric(parser);

    const open_pos = parser.current_index;

    if (parser.skip_if(.OpenParentheses)) {

        var args: ?ast.Node(ast.Expression) = null;

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
            Exit(true);
            return parser.makeExprNode(start_of_expression, .Error);
        }
    }

    Exit(false);
    return lhs;
}

fn parseGeneric(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Generic");

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
            Exit(true);
            return parser.makeExprNode(start_of_expression, .Error);
        }
    }

    Exit(false);
    return lhs;
}

fn parseParentheisis(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Parentheisis");

    const start_of_expression = parser.current_index;

    const open_pos = parser.current_index;
    
    if (parser.skip_if(.OpenParentheses)) {

        const rhs = parseList(parser);

        if (parser.skip_closing(open_pos, .CloseParentheses)) {
            Exit(true);
            return parser.makeExprNode(start_of_expression, .Error);
        }
        
        Exit(false);
        return rhs;
    }

    const node = parseBase(parser);
    Exit(false);
    return node;
}

fn parseBase(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Base");

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
            Exit(false);
            return node;
        },

        .Object => { 
            parser.skip();

            const node = parser.makeExprNode(start_of_expression, .{
                .Object = parseBlockWithNode(parser)
            });
            
            Exit(false);
            return node;
        },

        .Enum => {
            parser.skip();
            
            const node = parser.makeExprNode(start_of_expression, .{
                .Enum = parseBlockWithNode(parser)
            });

            Exit(false);
            return node;
        },

        .Interface => {
            parser.skip();
            
            const node = parser.makeExprNode(start_of_expression, .{
                .Interface = parseBlockWithNode(parser)
            });
            
            Exit(false);
            return node;
        },

        else => {
            Exit(true);
            parser.AddError("Invalid Token \x22{s}\x22.", .{parser.current().token_type.toString()}, null);
            parser.skip();
            return parser.makeExprNode(start_of_expression, .Error);
        },
    };

    Exit(false);
    parser.skip();

    return node;
}

fn parseFunctionPrototype(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Function Proto");

    const start_of_expression = parser.current_index;

    const function_hint = "func (arg: type) return: type";

    if (parser.skip_expected(.Function, function_hint)) {
        Exit(true);
        return parser.makeExprNode(start_of_expression, .Error);
    }

    if (parser.skip_expected(.OpenParentheses, function_hint)) {
        Exit(true);
        return parser.makeExprNode(start_of_expression, .Error);
    }

    var left: ?ast.Node(ast.Expression) = null;

    if (parser.current().token_type != .CloseParentheses) {
        left = parseDeclarationList(parser);
    }

    if (parser.current().token_type != .CloseParentheses) {
        parser.AddError(
            "Either Missing \x22)\x22 or \x22,\x22", .{}, 
            function_hint);
    } else {
        parser.skip();
    }

    var returns: ast.Node(ast.Expression) = undefined;

    if (parser.current().token_type == .OpenBrace) {
        returns = parser.makeExprNode(start_of_expression, .Error);
        parser.AddError(
            "You are required to have a return declarations", .{}, 
            function_hint);
    } else {
        returns = parseDeclarationList(parser);
    }

    const node =  parser.makeExprNode(start_of_expression, .{ 
        .FuncPrototype = .{
            .arguments = left,
            .returns = returns,
        }
    });

    Exit(false);
    return node;
}

fn parseDeclarationList(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Declaration List");

    const start_of_expression = parser.current_index;

    var node: ast.Node(ast.Expression) = undefined;
    
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
                    Exit(true);
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
            parser.AddError(
                "Cannot have a list after nothing.", 
                .{}, 
                "nothing, var: type => var: type");
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
            Exit(true);
            parser.AddError(
                "Invalid Token \x22{s}\x22 in declaration list.", 
                .{parser.current().token_type.toString()},
                "decl1: type, decl2: type");
            return parser.makeExprNode(start_of_expression, .Error);
        }
    }

    if (parser.current().token_type != .Comma) {
        Exit(false);
        return node;
    }

    var list = ast.List {
        .expressions = .empty,
    };

    list.expressions.append(parser.allocator, node) catch @panic("Out of Memory.");

    while (parser.skip_if(.Comma)) {
        var node2: ast.Node(ast.Expression) = undefined;

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
                        Exit(true);
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
                parser.AddError(
                    "You cannot have a list with nothing in it", 
                    .{},
                    "var: type, nothing => var: type");
                parser.skip();
                node2 = parser.makeExprNode(start_pos, .Error);
            },

            .Self => {
                Exit(true);
                parser.AddError(
                    "Self is required to be first in a declaration list.", 
                    .{},
                    "func (self, decl2: type) return: type");
                parser.skip();
                node2 = parser.makeExprNode(start_pos, .Error);
            },

            .Unknown => {
                parser.skip(); 
                node2 = parser.makeExprNode(start_pos, .Unknown);
            },

            else => {
                Exit(true);
                parser.AddError(
                    "Invalid Token \x22{s}\x22 in declaration list.", 
                    .{parser.current().token_type.toString()},
                    "decl1: type, decl2: type");
                return parser.makeExprNode(start_pos, .Error);
            },
        }

        list.expressions.append(parser.allocator, node2) catch @panic("Out of Memory.");
    }

    Exit(false);
    return parser.makeExprNode(start_of_expression, .{
        .List = list
    });
}

var depth: usize = 0;

fn Enter(parser: *Parser, string: []const u8) void {

    if (!_debug) {
        return;
    }

    depth += 1;

    for (0..depth) |_| {
        std.debug.print("| ", .{});
    }

    std.debug.print("{s} - token: \x22{s}\x22 = {s}\n", .{string, parser.current().token_type.toString(), parser.ast.source[parser.current().start..parser.current().end]});
}

fn Exit(is_error: bool) void {

    if (!_debug) {
        return;
    }

    if (is_error) {
        for (0..depth) |_| {
            std.debug.print("| ", .{});
        }
        std.debug.print("Failed!!!!!!!!!!!\n", .{});
    }
    
    depth -= 1;
}