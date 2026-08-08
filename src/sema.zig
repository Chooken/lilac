const std = @import("std");
const tokens = @import("tokens.zig");
const untyped = @import("untyped.zig");
const typed = @import("typed.zig");
const logging = @import("logger.zig");
pub const builder = @import("sema/builder.zig");
const analysis = @import("sema/analysis.zig");
const generation = @import("sema/generation.zig");
const files = @import("files.zig");

pub const Builder = builder.Builder;
pub const Scope = builder.Scope;
pub const Visability = builder.Visability;
pub const SemaError = error {
    MultipleDefinitions,
    TypeMismatch,
    MissingType,
    InvalidType,
    InvalidExpression,
    MissingDeclaration,
};

pub fn getFunctionTypeId(scope: *Scope, proto: *untyped.FuncPrototype, allow_raw_type_in_args: bool, allow_raw_type_in_return: bool) ?typed.TypeId {

    var proto_type = typed.FunctionProto { };

    if (proto.arguments) |arguements| {
        switch (arguements.data.*) {

            .Self => {
                proto_type.inputs.append(scope.builder.allocator, .{
                    .id = scope.typeid,
                    .is_ref = true,
                }) catch @panic("Out of Memory.");
            },

            .Declaration => |decl| {
                if (ExprToTypeRef(scope, decl.decl_type)) |typeref| {
                    proto_type.inputs.append(scope.builder.allocator, typeref) catch @panic("Out of Memory.");
                } else {
                    return null;
                }
            },

            .List => |list| {
                for (list.expressions.items) |expr| {
                    switch (expr.data.*) {

                        .Self => {
                            proto_type.inputs.append(scope.builder.allocator, .{
                                .id = scope.typeid,
                                .is_ref = true,
                            }) catch @panic("Out of Memory.");
                        },

                        .Declaration => |decl| {
                            if (ExprToTypeRef(scope, decl.decl_type)) |typeref| {
                                proto_type.inputs.append(scope.builder.allocator, typeref) catch @panic("Out of Memory.");
                            } else {
                                return null;
                            }
                        },
                            
                        else => {

                            if (!allow_raw_type_in_args) {
                                var log = scope.builder.logger.logError(
                                    "Invalid Type", .{}, 
                                    null, .{});
                                log.addLine(
                                    "Invalid function parameter type.", .{}, 
                                    proto.returns.span);
                                return null;
                            }

                            if (ExprToTypeRef(scope, proto.returns)) |typeref|
                                proto_type.inputs.append(
                                    scope.builder.allocator, 
                                    typeref) catch @panic("Out of Memory.") else return null;
                        },
                    }
                }
            },

            else => {

                if (!allow_raw_type_in_args) {
                    var log = scope.builder.logger.logError(
                        "Invalid Type", .{}, 
                        null, .{});
                    log.addLine(
                        "Invalid function parameter type.", .{}, 
                        proto.returns.span);
                    return null;
                }

                if (ExprToTypeRef(scope, proto.returns)) |typeref|
                    proto_type.inputs.append(
                        scope.builder.allocator, 
                        typeref) catch @panic("Out of Memory.") else return null;
            },
        }
    }

    switch (proto.returns.data.*) {

        .Nothing => {},

        .Self => {
            proto_type.outputs.append(scope.builder.allocator, .{
                .id = scope.typeid,
                .is_ref = false,
            }) catch @panic("Out of Memory.");
        },

        .Declaration => |decl| {
            if (ExprToTypeRef(scope, decl.decl_type)) |typeref| {
                proto_type.outputs.append(scope.builder.allocator, typeref) catch @panic("Out of Memory.");
            }
        },

        .List => |list| {
            
            for (list.expressions.items, 0..) |expr, index| {
                switch (expr.data.*) {

                    .Nothing => {
                        var log = scope.builder.logger.logError(
                            "Invalid Type", .{}, 
                            "Remove the nothing. nothing, var: type -> var: type", .{});
                        log.addLine(
                            "You can't have nothing in a list with something.", .{}, 
                            expr.span);
                        return null;
                    },

                    .Self => {
                        if (index > 0) {
                            var log = scope.builder.logger.logError(
                                "Invalid Type", .{}, 
                                "func () other: type, self -> func () self, other: type", .{});
                            log.addLine(
                                "You can only have the self result in the first position.", .{}, 
                                expr.span);
                            return null;
                        }
                        proto_type.outputs.append(scope.builder.allocator, .{
                            .id = scope.typeid,
                            .is_ref = false,
                        }) catch @panic("Out of Memory.");
                    },

                    .Declaration => |decl| {
                        if (ExprToTypeRef(scope, decl.decl_type)) |typeref| {
                            proto_type.outputs.append(scope.builder.allocator, typeref) catch @panic("Out of Memory.");
                        }
                    },
                        
                    else => {

                        if (!allow_raw_type_in_return) {
                            var log = scope.builder.logger.logError(
                                "Invalid Type", .{}, 
                                null, .{});
                            log.addLine(
                                "Invalid function return type.", .{}, 
                                proto.returns.span);
                            return null;
                        }

                        if (ExprToTypeRef(scope, proto.returns)) |typeref|
                            proto_type.outputs.append(
                                scope.builder.allocator, 
                                typeref) catch @panic("Out of Memory.") else return null;
                    },
                }
            }
        },

        else => {

            if (!allow_raw_type_in_return) {
                var log = scope.builder.logger.logError(
                    "Invalid Type", .{}, 
                    null, .{});
                log.addLine(
                    "Invalid function return type.", .{}, 
                    proto.returns.span);
                return null;
            }

            if (ExprToTypeRef(scope, proto.returns)) |typeref|
                proto_type.outputs.append(
                    scope.builder.allocator, 
                    typeref) catch @panic("Out of Memory.") else return null;
        },
    }

    return scope.builder.getOrAddFunctionType(proto_type);
}

pub fn runSema(allocator: std.mem.Allocator, uprogram: *untyped.Program, settings: builder.Settings, logger: *logging.Logger) typed.Program {

    var typed_builder = builder.Builder {
        .allocator = allocator,
        .logger = logger,
        .settings = settings,
        .uprogram = uprogram,
        .root = undefined,
    };
    defer typed_builder.deinit();

    std.debug.print("Collection Pass\n", .{});
    analysis.runCollectionPasses(&typed_builder);

    std.debug.print("Generation Pass\n", .{});
    generation.generate(&typed_builder);
    
    analysis.runCollisionCollection(&typed_builder);

    return typed_builder.program;
}

pub fn addFieldToTypeData(scope: *Scope, identifier: []const u8, node: untyped.Node(untyped.Expression), type_expr: untyped.Node(untyped.Expression), visability: Visability) void {
    if (ExprToTypeRef(scope, type_expr)) |type_ref| {
        scope.addField(identifier, node.span, visability, type_ref) catch return;
    }
}

pub fn ExprToTypeRef(scope: *Scope, expression: untyped.Node(untyped.Expression)) ?typed.TypeRef {

    var current_scope: *Scope = scope;
    var expr = expression;
    var public_only = false;
    var is_ref = false;

    switch (expression.data.*) {
        .Unary => |unary| {
            if (unary.op_token.token_type == .Reference) {
                expr = unary.right;
                is_ref = true;
            } else {
                var log = scope.builder.logger.logError(
                    "Invalid Type", .{}, 
                    "Name: Type or Name: ref Type", .{});
                log.addLine(
                    "Invalid unary operator on type.", .{}, 
                    unary.op_token.span);
            }
        },
        
        else => {},
    }

    while (true) {

        const result = isTypeInScope(scope, current_scope, expr, public_only) catch return null;
        
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
        "Did you forget an import or spell it wrong?", .{});
    log.addLine(
        "Type doesn't exist in scope.", .{}, 
        expression.span);

    return null;
}

pub fn isTypeInScope(access_scope: *Scope, scope: *Scope, expression: untyped.Node(untyped.Expression), public_only: bool) SemaError!?typed.TypeId {
    switch (expression.data.*) {

        .Member => |member| {

            const result = try isTypeInScope(access_scope, scope, member.parent, public_only);

            if (result) |parent_typeid| {

                if (scope.builder.getScope(parent_typeid)) |parent_scope| {
                    return isTypeInScope(access_scope, parent_scope, member.child, true);
                }

                return null;
            }

            return null;
        },

        .Identifier => |ident| {
            const name = ident.token.span.getString();

            return scope.getType(
                name, 
                expression.span, 
                if (public_only) .public else .private) catch return SemaError.InvalidType;
        },

        .Unknown => {
            if (scope.builder.getScope(scope.builder.root)) |root_scope| {

                if (root_scope.getType("unknown", expression.span, .private) catch return SemaError.InvalidType) |typeid| {
                    return typeid;
                } else unreachable;
            }

            return null;
        },

        .Builtin => |builtin| {
            if (scope.builder.getScope(scope.builder.root)) |root_scope| {
                const name = builtin.token.span.getString();
                
                if (root_scope.getType(name, expression.span, .private) catch return SemaError.InvalidType) |typeid| {
                    return typeid;
                } else {
                    var log = scope.builder.logger.logError(
                        "Invalid Builtin Type", .{}, 
                        "built-in types include: @bit8, @bit16, @bit32, @bit64, @bitNative, @numberLiteral", .{});
                    log.addLine(
                        "Built-in type \x22{s}\x22 doesn't exist.", .{name}, 
                        expression.span);
                    return SemaError.MissingType;
                }
            }
            
            return null;
        },

        .FuncPrototype => |*proto| {
            return getFunctionTypeId(
                scope,
                proto, 
                true, 
                true);
        },

        .Generic => |*generic| {

            var sub_list = std.ArrayList(typed.TypeId).empty;

            switch (generic.arguements.data.*) {
                
                .Identifier, .Builtin => {
                    const result = ExprToTypeRef(access_scope, generic.arguements);

                    if (result) |typeref| {
                        sub_list.append(scope.builder.allocator, typeref.id) catch @panic("Out of Memory.");
                    } else {
                        var log = scope.builder.logger.logError(
                            "Invalid Type", .{}, 
                            "Did you forget an import or spell it wrong?", .{});
                        log.addLine(
                            "Type doesn't exist in {s} scope.", .{scope.allocFullName()}, 
                            generic.arguements.span);
                        return SemaError.MissingType;
                    }
                },

                .List => |list| {
                    for (list.expressions.items) |expr| {
                        const result = ExprToTypeRef(access_scope, expr);

                        if (result) |typeref| {
                            if (typeref.is_ref) {
                                var log = scope.builder.logger.logError(
                                    "Invalid Type", .{}, 
                                    "Generics can't have ref types as parameters.", .{});
                                log.addLine(
                                    "ref type here.", .{}, 
                                    expression.span);
                                return SemaError.InvalidType;
                            }
                            sub_list.append(scope.builder.allocator, typeref.id) catch @panic("Out of Memory.");
                        } else {
                            var log = scope.builder.logger.logError(
                                "Invalid Type", .{}, 
                                "Did you forget an import or spell it wrong?", .{});
                            log.addLine(
                                "Type doesn't exist in {s} module scope.", .{scope.allocFullName()}, 
                                expression.span);
                            return SemaError.MissingType;
                        }
                    }
                },

                else => {
                    var log = scope.builder.logger.logError(
                        "Invalid Type", .{}, 
                        "Generics can't have ref or nothing types", .{});
                    log.addLine(
                        "Invalid type for generic.", .{}, 
                        expression.span);
                    return SemaError.InvalidType;
                },
            }

            switch (generic.callee.data.*) {

                .Identifier => |ident| {
                    
                    const name = ident.token.span.getString();

                    if (public_only) {
                        return try getOrGenerateGeneric(scope, name, sub_list, expression.span, .public);
                    }
                    return try getOrGenerateGeneric(scope, name, sub_list, expression.span, .private);
                },

                else => @panic("Generic callee can't be anything but identifier."),
            }
        },

        else => {
            var log = scope.builder.logger.logError(
                "Invalid Type", .{}, 
                "Types can only be names, members, or functions.", .{});
            log.addLine(
                "Invalid Type Name.", .{}, 
                expression.span);
            return SemaError.InvalidType;
        }
    }
}

pub fn getOrGenerateGeneric(self: *Scope, identifier: []const u8, sub_types: std.ArrayList(typed.TypeId), opt_span: ?files.Span, visability: Visability) SemaError!?typed.TypeId {

    const gen = (self.getGenericType(identifier,  opt_span, visability) catch return SemaError.TypeMismatch) orelse return null;

    if (sub_types.items.len != gen.sub_identifiers.items.len) {
        return null;
    }

    if (gen.getCached(sub_types)) |typeid| {
        return typeid;
    }

    std.debug.print("Type {s}[", .{identifier});
    std.debug.print("{s}", .{self.builder.getScope(sub_types.items[0]).?.allocFullName()});

    for (sub_types.items[1..]) |typeid| {
        std.debug.print(", {s}", .{self.builder.getScope(typeid).?.allocFullName()});
    }

    std.debug.print("] to {s}\n", .{self.allocFullName()});

    var type_data: typed.TypeData = undefined;
    var body: untyped.Node(untyped.Block) = undefined;
    var scope_type: builder.ScopeType = undefined;

    switch (gen.base.data.*) {

        .Object => |obj| {
            scope_type = .Object;
            type_data = .{ .Object = .{} };
            body = obj;
        },  

        .Enum => |_enum| {
            scope_type = .Enum;
            type_data = .{ .Object = .{} };
            body = _enum;
        },

        .Union => |_union| {
            scope_type = .Union;
            type_data = .{ .Object = .{} };
            body = _union;
        },

        .Interface => |interface| {
            scope_type = .Interface;
            type_data = .{ .Object = .{} };
            body = interface;
        },

        else => {
            var log = self.builder.logger.logError(
                "Invalid Generic", .{}, 
                "You can only put a generic on object, enum, and interfaces.", .{});
            log.addLine(
                "Invalid body for a Generic.", .{}, 
                gen.base.span);
            return null;
        },
    }

    const typeid = self.builder.getNewType(identifier, scope_type, self);
    var gen_type = self.builder.getType(typeid);

    gen_type.data = type_data;

    gen.cache.append(self.builder.allocator, .{
        .sub_types = sub_types,
        .typeid = typeid,
    }) catch @panic("Out of Memory.");

    if (self.builder.getScope(typeid)) |type_scope| {
        for (0..sub_types.items.len) |index| {
            type_scope.addTypeSubstitution(gen.sub_identifiers.items[index], sub_types.items[index], null) catch continue;
        }
        analysis.runCollectionOnGeneric(type_scope, body.data);
    }

    return typeid;
}

