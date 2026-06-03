const std = @import("std");
const tokens = @import("tokens.zig");
const lexer = @import("lexer.zig");
const logger = @import("logger.zig");
const files = @import("files.zig");

pub const Program = struct {
    root_module: Module = .{},
};

pub const Module = struct {
    submodules: std.StringHashMapUnmanaged(Module) = .empty,
    asts: std.ArrayList(Ast) = .empty,
};

pub const Ast = struct {
    file: files.FileId,
    source: []const u8,
    tokens: []tokens.Token,
    root_block: Block,
};

pub const Block = struct {
    body: std.ArrayList(Node(Statement)),
};

pub const Using = struct {
    alias: ?Node(Expression),
    namespace: Node(Expression),
};

pub fn Node(comptime T: type) type {
    return struct {
        data: *T,
        start: usize,
        end: usize,
        file_id: files.FileId,

        pub fn init(allocator: std.mem.Allocator, start: usize, end: usize, value: T, file_id: files.FileId) !Node(T) {
            const data = try allocator.create(T);
            data.* = value;

            return .{
                .data = data,
                .start = start,
                .end = end,
                .file_id = file_id,
            };
        }
    };
}

pub const Statement = union(enum) {
    Block: Node(Block),
    Private: Node(Block),
    Loop: Node(Statement),
    Using: Using,
    Return,
    Break,
    Continue,
    Expression: Node(Expression),
    Error,
};

pub const Expression = union(enum) {
    If: Conditional,
    Match: Match,
    Assignment: Assignment,
    Declaration: Declaration,
    List: List,
    Binop: Binop,
    Unary: Unary,
    Setter: Setter,
    Call: Call,
    Generic: Generic,
    Member: Member,
    ImplicitMember: Node(Expression),
    Function: Function,
    FuncPrototype: FuncPrototype,
    Object: Node(Block),
    Enum: Node(Block),
    Interface: Node(Block),
    Identifier: Identifier,
    Builtin: Identifier,
    Literal: Literal,
    Nothing,
    Unknown,
    Self,
    Error,
};

pub const Function = struct {
    is_inline: bool,
    prototype: Node(Expression),
    body: Node(Statement),
};

pub const FuncPrototype = struct {
    arguments: ?Node(Expression),
    returns: Node(Expression),
};

pub const Assignment = struct {
    assignee: Node(Expression),
    value: Node(Expression),
    op_token_index: usize,
};

pub const List = struct {
    expressions: std.ArrayList(Node(Expression)),
};

pub const Declaration = struct {
    name: Node(Expression),
    decl_type: Node(Expression),
};

pub const Conditional = struct {
    condition: Node(Expression),
    captures: ?Node(Expression),
    body: Node(Statement),
    else_body: ?Node(Else),
};

pub const Else = struct {
    body: Node(Statement),
};

pub const Match = struct {
    value: Node(Expression),
    cases: std.ArrayList(Case),
    else_case: ?Node(Else),
};

pub const Case = struct {
    pattern: Node(Expression),
    captures: ?Node(Expression),
    body: Node(Statement),
};

pub const Binop = struct {
    left: Node(Expression),
    right: Node(Expression),
    op_token_index: usize,
};

pub const Unary = struct {
    right: Node(Expression),
    op_token_index: usize,
};

pub const Setter = struct {
    settee: Node(Expression),
    body: Node(Block)
};

pub const Call = struct {
    callee: Node(Expression),
    arguements: ?Node(Expression),
};

pub const Generic = struct {
    callee: Node(Expression),
    arguements: Node(Expression),
};

pub const Member = struct {
    parent: Node(Expression),
    child: Node(Expression),
};

pub const Identifier = struct { 
    token_index: usize,
};

pub const Literal = struct {
    literal_type: tokens.TokenType,
    token_index: usize,
};

pub fn printAST(ast: *Ast) void {
    printBlock(&ast.root_block, 0, ast);
}

fn printBlock(block: *Block, indent: usize, ast: *Ast) void {
    printWithIndent(indent, "Block:", .{});
    for (block.body.items) |item| {
        printStatement(item, indent + 1, ast);
    }
}

fn printStatement(node: Node(Statement), indent: usize, ast: *Ast) void {

    switch (node.data.*) {
        .Block => |block| {
            printBlock(block.data, indent, ast);
        },

        .Loop => |loop| {
            printWithIndent(indent, "Loop:", .{});
            printStatement(loop, indent + 1, ast);
        },

        .Private => |private| {
            printWithIndent(indent, "Private:", .{});
            printBlock(private.data, indent + 1, ast);
        },

        .Using => |using| {
            printWithIndent(indent, "Using:", .{});

            printWithIndent(indent + 1, "Namespace:", .{});
            printExpression(using.namespace, indent + 2, ast);

            if (using.alias) |alias| {
                printWithIndent(indent + 1, "Alias:", .{});
                printExpression(alias, indent + 2, ast);
            }
        },

        .Return => printWithIndent(indent, "Return", .{}),
        .Continue => printWithIndent(indent, "Continue", .{}),
        .Break => printWithIndent(indent, "Break", .{}),

        .Error => printWithIndent(indent, "Error", .{}),

        .Expression => |expr| printExpression(expr, indent, ast),
    }
}

fn printExpression(node: Node(Expression), indent: usize, ast: *Ast) void {
    
    switch (node.data.*) {

        .Assignment => |assignment| {
            printWithIndent(indent, "Assignment:", .{});
            
            printWithIndent(indent + 1, "Op: {s}", .{getTokenString(ast.tokens[assignment.op_token_index].start, ast.tokens[assignment.op_token_index].end, ast)});
            printWithIndent(indent + 1, "Assignee:", .{});
            printExpression(assignment.assignee, indent + 2, ast);
            printWithIndent(indent + 1, "Value:", .{});
            printExpression(assignment.value, indent + 2, ast);
        },

        .Binop => |binop| {
            printWithIndent(indent, "Bin Op:", .{});
            
            printWithIndent(indent + 1, "Op: {s}", .{getTokenString(ast.tokens[binop.op_token_index].start, ast.tokens[binop.op_token_index].end, ast)});
            printWithIndent(indent + 1, "Left:", .{});
            printExpression(binop.left, indent + 2, ast);
            printWithIndent(indent + 1, "Right:", .{});
            printExpression(binop.right, indent + 2, ast);
        },

        .Call => |call| {
            printWithIndent(indent, "Call:", .{});
            
            printWithIndent(indent + 1, "Callee:", .{});
            printExpression(call.callee, indent + 2, ast);

            if (call.arguements) |arguements| {
                printWithIndent(indent + 1, "Args:", .{});
                printExpression(arguements, indent + 2, ast);
            }
        },

        .Declaration => |decl| {
            printWithIndent(indent, "Declaration:", .{});

            printWithIndent(indent + 1, "Name:", .{});
            printExpression(decl.name, indent + 2, ast);
            printWithIndent(indent + 1, "Type:", .{});
            printExpression(decl.decl_type, indent + 2, ast);
        },

        .Generic => |gen| {
            printWithIndent(indent, "Generic:", .{});

            printWithIndent(indent + 1, "Callee:", .{});
            printExpression(gen.callee, indent + 2, ast);
            printWithIndent(indent + 1, "Type:", .{});
            printExpression(gen.arguements, indent + 2, ast);
        },
        
        .List => |list| {
            printWithIndent(indent, "List:", .{});
            for (list.expressions.items) |item| {
                printExpression(item, indent + 1, ast);
            }
        },

        .If => |_if| {
            printWithIndent(indent, "If:", .{});
            
            printWithIndent(indent + 1, "Condition:", .{});
            printExpression(_if.condition, indent + 2, ast);

            if (_if.captures) |captures| {
                printWithIndent(indent + 1, "Captures:", .{});
                printExpression(captures, indent + 2, ast);
            }
            printWithIndent(indent + 1, "Body:", .{});
            printStatement(_if.body, indent + 2, ast);

            if (_if.else_body) |else_body| {
                printWithIndent(indent + 1, "Else Body:", .{});
                printStatement(else_body.data.body, indent + 2, ast);
            }
        },

        .Match => |match| {
            printWithIndent(indent, "Match:", .{});
            
            printWithIndent(indent + 1, "Value:", .{});
            printExpression(match.value, indent + 2, ast);
            printWithIndent(indent + 1, "Cases:", .{});
            for (match.cases.items) |item| {
                
                printWithIndent(indent + 2, "Case:", .{});
                printWithIndent(indent + 3, "Pattern:", .{});
                printExpression(item.pattern, indent + 4, ast);
                
                if (item.captures) |capture| {
                    printWithIndent(indent + 3, "Captures:", .{});
                    printExpression(capture, indent + 4, ast);
                }

                printWithIndent(indent + 3, "Body:", .{});
                printStatement(item.body, indent + 4, ast);
            }
        },

        .Member => |member| {
            printWithIndent(indent, "Member:", .{});
            
            printWithIndent(indent + 1, "Parent:", .{});
            printExpression(member.parent, indent + 2, ast);
            printWithIndent(indent + 1, "Child:", .{});
            printExpression(member.child, indent + 2, ast);
        },

        .ImplicitMember => |implicit_member| {
            printWithIndent(indent, "Implicit Member:", .{});
            printExpression(implicit_member, indent + 1, ast);
        },

        .Unary => |unary| {
            printWithIndent(indent, "Unary:", .{});
            
            printWithIndent(indent + 1, "Op: {s}", .{getTokenString(ast.tokens[unary.op_token_index].start, ast.tokens[unary.op_token_index].end, ast)});
            printWithIndent(indent + 1, "Right:", .{});
            printExpression(unary.right, indent + 2, ast);
        },

        .Setter => |setter| {
            
            printWithIndent(indent, "Setter:", .{});

            printWithIndent(indent + 1, "Settee:", .{});
            printExpression(setter.settee, indent + 2, ast);
            printBlock(setter.body.data, indent + 1, ast);
        },

        .Object => |obj| {
            printWithIndent(indent, "Object:", .{});
            printBlock(obj.data, indent + 1, ast);
        },

        .Enum => |_enum| {
            printWithIndent(indent, "Enum:", .{});
            printBlock(_enum.data, indent + 1, ast);
        },

        .Interface => |interface| {
            printWithIndent(indent, "Interface:", .{});
            printBlock(interface.data, indent + 1, ast);
        },

        .Function => |function| {
            printWithIndent(indent, "Function", .{});
            printWithIndent(indent + 1, "Is Inlined:", .{});
            printWithIndent(indent + 2, "{}", .{function.is_inline});
            printExpression(function.prototype, indent + 1, ast);
            printStatement(function.body, indent + 1, ast);
        },

        .FuncPrototype => |funcProto| {
            printWithIndent(indent, "Function Proto:", .{});
            
            if (funcProto.arguments) |arguements| {
                printWithIndent(indent + 1, "Arguements:", .{});
                printExpression(arguements, indent + 2, ast);
            }
            printWithIndent(indent + 1, "Returns:", .{});
            printExpression(funcProto.returns, indent + 2, ast);
        },

        .Identifier => {
            printWithIndent(indent, "Indentifier: {s}", .{getTokenString(node.start, node.end, ast)});
        },

        .Builtin => {
            printWithIndent(indent, "Builtin: {s}", .{getTokenString(node.start, node.end, ast)});
        },

        .Literal => |lit| {
            printWithIndent(indent, "Literal:", .{});
            printWithIndent(indent + 1, "Type: {s}", .{@tagName(lit.literal_type)});
            printWithIndent(indent + 1, "Value: {s}", .{getTokenString(node.start, node.end, ast)});
        },

        .Self => printWithIndent(indent, "Self", .{}),
        
        .Nothing => printWithIndent(indent, "Nothing", .{}),

        .Unknown => printWithIndent(indent, "Unknown", .{}),

        .Error => printWithIndent(indent, "Error", .{}),
    }
}

fn getTokenString(start: usize, end: usize, ast: *Ast) []const u8 {
    return ast.source[start..end];
}

fn printWithIndent(indent: usize, comptime fmt: []const u8, args: anytype) void {

    printIndent(indent);
    logger.printFmt(fmt, args);
    logger.endLine();
}

fn printIndent(size: usize) void {
    logger.setColor(.dim);
    for (0..size) |_| {
        logger.print("| ");
    }
    logger.setColor(.reset);
}