const std = @import("std");
const untyped = @import("untyped.zig");
const typed = @import("typed.zig");

pub const Builder = struct {

    allocator: std.mem.Allocator,

    program: typed.Program = .{},
    uprogram: *untyped.Program,

    scopes: std.AutoHashMapUnmanaged(typed.TypeId, Scope) = .empty,
    root: typed.TypeId,
    
    // Events
    initialisers: std.AutoHashMapUnmanaged(typed.TypeId, typed.FunctionId) = .empty,
    oncopy: std.AutoHashMapUnmanaged(typed.TypeId, typed.FunctionId) = .empty,
    onoverride: std.AutoHashMapUnmanaged(typed.TypeId, typed.FunctionId) = .empty,
    ondrop: std.AutoHashMapUnmanaged(typed.TypeId, typed.FunctionId) = .empty,

    conversions: std.AutoHashMapUnmanaged(typed.Conversion, typed.FunctionId) = .empty,
    operation: std.AutoHashMapUnmanaged(typed.Operator, typed.FunctionId) = .empty,

    pub fn getNewType(self: *Builder, parent: ?*Scope) typed.TypeId {
        const typeid = self.program.addType(self.allocator, .{ });

        self.scopes.put(self.allocator, typeid, .{
            .builder = self,
            .parent = parent,
            .typeid = typeid,
        });

        return typeid;
    }

    pub fn getScope(self: *Builder, typeid: typed.TypeId) ?*Scope {
        return self.scopes.getPtr(typeid);
    }
};

pub const Scope = struct {
    builder: *Builder,
    parent: ?*Scope,
    typeid: typed.TypeId,

    usings: std.ArrayList(typed.TypeId) = .empty,
    alias: std.StringHashMapUnmanaged(typed.TypeId) = .empty,

    declarations: std.AutoHashMapUnmanaged(Visability, Declarations) = .empty,

    pub fn addTypeDecl(self: *Scope, identifier: []const u8, visability: Visability) typed.TypeId {

        const decl_iter = self.declarations.valueIterator();

        while (decl_iter.next()) |decl| {
            if (decl.getType(identifier)) |typeid| {
                
                return typeid;
            }
        }

        const decls = self.declarations.getOrPut(self.builder.allocator, visability, .{}) catch @panic("Out of Memory");
        decls.value_ptr.addTypeDecl(identifier, self);
    }

    pub fn getType(self: *Scope, identifier: []const u8, visability: Visability) ?typed.TypeId {
        
        if (visability == .private) {
            if (self.declarations.get(.private)) |private_decls| {
                if (private_decls.getType(identifier)) |typeid| {
                    return typeid;
                }
            }
        }

        if (self.declarations.get(.public)) |private_decls| {
            if (private_decls.getType(identifier)) |typeid| {
                return typeid;
            }
        }

        return null;
    }
};

pub const Visability = enum {
    public,
    private,
};

pub const Declarations = struct {
    types: std.StringHashMapUnmanaged(typed.TypeId) = .empty,
    functions: std.StringHashMapUnmanaged(typed.FunctionId) = .empty,
    generics: std.StringHashMapUnmanaged(Generic) = .empty,

    pub fn contains(self: *Declarations, identifier: []const u8) bool {
        return self.types.contains(identifier) or 
            self.functions.contains(identifier) or 
            self.generics.contains(identifier);
    }

    pub fn addTypeDecl(self: *Declarations, identifier: []const u8, scope: *Scope) typed.TypeId {
        const typeid= scope.builder.getNewType(scope);
        self.types.put(scope.builder.allocator, identifier, typeid) catch @panic("Out of Memory.");
        return typeid;
    }

    pub fn getType(self: *Declarations, identifier: []const u8) ?typed.TypeId {
        return self.types.get(identifier);
    }
};

pub const Generic = struct {
    base: *untyped.Generic,
    cache: std.StringHashMapUnmanaged(typed.TypeId) = .empty,
};

pub fn runSema(allocator: std.mem.Allocator, uprogram: *untyped.Program) typed.Program {

    var builder = Builder {
        .allocator = allocator,
        .uprogram = uprogram,
    };

    collectTypeIds(&builder);
}

pub fn collectTypeIds(builder: *Builder) void {

    builder.root = builder.getNewType(null);
    
    if (builder.getScope(builder.root)) |scope| {
        collectTypeIdsFromModule(scope, &builder.uprogram.root_module);
    }
}

pub fn collectTypeIdsFromModule(scope: *Scope, module: *untyped.Module) void {
    for (module.asts.items) |*ast| {
        collectTypeIdsFromAst(scope, ast);
    }

    const sub_mod_iter = module.submodules.iterator();

    while (sub_mod_iter.next()) |sub_mod_entry| {
        scope.
    }
}

pub fn collectTypeIdsFromAst(scope: *Scope, ast: *untyped.Ast) void {
    
}

pub fn collectFunctionTypeIds(builder: *Builder) void {
    
}