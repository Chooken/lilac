const std = @import("std");
const tokens = @import("tokens.zig");

pub fn TypedNode(comptime T: type) type {
    return struct {
        typeid: TypeId,
        data: *T,
        start_token: usize,
        end_token: usize,

        pub fn init(allocator: std.mem.Allocator, start_token: usize, end_token: usize, typeid: TypeId, value: T) !TypedNode(T) {
            const data = try allocator.create(T);
            data.* = value;

            return .{
                .typeid = typeid,
                .data = data,
                .start_token = start_token,
                .end_token = end_token,
            };
        }
    };
}

pub const Program = struct {
    types: std.ArrayList(Type) = .empty,
    func_types: std.ArrayList(TypeId) = .empty,
    functions: std.ArrayList(Function) = .empty,

    pub fn addType(self: *Program, allocator: std.mem.Allocator, typedata: Type) TypeId {
        self.types.append(allocator, typedata);
        return .{
            .index = self.types.items.len - 1,
        };
    }
};

pub const Type = struct {
    size: ?usize,
    data: ?TypeData,
};

pub const TypeId = struct {
    index: usize,
};

pub const TypeData = union(enum) {
    Primative: Primative,
    Object: Object,
    Enum: Enum,
    Function: FunctionProto,
    Interface: Interface,
    Namespace,
    Nothing,
};

pub const Conversion = struct {
    from: TypeId,
    to: TypeId,
};

pub const Operator = struct {
    lhs: TypeId,
    rhs: TypeId,
    op: tokens.TokenType,
};

pub const Primative = struct {
    size: usize,
};

pub const Field = struct {
    name: []const u8,
    typeid: TypeId,
};

pub const Object = struct {
    structure: std.ArrayList(Field),
};

pub const Enum = struct {
    structure: std.ArrayList(Field),
};

pub const FunctionProto = struct {
    inputs: std.ArrayList(TypeId),
    outputs: std.ArrayList(TypeId),
};

pub const Interface = struct {
    structure: std.ArrayList(Field),
};

// Typed Ast.

pub const FunctionId = struct {
    index: usize,
};

pub const Function = struct {
    typeid: TypeId, 
    block: Block,
};

pub const Block = struct {
    body: std.ArrayList(Statement),
};

pub const Statement = union(enum) {
    Block,
    Loop,
    Return,
    Break,
    Continue,
    Expression,
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
    Member: Member,
    Identifier: Identifier,
    Builtin: Identifier,
    Literal: Literal,
    Error,
};

pub const Assignment = struct {
    assignee: TypedNode(Expression),
    value: TypedNode(Expression),
    op_token_index: usize,
};

pub const List = struct {
    expressions: std.ArrayList(TypedNode(Expression)),
};

pub const Declaration = struct {
    name: TypedNode(Expression),
    decl_type: TypedNode(Expression),
};

pub const Conditional = struct {
    condition: TypedNode(Expression),
    captures: ?TypedNode(Expression),
    body: TypedNode(Statement),
    else_body: ?TypedNode(Else),
};

pub const Else = struct {
    body: TypedNode(Statement),
};

pub const Match = struct {
    value: TypedNode(Expression),
    cases: std.ArrayList(Case),
    else_case: ?TypedNode(Else),
};

pub const Case = struct {
    pattern: TypedNode(Expression),
    captures: ?TypedNode(Expression),
    body: TypedNode(Statement),
};

pub const Binop = struct {
    left: TypedNode(Expression),
    right: TypedNode(Expression),
    op_token_index: usize,
};

pub const Unary = struct {
    right: TypedNode(Expression),
    op_token_index: usize,
};

pub const Setter = struct {
    settee: TypedNode(Expression),
    body: TypedNode(Block)
};

pub const Call = struct {
    callee: TypedNode(Expression),
    arguements: ?TypedNode(Expression),
};

pub const Member = struct {
    parent: TypedNode(Expression),
    child: TypedNode(Expression),
};

pub const Identifier = struct { 
    token_index: usize,
};

pub const Literal = struct {
    literal_type: tokens.TokenType,
    token_index: usize,
};