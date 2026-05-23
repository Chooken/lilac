const std = @import("std");
const tokens = @import("tokens.zig");

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
    
    pub fn logAt(self: *Logger, comptime fmt: []const u8, args: anytype, hint: ?[]const u8, start: usize, end: usize, log_level: LogLevel, source: []const u8) void {
        const message = std.fmt.allocPrint(self.allocator, fmt, args) catch return;
        
        self.logs.append(self.allocator, .{
            .start = start,
            .end = end,
            .source = source,
            .message = message,
            .hint = hint,
            .level = log_level,
        }) catch @panic("Out of Memory.");
    }

    pub fn logError(self: *Logger, comptime fmt: []const u8, args: anytype, hint: ?[]const u8, start: usize, end: usize, source: []const u8) void {
        self.logAt(fmt, args, hint, start, end, .Error, source);
    }

    pub fn logWarning(self: *Logger, comptime fmt: []const u8, args: anytype, hint: ?[]const u8, start: usize, end: usize, source: []const u8) void {
        self.logAt(fmt, args, hint, start, end, .Warning, source);
    }

    pub fn logNote(self: *Logger, comptime fmt: []const u8, args: anytype, hint: ?[]const u8, start: usize, end: usize, source: []const u8) void {
        self.logAt(fmt, args, hint, start, end, .Note, source);
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

pub const Log = struct {
    start: usize,
    end: usize,
    source: []const u8,
    message: []const u8,
    hint: ?[]const u8,
    level: LogLevel,

    pub fn Error(msg: []const u8, token: tokens.Token, source: []const u8) Log {
        return .{
            .token = token,
            .source = source,
            .message = msg,
            .level = .Error,  
        };
    }

    pub fn Warning(msg: []const u8, token: tokens.Token, source: []const u8) Log {
        return .{
            .token = token,
            .source = source,
            .message = msg,
            .level = .Warning,  
        };
    }

    pub fn Note(msg: []const u8, token: tokens.Token, source: []const u8) Log {
        return .{
            .token = token,
            .source = source,
            .message = msg,
            .level = .Note,  
        };
    }
};

pub const Location = struct {
    start: usize,
    end: usize,
    line: usize,
    character: usize,

    pub fn get(log: Log) Location {

        var line: usize = 1;
        var character: usize = 1;

        for (0..log.start) |index| {

            character += 1;

            if (log.source[index] == '\n') {
                line += 1;
                character = 1;
            }
        }

        var start_print = log.start;

        while (start_print > 0) {

            if (log.source[start_print] == '\n') {
                start_print += 1;
                break;
            }

            start_print -= 1;
        }

        var end_print = log.start;

        while (end_print < log.source.len - 1) {

            if (log.source[end_print] == '\n') {
                end_print -= 1;
                break;
            }

            end_print += 1;
        }

        return .{
            .start = start_print,
            .end = end_print,
            .line = line,
            .character = character,
        };
    }
};

pub fn printLogs(logger: Logger) void {
    for (logger.logs.items) |log| {
        printLog(log);
    }
}

pub fn printLog(log: Log) void {

    const location = Location.get(log);
    const log_color = log.level.getColor();

    setColor(log_color);
    setColor(.bold);
    print(@tagName(log.level));
    setColor(.reset);
    printFmt(":{d}:{d}", .{location.line, location.character});
    endLine();

    setColor(log_color);
    setColor(.bold);
    print("| ");
    setColor(.reset);
    printFmt("{s}", .{log.source[location.start..location.end]});
    endLine();

    const line_options = LineOptions { 
        .indent = log.start - location.start 
    };

    setColor(log_color);
    setColor(.bold);
    print("| ");
    setColor(.reset);
    startLine(line_options);
    setColor(log_color);
    setColor(.bold);
    for (log.start - 1..@min(log.end - 1, location.end - 1)) |_| print("^");
    setColor(.reset);
    endLine();
    
    setColor(log_color);
    setColor(.bold);
    print("| ");
    setColor(.reset);
    startLine(line_options);
    print(log.message);
    endLine();

    if (log.hint) |hint| {
        setColor(log_color);
        setColor(.bold);
        print("| ");
        setColor(.reset);
        startLine(line_options);
        printColored("hint: ", .{}, .bright_green);
        setColor(.dim);
        print(hint);
        setColor(.reset);
        endLine();
    }
}