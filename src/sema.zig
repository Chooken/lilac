const std = @import("std");
const untyped = @import("untyped.zig");
const typed = @import("typed.zig");
const logging = @import("logger.zig");


const TypeError = error {
    MultipleDefinitions,
    InvalidType,
};

const TypeSubstitution = struct {
    identifier: []const u8,
    typeid: typed.TypeId,
};

pub const Builder = struct {

    allocator: std.mem.Allocator,

    logger: logging.Logger,

    program: typed.Program = .{},
    uprogram: *untyped.Program,

    scopes: std.AutoHashMapUnmanaged(typed.TypeId, *Scope) = .empty,
    root: typed.TypeId,
    
    // Events
    initialisers: std.AutoHashMapUnmanaged(typed.TypeId, typed.FunctionId) = .empty,
    oncopy: std.AutoHashMapUnmanaged(typed.TypeId, typed.FunctionId) = .empty,
    onoverride: std.AutoHashMapUnmanaged(typed.TypeId, typed.FunctionId) = .empty,
    ondrop: std.AutoHashMapUnmanaged(typed.TypeId, typed.FunctionId) = .empty,

    conversions: std.AutoHashMapUnmanaged(typed.Conversion, typed.FunctionId) = .empty,
    operation: std.AutoHashMapUnmanaged(typed.Operator, typed.FunctionId) = .empty,

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

    pub fn getOrAddFunctionType(self: *Builder, proto: *typed.FunctionProto) typed.TypeId {

        type_loop: for (self.program.func_types.items) |func_typeid| {
            const func_type = self.getType(func_typeid);

            if (func_type.data) |data| {
                switch (data) {

                    .Function => |function| {

                        if (function.inputs.items.len != proto.inputs.items.len or 
                            function.outputs.items.len != proto.outputs.items.len)
                        {
                            continue :type_loop;
                        }
                        
                        for (0..function.inputs.items.len) |index| {
                            if (!function.inputs.items[index].cmp(proto.inputs.items[index])) {
                                continue :type_loop;
                            }
                        }

                        for (0..function.outputs.items.len) |index| {
                            if (!function.outputs.items[index].cmp(proto.outputs.items[index])) {
                                continue :type_loop;
                            }
                        }

                        proto.deinit(self.allocator);
                        std.debug.print("Found function type.\n", .{});

                        return func_typeid;
                    },

                    else => @panic("Non function type in the FunctionTypes list")
                }
            }
        }

        std.debug.print("Added function type.\n", .{});

        const typeid = self.program.addType(self.allocator, typed.Type{
            .name = null,
            .data = .{
                .Function = proto.*
            },
        });

        self.program.func_types.append(self.allocator, typeid) catch @panic("Out of Memory.");

        return typeid;
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

    declarations: std.AutoHashMapUnmanaged(typed.Visability, Declarations) = .empty,

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

        const self_name = self.builder.getType(self.typeid).name orelse @panic("Getting a Name of a type without a name. possibly a function type.");

        size += self_name.len;

        var name: []u8 = self.builder.allocator.alloc(u8, size) catch @panic("Out of Memory.");
        var offset: usize = 0;

        while (parents.pop()) |parent| {
            if (self.builder.getType(parent.typeid).name) |parent_name| {
                @memcpy(name[offset..offset + parent_name.len], parent_name);
                name[offset + parent_name.len] = '.';
                offset += parent_name.len + 1;
            }
        }

        @memcpy(name[offset..offset + self_name.len], self_name);
        return name;
    }

    pub fn shadows(self: *Scope, identifier: []const u8) bool {
        var current_scope: *Scope = self;
        var public_only = false;

        while (true) {

            if (public_only) {
                if (current_scope.declarations.get(.public)) |decl| {
                    if (decl.contains(identifier)) {
                        return true;
                    }
                }
            } else {
                if (current_scope.contains(identifier)) {
                    return true;
                }

                for (current_scope.usings.items) |using| {
                    if (self.builder.getScope(using)) |using_scope| {
                        if (using_scope.declarations.get(.public)) |decl| {
                            if (decl.contains(identifier)) {
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
        var decl_iter = self.declarations.valueIterator();

        while (decl_iter.next()) |decl| {
            if (decl.contains(identifier)) {
                return true;
            }
        }

        return false;
    }

    pub fn addTypeDecl(self: *Scope, identifier: []const u8, visability: typed.Visability, node: ?untyped.Node(untyped.Expression)) TypeError!typed.TypeId {

        std.debug.print("Type {s} {s}\n", .{@tagName(visability), identifier});

        const decls = self.declarations.getOrPut(self.builder.allocator, visability) catch @panic("Out of Memory");

        if (!decls.found_existing) {
            decls.value_ptr.* = .{
                .parent_scope = self,
            };
        }

        return decls.value_ptr.addTypeDecl(identifier, node);
    }

    pub fn addTypeSubstitution(self: *Scope, identifier: []const u8, typeid: typed.TypeId, node: ?untyped.Node(untyped.Expression)) TypeError!void {

        std.debug.print("Type Sub {s} -> {s}\n", .{identifier, self.builder.getScope(typeid).?.allocFullName()});

        const decls = self.declarations.getOrPut(self.builder.allocator, .public) catch @panic("Out of Memory");

        if (!decls.found_existing) {
            decls.value_ptr.* = .{
                .parent_scope = self,
            };
        }

        try decls.value_ptr.addTypeSubstitution( identifier, typeid, node);
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

            .Nothing => {
                if (self.builder.getScope(self.builder.root)) |root_scope| {

                    if (root_scope.getType("nothing", proto.returns) catch return null) |typeid| {
                        return typeid;
                    } else {
                        var log = self.builder.logger.logError(
                            "Invalid Type", .{}, 
                            "This is a compiler error. Contact the developer.");
                        log.addLine(
                            self.builder.allocator, 
                            ast.file, 
                            "Failed to get nothing type.", .{}, 
                            proto.returns.start, 
                            proto.returns.end);
                        return null;
                    }
                }

                return null;
            },

            .Self => {
                proto_type.inputs.append(self.builder.allocator, .{
                    .id = self.typeid,
                    .is_ref = true,
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
                                    "func (other: type, self) -> func (self, other: type)");
                                log.addLine(
                                    self.builder.allocator, 
                                    ast.file, 
                                    "You can only have the self parameter in the first position.", .{}, 
                                    expr.start, 
                                    expr.end);
                                return null;
                            }
                            proto_type.inputs.append(self.builder.allocator, .{
                                .id = self.typeid,
                                .is_ref = true,
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

        return self.builder.getOrAddFunctionType(&proto_type);
    }

    pub fn addFunction(self: *Scope, identifier: []const u8, typeid: typed.TypeId, is_inline: bool, requires_self: bool, visability: typed.Visability, node: ?untyped.Node(untyped.Expression)) TypeError!void {

        std.debug.print("Function {s} {s}\n", .{@tagName(visability), identifier});

        const decls = self.declarations.getOrPut(self.builder.allocator, visability) catch @panic("Out of Memory");

        if (!decls.found_existing) {
            decls.value_ptr.* = .{
                .parent_scope = self,
            };
        }

        const functionid = self.builder.addFunction(typeid, is_inline, requires_self);

        try decls.value_ptr.addFunction(identifier, functionid, node);
    }

    pub fn getPublicType(self: *Scope, identifier: []const u8, node: untyped.Node(untyped.Expression)) TypeError!?typed.TypeId {
        if (self.declarations.get(.public)) |private_decls| {
            if (try private_decls.getType(identifier, node)) |typeid| {
                return typeid;
            }
        }

        return null;
    }

    pub fn getType(self: *Scope, identifier: []const u8, node: ?untyped.Node(untyped.Expression)) TypeError!?typed.TypeId {
        
        if (self.declarations.get(.private)) |private_decls| {
            if (try private_decls.getType(identifier, node)) |typeid| {
                return typeid;
            }
        }

        if (self.declarations.get(.public)) |private_decls| {
            if (try private_decls.getType(identifier, node)) |typeid| {
                return typeid;
            }
        }

        if (self.alias.get(identifier)) |alias| {
            return alias;
        }

        return null;
    }

    pub fn addGenericType(self: *Scope, identifier: []const u8, generic: *untyped.Generic, base: untyped.Node(untyped.Expression), visability: typed.Visability, ast: *untyped.Ast, node: untyped.Node(untyped.Expression)) TypeError!void {

        std.debug.print("Generic Type {s} {s}", .{@tagName(visability), ast.source[generic.callee.start..generic.arguements.end]});
        std.debug.print("]\n", .{});
        
        const decls = self.declarations.getOrPut(self.builder.allocator, visability) catch @panic("Out of Memory");

        if (!decls.found_existing) {
            decls.value_ptr.* = .{
                .parent_scope = self,
            };
        }

        try decls.value_ptr.addGeneric(identifier, generic, base, ast, node);
    }

    pub fn getGenericType(self: *Scope, identifier: []const u8, sub_types: std.ArrayList(typed.TypeId), ast: *untyped.Ast, node: untyped.Node(untyped.Expression)) TypeError!?typed.TypeId {

        if (self.declarations.get(.private)) |*decl| {
            if (try decl.getGeneric(identifier, sub_types, ast, node)) |typeid| {
                return typeid;
            }
        }

        if (self.declarations.get(.public)) |*decl| {
            if (try decl.getGeneric(identifier, sub_types, ast, node)) |typeid| {
                return typeid;
            }
        }

        return null;
    }

    pub fn getPublicGenericType(self: *Scope, identifier: []const u8, sub_types: std.ArrayList(typed.TypeId), ast: *untyped.Ast, node: untyped.Node(untyped.Expression)) TypeError!?typed.TypeId {
        
        if (self.declarations.get(.public)) |*decl| {
            if (try decl.getGeneric(identifier, sub_types, ast, node)) |typeid| {
                return typeid;
            }
        }

        return null;
    }

    pub fn addField(self: *Scope, identifier: []const u8, node: untyped.Node(untyped.Expression), visability: typed.Visability, type_ref: typed.TypeRef) TypeError!void {
        const decls = self.declarations.getOrPut(self.builder.allocator, visability) catch @panic("Out of Memory");

        if (!decls.found_existing) {
            decls.value_ptr.* = .{
                .parent_scope = self,
            };
        }

        try decls.value_ptr.addField(identifier, node, type_ref);
    }
};

pub const Declaration = struct {
    node: ?untyped.Node(untyped.Expression),
    decl_type: DeclarationType,
    collisions: std.ArrayList(?untyped.Node(untyped.Expression)) = .empty,
};

pub const DeclarationType = union(enum) {
    Field: typed.TypeRef,
    Type: typed.TypeId,
    Function: typed.FunctionId,
    Generic: Generic,
};

pub const Declarations = struct {

    parent_scope: *Scope,
    decls: std.StringHashMapUnmanaged(Declaration) = .empty,

    pub fn contains(self: *Declarations, identifier: []const u8) bool {
        return self.decls.contains(identifier);
    }

    pub fn addDecl(self: *Declarations, identifier: []const u8, node: ?untyped.Node(untyped.Expression), decl_type: DeclarationType) TypeError!void {
        const decl = self.decls.getOrPut(self.parent_scope.builder.allocator, identifier) catch @panic("Out of Memory.");

        if (decl.found_existing) {
            decl.value_ptr.collisions.append(self.parent_scope.builder.allocator, node) catch @panic("Out of Memory.");
            return TypeError.MultipleDefinitions;
        }

        decl.value_ptr.* = .{
            .node = node,
            .decl_type = decl_type,
        };
    }

    pub fn addTypeDecl(self: *Declarations, identifier: []const u8, node: ?untyped.Node(untyped.Expression)) TypeError!typed.TypeId {
        const typeid= self.parent_scope.builder.getNewType(identifier, self.parent_scope);
        try self.addDecl(
            identifier, 
            node, 
            .{ .Type = typeid, });
        return typeid;
    }

    pub fn addTypeSubstitution(self: *Declarations, identifier: []const u8, typeid: typed.TypeId, node: ?untyped.Node(untyped.Expression)) TypeError!void {
        try self.addDecl(
            identifier, 
            node, 
            .{ .Type = typeid, });
    }

    pub fn getType(self: *const Declarations, identifier: []const u8, identifer_node: ?untyped.Node(untyped.Expression)) TypeError!?typed.TypeId {

        if (self.decls.get(identifier)) |decl| {

            switch (decl.decl_type) {
                
                .Type => |typeid| {
                    return typeid;
                },

                else => {
                    var log = self.parent_scope.builder.logger.logError(
                        "Type Error", .{}, 
                        null);
                    if (identifer_node) |ident_node| {
                        log.addLine(
                            self.parent_scope.builder.allocator, 
                            ident_node.file_id, 
                            "Declaration is not an type.", .{}, 
                            ident_node.start, ident_node.end);
                    }
                    if (decl.node) |node| {
                        log.addLine(
                            self.parent_scope.builder.allocator, 
                            node.file_id, 
                            "This is the types declaration.", .{}, 
                            node.start, node.end);
                    }
                    return TypeError.InvalidType;
                },
            }
        }

        return null;
    }

    pub fn addFunction(self: *Declarations, identifier: []const u8, functionid: typed.FunctionId, node: ?untyped.Node(untyped.Expression)) TypeError!void {
        try self.addDecl(
            identifier, 
            node, 
            .{ .Function = functionid, });
    }

    pub fn addGeneric(self: *Declarations, identifier: []const u8, generic: *untyped.Generic, base: untyped.Node(untyped.Expression), ast: *untyped.Ast, node: ?untyped.Node(untyped.Expression)) TypeError!void {

        var sub_list = std.ArrayList([]const u8).empty;

        switch (generic.arguements.data.*) {
            
            .Identifier => |ident| {
                const token = ast.tokens[ident.token_index];
                const name = ast.source[token.start..token.end];
                sub_list.append(self.parent_scope.builder.allocator, name) catch @panic("Out of Memory.");
            },

            .List => |list| {
                for (list.expressions.items) |expr| {
                    switch (expr.data.*) {
                        
                        .Identifier => |ident| {
                            const token = ast.tokens[ident.token_index];
                            const name = ast.source[token.start..token.end];
                            sub_list.append(self.parent_scope.builder.allocator, name) catch @panic("Out of Memory.");
                        },

                        else => {
                            var log = self.parent_scope.builder.logger.logError(
                                "Invalid Generic", .{}, 
                                "You can only use names in a generic list. Generic[T, T2]");
                            log.addLine(
                                self.parent_scope.builder.allocator, 
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
                var log = self.parent_scope.builder.logger.logError(
                    "Invalid Generic", .{}, 
                    "You can only use names in a generic list. Generic[T, T2]");
                log.addLine(
                    self.parent_scope.builder.allocator, 
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
            }, });
    }

    pub fn getGeneric(self: *const Declarations, identifier: []const u8, sub_types: std.ArrayList(typed.TypeId), ast: *untyped.Ast, identifier_node: untyped.Node(untyped.Expression)) TypeError!?typed.TypeId {

        var gen: *Generic = undefined;
        var node: ?untyped.Node(untyped.Expression) = undefined;

        if (self.decls.getPtr(identifier)) |decl| {

            node = decl.node;

            switch (decl.decl_type) {
                
                .Generic => |*generic| {
                    gen = generic;
                },

                else => {
                    var log = self.parent_scope.builder.logger.logError(
                        "Type Error", .{}, 
                        "If it is meant to be generic add [] to the type declaration. Type[T]");

                    log.addLine(
                        self.parent_scope.builder.allocator, 
                        identifier_node.file_id, 
                        "This type is not a generic.", .{}, 
                        identifier_node.start, identifier_node.end);

                    if (node) |decl_node| {
                        log.addLine(
                            self.parent_scope.builder.allocator, 
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
                if (gen_type.sub_types.items[index].index == sub_types.items[index].index) {
                    continue :cache_loop;
                }
            }

            return gen_type.typeid;
        }

        std.debug.print("Type {s}[", .{identifier});
        std.debug.print("{s}", .{self.parent_scope.builder.getScope(sub_types.items[0]).?.allocFullName()});

        for (sub_types.items[1..]) |typeid| {
            std.debug.print(", {s}", .{self.parent_scope.builder.getScope(typeid).?.allocFullName()});
        }

        std.debug.print("]\n", .{});

        const typeid = self.parent_scope.builder.getNewType(identifier, self.parent_scope);

        var gen_type = self.parent_scope.builder.getType(typeid);
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
                var log = self.parent_scope.builder.logger.logError(
                    "Invalid Generic", .{}, 
                    "You can only put a generic on object, enum, and interfaces.");
                log.addLine(
                    self.parent_scope.builder.allocator, 
                    ast.file, 
                    "Invalid body for a Generic.", .{}, 
                    gen.base.start, 
                    gen.base.end);
                return null;
            },
        }

        gen.cache.append(self.parent_scope.builder.allocator, .{
            .sub_types = sub_types,
            .typeid = typeid,
        }) catch @panic("Out of Memory.");

        if (self.parent_scope.builder.getScope(typeid)) |type_scope| {
            for (0..sub_types.items.len) |index| {
                type_scope.addTypeSubstitution(gen.sub_identifiers.items[index], sub_types.items[index], node) catch continue;
            }
            collectTypeIdsFromBlock(type_scope, ast, body.data, .public);
            collectTypeDataFromBlock(type_scope, ast, body.data, .public);
        }

        return typeid;
    }

    pub fn addField(self: *Declarations, identifier: []const u8, node: untyped.Node(untyped.Expression), type_ref: typed.TypeRef) TypeError!void {
        try self.addDecl(identifier, node, .{ .Field = type_ref });
    }
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

pub fn runSema(allocator: std.mem.Allocator, uprogram: *untyped.Program) typed.Program {

    var builder = Builder {
        .allocator = allocator,
        .logger = logging.Logger {
            .allocator = allocator,
        },
        .uprogram = uprogram,
        .root = undefined,
    };
    defer builder.deinit();

    collectTypeIds(&builder);
    collectTypeData(&builder);
    logCollisionErrors(&builder);

    logging.printLogs(builder.logger, allocator);

    return builder.program;
}

pub fn collectTypeIds(builder: *Builder) void {

    builder.root = builder.getNewType("Root", null);
    const module_type = builder.getType(builder.root);
    module_type.data = .{ .Module = .{} };
    
    if (builder.getScope(builder.root)) |scope| {

        _ = scope.addTypeDecl("@bit8", .public, null) catch @panic("@bit8 was already added to root scope.");
        _ = scope.addTypeDecl("@bit16", .public, null) catch @panic("@bit16 was already added to root scope.");
        _ = scope.addTypeDecl("@bit32", .public, null) catch @panic("@bit32 was already added to root scope.");
        _ = scope.addTypeDecl("@bit64", .public, null) catch @panic("@bit64 was already added to root scope.");
        _ = scope.addTypeDecl("@bitNative", .public, null) catch @panic("@bitNative was already added to root scope.");
        _ = scope.addTypeDecl("@numberLiteral", .public, null) catch @panic("@numberLiteral was already added to root scope.");
        _ = scope.addTypeDecl("unknown", .public, null) catch @panic("unknown was already added to root scope.");
        _ = scope.addTypeDecl("nothing", .public, null) catch @panic("nothing was already added to root scope.");

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

                // Built-ins don't need to be added.
                // Generics get genned by usage not declaration.
                .Builtin, .Generic => return,

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

        const result = isTypeInScope(current_scope, ast, expression, public_only) catch return null;
        
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
        "Type doesn't exist in {s} module scope.", .{scope.allocFullName()}, 
        expression.start, 
        expression.end);

    return null;
}

pub fn isTypeInScope(scope: *Scope, ast: *untyped.Ast, expression: untyped.Node(untyped.Expression), public_only: bool) TypeError!?typed.TypeId {
    switch (expression.data.*) {

        .Member => |member| {

            const result = try isTypeInScope(scope, ast, member.parent, public_only);

            if (result) |parent_typeid| {

                if (scope.builder.getScope(parent_typeid)) |parent_scope| {
                    return isTypeInScope(parent_scope, ast, member.child, true);
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
                    const result = try isTypeInScope(scope, ast, generic.arguements, public_only);

                    if (result) |typeid| {
                        sub_list.append(scope.builder.allocator, typeid) catch @panic("Out of Memory.");
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
                },

                .List => |list| {
                    for (list.expressions.items) |expr| {
                        const result = try isTypeInScope(scope, ast, expr, public_only);

                        if (result) |typeid| {
                            sub_list.append(scope.builder.allocator, typeid) catch @panic("Out of Memory.");
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

pub fn logCollisionErrors(builder: *Builder) void {

    var iter = builder.scopes.valueIterator();

    while (iter.next()) |scope| {
        var decl_iter = scope.*.*.declarations.valueIterator();

        while (decl_iter.next()) |decl| {
            var decl_iter_2 = decl.decls.valueIterator();

            while (decl_iter_2.next()) |declaration| {
                
                if (declaration.collisions.items.len == 0) {
                    continue;
                }

                var log = builder.logger.logError(
                    "Declaration Collision", .{}, 
                    "Try renaming the conflicting declarations.");

                if (declaration.node) |first_decl| {
                    log.addLine(
                        builder.allocator, 
                        first_decl.file_id, 
                        "First declaration here.", .{}, 
                        first_decl.start, first_decl.end);
                }

                for (declaration.collisions.items) |collision| {
                    if (collision) |collision_decl| {
                        log.addLine(
                            builder.allocator, 
                            collision_decl.file_id, 
                            "Collision here.", .{}, 
                            collision_decl.start, collision_decl.end);
                    }
                }
            }
        }
    }
}