const std = @import("std");
const sema = @import("../../sema.zig");
const untyped = @import("../../untyped.zig");
const typed = @import("../../typed.zig");
const tokens = @import("../../tokens.zig");

pub fn collectTypeData(builder: *sema.Builder) void {

    if (builder.getScope(builder.root)) |scope| {
        collectTypeDataModule(scope, &builder.uprogram.root_module);
    }
}

pub fn collectTypeDataModule(scope: *sema.Scope, module: *untyped.Module) void {
    for (module.asts.items) |*ast| {
        collectTypeDataFromBlock(scope, &ast.root_block, .public);
    }

    var sub_mod_iter = module.submodules.iterator();

    while (sub_mod_iter.next()) |sub_mod_entry| {
        if (scope.getType(sub_mod_entry.key_ptr.*, null, .private) catch continue) |typeid| {

            if (scope.builder.getScope(typeid)) |sub_scope| {
                collectTypeDataModule(sub_scope, sub_mod_entry.value_ptr);
            }
        }
    }
}

pub fn collectTypeDataFromBlock(scope: *sema.Scope, block: *untyped.Block, visability: sema.Visability) void {
    for (block.body.items) |stmt| {
        collectTypeDataFromStatements(scope, stmt, visability);
    }
}

pub fn collectTypeDataFromStatements(scope: *sema.Scope, statement: untyped.Node(untyped.Statement), visability: sema.Visability) void {
    
    switch (statement.data.*) {

        .Block => |block| {
            collectTypeDataFromBlock(scope, block.data, visability);
        },

        .Expression => |expr| {
            collectTypeDataFromExpressions(scope, expr, visability);
        },

        .Private => |private| {
            collectTypeDataFromBlock(scope, private.data, .private);
        },

        else => return,
    }
}

pub fn collectTypeDataFromExpressions(scope: *sema.Scope, expression: untyped.Node(untyped.Expression), visability: sema.Visability) void {

    switch (expression.data.*) {

        .Identifier => {
            
            // Add Inline Field to Enums.
            return;
        },

        .Declaration => |decl| {

            switch (decl.name.data.*) {

                .Identifier => |ident| {

                    const name = ident.token.span.getString();

                    switch (decl.decl_type.data.*) {

                        .Object => |obj| {
                            if (scope.getType(name, decl.name.span, .private) catch return) |typeid| {
                                if (scope.builder.getScope(typeid)) |new_scope| {
                                    collectTypeDataFromBlock(new_scope, obj.data, .public);
                                }
                            }
                        },  

                        .Enum => |_enum| {
                            if (scope.getType(name, decl.name.span, .private) catch return) |typeid| {
                                if (scope.builder.getScope(typeid)) |new_scope| {
                                    scope.addDecl("value", 
                                        decl.name.span, 
                                        .{ 
                                            .Field = .{ 
                                                .id = scope.builder.bit8,
                                                .is_ref = false, }}, 
                                        .private) catch return;
                                    collectTypeDataFromBlock(new_scope, _enum.data, .public);
                                }
                            }
                        },

                        .Union => |_union| {
                            if (scope.getType(name, decl.name.span, .private) catch return) |typeid| {
                                if (scope.builder.getScope(typeid)) |new_scope| {
                                    collectTypeDataFromBlock(new_scope, _union.data, .public);
                                }
                            }
                        },

                        .Interface => |interfaces| {
                            if (scope.getType(name, decl.name.span, .private) catch return) |typeid| {
                                if (scope.builder.getScope(typeid)) |new_scope| {
                                    collectTypeDataFromBlock(new_scope, interfaces.data, .public);
                                }
                            }
                        },

                        // Will need to do setter for func decl.
                        .Function => |function| {

                            var requires_self = false;

                            var result: ?typed.TypeId = null;
                            
                            switch (function.prototype.data.*) {

                                .FuncPrototype => |*proto| {
                                    result = sema.getFunctionTypeId(
                                        scope,
                                        proto, 
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
                                    decl.name.span) catch return;
                            }
                        },

                        else => {
                            sema.addFieldToTypeData(
                                scope, 
                                name, 
                                decl.name,
                                decl.decl_type, 
                                visability);
                        },
                    }
                },

                .Builtin => |builtin| {
                    
                    const identifier = builtin.token.span.getString();

                    const function = switch (decl.decl_type.data.*) {

                        .Function => |*f| f,

                        else => {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "You can only declare builtin functions like: @init: func () self", .{});
                            log.addLine(
                                "This type isn't a function. Is it meant to be a builtin?", .{}, 
                                decl.decl_type.span);
                            return;
                        }
                    };

                    const proto = switch (function.prototype.data.*) {
                        .FuncPrototype => |*proto| proto,
                        else => {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "Functions can only have function prototype as types.", .{});
                            log.addLine(
                                "This type isn't a function prototype.", .{}, 
                                decl.decl_type.span);
                            return;
                        }
                    };

                    if (std.mem.eql(u8, "@init", identifier)) {

                        if (proto.arguments) |args| {
                            switch (args.data.*) {
                                .Self => {
                                    var log = scope.builder.logger.logError(
                                        "Type Error", .{}, 
                                        "@init can't have self as a parameter.", .{});
                                    log.addLine(
                                        "Self parameter declared here.", .{}, 
                                        args.span);
                                    return;
                                },

                                .List => |list| {
                                    switch (list.expressions.items[0].data.*) {
                                        .Self => {
                                            var log = scope.builder.logger.logError(
                                                "Type Error", .{}, 
                                                "@init can't have self as a parameter.", .{});
                                            log.addLine(
                                                "Self parameter declared here.", .{}, 
                                                list.expressions.items[0].span);
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
                                "@init requires self as the only return.", .{});
                            log.addLine(
                                "This needs to just be self.", .{}, 
                                proto.returns.span);
                            return;
                        }

                        if (sema.getFunctionTypeId(scope, proto, false, function.is_inline)) |typeid| {

                            const funct_id = scope.builder.addFunction(typeid, function.is_inline, false);

                            const result = scope.builder.initialisers.getOrPut(scope.builder.allocator, scope.typeid) catch @panic("Out of Memory.");

                            if (result.found_existing) {
                                result.value_ptr.collisions.append(scope.builder.allocator, decl.name.span) catch @panic("Out of Memory.");
                                return;
                            }

                            result.value_ptr.* = .{
                                .span = decl.name.span,
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
                                    "@on_override requires have self as the only parameter.", .{});
                                log.addLine(
                                    "Only put self in here.", .{}, 
                                    args.span);
                                return;
                            }
                        } else {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@on_override requires have self as the parameter.", .{});
                            log.addLine(
                                "Add self as a parameter.", .{}, 
                                function.prototype.span);
                            return;
                        }

                        if (proto.returns.data.* != .Nothing) {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@on_override requires have nothing as the return.", .{});
                            log.addLine(
                                "Add nothing here.", .{}, 
                                proto.returns.span);
                            return;
                        }

                        if (sema.getFunctionTypeId(scope, proto, false, function.is_inline)) |typeid| {

                            const funct_id = scope.builder.addFunction(typeid, function.is_inline, false);

                            const result = scope.builder.onoverride.getOrPut(scope.builder.allocator, scope.typeid) catch @panic("Out of Memory.");

                            if (result.found_existing) {
                                result.value_ptr.collisions.append(scope.builder.allocator, decl.name.span) catch @panic("Out of Memory.");
                                return;
                            }

                            result.value_ptr.* = .{
                                .span = decl.name.span,
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
                                    "@on_copy requires have self as the only parameter.", .{});
                                log.addLine(
                                    "Only put self in here.", .{}, 
                                    args.span);
                                return;
                            }
                        } else {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@on_copy requires have self as the parameter.", .{});
                            log.addLine(
                                "Add self as a parameter.", .{}, 
                                function.prototype.span);
                            return;
                        }

                        if (proto.returns.data.* != .Nothing) {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@on_copy requires have nothing as the return.", .{});
                            log.addLine(
                                "Add nothing here.", .{}, 
                                proto.returns.span);
                            return;
                        }

                        if (sema.getFunctionTypeId(scope, proto, false, function.is_inline)) |typeid| {

                            const funct_id = scope.builder.addFunction(typeid, function.is_inline, false);

                            const result = scope.builder.oncopy.getOrPut(scope.builder.allocator, scope.typeid) catch @panic("Out of Memory.");

                            if (result.found_existing) {
                                result.value_ptr.collisions.append(scope.builder.allocator, decl.name.span) catch @panic("Out of Memory.");
                                return;
                            }

                            result.value_ptr.* = .{
                                .span = decl.name.span,
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
                                    "@on_drop requires have self as the only parameter.", .{});
                                log.addLine(
                                    "Only put self in here.", .{}, 
                                    args.span);
                                return;
                            }
                        } else {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@on_drop requires have self as the parameter.", .{});
                            log.addLine(
                                "Add self as a parameter.", .{}, 
                                function.prototype.span);
                            return;
                        }

                        if (proto.returns.data.* != .Nothing) {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@on_drop requires have nothing as the return.", .{});
                            log.addLine(
                                "Add nothing here.", .{}, 
                                proto.returns.span);
                            return;
                        }

                        if (sema.getFunctionTypeId(scope, proto, false, function.is_inline)) |typeid| {

                            const funct_id = scope.builder.addFunction(typeid, function.is_inline, false);

                            const result = scope.builder.ondrop.getOrPut(scope.builder.allocator, scope.typeid) catch @panic("Out of Memory.");

                            if (result.found_existing) {
                                result.value_ptr.collisions.append(scope.builder.allocator, decl.name.span) catch @panic("Out of Memory.");
                                return;
                            }

                            result.value_ptr.* = .{
                                .span = decl.name.span,
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
                                        "@conversion with a ref type as a from.", .{});
                                    log.addLine( 
                                        "The self type is a ref type.", .{}, 
                                        args.span);
                                },

                                .List => {
                                    var log = scope.builder.logger.logError(
                                        "Type Error", .{}, 
                                        "@conversion can't have multiple parameters.", .{});
                                    log.addLine(
                                        "Here are the parameters.", .{}, 
                                        args.span);
                                    return;
                                },

                                else => {}
                            }
                        } else {
                            var log = scope.builder.logger.logError(
                                "Type Error", .{}, 
                                "@conversion requires a parameter as the from type.", .{});
                            log.addLine(
                                "Add a from parameter.", .{}, 
                                function.prototype.span);
                            return;
                        }

                        switch (proto.returns.data.*) {

                            .List => {
                                var log = scope.builder.logger.logError(
                                    "Type Error", .{}, 
                                    "@conversion can't have multiple results.", .{});
                                log.addLine(
                                    "Here are the parameters.", .{}, 
                                    proto.returns.span);
                                return;
                            },

                            else => {},
                        }

                        if (sema.getFunctionTypeId(scope, proto, false, function.is_inline)) |typeid| {

                            const func_type = scope.builder.getType(typeid);

                            const funct_id = scope.builder.addFunction(typeid, function.is_inline, false);

                            const result = scope.builder.conversions.getOrPut(scope.builder.allocator, .{ 
                                .from = func_type.data.?.Function.inputs.items[0], 
                                .to = func_type.data.?.Function.outputs.items[0]
                            }) catch @panic("Out of Memory.");

                            if (result.found_existing) {
                                result.value_ptr.collisions.append(scope.builder.allocator, decl.name.span) catch @panic("Out of Memory.");
                                return;
                            }
                            
                            const from_scope = scope.builder.getScope(result.key_ptr.from.id).?;
                            const to_scope = scope.builder.getScope(result.key_ptr.to.id).?;

                            std.debug.print("Added conversion {s} -> {s}\n", .{from_scope.allocFullName(), to_scope.allocFullName()});

                            result.value_ptr.* = .{
                                .span = decl.name.span,
                                .visability = .public,
                                .decl_type = .{
                                    .Function = funct_id,
                                }
                            };
                        }
                    } 
                    else if (std.mem.eql(u8, "@add", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, function, proto, .Plus);
                    } 
                    else if (std.mem.eql(u8, "@sub", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, function, proto, .Minus);
                    } 
                    else if (std.mem.eql(u8, "@mul", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, function, proto, .Asterisk);
                    } 
                    else if (std.mem.eql(u8, "@div", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, function, proto, .Slash);
                    } 
                    else if (std.mem.eql(u8, "@mod", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, function, proto, .Percentage);
                    } 
                    else if (std.mem.eql(u8, "@less_than", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, function, proto, .LessThan);
                    } 
                    else if (std.mem.eql(u8, "@greater_than", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, function, proto, .GreaterThan);
                    } 
                    else if (std.mem.eql(u8, "@less_than_or_equal", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, function, proto, .LessThanOrEquals);
                    } 
                    else if (std.mem.eql(u8, "@greater_than_or_equal", identifier)) {
                        addBinopOperatorFunction(scope, identifier, decl.name, function, proto, .GreaterThanOrEquals);
                    }
                    else if (std.mem.eql(u8, "@negate", identifier)) {
                        addPrefixOperatorFunction(scope, identifier, decl.name, function, proto, .Minus);
                    } 
                },

                // Built-ins don't need to be added.
                // Generics get genned by usage not declaration.
                .Generic => return,

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

        .Assignment => |assignment| {
            collectTypeDataFromExpressions(scope, assignment.assignee, visability);
        },

        .List => |list| {
            for (list.expressions.items) |expr| {
                collectTypeDataFromExpressions(scope, expr, visability);
            }
        },

        else => return,
    }
}

pub fn addBinopOperatorFunction(scope: *sema.Scope, name: []const u8, name_node: untyped.Node(untyped.Expression), func: *untyped.Function, proto: *untyped.FuncPrototype, operation: tokens.TokenType) void {

    if (proto.arguments) |args| {
        switch (args.data.*) {

            .List => |list| {
                
                if (list.expressions.items.len != 2) {
                    var log = scope.builder.logger.logError(
                        "Type Error", .{}, 
                        "{s} requires two parameters for the left and right sides of the operator", .{name});
                    log.addLine(
                        "Remove some parameters.", .{}, 
                        args.span);
                    return;
                }
            },

            else => {
                var log = scope.builder.logger.logError(
                    "Type Error", .{}, 
                    "{s} requires two parameters for the left and right sides of the operator", .{name});
                log.addLine(
                    "Add another parameter.", .{}, 
                    args.span);
                return;
            }
        }
    } else {
        var log = scope.builder.logger.logError(
            "Type Error", .{}, 
            "{s} requires two parameters for the left and right sides of the operator", .{name});
        log.addLine(
            "Add a left and right parameter.", .{}, 
            func.prototype.span);
        return;
    }

    switch (proto.returns.data.*) {

        .Nothing => {
            var log = scope.builder.logger.logWarning(
                "Concerning Operator", .{}, 
                "Operators typically return a value. This could be a sign you are using them weirdly.",. {});
            log.addLine(
                "Did you mean to put nothing here?", .{}, 
                proto.returns.span);
        },

        .List => {
            var log = scope.builder.logger.logWarning(
                "Concerning Operator", .{}, 
                "Operators typically return a value. This could be a sign you are using them weirdly.", .{});
            log.addLine(
                "Did you mean to put multiple returns here?", .{}, 
                proto.returns.span);
        },

        else => {},
    }

    if (sema.getFunctionTypeId(scope, proto, false, func.is_inline)) |typeid| {

        const func_type = scope.builder.getType(typeid);

        const funct_id = scope.builder.addFunction(typeid, func.is_inline, false);

        const result = scope.builder.binop_operation.getOrPut(scope.builder.allocator, .{ 
            .lhs = func_type.data.?.Function.inputs.items[0],
            .rhs = func_type.data.?.Function.inputs.items[1],
            .op = operation,
        }) catch @panic("Out of Memory.");

        if (result.found_existing) {
            result.value_ptr.collisions.append(scope.builder.allocator, name_node.span) catch @panic("Out of Memory.");
            return;
        }
        
        const lhs_scope = scope.builder.getScope(result.key_ptr.lhs.id).?;
        const rhs_scope = scope.builder.getScope(result.key_ptr.rhs.id).?;

        std.debug.print("Added op {s} {s} {s}\n", .{lhs_scope.allocFullName(), operation.toString(), rhs_scope.allocFullName()});

        result.value_ptr.* = .{
            .span = name_node.span,
            .visability = .public,
            .decl_type = .{
                .Function = funct_id,
            }
        };
    }
}

pub fn addPrefixOperatorFunction(scope: *sema.Scope, name: []const u8, name_node: untyped.Node(untyped.Expression), func: *untyped.Function, proto: *untyped.FuncPrototype, operation: tokens.TokenType) void {

    if (proto.arguments) |args| {
        switch (args.data.*) {

            .List => {
                
                var log = scope.builder.logger.logError(
                    "Type Error", .{},
                    "{s} requires only one parameter for the unary operator.", .{name});
                log.addLine(
                    "Remove some parameters.", .{}, 
                    args.span);
                return;
            },

            else => {}
        }
    } else {
        var log = scope.builder.logger.logError(
            "Type Error", .{}, 
            "{s} requires one parameters for the unary operator.", .{name});
        log.addLine(
            "Add a parameter.", .{}, 
            func.prototype.span);
        return;
    }

    switch (proto.returns.data.*) {

        .Nothing => {
            var log = scope.builder.logger.logWarning(
                "Concerning Operator", .{}, 
                "Operators typically return a value. This could be a sign you are using them weirdly.", .{});
            log.addLine(
                "Did you mean to put nothing here?", .{}, 
                proto.returns.span);
        },

        .List => {
            var log = scope.builder.logger.logWarning(
                "Concerning Operator", .{}, 
                "Operators typically return a value. This could be a sign you are using them weirdly.", .{});
            log.addLine(
                "Did you mean to put multiple returns here?", .{}, 
                proto.returns.span);
        },

        else => {},
    }

    if (sema.getFunctionTypeId(scope, proto, false, func.is_inline)) |typeid| {

        const func_type = scope.builder.getType(typeid);

        const funct_id = scope.builder.addFunction(typeid, func.is_inline, false);

        const result = scope.builder.prefix_operation.getOrPut(scope.builder.allocator, .{ 
            .value = func_type.data.?.Function.inputs.items[0],
            .op = operation,
        }) catch @panic("Out of Memory.");

        if (result.found_existing) {
            result.value_ptr.collisions.append(scope.builder.allocator, name_node.span) catch @panic("Out of Memory.");
            return;
        }
        
        const value_scope = scope.builder.getScope(result.key_ptr.value.id).?;

        std.debug.print("Added op {s} {s}\n", .{operation.toString(), value_scope.allocFullName()});

        result.value_ptr.* = .{
            .span = name_node.span,
            .visability = .public,
            .decl_type = .{
                .Function = funct_id,
            }
        };
    }
}