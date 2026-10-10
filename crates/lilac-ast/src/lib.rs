//! Untyped AST produced by `lilac-parse`.
//!
//! Covers `SPEC.md` surface syntax: functions (multi-return, inline `=>`,
//! generics), `object`/`enum`, `using`, `extern`, `ref` types, and the
//! statement/expression forms (`if` values lower later; `match` arms keep
//! their `=>`-or-block bodies).

use lilac_span::Span;

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
}

/// Declaration bodies.
#[derive(Clone, Debug)]
pub enum ItemKind {
    /// `name: function ...` (or `extern function ...` with no body).
    Func(FuncDecl),
    /// `name: object { ... }`.
    Object {
        /// Member declarations.
        members: Vec<Item>,
        /// Generic parameters (`GenObj[T]`).
        generic_params: Vec<String>,
    },
    /// `name: enum { One, Two }` (payload variants like `Some: @bit32`
    /// make it the single tagged-data type).
    Enum {
        /// Variant names in order.
        variants: Vec<EnumVariant>,
        /// Methods, hooks, and `private` blocks declared in the enum body.
        members: Vec<Item>,
        /// Generic parameters.
        generic_params: Vec<String>,
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
    },
    /// `private { <items> }` visibility wrapper (file or object scope).
    Private {
        /// Wrapped declarations.
        items: Vec<Item>,
    },
    /// `member: @bit8` bare field inside an `object` (no initializer).
    Field {
        /// Declared type.
        ty: Type,
    },
    /// `name: Type = expr` / `name: Type => expr` module-level constant.
    Const {
        /// Declared type.
        ty: Type,
        /// Initializer.
        value: Expr,
        /// `true` for `=>` (inline; folds through substitution).
        is_inline: bool,
    },
    /// An item that failed to parse; its `span` covers the consumed tokens.
    /// Carries no name and defines nothing downstream.
    Error,
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
}

/// `function (params) returns body`: named multi-return slots, bare `return`.
#[derive(Clone, Debug)]
pub struct FuncDecl {
    /// Generic parameters (`[T]` in `GenFunc[T]`).
    pub generic_params: Vec<String>,
    /// Input parameters.
    pub params: Vec<Param>,
    /// Named return slots (empty for `nothing`).
    pub returns: Vec<Param>,
    /// Body (`None` for `extern` declarations).
    pub body: Option<FuncBody>,
    /// Whether this is an `extern` (foreign, C ABI) declaration.
    pub is_extern: bool,
    /// An `extern`'s link name (`extern "write" function …`); `None` links
    /// under the declared name.
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
    /// Expression used for effects (`f()`).
    ExprStmt(Expr),
    /// Bare `return` (returns current named-slot values).
    Return {
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
        /// Optional `-> binding: Type` multi-return capture.
        capture: Option<Box<Param>>,
        /// Live branch.
        then: Block,
        /// Optional `else` statement (`else { }`, `else if ...`, `else break`).
        els: Option<Box<Stmt>>,
        /// Whole-statement span.
        span: Span,
    },
    /// `loop <stmt>`: `loop { ... }`, `loop if cond { ... } else break`.
    Loop {
        /// Repeated statement.
        body: Box<Stmt>,
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

/// A match arm pattern: `.Tag`, `Enum.Tag`, or a byte literal.
#[derive(Clone, Debug)]
pub enum MatchPattern {
    /// `.Some` / `Optional.Some` / `Optional[T].Some` tag (the enum comes
    /// from the scrutinee; a qualifier must name that same enum).
    Tag {
        /// `Enum` / `Enum[T]` before the dot, if written.
        qualifier: Option<Type>,
        /// Variant name.
        name: String,
    },
    /// `0x0` byte pattern (source bytes).
    Const(String),
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
    /// `$"Hello {name}!"` (lowers to `@format_args` construction).
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
    /// `expr as T` (postfix cast; `T` is the operand's expected type).
    Cast(Box<Expr>, Box<Type>),
    /// `Name[args]` in expression position: the receiver of a static member
    /// (`Box[@bit32].make()`, `Optional[T].Some(x)`).
    TypeApp(Box<Type>),
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
    /// Static chunk (borrows a `data` const after lowering).
    Lit(Vec<u8>),
    /// `{expr}` hole (converts via `@conversion(T -> @byte_buffer)`).
    Hole(Expr),
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
            | Self::Member { span, .. }
            | Self::BinOp { span, .. }
            | Self::IfValue { span, .. }
            | Self::MatchValue { span, .. }
            | Self::StructLit { span, .. }
            | Self::RefOf { span, .. }
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
            | Self::Return { span }
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
