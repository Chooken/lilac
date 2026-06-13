const std = @import("std");
const tokens = @import("tokens.zig");
const untyped = @import("untyped.zig");
const typed = @import("typed.zig");
const logging = @import("logger.zig");

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

pub const BuilderSettings = struct {
    bitNativeSize: usize,
    warnOnOperatorTypeChange: bool,
};  

pub const Builder = struct {

    allocator: std.mem.Allocator,

    logger: logging.Logger,

    settings: BuilderSettings,

    program: typed.Program = .{},
    uprogram: *untyped.Program,

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

    conversions: std.AutoHashMapUnmanaged(typed.Conversion, Declaration) = .empty,
    binop_operation: std.AutoHashMapUnmanaged(typed.BinopOperator, Declaration) = .empty,
    prefix_operation: std.AutoHashMapUnmanaged(typed.UnaryOperator, Declaration) = .empty,

    pub fn getNewType(self: *Builder, name: ?[]const u8, parent: ?*Scope) typed.TypeId {
        const typeid = self.program.addType(self.allocator, .{
            .name = name,
        });

        const scope = self.allocator.create(Scope) catch @panic("Out of Memory.");

        scope.* = .{
            .builder = self,
            .parent = parent,
            .typeid = typeid,
        };

        self.scopes.put(self.allocator, typeid, scope) catch @panic("Out of Memory.");

        return typeid;
    }

    pub fn getOrAddFunctionType(self: *Builder, proto: typed.FunctionProto) typed.TypeId {

        const result = self.program.func_types.getOrPut(self.allocator, proto) catch @panic("Out of Memory.");

        if (!result.found_existing) {

            std.debug.print("Added function type func (", .{});

            for (proto.inputs.items) |input| {
                if (input.is_ref) {
                    std.debug.print("ref ", .{});
                }
                if (self.getScope(input.id)) |scope| {
                    std.debug.print("{s}", .{scope.allocFullName()});
                }
            }
            
            std.debug.print(") ", .{});

            if (proto.outputs.items.len == 0) {
                std.debug.print("nothing", .{});
            }

            for (proto.outputs.items) |output| {
                if (output.is_ref) {
                    std.debug.print("ref ", .{});
                }
                if (self.getScope(output.id)) |scope| {
                    std.debug.print("{s}", .{scope.allocFullName()});
                }
            }
            std.debug.print("\n", .{});

            result.value_ptr.* = self.program.addType(self.allocator, typed.Type{
                .name = null,
                .data = .{
                    .Function = proto
                },
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

pub const Collision = struct {
    node: ?untyped.Node(untyped.Expression),
};

pub const Scope = struct {
    builder: *Builder,
    parent: ?*Scope,
    typeid: typed.TypeId,

    usings: std.ArrayList(typed.TypeId) = .empty,
    alias: std.StringHashMapUnmanaged(typed.TypeId) = .empty,

    declarations: std.StringHashMapUnmanaged(Declaration) = .empty,

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

    pub fn addTypeDecl(self: *Scope, identifier: []const u8, visability: typed.Visability, node: ?untyped.Node(untyped.Expression)) TypeError!typed.TypeId {

        std.debug.print("Type {s} {s} to {s}\n", .{@tagName(visability), identifier, self.allocFullName()});

        const typeid= self.builder.getNewType(identifier, self);
        try self.addDecl(
            identifier, 
            node, 
            .{ .Type = typeid, },
            visability);
        return typeid;
    }

    pub fn addTypeSubstitution(self: *Scope, identifier: []const u8, typeid: typed.TypeId, node: ?untyped.Node(untyped.Expression)) TypeError!void {

        std.debug.print("Type Sub {s} -> {s}\n", .{identifier, self.builder.getScope(typeid).?.allocFullName()});

        try self.addDecl(
            identifier, 
            node, 
            .{ .Type = typeid },
            .public);
    }

    pub fn getFunctionTypeId(self: *Scope, proto: *untyped.FuncPrototype, ast: *untyped.Ast, allow_raw_type_in_args: bool, allow_raw_type_in_return: bool) ?typed.TypeId {

        var proto_type = typed.FunctionProto { };

        if (proto.arguments) |arguements| {
            switch (arguements.data.*) {

                .Self => {
                    proto_type.inputs.append(self.builder.allocator, .{
                        .id = self.typeid,
                        .is_ref = true,
                    }) catch @panic("Out of Memory.");
                },

                .Declaration => |decl| {
                    if (ExprToTypeRef(self, ast, decl.decl_type)) |typeref| {
                        proto_type.inputs.append(self.builder.allocator, typeref) catch @panic("Out of Memory.");
                    } else {
                        return null;
                    }
                },

                .List => |list| {
                    for (list.expressions.items) |expr| {
                        switch (expr.data.*) {

                            .Self => {
                                proto_type.inputs.append(self.builder.allocator, .{
                                    .id = self.typeid,
                                    .is_ref = true,
                                }) catch @panic("Out of Memory.");
                            },

                            .Declaration => |decl| {
                                if (ExprToTypeRef(self, ast, decl.decl_type)) |typeref| {
                                    proto_type.inputs.append(self.builder.allocator, typeref) catch @panic("Out of Memory.");
                                } else {
                                    return null;
                                }
                            },
                             
                            else => {

                                if (!allow_raw_type_in_args) {
                                    var log = self.builder.logger.logError(
                                        "Invalid Type", .{}, 
                                        null);
                                    log.addLine(
                                        self.builder.allocator, 
                                        ast.file, 
                                        "Invalid function parameter type.", .{}, 
                                        proto.returns.start, 
                                        proto.returns.end);
                                    return null;
                                }

                                if (ExprToTypeRef(self, ast, proto.returns)) |typeref|
                                    proto_type.inputs.append(
                                        self.builder.allocator, 
                                        typeref) catch @panic("Out of Memory.") else return null;
                            },
                        }
                    }
                },

                else => {

                    if (!allow_raw_type_in_args) {
                        var log = self.builder.logger.logError(
                            "Invalid Type", .{}, 
                            null);
                        log.addLine(
                            self.builder.allocator, 
                            ast.file, 
                            "Invalid function parameter type.", .{}, 
                            proto.returns.start, 
                            proto.returns.end);
                        return null;
                    }

                    if (ExprToTypeRef(self, ast, proto.returns)) |typeref|
                        proto_type.inputs.append(
                            self.builder.allocator, 
                            typeref) catch @panic("Out of Memory.") else return null;
                },
            }
        }

        switch (proto.returns.data.*) {

            .Nothing => {},

            .Self => {
                proto_type.outputs.append(self.builder.allocator, .{
                    .id = self.typeid,
                    .is_ref = false,
                }) catch @panic("Out of Memory.");
            },

            .Declaration => |decl| {
                if (ExprToTypeRef(self, ast, decl.decl_type)) |typeref| {
                    proto_type.outputs.append(self.builder.allocator, typeref) catch @panic("Out of Memory.");
                }
            },

            .List => |list| {
                
                for (list.expressions.items, 0..) |expr, index| {
                    switch (expr.data.*) {

                        .Nothing => {
                            var log = self.builder.logger.logError(
                                "Invalid Type", .{}, 
                                "Remove the nothing. nothing, var: type -> var: type");
                            log.addLine(
                                self.builder.allocator, 
                                ast.file, 
                                "You can't have nothing in a list with something.", .{}, 
                                expr.start, 
                                expr.end);
                            return null;
                        },

                        .Self => {
                            if (index > 0) {
                                var log = self.builder.logger.logError(
                                    "Invalid Type", .{}, 
                                    "func () other: type, self -> func () self, other: type");
                                log.addLine(
                                    self.builder.allocator, 
                                    ast.file, 
                                    "You can only have the self result in the first position.", .{}, 
                                    expr.start, 
                                    expr.end);
                                return null;
                            }
                            proto_type.outputs.append(self.builder.allocator, .{
                                .id = self.typeid,
                                .is_ref = false,
                            }) catch @panic("Out of Memory.");
                        },

                        .Declaration => |decl| {
                            if (ExprToTypeRef(self, ast, decl.decl_type)) |typeref| {
                                proto_type.outputs.append(self.builder.allocator, typeref) catch @panic("Out of Memory.");
                            }
                        },
                            
                        else => {

                            if (!allow_raw_type_in_return) {
                                var log = self.builder.logger.logError(
                                    "Invalid Type", .{}, 
                                    null);
                                log.addLine(
                                    self.builder.allocator, 
                                    ast.file, 
                                    "Invalid function return type.", .{}, 
                                    proto.returns.start, 
                                    proto.returns.end);
                                return null;
                            }

                            if (ExprToTypeRef(self, ast, proto.returns)) |typeref|
                                proto_type.outputs.append(
                                    self.builder.allocator, 
                                    typeref) catch @panic("Out of Memory.") else return null;
                        },
                    }
                }
            },

            else => {

                if (!allow_raw_type_in_return) {
                    var log = self.builder.logger.logError(
                        "Invalid Type", .{}, 
                        null);
                    log.addLine(
                        self.builder.allocator, 
                        ast.file, 
                        "Invalid function return type.", .{}, 
                        proto.returns.start, 
                        proto.returns.end);
                    return null;
                }

                if (ExprToTypeRef(self, ast, proto.returns)) |typeref|
                    proto_type.outputs.append(
                        self.builder.allocator, 
                        typeref) catch @panic("Out of Memory.") else return null;
            },
        }

        return self.builder.getOrAddFunctionType(proto_type);
    }

    pub fn addFunction(self: *Scope, identifier: []const u8, typeid: typed.TypeId, is_inline: bool, requires_self: bool, visability: typed.Visability, node: ?untyped.Node(untyped.Expression)) TypeError!void {

        std.debug.print("Function {s} {s} to {s}\n", .{@tagName(visability), identifier, self.allocFullName()});

        const functionid = self.builder.addFunction(typeid, is_inline, requires_self);

        try self.addDecl(
            identifier, 
            node, 
            .{ .Function = functionid, },
            visability);
    }

    pub fn getPublicType(self: *Scope, identifier: []const u8, node: ?untyped.Node(untyped.Expression)) TypeError!?typed.TypeId {
        if (self.declarations.get(identifier)) |decl| {

            if (decl.visability != .public) {
                var log = self.builder.logger.logError(
                    "Visability Error", .{}, 
                    "You are trying to access a declaration within a private block.");
                if (node) |ident_node| {
                    log.addLine(
                        self.builder.allocator, 
                        ident_node.file_id, 
                        "Accesser is here.", .{}, 
                        ident_node.start, ident_node.end);
                }
                if (decl.node) |decl_node| {
                    log.addLine(
                        self.builder.allocator, 
                        decl_node.file_id, 
                        "Declaration is in another scope in a private block.", .{}, 
                        decl_node.start, decl_node.end);
                }
                return TypeError.Visability;
            }

            return try self.getType(identifier, node);
        }

        return null;
    }

    pub fn getType(self: *Scope, identifier: []const u8, node: ?untyped.Node(untyped.Expression)) TypeError!?typed.TypeId {
        
        if (self.declarations.get(identifier)) |decl| {

            switch (decl.decl_type) {
                
                .Type => |typeid| {
                    return typeid;
                },

                else => {
                    var log = self.builder.logger.logError(
                        "Type Error", .{}, 
                        null);
                    if (node) |ident_node| {
                        log.addLine(
                            self.builder.allocator, 
                            ident_node.file_id, 
                            "Declaration is not an type.", .{}, 
                            ident_node.start, ident_node.end);
                    }
                    if (decl.node) |decl_node| {
                        log.addLine(
                            self.builder.allocator, 
                            decl_node.file_id, 
                            "This is the types declaration.", .{}, 
                            decl_node.start, decl_node.end);
                    }
                    return TypeError.InvalidType;
                },
            }
        }

        return null;
    }

    pub fn addGenericType(self: *Scope, identifier: []const u8, generic: *untyped.Generic, base: untyped.Node(untyped.Expression), visability: typed.Visability, ast: *untyped.Ast, node: untyped.Node(untyped.Expression)) TypeError!void {

        std.debug.print("Generic Type {s} {s}", .{@tagName(visability), ast.source[generic.callee.start..generic.arguements.end]});
        std.debug.print("] to {s}\n", .{self.allocFullName()});
        
        var sub_list = std.ArrayList([]const u8).empty;

        switch (generic.arguements.data.*) {
            
            .Identifier => |ident| {
                const token = ast.tokens[ident.token_index];
                const name = ast.source[token.start..token.end];
                sub_list.append(self.builder.allocator, name) catch @panic("Out of Memory.");
            },

            .List => |list| {
                for (list.expressions.items) |expr| {
                    switch (expr.data.*) {
                        
                        .Identifier => |ident| {
                            const token = ast.tokens[ident.token_index];
                            const name = ast.source[token.start..token.end];
                            sub_list.append(self.builder.allocator, name) catch @panic("Out of Memory.");
                        },

                        else => {
                            var log = self.builder.logger.logError(
                                "Invalid Generic", .{}, 
                                "You can only use names in a generic list. Generic[T, T2]");
                            log.addLine(
                                self.builder.allocator, 
                                ast.file, 
                                "Invalid format for a generic.", .{}, 
                                generic.arguements.start, 
                                generic.arguements.end);
                            return;
                        }
                    }
                }
            },

            else => {
                var log = self.builder.logger.logError(
                    "Invalid Generic", .{}, 
                    "You can only use names in a generic list. Generic[T, T2]");
                log.addLine(
                    self.builder.allocator, 
                    ast.file, 
                    "Invalid format for a generic.", .{}, 
                    generic.arguements.start, 
                    generic.arguements.end);
                return;
            }
        }

        try self.addDecl(
            identifier, 
            node, 
            .{ .Generic = .{
                .base = base,
                .sub_identifiers = sub_list,
            },},
            visability);
    }

    pub fn getGenericType(self: *Scope, identifier: []const u8, sub_types: std.ArrayList(typed.TypeId), ast: *untyped.Ast, node: ?untyped.Node(untyped.Expression)) TypeError!?typed.TypeId {

        var gen: *Generic = undefined;
        var found_node: ?untyped.Node(untyped.Expression) = undefined;
        
        if (self.declarations.getPtr(identifier)) |decl| {

            found_node = decl.node;

            switch (decl.decl_type) {
                
                .Generic => |*generic| {
                    gen = generic;
                },

                else => {
                    var log = self.builder.logger.logError(
                        "Type Error", .{}, 
                        "If it is meant to be generic add [] to the type declaration. Type[T]");
                    if (node) |ident_node| {
                        log.addLine(
                            self.builder.allocator, 
                            ident_node.file_id, 
                            "This type is not a generic.", .{}, 
                            ident_node.start, ident_node.end);
                    }
                    if (decl.node) |decl_node| {
                        log.addLine(
                            self.builder.allocator, 
                            decl_node.file_id, 
                            "This is the types declaration.", .{}, 
                            decl_node.start, decl_node.end);
                    }
                    return TypeError.InvalidType;
                },
            }
        } else return null;

        if (sub_types.items.len != gen.sub_identifiers.items.len) {
            return null;
        }

        cache_loop: for (gen.cache.items) |gen_type| {

            for (0..gen_type.sub_types.items.len) |index| {
                if (gen_type.sub_types.items[index].index != sub_types.items[index].index) {
                    continue :cache_loop;
                }
            }

            return gen_type.typeid;
        }

        std.debug.print("Type {s}[", .{identifier});
        std.debug.print("{s}", .{self.builder.getScope(sub_types.items[0]).?.allocFullName()});

        for (sub_types.items[1..]) |typeid| {
            std.debug.print(", {s}", .{self.builder.getScope(typeid).?.allocFullName()});
        }

        std.debug.print("] to {s}\n", .{self.allocFullName()});

        const typeid = self.builder.getNewType(identifier, self);

        var gen_type = self.builder.getType(typeid);
        var body: untyped.Node(untyped.Block) = undefined;

        switch (gen.base.data.*) {

            .Object => |obj| {
                gen_type.data = .{ .Object = .{} };
                body = obj;
            },  

            .Enum => |_enum| {
                gen_type.data = .{ .Enum = .{} };
                body = _enum;
            },

            .Interface => |interface| {
                gen_type.data = .{ .Interface = .{} };
                body = interface;
            },

            else => {
                var log = self.builder.logger.logError(
                    "Invalid Generic", .{}, 
                    "You can only put a generic on object, enum, and interfaces.");
                log.addLine(
                    self.builder.allocator, 
                    ast.file, 
                    "Invalid body for a Generic.", .{}, 
                    gen.base.start, 
                    gen.base.end);
                return null;
            },
        }

        gen.cache.append(self.builder.allocator, .{
            .sub_types = sub_types,
            .typeid = typeid,
        }) catch @panic("Out of Memory.");

        if (self.builder.getScope(typeid)) |type_scope| {
            for (0..sub_types.items.len) |index| {
                type_scope.addTypeSubstitution(gen.sub_identifiers.items[index], sub_types.items[index], found_node) catch continue;
            }
            collectTypeIdsFromBlock(type_scope, ast, body.data, .public);
            collectTypeDataFromBlock(type_scope, ast, body.data, .public);
        }

        return typeid;
    }

    pub fn getPublicGenericType(self: *Scope, identifier: []const u8, sub_types: std.ArrayList(typed.TypeId), ast: *untyped.Ast, node: ?untyped.Node(untyped.Expression)) TypeError!?typed.TypeId {
        
        if (self.declarations.get(identifier)) |*decl| {

            if (decl.visability != .public) {
                var log = self.builder.logger.logError(
                    "Visability Error", .{}, 
                    "You are trying to access a declaration within a private block.");
                if (node) |ident_node| {
                    log.addLine(
                        self.builder.allocator, 
                        ident_node.file_id, 
                        "Accesser is here.", .{}, 
                        ident_node.start, ident_node.end);
                }
                if (decl.node) |decl_node| {
                    log.addLine(
                        self.builder.allocator, 
                        decl_node.file_id, 
                        "Declaration is in another scope in a private block.", .{}, 
                        decl_node.start, decl_node.end);
                }
                return TypeError.InvalidType;
            }
            return try self.getGenericType(identifier, sub_types, ast, node);
        }

        return null;
    }

    pub fn getFunction(self: *Scope, identifier: []const u8, node: ?untyped.Node(untyped.Expression)) TypeError!?typed.FunctionId {
        const opt_decl = self.declarations.get(identifier);

        if (opt_decl) |decl| {
            switch (decl.decl_type) {

                .Function => |func_id| {
                    return func_id;
                },

                else => {
                    var log = self.builder.logger.logError(
                        "Type Error", .{}, 
                        null);
                    if (node) |ident_node| {
                        log.addLine(
                            self.builder.allocator, 
                            ident_node.file_id, 
                            "Declaration is not an type.", .{}, 
                            ident_node.start, ident_node.end);
                    }
                    if (decl.node) |decl_node| {
                        log.addLine(
                            self.builder.allocator, 
                            decl_node.file_id, 
                            "This is the types declaration.", .{}, 
                            decl_node.start, decl_node.end);
                    }
                    return TypeError.InvalidType;
                }
            }
        }

        return null;
    }

    pub fn addField(self: *Scope, identifier: []const u8, node: untyped.Node(untyped.Expression), visability: typed.Visability, type_ref: typed.TypeRef) TypeError!void {
        std.debug.print("Added Field: {s} to {s}\n", .{identifier, self.allocFullName()});
        try self.addDecl(
            identifier, 
            node, 
            .{ .Field = type_ref }, 
            visability);
    }

    pub fn addDecl(self: *Scope, identifier: []const u8, node: ?untyped.Node(untyped.Expression), decl_type: DeclarationType, visability: typed.Visability) TypeError!void {
        const decl = self.declarations.getOrPut(self.builder.allocator, identifier) catch @panic("Out of Memory.");

        if (decl.found_existing) {
            decl.value_ptr.collisions.append(self.builder.allocator, node) catch @panic("Out of Memory.");
            return TypeError.MultipleDefinitions;
        }

        decl.value_ptr.* = .{
            .node = node,
            .visability = visability,
            .decl_type = decl_type,
        };
    }

    pub fn createTypedStatement(self: *Scope, old_node: untyped.Node(untyped.Statement), stmt: typed.Statement, value: std.ArrayList(typed.TypeRef)) typed.TypedNode(typed.Statement) {
        return typed.TypedNode(typed.Statement).init(
            self.builder.allocator, 
            old_node.start, 
            old_node.end, 
            old_node.file_id,
            value, 
            stmt);
    }

    pub fn createTypedExpression(self: *Scope, old_node: untyped.Node(untyped.Expression), expr: typed.Expression, value: std.ArrayList(typed.TypeRef)) typed.TypedNode(typed.Expression) {
        return typed.TypedNode(typed.Expression).init(
            self.builder.allocator, 
            old_node.start, 
            old_node.end, 
            old_node.file_id,
            value, 
            expr);
    }

    pub fn typeEql(self: *Scope, lhs: typed.TypedNode(typed.Expression), rhs: *typed.TypedNode(typed.Expression)) bool {

        if (lhs.value.items.len != rhs.value.items.len) {

            var log = self.builder.logger.logError(
                "Type Error", .{}, 
                null);
            log.addLine(
                self.builder.allocator, 
                lhs.file_id, 
                "Has {d} Types", .{lhs.value.items.len}, 
                lhs.start, lhs.end);
            log.addLine(
                self.builder.allocator, 
                lhs.file_id, 
                "Has {d} Types", .{rhs.value.items.len}, 
                rhs.start, rhs.end);

            return false;
        }

        if (lhs.value.items.len < 1) {
            return true;
        }

        if (lhs.value.items.len == 1) {
            const lhs_type = lhs.value.items[0];
            const rhs_type = rhs.value.items[0];

            if (lhs_type.cmp(rhs_type)) {
                return true;
            }

            if (self.builder.conversions.get(.{
                .from = rhs_type,
                .to = lhs_type,
            })) |conversion| {

                const conv = typed.Call {
                    .arguements = rhs.*,
                    .callee = conversion.decl_type.Function
                };

                rhs.* = typed.TypedNode(typed.Expression).init(
                    self.builder.allocator, 
                    rhs.start, rhs.end, 
                    rhs.file_id, 
                    lhs.value, 
                    .{ .Call = conv });

                return true;
            }

            const lhs_scope = self.builder.getScope(lhs_type.id);
            const rhs_scope = self.builder.getScope(rhs_type.id);

            var log = self.builder.logger.logError(
                "Type Error", .{}, 
                "The left and right types do not match.");
            if (lhs_scope) |scope| {
                log.addLine(
                    self.builder.allocator, 
                    lhs.file_id, 
                    "Has Type: {s}{s}", .{ if (lhs_type.is_ref) "ref " else "", scope.allocFullName()}, 
                    lhs.start, lhs.end);
            }
            if (rhs_scope) |scope| {
                log.addLine(
                    self.builder.allocator, 
                    lhs.file_id, 
                    "Has Type: {s}{s}", .{ if (rhs_type.is_ref) "ref " else "", scope.allocFullName()}, 
                    rhs.start, rhs.end);
            }

            return false;
        }

        var split = typed.Split {
            .results = .empty,
            .value = rhs.*,
        };

        for (0..lhs.value.items.len) |index| {

            const lhs_type = lhs.value.items[index];
            const rhs_type = rhs.value.items[index];

            if (lhs_type.cmp(rhs_type)) {
                
                split.results.append(self.builder.allocator, 
                    typed.TypedNode(typed.Expression).init(
                        self.builder.allocator, 
                        rhs.start, rhs.end, rhs.file_id, 
                        lhs.value, 
                        .{ .SplitLiteral = .{ .index = index } })) catch @panic("Out of Memory.");
                continue;
            }

            if (self.builder.conversions.get(
                .{ .from = rhs_type, .to = lhs_type }
            )) |conversion| {

                const conv_call = typed.Call {
                    .arguements = typed.TypedNode(typed.Expression).init(
                        self.builder.allocator, 
                        rhs.start, rhs.end, rhs.file_id, 
                        lhs.value, 
                        .{ .SplitLiteral = .{ .index = index } }),
                    .callee = conversion.decl_type.Function
                };

                const conv_expr = typed.TypedNode(typed.Expression).init(
                    self.builder.allocator, 
                    rhs.start, rhs.end, 
                    rhs.file_id, 
                    lhs.value, 
                    .{ .Call = conv_call });

                split.results.append(self.builder.allocator, 
                    conv_expr) catch @panic("Out of Memory.");
            }

            var log = self.builder.logger.logError(
                "Type Error", .{}, 
                "The left and right types do not match.");

            var buffer: [1024]u8 = undefined;
            var buf_index: usize = 0;
            
            for (lhs.value.items) |item| {
                const item_scope = self.builder.getScope(item.id);
                if (item_scope) |scope| {
                    const section = std.fmt.bufPrint(buffer[buf_index..], "{s}{s}", .{ if (item.is_ref) "ref " else "", scope.allocFullName() }) catch @panic("Buffer Print Error.");
                    buf_index += section.len;
                }
            }

            log.addLine(
                self.builder.allocator, 
                lhs.file_id, 
                "Has Type: {s}", .{buffer[0..buf_index]}, 
                lhs.start, lhs.end);

            buf_index = 0;

            for (rhs.value.items) |item| {
                const item_scope = self.builder.getScope(item.id);
                if (item_scope) |scope| {
                    const section = std.fmt.bufPrint(buffer[buf_index..], "{s}{s}", .{ if (item.is_ref) "ref " else "", scope.allocFullName() }) catch @panic("Buffer Print Error.");
                    buf_index += section.len;
                }
            }

            log.addLine(
                self.builder.allocator, 
                lhs.file_id, 
                "Has Type: {s}", .{buffer[0..buf_index]}, 
                rhs.start, rhs.end);

            return false;
        }

        rhs.* = typed.TypedNode(typed.Expression).init(
            self.builder.allocator, 
            rhs.start, rhs.end, rhs.file_id, 
            lhs.value, 
            .{ .Split = split });
        return true;
    }
};

pub const Declaration = struct {
    node: ?untyped.Node(untyped.Expression),
    visability: typed.Visability,
    decl_type: DeclarationType,
    collisions: std.ArrayList(?untyped.Node(untyped.Expression)) = .empty,
};

pub const DeclarationType = union(enum) {
    Field: typed.TypeRef,
    Type: typed.TypeId,
    Function: typed.FunctionId,
    Generic: Generic,
};

pub const Generic = struct {
    base: untyped.Node(untyped.Expression),
    sub_identifiers: std.ArrayList([]const u8) = .empty,
    cache: std.ArrayList(GenericTypeCache) = .empty,
};

pub const GenericTypeCache = struct {
    sub_types: std.ArrayList(typed.TypeId),
    typeid: typed.TypeId,
};

pub fn runSema(allocator: std.mem.Allocator, uprogram: *untyped.Program, settings: BuilderSettings) typed.Program {

    var builder = Builder {
        .allocator = allocator,
        .logger = logging.Logger {
            .allocator = allocator,
        },
        .settings = settings,
        .uprogram = uprogram,
        .root = undefined,
    };
    defer builder.deinit();

    std.debug.print("Collecting ID's\n", .{});
    collectTypeIds(&builder);
    std.debug.print("Collecting Type Data\n", .{});
    collectTypeData(&builder);
    std.debug.print("Generate Typed Ast\n", .{});
    generate(&builder);
    logCollisionErrors(&builder);

    logging.printLogs(builder.logger, allocator);

    return builder.program;
}

pub fn collectTypeIds(builder: *Builder) void {

    builder.root = builder.getNewType("Root", null);
    const module_type = builder.getType(builder.root);
    module_type.data = .{ .Module = .{} };
    
    if (builder.getScope(builder.root)) |scope| {

        builder.bit8 = scope.addTypeDecl("@bit8", .public, null) catch @panic("@bit8 was already added to root scope.");
        builder.bit16 = scope.addTypeDecl("@bit16", .public, null) catch @panic("@bit16 was already added to root scope.");
        builder.bit32 = scope.addTypeDecl("@bit32", .public, null) catch @panic("@bit32 was already added to root scope.");
        builder.bit64 = scope.addTypeDecl("@bit64", .public, null) catch @panic("@bit64 was already added to root scope.");
        builder.bitNative = scope.addTypeDecl("@bitNative", .public, null) catch @panic("@bitNative was already added to root scope.");
        builder.numberLiteral = scope.addTypeDecl("@numberLiteral", .public, null) catch @panic("@numberLiteral was already added to root scope.");
        builder.unknown = scope.addTypeDecl("unknown", .public, null) catch @panic("unknown was already added to root scope.");
        builder.nothing = scope.addTypeDecl("nothing", .public, null) catch @panic("nothing was already added to root scope.");

        collectTypeIdsFromModule(scope, &builder.uprogram.root_module);
    }
}

pub fn collectTypeIdsFromModule(scope: *Scope, module: *untyped.Module) void {
    for (module.asts.items) |*ast| {
        collectTypeIdsFromBlock(scope, ast, &ast.root_block, .public);
    }

    var sub_mod_iter = module.submodules.iterator();

    while (sub_mod_iter.next()) |sub_mod_entry| {
        const typeid = scope.addTypeDecl(sub_mod_entry.key_ptr.*, .public, null) catch return;
        const module_type = scope.builder.getType(typeid);
        module_type.data = .{ .Module = .{} };

        if (scope.builder.getScope(typeid)) |sub_scope| {
            collectTypeIdsFromModule(sub_scope, sub_mod_entry.value_ptr);
        }
    }
}

pub fn collectTypeIdsFromBlock(scope: *Scope, ast: *untyped.Ast, block: *untyped.Block, visability: typed.Visability) void {
    for (block.body.items) |statement| {
        collectTypeIdsFromStatements(scope, ast, statement, visability);
    }
}

pub fn collectTypeIdsFromStatements(scope: *Scope, ast: *untyped.Ast, statement: untyped.Node(untyped.Statement), visability: typed.Visability) void {
    
    switch (statement.data.*) {

        .Block => |block| {
            collectTypeIdsFromBlock(scope, ast, block.data, visability);
        },

        .Expression => |expr| {
            collectTypeIdsFromExpressions(scope, ast, expr, visability);
        },

        .Private => |private| {
            collectTypeIdsFromBlock(scope, ast, private.data, .private);
        },

        else => return,
    }
}

pub fn collectTypeIdsFromExpressions(scope: *Scope, ast: *untyped.Ast, expression: untyped.Node(untyped.Expression), visability: typed.Visability) void {
    
    switch (expression.data.*) {

        .Assignment => |assignment| {

            const token = ast.tokens[assignment.op_token_index];

            switch (token.token_type) {

                .FatRightArrow => {
                    return;
                },

                else => {
                    var log = scope.builder.logger.logError(
                        "Assignment Error", .{}, 
                        "You can't assign to a field.");
                    log.addLine(
                        scope.builder.allocator, 
                        ast.file, 
                        "Assignment here.", .{}, 
                        token.start, token.end);
                    return;
                }
            }
        },

        .Declaration => |decl| {

            switch (decl.name.data.*) {

                .Identifier => |ident| {

                    const token = ast.tokens[ident.token_index];

                    switch (decl.decl_type.data.*) {

                        .Object => |obj| {
                            const typeid = scope.addTypeDecl(ast.source[token.start..token.end], visability, decl.name) catch return;
                            const obj_type = scope.builder.getType(typeid);
                            obj_type.data = .{ .Object = .{} };

                            for (obj.data.body.items) |child_statement| {
                                collectTypeIdsFromStatements(scope, ast, child_statement, .public);
                            }
                        },  

                        .Enum => |_enum| {
                            const typeid = scope.addTypeDecl(ast.source[token.start..token.end], visability, decl.name) catch return;
                            const obj_type = scope.builder.getType(typeid);
                            obj_type.data = .{ .Enum = .{} };

                            for (_enum.data.body.items) |child_statement| {
                                collectTypeIdsFromStatements(scope, ast, child_statement, .public);
                            }
                        },

                        .Interface => |interfaces| {
                            const typeid = scope.addTypeDecl(ast.source[token.start..token.end], visability, decl.name) catch return;
                            const obj_type = scope.builder.getType(typeid);
                            obj_type.data = .{ .Interface = .{} };

                            for (interfaces.data.body.items) |child_statement| {
                                collectTypeIdsFromStatements(scope, ast, child_statement, .public);
                            }
                        },

                        else => return,
                    }
                },

                .Generic => |*generic| {

                    const base = decl.decl_type;
                    
                    switch (generic.callee.data.*) {

                        .Identifier => |ident| {
                            
                            const token = ast.tokens[ident.token_index];

                            scope.addGenericType(ast.source[token.start..token.end], generic, base, visability, ast, decl.name) catch return;
                        },

                        else => {
                            var log = scope.builder.logger.logError(
                                "Invalid Declaration", .{}, 
                                "Names can only use a..z, 0..9 and _");
                            log.addLine(
                                scope.builder.allocator, 
                                ast.file, 
                                "Invalid name for a declaration.", .{}, 
                                decl.name.start, 
                                decl.name.end);
                        },
                    }
                },

                // Built-ins don't need to be added
                .Builtin => return,

                else => {
                    var log = scope.builder.logger.logError(
                        "Invalid Declaration", .{}, 
                        "Names can only use a..z, 0..9 and _");
                    log.addLine(
                        scope.builder.allocator, 
                        ast.file, 
                        "Invalid name for a declaration.", .{}, 
                        decl.name.start, 
                        decl.name.end);
                },
            }
        },

        .List => |list| {
            
            for (list.expressions.items) |expr| {
                collectTypeIdsFromExpressions(scope, ast, expr, visability);
            }
        },

        else => return,
    }
}

pub fn collectTypeData(builder: *Builder) void {

    if (builder.getScope(builder.root)) |scope| {
        collectTypeDataModule(scope, &builder.uprogram.root_module);
    }
}

pub fn collectTypeDataModule(scope: *Scope, module: *untyped.Module) void {
    for (module.asts.items) |*ast| {
        collectTypeDataFromBlock(scope, ast, &ast.root_block, .public);
    }

    var sub_mod_iter = module.submodules.iterator();

    while (sub_mod_iter.next()) |sub_mod_entry| {
        if (scope.getType(sub_mod_entry.key_ptr.*, null) catch continue) |typeid| {

            if (scope.builder.getScope(typeid)) |sub_scope| {
                collectTypeDataModule(sub_scope, sub_mod_entry.value_ptr);
            }
        }
    }
}

pub fn collectTypeDataFromBlock(scope: *Scope, ast: *untyped.Ast, block: *untyped.Block, visability: typed.Visability) void {
    for (block.body.items) |stmt| {
        collectTypeDataFromStatements(scope, ast, stmt, visability);
    }
}

pub fn collectTypeDataFromStatements(scope: *Scope, ast: *untyped.Ast, statement: untyped.Node(untyped.Statement), visability: typed.Visability) void {
    
    switch (statement.data.*) {

        .Block => |block| {
            collectTypeDataFromBlock(scope, ast, block.data, visability);
        },

        .Expression => |expr| {
            collectTypeDataFromExpressions(scope, ast, expr, visability);
        },

        .Private => |private| {
            collectTypeDataFromBlock(scope, ast, private.data, .private);
        },

        else => return,
    }
}

pub fn collectTypeDataFromExpressions(scope: *Scope, ast: *untyped.Ast, expression: untyped.Node(untyped.Expression), visability: typed.Visability) void {

    switch (expression.data.*) {

        .Identifier => |ident| {
            
            const token = ast.tokens[ident.token_index];
            const name = ast.source[token.start..token.end];
            const self_type = scope.builder.getType(scope.typeid);

            if (self_type.data) |*data| {
                
                switch (data.*) {

                    .Enum => |*_enum| {

                        std.debug.print("Added Field: {s} to {s}\n", .{name, scope.allocFullName()});

                        _enum.structure.append(scope.builder.allocator, typed.Field {
                            .visability = visability,
                            .name = name,
                            .type_ref = null,
                        }) catch @panic("Out of Memory");
                    },

                    else => {
                        var log = scope.builder.logger.logWarning(
                            "Invalid Declaration", .{}, 
                            "Did you forget the type or mean to get rid of it?");
                        log.addLine(
                            scope.builder.allocator, 
                            ast.file, 
                            "You have a identifier without a type in a {s}.", .{@tagName(data.*)}, 
                            expression.start, 
                            expression.end);
                        return;
                    }
                }
            }
        },

        .Declaration => |decl| {

            switch (decl.name.data.*) {

                .Identifier => |ident| {

                    const token = ast.tokens[ident.token_index];
                    const name = ast.source[token.start..token.end];

                    switch (decl.decl_type.data.*) {

                        .Object => |obj| {
                            if (scope.getType(name, decl.name) catch return) |typeid| {
                                if (scope.builder.getScope(typeid)) |new_scope| {
                                    collectTypeDataFromBlock(new_scope, ast, obj.data, .public);
                                }
                            }
                        },  

                        .Enum => |_enum| {
                            if (scope.getType(name, decl.name) catch return) |typeid| {
                                if (scope.builder.getScope(typeid)) |new_scope| {
                                    collectTypeDataFromBlock(new_scope, ast, _enum.data, .public);
                                }
                            }
                        },

                        .Interface => |interfaces| {
                            if (scope.getType(name, decl.name) catch return) |typeid| {
                                if (scope.builder.getScope(typeid)) |new_scope| {
                                    collectTypeDataFromBlock(new_scope, ast, interfaces.data, .public);
                                }
                            }
                        },

                        // Will need to do setter for func decl.
                        .Function => |function| {

                            var requires_self = false;

                            var result: ?typed.TypeId = null;
                            
                            switch (function.prototype.data.*) {

                                .FuncPrototype => |*proto| {
                                    result = scope.getFunctionTypeId(
                                        proto, 
                                        ast, 
                                        false, 
                                        function.is_inline);

                                    if (proto.arguments) |args| {
                                        switch (args.data.*) {
                                            .Self => requires_self = true,

                                            .List => |list| {
                                                switch(list.expressions.items[0].data.*) {
                                                    .Self => requires_self = true,
                                                    else => {},
                                                }
                                            },
                                            else => {},
                                        }
                                    }
                                },
                               
                                else => @panic("Type other then function prototype in prototype slot.")
                            }

                            if (result) |typeid| {
                                scope.addFunction(
                                    name, 
                                    typeid, 
                                    function.is_inline, 
                                    requires_self, 
                                    visability, 
                                    decl.name) catch return;
                            }
                        },

                        else => {
                            addFieldToTypeData(
                                scope, 
                                ast, 
                                name, 
                                decl.name,
                                decl.decl_type, 
                                visability);
                        },
                    }
                },

                .Builtin => |builtin| {
                    
                    const token = ast.tokens[builtin.token_index];
                    const identifier = ast.source[token.start..token.end];

                    const function = switch (decl.decl_type.data.*) {

                        .Function => |*f| f,

                        else => {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "You can only declare builtin functions like: @init: func () self");
                            log.addLine(
                                scope.builder.allocator, 
                                ast.file, 
                                "This type isn't a function. Is it meant to be a builtin?", .{}, 
                                decl.decl_type.start, decl.decl_type.end);
                            return;
                        }
                    };

                    const proto = switch (function.prototype.data.*) {
                        .FuncPrototype => |*proto| proto,
                        else => {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "Functions can only have function prototype as types.");
                            log.addLine(
                                scope.builder.allocator, 
                                ast.file, 
                                "This type isn't a function prototype.", .{}, 
                                decl.decl_type.start, decl.decl_type.end);
                            return;
                        }
                    };

                    if (std.mem.eql(u8, "@init", identifier)) {

                        if (proto.arguments) |args| {
                            switch (args.data.*) {
                                .Self => {
                                    var log = scope.builder.logger.logError(
                                        "Type Error", .{}, 
                                        "@init can't have self as a parameter.");
                                    log.addLine(
                                        scope.builder.allocator, 
                                        ast.file, 
                                        "Self parameter declared here.", .{}, 
                                        args.start, args.end);
                                    return;
                                },

                                .List => |list| {
                                    switch (list.expressions.items[0].data.*) {
                                        .Self => {
                                            var log = scope.builder.logger.logError(
                                                "Type Error", .{}, 
                                                "@init can't have self as a parameter.");
                                            log.addLine(
                                                scope.builder.allocator, 
                                                ast.file, 
                                                "Self parameter declared here.", .{}, 
                                                list.expressions.items[0].start, list.expressions.items[0].end);
                                            return;
                                        },

                                        else => {},
                                    }
                                },

                                else => {},
                            }
                        }

                        if (proto.returns.data.* != .Self) {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@init requires self as the only return.");
                            log.addLine(
                                scope.builder.allocator, 
                                ast.file, 
                                "This needs to just be self.", .{}, 
                                proto.returns.start, proto.returns.end);
                            return;
                        }

                        if (scope.getFunctionTypeId(proto, ast, false, function.is_inline)) |typeid| {

                            const funct_id = scope.builder.addFunction(typeid, function.is_inline, false);

                            const result = scope.builder.initialisers.getOrPut(scope.builder.allocator, scope.typeid) catch @panic("Out of Memory.");

                            if (result.found_existing) {
                                result.value_ptr.collisions.append(scope.builder.allocator, decl.name) catch @panic("Out of Memory.");
                                return;
                            }

                            result.value_ptr.* = .{
                                .node = decl.name,
                                .visability = .public,
                                .decl_type = .{
                                    .Function = funct_id,
                                }
                            };
                        }
                    } 
                    else if (std.mem.eql(u8, "@on_override", identifier)) {

                        if (proto.arguments) |args| {
                            if (args.data.* != .Self) {
                                var log = scope.builder.logger.logError(
                                    "Type Error", .{}, 
                                    "@on_override requires have self as the only parameter.");
                                log.addLine(
                                    scope.builder.allocator, 
                                    ast.file, 
                                    "Only put self in here.", .{}, 
                                    args.start, args.end);
                                return;
                            }
                        } else {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@on_override requires have self as the parameter.");
                            log.addLine(
                                scope.builder.allocator, 
                                ast.file, 
                                "Add self as a parameter.", .{}, 
                                function.prototype.start, function.prototype.end);
                            return;
                        }

                        if (proto.returns.data.* != .Nothing) {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@on_override requires have nothing as the return.");
                            log.addLine(
                                scope.builder.allocator, 
                                ast.file, 
                                "Add nothing here.", .{}, 
                                proto.returns.start, proto.returns.end);
                            return;
                        }

                        if (scope.getFunctionTypeId(proto, ast, false, function.is_inline)) |typeid| {

                            const funct_id = scope.builder.addFunction(typeid, function.is_inline, false);

                            const result = scope.builder.onoverride.getOrPut(scope.builder.allocator, scope.typeid) catch @panic("Out of Memory.");

                            if (result.found_existing) {
                                result.value_ptr.collisions.append(scope.builder.allocator, decl.name) catch @panic("Out of Memory.");
                                return;
                            }

                            result.value_ptr.* = .{
                                .node = decl.name,
                                .visability = .public,
                                .decl_type = .{
                                    .Function = funct_id,
                                }
                            };
                        }
                    } 
                    else if (std.mem.eql(u8, "@on_copy", identifier)) {
                        
                        if (proto.arguments) |args| {
                            if (args.data.* != .Self) {
                                var log = scope.builder.logger.logError(
                                    "Type Error", .{}, 
                                    "@on_copy requires have self as the only parameter.");
                                log.addLine(
                                    scope.builder.allocator, 
                                    ast.file, 
                                    "Only put self in here.", .{}, 
                                    args.start, args.end);
                                return;
                            }
                        } else {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@on_copy requires have self as the parameter.");
                            log.addLine(
                                scope.builder.allocator, 
                                ast.file, 
                                "Add self as a parameter.", .{}, 
                                function.prototype.start, function.prototype.end);
                            return;
                        }

                        if (proto.returns.data.* != .Nothing) {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@on_copy requires have nothing as the return.");
                            log.addLine(
                                scope.builder.allocator, 
                                ast.file, 
                                "Add nothing here.", .{}, 
                                proto.returns.start, proto.returns.end);
                            return;
                        }

                        if (scope.getFunctionTypeId(proto, ast, false, function.is_inline)) |typeid| {

                            const funct_id = scope.builder.addFunction(typeid, function.is_inline, false);

                            const result = scope.builder.oncopy.getOrPut(scope.builder.allocator, scope.typeid) catch @panic("Out of Memory.");

                            if (result.found_existing) {
                                result.value_ptr.collisions.append(scope.builder.allocator, decl.name) catch @panic("Out of Memory.");
                                return;
                            }

                            result.value_ptr.* = .{
                                .node = decl.name,
                                .visability = .public,
                                .decl_type = .{
                                    .Function = funct_id,
                                }
                            };
                        }
                    } 
                    else if (std.mem.eql(u8, "@on_drop", identifier)) {
                        
                        if (proto.arguments) |args| {
                            if (args.data.* != .Self) {
                                var log = scope.builder.logger.logError(
                                    "Type Error", .{}, 
                                    "@on_drop requires have self as the only parameter.");
                                log.addLine(
                                    scope.builder.allocator, 
                                    ast.file, 
                                    "Only put self in here.", .{}, 
                                    args.start, args.end);
                                return;
                            }
                        } else {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@on_drop requires have self as the parameter.");
                            log.addLine(
                                scope.builder.allocator, 
                                ast.file, 
                                "Add self as a parameter.", .{}, 
                                function.prototype.start, function.prototype.end);
                            return;
                        }

                        if (proto.returns.data.* != .Nothing) {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@on_drop requires have nothing as the return.");
                            log.addLine(
                                scope.builder.allocator, 
                                ast.file, 
                                "Add nothing here.", .{}, 
                                proto.returns.start, proto.returns.end);
                            return;
                        }

                        if (scope.getFunctionTypeId(proto, ast, false, function.is_inline)) |typeid| {

                            const funct_id = scope.builder.addFunction(typeid, function.is_inline, false);

                            const result = scope.builder.ondrop.getOrPut(scope.builder.allocator, scope.typeid) catch @panic("Out of Memory.");

                            if (result.found_existing) {
                                result.value_ptr.collisions.append(scope.builder.allocator, decl.name) catch @panic("Out of Memory.");
                                return;
                            }

                            result.value_ptr.* = .{
                                .node = decl.name,
                                .visability = .public,
                                .decl_type = .{
                                    .Function = funct_id,
                                }
                            };
                        }
                    } 
                    else if (std.mem.eql(u8, "@conversion", identifier)) {

                        if (proto.arguments) |args| {
                            switch (args.data.*) {

                                .Self => {
                                    var log = scope.builder.logger.logWarning(
                                        "Concerning self type.", .{}, 
                                        "@conversion with a ref type as a from.");
                                    log.addLine(
                                        scope.builder.allocator, 
                                        ast.file, 
                                        "The self type is a ref type.", .{}, 
                                        args.start, args.end);
                                },

                                .List => {
                                    var log = scope.builder.logger.logError(
                                        "Type Error", .{}, 
                                        "@conversion can't have multiple parameters.");
                                    log.addLine(
                                        scope.builder.allocator, 
                                        ast.file, 
                                        "Here are the parameters.", .{}, 
                                        args.start, args.end);
                                    return;
                                },

                                else => {}
                            }
                        } else {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@conversion requires a parameter as the from type.");
                            log.addLine(
                                scope.builder.allocator, 
                                ast.file, 
                                "Add a from parameter.", .{}, 
                                function.prototype.start, function.prototype.end);
                            return;
                        }

                        switch (proto.returns.data.*) {

                            .List => {
                                var log = scope.builder.logger.logError(
                                    "Type Error", .{}, 
                                    "@conversion can't have multiple results.");
                                log.addLine(
                                    scope.builder.allocator, 
                                    ast.file, 
                                    "Here are the parameters.", .{}, 
                                    proto.returns.start, proto.returns.end);
                                return;
                            },

                            else => {},
                        }

                        if (scope.getFunctionTypeId(proto, ast, false, function.is_inline)) |typeid| {

                            const func_type = scope.builder.getType(typeid);

                            const funct_id = scope.builder.addFunction(typeid, function.is_inline, false);

                            const result = scope.builder.conversions.getOrPut(scope.builder.allocator, .{ 
                                .from = func_type.data.?.Function.inputs.items[0], 
                                .to = func_type.data.?.Function.outputs.items[0]
                            }) catch @panic("Out of Memory.");

                            if (result.found_existing) {
                                result.value_ptr.collisions.append(scope.builder.allocator, decl.name) catch @panic("Out of Memory.");
                                return;
                            }
                            
                            const from_scope = scope.builder.getScope(result.key_ptr.from.id).?;
                            const to_scope = scope.builder.getScope(result.key_ptr.to.id).?;

                            std.debug.print("Added conversion {s} -> {s}\n", .{from_scope.allocFullName(), to_scope.allocFullName()});

                            result.value_ptr.* = .{
                                .node = decl.name,
                                .visability = .public,
                                .decl_type = .{
                                    .Function = funct_id,
                                }
                            };
                        }
                    } 
                    else if (std.mem.eql(u8, "@add", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, ast, function, proto, .Plus);
                    } 
                    else if (std.mem.eql(u8, "@sub", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, ast, function, proto, .Minus);
                    } 
                    else if (std.mem.eql(u8, "@mul", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, ast, function, proto, .Asterisk);
                    } 
                    else if (std.mem.eql(u8, "@div", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, ast, function, proto, .Slash);
                    } 
                    else if (std.mem.eql(u8, "@mod", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, ast, function, proto, .Percentage);
                    } 
                    else if (std.mem.eql(u8, "@less_than", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, ast, function, proto, .LessThan);
                    } 
                    else if (std.mem.eql(u8, "@greater_than", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, ast, function, proto, .GreaterThan);
                    } 
                    else if (std.mem.eql(u8, "@less_than_or_equal", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, ast, function, proto, .LessThanOrEquals);
                    } 
                    else if (std.mem.eql(u8, "@greater_than_or_equal", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, ast, function, proto, .GreaterThanOrEquals);
                    }
                    else if (std.mem.eql(u8, "@negate", identifier)) {
                        addPrefixOperatorFunction(scope, identifier, decl.name, ast, function, proto, .Minus);
                    } 
                },

                // Built-ins don't need to be added.
                // Generics get genned by usage not declaration.
                .Generic => return,

                else => {
                    var log = scope.builder.logger.logError(
                        "Invalid Declaration", .{}, 
                        "Names can only use a..z, 0..9 and _");
                    log.addLine(
                        scope.builder.allocator, 
                        ast.file, 
                        "Invalid name for a declaration.", .{}, 
                        decl.name.start, 
                        decl.name.end);
                },
            }
        },

        .Assignment => |assignment| {
            collectTypeDataFromExpressions(scope, ast, assignment.assignee, visability);
        },

        .List => |list| {
            for (list.expressions.items) |expr| {
                collectTypeDataFromExpressions(scope, ast, expr, visability);
            }
        },

        else => return,
    }
}

pub fn addFieldToTypeData(scope: *Scope, ast: *untyped.Ast, identifier: []const u8, node: untyped.Node(untyped.Expression), type_expr: untyped.Node(untyped.Expression), visability: typed.Visability) void {
    if (ExprToTypeRef(scope, ast, type_expr)) |type_ref| {
        scope.addField(identifier, node, visability, type_ref) catch return;
    }
}

pub fn ExprToTypeRef(scope: *Scope, ast: *untyped.Ast, expression: untyped.Node(untyped.Expression)) ?typed.TypeRef {

    var current_scope: *Scope = scope;
    var expr = expression;
    var public_only = false;
    var is_ref = false;

    switch (expression.data.*) {
        .Unary => |unary| {
            const unary_token = ast.tokens[unary.op_token_index];

            if (unary_token.token_type == .Reference) {
                expr = unary.right;
                is_ref = true;
            } else {
                var log = scope.builder.logger.logError(
                    "Invalid Type", .{}, 
                    "Name: Type or Name: ref Type");
                log.addLine(
                    scope.builder.allocator, 
                    ast.file, 
                    "Invalid unary operator on type.", .{}, 
                    unary_token.start, 
                    unary_token.end);
            }
        },
        
        else => {},
    }


    while (true) {

        const result = isTypeInScope(scope, current_scope, ast, expr, public_only) catch return null;
        
        if (result) |found_scope| {
            return typed.TypeRef{
                .id = found_scope,
                .is_ref = is_ref,
            };
        }

        if (current_scope.parent) |parent_scope| {
            current_scope = parent_scope;
        } else {
            break;
        }

        public_only = true;
    }

    var log = scope.builder.logger.logError(
        "Invalid Type", .{}, 
        "Did you forget an import or spell it wrong?");
    log.addLine(
        scope.builder.allocator, 
        ast.file, 
        "Type doesn't exist in scope.", .{}, 
        expression.start, 
        expression.end);

    return null;
}

pub fn isTypeInScope(access_scope: *Scope, scope: *Scope, ast: *untyped.Ast, expression: untyped.Node(untyped.Expression), public_only: bool) TypeError!?typed.TypeId {
    switch (expression.data.*) {

        .Member => |member| {

            const result = try isTypeInScope(access_scope, scope, ast, member.parent, public_only);

            if (result) |parent_typeid| {

                if (scope.builder.getScope(parent_typeid)) |parent_scope| {
                    return isTypeInScope(access_scope, parent_scope, ast, member.child, true);
                }

                return null;
            }

            return null;
        },

        .Identifier => |ident| {
            const token = ast.tokens[ident.token_index];
            const name = ast.source[token.start..token.end];

            return if (public_only) scope.getPublicType(name, expression) else scope.getType(name, expression);
        },

        .Unknown => {
            if (scope.builder.getScope(scope.builder.root)) |root_scope| {

                if (try root_scope.getType("unknown", expression)) |typeid| {
                    return typeid;
                } else {
                    var log = scope.builder.logger.logError(
                        "Invalid Type", .{}, 
                        "This is a compiler error, contact the developer.");
                    log.addLine(
                        scope.builder.allocator, 
                        ast.file, 
                        "Failed to get unknown type.", .{}, 
                        expression.start, 
                        expression.end);
                    return TypeError.InvalidType;
                }
            }

            return null;
        },

        .Builtin => |builtin| {
            if (scope.builder.getScope(scope.builder.root)) |root_scope| {
                const token = ast.tokens[builtin.token_index];
                
                if (try root_scope.getType(ast.source[token.start..token.end], expression)) |typeid| {
                    return typeid;
                } else {
                    var log = scope.builder.logger.logError(
                        "Invalid Builtin Type", .{}, 
                        "built-in types include: @bit8, @bit16, @bit32, @bit64, @bitNative, @numberLiteral");
                    log.addLine(
                        scope.builder.allocator, 
                        ast.file, 
                        "Built-in type \x22{s}\x22 doesn't exist.", .{ast.source[token.start..token.end]}, 
                        expression.start, 
                        expression.end);
                    return TypeError.InvalidType;
                }
            }
            
            return null;
        },

        .FuncPrototype => |*proto| {
            return scope.getFunctionTypeId(
                proto, 
                ast, 
                true, 
                true);
        },

        .Generic => |*generic| {

            var sub_list = std.ArrayList(typed.TypeId).empty;

            switch (generic.arguements.data.*) {
                
                .Identifier, .Builtin => {
                    const result = ExprToTypeRef(access_scope, ast, generic.arguements);

                    if (result) |typeref| {
                        sub_list.append(scope.builder.allocator, typeref.id) catch @panic("Out of Memory.");
                    } else {
                        var log = scope.builder.logger.logError(
                            "Invalid Type", .{}, 
                            "Did you forget an import or spell it wrong?");
                        log.addLine(
                            scope.builder.allocator, 
                            ast.file, 
                            "Type doesn't exist in {s} scope.", .{scope.allocFullName()}, 
                            generic.arguements.start, 
                            generic.arguements.end);
                        return TypeError.InvalidType;
                    }
                },

                .List => |list| {
                    for (list.expressions.items) |expr| {
                        const result = ExprToTypeRef(access_scope, ast, expr);

                        if (result) |typeref| {
                            if (typeref.is_ref) {
                                var log = scope.builder.logger.logError(
                                    "Invalid Type", .{}, 
                                    "Generics can't have ref types as parameters.");
                                log.addLine(
                                    scope.builder.allocator, 
                                    ast.file, 
                                    "ref type here.", .{}, 
                                    expression.start, 
                                    expression.end);
                                return TypeError.InvalidType;
                            }
                            sub_list.append(scope.builder.allocator, typeref.id) catch @panic("Out of Memory.");
                        } else {
                            var log = scope.builder.logger.logError(
                                "Invalid Type", .{}, 
                                "Did you forget an import or spell it wrong?");
                            log.addLine(
                                scope.builder.allocator, 
                                ast.file, 
                                "Type doesn't exist in {s} module scope.", .{scope.allocFullName()}, 
                                expression.start, 
                                expression.end);
                            return TypeError.InvalidType;
                        }
                    }
                },

                else => {
                    var log = scope.builder.logger.logError(
                        "Invalid Type", .{}, 
                        "Generics can't have ref types or nothing");
                    log.addLine(
                        scope.builder.allocator, 
                        ast.file, 
                        "Invalid type for generic.", .{}, 
                        expression.start, 
                        expression.end);
                    return TypeError.InvalidType;
                },
            }

            switch (generic.callee.data.*) {

                .Identifier => |ident| {
                    
                    const token = ast.tokens[ident.token_index];

                    if (public_only) {
                        return try scope.getPublicGenericType(ast.source[token.start..token.end], sub_list, ast, expression);
                    }
                    return try scope.getGenericType(ast.source[token.start..token.end], sub_list, ast, expression);
                },

                else => @panic("Generic callee can't be anything but identifier."),
            }
        },

        else => {
            var log = scope.builder.logger.logError(
                "Invalid Type", .{}, 
                "Types can only be names, members, or functions.");
            log.addLine(
                scope.builder.allocator, 
                ast.file, 
                "Invalid Type Name.", .{}, 
                expression.start, 
                expression.end);
            return TypeError.InvalidType;
        }
    }
}

pub fn addBinopOperatorFunction(scope: *Scope, name: []const u8, name_node: untyped.Node(untyped.Expression), ast: *untyped.Ast, func: *untyped.Function, proto: *untyped.FuncPrototype, operation: tokens.TokenType) void {

    if (proto.arguments) |args| {
        switch (args.data.*) {

            .List => |list| {
                
                if (list.expressions.items.len != 2) {
                    var log = scope.builder.logger.logError(
                        "Type Error", .{}, 
                        std.fmt.allocPrint(
                            scope.builder.allocator, 
                            "{s} requires two parameters for the left and right sides of the operator", 
                            .{name}) catch @panic("Out of Memory."));
                    log.addLine(
                        scope.builder.allocator, 
                        ast.file, 
                        "Remove some parameters.", .{}, 
                        args.start, args.end);
                    return;
                }
            },

            else => {
                var log = scope.builder.logger.logError(
                    "Type Error", .{}, 
                    std.fmt.allocPrint(
                        scope.builder.allocator, 
                        "{s} requires two parameters for the left and right sides of the operator", 
                        .{name}) catch @panic("Out of Memory."));
                log.addLine(
                    scope.builder.allocator, 
                    ast.file, 
                    "Add another parameter.", .{}, 
                    args.start, args.end);
                return;
            }
        }
    } else {
        var log = scope.builder.logger.logError(
            "Type Error", .{}, 
            std.fmt.allocPrint(
                scope.builder.allocator, 
                "{s} requires two parameters for the left and right sides of the operator", 
                .{name}) catch @panic("Out of Memory."));
        log.addLine(
            scope.builder.allocator, 
            ast.file, 
            "Add a left and right parameter.", .{}, 
            func.prototype.start, func.prototype.end);
        return;
    }

    switch (proto.returns.data.*) {

        .Nothing => {
            var log = scope.builder.logger.logWarning(
                "Concerning Operator", .{}, 
                "Operators typically return a value. This could be a sign you are using them weirdly.");
            log.addLine(
                scope.builder.allocator, 
                ast.file, 
                "Did you mean to put nothing here?", .{}, 
                proto.returns.start, proto.returns.end);
        },

        .List => {
            var log = scope.builder.logger.logWarning(
                "Concerning Operator", .{}, 
                "Operators typically return a value. This could be a sign you are using them weirdly.");
            log.addLine(
                scope.builder.allocator, 
                ast.file, 
                "Did you mean to put multiple returns here?", .{}, 
                proto.returns.start, proto.returns.end);
        },

        else => {},
    }

    if (scope.getFunctionTypeId(proto, ast, false, func.is_inline)) |typeid| {

        const func_type = scope.builder.getType(typeid);

        const funct_id = scope.builder.addFunction(typeid, func.is_inline, false);

        const result = scope.builder.binop_operation.getOrPut(scope.builder.allocator, .{ 
            .lhs = func_type.data.?.Function.inputs.items[0],
            .rhs = func_type.data.?.Function.inputs.items[1],
            .op = operation,
        }) catch @panic("Out of Memory.");

        if (result.found_existing) {
            result.value_ptr.collisions.append(scope.builder.allocator, name_node) catch @panic("Out of Memory.");
            return;
        }
        
        const lhs_scope = scope.builder.getScope(result.key_ptr.lhs.id).?;
        const rhs_scope = scope.builder.getScope(result.key_ptr.rhs.id).?;

        std.debug.print("Added op {s} {s} {s}\n", .{lhs_scope.allocFullName(), operation.toString(), rhs_scope.allocFullName()});

        result.value_ptr.* = .{
            .node = name_node,
            .visability = .public,
            .decl_type = .{
                .Function = funct_id,
            }
        };
    }
}

pub fn addPrefixOperatorFunction(scope: *Scope, name: []const u8, name_node: untyped.Node(untyped.Expression), ast: *untyped.Ast, func: *untyped.Function, proto: *untyped.FuncPrototype, operation: tokens.TokenType) void {

    if (proto.arguments) |args| {
        switch (args.data.*) {

            .List => {
                
                var log = scope.builder.logger.logError(
                    "Type Error", .{}, 
                    std.fmt.allocPrint(
                        scope.builder.allocator, 
                        "{s} requires only one parameter for the unary operator.", 
                        .{name}) catch @panic("Out of Memory."));
                log.addLine(
                    scope.builder.allocator, 
                    ast.file, 
                    "Remove some parameters.", .{}, 
                    args.start, args.end);
                return;
            },

            else => {}
        }
    } else {
        var log = scope.builder.logger.logError(
            "Type Error", .{}, 
            std.fmt.allocPrint(
                scope.builder.allocator, 
                "{s} requires one parameters for the unary operator.", 
                .{name}) catch @panic("Out of Memory."));
        log.addLine(
            scope.builder.allocator, 
            ast.file, 
            "Add a parameter.", .{}, 
            func.prototype.start, func.prototype.end);
        return;
    }

    switch (proto.returns.data.*) {

        .Nothing => {
            var log = scope.builder.logger.logWarning(
                "Concerning Operator", .{}, 
                "Operators typically return a value. This could be a sign you are using them weirdly.");
            log.addLine(
                scope.builder.allocator, 
                ast.file, 
                "Did you mean to put nothing here?", .{}, 
                proto.returns.start, proto.returns.end);
        },

        .List => {
            var log = scope.builder.logger.logWarning(
                "Concerning Operator", .{}, 
                "Operators typically return a value. This could be a sign you are using them weirdly.");
            log.addLine(
                scope.builder.allocator, 
                ast.file, 
                "Did you mean to put multiple returns here?", .{}, 
                proto.returns.start, proto.returns.end);
        },

        else => {},
    }

    if (scope.getFunctionTypeId(proto, ast, false, func.is_inline)) |typeid| {

        const func_type = scope.builder.getType(typeid);

        const funct_id = scope.builder.addFunction(typeid, func.is_inline, false);

        const result = scope.builder.prefix_operation.getOrPut(scope.builder.allocator, .{ 
            .value = func_type.data.?.Function.inputs.items[0],
            .op = operation,
        }) catch @panic("Out of Memory.");

        if (result.found_existing) {
            result.value_ptr.collisions.append(scope.builder.allocator, name_node) catch @panic("Out of Memory.");
            return;
        }
        
        const value_scope = scope.builder.getScope(result.key_ptr.value.id).?;

        std.debug.print("Added op {s} {s}\n", .{operation.toString(), value_scope.allocFullName()});

        result.value_ptr.* = .{
            .node = name_node,
            .visability = .public,
            .decl_type = .{
                .Function = funct_id,
            }
        };
    }
}

pub fn generate(builder: *Builder) void {

    if (builder.getScope(builder.root)) |scope| {
        generateModule(scope, &builder.uprogram.root_module);
    }
}

pub fn generateModule(scope: *Scope, module: *untyped.Module) void {
    for (module.asts.items) |*ast| {
        generateFromBlock(scope, ast, &ast.root_block);
    }

    var sub_mod_iter = module.submodules.iterator();

    while (sub_mod_iter.next()) |sub_mod_entry| {
        if (scope.getType(sub_mod_entry.key_ptr.*, null) catch continue) |typeid| {

            if (scope.builder.getScope(typeid)) |sub_scope| {
                generateModule(sub_scope, sub_mod_entry.value_ptr);
            }
        }
    }
}

pub fn generateFromBlock(scope: *Scope, ast: *untyped.Ast, block: *untyped.Block) void {
    for (block.body.items) |stmt| {
        generateFromStatements(scope, ast, stmt);
    }
}

pub fn generateFromStatements(scope: *Scope, ast: *untyped.Ast, statement: untyped.Node(untyped.Statement)) void {
    
    switch (statement.data.*) {

        .Block => |block| {
            generateFromBlock(scope, ast, block.data);
        },

        .Expression => |expr| {
            generateFromExpressions(scope, ast, expr);
        },

        .Private => |private| {
            generateFromBlock(scope, ast, private.data);
        },

        else => return,
    }
}

pub fn generateFromExpressions(scope: *Scope, ast: *untyped.Ast, expression: untyped.Node(untyped.Expression)) void {

    switch (expression.data.*) {

        .Declaration => |decl| {

            const ident = switch (decl.name.data.*) {
                
                .Identifier => |ident| ident,

                else => return,
            };

            const token = ast.tokens[ident.token_index];
            const name = ast.source[token.start..token.end];
            
            switch (decl.decl_type.data.*) {
                
                .Function => |*function| {
                    if (scope.getFunction(name, decl.name) catch return) |func_id| {

                        std.debug.print("generating {s}\n", .{name});
                        generateFunction(scope, ast, function, name, func_id);
                    }
                },

                else => {},
            }
        },

        else => {},
    }
}

pub fn generateFunction(scope: *Scope, ast: *untyped.Ast, function: *untyped.Function, func_name: []const u8, func_id: typed.FunctionId) void {

    const prototype = switch (function.prototype.data.*) {

        .FuncPrototype => |proto| proto,

        else => return,
    };

    const funct = scope.builder.getFunction(func_id);
    const proto_type = scope.builder.getType(funct.typeid).data.?.Function;

    const funct_scope = scope.builder.getScope(scope.builder.getNewType(func_name, scope)).?;

    if (prototype.arguments) |args| {

        switch (args.data.*) {

            .Declaration => |decl| {

                const ident = switch (decl.name.data.*) {
                    
                    .Identifier => |ident| ident,

                    else => return,
                };
                
                const token = ast.tokens[ident.token_index];
                const name = ast.source[token.start..token.end];

                funct_scope.addField(name, args, .public, proto_type.inputs.items[0]) catch return;
            }, 

            .List => |list| {

                for (list.expressions.items, 0..) |arg, index| {

                    switch (arg.data.*) {

                        .Declaration => |decl| {
                            const ident = switch (decl.name.data.*) {
                                
                                .Identifier => |ident| ident,

                                else => return,
                            };
                            
                            const token = ast.tokens[ident.token_index];
                            const name = ast.source[token.start..token.end];

                            funct_scope.addField(name, arg, .public, proto_type.inputs.items[index]) catch return;
                        },

                        else => return,
                    }
                }
            },

            else => return,
        }
    }

    switch (prototype.returns.data.*) {

        .Declaration => |decl| {

            const ident = switch (decl.name.data.*) {
                
                .Identifier => |ident| ident,

                else => return,
            };
            
            const token = ast.tokens[ident.token_index];
            const name = ast.source[token.start..token.end];

            funct_scope.addField(name, prototype.returns, .public, proto_type.inputs.items[0]) catch return;
        }, 

        .List => |list| {

            for (list.expressions.items, 0..) |return_, index| {

                switch (return_.data.*) {

                    .Declaration => |decl| {
                        const ident = switch (decl.name.data.*) {
                            
                            .Identifier => |ident| ident,

                            else => return,
                        };
                        
                        const token = ast.tokens[ident.token_index];
                        const name = ast.source[token.start..token.end];

                        funct_scope.addField(name, return_, .public, proto_type.outputs.items[index]) catch return;
                    },

                    else => return,
                }
            }
        },

        .Nothing => {},

        else => return,
    }

    
    const funct_ = scope.builder.getFunction(func_id);
    funct_.block = generateFunctionStatements(funct_scope, ast, function.body);
}

pub fn generateFunctionBlock(scope: *Scope, ast: *untyped.Ast, block: untyped.Node(untyped.Block)) typed.TypedNode(typed.Block) {

    const block_scope = scope.builder.getScope(scope.builder.getNewType(null, scope)).?;
    var typed_block = typed.Block {};

    for (block.data.body.items) |stmt| {
        typed_block.body.append(scope.builder.allocator, generateFunctionStatements(block_scope, ast, stmt)) catch @panic("Out of Memory.");
    }

    return typed.TypedNode(typed.Block).init(
        scope.builder.allocator, 
        block.start, block.end, block.file_id, 
        .empty, 
        typed_block);
}

pub fn generateFunctionStatements(scope: *Scope, ast: *untyped.Ast, statement: untyped.Node(untyped.Statement)) typed.TypedNode(typed.Statement) {

    switch (statement.data.*) {

        .Block => |block| {

            const typed_block = generateFunctionBlock(scope, ast, block);

            return scope.createTypedStatement(
                statement, 
                .{ 
                    .Block = typed_block,
                }, 
                typed_block.value);
        },

        .Loop => |body| {
            const loop = generateFunctionStatements(scope, ast, body);

            return scope.createTypedStatement(
                statement, 
                .{
                    .Loop = loop,
                },
                .empty);
        },

        .Expression => |expr| {
            const typed_expr = generateFunctionExpressions(scope, ast, expr, null) catch {
                return scope.createTypedStatement(
                    statement, 
                    .Error, 
                    .empty);
            };

            return scope.createTypedStatement(
                statement, 
                .{
                    .Expression = typed_expr ,
                }, 
                .empty);
        },

        .Return => return scope.createTypedStatement(
                statement, 
                .Return, 
                .empty),

        .Break => return scope.createTypedStatement(
                statement, 
                .Break, 
                .empty),

        .Continue => return scope.createTypedStatement(
                statement, 
                .Continue, 
                .empty),

        else => {
            return scope.createTypedStatement(
                statement, 
                .Error, 
                .empty);
        },
    }
}

pub fn generateFunctionExpressions(scope: *Scope, ast: *untyped.Ast, expression: untyped.Node(untyped.Expression), opt_inferred_type: ?typed.TypeRef) TypeError!typed.TypedNode(typed.Expression) {

    switch (expression.data.*) {
        
        .Assignment => |assignment| {

            var note_log = scope.builder.logger.logNote(
                "Assignment", .{}, 
                null);
            note_log.addLine(
                scope.builder.allocator, 
                expression.file_id, 
                "Assignment", .{}, 
                expression.start, expression.end);
            
            switch (assignment.assignee.data.*) {
                
                .Identifier, .Member, .Declaration => { },

                else => {
                    return scope.createTypedExpression(expression, .Error, .empty);
                }
            }

            const assignee = try generateFunctionExpressions(scope, ast, assignment.assignee, null);
            var value = try generateFunctionExpressions(scope, ast, assignment.value, assignee.getInferable());

            if (scope.typeEql(assignee, &value)) {
                return scope.createTypedExpression(
                    expression, 
                    .{ .Assignment = .{ 
                        .assignee = assignee, 
                        .value = value,
                        .op_token_type = ast.tokens[assignment.op_token_index].token_type } },
                    assignee.value);
            }

            return scope.createTypedExpression(expression, .Error, .empty);
        },

        .Declaration => |decl| {

            const name = switch (decl.name.data.*) {
                .Identifier => |identifer| identifer,

                else => {
                    return TypeError.InvalidType;
                }
            };

            const name_token = ast.tokens[name.token_index];
            const name_string = ast.source[name_token.start..name_token.end];

            const typed_decl = typed.Declaration {
                .name = typed.TypedNode(typed.Identifier).init(
                    scope.builder.allocator, 
                    decl.name.start, decl.name.end, decl.name.file_id, 
                    .empty, 
                    .{ .token_index = name.token_index }),
            };

            switch (decl.decl_type.data.*) {

                .Setter => |setter| {
                    if (ExprToTypeRef(scope, ast, setter.settee)) |decl_type_ref| {

                        try scope.addField(name_string, decl.name, .public, decl_type_ref);

                        var types = std.ArrayList(typed.TypeRef).empty;
                        types.append(scope.builder.allocator, decl_type_ref) catch @panic("Out of Memory.");

                        const typed_setter = typed.Setter {
                            .settee = decl_type_ref.id,
                            .body = generateFunctionBlock(scope, ast, setter.body),
                        };

                        const decl_node = scope.createTypedExpression(
                            expression, 
                            .{ .Declaration = typed_decl }, 
                            types);

                        const setter_node = scope.createTypedExpression(
                            decl.decl_type, 
                            .{ .Setter = typed_setter }, 
                            types);

                        const assignment = typed.Assignment {
                            .assignee = decl_node,
                            .value = setter_node,
                            .op_token_type = tokens.TokenType.Equals,
                        };

                        return scope.createTypedExpression(
                            expression,
                            .{ .Assignment = assignment }, 
                            .empty);
                    }
                },

                else => {
                    if (ExprToTypeRef(scope, ast, decl.decl_type)) |decl_type_ref| {

                        try scope.addField(name_string, decl.name, .public, decl_type_ref);

                        var types = std.ArrayList(typed.TypeRef).empty;
                        types.append(scope.builder.allocator, decl_type_ref) catch @panic("Out of Memory.");

                        return scope.createTypedExpression(
                            expression, 
                            .{ .Declaration = typed_decl }, 
                            types);
                    }
                }
            }

            return TypeError.InvalidType;
        },

        .Literal => |literal| {

            var types = std.ArrayList(typed.TypeRef).empty;
            
            const token = ast.tokens[literal.token_index];
            const literal_value = ast.source[token.start..token.end];
            
            switch (literal.literal_type) {

                .Number => types.append(scope.builder.allocator, .{ .id = scope.builder.numberLiteral, .is_ref = false }) catch @panic("Out of Memory."),

                .Bool => types.append(scope.builder.allocator, .{ .id = scope.builder.bit8, .is_ref = false }) catch @panic("Out of Memory."),

                .Binary => {

                    const bits_per_char: usize = switch (literal_value[1]) {
                        'b' => 1,
                        'x' => 4,
                        else => {
                            var log = scope.builder.logger.logError(
                                "Literal Error", .{},
                                null);

                            log.addLine(
                                scope.builder.allocator, 
                                expression.file_id, 
                                "Binary Literal Type doesn't exist.", .{}, 
                                expression.start, expression.end);

                            return TypeError.InvalidType;
                        }
                    };

                    var bit_size: usize = 0;
                    var padding: bool = true;

                    for (literal_value[2..]) |char| {

                        if (padding) {
                            if (char == '0') {
                                continue;
                            }
                            padding = false;
                        }

                        if (char != '_') {
                            bit_size += 1;
                        }
                    }

                    bit_size *= bits_per_char;

                    if (bit_size > 64) {

                        var log = scope.builder.logger.logError(
                            "Type Error", .{},
                            "Literals can only go up to 0xFFFF_FFFF_FFFF_FFFF or 64 bits. Use a smaller value.");

                        log.addLine(
                            scope.builder.allocator, 
                            expression.file_id, 
                            "This value would be {d} bits long.", .{bit_size}, 
                            expression.start, expression.end);

                        return TypeError.TypeMismatch;
                    }

                    if (opt_inferred_type) |inferred_type| {

                        if (inferred_type.id.index == scope.builder.bit8.index) {

                            if (bit_size > 8) {

                                var log = scope.builder.logger.logError(
                                    "Type Error", .{},
                                    "@bit8 can only fit values up to 0b1000_0000 or 0xFF. Use a smaller value or a larger type.");

                                log.addLine(
                                    scope.builder.allocator, 
                                    expression.file_id, 
                                    "This value is {d} bits long.", .{bit_size}, 
                                    expression.start, expression.end);

                                return scope.createTypedExpression(
                                    expression, 
                                    .Error, 
                                    .empty);
                            }

                            types.append(scope.builder.allocator, .{ .id = scope.builder.bit8, .is_ref = false }) catch @panic("Out of Memory.");

                        } else if (inferred_type.id.index == scope.builder.bit16.index) {

                            if (bit_size > 16) {

                                var log = scope.builder.logger.logError(
                                    "Type Error", .{},
                                    "@bit16 can only fit values up to 0b1000_0000_0000_0000 or 0xFFFF. Use a smaller value or a larger type.");

                                log.addLine(
                                    scope.builder.allocator, 
                                    expression.file_id, 
                                    "This value is {d} bits long.", .{bit_size}, 
                                    expression.start, expression.end);

                                return scope.createTypedExpression(
                                    expression, 
                                    .Error, 
                                    .empty);
                            }

                            types.append(scope.builder.allocator, .{ .id = scope.builder.bit16, .is_ref = false }) catch @panic("Out of Memory.");

                        } else if (inferred_type.id.index == scope.builder.bit32.index) {

                            if (bit_size > 32) {

                                var log = scope.builder.logger.logError(
                                    "Type Error", .{},
                                    "@bit32 can only fit values up to 0xFFFF_FFFF. Use a smaller value or a larger type.");

                                log.addLine(
                                    scope.builder.allocator, 
                                    expression.file_id, 
                                    "This value is {d} bits long.", .{bit_size}, 
                                    expression.start, expression.end);

                                return TypeError.TypeMismatch;
                            }

                            types.append(scope.builder.allocator, .{ .id = scope.builder.bit32, .is_ref = false }) catch @panic("Out of Memory.");

                        } else if (inferred_type.id.index == scope.builder.bitNative.index){

                            if (bit_size > scope.builder.settings.bitNativeSize) {

                                var log = scope.builder.logger.logError(
                                    "Type Error", .{},
                                    "@bit64 can only fit values up to 0xFFFF_FFFF_FFFF_FFFF. Use a smaller value or a larger type.");

                                log.addLine(
                                    scope.builder.allocator, 
                                    expression.file_id, 
                                    "This value is {d} bits long.", .{bit_size}, 
                                    expression.start, expression.end);

                                return TypeError.TypeMismatch;

                            } else if (bit_size > 32) {

                                var log = scope.builder.logger.logWarning(
                                    "Concerning Literal", .{},
                                    "@bitNative recommendeds to only use literals up to 0xFFFF_FFFF as you don't know if the target is 32 bit.");

                                log.addLine(
                                    scope.builder.allocator, 
                                    expression.file_id, 
                                    "This value is {d} bits long.", .{bit_size}, 
                                    expression.start, expression.end);
                            }

                            types.append(scope.builder.allocator, .{ .id = scope.builder.bitNative, .is_ref = false }) catch @panic("Out of Memory.");

                        } else {
                            types.append(scope.builder.allocator, .{ .id = scope.builder.bit64, .is_ref = false }) catch @panic("Out of Memory.");
                        }
                        
                    } else {
                        types.append(scope.builder.allocator, .{ .id = scope.builder.bit64, .is_ref = false }) catch @panic("Out of Memory.");
                    }
                },

                else => {
                    var log = scope.builder.logger.logError(
                        "Type Error", .{},
                        "Literal not implemented yet....");

                    log.addLine(
                        scope.builder.allocator, 
                        expression.file_id, 
                        "{s} literal", .{@tagName(literal.literal_type)}, 
                        expression.start, expression.end);

                    return TypeError.InvalidType;
                }
            }

            return scope.createTypedExpression(
                expression, 
                .{ .Literal = .{ 
                    .literal_type = literal.literal_type, 
                    .token_index = literal.token_index } }, 
                types);
        },

        else => {
            var log = scope.builder.logger.logError(
                "Invalid Expression", .{},
                null);

            log.addLine(
                scope.builder.allocator, 
                expression.file_id, 
                "Here", .{}, 
                expression.start, expression.end);

            return TypeError.InvalidType;
        }
    }
}

pub fn logCollisionErrors(builder: *Builder) void {

    var iter = builder.scopes.valueIterator();

    while (iter.next()) |scope| {
        var decl_iter = scope.*.*.declarations.valueIterator();

        while (decl_iter.next()) |decl| {
            logDeclaration(builder, decl);
        }
    }

    var init_iter = builder.initialisers.valueIterator();

    while (init_iter.next()) |decl| {
        logDeclaration(builder, decl);
    }

    var copy_iter = builder.oncopy.valueIterator();

    while (copy_iter.next()) |decl| {
        logDeclaration(builder, decl);
    }
}

fn logDeclaration(builder: *Builder, decl: *Declaration) void {

    if (decl.collisions.items.len == 0) {
        return;
    }

    var log = builder.logger.logError(
        "Declaration Collision", .{}, 
        "Try renaming the conflicting declarations.");

    if (decl.node) |first_decl| {
        log.addLine(
            builder.allocator, 
            first_decl.file_id, 
            "First declaration here.", .{}, 
            first_decl.start, first_decl.end);
    }

    for (decl.collisions.items) |collision| {
        if (collision) |collision_decl| {
            log.addLine(
                builder.allocator, 
                collision_decl.file_id, 
                "Collision here.", .{}, 
                collision_decl.start, collision_decl.end);
        }
    }
}