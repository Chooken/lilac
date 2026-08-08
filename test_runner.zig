const std = @import("std");
const Io = std.Io;

const Lilac = @import("Lilac");
const logger = Lilac.logger;

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

        const error_count = Lilac.testCompile(io, allocator, path);

        const expect_fail = std.mem.startsWith(u8, name, "FAILS_");

        const ok = (error_count > 0) == expect_fail;

        if (ok) {
            passed += 1;
            logger.setColor(.reset);
            logger.setColor(.dim);
            logger.print("[");
            logger.setColor(.reset);
            logger.setColor(.bold);
            logger.setColor(.bright_green);
            logger.print("PASS");
            logger.setColor(.reset);
            logger.setColor(.dim);
            logger.print("]: ");
            logger.setColor(.reset);
            logger.printFmt("{s}\n", .{name});
        } else {
            failed += 1;

            logger.setColor(.reset);
            logger.setColor(.dim);
            logger.print("[");
            logger.setColor(.reset);
            logger.setColor(.bold);
            logger.setColor(.bright_red);
            logger.print("FAIL");
            logger.setColor(.reset);
            logger.setColor(.dim);
            logger.print("]: ");
            logger.setColor(.reset);
            logger.printFmt("{s} (expected {s}, got {d} error{s})\n",
                .{
                    name,
                    if (expect_fail) "diagnostics" else "no diagnostics",
                    error_count,
                    if (error_count == 1) "" else "s",
            });
        }
    }
    logger.setColor(.reset);
    logger.printFmt("Passed: {d}", .{passed});
    logger.printFmt("Failed: {d}", .{passed});

    logger.deinit(io);
}

fn isTestInput(name: []const u8) bool {
    if (std.mem.endsWith(u8, name, ".lilac")) return true;
    return false;
}
