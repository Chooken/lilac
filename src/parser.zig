const std = @import("std");
const _tokens = @import("tokens.zig");
const Token = _tokens.Token;
const TokenType = _tokens.TokenType;
const Tokenizer = @import("lexer.zig").Tokenizer;
const untyped = @import("untyped.zig");
const Ast = untyped.Ast;
const logger = @import("logger.zig");
const files = @import("files.zig");

const Parser = struct { 
    ast: Ast,
    logger: logger.Logger,
    current_index: usize = 0,
    allocator: std.mem.Allocator,

    pub fn LogInvalidToken(self: *Parser, comptime fmt: []const u8, args: anytype, hint: ?[]const u8, token_index: usize) void {
        const token = self.ast.tokens[token_index];
        var log = self.logger.logError(
            "Invalid Token", .{}, 
            hint);
        log.addLine(
            self.allocator, 
            self.ast.file, 
            fmt, args, 
            token.start, 
            token.end);
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
            self.LogInvalidToken(
                "Unexpected token. Expected \x22{s}\x22 but got \x22{s}\x22 instead.", 
                .{ token_type.toString(), self.ast.source[prev_token.start..prev_token.end] },
                hint,
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
            self.LogInvalidToken(
                "Unexpected token. Expected \x22{s}\x22 but got \x22{s}\x22 instead.", 
                .{ token_type.toString(), self.ast.source[prev_token.start..prev_token.end]},
                hint,
                self.current_index);
            return true;
        }

        self.current_index += 1;
        return false;
    }

    pub fn skip_closing(self: *Parser, open_token_position: usize, token_type: TokenType) bool {
        const prev_token = self.current();

        if (prev_token.token_type != token_type) {
            self.LogInvalidToken(
                "Failed to find closing \x22{s}\x22 token.", .{ token_type.toString() }, 
                null,
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

pub fn parse(file_id: files.FileId, allocator: std.mem.Allocator, debug: bool) Ast {

    _debug = debug;

    const source = file_id.getFile().source;

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
            .file = file_id,
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

    logger.printLogs(parser.logger, allocator);

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

            const namespace = parseMember(parser, false);

            var alias: ?untyped.Node(untyped.Expression) = null;

            if (parser.skip_if(.As)) {
                alias = parseExpression(parser, false);
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
            parser.LogInvalidToken(
                "Invalid Token", .{}, 
                null,
                parser.current_index);
            parser.skip();
            return parser.makeStmtNode(parser.current_index - 1, .Error);
        },

        else => { 
            const node = parser.makeStmtNode(parser.current_index, .{
                .Expression = parseExpression(parser, true),
            });
            return node;
        },
    }
}

fn parseExpression(parser: *Parser, allow_setters: bool) untyped.Node(untyped.Expression) {
    const node = parseAssignment(parser, allow_setters);
    return node;
}

// =, -=, +=, *=, /=, %=
fn parseAssignment(parser: *Parser, allow_setters: bool) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    const lhs = parseList(parser, allow_setters);
    
    switch (parser.current().token_type) {

        .Equals, .PlusEquals, .MinusEquals, .TimesEquals, .DivEquals, .PercentEquals => {

            const op = parser.current_index;

            parser.skip();

            const value = parseList(parser, allow_setters);

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

fn parseList(parser: *Parser, allow_setters: bool) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    const lhs = parseDeclaration(parser, allow_setters);

    if (parser.current().token_type != .Comma) {
        return lhs;
    }

    var list = std.ArrayList(untyped.Node(untyped.Expression)).empty;
    list.append(parser.allocator, lhs) catch @panic("Out of Memory.");

    while (parser.skip_if(.Comma)) {
        list.append(parser.allocator, parseDeclaration(parser, allow_setters)) catch @panic("Out of Memory.");
    }

    return parser.makeExprNode(start_of_expression, .{
        .List = .{
            .expressions = list,
        }
    });
}

fn parseDeclaration(parser: *Parser, allow_setters: bool) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseBinop1(parser, allow_setters);
    
    if (parser.skip_if(.Colon)) {

        lhs = parser.makeExprNode(start_of_expression, .{
            .Declaration = .{
                .name = lhs,
                .decl_type = parseUnary(parser, allow_setters),
            }
        });
    }

    return lhs;
}

// and, or
fn parseBinop1(parser: *Parser, allow_setters: bool) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseBinop2(parser, allow_setters);
   
    outer: switch (parser.current().token_type) {

        .And, .Or => {
            const op = parser.current_index;
            parser.skip();
            lhs = parser.makeExprNode(start_of_expression, .{ 
                .Binop = .{ 
                    .left = lhs, 
                    .right = parseBinop2(parser, allow_setters), 
                    .op_token_index = op }});
            continue: outer parser.current().token_type;
        },

        else => {
            return lhs;
        },
    }
}

// ==, <, >, <=, >=
fn parseBinop2(parser: *Parser, allow_setters: bool) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseBinop3(parser, allow_setters);
   
    outer: switch (parser.current().token_type) {

        .EqualsEquals, .LessThan, .GreaterThan, .LessThanOrEquals, .GreaterThanOrEquals => {
            const op = parser.current_index;
            parser.skip();
            lhs = parser.makeExprNode(start_of_expression, .{ 
                .Binop = .{ 
                    .left = lhs, 
                    .right = parseBinop3(parser, allow_setters), 
                    .op_token_index = op }});

            continue: outer parser.current().token_type;
        },

        else => {
            return lhs;
        },
    }
}

// +, -
fn parseBinop3(parser: *Parser, allow_setters: bool) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseBinop4(parser, allow_setters);
   
    outer: switch (parser.current().token_type) {

        .Plus, .Minus => {
            const op = parser.current_index;
            parser.skip();
            lhs = parser.makeExprNode(start_of_expression, .{ 
                .Binop = .{ 
                    .left = lhs, 
                    .right = parseBinop4(parser, allow_setters), 
                    .op_token_index = op }});
            continue: outer parser.current().token_type;
        },

        else => {
            return lhs;
        },
    }
}

// *, /, %
fn parseBinop4(parser: *Parser, allow_setters: bool) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseUnary(parser, allow_setters);

    outer: switch (parser.current().token_type) {

        .Asterisk, .Slash, .Percentage => {
            const op = parser.current_index;
            parser.skip();
            lhs = parser.makeExprNode(start_of_expression, .{ 
                .Binop = .{ 
                    .left = lhs, 
                    .right = parseUnary(parser, allow_setters), 
                    .op_token_index = op }});
            continue: outer parser.current().token_type;
        },

        else => {
            return lhs;
        },
    }
}

fn parseUnary(parser: *Parser, allow_setters: bool) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    const op = parser.current_index;

    switch (parser.current().token_type) {

        .Minus, .Exclamation, .Reference => {
            parser.skip();
            const node = parser.makeExprNode(start_of_expression, .{
                .Unary = .{
                    .right = parseSetter(parser,allow_setters),
                    .op_token_index = op,
                }
            });
            
            return node;
        },

        else => {},
    }

    return parseSetter(parser, allow_setters);
}

fn parseSetter(parser: *Parser, allow_setters: bool) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseMember(parser, allow_setters);

    if (allow_setters and parser.current().token_type == .OpenBrace) {

        lhs = parser.makeExprNode(start_of_expression, .{
            .Setter = .{
                .settee = lhs,
                .body = parseBlockWithNode(parser),
            }
        });
    }

    return lhs;
}

fn parseMember(parser: *Parser, allow_setters: bool) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs: untyped.Node(untyped.Expression) = undefined;

    if (parser.skip_if(.Dot)) {
        lhs = parser.makeExprNode(start_of_expression, .{
            .ImplicitMember = parseCall(parser, allow_setters),
        });
    } else {
        lhs = parseCall(parser, allow_setters);
    }

    while (parser.skip_if(.Dot)) {

        lhs = parser.makeExprNode(start_of_expression, .{
            .Member = .{ 
                .parent = lhs,
                .child = parseCall(parser, allow_setters) 
            }
        });
    }
    return lhs;
}

fn parseCall(parser: *Parser, allow_setters: bool) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseGeneric(parser, allow_setters);

    const open_pos = parser.current_index;

    if (parser.skip_if(.OpenParentheses)) {

        var args: ?untyped.Node(untyped.Expression) = null;

        if (parser.current().token_type != .CloseParentheses) {
            args = parseList(parser, true);
        }

        if (parser.skip_closing(open_pos, .CloseParentheses)) {
            return parser.makeExprNode(start_of_expression, .Error);
        }

        lhs = parser.makeExprNode(start_of_expression, .{
            .Call = .{
                .callee = lhs,
                .arguements = args,
            }
        });
    }

    return lhs;
}

fn parseGeneric(parser: *Parser, allow_setters: bool) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    var lhs = parseParentheisis(parser, allow_setters);

    const open_pos = parser.current_index;

    if (parser.skip_if(.OpenBracket)) {

        const args = parseList(parser, false);

        if (parser.skip_closing(open_pos, .CloseBracket)) {
            return parser.makeExprNode(start_of_expression, .Error);
        }

        lhs = parser.makeExprNode(start_of_expression, .{
            .Generic =  .{
                .callee = lhs,
                .arguements = args,
            }
        });
    }

    return lhs;
}

fn parseParentheisis(parser: *Parser, allow_setters: bool) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    const open_pos = parser.current_index;
    
    if (parser.skip_if(.OpenParentheses)) {

        const rhs = parseBinop1(parser, allow_setters);

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
    
    const expr: untyped.Expression = switch (parser.current().token_type) {

        .Identifier => .{
            .Identifier = .{
                .token_index = parser.current_index,
            }
        },

        .Builtin => .{
            .Builtin = .{
                .token_index = parser.current_index,
            }
        },

        .String => .{
            .Literal = .{
                .literal_type = parser.current().token_type,
                .token_index = parser.current_index,
            }
        },

        .Char => .{
            .Literal = .{
                .literal_type = parser.current().token_type,
                .token_index = parser.current_index,
            }
        },

        .Bool => .{
            .Literal = .{
                .literal_type = parser.current().token_type,
                .token_index = parser.current_index,
            }
        },

        .Number => .{
            .Literal = .{
                .literal_type = parser.current().token_type,
                .token_index = parser.current_index,
            }
        },

        .Binary => .{
            .Literal = .{
                .literal_type = parser.current().token_type,
                .token_index = parser.current_index,
            }
        },

        .Self => .Self,

        .Nothing => .Nothing,

        .Unknown => .Unknown,

        .Function => { 
            const proto = parseFunctionPrototype(parser);
            var is_inline: bool = undefined;
            var body: untyped.Node(untyped.Statement) = undefined;

            switch (parser.current().token_type) {

                .OpenBrace => {
                    is_inline = false;
                    body = parser.makeStmtNode(parser.current_index, .{
                        .Block = parseBlockWithNode(parser)
                    }); 
                },

                .FatRightArrow => {
                    parser.skip();
                    is_inline = true;
                    body = parseStatement(parser); 
                },

                else => return proto,
            }

            return parser.makeExprNode(start_of_expression, .{
                .Function = .{
                    .prototype = proto,
                    .body = body,
                    .is_inline = is_inline,
                }
            });
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

        .If => {
            parser.skip();

            var condition: untyped.Node(untyped.Expression) = undefined;
            
            if (parser.current().token_type == .OpenBrace) {
                condition = parser.makeExprNode(start_of_expression, .Error);
                parser.LogInvalidToken(
                    "You forgot the if condition.", .{}, 
                    "if condition {}",
                    parser.current_index - 1);
            } else {
                condition = parseBinop1(parser, false);
            }

            var captures: ?untyped.Node(untyped.Expression) = null;

            if (parser.skip_if(.RightArrow)) {

                captures = parseParamList(parser);
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
                parser.LogInvalidToken(
                    "You forgot the matches value.", .{}, 
                    "match value {}",
                    parser.current_index);
            } else {
                value = parseMember(parser, false);
            }

            var body = std.ArrayList(untyped.Case).empty;
            var else_case: ?untyped.Node(untyped.Else) = null;

            const open_brace_pos = parser.current_index;

            if (parser.skip_expected(.OpenBrace, "match value {}")) {
                return parser.makeExprNode(start_of_expression, .Error);
            }

            while (parser.current().token_type != .CloseBrace and !parser.eof()) {
                if (parser.current().token_type == .Else) {
                    if (else_case) |else_| {
                        const other_else = parseElseCase(parser);

                        var log = parser.logger.logError(
                            "Invalid Match", .{}, 
                            "Try removing or merging your else blocks.");
                        log.addLine(
                            parser.allocator, 
                            parser.ast.file, 
                            "This is the first else block declaration.", .{}, 
                            else_.start, 
                            else_.end);
                        log.addLine(
                            parser.allocator, 
                            parser.ast.file, 
                            "Else has been declared twice in match expression.", .{}, 
                            other_else.start, 
                            other_else.end);
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
            parser.LogInvalidToken(
                "Invalid Token \x22{s}\x22.", .{parser.current().token_type.toString()}, 
                null,
                parser.current_index);
            parser.skip();
            return parser.makeExprNode(start_of_expression, .Error);
        },
    };

    parser.skip();

    return parser.makeExprNode(start_of_expression, expr);
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
        left = parseParamList(parser);
    }

    if (parser.current().token_type != .CloseParentheses) {
        parser.LogInvalidToken(
            "Either Missing \x22)\x22 or \x22,\x22", .{}, 
            function_hint,
            parser.current_index);
    } else {
        parser.skip();
    }

    var returns: untyped.Node(untyped.Expression) = undefined;

    if (parser.current().token_type == .OpenBrace) {
        returns = parser.makeExprNode(start_of_expression, .Error);
        parser.LogInvalidToken(
            "You are required to have a return declarations", .{}, 
            function_hint,
            parser.current_index);
    } else {
        returns = parseParamList(parser);
    }

    const node =  parser.makeExprNode(start_of_expression, .{ 
        .FuncPrototype = .{
            .arguments = left,
            .returns = returns,
        }
    });

    return node;
}

fn parseParamList(parser: *Parser) untyped.Node(untyped.Expression) {

    const start_of_expression = parser.current_index;

    const node: untyped.Node(untyped.Expression) = parseExpression(parser, false);

    if (parser.current().token_type != .Comma) {
        return node;
    }

    var list = untyped.List {
        .expressions = .empty,
    };

    list.expressions.append(parser.allocator, node) catch @panic("Out of Memory.");

    while (parser.skip_if(.Comma)) {
        const node2: untyped.Node(untyped.Expression) = parseUnary(parser, false);
        list.expressions.append(parser.allocator, node2) catch @panic("Out of Memory.");
    }

    return parser.makeExprNode(start_of_expression, .{
        .List = list
    });
}

fn parseCase(parser: *Parser) untyped.Case {

    const pattern = parseMember(parser, false);

    var captures: ?untyped.Node(untyped.Expression) = null;

    if (parser.skip_if(.RightArrow)) {
        captures = parseParamList(parser);
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
            parser.LogInvalidToken(
                "Invalid case body.", .{}, 
                "pattern -> capture: type {} or pattern -> capture: type => value",
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
            parser.LogInvalidToken(
                "Invalid else case body.", .{}, 
                "else {} or else => value",
                parser.current_index);
            return untyped.Node(untyped.Else).init(parser.allocator, start, parser.ast.tokens[parser.current_index].end, .{
                .body = parser.makeStmtNode(start_token_of_expression, .Error),
            }) catch @panic("Out of Memory.");
        },
    }
}