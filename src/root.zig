const std = @import("std");
const lexer = @import("lexer.zig");
const Token = @import("tokens.zig").Token;

pub fn testCompile(io: std.Io, allocator: std.mem.Allocator, file_path: []const u8) void {

    const opt_file = loadFile(io, allocator, file_path);

    if (opt_file) |file| {

        var tokenizer = lexer.Tokenizer.init(file);
        var file_tokens: std.ArrayList(Token) = .empty;

        while (tokenizer.next()) |token| {
            //std.debug.print("{s} - {s}\n", .{@tagName(token.token_type), file[token.start..token.end]});
            file_tokens.append(allocator, token) catch {
                std.debug.print("Ran out of memory whilst lexing.\n", .{});
                return;
            };
        }


    }
}

pub fn loadFile(io: std.Io, allocator: std.mem.Allocator, file_path: []const u8) ?[:0]u8 {
    return std.Io.Dir.cwd().readFileAllocOptions(io, file_path, allocator, .unlimited, .@"1", 0) catch null;
}