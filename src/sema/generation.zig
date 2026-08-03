const std = @import("std");
const sema = @import("../sema.zig");
const untyped = @import("../untyped.zig");
const typed = @import("../typed.zig");
const tokens = @import("../tokens.zig");
const common_logs = @import("common_logs.zig");

pub fn generate(builder: *sema.Builder) void {

    if (builder.getScope(builder.root)) |scope| {
        generateModule(scope, &builder.uprogram.root_module);
    }
}

pub fn generateModule(scope: *sema.Scope, module: *untyped.Module) void {
    for (module.asts.items) |*ast| {
        generateFromBlock(scope, &ast.root_block);
    }

    var sub_mod_iter = module.submodules.iterator();

    while (sub_mod_iter.next()) |sub_mod_entry| {
        if (scope.getType(
            sub_mod_entry.key_ptr.*, 
            null, 
            .private) catch continue) |typeid| {

            if (scope.builder.getScope(typeid)) |sub_scope| {
                generateModule(sub_scope, sub_mod_entry.value_ptr);
            }
        }
    }
}

pub fn generateFromBlock(scope: *sema.Scope, block: *untyped.Block) void {
    for (block.body.items) |stmt| {
        generateFromStatements(scope, stmt);
    }
}

pub fn generateFromStatements(scope: *sema.Scope, statement: untyped.Node(untyped.Statement)) void {
    
    switch (statement.data.*) {

        .Block => |block| {
            generateFromBlock(scope, block.data);
        },

        .Expression => |expr| {
            generateFromExpressions(scope, expr);
        },

        .Private => |private| {
            generateFromBlock(scope, private.data);
        },

        else => return,
    }
}

pub fn generateFromExpressions(scope: *sema.Scope, expression: untyped.Node(untyped.Expression)) void {

    switch (expression.data.*) {

        .Declaration => |decl| {

            const ident = switch (decl.name.data.*) {
                
                .Identifier => |ident| ident,

                else => return,
            };

            const name = ident.token.span.getString();
            
            switch (decl.decl_type.data.*) {
                
                .Function => |*function| {
                    std.debug.print("generating {s}\n", .{name});
                    generateFunction(
                        scope, 
                        function, 
                        name, 
                        scope.getDecl(name).?.decl_type.Function);
                },

                else => {},
            }
        },

        else => {},
    }
}

pub fn generateFunction(scope: *sema.Scope, function: *untyped.Function, func_name: []const u8, func_id: typed.FunctionId) void {

    const prototype = switch (function.prototype.data.*) {

        .FuncPrototype => |proto| proto,

        else => return,
    };

    const funct = scope.builder.getFunction(func_id);
    const proto_type = scope.builder.getType(funct.typeid).data.?.Function;

    const funct_scope = scope.builder.getScope(scope.builder.getNewType(func_name, .Function, scope)).?;

    if (prototype.arguments) |args| {

        switch (args.data.*) {

            .Declaration => |decl| {

                const ident = switch (decl.name.data.*) {
                    
                    .Identifier => |ident| ident,

                    else => return,
                };
                
                const name = ident.token.span.getString();

                funct_scope.addField(name, args.span, .public, proto_type.inputs.items[0]) catch return;
            }, 

            .List => |list| {

                for (list.expressions.items, 0..) |arg, index| {

                    switch (arg.data.*) {

                        .Declaration => |decl| {
                            const ident = switch (decl.name.data.*) {
                                
                                .Identifier => |ident| ident,

                                else => return,
                            };
                            
                            const name = ident.token.span.getString();

                            funct_scope.addField(name, arg.span, .public, proto_type.inputs.items[index]) catch return;
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
            
            const name = ident.token.span.getString();

            funct_scope.addField(name, prototype.returns.span, .public, proto_type.inputs.items[0]) catch return;
        }, 

        .List => |list| {

            for (list.expressions.items, 0..) |return_, index| {

                switch (return_.data.*) {

                    .Declaration => |decl| {
                        const ident = switch (decl.name.data.*) {
                            
                            .Identifier => |ident| ident,

                            else => return,
                        };
                        
                        const name = ident.token.span.getString();

                        funct_scope.addField(name, return_.span, .public, proto_type.outputs.items[index]) catch return;
                    },

                    else => return,
                }
            }
        },

        .Nothing => {},

        else => return,
    }

    
    const funct_ = scope.builder.getFunction(func_id);
    funct_.block = generateFunctionStatements(funct_scope, function.body);
}

pub fn generateFunctionBlock(scope: *sema.Scope, block: untyped.Node(untyped.Block)) typed.TypedNode(typed.Block) {

    const block_scope = scope.builder.getScope(scope.builder.getNewType(null, .Function, scope)).?;
    var typed_block = typed.Block {};

    for (block.data.body.items) |stmt| {
        typed_block.body.append(scope.builder.allocator, generateFunctionStatements(block_scope, stmt)) catch @panic("Out of Memory.");
    }

    return typed.TypedNode(typed.Block).init(
        scope.builder.allocator, 
        block.span, 
        .empty, 
        typed_block);
}

pub fn generateFunctionStatements(scope: *sema.Scope, statement: untyped.Node(untyped.Statement)) typed.TypedNode(typed.Statement) {

    switch (statement.data.*) {

        .Block => |block| {

            const typed_block = generateFunctionBlock(scope, block);

            return scope.createTypedStatement(
                statement, 
                .{ 
                    .Block = typed_block,
                }, 
                typed_block.value);
        },

        .Loop => |body| {
            const loop = generateFunctionStatements(scope, body);

            return scope.createTypedStatement(
                statement, 
                .{
                    .Loop = loop,
                },
                .empty);
        },

        .Expression => |expr| {
            const typed_expr = generateFunctionExpressions(scope, expr, null) catch {
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

pub fn generateFunctionExpressions(scope: *sema.Scope, expression: untyped.Node(untyped.Expression), opt_inferred_type: ?typed.TypeRef) sema.SemaError!typed.TypedNode(typed.Expression) {

    switch (expression.data.*) {
        
        .Assignment => |assignment| {

            var note_log = scope.builder.logger.logNote(
                "Assignment", .{}, 
                null, .{});
            note_log.addLine(
                "Assignment", .{}, 
                expression.span);
            
            switch (assignment.assignee.data.*) {
                
                .Identifier, .Member, .Declaration => { },

                else => {
                    return scope.createTypedExpression(expression, .Error, .empty);
                }
            }

            const assignee = try generateFunctionExpressions(scope, assignment.assignee, null);
            var value = try generateFunctionExpressions(scope, assignment.value, assignee.getInferable());

            if (generateTypeEquality(scope, assignee.value, &value)) {
                return scope.createTypedExpression(
                    expression, 
                    .{ .Assignment = .{ 
                        .assignee = assignee, 
                        .value = value,
                        .inlined = assignment.inlined } },
                    assignee.value);
            }

            return scope.createTypedExpression(expression, .Error, .empty);
        },

        .Declaration => |decl| {

            const name = switch (decl.name.data.*) {
                .Identifier => |identifer| identifer,
                else => unreachable,
            };

            const name_string = name.token.span.getString();

            switch (decl.decl_type.data.*) {

                .Setter => |setter| {
                    if (sema.ExprToTypeRef(scope, setter.settee)) |decl_type_ref| {

                        scope.addField(name_string, decl.name.span, .public, decl_type_ref) catch return sema.SemaError.InvalidType;

                        var types = std.ArrayList(typed.TypeRef).empty;
                        types.append(scope.builder.allocator, decl_type_ref) catch @panic("Out of Memory.");

                        const typed_setter = typed.Setter {
                            .settee = decl_type_ref.id,
                            .body = try generateSetterBlock(scope, scope.builder.getScope(decl_type_ref.id).?, setter.body),
                        };

                        const decl_node = scope.createTypedExpression(
                            expression, 
                            .{ .Declaration = name.token.span.getString() }, 
                            types);

                        const setter_node = scope.createTypedExpression(
                            decl.decl_type, 
                            .{ .Setter = typed_setter }, 
                            types);

                        const assignment = typed.Assignment {
                            .assignee = decl_node,
                            .value = setter_node,
                            .inlined = false,
                        };

                        return scope.createTypedExpression(
                            expression,
                            .{ .Assignment = assignment }, 
                            .empty);
                    }
                },

                else => {
                    if (sema.ExprToTypeRef(scope, decl.decl_type)) |decl_type_ref| {

                        scope.addField(name_string, decl.name.span, .public, decl_type_ref) catch return sema.SemaError.InvalidType;

                        var types = std.ArrayList(typed.TypeRef).empty;
                        types.append(scope.builder.allocator, decl_type_ref) catch @panic("Out of Memory.");

                        return scope.createTypedExpression(
                            expression, 
                            .{ .Declaration = name.token.span.getString() }, 
                            types);
                    }
                }
            }

            return sema.SemaError.InvalidType;
        },

        .Literal => |literal| {

            var types = std.ArrayList(typed.TypeRef).empty;
            
            const literal_value = literal.token.span.getString();
            
            switch (literal.token.token_type) {

                .Number => types.append(scope.builder.allocator, .{ .id = scope.builder.numberLiteral, .is_ref = false }) catch @panic("Out of Memory."),

                .Bool => types.append(scope.builder.allocator, .{ .id = scope.builder.bit8, .is_ref = false }) catch @panic("Out of Memory."),

                .Binary => {

                    const bits_per_char: usize = switch (literal_value[1]) {
                        'b' => 1,
                        'x' => 4,
                        else => {
                            var log = scope.builder.logger.logError(
                                "Literal Error", .{},
                                null, .{});

                            log.addLine(
                                "Binary Literal Type doesn't exist.", .{}, 
                                expression.span);

                            return sema.SemaError.InvalidType;
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

                    bit_size = @max(2, bit_size);

                    if (bit_size > 64) {

                        var log = scope.builder.logger.logError(
                            "Type Error", .{},
                            "Literals can only go up to 0xFFFF_FFFF_FFFF_FFFF or 64 bits. Use a smaller value.", .{});

                        log.addLine(
                            "This value would be {d} bits long.", .{bit_size}, 
                            expression.span);

                        return sema.SemaError.TypeMismatch;
                    }

                    if (opt_inferred_type) |inferred_type| {

                        if (inferred_type.id.index == scope.builder.bit8.index) {

                            if (bit_size > 8) {

                                var log = scope.builder.logger.logError(
                                    "Type Error", .{},
                                    "@bit8 can only fit values up to 0b1000_0000 or 0xFF. Use a smaller value or a larger type.", .{});

                                log.addLine(
                                    "This value is {d} bits long.", .{bit_size}, 
                                    expression.span);

                                return sema.SemaError.TypeMismatch;
                            }

                            types.append(scope.builder.allocator, .{ .id = scope.builder.bit8, .is_ref = false }) catch @panic("Out of Memory.");

                        } else if (inferred_type.id.index == scope.builder.bit16.index) {

                            if (bit_size > 16) {

                                var log = scope.builder.logger.logError(
                                    "Type Error", .{},
                                    "@bit16 can only fit values up to 0b1000_0000_0000_0000 or 0xFFFF. Use a smaller value or a larger type.", .{});

                                log.addLine(
                                    "This value is {d} bits long.", .{bit_size}, 
                                    expression.span);

                                return sema.SemaError.TypeMismatch;
                            }

                            types.append(scope.builder.allocator, .{ .id = scope.builder.bit16, .is_ref = false }) catch @panic("Out of Memory.");

                        } else if (inferred_type.id.index == scope.builder.bit32.index) {

                            if (bit_size > 32) {

                                var log = scope.builder.logger.logError(
                                    "Type Error", .{},
                                    "@bit32 can only fit values up to 0xFFFF_FFFF. Use a smaller value or a larger type.", .{});

                                log.addLine(
                                    "This value is {d} bits long.", .{bit_size}, 
                                    expression.span);

                                return sema.SemaError.TypeMismatch;
                            }

                            types.append(scope.builder.allocator, .{ .id = scope.builder.bit32, .is_ref = false }) catch @panic("Out of Memory.");

                        } else if (inferred_type.id.index == scope.builder.bitNative.index){

                            if (bit_size > scope.builder.settings.bitNativeSize) {

                                var log = scope.builder.logger.logError(
                                    "Type Error", .{},
                                    "@bit64 can only fit values up to 0xFFFF_FFFF_FFFF_FFFF. Use a smaller value or a larger type.", .{});

                                log.addLine(
                                    "This value is {d} bits long.", .{bit_size}, 
                                    expression.span);

                                return sema.SemaError.TypeMismatch;

                            } else if (bit_size > 32) {

                                var log = scope.builder.logger.logWarning(
                                    "Concerning Literal", .{},
                                    "@bitNative recommendeds to only use literals up to 0xFFFF_FFFF as you don't know if the target is 32 bit.", .{});

                                log.addLine(
                                    "This value is {d} bits long.", .{bit_size}, 
                                    expression.span);
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
                        "Literal not implemented yet....", .{});

                    log.addLine(
                        "{s} literal", .{@tagName(literal.token.token_type)}, 
                        expression.span);

                    return sema.SemaError.InvalidType;
                }
            }

            return scope.createTypedExpression(
                expression, 
                .{ .Literal = literal.token.span.getString() }, 
                types);
        },

        .Identifier => |identifier| {

            const name = identifier.token.span.getString();

            if (scope.getDecl(name)) |decl| {

                switch (decl.decl_type) {

                    .Field => |type_ref| {
                        var types = std.ArrayList(typed.TypeRef).empty;
                        types.append(scope.builder.allocator, type_ref) catch @panic("Out of Memory.");

                        return scope.createTypedExpression(
                            expression, 
                            .{ .Identifier = identifier.token.span.getString() }, 
                            types);
                    },

                    .Function => |function_id| {
                        var types = std.ArrayList(typed.TypeRef).empty;

                        const funct = scope.builder.getFunction(function_id);
                        types.append(scope.builder.allocator, .{ .id = funct.typeid, .is_ref = false }) catch @panic("Out of Memory.");

                        return scope.createTypedExpression(
                            expression, 
                            .{ .Function = function_id }, 
                            types);
                    },

                    else => {
                        var log = scope.builder.logger.logError(
                            "Invalid Identifer", .{}, 
                            null, .{});
                        log.addLine(
                            "This identifier can't be used as a field.", .{}, 
                            expression.span);
                        return sema.SemaError.InvalidType;
                    }
                }
            }

            var log = scope.builder.logger.logError(
                "Missing Field", .{}, 
                null, .{});
            log.addLine(
                "This field doesn't exist.", .{}, 
                expression.span);

            return sema.SemaError.InvalidType;
        },

        .ImplicitMember => |implicit_member| {
            if (opt_inferred_type) |inferred_type| {

                const opt_implicit_parent = scope.builder.getScope(inferred_type.id);

                if (opt_implicit_parent) |implicit_parent| {

                    var types = std.ArrayList(typed.TypeRef).empty;
                    types.append(scope.builder.allocator, inferred_type) catch @panic("Out of Memory.");

                    const parent = scope.createTypedExpression(
                        expression, 
                        .Type, 
                        types);

                    var child = try generateFunctionExpressions(implicit_parent, implicit_member, inferred_type);

                    if (generateTypeEquality(scope, parent.value, &child)) {
                        
                        const member = scope.createTypedExpression(
                            expression, 
                            .{ .Member = .{ .parent = parent, .child = child } }, 
                            types);

                        return member;
                    }

                    return sema.SemaError.InvalidType;
                }

                var log = scope.builder.logger.logError(
                    "Type Error", .{},
                    null, .{});

                log.addLine(
                    "Doesn't Exist.", .{}, 
                    expression.span);

                return sema.SemaError.InvalidType;
            }

            var log = scope.builder.logger.logError(
                "Type Error", .{},
                null, .{});

            log.addLine(
                "Can't infer the type of this.", .{}, 
                expression.span);

            return sema.SemaError.InvalidType;
        },

        .Call => |call| {
            
            const callee = try generateFunctionExpressions(scope, call.callee, opt_inferred_type);

            std.debug.print("{s}\n", .{scope.builder.typeListToString(callee.value)});

            if (callee.value.items.len == 1) {

                const type_data = scope.builder.getType(callee.value.items[0].id);
                
                switch (type_data.data.?) {

                    .Function => |proto| {
                        
                        if (call.arguements) |args| {

                            var expressions = std.ArrayList(typed.TypedNode(typed.Expression)).empty;
                            
                            switch (args.data.*) {
                                .List => |list| {
                                    for (list.expressions.items, 0..) |arg, index| {
                                        expressions.append(
                                            scope.builder.allocator, 
                                            try generateFunctionExpressions(
                                                scope, 
                                                arg, 
                                                if (index < proto.inputs.items.len) proto.inputs.items[index] else null)) catch @panic("Out of Memory.");
                                    }
                                },

                                else => {
                                    expressions.append(
                                        scope.builder.allocator, 
                                        try generateFunctionExpressions(
                                            scope, 
                                            args, 
                                            if (proto.inputs.items.len > 0) proto.inputs.items[0] else null)) catch @panic("Out of Memory.");
                                },
                            }

                            if (expressions.items.len == proto.inputs.items.len) {

                            }

                            var log = scope.builder.logger.logError(
                                "Type Mismatch", .{}, 
                                null, .{});
                            
                            log.addLine(
                                "Has type: {s}", .{scope.builder.typeListToString(callee.value)}, 
                                expression.span);

                            var types = std.ArrayList(u8).empty;
                            defer types.deinit(scope.builder.allocator);

                            for (expressions.items, 0..) |expr, index| {
                                
                                if (index != 0) {
                                    types.appendSlice(scope.builder.allocator, ", ") catch @panic("Out of Memory.");
                                }
                                
                                if (expr.value.items.len > 1) {
                                    types.appendSlice(scope.builder.allocator, "[") catch @panic("Out of Memory.");
                                    types.appendSlice(scope.builder.allocator, scope.builder.typeListToString(expr.value)) catch @panic("Out of Memory.");
                                    types.appendSlice(scope.builder.allocator, "]") catch @panic("Out of Memory.");
                                } else {
                                    types.appendSlice(scope.builder.allocator, scope.builder.typeListToString(expr.value)) catch @panic("Out of Memory.");
                                }
                            }

                            log.addLine(
                                "calls types: {s}", .{types.items}, 
                                expression.span);

                            return sema.SemaError.TypeMismatch;

                        } else {
                            const args = scope.createTypedExpression(
                                expression, 
                                .Error, 
                                .empty);
                            if (callee.value.items.len != 0) {
                                common_logs.TypeMismatch(
                                    scope.builder, 
                                    callee, 
                                    args);
                                return sema.SemaError.TypeMismatch;
                            }
                        }
                    },

                    else => {}
                }
            }

            var log = scope.builder.logger.logError(
                "Type Mismatch", .{}, 
                null, .{});
            log.addLine(
                "Can't call a non function type.", .{}, 
                expression.span);
            log.addLine(
                "Has type: {s}", .{scope.builder.typeListToString(callee.value)}, 
                callee.span);
            return sema.SemaError.TypeMismatch;
        },

        else => {
            var log = scope.builder.logger.logError(
                "Invalid Expression", .{},
                null, .{});

            log.addLine(
                "Here", .{}, 
                expression.span);

            return sema.SemaError.InvalidType;
        }
    }
}

pub fn generateSetterBlock(scope: *sema.Scope, settee_scope: *sema.Scope, block: untyped.Node(untyped.Block)) sema.SemaError!std.ArrayList(typed.TypedNode(typed.Assignment)) {
    
    var assignments = std.ArrayList(typed.TypedNode(typed.Assignment)).empty;

    for (block.data.body.items) |statement| {
        
        const expr = switch (statement.data.*) {
            .Expression => |e| e,
            else => {
                var log = scope.builder.logger.logError(
                    "Invalid Statement", .{}, 
                    "Setters can only have assignments to fields in them.", .{});
                log.addLine(
                    "This is a {s} in a setter.", .{@tagName(statement.data.*)}, 
                    statement.span);
                return sema.SemaError.InvalidExpression;
            }
        };

        const assignment = switch (expr.data.*) {

            .Assignment => |a| a,

            else => {
                var log = scope.builder.logger.logError(
                    "Invalid Expression", .{}, 
                    "Setters can only have assignments to fields in them.", .{});
                log.addLine(
                    "This is a {s} in a setter.", .{@tagName(expr.data.*)}, 
                    expr.span);
                return sema.SemaError.InvalidExpression;
            }
        };

        const assignee = switch (assignment.assignee.data.*) {
            .Identifier => |ident| ident,
            else => {
                var log = scope.builder.logger.logError(
                    "Invalid Assignment", .{}, 
                    "Setters can only have assignments to fields within the type.", .{});
                log.addLine(
                    "You are assigning to a {s}.", .{@tagName(assignment.assignee.data.*)}, 
                    assignment.assignee.span);
                return sema.SemaError.InvalidExpression;
            }
        };

        const decl = settee_scope.getDecl(assignee.token.span.getString()) orelse {
            var log = scope.builder.logger.logError(
                "Missing Field", .{}, 
                "Make sure your spelt the name of the field correctly.", .{});
            log.addLine(
                "This field doesn't exist in type {s}.", .{settee_scope.allocFullName()}, 
                assignment.assignee.span);
            return sema.SemaError.MissingDeclaration;
        };

        if (decl.decl_type != .Field) {
            var log = scope.builder.logger.logError(
                "Invalid Field", .{}, 
                "Was this identifier mean't to be a field?", .{});
            if (decl.span) |span| {
                log.addLine(
                    "This declaration isn't a field.", .{}, 
                    span);
            }
            log.addLine(
                "You are trying to assign to it here.", .{}, 
                expr.span);
            return sema.SemaError.InvalidExpression;
        }

        var types = std.ArrayList(typed.TypeRef).empty;
        types.append(scope.builder.allocator, decl.decl_type.Field) catch @panic("Out of Memory.");

        var value = try generateFunctionExpressions(
            scope, 
            assignment.value, 
            decl.decl_type.Field);

        const ident = scope.createTypedExpression(
            assignment.assignee, 
            .{ .Identifier = assignee.token.span.getString() }, 
            types);

        if (!generateTypeEquality(scope, types, &value)) {
            common_logs.TypeMismatch(scope.builder, ident, value);
            return sema.SemaError.TypeMismatch;
        }
        
        const typed_assignment = typed.TypedNode(typed.Assignment).init(
            scope.builder.allocator, 
            expr.span, 
            types, 
            .{
                .assignee = ident,
                .value = value,
                .inlined = false,
            });

        assignments.append(scope.builder.allocator, typed_assignment) catch @panic("Out of Memory.");
    }

    return assignments;
}

pub fn generateTypeEquality(scope: *sema.Scope, expected: std.ArrayList(typed.TypeRef), expr: *typed.TypedNode(typed.Expression)) bool {

    if (expected.items.len != expr.value.items.len) {
        return false;
    }

    if (expected.items.len == 0) {
        return true;
    }

    if (expected.items.len == 1) {
        const expected_type = expected.items[0];
        const expr_type = expr.value.items[0];


        switch (scope.typesAreCompatible(expected_type, expr_type)) {

            .Match => return true,

            .Failed => return false,

            .RequiresConversion => |conversion_funct_id| {
                const conv = typed.Call {
                    .arguements = expr.*,
                    .callee = conversion_funct_id
                };

                expr.* = typed.TypedNode(typed.Expression).init(
                    scope.builder.allocator, 
                    expr.span, 
                    expected, 
                    .{ .Call = conv });

                return true;
            },
        }

        return false;
    }

    var split = typed.Split {
        .results = .empty,
        .value = expr.*,
    };

    for (0..expected.items.len) |index| {

        const expected_type = expected.items[index];
        const expr_type = expr.value.items[index];

        switch (scope.typesAreCompatible(expected_type, expr_type)) {

            .Match => {
                split.results.append(
                    scope.builder.allocator, 
                    typed.TypedNode(typed.Expression).init(
                        scope.builder.allocator, 
                        expr.span, 
                        expected, 
                        .{ .SplitLiteral = .{ .index = index } })) catch @panic("Out of Memory.");
            },

            .Failed => return false,

            .RequiresConversion => |conversion_funct_id| {
                const conv_call = typed.Call {
                    .arguements = typed.TypedNode(typed.Expression).init(
                        scope.builder.allocator, 
                        expr.span, 
                        expected, 
                        .{ .SplitLiteral = .{ .index = index } }),
                    .callee = conversion_funct_id,
                };

                const conv_expr = typed.TypedNode(typed.Expression).init(
                    scope.builder.allocator, 
                    expr.span, 
                    expected, 
                    .{ .Call = conv_call });

                split.results.append(scope.builder.allocator, 
                    conv_expr) catch @panic("Out of Memory.");
                },
        }
    }

    expr.* = typed.TypedNode(typed.Expression).init(
        scope.builder.allocator, 
        expr.span, 
        expected, 
        .{ .Split = split });
    return true;
}


