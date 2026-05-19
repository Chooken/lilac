const std = @import("std");
const tokens = @import("tokens.zig");
const lexer = @import("lexer.zig");

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
        start_token: usize,

        pub fn init(allocator: std.mem.Allocator, start_token: usize, value: T) !Node(T) {
            const data = try allocator.create(T);
            data.* = value;

            return .{
                .data = data,
                .start_token = start_token,
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

pub const FuncPrototype = struct {
    arguments: ?Node(Expression),
    return_type: Node(Expression),
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
    block: Node(Statement),
};

pub const Match = struct {
    value: Node(Expression),
    cases: std.ArrayList(Case),
};

pub const Case = struct {
    value: Node(Expression),
    capture: ?Node(Expression),
    block: Node(Statement),
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
    assignments: Node(Block)
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
    member: Node(Expression),
};

pub const Identifier = struct { 
    token_index: usize,
};

pub const Literal = struct {
    literal_type: tokens.TokenType,
    token_index: usize,
};

pub const AstError = struct {
    token_index: usize,
    message: []const u8,
};

pub const Ast = struct {

    source: []const u8,
    tokens: []tokens.Token,
    root_block: Block,
    errors: std.ArrayList(AstError),

    pub fn deinit(self: *Ast, allocator: std.mem.Allocator) void {
        allocator.free(self.tokens);
    }
};

