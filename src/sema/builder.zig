const std = @import("std");
const tokens = @import("../tokens.zig");
const untyped = @import("../untyped.zig");
const typed = @import("../typed.zig");
const logging = @import("../logger.zig");
const files = @import("../files.zig");

const TypeError = error {
    MultipleDefinitions,
    InvalidType,
    Visability,
    TypeMismatch,
};

const TypeSubstitution = struct {
    identifier: []const u8,
    typeid: typed.TypeId,
};

pub const Visability = enum {
    public,
    private,
};

pub const Conversion = struct {
    from: typed.TypeRef,
    to: typed.TypeRef,
};

pub const BinopOperator = struct { 
    lhs: typed.TypeRef,
    rhs: typed.TypeRef,
    op: tokens.TokenType,
};

pub const UnaryOperator = struct { 
    value: typed.TypeRef,
    op: tokens.TokenType,
};

pub const Declaration = struct {
    visability: Visability,
    decl_type: DeclarationType,
    span: ?files.Span,
    collisions: std.ArrayList(?files.Span) = .empty,
};

pub const DeclarationType = union(enum) {
    Field: typed.TypeRef,
    InlineExpression: typed.Expression,
    Type: typed.TypeId,
    Function: typed.FunctionId,
    Generic: Generic,
};

pub const Generic = struct {
    base: untyped.Node(untyped.Expression),
    sub_identifiers: std.ArrayList([]const u8) = .empty,
    cache: std.ArrayList(GenericTypeCache) = .empty,

    pub fn getCached(self: *Generic, types: std.ArrayList(typed.TypeId)) ?typed.TypeId {
        cache_loop: for (self.cache.items) |gen_type| {

            for (0..gen_type.sub_types.items.len) |index| {
                if (gen_type.sub_types.items[index].index != types.items[index].index) {
                    continue :cache_loop;
                }
            }

            return gen_type.typeid;
        }

        return null;
    }
};

pub const GenericTypeCache = struct {
    sub_types: std.ArrayList(typed.TypeId),
    typeid: typed.TypeId,
};

pub const Settings = struct {
    bitNativeSize: usize,
    warnOnOperatorTypeChange: bool,
};  

pub const Builder = struct {

    allocator: std.mem.Allocator,

    logger: logging.Logger,

    settings: Settings,

    program: typed.Program = .{},
    uprogram: *untyped.Program,
    
    func_proto_interner: std.HashMapUnmanaged(typed.FunctionProto, typed.TypeId, typed.FunctionProto.HashContext, 80) = .empty,

    scopes: std.AutoHashMapUnmanaged(typed.TypeId, *Scope) = .empty,
    root: typed.TypeId,

    // default types
    bit8: typed.TypeId = undefined,
    bit16: typed.TypeId = undefined,
    bit32: typed.TypeId = undefined,
    bit64: typed.TypeId = undefined,
    bitNative: typed.TypeId = undefined,
    nothing: typed.TypeId = undefined,
    unknown: typed.TypeId = undefined,
    numberLiteral: typed.TypeId = undefined,
    
    // Events

    // Typeid is the type its for.
    initialisers: std.AutoHashMapUnmanaged(typed.TypeId, Declaration) = .empty,
    oncopy: std.AutoHashMapUnmanaged(typed.TypeId, Declaration) = .empty,
    onoverride: std.AutoHashMapUnmanaged(typed.TypeId, Declaration) = .empty,
    ondrop: std.AutoHashMapUnmanaged(typed.TypeId, Declaration) = .empty,

    conversions: std.AutoHashMapUnmanaged(Conversion, Declaration) = .empty,
    binop_operation: std.AutoHashMapUnmanaged(BinopOperator, Declaration) = .empty,
    prefix_operation: std.AutoHashMapUnmanaged(UnaryOperator, Declaration) = .empty,

    pub fn typeListToString(self: *Builder, type_list: std.ArrayList(typed.TypeRef)) []const u8 {

        if (type_list.items.len == 0) {
            return "nothing";
        }

        var string = std.ArrayList(u8).empty;

        for (type_list.items, 0..) |type_ref, index| {

            if (index != 0) {
                string.appendSlice(self.allocator, ", ") catch @panic("Out of Memory.");
            }

            if (type_ref.is_ref) {
                string.appendSlice(self.allocator, "ref ") catch @panic("Out of Memory.");
            }

            if (self.getType(type_ref.id).name) |name| {
                string.appendSlice(self.allocator, name) catch @panic("Out of Memory.");
            } else {
                string.appendSlice(self.allocator, "Unnamed Type") catch @panic("Out of Memory.");
            }
        }

        return string.toOwnedSlice(self.allocator) catch @panic("Out of Memory.");
    }

    pub fn getNewType(self: *Builder, name: ?[]const u8, scope_type: ScopeType, parent: ?*Scope) typed.TypeId {
        const typeid = self.program.addType(self.allocator, .{
            .name = name,
        });

        const scope = self.allocator.create(Scope) catch @panic("Out of Memory.");

        scope.* = .{
            .builder = self,
            .parent = parent,
            .typeid = typeid,
            .scope_type = scope_type,
        };

        self.scopes.put(self.allocator, typeid, scope) catch @panic("Out of Memory.");

        return typeid;
    }

    pub fn getOrAddFunctionType(self: *Builder, proto: typed.FunctionProto) typed.TypeId {

        const result = self.func_proto_interner.getOrPut(self.allocator, proto) catch @panic("Out of Memory.");

        if (!result.found_existing) {

            var name = std.ArrayList(u8).empty;

            name.appendSlice(self.allocator, "func (") catch @panic("Out of Memory.");
            name.appendSlice(self.allocator, self.typeListToString(proto.inputs)) catch @panic("Out of Memory.");
            name.appendSlice(self.allocator, ") ") catch @panic("Out of Memory.");
            name.appendSlice(self.allocator, self.typeListToString(proto.outputs)) catch @panic("Out of Memory.");

            std.debug.print("Added function type {s}\n", .{name.items});

            result.value_ptr.* = self.program.addType(self.allocator, typed.Type{
                .name = name.toOwnedSlice(self.allocator) catch @panic("Out of Memory."),
            });
        }

        return result.value_ptr.*;
    }

    pub fn addFunction(self: *Builder, typeid: typed.TypeId, is_inlined: bool, requires_self: bool) typed.FunctionId {
        self.program.functions.append(self.allocator, .{
            .typeid = typeid,
            .is_inlined = is_inlined,
            .requires_self = requires_self,
            .block = null,
        }) catch @panic("Out of Memory.");

        return typed.FunctionId {
            .index = self.program.functions.items.len - 1,
        };
    }

    pub fn getType(self: *Builder, typeid: typed.TypeId) *typed.Type {
        return &self.program.types.items[typeid.index];
    }

    pub fn getScope(self: *Builder, typeid: typed.TypeId) ?*Scope {
        return self.scopes.get(typeid);
    }

    pub fn getFunction(self: *Builder, function_id: typed.FunctionId) *typed.Function {
        return &self.program.functions.items[function_id.index];
    }
 
    pub fn deinit(self: *Builder) void {
        self.logger.deinit();
    }
};

pub const ScopeType = enum {
    Module,
    Object,
    Function,
    Enum,
    Union,
    Interface,
};

pub const Scope = struct {
    builder: *Builder,
    parent: ?*Scope,
    typeid: typed.TypeId,
    scope_type: ScopeType,

    usings: std.ArrayList(typed.TypeId) = .empty,
    alias: std.StringHashMapUnmanaged(typed.TypeId) = .empty,

    declarations: std.StringHashMapUnmanaged(Declaration) = .empty,

    num_fields: usize = 0,
    fields: std.StringHashMapUnmanaged(usize, typed.TypeRef) = .empty,

    pub fn allocFullName(self: *Scope) []const u8 {
        var parents = std.ArrayList(*Scope).empty;

        var opt_parent: ?*Scope = self.parent;
        var size: usize = 0;

        while (opt_parent) |parent| {
            if (self.builder.getType(parent.typeid).name) |name| {
                size += name.len + 1;
                parents.append(self.builder.allocator, parent) catch @panic("Out of Memory.");
                opt_parent = parent.parent;
            } else {
                break;
            }
        }

        const opt_self_name = self.builder.getType(self.typeid).name;

        if (opt_self_name) |self_name|
            size += self_name.len
        else size -= 1;

        var name: []u8 = self.builder.allocator.alloc(u8, size) catch @panic("Out of Memory.");
        var offset: usize = 0;

        while (parents.pop()) |parent| {
            if (self.builder.getType(parent.typeid).name) |parent_name| {
                @memcpy(name[offset..offset + parent_name.len], parent_name);
                if (parents.items.len != 0 or opt_self_name != null) {
                    name[offset + parent_name.len] = '.';
                }
                offset += parent_name.len + 1;
            }
        }

        if (opt_self_name) |self_name| {
            @memcpy(name[offset..offset + self_name.len], self_name);
        }
        return name;
    }

    pub fn shadows(self: *Scope, identifier: []const u8) bool {
        var current_scope: *Scope = self;
        var public_only = false;

        while (true) {

            if (public_only) {
                if (current_scope.declarations.decls.get(identifier)) |decl| {
                    if (decl.visabiltiy == .public) {
                        return true;
                    }
                }
            } else {
                if (current_scope.declarations.decls.contains(identifier)) {
                    return true;
                }

                for (current_scope.usings.items) |using| {
                    if (self.builder.getScope(using)) |using_scope| {
                        if (using_scope.declarations.decls.get(identifier)) |decl| {
                            if (decl.visabiltiy == .public) {
                                return true;
                            }
                        }
                    }
                }
            }

            if (current_scope.parent) |parent_scope| {
                current_scope = parent_scope;
            } else {
                break;
            }

            public_only = true;
        }

        return false;
    }

    pub fn contains(self: *Scope, identifier: []const u8) bool {
        return self.declarations.decls.contains(identifier);
    }

    pub fn addTypeDecl(self: *Scope, identifier: []const u8, scope_type: ScopeType, visability: Visability, span: ?files.Span) TypeError!typed.TypeId {

        std.debug.print("Type {s} {s} to {s}\n", .{@tagName(visability), identifier, self.allocFullName()});

        const typeid= self.builder.getNewType(identifier, scope_type, self);
        try self.addDecl(
            identifier, 
            span, 
            .{ .Type = typeid, },
            visability);
        return typeid;
    }

    pub fn addTypeSubstitution(self: *Scope, identifier: []const u8, typeid: typed.TypeId, span: ?files.Span) TypeError!void {

        std.debug.print("Type Sub {s} -> {s}\n", .{identifier, self.builder.getScope(typeid).?.allocFullName()});

        try self.addDecl(
            identifier, 
            span, 
            .{ .Type = typeid },
            .public);
    }

    pub fn addFunction(self: *Scope, identifier: []const u8, typeid: typed.TypeId, is_inline: bool, requires_self: bool, visability: Visability, span: ?files.Span) TypeError!void {

        std.debug.print("Function {s} {s} to {s}\n", .{@tagName(visability), identifier, self.allocFullName()});

        const functionid = self.builder.addFunction(typeid, is_inline, requires_self);

        try self.addDecl(
            identifier, 
            span, 
            .{ .Function = functionid, },
            visability);
    }

    pub fn getType(self: *Scope, identifier: []const u8, opt_span: ?files.Span, visability: Visability) TypeError!?typed.TypeId {
        
        if (self.declarations.get(identifier)) |decl| {

            if (visability == .public and decl.visability != .public) {
                var log = self.builder.logger.logError(
                    "Visability Error", .{}, 
                    "You are trying to access a declaration within a private block.", .{});
                if (opt_span) |span| {
                    log.addLine( 
                        "Accesser is here.", .{}, 
                        span);
                }
                if (decl.span) |span| {
                    log.addLine(
                        "Declaration is in another scope in a private block.", .{}, 
                        span);
                }
                return TypeError.Visability;
            }

            switch (decl.decl_type) {
                
                .Type => |typeid| {
                    return typeid;
                },

                else => {
                    var log = self.builder.logger.logError(
                        "Type Error", .{}, 
                        null, .{});
                    if (opt_span) |span| {
                        log.addLine( 
                            "Declaration is not an type.", .{}, 
                            span);
                    }
                    if (decl.span) |span| {
                        log.addLine(
                            "This is the types declaration.", .{}, 
                            span);
                    }
                    return TypeError.InvalidType;
                },
            }
        }

        return null;
    }

    pub fn addGenericType(self: *Scope, identifier: []const u8, sub_list: std.ArrayList([]const u8), base: untyped.Node(untyped.Expression), visability: Visability, span: ?files.Span) TypeError!void {

        std.debug.print("Generic Type {s} {s} to {s}", .{@tagName(visability), identifier, self.allocFullName()});

        try self.addDecl(
            identifier, 
            span, 
            .{ .Generic = .{
                .base = base,
                .sub_identifiers = sub_list,
            },},
            visability);
    }

    pub fn getGenericType(self: *Scope, identifier: []const u8, opt_span: ?files.Span, visability: Visability) TypeError!?*Generic {

        if (self.declarations.getPtr(identifier)) |decl| {

            if (visability == .public and decl.visability != .public) {
                var log = self.builder.logger.logError(
                    "Visability Error", .{}, 
                    "You are trying to access a declaration within a private block.", .{});
                if (opt_span) |span| {
                    log.addLine( 
                        "Accesser is here.", .{}, 
                        span);
                }
                if (decl.span) |span| {
                    log.addLine(
                        "Declaration is in another scope in a private block.", .{}, 
                        span);
                }
                return TypeError.Visability;
            }

            switch (decl.decl_type) {
                
                .Generic => |*generic| {
                    return generic;
                },

                else => {
                    var log = self.builder.logger.logError(
                        "Type Error", .{}, 
                        "If it is meant to be generic add [] to the type declaration. Type[T]", .{});
                    if (opt_span) |span| {
                        log.addLine(
                            "This type is not a generic.", .{}, 
                            span);
                    }
                    if (decl.span) |span| {
                        log.addLine(
                            "This is the types declaration.", .{}, 
                            span);
                    }
                    return TypeError.InvalidType;
                },
            }
        } else return null;
    }

    pub fn getPublicGenericType(self: *Scope, identifier: []const u8, sub_types: std.ArrayList(typed.TypeId), ast: *untyped.Ast, node: ?untyped.Node(untyped.Expression)) TypeError!?typed.TypeId {
        
        if (self.declarations.get(identifier)) |*decl| {

            if (decl.visability != .public) {
                var log = self.builder.logger.logError(
                    "Visability Error", .{}, 
                    "You are trying to access a declaration within a private block.", .{});
                if (node) |ident_node| {
                    log.addLine(
                        "Accesser is here.", .{}, 
                        ident_node.span);
                }
                if (decl.node) |decl_node| {
                    log.addLine(
                        "Declaration is in another scope in a private block.", .{}, 
                        decl_node.span);
                }
                return TypeError.InvalidType;
            }
            return try self.getGenericType(identifier, sub_types, ast, node);
        }

        return null;
    }

    pub fn addField(self: *Scope, identifier: []const u8, span: files.Span, visability: Visability, type_ref: typed.TypeRef) TypeError!void {
        std.debug.print("Added Field: {s} to {s}\n", .{identifier, self.allocFullName()});
        
        switch (self.scope_type) {

            .Object => {

                try self.addDecl(
                    identifier, 
                    span, 
                    .{ .Field = type_ref }, 
                    visability);

                self.fields.put(
                    self.builder.allocator,
                    identifier,
                    self.fields,
                );

                self.fields += 1;
            },

            .Enum => {

                var value = std.ArrayList(typed.TypeRef).empty;
                value.append(self.builder.allocator, .{ 
                    .id = self.builder.bit8,
                    .is_ref = false,
                }) catch @panic("Out of Memory.");

                try self.addDecl(
                    identifier, 
                    span, 
                    .{ .InlineExpression = typed.TypedNode(typed.Expression).init(
                        self.builder.allocator,
                        span,
                        value,
                        .{ .Int = self.fields }
                    )}, 
                    visability);

                self.fields.put(
                    self.builder.allocator,
                    identifier,
                    self.fields,
                );

                self.fields += 1;
            },

            else => unreachable,
        }
    }

    pub fn addDecl(self: *Scope, identifier: []const u8, span: ?files.Span, decl_type: DeclarationType, visability: Visability) TypeError!void {
        const decl = self.declarations.getOrPut(self.builder.allocator, identifier) catch @panic("Out of Memory.");

        if (decl.found_existing) {
            decl.value_ptr.collisions.append(self.builder.allocator, span) catch @panic("Out of Memory.");
            return TypeError.MultipleDefinitions;
        }

        decl.value_ptr.* = .{
            .span = span,
            .visability = visability,
            .decl_type = decl_type,
        };
    }

    pub fn getDecl(self: *Scope, identifier: []const u8) ?Declaration {
        return self.declarations.get(identifier);
    }

    pub fn createTypedStatement(self: *Scope, old_node: untyped.Node(untyped.Statement), stmt: typed.Statement, value: std.ArrayList(typed.TypeRef)) typed.TypedNode(typed.Statement) {
        return typed.TypedNode(typed.Statement).init(
            self.builder.allocator, 
            old_node.span,
            value, 
            stmt);
    }

    pub fn createTypedExpression(self: *Scope, old_node: untyped.Node(untyped.Expression), expr: typed.Expression, value: std.ArrayList(typed.TypeRef)) typed.TypedNode(typed.Expression) {
        return typed.TypedNode(typed.Expression).init(
            self.builder.allocator, 
            old_node.span,
            value, 
            expr);
    }

    pub const TypeResult = union(enum) {
        Match,
        RequiresConversion: typed.FunctionId,
        Failed,
    };

    pub fn typesAreCompatible(self: *Scope, lhs: typed.TypeRef, rhs: typed.TypeRef) TypeResult {

        if (lhs.cmp(rhs)) {
            return .Match;
        }

        if (self.builder.conversions.get(.{
            .from = rhs,
            .to = lhs,
        })) |conversion| {
            return .{ .RequiresConversion = conversion.decl_type.Function };
        }

        return .Failed;
    }
};
