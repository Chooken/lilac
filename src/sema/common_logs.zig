const std = @import("std");
const logging = @import("../logger.zig");
const files = @import("../files.zig");
const sema = @import("../sema.zig");
const typed = @import("../typed.zig");

pub fn cantHaveParameterError(logger: logging.Logger, span: files.Span, function_name: []const u8, param: []const u8) void {
    var log = logger.logError(
        "Type Error", .{}, 
        "{s} can't have self as a parameter.", .{function_name});
    log.addLine(
        "{s} parameter declared here.", .{param}, 
        span);
}

pub fn TypeMismatch(builder: *sema.Builder, lhs: typed.TypedNode(typed.Expression), rhs: typed.TypedNode(typed.Expression)) void {

    var log = builder.logger.logError(
        "Type Mismatch", .{}, 
        "The left and right types do not match.", .{});
    log.addLine(
        "Has Type: {s}", .{ builder.typeListToString(lhs.value)}, 
        lhs.span);
    log.addLine(
        "Has Type: {s}", .{ builder.typeListToString(rhs.value)}, 
        rhs.span);
}