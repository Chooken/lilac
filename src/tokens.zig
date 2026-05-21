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
        .{"if", TokenType.If},
        .{"else", TokenType.Else},
        .{"match", TokenType.Match},
        .{"loop", TokenType.Loop},
        .{"using", TokenType.Using},
        .{"as", TokenType.As},
        .{"private", TokenType.Private},
        .{"and", TokenType.And},
        .{"or", TokenType.Or},
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
    GreaterThan, // >
    GreaterThanOrEquals, // >=
    And,
    Or,

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

    // Statements.
    If,
    Else,
    Match,
    Loop,
    Using,
    As,
    Private,

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

    pub fn toString(token_type: TokenType) []const u8 {
        return switch (token_type) {
            .Object => "obj",
            .Function => "func",
            .Enum => "enum",
            .Interface => "interface",
            .Self => "self",
            .Nothing => "nothing",
            .Return => "return",
            .Break => "break",
            .Continue => "continue",
            .Reference => "ref",
            .Unknown => "unknown",
            .If => "if",
            .Else => "else",
            .Match => "match",
            .Loop => "loop",
            .Using => "using",
            .As => "as",
            .Private => "private",
            .And => "and",
            .Or => "or",

            .Binary => "Binary",
            .Number => "Number",
            .Bool => "Bool",
            .String => "String",
            .Char => "Char",
            
            .Identifier => "Indentifier",
            .Builtin => "Builtin",

            .Equals => "=",
            .PlusEquals => "+=",
            .MinusEquals => "-=",
            .TimesEquals => "*=",
            .DivEquals => "/=",
            .PercentEquals => "%=",

            .EqualsEquals => "==",
            .ExclamationEquals => "!=",
            .Plus => "+",
            .Minus => "-",
            .Asterisk => "*",
            .Slash => "/",
            .Percentage => "%",
            .LessThan => "<",
            .LessThanOrEquals => "<=",
            .GreaterThan => ">",
            .GreaterThanOrEquals => ">=",

            .Exclamation => "!",

            .OpenParentheses => "(",
            .CloseParentheses => ")",

            .OpenBrace => "{",
            .CloseBrace => "}",

            .OpenBracket => "[",
            .CloseBracket => "]",

            .Semicolon => ";",
            .Colon => ":",
            .Comma => ",",
            .Dot => ".",

            .RightArrow => "->",
            .FatRightArrow => "=>",

            .EndOfFile => "End of File",

            .Invalid => "Invalid",
        };
    }
};