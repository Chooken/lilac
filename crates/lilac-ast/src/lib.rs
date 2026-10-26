//! Untyped AST produced by `lilac-parse`.
//!
//! Covers `SPEC.md` surface syntax: functions (multi-return, inline `=>`,
//! generics), `structure`/`either`, `using`, `extern`, `ref` types, and the
//! statement/expression forms (`if` values lower later; `match` arms keep
//! their `=>`-or-block bodies).

use lilac_span::{Doc, Span};

/// A whole file: one namespace unit (folders share namespaces, see `SPEC.md` §6).
#[derive(Clone, Debug)]
pub struct Program {
    /// Top-level items in source order.
    pub items: Vec<Item>,
    /// Whole-file span.
    pub span: Span,
}

/// One named declaration: `name: <kind>`.
#[derive(Clone, Debug)]
pub struct Item {
    /// Declared name.
    pub name: String,
    /// Declaration body.
    pub kind: ItemKind,
    /// Whole-item span.
    pub span: Span,
    /// The doc comment just above it (a declaration only).
    pub doc: Option<Doc>,
}

/// Declaration bodies.
#[derive(Clone, Debug)]
pub enum ItemKind {
    /// `name: function ...` (or `extern function ...` with no body).
    Func(FuncDecl),
    /// `name: structure { ... }`.
    Object {
        /// Member declarations.
        members: Vec<Item>,
        /// Generic parameters (`GenObj[T]`).
        generic_params: Vec<String>,
    },
    /// `name: either { One, Two }` (payload variants like `Some: @bit32`
    /// make it the single tagged-data type).
    Enum {
        /// Variant names in order.
        variants: Vec<EnumVariant>,
        /// Methods, hooks, and `private` blocks declared in the enum body.
        members: Vec<Item>,
        /// Generic parameters.
        generic_params: Vec<String>,
    },
    /// `name: alias Type`: another name for a type (the same type, not a
    /// wrapper).
    Alias {
        /// The type it names (never `ref`).
        ty: Type,
    },
    /// `using Standard` (folder import, non-transitive), or `using *.C`
    /// (relative to the importing file's own namespace).
    Using {
        /// Dotted folder path (without the `*.` marker).
        path: String,
        /// `using *.Path`: resolved against the importing file's namespace.
        relative: bool,
        /// `using Path as Name`: binds the namespace to `Name` (and its span)
        /// instead of importing its names.
        alias: Option<(String, Span)>,
        /// The span of each written segment of `path`, in order.
        segments: Vec<Span>,
    },
    /// `private { <items> }` visibility wrapper (file or object scope).
    Private {
        /// Wrapped declarations.
        items: Vec<Item>,
    },
    /// `member: @bit8` bare field inside a `structure` (no initializer).
    Field {
        /// Declared type.
        ty: Type,
    },
    /// `name: Type => expr`: an inline constant (folds through substitution),
    /// at the top level or in a type's body (`Int.MAX`).
    Const {
        /// Declared type.
        ty: Type,
        /// Initializer.
        value: Expr,
    },
    /// `name: Type = expr` (or `name: Type`, zeroed): a global variable,
    /// initialized with a constant expression.
    Global {
        /// Declared type.
        ty: Type,
        /// Initializer (`None` starts zeroed).
        value: Option<Expr>,
    },
    /// A top-level `if` or `match` over items. Only the arm its constant
    /// condition selects exists: the driver splices it in place before the
    /// program is resolved.
    Conditional(Conditional),
    /// An item that failed to parse; its `span` covers the consumed tokens.
    /// Its `name` is the declared name when the parser read the header
    /// (`name: kind`) before failing, else empty. It defines nothing
    /// downstream; tools read the header (a half-typed `main: function (`
    /// still makes its file a program's `main`).
    Error {
        /// Whether the header declared a function (`function`, `extern`,
        /// `export`).
        function: bool,
    },
}

/// `if a { items } else if b { items } else { items }` or
/// `match s { .A { items } .B => item } else { items }` at the top level.
#[derive(Clone, Debug)]
pub struct Conditional {
    /// The `match` scrutinee (`None` for an `if` chain).
    pub scrutinee: Option<Expr>,
    /// The arms, in order: the first whose test holds is selected.
    pub arms: Vec<ItemArm>,
    /// The items of the trailing `else`, if any.
    pub els: Option<Vec<Item>>,
}

/// One arm of a [`Conditional`].
#[derive(Clone, Debug)]
pub struct ItemArm {
    /// Its condition (`if` chain) or pattern (`match`).
    pub test: ArmTest,
    /// The items it declares.
    pub items: Vec<Item>,
    /// Whole-arm span.
    pub span: Span,
}

/// The test of an [`ItemArm`].
#[derive(Clone, Debug)]
pub enum ArmTest {
    /// `if cond` / `else if cond`.
    If(Expr),
    /// A `match` pattern, with its span.
    Pattern(MatchPattern, Span),
}

impl Conditional {
    /// The spans of what decides the arm: the conditions, or the scrutinee
    /// and the patterns.
    #[must_use]
    pub fn test_spans(&self) -> Vec<Span> {
        let mut spans: Vec<Span> = self.scrutinee.iter().map(Expr::span).collect();
        spans.extend(self.arms.iter().map(|arm| match &arm.test {
            ArmTest::If(cond) => cond.span(),
            ArmTest::Pattern(_, span) => *span,
        }));
        spans
    }
}

/// One enum variant: `One` or `Some: @bit32` (payload).
#[derive(Clone, Debug)]
pub struct EnumVariant {
    /// Variant name.
    pub name: String,
    /// Payload type (`None` for payload-less variants).
    pub payload: Option<Type>,
    /// Whole-variant span (`name` through the payload type, if any).
    pub span: Span,
    /// Span of the variant name token alone, for semantic tokens.
    pub name_span: Span,
    /// The doc comment just above it.
    pub doc: Option<Doc>,
}

/// `function (params) returns body`: named multi-return slots, bare `return`.
#[derive(Clone, Debug)]
pub struct FuncDecl {
    /// Generic parameters (`[T]` in `GenFunc[T]`).
    pub generic_params: Vec<String>,
    /// Input parameters.
    pub params: Vec<Param>,
    /// Named return slots: the success outs (empty for `nothing`).
    pub returns: Vec<Param>,
    /// The failure outs after `else` (empty for `else nothing`); `None`
    /// for a function that cannot fail.
    pub failures: Option<Vec<Param>>,
    /// Body (`None` for `extern` declarations).
    pub body: Option<FuncBody>,
    /// Whether this is an `extern` (foreign, C ABI) declaration.
    pub is_extern: bool,
    /// Whether this is an `export`: a definition C can call, by its link
    /// name, with the C ABI.
    pub is_export: bool,
    /// An `extern` or `export`'s link name (`extern "write" function …`);
    /// `None` links under the declared name.
    pub link_name: Option<String>,
    /// Whether an `extern` ends its parameters with `...` (C varargs).
    pub variadic: bool,
}

/// Function body: block or single-expression inline (`=>`).
#[derive(Clone, Debug)]
pub enum FuncBody {
    /// `{ ... }`.
    Block(Block),
    /// `=> expr` (inline; the only form that folds through `=>` substitution).
    Inline(Expr),
}

/// One `name: Type` slot (parameter or named return).
#[derive(Clone, Debug)]
pub struct Param {
    /// Slot name (`""` for anonymous inline return types).
    pub name: String,
    /// Declared type.
    pub ty: Type,
    /// Whole-slot span.
    pub span: Span,
}

impl Param {
    /// Whether this is the bare `self` receiver (`ref Self`).
    #[must_use]
    pub fn is_ref_self(&self) -> bool {
        self.name.is_empty() && self.ty.is_self()
    }

    /// Whether this is a receiver: the bare `self`, or `self: T`, which
    /// takes the receiver by value.
    #[must_use]
    pub fn is_receiver(&self) -> bool {
        self.is_ref_self() || self.name == "self"
    }
}

/// A type reference, with an optional `ref` flag.
#[derive(Clone, Debug)]
pub struct Type {
    /// What the type is.
    pub shape: TypeShape,
    /// Span of the type's name (`function`, `@array`, `self`, or the
    /// path), excluding a leading `ref` and any generic arguments.
    pub name_span: Span,
    /// `ref T` (a type can never be doubly flagged).
    pub is_ref: bool,
    /// Whole-type span.
    pub span: Span,
}

/// The kinds of type syntax.
#[derive(Clone, Debug)]
pub enum TypeShape {
    /// A named type: `Name`, a path (`A.B.Type`), a builtin (`@bit32`
    /// keeps its `@`), or `unknown`, with any generic arguments
    /// (`GenObj[T]`).
    Named {
        /// The name or dotted path.
        name: String,
        /// Generic arguments.
        args: Vec<Type>,
    },
    /// `function (params) returns`.
    Function(FuncType),
    /// `@array(T, N)`.
    Array {
        /// The element type.
        element: Box<Type>,
        /// The length literal's text.
        len: String,
        /// The length literal's span.
        len_span: Span,
    },
    /// `@format_args(W)`: an f-string formatted into a `W`.
    FormatArgs {
        /// The type its formatters write into.
        writer: Box<Type>,
    },
    /// `self`: the enclosing type.
    SelfType,
    /// A type the parser could not read; it already reported why.
    Error,
}

impl Type {
    /// The name of a named type.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match &self.shape {
            TypeShape::Named { name, .. } => Some(name),
            _ => None,
        }
    }

    /// The generic arguments of a named type (empty for every other shape).
    #[must_use]
    pub fn args(&self) -> &[Type] {
        match &self.shape {
            TypeShape::Named { args, .. } => args,
            _ => &[],
        }
    }

    /// What a diagnostic calls the type: a named type's name, else the
    /// keyword it starts with (`function`, `@array`, `self`).
    #[must_use]
    pub fn head(&self) -> &str {
        match &self.shape {
            TypeShape::Named { name, .. } => name,
            TypeShape::Function(_) => "function",
            TypeShape::Array { .. } => "@array",
            TypeShape::FormatArgs { .. } => "@format_args",
            TypeShape::SelfType => "self",
            TypeShape::Error => "",
        }
    }

    /// Whether this is `self`.
    #[must_use]
    pub fn is_self(&self) -> bool {
        matches!(self.shape, TypeShape::SelfType)
    }

    /// Whether this is the parser's stand-in for a type it could not read.
    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self.shape, TypeShape::Error)
    }
}

/// A `function (params) returns` type in type position.
#[derive(Clone, Debug)]
pub struct FuncType {
    /// Parameter slots (names may be empty for anonymous types).
    pub params: Vec<Param>,
    /// Return slots.
    pub returns: Vec<Param>,
    /// Failure outs after `else`. A function type cannot fail yet, so
    /// typeck rejects one; an enum body's functions are read through this
    /// and keep theirs.
    pub failures: Option<Vec<Param>>,
    /// `export function (…)`: a C function's address, called with the C
    /// ABI.
    pub is_export: bool,
}

/// A `{ ... }` statement sequence.
#[derive(Clone, Debug)]
pub struct Block {
    /// Statements in order.
    pub stmts: Vec<Stmt>,
    /// Whole-block span (braces included).
    pub span: Span,
}

/// Statements.
#[derive(Clone, Debug)]
pub enum Stmt {
    /// `name: Type = expr`.
    Let {
        /// Bound name.
        name: String,
        /// Declared type.
        ty: Box<Type>,
        /// Initializer.
        value: Expr,
        /// Whole-statement span.
        span: Span,
    },
    /// `a: T, b: U = f()` (multi-return destructuring; one `=`).
    MultiLet {
        /// Bound slots in order.
        bindings: Vec<LetBinding>,
        /// Call producing one value per binding.
        value: Expr,
        /// Whole-statement span.
        span: Span,
    },
    /// `target = expr` (fires `@on_override` + `@on_drop` when bound).
    Assign {
        /// Assignment target.
        target: Expr,
        /// New value.
        value: Expr,
        /// Whole-statement span.
        span: Span,
    },
    /// `target op= expr` (`+=`, `-=`, `*=`, `/=`, `%=`): `target = target op
    /// expr`, with the target checked once.
    CompoundAssign {
        /// Assignment target.
        target: Expr,
        /// The operator: `Add`, `Sub`, `Mul`, `Div` or `Mod`.
        op: BinOp,
        /// The `op=` token.
        op_span: Span,
        /// Right operand.
        value: Expr,
        /// Whole-statement span.
        span: Span,
    },
    /// Expression used for effects (`f()`).
    ExprStmt(Expr),
    /// Bare `return` (returns current named-slot values).
    Return {
        /// Keyword span.
        span: Span,
    },
    /// Bare `fail`: leaves a function that can fail through its failure
    /// outs.
    Fail {
        /// Keyword span.
        span: Span,
    },
    /// `break` (loop exit).
    Break {
        /// Keyword span.
        span: Span,
    },
    /// `continue` (next loop iteration).
    Continue {
        /// Keyword span.
        span: Span,
    },
    /// `if cond { ... } else <stmt>`.
    If {
        /// Condition (strict non-zero `@bit8` test).
        cond: Expr,
        /// `-> a: A, b: B`: the success outs of a call that can fail, by
        /// position.
        captures: Vec<Param>,
        /// Live branch.
        then: Block,
        /// `else -> e: E`: the failure outs, by position (its `els` is then a
        /// block).
        else_captures: Vec<Param>,
        /// Optional `else` statement (`else { }`, `else if ...`, `else break`).
        els: Option<Box<Stmt>>,
        /// Whole-statement span.
        span: Span,
    },
    /// `loop <stmt>`: `loop { ... }`, `loop if cond { ... }` (which typeck
    /// ends with `else break` when the chain has no `else`).
    Loop {
        /// Repeated statement.
        body: Box<Stmt>,
        /// Whole-statement span.
        span: Span,
    },
    /// `a: A, b: B = f() else -> e: E { ... }`: a call that can fail whose
    /// success outs are bound (by position) in the current scope; the
    /// `else` block, which must leave, sees the failure outs. With no
    /// bindings it is the statement `f() else { ... }`.
    Guard {
        /// Success-out bindings (none for the statement form).
        bindings: Vec<LetBinding>,
        /// The call.
        value: Expr,
        /// `-> e: E` failure-out bindings.
        captures: Vec<Param>,
        /// The `else` block.
        els: Block,
        /// Whole-statement span.
        span: Span,
    },
    /// A nested `{ ... }` block statement.
    Block(Block),
    /// `match scrutinee { arms } else { ... }` (non-exhaustive by design).
    Match {
        /// Inspected value.
        scrutinee: Expr,
        /// `.Tag -> binding { ... }` / `.Tag => stmt` arms.
        arms: Vec<MatchArm>,
        /// Optional trailing `else`.
        els: Option<Block>,
        /// Whole-statement span.
        span: Span,
    },
    /// A statement that failed to parse; its `span` covers the consumed tokens.
    /// Downstream passes skip it.
    Error {
        /// Span of the consumed tokens.
        span: Span,
    },
}

/// One `name: Type` slot of a multi-`let`.
#[derive(Clone, Debug)]
pub struct LetBinding {
    /// Bound name.
    pub name: String,
    /// Declared type.
    pub ty: Type,
    /// Whole-slot span.
    pub span: Span,
}

/// One match arm: `.Some -> value: T { ... }` or `.None => return`.
#[derive(Clone, Debug)]
pub struct MatchArm {
    /// Tag or byte pattern after the dot.
    pub pattern: MatchPattern,
    /// Optional `-> binding: Type` capture.
    pub capture: Option<Param>,
    /// Arm body.
    pub body: MatchArmBody,
    /// Whole-arm span.
    pub span: Span,
}

/// A match arm pattern: `.Tag`, `Enum.Tag`, or a literal.
#[derive(Clone, Debug)]
pub enum MatchPattern {
    /// `.Some` / `Optional.Some` / `Optional[T].Some` tag (the enum comes
    /// from the scrutinee; a qualifier must name that same enum).
    Tag {
        /// `Enum` / `Enum[T]` before the dot, if written.
        qualifier: Option<Type>,
        /// Variant name (empty for a `.` with no name after it, mid-edit).
        name: String,
        /// The tag: its `.` through the name, or the qualifier through the
        /// name.
        span: Span,
    },
    /// `0x0` / `'a'`: a literal the scrutinee's bits are compared with.
    Literal {
        /// The literal as written.
        literal: PatternLiteral,
        /// The literal's span.
        span: Span,
    },
}

/// The literal of a literal match pattern. The parser takes any literal
/// token; typeck accepts integers and characters.
#[derive(Clone, Debug)]
pub enum PatternLiteral {
    /// A number's source text (`0x0`, `42`, `1.5`).
    Number(String),
    /// A character's scalar value.
    Char(char),
    /// A string's bytes.
    Str(Vec<u8>),
}

/// Match arm bodies: block or inline `=>` single statement.
#[derive(Clone, Debug)]
pub enum MatchArmBody {
    /// `{ ... }`.
    Block(Block),
    /// `=> stmt` (e.g. `.None => return`).
    Inline(Box<Stmt>),
}

/// Expressions.
#[derive(Clone, Debug)]
pub enum Expr {
    /// `foo`, `T.get` base, hole contents.
    Ident(String, Span),
    /// `.Some` tag literal (enum tag; type comes from context).
    Tag(String, Span),
    /// `5`, `0xFF` (source bytes; meaning comes from use).
    Number(String, Span),
    /// `"hi"` (decoded bytes; `\xHH` is one raw byte).
    Str(Vec<u8>, Span),
    /// `'A'` (Unicode scalar).
    Char(char, Span),
    /// `f(args)`, `gen_function[@bit32](0x1)`.
    Call {
        /// Function expression.
        callee: Box<Expr>,
        /// Explicit generic arguments (`f[@bit32]`).
        generic_args: Vec<Type>,
        /// Call arguments.
        args: Vec<Expr>,
        /// Whole-call span.
        span: Span,
    },
    /// `obj.field`, `instance.Method` (desugars to `Object.Method(ref instance)`).
    Member {
        /// Receiver.
        obj: Box<Expr>,
        /// Field name.
        field: String,
        /// Whole-access span.
        span: Span,
    },
    /// Desugared operator call (`a + b` is `@add(a, b)`).
    BinOp {
        /// Left operand (conversion anchor).
        lhs: Box<Expr>,
        /// Operator.
        op: BinOp,
        /// Right operand.
        rhs: Box<Expr>,
        /// Whole-expression span.
        span: Span,
    },
    /// `if cond then x else y` used as a value.
    IfValue {
        /// Condition.
        cond: Box<Expr>,
        /// Then value.
        then: Box<Expr>,
        /// Else value.
        els: Box<Expr>,
        /// Whole-expression span.
        span: Span,
    },
    /// `match s { arms } else d` used as a value (`else` required).
    MatchValue {
        /// Inspected value.
        scrutinee: Box<Expr>,
        /// Arms (same shapes as statement `match`).
        arms: Vec<MatchArm>,
        /// Required `else` value.
        els: Box<Expr>,
        /// Whole-expression span.
        span: Span,
    },
    /// `Object { member = expr, ... }` / `Name[T] { ... }` (`=` form only).
    StructLit {
        /// Type before the brace (generic arguments are optional and inferred
        /// from the expected type when omitted).
        ty: Box<Type>,
        /// Field initializers in order.
        fields: Vec<StructField>,
        /// Whole-literal span.
        span: Span,
    },
    /// `$"Hello {name}!"`: an `@format_args(W)` of its pieces and holes.
    FString {
        /// Literal chunks and holes in order.
        parts: Vec<FStrPart>,
        /// Whole-literal span.
        span: Span,
    },
    /// `ref variable` (takes the address of a place).
    RefOf {
        /// Place expression.
        value: Box<Expr>,
        /// Whole-expression span (`ref` through value).
        span: Span,
    },
    /// `not condition` (the `@bit8` negation, no hook; binds like a prefix `-`).
    Not {
        /// The condition.
        value: Box<Expr>,
        /// Whole-expression span (`not` through value).
        span: Span,
    },
    /// `expr as T` (postfix cast; `T` is the operand's expected type).
    Cast(Box<Expr>, Box<Type>),
    /// `Name[args]` in expression position: the receiver of a static member
    /// (`Box[@bit32].make()`, `Optional[T].Some(x)`).
    TypeApp(Box<Type>),
    /// `callee[args]` with no call after it, where `callee` is no plain path
    /// (`Box[@bit8].convert[@bit32]`): an instance of a generic function, a
    /// value behind `ref`. A plain path's (`id[@bit32]`) is a `TypeApp`.
    Instance {
        /// The function: a member chain.
        callee: Box<Expr>,
        /// The function's own generic arguments.
        generic_args: Vec<Type>,
        /// Whole-expression span (callee through `]`).
        span: Span,
    },
    /// The initializer of the declaration shorthand `name: Type.function(args)`
    /// (`name: Type = Type.function(args)`): the declared type is the
    /// receiver, and `function` must be a static function of it returning
    /// exactly one value of it.
    Construct {
        /// The static function's name.
        function: String,
        /// `Type.function`: the function's name is its tail.
        member_span: Span,
        /// Explicit generic arguments of the function (`Type.make[@bit32]`).
        generic_args: Vec<Type>,
        /// Call arguments.
        args: Vec<Expr>,
        /// `Type.function(args)`.
        span: Span,
    },
    /// An expression that failed to parse; its `span` covers the consumed
    /// tokens. Downstream passes map it to a poison value.
    Error {
        /// Span of the consumed tokens.
        span: Span,
    },
}

/// Binary operators and their `@` hook names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    /// `+` is `@add`.
    Add,
    /// `-` is `@sub`.
    Sub,
    /// `*` is `@mul`.
    Mul,
    /// `/` is `@div`.
    Div,
    /// `%` is `@mod`.
    Mod,
    /// `<` is `@less_than`.
    Lt,
    /// `>` is `@greater_than`.
    Gt,
    /// `<=` is `@less_than_or_equal`.
    LtEq,
    /// `>=` is `@greater_than_or_equal`.
    GtEq,
    /// `==` (strict bit-equality, never converted or overridden).
    EqEq,
    /// `!=` (strict bit-inequality, never converted or overridden).
    NotEq,
    /// `and` (short-circuit conjunction over `@bit8`, no hook).
    And,
    /// `or` (short-circuit disjunction over `@bit8`, no hook).
    Or,
}

/// One struct-literal field: `member = expr`.
#[derive(Clone, Debug)]
pub struct StructField {
    /// Field name.
    pub name: String,
    /// Field value.
    pub value: Expr,
    /// Whole-field span.
    pub span: Span,
}

/// One f-string piece: literal bytes or an interpolated hole.
#[derive(Clone, Debug)]
pub enum FStrPart {
    /// Static text (a data constant after lowering).
    Lit {
        /// The text, escapes resolved.
        bytes: Vec<u8>,
        /// Where it is written.
        span: Span,
    },
    /// `{expr}` hole: borrowed, and formatted by its type's `@format`.
    Hole {
        /// The value.
        expr: Expr,
        /// The hole from its `{` to its `}`.
        span: Span,
    },
}

impl Expr {
    /// Span of any expression node.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Ident(_, span)
            | Self::Tag(_, span)
            | Self::Number(_, span)
            | Self::Str(_, span)
            | Self::Char(_, span)
            | Self::Call { span, .. }
            | Self::Construct { span, .. }
            | Self::Member { span, .. }
            | Self::Instance { span, .. }
            | Self::BinOp { span, .. }
            | Self::IfValue { span, .. }
            | Self::MatchValue { span, .. }
            | Self::StructLit { span, .. }
            | Self::RefOf { span, .. }
            | Self::Not { span, .. }
            | Self::FString { span, .. }
            | Self::Error { span } => *span,
            Self::TypeApp(ty) => ty.span,
            Self::Cast(operand, ty) => {
                let start = operand.span();
                Span::new(start.lo.0, ty.span.hi.0, start.file)
            }
        }
    }
}

impl Stmt {
    /// Span of any statement node.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::ExprStmt(expr) => expr.span(),
            Self::Let { span, .. }
            | Self::MultiLet { span, .. }
            | Self::Assign { span, .. }
            | Self::CompoundAssign { span, .. }
            | Self::Guard { span, .. }
            | Self::Return { span }
            | Self::Fail { span }
            | Self::Break { span }
            | Self::Continue { span }
            | Self::If { span, .. }
            | Self::Loop { span, .. }
            | Self::Match { span, .. }
            | Self::Error { span } => *span,
            Self::Block(block) => block.span,
        }
    }
}
