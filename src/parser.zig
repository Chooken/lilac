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

    pub fn AddError(self: *Parser, comptime fmt: []const u8, args: anytype) void {
        const message = std.fmt.allocPrint(self.allocator, fmt, args) catch return;
        
        self.ast.errors.append(self.allocator, .{
            .token_index = self.current_index,
            .message = message,
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

        if (self.current_index + 1 >= self.ast.tokens) return .{
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

    pub fn eat_expected(self: *Parser, token_type: TokenType) ?Token {
        
        const prev_token = self.current();

        if (prev_token.token_type != token_type) {
            self.AddError("Unexpected token. Expected {s} but got {s} instead", .{ @tagName(token_type), self.ast.source[prev_token.start..prev_token.end] });
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

    pub fn skip_expected(self: *Parser, token_type: TokenType) bool {

        const prev_token = self.current();

        if (prev_token.token_type != token_type) {
            self.AddError("Unexpected token. Expected {s} but got {s} instead", .{ @tagName(token_type), self.ast.source[prev_token.start..prev_token.end] });
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

    pub fn makeStmtNode(self: *Parser, expression: ast.Statement) ast.Node(ast.Statement) {
        return ast.Node(ast.Statement).init(
            self.allocator, 
            self.current_index, 
            expression) catch @panic("Out of Memory.");
    }

    pub fn makeExprNode(self: *Parser, expression: ast.Expression) ast.Node(ast.Expression) {
        return ast.Node(ast.Expression).init(
            self.allocator, 
            self.current_index, 
            expression) catch @panic("Out of Memory.");
    }
};

pub fn parse(source: [:0]const u8, allocator: std.mem.Allocator) !Ast {

    var tokens: std.ArrayList(Token) = .empty;
    defer tokens.deinit(allocator);

    var tokenizer = Tokenizer.init(source);

    std.debug.print("-- Tokens --\n", .{});
    while (tokenizer.next()) |token| {
        try tokens.append(allocator, token);
        std.debug.print("{s} - {s}\n", .{@tagName(token.token_type), source[token.start..token.end]});
    }

    var parser = Parser { 
        .ast = Ast {
            .source = source,
            .tokens = try tokens.toOwnedSlice(allocator),
            .errors = .empty,
            .root_block = undefined,
        },
        .allocator = allocator,
    };

    parser.ast.root_block = parseTopBlock(&parser);

    return parser.ast;
}

fn parseTopBlock(parser: *Parser) ast.Block {
    Enter(parser, "TopBlock");
    var body = std.ArrayList(ast.Node(ast.Statement)).empty;

    while (!parser.eof()) {
        std.debug.print("At {s} token {d}\n", .{@tagName(parser.current().token_type), parser.current_index});
        body.append(parser.allocator, parseStatement(parser)) catch @panic("Out of Memory");
    }

    Exit(false);
    return ast.Block {
        .body = body
    };
}

fn parseBlockWithNode(parser: *Parser) ast.Node(ast.Block) {
    return ast.Node(ast.Block).init(parser.allocator, parser.current_index, parseBlock(parser)) catch @panic("Out of Memory.");
}

fn parseBlock(parser: *Parser) ast.Block {

    Enter(parser, "Block");
    var body = std.ArrayList(ast.Node(ast.Statement)).empty;

    if (parser.skip_expected(.OpenBrace)) {
        Exit(true);
        return ast.Block {
            .body = body
        };
    }

    while (parser.current().token_type != .CloseBrace and !parser.eof()) {
        body.append(parser.allocator, parseStatement(parser)) catch @panic("Out of Memory");
    }

    if (parser.skip_expected(.CloseBrace)) {
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

                alias = parseBase(parser);
            }

            Exit(false);
            return parser.makeStmtNode(.{ 
                .Using = .{
                    .namespace = namespace,
                    .alias = alias,
                }
            });
        },

        .Private => {
            parser.skip();

            const node = parser.makeStmtNode(.{
                .Private = parseBlockWithNode(parser),
            });
            Exit(false);
            return node;
        },

        .OpenBrace => {
            const node = parser.makeStmtNode(.{
                .Block = parseBlockWithNode(parser),
            });
            Exit(false);
            return node;
        },

        .Loop => {
            parser.skip();

            const node = parser.makeStmtNode(.{
                .Loop = parser.makeStmtNode(.{
                    .Block = parseBlockWithNode(parser),
                }),
            });
            Exit(false);
            return node;
        },

        .Return => { 
            parser.skip();
            Exit(false);
            return parser.makeStmtNode(.Return);
        },

        .Break => { 
            parser.skip();
            Exit(false);
            return parser.makeStmtNode(.Break);
        },

        .Continue => { 
            parser.skip();
            Exit(false);
            return parser.makeStmtNode(.Continue);
        },

        .Invalid => {
            parser.skip();
            Exit(true);
            return parser.makeStmtNode(.Error);
        },

        else => { 
            const node = parser.makeStmtNode(.{
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

fn parseAssignment(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Assignment");

    const lhs = parseConditionals(parser);
    
    switch (parser.current().token_type) {

        .Equals, .PlusEquals, .MinusEquals, .TimesEquals, .DivEquals, .PercentEquals => {

            const op = parser.current_index;

            parser.skip();

            const value = parseConditionals(parser);

            Exit(false);
            return parser.makeExprNode(.{
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
                block = parser.makeStmtNode(.{ 
                    .Block = parseBlockWithNode(parser),
                });
            } else {
                block = parser.makeStmtNode(.{ 
                    .Expression = parseExpression(parser),
                });
            }

            Exit(false);
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

            @panic("Not Implemented.");
        },

        else => {
            const node = parseList(parser);
            Exit(false);
            return node;
        },
    }
}

fn parseList(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "List");

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
    return parser.makeExprNode(.{
        .List = .{
            .expressions = list,
        }
    });
}

fn parseDeclaration(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Declaration");

    var lhs = parseBinop1(parser);
    
    if (parser.skip_if(.Colon)) {

        lhs = parser.makeExprNode(.{
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

    var lhs = parseBinop2(parser);
   
    outer: switch (parser.current().token_type) {

        .And, .Or => {
            const op = parser.current_index;
            parser.skip();
            lhs = parser.makeExprNode(.{ 
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

    var lhs = parseBinop3(parser);
   
    outer: switch (parser.current().token_type) {

        .EqualsEquals, .LessThan, .GreaterThan, .LessThanOrEquals, .GreaterThanOrEquals => {
            const op = parser.current_index;
            parser.skip();
            lhs = parser.makeExprNode(.{ 
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

    var lhs = parseBinop4(parser);
   
    outer: switch (parser.current().token_type) {

        .Plus, .Minus => {
            const op = parser.current_index;
            parser.skip();
            lhs = parser.makeExprNode(.{ 
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

    var lhs = parseUnary(parser);

    outer: switch (parser.current().token_type) {

        .Asterisk, .Slash, .Percentage => {
            const op = parser.current_index;
            parser.skip();
            lhs = parser.makeExprNode(.{ 
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

    const op = parser.current_index;

    switch (parser.current().token_type) {

        .Minus, .Exclamation, .Reference => {
            parser.skip();
            const node = parser.makeExprNode(.{
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

    var lhs = parseMember(parser);

    if (parser.current().token_type == .OpenBrace) {

        lhs = parser.makeExprNode(.{
            .Setter = .{
                .settee = lhs,
                .assignments = parseBlockWithNode(parser),
            }
        });
    }

    Exit(false);
    return lhs;
}

fn parseMember(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Member");

    var lhs = parseCall(parser);

    while (parser.skip_if(.Dot)) {

        lhs = parser.makeExprNode(.{
            .Member = .{ 
                .parent = lhs,
                .member = parseCall(parser) 
            }
        });
    }

    Exit(false);
    return lhs;
}

fn parseCall(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Call");

    var lhs = parseGeneric(parser);

    if (parser.skip_if(.OpenParentheses)) {

        var args: ?ast.Node(ast.Expression) = null;

        if (parser.current().token_type != .CloseParentheses) {
            args = parseList(parser);
        }

        lhs = parser.makeExprNode(.{
            .Call = .{
                .callee = lhs,
                .arguements = args,
            }
        });

        if (parser.skip_expected(.CloseParentheses)) {
            Exit(true);
            return parser.makeExprNode(.Error);
        }
    }

    Exit(false);
    return lhs;
}

fn parseGeneric(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Generic");

    var lhs = parseParentheisis(parser);

    if (parser.skip_if(.OpenBracket)) {

        lhs = parser.makeExprNode(.{
            .Generic =  .{
                .callee = lhs,
                .arguements = parseDeclarationList(parser),
            }
        });

        if (parser.skip_expected(.CloseBracket)) {
            Exit(true);
            return parser.makeExprNode(.Error);
        }
    }

    Exit(false);
    return lhs;
}

fn parseParentheisis(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Parentheisis");
    
    if (parser.skip_if(.OpenParentheses)) {

        const rhs = parseList(parser);

        if (parser.skip_expected(.CloseParentheses)) {
            Exit(true);
            return parser.makeExprNode(.Error);
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

        .Function => { 
            const node = parseFunctionPrototype(parser);
            Exit(false);
            return node;
        },

        .Object => { 
            parser.skip();

            const node = parser.makeExprNode(.{
                .Object = parseBlockWithNode(parser)
            });
            
            Exit(false);
            return node;
        },

        .Enum => {
            parser.skip();
            
            const node = parser.makeExprNode(.{
                .Enum = parseBlockWithNode(parser)
            });

            Exit(false);
            return node;
        },

        .Interface => {
            parser.skip();
            
            const node = parser.makeExprNode(.{
                .Interface = parseBlockWithNode(parser)
            });
            
            Exit(false);
            return node;
        },

        else => {
            Exit(true);
            parser.AddError("Invalid Token {s}", .{@tagName(parser.current().token_type)});
            parser.skip();
            return parser.makeExprNode(.Error);
        },
    };

    Exit(false);
    parser.skip();

    return node;
}

fn parseFunctionPrototype(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Function Proto");

    if (parser.skip_expected(.Function)) {
        Exit(true);
        return parser.makeExprNode(.Error);
    }

    if (parser.skip_expected(.OpenParentheses)) {
        Exit(true);
        return parser.makeExprNode(.Error);
    }

    var left: ?ast.Node(ast.Expression) = null;

    if (parser.current().token_type != .CloseParentheses) {
        left = parseDeclarationList(parser);
    }

    if (parser.skip_expected(.CloseParentheses)) {
        Exit(true);
        return parser.makeExprNode(.Error);
    }

    const node =  parser.makeExprNode(.{ 
        .FuncPrototype = .{
            .arguments = left,
            .return_type = parseDeclarationList(parser),
        }
    });

    Exit(false);
    return node;
}

fn parseDeclarationList(parser: *Parser) ast.Node(ast.Expression) {

    Enter(parser, "Declaration List");

    var node: ast.Node(ast.Expression) = undefined;
    
    switch (parser.current().token_type) {

        .Identifier => { 

            const name = parser.makeExprNode(.{
                .Identifier = .{
                    .token_index = parser.current_index,
                }
            });

            parser.skip();
            
            node = parser.makeExprNode(.{
                .Declaration = .{
                    .name = name,
                    .decl_type = parseMember(parser),
                }
            });
        },

        .Nothing => {
            parser.skip();
            node = parser.makeExprNode(.Nothing);
        },

        .Self => {
            parser.skip();
            node = parser.makeExprNode(.Self);
        },

        .Unknown => {
            parser.skip();
            node = parser.makeExprNode(.Unknown);
        },

        else => {
            Exit(true);
            parser.AddError("Invalid Token {s}", .{@tagName(parser.current().token_type)});
            return parser.makeExprNode(.Error);
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
        const node2 = switch (parser.current().token_type) {

            .Identifier => parser.makeExprNode(.{
                .Declaration = .{
                    .name = parser.makeExprNode(.{
                        .Identifier = .{
                            .token_index = parser.current_index,
                        }
                    }),
                    .decl_type = parseMember(parser),
                }
            }),

            .Nothing => parser.makeExprNode(.Nothing),

            .Self => parser.makeExprNode(.Self),

            .Unknown => parser.makeExprNode(.Unknown),

            else => {
                Exit(true);
                parser.AddError("Invalid Token {s}", .{@tagName(parser.current().token_type)});
                return parser.makeExprNode(.Error);
            }
        };

        list.expressions.append(parser.allocator, node2) catch @panic("Out of Memory.");
    }

    Exit(false);
    return node;
}

var depth: usize = 0;

fn Enter(parser: *Parser, string: []const u8) void {

    depth += 1;

    for (0..depth) |_| {
        std.debug.print("| ", .{});
    }

    std.debug.print("{s} - token: {s} = {s}\n", .{string, @tagName(parser.current().token_type), parser.ast.source[parser.current().start..parser.current().end]});
}

fn Exit(is_error: bool) void {
    if (is_error) {
        for (0..depth) |_| {
            std.debug.print("| ", .{});
        }
        std.debug.print("Failed!!!!!!!!!!!\n", .{});
    }
    
    depth -= 1;
}