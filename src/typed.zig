const std = @import("std");
const tokens = @import("tokens.zig");
const files = @import("files.zig");

pub fn TypedNode(comptime T: type) type {
    return struct {
        value: std.ArrayList(TypeRef),
        data: *T,
        start: usize,
        end: usize,
        file_id: files.FileId,

        pub fn init(allocator: std.mem.Allocator, start: usize, end: usize, file_id: files.FileId, value: std.ArrayList(TypeRef), data: T) TypedNode(T) {
            
            const data_ptr = allocator.create(T) catch @panic("Out of Memory.");
            data_ptr.* = data;

            return .{
                .value = value,
                .data = data_ptr,
                .start = start,
                .end = end,
                .file_id = file_id,
            };
        }

        pub fn getInferable(self: TypedNode(T)) ?TypeRef {
            if (self.value.items.len == 1) {
                return self.value.items[0];
            }
            return null;
        }
    };
}

pub const Program = struct {
    types: std.ArrayList(Type) = .empty,
    func_types: std.HashMapUnmanaged(FunctionProto, TypeId, FunctionProto.HashContext, 80) = .empty,
    functions: std.ArrayList(Function) = .empty,

    pub fn addType(self: *Program, allocator: std.mem.Allocator, typedata: Type) TypeId {
        self.types.append(allocator, typedata) catch @panic("Out of Memory.");
        return .{
            .index = self.types.items.len - 1,
        };
    }
};



pub const Type = struct {
    name: ?[]const u8,
    size: ?usize = null,
    data: ?TypeData = null,
};

pub const Visability = enum {
    public,
    private,
};

pub const TypeId = struct {
    index: usize,
};

pub const TypeRef = struct {
    id: TypeId,
    is_ref: bool,

    pub fn cmp(self: TypeRef, other: TypeRef) bool {
        return self.id.index == other.id.index and self.is_ref == other.is_ref;
    }
};

pub const TypeData = union(enum) {
    Primative,
    Object: Object,
    Enum: Enum,
    Function: FunctionProto,
    Interface: Interface,
    Module: Module,
    Nothing,
};

pub const Conversion = struct {
    from: TypeRef,
    to: TypeRef,
};

pub const BinopOperator = struct {
    lhs: TypeRef,
    rhs: TypeRef,
    op: tokens.TokenType,
};

pub const UnaryOperator = struct {
    value: TypeRef,
    op: tokens.TokenType,
};

pub const Field = struct {
    visability: Visability,
    name: []const u8,
    type_ref: ?TypeRef,
};

pub const Object = struct {
    structure: std.ArrayList(Field) = .empty,
};

pub const Enum = struct {
    structure: std.ArrayList(Field) = .empty,
};

pub const FunctionProto = struct {
    inputs: std.ArrayList(TypeRef) = .empty,
    outputs: std.ArrayList(TypeRef) = .empty,

    pub const HashContext = struct {

        pub fn hash(_: HashContext, proto: FunctionProto) u64 {

            var hasher = std.hash.XxHash64.init(0);

            const input_bytes = std.mem.sliceAsBytes(proto.inputs.items);
            hasher.update(input_bytes);
            const output_bytes = std.mem.sliceAsBytes(proto.inputs.items);
            hasher.update(output_bytes);

            return hasher.final();
        }

        pub fn eql(_: HashContext, left: FunctionProto, right: FunctionProto) bool {
            
            if (left.inputs.items.len != right.inputs.items.len or left.outputs.items.len != right.outputs.items.len) {
                return false;
            }

            for (0..left.inputs.items.len) |index| {
                if (!left.inputs.items[index].cmp(right.inputs.items[index])) {
                    return false;
                }
            }

            for (0..left.outputs.items.len) |index| {
                if (!left.outputs.items[index].cmp(right.outputs.items[index])) {
                    return false;
                }
            }

            return true;
        }
    };
};

pub const Interface = struct {
    structure: std.ArrayList(struct { []const u8,  }) = .empty,
};

pub const Module = struct {
    globals: std.ArrayList(Field) = .empty,
};

// Typed Ast.

pub const FunctionId = struct {
    index: usize,
};

pub const Function = struct {
    requires_self: bool,
    is_inlined: bool,
    typeid: TypeId, 
    block: ?TypedNode(Statement),
};

pub const Block = struct {
    body: std.ArrayList(TypedNode(Statement)) = .empty,
};

pub const Statement = union(enum) {
    Block: TypedNode(Block),
    Loop: TypedNode(Statement),
    Defer: TypedNode(Statement),
    Expression: TypedNode(Expression),
    Return,
    Break,
    Continue,
    Error,
};

pub const Expression = union(enum) {
    Split: Split,
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
    SplitLiteral: SplitLiteral,
    Error,
};

pub const Split = struct { 
    results: std.ArrayList(TypedNode(Expression)),
    value: TypedNode(Expression),
};

pub const Assignment = struct {
    assignee: TypedNode(Expression),
    value: TypedNode(Expression),
    op_token_type: tokens.TokenType,
};

pub const List = struct {
    expressions: std.ArrayList(TypedNode(Expression)),
};

pub const Declaration = struct {
    name: TypedNode(Identifier),
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
    settee: TypeId,
    body: TypedNode(Block)
};

pub const Call = struct {
    callee: FunctionId,
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

pub const SplitLiteral = struct {
    index: usize,
};