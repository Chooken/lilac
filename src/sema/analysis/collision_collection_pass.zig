const std = @import("std");
const sema = @import("../../sema.zig");

pub fn collectCollisionErrors(builder: *sema.Builder) void {

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

fn logDeclaration(builder: *sema.Builder, decl: *sema.builder.Declaration) void {

    if (decl.collisions.items.len == 0) {
        return;
    }

    var log = builder.logger.logError(
        "Declaration Collision", .{}, 
        "Try renaming the conflicting declarations.", .{});

    if (decl.span) |span| {
        log.addLine(
            "First declaration here.", .{}, 
            span);
    }

    for (decl.collisions.items) |collision| {
        if (collision) |span| {
            log.addLine(
                "Collision here.", .{}, 
                span);
        }
    }
}