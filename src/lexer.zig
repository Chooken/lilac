const std = @import("std");
const tokens = @import("tokens.zig");
const Token = tokens.Token;
const TokenType = tokens.TokenType;

pub const Tokenizer = struct {

    source: [:0]const u8,
    index: usize,

    pub fn init(source: [:0]const u8) Tokenizer
    {
        return .{
            .source = source,
            .index = 0,
        };
    }

    const State = enum {
        Start,
        Identifier,
        Ampersat, // @
        StringLiteral, // "Hello"
        CharLiteral, // 'A'
        NumberLiteral, // 0b, 0x, 10
        BinLiteral,
        HexLiteral,
        BeforeDotNumber,
        AfterDotNumber,
        Dot, // ., .0
        Equals, // =, ==, =>
        Plus, // +, +=
        Minus, // -, -=, ->
        Slash, // /, /=
        Asterisk, // *, *=
        Percentage, // %, %=
        LessThan, // <, <<, <=
        GreaterThan, // >, >>, >=
        Exclamation, // !, !=
        Comment,
        EndOfFile,
        Invalid,
    };

    pub fn next(self: *Tokenizer) ?Token {

        var token = Token {
            .start = self.index,
            .end = undefined,
            .token_type = undefined,
        };

        start: switch (State.Start) {
            .Start => { 
                switch (self.source[self.index]) {

                    0 => {
                        continue :start .EndOfFile;
                    },

                    ' ', '\n', '\t', '\r' => {
                        self.index += 1;
                        token.start = self.index;
                        continue :start .Start;
                    },

                    'a'...'z', 'A'...'Z', '_' => {
                        continue :start .Identifier;
                    },

                    '0' => {
                        continue :start .NumberLiteral;
                    },

                    '1'...'9' => {
                        continue :start .BeforeDotNumber;
                    },

                    '@' => {
                        continue :start .Ampersat;
                    },

                    '\"' => {
                        continue :start .StringLiteral;
                    },

                    '\'' => {
                        continue :start .CharLiteral;
                    },

                    '.' => {
                        continue :start .Dot;
                    },

                    '=' => {
                        continue :start .Equals;
                    },

                    '+' => {
                        continue :start .Plus;
                    },

                    '-' => {
                        continue :start .Minus;
                    },

                    '/' => {
                        continue :start .Slash;
                    },

                    '*' => {
                        continue :start .Asterisk;
                    },

                    '%' => {
                        continue :start .Percentage;
                    },

                    '<' => {
                        continue :start .LessThan;
                    },

                    '>' => {
                        continue :start .GreaterThan;
                    },

                    '!' => {
                        continue :start .Exclamation;
                    },

                    '{' => {
                        self.index += 1;
                        token.token_type = TokenType.OpenBrace;
                    },

                    '}' => {
                        self.index += 1;
                        token.token_type = TokenType.CloseBrace;
                    },

                    '(' => {
                        self.index += 1;
                        token.token_type = TokenType.OpenParentheses;
                    },

                    ')' => {
                        self.index += 1;
                        token.token_type = TokenType.CloseParentheses;
                    },

                    '[' => {
                        self.index += 1;
                        token.token_type = TokenType.OpenBracket;
                    },

                    ']' => {
                        self.index += 1;
                        token.token_type = TokenType.CloseBracket;
                    },

                    ':' => {
                        self.index += 1;
                        token.token_type = TokenType.Colon;
                    },

                    ';' => {
                        self.index += 1;
                        token.token_type = TokenType.Semicolon;
                    },

                    ',' => {
                        self.index += 1;
                        token.token_type = TokenType.Comma;
                    },

                    else => continue :start .Invalid,
                }
            },

            .EndOfFile => {
                if (self.index == self.source.len) {
                    return null;
                } else {
                    continue :start .Invalid;
                }
            },

            .Identifier => {
                self.index += 1;
                switch (self.source[self.index]) {

                    'a'...'z', 'A'...'Z', '_', '0'...'9' => {
                        continue :start .Identifier;
                    },

                    else => {

                        token.token_type = TokenType.Identifier;

                        const identifier = self.source[token.start..self.index];

                        if (Token.getReserved(identifier)) |tokenType|
                        {
                            token.token_type = tokenType;
                        }
                    }
                }
            },

            .Ampersat => {
                self.index += 1;
                switch (self.source[self.index]) {

                    'a'...'z', 'A'...'Z', '_', '0'...'9' => {
                        continue :start .Ampersat;
                    },

                    else => token.token_type = TokenType.Builtin,
                }
            },

            .StringLiteral => {
                self.index += 1;
                switch (self.source[self.index]) {

                    0 => {
                        continue :start .EndOfFile;
                    },

                    '\"' => {
                        self.index += 1;
                        token.token_type = TokenType.String;
                    },

                    else => continue :start .StringLiteral,
                }
            },

            .CharLiteral => {
                self.index += 1;
                switch (self.source[self.index]) {

                    0 => {
                        continue :start .EndOfFile;
                    },

                    '\'' => {
                        self.index += 1;
                        token.token_type = TokenType.Char;
                    },

                    else => continue :start .CharLiteral,
                }
            },

            .NumberLiteral => {
                
                switch (self.source[self.index + 1]) {
                    'b' => {
                        self.index += 1;
                        continue :start .BinLiteral;
                    },

                    'x' => {
                        self.index += 1;
                        continue :start .HexLiteral;
                    },

                    else => {
                        continue :start .BeforeDotNumber;
                    },
                }
            },

            .BinLiteral => {
                self.index += 1;
                switch (self.source[self.index]) {
                    '0', '1', '_' => {
                        self.index += 1;
                        continue :start .BinLiteral;
                    },

                    else => token.token_type = TokenType.Binary,
                }
            },

            .HexLiteral => {
                self.index += 1;
                switch (self.source[self.index]) {
                    '0'...'9', 'a'...'f', '_' => {
                        self.index += 1;
                        continue :start .HexLiteral;
                    },

                    else => token.token_type = TokenType.Binary
                }
            },

            .BeforeDotNumber => {
                self.index += 1;
                switch (self.source[self.index]) {

                    '.' => {
                        continue :start .AfterDotNumber;
                    },

                    '0'...'9', '_' => {
                        continue :start .BeforeDotNumber;
                    },

                    else => token.token_type = TokenType.Number,
                }
            },

            .AfterDotNumber => {
                self.index += 1;
                switch (self.source[self.index]) {

                    '0'...'9', '_' => {
                        continue :start .AfterDotNumber;
                    },

                    else => token.token_type = TokenType.Number,
                }
            },

            .Dot => {
                self.index += 1;
                switch (self.source[self.index]) {

                    '0'...'9' => {
                        
                        continue :start .AfterDotNumber;
                    },

                    else => token.token_type = TokenType.Dot,
                }
            },

            .Equals => {
                self.index += 1;
                switch (self.source[self.index]) {

                    '=' => {
                        self.index += 1;
                        token.token_type = TokenType.EqualsEquals;
                    },

                    '>' => {
                        self.index += 1;
                        token.token_type = TokenType.FatRightArrow;
                    },

                    else => token.token_type = TokenType.Equals,
                }
            },

            .Plus => {
                self.index += 1;
                switch (self.source[self.index]) {

                    '=' => {
                        self.index += 1;
                        token.token_type = TokenType.PlusEquals;
                    },

                    else => token.token_type = TokenType.Plus,
                }
            },

            .Minus => {
                self.index += 1;
                switch (self.source[self.index]) {

                    '=' => {
                        self.index += 1;
                        token.token_type = TokenType.MinusEquals;
                    },

                    '>' => {
                        self.index += 1;
                        token.token_type = TokenType.RightArrow;
                    },

                    else => token.token_type = TokenType.Minus,
                }
            },

            .Slash => {
                self.index += 1;
                switch (self.source[self.index]) {

                    '=' => {
                        self.index += 1;
                        token.token_type = TokenType.DivEquals;
                    },

                    '/' => {
                        continue :start .Comment;
                    },

                    else => token.token_type = TokenType.Slash,
                }
            },

            .Asterisk => {
                self.index += 1;
                switch (self.source[self.index]) {

                    '=' => {
                        self.index += 1;
                        token.token_type = TokenType.TimesEquals;
                    },

                    else => token.token_type = TokenType.Asterisk,
                }
            },

            .Percentage => {
                self.index += 1;
                switch (self.source[self.index]) {

                    '=' => {
                        self.index += 1;
                        token.token_type = TokenType.PercentEquals;
                    },

                    else => token.token_type = TokenType.Percentage,
                }
            },

            .Exclamation => {
                self.index += 1;
                switch (self.source[self.index]) {

                    '=' => {
                        self.index += 1;
                        token.token_type = TokenType.ExclamationEquals;
                    },

                    else => token.token_type = TokenType.Exclamation,
                }
            },

            .LessThan => {
                self.index += 1;
                switch (self.source[self.index]) {

                    '<' => {
                        self.index += 1;
                        token.token_type = TokenType.ShiftLeft;
                    },

                    '=' => {
                        self.index += 1;
                        token.token_type = TokenType.LessThanOrEquals;
                    },

                    else => token.token_type = TokenType.LessThan,
                }
            },

            .GreaterThan => {
                self.index += 1;
                switch (self.source[self.index]) {

                    '>' => {
                        self.index += 1;
                        token.token_type = TokenType.ShiftRight;
                    },

                    '=' => {
                        self.index += 1;
                        token.token_type = TokenType.GreaterThanOrEquals;
                    },

                    else => token.token_type = TokenType.GreaterThan,
                }
            },

            .Comment => {
                self.index += 1;
                switch (self.source[self.index]) {

                    0 => {
                        continue :start .EndOfFile;
                    },

                    '\n' => {
                        self.index += 1;
                        token.start = self.index;
                        continue :start .Start;
                    },

                    else => continue :start .Comment,
                }
            },

            .Invalid => {
                self.index += 1;
                switch (self.source[self.index]) {

                    0 => continue :start .EndOfFile,

                    '\n' => token.token_type = .Invalid,

                    else => continue :start .Invalid,
                }
            }
        }

        token.end = self.index;
        return token;
    }
};