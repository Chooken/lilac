const std = @import("std");
const Io = std.Io;

const Lilac = @import("Lilac");
const logging = Lilac.logging;

pub fn main(init: std.process.Init) !void {

    const io = init.io;
    const allocator = init.arena.allocator();

    var tests_dir = try Io.Dir.cwd().openDir(io, "tests", .{ .iterate = true });
    defer tests_dir.close(io);

    var iter = tests_dir.iterate();

    var passed: usize = 0;
    var failed: usize = 0;

    Lilac.init(io);
    defer Lilac.deinit(io);

    while (try iter.next(io)) |entry| {
        if (entry.kind != .file) continue;

        const name = entry.name;

        if (!isTestInput(name)) continue;

        const path = try std.fmt.allocPrint(allocator, "tests/{s}", .{name});

        const ok = Lilac.testCompile(io, allocator, path);

        const expect_fail = std.mem.startsWith(u8, name, "FAILS_");

        if (ok or (!ok and expect_fail)) {
            passed += 1;
            logging.setColor(.reset);
            logging.setColor(.dim);
            logging.print("[");
            logging.setColor(.reset);
            logging.setColor(.bold);
            logging.setColor(.bright_green);
            logging.print("PASS");
            logging.setColor(.reset);
            logging.setColor(.dim);
            logging.print("]: ");
            logging.setColor(.reset);
            logging.printFmt("{s}\n", .{name});
        } else {
            failed += 1;

            logging.setColor(.reset);
            logging.setColor(.dim);
            logging.print("[");
            logging.setColor(.reset);
            logging.setColor(.bold);
            logging.setColor(.bright_red);
            logging.print("FAIL");
            logging.setColor(.reset);
            logging.setColor(.dim);
            logging.print("]: ");
            logging.setColor(.reset);
            logging.printFmt("{s}\n", .{ name });
        }
    }
    logging.setColor(.reset);
    logging.printFmt("Passed: {d}", .{passed});
    logging.printFmt("Failed: {d}", .{passed});

    logging.deinit(io);
}

fn isTestInput(name: []const u8) bool {
    if (std.mem.endsWith(u8, name, ".lilac")) return true;
    return false;
}
