const std = @import("std");

pub const Token = struct {
    token_type: TokenType,
    start: usize,
    end: usize,

    pub const reserved_tokens = std.StaticStringMap(TokenType).initComptime(.{
        .{"obj", TokenType.Object},
        .{"func", TokenType.Function},
        .{"enum", TokenType.Enum},
        .{"interface", TokenType.Interface},
        .{"self", TokenType.Self},
        .{"nothing", TokenType.Nothing},
        .{"return", TokenType.Return},
        .{"break", TokenType.Break},
        .{"continue", TokenType.Continue},
        .{"true", TokenType.Bool},
        .{"false", TokenType.Bool},
        .{"ref", TokenType.Reference},
        .{"unknown", TokenType.Unknown},
    });

    pub fn getReserved(token: []const u8) ?TokenType {
        return reserved_tokens.get(token);
    }

    pub fn not_eof(self: *const Token) bool {
        return self.token_type != TokenType.EndOfFile;
    }
};

pub const TokenType = enum {
    Binary,
    Number,
    Bool,
    String,
    Char,
    Self,
    Nothing,
    Unknown,

    Identifier,

    // Assignment
    Equals, // =
    PlusEquals, // +=
    MinusEquals, // -=
    DivEquals, // /=
    TimesEquals, // *=
    PercentEquals, // %=

    // Bin op
    EqualsEquals, // ==
    ExclamationEquals, // !=
    Plus, // +
    Minus, // -
    Slash, // /
    Percentage, // %
    Asterisk, // *
    LessThan, // <
    LessThanOrEquals, // <=
    ShiftLeft, // <<
    GreaterThan, // >
    GreaterThanOrEquals, // >=
    ShiftRight, // >>

    Exclamation, // !

    // Symbols
    OpenParentheses, // (
    CloseParentheses, // )

    OpenBrace, // {
    CloseBrace, // }

    OpenBracket, // [
    CloseBracket, // ]

    Semicolon, // ;
    Colon, // :
    Comma, // ,
    Dot, // .

    RightArrow, // ->
    FatRightArrow, // =>

    // Reserved Tokens
    Function,
    Object,
    Enum,
    Interface,

    // Jump Tokens
    Return,
    Break,
    Continue,

    Reference,

    // Compiler Ops
    Builtin, // @

    // File
    EndOfFile,

    Invalid,
};