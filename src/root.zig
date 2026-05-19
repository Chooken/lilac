const std = @import("std");
const lexer = @import("lexer.zig");
const Token = @import("tokens.zig").Token;
const parser = @import("parser.zig");

pub fn testCompile(io: std.Io, allocator: std.mem.Allocator, file_path: []const u8) void {

    const opt_file = loadFile(io, allocator, file_path);

    if (opt_file) |file| {

        _ = parser.parse(file, allocator) catch {
            return;
        };
    }
}

pub fn loadFile(io: std.Io, allocator: std.mem.Allocator, file_path: []const u8) ?[:0]u8 {
    return std.Io.Dir.cwd().readFileAllocOptions(io, file_path, allocator, .unlimited, .@"1", 0) catch null;
}