const std = @import("std");
const lexer = @import("lexer.zig");
const Token = @import("tokens.zig").Token;
const parser = @import("parser.zig");
const untyped = @import("untyped.zig");
const typed = @import("typed.zig");
const sema = @import("sema.zig");
const logger = @import("logger.zig");
const files = @import("files.zig");

pub fn testCompile(io: std.Io, allocator: std.mem.Allocator, file_path: []const u8) void {

    logger.init(io);
    defer logger.deinit(io);

    var user_arena = std.heap.ArenaAllocator.init(allocator);
    const user_allocator = user_arena.allocator();

    var uprogram = untyped.Program {};

    const opt_file = files.loadFile(io, user_allocator, file_path);

    if (opt_file) |file| {

        // Parsing file to Untyped Ast.
        uprogram.root_module.asts.append(user_allocator, parser.parse(file, user_allocator, false)) catch {
            return;
        };

        untyped.printAST(&uprogram.root_module.asts.items[0]);
    }

    // Convert Untyped Program to Typed Program.
    _ = sema.runSema(user_allocator, &uprogram);

    // Make a new arena for IR so we can free user data structures.
    var ir_arena = std.heap.ArenaAllocator.init(allocator);
    const ir_allocator = ir_arena.allocator();

    // Convert Typed Ast to IR.

    // Since IR isn't user facing deallocate files and ast's as errors are IR based now.
    user_arena.deinit();

    _ = ir_allocator;
    ir_arena.deinit();
}

