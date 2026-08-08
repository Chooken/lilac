const std = @import("std");
const tokens = @import("tokens.zig");
const files = @import("files.zig");

pub fn TypedNode(comptime T: type) type {
    return struct {
        value: std.ArrayList(TypeRef),
        data: *T,
        span: files.Span,

        pub fn init(allocator: std.mem.Allocator, span: files.Span, value: std.ArrayList(TypeRef), data: T) TypedNode(T) {
            
            const data_ptr = allocator.create(T) catch @panic("Out of Memory.");
            data_ptr.* = data;

            return .{
                .value = value,
                .data = data_ptr,
                .span = span,
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
    functions: std.ArrayList(Function) = .empty,
    globals: std.ArrayList(TypeRef) = .empty,

    pub fn addType(self: *Program, allocator: std.mem.Allocator, typedata: Type) TypeId {
        self.types.append(allocator, typedata) catch @panic("Out of Memory.");
        return .{
            .index = self.types.items.len - 1,
        };
    }
};

pub const Layout = enum {
    Object,
    Enum,
    Union,
};

pub const Type = struct {
    name: ?[]const u8,
    size: ?usize = null,
    structure: std.ArrayList(TypeRef) = .empty,
};

pub const TypeId = struct {
    index: usize,
};

pub const FunctionId = struct {
    index: usize,
};

pub const globalId = struct {
    index: usize,
};

pub const TypeRef = struct {
    id: TypeId,
    is_ref: bool,

    pub fn cmp(self: TypeRef, other: TypeRef) bool {
        return self.id.index == other.id.index and self.is_ref == other.is_ref;
    }
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

// Typed Ast.

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
    Declaration: TypeRef,
    List: List,
    Setter: Setter,
    Call: Call,
    BuiltinCall: BuiltinCall, 
    FieldAccessor: FieldAccessor,
    GlobalId: usize,
    LocalVar: usize,
    SplitVar: usize,
    Function: FunctionId,
    Type,
    Error,
};

pub const Split = struct { 
    results: std.ArrayList(TypedNode(Expression)),
    value: TypedNode(Expression),
};

pub const Assignment = struct {
    assignee: TypedNode(Expression),
    value: TypedNode(Expression),
    inlined: bool,
};

pub const List = struct {
    expressions: std.ArrayList(TypedNode(Expression)),
};

pub const Conditional = struct {
    condition: TypedNode(Expression),
    captures: std.ArrayList(TypedNode(TypeRef)),
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

pub const Setter = struct {
    settee: TypeId,
    body: std.ArrayList(TypedNode(Assignment))
};

pub const EnumSetter = struct {
    settee: TypeId,
    tag: TypedNode(Expression), 
    value: ?TypedNode(Expression),
};

pub const Call = struct {
    callee: FunctionId,
    arguements: ?TypedNode(Expression),
};

pub const BuiltinCall = struct {
    callee: []const u8,
    arguements: ?TypedNode(Expression),
};

pub const FieldAccessor = struct {
    parent: TypedNode(Expression),
    field_index: usize,
};

pub const SplitVar = struct {
    index: usize,
};