const std = @import("std");
const tokens = @import("tokens.zig");
const lexer = @import("lexer.zig");

const Block = struct {
    usings: std.ArrayList(Using),
    body: std.ArrayList(Node(Statement)),
};

const Using = struct {
    alias: Node(Expression),
    namespace: Node(Expression),
};

fn Node(comptime T: type) type {
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

const Statement = union(enum) {
    Block: Node(Block),
    Private: Node(Statement),
    Loop: Node(Statement),
    Return,
    Break,
    Continue,
    Expression: Node(Expression),
    Error,
};

const Expression = union(enum) {
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

const FuncPrototype = struct {
    arguments: ?Node(Expression),
    return_type: Node(Expression),
};

const Assignment = struct {
    assignee: Node(Expression),
    value: Node(Expression),
    op_token_index: usize,
};

const List = struct {
    expressions: std.ArrayList(Node(Expression)),
};

const Declaration = struct {
    is_reference: bool,
    name: Node(Expression),
    decl_type: Node(Expression),
};

const Conditional = struct {
    condition: Node(Expression),
    captures: ?Node(Expression),
    block: Node(Statement),
};

const Match = struct {
    value: Node(Expression),
    cases: std.ArrayList(Case),
};

const Case = struct {
    value: Node(Expression),
    capture: ?Node(Expression),
    block: Node(Statement),
};

const Binop = struct {
    left: Node(Expression),
    right: Node(Expression),
    op_token_index: usize,
};

const Unary = struct {
    right: Node(Expression),
    op_token_index: usize,
};

const Setter = struct {
    settee: Node(Expression),
    assignments: Node(Block)
};

const Call = struct {
    callee: Node(Expression),
    arguements: Node(Expression),
};

const Generic = struct {
    callee: Node(Expression),
    arguements: Node(Expression),
};

const Member = struct {
    parent: Node(Expression),
    member: Node(Expression),
};

const Identifier = struct { 
    token_index: usize,
};

const Literal = struct {
    literal_type: tokens.TokenType,
    token_index: usize,
};

const AstError = struct {
    token_index: usize,
    message: []const u8,
};

const Ast = struct {

    source: []const u8,
    tokens: []lexer.Token,
    root_block: Block,
    errors: std.ArrayList(AstError),

    pub fn deinit(self: *Ast, allocator: std.mem.Allocator) void {
        allocator.free(self.tokens);
    }
};