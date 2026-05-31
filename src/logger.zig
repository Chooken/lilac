const std = @import("std");
const tokens = @import("tokens.zig");
const files = @import("files.zig");

pub const LineOptions = struct {
    indent: usize,
};

var buffer: [512]u8 = undefined;
var file: std.Io.File = undefined;
var file_writer: std.Io.File.Writer = undefined;
var terminal: std.Io.Terminal = undefined;

pub fn init(io: std.Io) void {

    file = std.Io.File.stdout();
    file_writer = file.writer(io, &buffer);
    terminal = std.Io.Terminal {
        .writer = &file_writer.interface,
        .mode = std.Io.Terminal.Mode.detect(io, file, false, false) catch @panic("Failed to Detect Terminal Mode."),
    };
}

pub fn deinit(io: std.Io) void {
    file.close(io);
}

pub fn setColor(color: std.Io.Terminal.Color) void {
    terminal.setColor(color) catch {};
}

pub fn startLine(options: LineOptions) void {
    terminal.setColor(.reset) catch {};
    if (options.indent > 0) {
        indent_print(options.indent);
    } 
}

pub fn printColored(comptime fmt: []const u8, args: anytype, color: std.Io.Terminal.Color) void {
    terminal.setColor(color) catch {};
    terminal.writer.print(fmt, args) catch return;
    terminal.setColor(.reset) catch {};
    terminal.writer.flush() catch return;
}

pub fn print(string: []const u8) void {
    terminal.writer.writeAll(string) catch return;
    terminal.writer.flush() catch return;
}

pub fn printFmt(comptime fmt: []const u8, args: anytype) void {
    terminal.writer.print(fmt, args) catch return;
    terminal.writer.flush() catch return;
}

pub fn endLine() void {
    terminal.writer.writeByte('\n') catch return;
    terminal.writer.flush() catch return;
}

fn indent_print(indent: usize) void {
    for (0..indent) |_| terminal.writer.writeByte(' ') catch return;
}

pub const Logger = struct {
    allocator: std.mem.Allocator,
    logs: std.ArrayList(Log) = .empty,

    pub fn log(self: *Logger, _log: Log) void {
        self.logs.append(self.allocator, _log) catch @panic("Out of Memory.");
    }

    pub fn logError(self: *Logger, comptime fmt: []const u8, args: anytype, hint: ?[]const u8) *Log {
        self.log(.{
            .message = std.fmt.allocPrint(self.allocator, fmt, args) catch @panic("Out of Message."),
            .hint = hint,
            .level = .Error,
        });

        return &self.logs.items[self.logs.items.len - 1];
    }

    pub fn logWarning(self: *Logger, comptime fmt: []const u8, args: anytype, hint: ?[]const u8) *Log {
        self.log(.{
            .message = std.fmt.allocPrint(self.allocator, fmt, args) catch @panic("Out of Message."),
            .hint = hint,
            .level = .Warning,
        });

        return &self.logs.items[self.logs.items.len];
    }

    pub fn logNote(self: *Logger, comptime fmt: []const u8, args: anytype, hint: ?[]const u8) *Log {
        self.log(.{
            .message = std.fmt.allocPrint(self.allocator, fmt, args) catch @panic("Out of Message."),
            .hint = hint,
            .level = .Note,
        });

        return &self.logs.items[self.logs.items.len];
    }

    pub fn deinit(self: *Logger) void {
        self.logs.deinit(self.allocator);
    }
};

pub const LogLevel = enum {
    Error,
    Warning,
    Note,

    pub fn getColor(level: LogLevel) std.Io.Terminal.Color {
        return switch (level) {
            .Error => std.Io.Terminal.Color.bright_red,
            .Warning => std.Io.Terminal.Color.bright_yellow,
            .Note => std.Io.Terminal.Color.bright_blue,
        };
    }
};

pub const LogLine = struct { 
    start: usize,
    end: usize,
    message: []const u8,
};

pub const Log = struct {
    message: []const u8,
    logs: std.AutoHashMapUnmanaged(files.FileId, std.ArrayList(LogLine)) = .empty,
    hint: ?[]const u8,
    level: LogLevel,

    pub fn addLine(
        self: *Log, 
        allocator: std.mem.Allocator, 
        file_id: files.FileId, 
        comptime fmt: []const u8, 
        args: anytype, 
        start: usize, 
        end: usize
    ) void {
        const val = self.logs.getOrPut(allocator, file_id) catch @panic("Out of Memory.");

        if (!val.found_existing) {
            val.value_ptr.* = .empty;
        }

        val.value_ptr.append(allocator, .{
            .message = std.fmt.allocPrint(allocator, fmt, args) catch @panic("Out of Memory."),
            .start = start,
            .end = end,
        }) catch @panic("Out of Memory.");
    }
};

pub const Location = struct {
    start: usize,
    end: usize,
    line: usize,

    pub fn get(allocator: std.mem.Allocator, log: LogLine, source: []const u8) std.ArrayList(Location) {

        var locations = std.ArrayList(Location).empty;

        var line: usize = 1;
        var last_line_index: usize = 0;
        var current_line_index: usize = 0;

        for (0..log.start) |index| {

            if (source[index] == '\n') {
                line += 1;
                last_line_index = current_line_index;
                current_line_index = index + 1;
            }
        }

        line -= 1;

        var start_print = last_line_index;
        var index: usize = last_line_index;
        var count: usize = 3;

        while (count != 0 and index < source.len) {

            if (index >= source.len) {
                locations.append(allocator, .{
                    .start = start_print,
                    .end = index,
                    .line = line,
                }) catch @panic("Out of Memory.");
                break;
            }

            if (source[index] == '\n') {
                locations.append(allocator, .{
                    .start = start_print,
                    .end = index,
                    .line = line,
                }) catch @panic("Out of Memory.");
                line += 1;
                count -= 1;
                start_print = index + 1;
            }

            index += 1;
        }

        return locations;
    }
};

pub fn printLogs(logger: Logger, allocator: std.mem.Allocator) void {
    for (logger.logs.items) |log| {
        printLog(log, allocator);
    }
}

pub fn printLog(log: Log, allocator: std.mem.Allocator) void {

    const log_color = log.level.getColor();

    setColor(log_color);
    setColor(.bold);
    print(@tagName(log.level));
    setColor(.reset);
    printFmt(": {s}", .{log.message});
    endLine();

    // Log lines and Hints
    var log_iterator = log.logs.iterator();

    var prev_last: usize = 0;

    while (log_iterator.next()) |file_logs| {
        const log_file = file_logs.key_ptr.getFile();

        _ = printPadding(log_color, null, 0);
        setColor(.dim);
        printFmt("file: {s}", .{log_file.file});
        setColor(.reset);
        endLine();

        var offset: usize = 0;

        for (file_logs.value_ptr.items) |logline| {

            var locations = Location.get(allocator, logline, log_file.source);
            defer locations.deinit(allocator);

            for (locations.items, 0..) |location, index| {

                if (prev_last < location.line) {
                    
                    if (location.line - prev_last > 1) {
                        _ = printPadding(log_color, "...", 0);
                        endLine();      
                    }

                    prev_last = location.line;

                    const line_number = std.fmt.allocPrint(allocator, "{d}", .{location.line}) catch @panic("Out of Memory.");
                    defer allocator.free(line_number);

                    offset = printPadding(log_color, line_number, 6);
                    
                    print(log_file.source[location.start..location.end]);
                    endLine();
                }

                if (index == 1) {

                    const token_offset: usize = offset + (logline.start - location.start - 1);

                    _ = printPadding(log_color, "|", token_offset);
                    
                    setColor(.red);
                    setColor(.bold);
                    for (logline.start..logline.end) |_| {
                        print("^");
                    }
                    setColor(.reset);
                    endLine();

                    _ = printPadding(log_color, "|", token_offset);
                    print(logline.message);
                    setColor(.reset);
                    endLine();
                }
            }
        }
    }

    if (log.hint) |hint| {
        _ = printPadding(log_color, null, 0);
        printColored("hint: ", .{}, .bright_green);
        setColor(.dim);
        print(hint);
        setColor(.reset);
        endLine();
    }
}

fn printPadding(log_color: std.Io.Terminal.Color, line_head: ?[]const u8, padding: usize) usize {
    setColor(log_color);
    setColor(.bold);
    print("| ");
    setColor(.reset);

    var length: usize = padding;

    if (line_head) |string| {

        setColor(.dim);
        print(string); 
        setColor(.reset);

        if (string.len < padding) {
            indent_print(padding - string.len);
        } else {
            length = string.len;
        }
    } else {
        indent_print(padding);
    }

    return length + 1;
}