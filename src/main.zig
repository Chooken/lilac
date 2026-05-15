const std = @import("std");
const Io = std.Io;

const Lilac = @import("Lilac");

pub fn main(init: std.process.Init) !void {
    // This is appropriate for anything that lives as long as the process.
    const arena: std.mem.Allocator = init.arena.allocator();

    // Accessing command line arguments:
    const args = try init.minimal.args.toSlice(arena);

    for (args) |arg| {
        std.log.info("arg: {s}", .{arg});
    }

    // In order to do I/O operations need an `Io` instance.
    const io = init.io;
    
    var debug_allocator = std.heap.DebugAllocator(.{}).init;
    const allocator = debug_allocator.allocator();

    Lilac.testCompile(io, allocator, "tests/test.lilac");
}