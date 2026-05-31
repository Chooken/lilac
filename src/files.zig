const std = @import("std");

pub const File = struct {
    file: []const u8,
    source: [:0]const u8,
};

pub const FileId = struct {
    id: usize,

    pub fn getFile(self: FileId) File {
        if (sources.items.len == 0) {
            return .{
                .file = "Failed",
                .source = "File Error.",
            };
        }
        return sources.items[self.id];
    }
};

var sources = std.ArrayList(File).empty;

pub fn loadFile(io: std.Io, allocator: std.mem.Allocator, file_path: []const u8) ?FileId {
    if (loadFileFromDisk(io, allocator, file_path)) |file| {
        sources.append(allocator, .{
            .file = file_path,
            .source = file
        }) catch @panic("Out of Memory.");
        return .{
            .id = sources.items.len - 1,
        };
    }

    return null;
}

fn loadFileFromDisk(io: std.Io, allocator: std.mem.Allocator, file_path: []const u8) ?[:0]u8 {
    return std.Io.Dir.cwd().readFileAllocOptions(io, file_path, allocator, .unlimited, .@"1", 0) catch null;
}