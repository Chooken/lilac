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

pub const Line = struct {
    start: usize = 0,
    end: usize = 0,
    number: usize = 0,
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

    while (log_iterator.next()) |file_logs| {
        const log_file = file_logs.key_ptr.getFile();

        _ = printPadding(log_color, null, 0);
        setColor(.dim);
        printFmt("file: {s}", .{log_file.file});
        setColor(.reset);
        endLine();

        if (file_logs.value_ptr.items.len == 0) {
            continue;
        }

        var last_line: Line = .{};
        var current_line: Line = .{};
        var line_number: usize = 1;
        var opt_last_printed_line: ?usize = null;

        var current_log_index: usize = 0;
        var last_line_offset: usize = 0;

        var comment_last_line: bool = false;

        for (log_file.source, 0..) |character, index| {

            if (character != '\n') {
                continue;
            }

            last_line = current_line;
            current_line = Line {
                .start = if (last_line.end == 0) 0 else last_line.end + 1,
                .end = index,
                .number = line_number,
            };
            line_number += 1;

            if (comment_last_line) {
                last_line_offset = printLine(allocator, log_color, current_line.number, current_line.start, current_line.end, log_file.source);
                opt_last_printed_line = current_line.number;
                comment_last_line = false;
            }

            while (current_log_index < file_logs.value_ptr.items.len) {
                
                const logline = file_logs.value_ptr.items[current_log_index];

                if (logline.start >= index) {
                    break;
                }

                if (opt_last_printed_line) |last_printed_line| {

                    const distance = current_line.number - last_printed_line;

                    if (distance > 2) {
                        _ = printPadding(log_color, "...", 0);
                        endLine();  
                    }

                    if (distance > 1 and last_line.number != 0) {
                        _ = printLine(allocator, log_color, last_line.number, last_line.start, last_line.end, log_file.source);
                    }

                    if (distance > 0) {
                        last_line_offset = printLine(allocator, log_color, current_line.number, current_line.start, current_line.end, log_file.source);
                        opt_last_printed_line = current_line.number;
                    }

                } else {

                    if (last_line.number != 0) {
                        _ = printLine(allocator, log_color, last_line.number, last_line.start, last_line.end, log_file.source);
                    }

                    last_line_offset = printLine(allocator, log_color, current_line.number, current_line.start, current_line.end, log_file.source);
                    opt_last_printed_line = current_line.number;
                }

                printLineComment(log_color, last_line_offset + (logline.start - current_line.start) - 1, logline);
                current_log_index += 1;
                comment_last_line = true;
            }

            if (!comment_last_line and current_log_index >= file_logs.value_ptr.items.len) {
                break;
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

fn printLine(allocator: std.mem.Allocator, log_color: std.Io.Terminal.Color, line: usize, start: usize, end: usize, source: []const u8) usize {
    const line_number = std.fmt.allocPrint(allocator, "{d}", .{line}) catch @panic("Out of Memory.");
    defer allocator.free(line_number);

    const offset = printPadding(log_color, line_number, 6);
    
    print(source[start..end]);
    endLine();

    return offset;
}

fn printLineComment(log_color: std.Io.Terminal.Color, offset: usize, logline: LogLine) void {
    _ = printPadding(log_color, "|", offset);
                    
    setColor(.red);
    setColor(.bold);
    for (logline.start..logline.end) |_| {
        print("^");
    }
    setColor(.reset);
    endLine();

    _ = printPadding(log_color, "|", offset);
    print(logline.message);
    setColor(.reset);
    endLine();
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