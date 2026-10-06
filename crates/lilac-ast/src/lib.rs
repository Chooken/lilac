//! Untyped AST plus a [`Visitor`] so new syntax touches one trait.
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
        /// Generic parameters.
        generic_params: Vec<String>,
    },
    /// `using Standard` (folder import, non-transitive).
    Using {
        /// Dotted folder path.
        path: String,
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
}

/// One enum variant: `One` or `Some: @bit32` (payload).
#[derive(Clone, Debug)]
pub struct EnumVariant {
    /// Variant name.
    pub name: String,
    /// Payload type (`None` for payload-less variants).
    pub payload: Option<Type>,
    /// Name span.
    pub span: Span,
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

/// A type reference: `[@]name[args]` or `function` type, with an optional `ref` flag.
#[derive(Clone, Debug)]
pub struct Type {
    /// Type name (`@bit32` keeps its `@`; `ref` is in [`Type::is_ref`];
    /// `"function"` when [`Type::func`] is `Some`).
    pub name: String,
    /// `ref T` (a type can never be doubly flagged).
    pub is_ref: bool,
    /// Generic arguments (`GenObj[T]` instantiation).
    pub generic_args: Vec<Type>,
    /// Function type (`function (params) returns`); `None` for named types.
    pub func: Option<FuncType>,
    /// Whole-type span.
    pub span: Span,
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
        ty: Type,
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
    /// `if cond { ... } else { ... }`.
    If {
        /// Condition (strict non-zero `@bit8` test).
        cond: Expr,
        /// Optional `-> binding: Type` multi-return capture.
        capture: Option<Param>,
        /// Live branch.
        then: Block,
        /// Optional `else` branch.
        els: Option<Block>,
        /// Whole-statement span.
        span: Span,
    },
    /// `loop { ... }` / `loop if cond { ... } else break`.
    Loop {
        /// Optional condition (`None` loops forever).
        cond: Option<Expr>,
        /// Loop body.
        body: Block,
        /// Optional trailing `else` (runs on condition failure).
        els: Option<Block>,
        /// Whole-statement span.
        span: Span,
    },
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

/// A match arm pattern: `.Tag` or a byte literal.
#[derive(Clone, Debug)]
pub enum MatchPattern {
    /// `.Some` tag (enum tag; type comes from the scrutinee).
    Tag(String),
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
    /// `"hi"` (decoded value).
    Str(String, Span),
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
    /// `if cond { ... } else { ... }` used as a value.
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
    /// `Object { member = expr, ... }` (`=` form only).
    StructLit {
        /// Type name before the brace.
        ty_name: String,
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

/// One f-string piece: literal text or an interpolated hole.
#[derive(Clone, Debug)]
pub enum FStrPart {
    /// Static chunk (borrows a `data` const after lowering).
    Lit(String),
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
            | Self::FString { span, .. } => *span,
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
            | Self::If { span, .. }
            | Self::Loop { span, .. }
            | Self::Match { span, .. } => *span,
        }
    }
}

/// AST visitor: override only what a pass needs; defaults walk children.
pub trait Visitor {
    /// Visits a program root.
    fn visit_program(&mut self, program: &Program) {
        walk_program(self, program);
    }
    /// Visits one item.
    fn visit_item(&mut self, item: &Item) {
        walk_item(self, item);
    }
    /// Visits one statement.
    fn visit_stmt(&mut self, stmt: &Stmt) {
        walk_stmt(self, stmt);
    }
    /// Visits one expression.
    fn visit_expr(&mut self, expr: &Expr) {
        walk_expr(self, expr);
    }
}

/// Default program walk for [`Visitor`].
pub fn walk_program<V: Visitor + ?Sized>(visitor: &mut V, program: &Program) {
    for item in &program.items {
        visitor.visit_item(item);
    }
}

/// Default item walk for [`Visitor`].
pub fn walk_item<V: Visitor + ?Sized>(visitor: &mut V, item: &Item) {
    match &item.kind {
        ItemKind::Func(decl) => {
            if let Some(FuncBody::Block(block)) = &decl.body {
                for stmt in &block.stmts {
                    visitor.visit_stmt(stmt);
                }
            } else if let Some(FuncBody::Inline(expr)) = &decl.body {
                visitor.visit_expr(expr);
            }
        }
        ItemKind::Object { members, .. } | ItemKind::Private { items: members } => {
            for member in members {
                visitor.visit_item(member);
            }
        }
        ItemKind::Enum { .. } | ItemKind::Using { .. } | ItemKind::Field { .. } => {}
        ItemKind::Const { value, .. } => visitor.visit_expr(value),
    }
}

/// Default statement walk for [`Visitor`].
pub fn walk_stmt<V: Visitor + ?Sized>(visitor: &mut V, stmt: &Stmt) {
    match stmt {
        Stmt::Let { value, .. } | Stmt::MultiLet { value, .. } => visitor.visit_expr(value),
        Stmt::Assign { target, value, .. } => {
            visitor.visit_expr(target);
            visitor.visit_expr(value);
        }
        Stmt::ExprStmt(expr) => visitor.visit_expr(expr),
        Stmt::Return { .. } | Stmt::Break { .. } => {}
        Stmt::If {
            cond, then, els, ..
        } => {
            visitor.visit_expr(cond);
            for stmt in &then.stmts {
                visitor.visit_stmt(stmt);
            }
            if let Some(els) = els {
                for stmt in &els.stmts {
                    visitor.visit_stmt(stmt);
                }
            }
        }
        Stmt::Loop {
            cond, body, els, ..
        } => {
            if let Some(cond) = cond {
                visitor.visit_expr(cond);
            }
            for stmt in &body.stmts {
                visitor.visit_stmt(stmt);
            }
            if let Some(els) = els {
                for stmt in &els.stmts {
                    visitor.visit_stmt(stmt);
                }
            }
        }
        Stmt::Match {
            scrutinee,
            arms,
            els,
            ..
        } => {
            visitor.visit_expr(scrutinee);
            for arm in arms {
                match &arm.body {
                    MatchArmBody::Block(block) => {
                        for stmt in &block.stmts {
                            visitor.visit_stmt(stmt);
                        }
                    }
                    MatchArmBody::Inline(stmt) => visitor.visit_stmt(stmt),
                }
            }
            if let Some(els) = els {
                for stmt in &els.stmts {
                    visitor.visit_stmt(stmt);
                }
            }
        }
    }
}

/// Default expression walk for [`Visitor`].
pub fn walk_expr<V: Visitor + ?Sized>(visitor: &mut V, expr: &Expr) {
    match expr {
        Expr::Ident(..) | Expr::Tag(..) | Expr::Number(..) | Expr::Str(..) | Expr::Char(..) => {}
        Expr::Call { callee, args, .. } => {
            visitor.visit_expr(callee);
            for arg in args {
                visitor.visit_expr(arg);
            }
        }
        Expr::Member { obj, .. } => visitor.visit_expr(obj),
        Expr::BinOp { lhs, rhs, .. } => {
            visitor.visit_expr(lhs);
            visitor.visit_expr(rhs);
        }
        Expr::IfValue {
            cond, then, els, ..
        } => {
            visitor.visit_expr(cond);
            visitor.visit_expr(then);
            visitor.visit_expr(els);
        }
        Expr::MatchValue {
            scrutinee,
            arms,
            els,
            ..
        } => {
            visitor.visit_expr(scrutinee);
            for arm in arms {
                match &arm.body {
                    MatchArmBody::Block(block) => {
                        for stmt in &block.stmts {
                            visitor.visit_stmt(stmt);
                        }
                    }
                    MatchArmBody::Inline(stmt) => visitor.visit_stmt(stmt),
                }
            }
            visitor.visit_expr(els);
        }
        Expr::StructLit { fields, .. } => {
            for field in fields {
                visitor.visit_expr(&field.value);
            }
        }
        Expr::FString { parts, .. } => {
            for part in parts {
                if let FStrPart::Hole(expr) = part {
                    visitor.visit_expr(expr);
                }
            }
        }
        Expr::RefOf { value, .. } => visitor.visit_expr(value),
    }
}

/// Mutable AST visitor: override only what a pass needs; defaults walk children.
pub trait MutVisitor {
    /// Visits a program root.
    fn visit_program_mut(&mut self, program: &mut Program) {
        walk_program_mut(self, program);
    }
    /// Visits one item.
    fn visit_item_mut(&mut self, item: &mut Item) {
        walk_item_mut(self, item);
    }
    /// Visits one statement.
    fn visit_stmt_mut(&mut self, stmt: &mut Stmt) {
        walk_stmt_mut(self, stmt);
    }
    /// Visits one expression.
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        walk_expr_mut(self, expr);
    }
}

/// Default mutable program walk for [`MutVisitor`].
pub fn walk_program_mut<V: MutVisitor + ?Sized>(visitor: &mut V, program: &mut Program) {
    for item in &mut program.items {
        visitor.visit_item_mut(item);
    }
}

/// Default mutable item walk for [`MutVisitor`].
pub fn walk_item_mut<V: MutVisitor + ?Sized>(visitor: &mut V, item: &mut Item) {
    match &mut item.kind {
        ItemKind::Func(decl) => {
            if let Some(FuncBody::Block(block)) = &mut decl.body {
                for stmt in &mut block.stmts {
                    visitor.visit_stmt_mut(stmt);
                }
            } else if let Some(FuncBody::Inline(expr)) = &mut decl.body {
                visitor.visit_expr_mut(expr);
            }
        }
        ItemKind::Object { members, .. } | ItemKind::Private { items: members } => {
            for member in members {
                visitor.visit_item_mut(member);
            }
        }
        ItemKind::Enum { .. } | ItemKind::Using { .. } | ItemKind::Field { .. } => {}
        ItemKind::Const { value, .. } => visitor.visit_expr_mut(value),
    }
}

/// Default mutable statement walk for [`MutVisitor`].
pub fn walk_stmt_mut<V: MutVisitor + ?Sized>(visitor: &mut V, stmt: &mut Stmt) {
    match stmt {
        Stmt::Let { value, .. } | Stmt::MultiLet { value, .. } => visitor.visit_expr_mut(value),
        Stmt::Assign { target, value, .. } => {
            visitor.visit_expr_mut(target);
            visitor.visit_expr_mut(value);
        }
        Stmt::ExprStmt(expr) => visitor.visit_expr_mut(expr),
        Stmt::Return { .. } | Stmt::Break { .. } => {}
        Stmt::If {
            cond, then, els, ..
        } => {
            visitor.visit_expr_mut(cond);
            for stmt in &mut then.stmts {
                visitor.visit_stmt_mut(stmt);
            }
            if let Some(els) = els {
                for stmt in &mut els.stmts {
                    visitor.visit_stmt_mut(stmt);
                }
            }
        }
        Stmt::Loop {
            cond, body, els, ..
        } => {
            if let Some(cond) = cond {
                visitor.visit_expr_mut(cond);
            }
            for stmt in &mut body.stmts {
                visitor.visit_stmt_mut(stmt);
            }
            if let Some(els) = els {
                for stmt in &mut els.stmts {
                    visitor.visit_stmt_mut(stmt);
                }
            }
        }
        Stmt::Match {
            scrutinee,
            arms,
            els,
            ..
        } => {
            visitor.visit_expr_mut(scrutinee);
            for arm in arms {
                match &mut arm.body {
                    MatchArmBody::Block(block) => {
                        for stmt in &mut block.stmts {
                            visitor.visit_stmt_mut(stmt);
                        }
                    }
                    MatchArmBody::Inline(stmt) => visitor.visit_stmt_mut(stmt),
                }
            }
            if let Some(els) = els {
                for stmt in &mut els.stmts {
                    visitor.visit_stmt_mut(stmt);
                }
            }
        }
    }
}

/// Default mutable expression walk for [`MutVisitor`].
pub fn walk_expr_mut<V: MutVisitor + ?Sized>(visitor: &mut V, expr: &mut Expr) {
    match expr {
        Expr::Ident(..) | Expr::Tag(..) | Expr::Number(..) | Expr::Str(..) | Expr::Char(..) => {}
        Expr::Call { callee, args, .. } => {
            visitor.visit_expr_mut(callee);
            for arg in args {
                visitor.visit_expr_mut(arg);
            }
        }
        Expr::Member { obj, .. } => visitor.visit_expr_mut(obj),
        Expr::BinOp { lhs, rhs, .. } => {
            visitor.visit_expr_mut(lhs);
            visitor.visit_expr_mut(rhs);
        }
        Expr::IfValue {
            cond, then, els, ..
        } => {
            visitor.visit_expr_mut(cond);
            visitor.visit_expr_mut(then);
            visitor.visit_expr_mut(els);
        }
        Expr::MatchValue {
            scrutinee,
            arms,
            els,
            ..
        } => {
            visitor.visit_expr_mut(scrutinee);
            for arm in arms {
                match &mut arm.body {
                    MatchArmBody::Block(block) => {
                        for stmt in &mut block.stmts {
                            visitor.visit_stmt_mut(stmt);
                        }
                    }
                    MatchArmBody::Inline(stmt) => visitor.visit_stmt_mut(stmt),
                }
            }
            visitor.visit_expr_mut(els);
        }
        Expr::StructLit { fields, .. } => {
            for field in fields {
                visitor.visit_expr_mut(&mut field.value);
            }
        }
        Expr::FString { parts, .. } => {
            for part in parts {
                if let FStrPart::Hole(expr) = part {
                    visitor.visit_expr_mut(expr);
                }
            }
        }
        Expr::RefOf { value, .. } => visitor.visit_expr_mut(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_span::Span;

    struct Counter {
        exprs: usize,
    }

    impl Visitor for Counter {
        fn visit_expr(&mut self, expr: &Expr) {
            self.exprs += 1;
            walk_expr(self, expr);
        }
    }

    #[test]
    fn visitor_walks_nested_exprs() {
        let span = Span::dummy();
        let expr = Expr::BinOp {
            lhs: Box::new(Expr::Number("0x1".to_owned(), span)),
            op: BinOp::Add,
            rhs: Box::new(Expr::Number("0x2".to_owned(), span)),
            span,
        };
        let mut counter = Counter { exprs: 0 };
        counter.visit_expr(&expr);
        assert_eq!(counter.exprs, 3);
    }

    struct NumberRename;
    impl MutVisitor for NumberRename {
        fn visit_expr_mut(&mut self, expr: &mut Expr) {
            walk_expr_mut(self, expr);
            if let Expr::Number(text, _) = expr {
                if text == "0x1" {
                    *text = "0x9".to_owned();
                }
            }
        }
    }

    #[test]
    fn mut_visitor_rewrites_nested_exprs() {
        let span = Span::dummy();
        let mut expr = Expr::BinOp {
            lhs: Box::new(Expr::Number("0x1".to_owned(), span)),
            op: BinOp::Add,
            rhs: Box::new(Expr::Number("0x2".to_owned(), span)),
            span,
        };
        NumberRename.visit_expr_mut(&mut expr);
        let Expr::BinOp { lhs, .. } = expr else {
            panic!("still a binop")
        };
        assert!(matches!(*lhs, Expr::Number(ref t, _) if t == "0x9"));
    }
}
