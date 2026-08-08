const std = @import("std");
const lexer = @import("lexer.zig");
const Token = @import("tokens.zig").Token;
const parser = @import("parser.zig");
const untyped = @import("untyped.zig");
const typed = @import("typed.zig");
const sema = @import("sema.zig");
const files = @import("files.zig");
pub const logging = @import("logger.zig");

pub fn init(io: std.Io) void {
    logging.init(io);
}

pub fn deinit(io: std.Io) void {
    logging.deinit(io);
}

pub fn testCompile(io: std.Io, allocator: std.mem.Allocator, file_path: []const u8) bool {

    var user_arena = std.heap.ArenaAllocator.init(allocator);
    const user_allocator = user_arena.allocator();

    var uprogram = untyped.Program {};

    const opt_file = files.loadFile(io, user_allocator, file_path);

    var logger = logging.Logger {
        .allocator = allocator,
    };
    defer logger.deinit();

    if (opt_file) |file| {

        // Parsing file to Untyped Ast.
        uprogram.root_module.asts.append(user_allocator, parser.parse(file, user_allocator, false, &logger)) catch {
            return false;
        };

        untyped.printAST(&uprogram.root_module.asts.items[0]);
    } else {
        logging.printFmt("Failed to Load File: {s}", .{file_path});
        @panic("");
    }

    // Convert Untyped Program to Typed Program.
    _ = sema.runSema(user_allocator, &uprogram, .{
        .bitNativeSize = 64,
        .warnOnOperatorTypeChange = true,
    }, &logger);

    // Make a new arena for IR so we can free user data structures.
    var ir_arena = std.heap.ArenaAllocator.init(allocator);
    const ir_allocator = ir_arena.allocator();

    // Convert Typed Ast to IR.

    // Since IR isn't user facing deallocate files and ast's as errors are IR based now.
    user_arena.deinit();

    _ = ir_allocator;
    ir_arena.deinit();

    logging.printLogs(logger, allocator);

    for (logger.logs.items) |log| {
        if (log.level == .Error) {
            return false;
        }
    }

    return true;
}

