const std = @import("std");
const _tokens = @import("tokens.zig");
const Token = _tokens.Token;
const TokenType = _tokens.TokenType;
const Tokenizer = @import("lexer.zig").Tokenizer;
const ast = @import("ast.zig");
const Ast = ast.Ast;

const Parser = struct { 
    ast: Ast,
    current_index: usize = 0,
    allocator: std.mem.Allocator,

    pub fn AddError(self: *Parser, fmt: []const u8, args: anytype) void {
        const message = std.fmt.allocPrint(self.allocator, fmt, args) catch return;
        
        self.ast.errors.append(self.allocator, .{
            .token_index = self.current_index,
            .message = message,
        });
    }
    
    pub fn current(self: *Parser) Token {
        return self.ast.tokens[self.position];
    }

    pub fn next(self: *Parser) Token {
        return self.ast.tokens[self.position + 1];
    }

    pub fn eat(self: *Parser) Token {
        self.position += 1;
        return self.ast.tokens[self.position - 1];
    }

    pub fn eat_expected(self: *Parser, token_type: TokenType) ?Token {
        const prev_token = self.current();

        if (prev_token.token_type != token_type) {
            self.AddError("Unexpected token. Expected {s} but got {s} instead", .{ @tagName(token_type), prev_token.string }, prev_token.line_number);
            return null;
        }

        self.position += 1;
        return prev_token;
    }

    pub fn eat_if(self: *Parser, token_type: TokenType) ?Token {
        const prev_token = self.current();

        if (prev_token.token_type != token_type) return null;

        self.position += 1;
        return prev_token;
    }

    pub fn skip(self: *Parser) void {
        self.position += 1;
    }

    pub fn skip_expected(self: *Parser, token_type: TokenType) bool {
        const prev_token = self.current();

        if (prev_token.token_type != token_type) {
            self.AddError("Unexpected token. Expected {s} but got {s} instead", .{ @tagName(token_type), prev_token.string }, prev_token.line_number);
            return false;
        }

        self.position += 1;
        return true;
    }

    pub fn skip_if(self: *Parser, token_type: TokenType) bool {
        if (self.current().token_type != token_type) return false;

        self.position += 1;
        return true;
    }

    pub fn eof(self: *Parser) void {
        return self.ast.tokens.len <= self.current_index;
    }

    pub fn makeStmtNode(self: *Parser, expression: ast.Statement) ast.Node(ast.Statement) {
        return ast.Node(ast.Statement).init(
            self.allocator, 
            self.current_index, 
            expression);
    }

    pub fn makeExprNode(self: *Parser, expression: ast.Expression) ast.Node(ast.Expression) {
        return ast.Node(ast.Expression).init(
            self.allocator, 
            self.current_index, 
            expression);
    }
};

pub fn parse(source: [:0]const u8, allocator: std.mem.Allocator) !Ast {

    var tokens: std.ArrayList(Token) = .{ };
    defer tokens.deinit(allocator);

    var token: Token = undefined;
    var tokenizer = Tokenizer.init(source);

    std.debug.print("-- Tokens --\n", .{});
    while (token.not_eof()) {
        token = tokenizer.next();
        try tokens.append(allocator, token);
        std.debug.print("{s}\n", .{@tagName(token.token_type)});
    }

    var parser = Parser { 
        .ast = Ast {
            .source = source,
            .tokens = try tokens.toOwnedSlice(allocator),
            .errors = .{},
            .root_block = undefined,
        },
        .allocator = allocator,
    };

    parser.ast.root_block = parseBlock(&parser);

    return parser.ast;
}

fn parseTopBlock(parser: *Parser) ast.Block {
    const body = std.ArrayList(ast.Node(ast.Expression)) {};

    while (!parser.eof()) {
        body.append(parser.allocator, parseExpression(parser)) catch @panic("Out of Memory");
    }

    return body;
}

fn parseBlock(parser: *Parser) ?ast.Block {

    if (parser.skip_expected(.OpenBrace)) {

        const body = std.ArrayList(ast.Node(ast.Expression)) {};

        while (parser.current().token_type == .CloseBrace or !parser.eof()) {
            body.append(parser.allocator, parseExpression(parser)) catch @panic("Out of Memory");
        }

        _ = parser.skip_expected(.CloseBrace);

        return body;
    }
    
    return null;
}

fn parseStatement(parser: *Parser) ast.Node(ast.Statement) {

}

fn parseExpression(parser: *Parser) ast.Node(ast.Expression) {
    return parseAssignment(parser);
}

fn parseAssignment(parser: *Parser) ast.Node(ast.Expression) {
    const lhs = parseConditionals(parser);
    
    switch (parser.current().token_type) {

        .Equals => {

            const op = parser.current_index;

            return parser.makeExprNode(.{
                .Assignment = .{
                    .assignee = lhs,
                    .value = parseConditionals(parser),
                    .op_token_index = op,
                }
            });
        },

        else => return lhs,
    }
}

fn parseConditionals(parser: *Parser) ast.Node(ast.Expression) {
    
    switch (parser.current().token_type) {

        .If => {
            parser.skip();

            const condition = parseBinop1(parser);
            var captures: ?ast.Node(ast.Expression) = null;

            if (parser.skip_if(.RightArrow)) {

                captures = parseList(parser);
            }

            var block: ast.Node(ast.Statement) = undefined;

            if (parser.current().token_type == .OpenBrace) {
                block = parseBlock(parser);
            } else {
                block = parser.makeStmtNode(.{ 
                    .Expression = parseExpression(parser),
                });
            }

            return parser.makeExprNode(.{
                .If = .{
                    .condition = condition,
                    .captures = captures,
                    .block = block,
                }
            });
        },

        .Match => {
            parser.skip();
        },

        else => return parseList(parser),
    }
}

fn parseList(parser: *Parser) ast.Node(ast.Expression) {

    const lhs = parseDeclaration(parser);

    if (parser.current().token_type != .Comma) {
        return lhs;
    }

    var list = std.ArrayList(ast.Node(ast.Expression)).empty;
    list.append(parser.allocator, lhs);

    while (parser.skip_if(.Comma)) {
        
        list.append(parser.allocator, parseDeclaration(parser));
    }

    return parser.makeExprNode(.{
        .List = .{
            .expressions = list,
        }
    });
}

fn parseDeclaration(parser: *Parser) ast.Node(ast.Expression) {
    var lhs = parseList(parser);
    
    if (parser.skip_if(.Colon)) {

        lhs = parser.makeExprNode(.{
            .Declaration = .{
                .name = lhs,
                .decl_type = parseSetter(parser),
            }
        });
    }

    return lhs;
}

// +, -
fn parseBinop1(parser: *Parser) ast.Node(ast.Expression) {
    var lhs = parseBinop2(parser);
   
    outer: switch (parser.current().token_type) {

        .Plus, .Minus => {
            const op = parser.current_index;
            lhs = parser.makeExprNode(.{ 
                .Binop = .{ 
                    .left = lhs, 
                    .right = parseBinop2(parser), 
                    .op_token_index = op }});

            parser.skip();
            continue: outer parser.current().token_type;
        },

        else => return lhs,
    }
}

// *, /, %
fn parseBinop2(parser: *Parser) ast.Node(ast.Expression) {
    const lhs = parseCall(parser);

    outer: switch (parser.current().token_type) {

        .Asterisk, .Slash, .Percentage => {
            const op = parser.current_index;
            lhs = parser.makeExprNode(.{ 
                .Binop = .{ 
                    .left = lhs, 
                    .right = parseUnary(parser), 
                    .op_token_index = op }});

            parser.skip();
            continue: outer parser.current().token_type;
        },

        else => return lhs,
    }
}

fn parseUnary(parser: *Parser) ast.Node(ast.Expression) {

    const op = parser.current_index;

    switch (parser.current().token_type) {

        .Minus, .Exclamation, .Reference => {
            return parser.makeExprNode(.{
                .Unary = .{
                    .right = parseCall(parser),
                    .op_token_index = op,
                }
            });
        },

        else => {},
    }

    return parseCall(parser);
}

fn parseSetter(parser: *Parser) ast.Node(ast.Expression) {
    var lhs = parseCall(parser);

    if (parser.current().token_type == .OpenBrace) {

        lhs = parser.makeExprNode(.{
            .Setter = .{
                .settee = lhs,
                .assignments = parseBlock(parser),
            }
        });
    }

    return lhs;
}

fn parseCall(parser: *Parser) ast.Node(ast.Expression) {
    var lhs = parseGeneric(parser);

    if (parser.skip_if(.OpenParentheses)) {

        lhs = parser.makeExprNode(.{
            .Call {
                .callee = lhs,
                .arguements = parseList(parser),
            }
        });

        if (parser.skip_expected(.CloseBrace)) {
            return parser.makeExprNode(.Error);
        }
    }

    return lhs;
}

fn parseGeneric(parser: *Parser) ast.Node(ast.Expression) {
    var lhs = parseMember(parser);

    if (parser.skip_if(.OpenBracket)) {

        lhs = parser.makeExprNode(.{
            .Generic =  .{
                .callee = lhs,
                .arguements = parseList(parser),
            }
        });

        if (parser.skip_expected(.CloseBracket)) {
            return parser.makeExprNode(.Error);
        }
    }

    return lhs;
}

fn parseMember(parser: *Parser) ast.Node(ast.Expression) {
    var lhs = parseParentheisis(parser);

    if (parser.skip_if(.Dot)) {

        lhs = parser.makeExprNode(.{
            .Member = .{ 
                .parent = lhs,
                .member = parseMember(parser) 
            }
        });
    }

    return lhs;
}

fn parseParentheisis(parser: *Parser) ast.Node(ast.Expression) {
    
    if (parser.skip_if(.OpenParentheses)) {

        const rhs = parseList(parser);

        if (parser.skip_expected(.CloseParentheses)) {
            return parser.makeExprNode(.Error);
        }

        return rhs;
    }

    return parseBase(parser);
}

fn parseBase(parser: *Parser) ast.Node(ast.Expression) {
    
    const node = switch (parser.current().token_type) {

        .Identifier => parser.makeExprNode(.{
            .Identifier = .{
                .token_index = parser.current_index,
            }
        }),

        .Builtin => parser.makeExprNode(.{
            .Builtin = .{
                .token_index = parser.current_index,
            }
        }),

        .String => parser.makeExprNode(.{
            .Identifier = .{
                .token_index = parser.current_index,
            }
        }),

        .Char => parser.makeExprNode(.{
            .Identifier = .{
                .token_index = parser.current_index,
            }
        }),

        .Bool => parser.makeExprNode(.{
            .Identifier = .{
                .token_index = parser.current_index,
            }
        }),

        .Number => parser.makeExprNode(.{
            .Identifier = .{
                .token_index = parser.current_index,
            }
        }),

        .Binary => parser.makeExprNode(.{
            .Identifier = .{
                .token_index = parser.current_index,
            }
        }),

        .Self => parser.makeExprNode(.Self),

        .Nothing => parser.makeExprNode(.Nothing),

        .Unknown => parser.makeExprNode(.Unknown),

        .Function => return parseFunctionPrototype(parser),

        .Object => { 
            parser.skip();

            return parser.makeExprNode(.{
                .Object = parseBlock(parser)
            });
        },

        .Enum => {
            parser.skip();
            
            return parser.makeExprNode(.{
                .Enum = parseBlock(parser)
            });
        },

        .Interface => {
            parser.skip();
            
            return parser.makeExprNode(.{
                .Interface = parseBlock(parser)
            });
        },

        else => {
            parser.AddError("Invalid Token {s}", .{@tagName(parser.current().token_type)});
            return parser.makeExprNode(.Error);
        },
    };

    parser.skip();

    return node;
}

fn parseFunctionPrototype(parser: *Parser) ast.Node(ast.Expression) {

    if (parser.skip_expected(.Function)) {
        return parser.makeExprNode(.Error);
    }

    if (parser.skip_expected(.OpenParentheses)) {
        return parser.makeExprNode(.Error);
    }

    const left = parseList(parser);

    if (parser.skip_expected(.CloseParentheses)) {
        return parser.makeExprNode(.Error);
    }

    return parser.makeExprNode(.{ 
        .FuncPrototype = .{
            .arguments = left,
            .return_type = parseList(parser),
        }
    });
}