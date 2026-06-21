const std = @import("std");
const sema = @import("../../sema.zig");
const untyped = @import("../../untyped.zig");
const typed = @import("../../typed.zig");

pub fn collectTypeIds(builder: *sema.Builder) void {

    builder.root = builder.getNewType("Root", .Module, null);
    const module_type = builder.getType(builder.root);
    module_type.data = .{ .Module = .{} };
    
    if (builder.getScope(builder.root)) |scope| {

        builder.bit8 = scope.addTypeDecl("@bit8", .Object, .public, null) catch @panic("@bit8 was already added to root scope.");
        builder.bit16 = scope.addTypeDecl("@bit16", .Object, .public, null) catch @panic("@bit16 was already added to root scope.");
        builder.bit32 = scope.addTypeDecl("@bit32", .Object, .public, null) catch @panic("@bit32 was already added to root scope.");
        builder.bit64 = scope.addTypeDecl("@bit64", .Object, .public, null) catch @panic("@bit64 was already added to root scope.");
        builder.bitNative = scope.addTypeDecl("@bitNative", .Object, .public, null) catch @panic("@bitNative was already added to root scope.");
        builder.numberLiteral = scope.addTypeDecl("@numberLiteral", .Object, .public, null) catch @panic("@numberLiteral was already added to root scope.");
        builder.unknown = scope.addTypeDecl("unknown", .Object, .public, null) catch @panic("unknown was already added to root scope.");
        builder.nothing = scope.addTypeDecl("nothing", .Object, .public, null) catch @panic("nothing was already added to root scope.");

        collectTypeIdsFromModule(scope, &builder.uprogram.root_module);
    }
}

pub fn collectTypeIdsFromModule(scope: *sema.Scope, module: *untyped.Module) void {
    for (module.asts.items) |*ast| {
        collectTypeIdsFromBlock(scope, &ast.root_block, .public);
    }

    var sub_mod_iter = module.submodules.iterator();

    while (sub_mod_iter.next()) |sub_mod_entry| {
        const typeid = scope.addTypeDecl(sub_mod_entry.key_ptr.*, .Module, .public, null) catch return;
        const module_type = scope.builder.getType(typeid);
        module_type.data = .{ .Module = .{} };

        if (scope.builder.getScope(typeid)) |sub_scope| {
            collectTypeIdsFromModule(sub_scope, sub_mod_entry.value_ptr);
        }
    }
}

pub fn collectTypeIdsFromBlock(scope: *sema.Scope, block: *untyped.Block, visability: sema.Visability) void {
    for (block.body.items) |statement| {
        collectTypeIdsFromStatements(scope, statement, visability);
    }
}

pub fn collectTypeIdsFromStatements(scope: *sema.Scope, statement: untyped.Node(untyped.Statement), visability: sema.Visability) void {
    
    switch (statement.data.*) {

        .Block => |block| {
            collectTypeIdsFromBlock(scope, block.data, visability);
        },

        .Expression => |expr| {
            collectTypeIdsFromExpressions(scope, expr, visability);
        },

        .Private => |private| {
            collectTypeIdsFromBlock(scope, private.data, .private);
        },

        else => return,
    }
}

pub fn collectTypeIdsFromExpressions(scope: *sema.Scope, expression: untyped.Node(untyped.Expression), visability: sema.Visability) void {
    
    switch (expression.data.*) {

        .Assignment => |assignment| {

            if (assignment.inlined) {
                return;
            }

            var log = scope.builder.logger.logError(
                "Assignment Error", .{}, 
                "You can't assign to a field.", .{});
            log.addLine(
                "Assignment here.", .{}, 
                expression.span);
            return;
        },

        .Declaration => |decl| {

            switch (decl.name.data.*) {

                .Identifier => |ident| {

                    const token = ident.token;

                    switch (decl.decl_type.data.*) {

                        .Object => |obj| {
                            const typeid = scope.addTypeDecl(token.span.getString(), .Object, visability, decl.name.span) catch return;
                            const obj_type = scope.builder.getType(typeid);
                            obj_type.data = .{ .Object = .{} };

                            for (obj.data.body.items) |child_statement| {
                                collectTypeIdsFromStatements(scope, child_statement, .public);
                            }
                        },  

                        .Enum => |_enum| {
                            const typeid = scope.addTypeDecl(token.span.getString(), .Enum, visability, decl.name.span) catch return;
                            const obj_type = scope.builder.getType(typeid);
                            obj_type.data = .{ .Object = .{} };

                            for (_enum.data.body.items) |child_statement| {
                                collectTypeIdsFromStatements(scope, child_statement, .public);
                            }
                        },

                        .Union => |_union| {
                            const typeid = scope.addTypeDecl(token.span.getString(), .Union, visability, decl.name.span) catch return;
                            const obj_type = scope.builder.getType(typeid);
                            obj_type.data = .{ .Object = .{} };

                            for (_union.data.body.items) |child_statement| {
                                collectTypeIdsFromStatements(scope, child_statement, .public);
                            }
                        },

                        .Interface => |interfaces| {
                            const typeid = scope.addTypeDecl(token.span.getString(), .Interface, visability, decl.name.span) catch return;
                            const obj_type = scope.builder.getType(typeid);
                            obj_type.data = .{ .Object = .{} };

                            for (interfaces.data.body.items) |child_statement| {
                                collectTypeIdsFromStatements(scope, child_statement, .public);
                            }
                        },

                        else => return,
                    }
                },

                .Generic => |*generic| {

                    const base = decl.decl_type;
                    
                    switch (generic.callee.data.*) {

                        .Identifier => |ident| {
                            
                            const token = ident.token;

                            var sub_list = std.ArrayList([]const u8).empty;

                            switch (generic.arguements.data.*) {
                                
                                .Identifier => |arg_ident| {
                                    const name = arg_ident.token.span.getString();
                                    sub_list.append(scope.builder.allocator, name) catch @panic("Out of Memory.");
                                },

                                .List => |list| {
                                    for (list.expressions.items) |expr| {
                                        switch (expr.data.*) {
                                            
                                            .Identifier => |arg_ident| {
                                                const name = arg_ident.token.span.getString();
                                                sub_list.append(scope.builder.allocator, name) catch @panic("Out of Memory.");
                                            },

                                            else => {
                                                var log = scope.builder.logger.logError(
                                                    "Invalid Generic", .{}, 
                                                    "You can only use names in a generic list. Generic[T, T2]", .{});
                                                log.addLine(
                                                    "Invalid format for a generic.", .{}, 
                                                    generic.arguements.span);
                                                return;
                                            }
                                        }
                                    }
                                },

                                else => {
                                    var log = scope.builder.logger.logError(
                                        "Invalid Generic", .{}, 
                                        "You can only use names in a generic list. Generic[T, T2]", .{});
                                    log.addLine(
                                        "Invalid format for a generic.", .{}, 
                                        generic.arguements.span);
                                    return;
                                }
                            }

                            scope.addGenericType(
                                token.span.getString(), 
                                sub_list, 
                                base, 
                                visability, 
                                decl.name.span) catch return;
                        },

                        else => {
                            var log = scope.builder.logger.logError(
                                "Invalid Declaration", .{}, 
                                "Names can only use a..z, 0..9 and _", .{});
                            log.addLine(
                                "Invalid name for a declaration.", .{}, 
                                decl.name.span);
                        },
                    }
                },

                // Built-ins don't need to be added
                .Builtin => return,

                else => {
                    var log = scope.builder.logger.logError(
                        "Invalid Declaration", .{}, 
                        "Names can only use a..z, 0..9 and _",. {});
                    log.addLine(
                        "Invalid name for a declaration.", .{}, 
                        decl.name.span);
                },
            }
        },

        .List => |list| {
            
            for (list.expressions.items) |expr| {
                collectTypeIdsFromExpressions(scope, expr, visability);
            }
        },

        else => return,
    }
}