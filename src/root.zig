const std = @import("std");
const lexer = @import("lexer.zig");
const Token = @import("tokens.zig").Token;
const parser = @import("parser.zig");
const untyped = @import("untyped.zig");
const logger = @import("logger.zig");

pub fn testCompile(io: std.Io, allocator: std.mem.Allocator, file_path: []const u8) void {

    logger.init(io);
    defer logger.deinit(io);

    const opt_file = loadFile(io, allocator, file_path);

    if (opt_file) |file| {

        var uprogram = untyped.Program {};

        uprogram.root_module.asts.append(allocator, parser.parse(file, allocator, false)) catch {
            return;
        };

        // ast.printAST(&file_ast);
    }
}

pub fn loadFile(io: std.Io, allocator: std.mem.Allocator, file_path: []const u8) ?[:0]u8 {
    return std.Io.Dir.cwd().readFileAllocOptions(io, file_path, allocator, .unlimited, .@"1", 0) catch null;
}