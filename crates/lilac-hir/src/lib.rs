//! Typed HIR: checked functions with desugared control flow.
//!
//! Every expression carries its `TypeId` and source `Span`. `If` and `Match`
//! with `out: Some(t)` end each branch by assigning the value temp; lowering
//! turns them into labeled blocks mechanically.

use std::collections::HashMap;

use lilac_resolve::DefId;
use lilac_span::Span;
use lilac_ty::TypeRef;

/// Typed program: checked functions.
#[derive(Clone, Debug, Default)]
pub struct HirProgram {
    /// Functions in source order.
    pub funcs: Vec<HirFunc>,
    /// Module constants: name, identity, and checked value (inlined at use).
    pub consts: Vec<HirConst>,
    /// Type layouts: display name → `(member, payload)` in declaration
    /// order (fields carry `Some`, payload-less variants `None`).
    pub layouts: HashMap<String, Vec<(String, Option<TypeRef>)>>,
    /// Drop hooks: display name → mangled `@on_drop` callee.
    pub drops: HashMap<String, String>,
}

/// One checked module constant (`name: Type = expr` or `=> expr`).
#[derive(Clone, Debug)]
pub struct HirConst {
    /// Constant name.
    pub name: String,
    /// Stable identity.
    pub def: DefId,
    /// Checked value (inlined wherever the constant is used).
    pub value: HirExpr,
}

/// One checked function body.
#[derive(Clone, Debug)]
pub struct HirFunc {
    /// Stable identity.
    pub def: DefId,
    /// Function name.
    pub name: String,
    /// Input parameters.
    pub params: Vec<HirParam>,
    /// Named return slots.
    pub returns: Vec<HirParam>,
    /// Whether this is an `extern` (foreign, C ABI) declaration with no body.
    pub is_extern: bool,
    /// Checked body.
    pub body: HirBlock,
    /// Whole-item span.
    pub span: Span,
}

/// One `name: Type` slot (parameter, return, or capture).
#[derive(Clone, Debug)]
pub struct HirParam {
    /// Slot name.
    pub name: String,
    /// Checked type.
    pub ty: TypeRef,
    /// Whole-slot span.
    pub span: Span,
}

/// A checked statement sequence.
#[derive(Clone, Debug)]
pub struct HirBlock {
    /// Statements in order.
    pub stmts: Vec<HirStmt>,
    /// Whole-block span.
    pub span: Span,
}

/// Fresh value temporary for desugared control flow.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TempId(pub u32);

/// Checked statements. Control flow is desugared at build time: `If` and
/// `Match` with `out: Some(t)` end each branch by assigning the value temp.
#[derive(Clone, Debug)]
pub enum HirStmt {
    /// `name: Type = expr`.
    Let {
        /// Bound name.
        name: String,
        /// Checked type.
        ty: TypeRef,
        /// Initializer.
        value: HirExpr,
        /// Whole-statement span.
        span: Span,
    },
    /// `a: T, b: U = f()` (value is always a call).
    MultiLet {
        /// Bound slots in order.
        bindings: Vec<HirParam>,
        /// Call producing one value per binding.
        value: HirExpr,
        /// Whole-statement span.
        span: Span,
    },
    /// `target = expr` (targets include `Temp` for desugared joins).
    Assign {
        /// Assignment target.
        target: HirExpr,
        /// New value.
        value: HirExpr,
        /// Whole-statement span.
        span: Span,
    },
    /// Expression used for effects.
    ExprStmt(HirExpr),
    /// Bare `return`.
    Return {
        /// Keyword span.
        span: Span,
    },
    /// `break` (loop exit).
    Break {
        /// Keyword span.
        span: Span,
    },
    /// `if` statement or desugared `if` value.
    If {
        /// Condition (`@bitN`, tested non-zero).
        cond: HirExpr,
        /// Live branch.
        then: HirBlock,
        /// `else` branch (always present in HIR; empty when absent in source).
        els: HirBlock,
        /// Value temp when this `if` produces a value.
        out: Option<TempId>,
        /// Whole-statement span.
        span: Span,
    },
    /// `loop { ... }` (condition loops desugar to `If` + `Break` inside).
    Loop {
        /// Loop body.
        body: HirBlock,
        /// Whole-statement span.
        span: Span,
    },
    /// `match` statement or desugared `match` value.
    Match {
        /// Inspected value.
        scrutinee: HirExpr,
        /// Arms in order.
        arms: Vec<HirArm>,
        /// `else` branch (explicit block, or `unreachable` when absent).
        els: HirBlock,
        /// Value temp when this `match` produces a value.
        out: Option<TempId>,
        /// Whole-statement span.
        span: Span,
    },
}

/// One checked expression: type + span + shape.
#[derive(Clone, Debug)]
pub struct HirExpr {
    /// Checked type (with its `ref` flag).
    pub ty: TypeRef,
    /// Source span.
    pub span: Span,
    /// Expression shape.
    pub kind: HirExprKind,
}

/// Checked expression shapes (typed mirrors of `ast::Expr`, desugared).
#[derive(Clone, Debug)]
pub enum HirExprKind {
    /// Resolved name.
    Ident {
        /// Source name.
        name: String,
        /// What it resolved to.
        def: DefId,
    },
    /// `.Some` tag literal (checked against the expected enum).
    Tag {
        /// Tag name.
        name: String,
    },
    /// Enum variant construction (`.Some(0x1)`).
    Variant {
        /// Owning enum definition.
        def: DefId,
        /// Variant name.
        variant: String,
        /// Payload value (`None` for payload-less variants).
        payload: Option<Box<HirExpr>>,
    },
    /// Number literal (source bytes; width comes from checking).
    Number {
        /// Source bytes.
        bytes: String,
    },
    /// String literal (decoded value).
    Str {
        /// Decoded value.
        value: String,
    },
    /// Char literal (Unicode scalar).
    Char {
        /// Scalar value.
        value: char,
    },
    /// Checked call (overload selected at build).
    Call {
        /// Resolved callee.
        callee: HirCallee,
        /// Checked arguments.
        args: Vec<HirExpr>,
    },
    /// `==` bit-equality (primitive: no hook to desugar to).
    EqEq {
        /// Left operand.
        lhs: Box<HirExpr>,
        /// Right operand.
        rhs: Box<HirExpr>,
    },
    /// Field read on an object value.
    Member {
        /// Receiver.
        obj: Box<HirExpr>,
        /// Field name.
        field: String,
    },
    /// `ref place`.
    RefOf {
        /// Place expression.
        value: Box<HirExpr>,
    },
    /// Checked struct literal (direct field init; never calls `@init`).
    StructLit {
        /// Checked fields in order.
        fields: Vec<HirField>,
    },
    /// Branch-join value temporary.
    Temp(TempId),
    /// Recorded `@compile_error` (folding decides liveness; never emitted here).
    CompileError {
        /// Error message.
        msg: String,
        /// Message literal span.
        msg_span: Span,
        /// Call-site span.
        call_span: Span,
    },
    /// Error-recovery poison (checking continues around it).
    Poison,
}

/// A resolved call target.
#[derive(Clone, Debug)]
pub enum HirCallee {
    /// User function by identity.
    Item(DefId),
    /// Function-typed local (indirect call through the variable).
    Local {
        /// Local name holding the function value.
        name: String,
    },
    /// Hook overload by name and identity.
    Hook {
        /// Hook name (`@add`, `@conversion`, …).
        name: String,
        /// Selected overload.
        def: DefId,
    },
    /// Compiler intrinsic by `@`-name.
    Intrinsic {
        /// Intrinsic name (`@i_add`, `@compile_error`, …).
        name: String,
    },
}

/// One checked struct-literal field.
#[derive(Clone, Debug)]
pub struct HirField {
    /// Field name.
    pub name: String,
    /// Checked value.
    pub value: HirExpr,
    /// Whole-field span.
    pub span: Span,
}

/// One checked match arm.
#[derive(Clone, Debug)]
pub struct HirArm {
    /// Tag or byte pattern.
    pub pattern: HirPattern,
    /// Optional `-> binding: Type` capture.
    pub capture: Option<HirParam>,
    /// Arm body.
    pub body: HirBlock,
}

/// A checked match pattern.
#[derive(Clone, Debug)]
pub enum HirPattern {
    /// `.Some` tag.
    Tag {
        /// Tag name.
        name: String,
    },
    /// `0x0` byte pattern.
    Const {
        /// Source bytes.
        bytes: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_ty::TypeId;

    fn temp_assign(temp: TempId, value: HirExpr) -> HirStmt {
        HirStmt::Assign {
            target: HirExpr {
                ty: TypeRef {
                    id: TypeId(0),
                    is_ref: false,
                },
                span: Span::dummy(),
                kind: HirExprKind::Temp(temp),
            },
            value,
            span: Span::dummy(),
        }
    }

    #[test]
    fn hir_nodes_carry_types_and_spans() {
        let bit8 = TypeRef {
            id: TypeId(0),
            is_ref: false,
        };
        let temp = TempId(0);
        let cond = HirExpr {
            ty: TypeRef {
                id: TypeId(0),
                is_ref: false,
            },
            span: Span::dummy(),
            kind: HirExprKind::Number {
                bytes: "0x1".to_owned(),
            },
        };
        let one = HirExpr {
            ty: TypeRef {
                id: TypeId(0),
                is_ref: false,
            },
            span: Span::dummy(),
            kind: HirExprKind::Number {
                bytes: "0x1".to_owned(),
            },
        };
        let zero = HirExpr {
            ty: TypeRef {
                id: TypeId(0),
                is_ref: false,
            },
            span: Span::dummy(),
            kind: HirExprKind::Number {
                bytes: "0x0".to_owned(),
            },
        };
        let func = HirFunc {
            def: DefId(0),
            name: "main".to_owned(),
            params: Vec::new(),
            returns: Vec::new(),
            is_extern: false,
            body: HirBlock {
                stmts: vec![
                    HirStmt::Let {
                        name: "x".to_owned(),
                        ty: bit8,
                        value: one,
                        span: Span::dummy(),
                    },
                    HirStmt::If {
                        cond,
                        then: HirBlock {
                            stmts: vec![temp_assign(temp, zero)],
                            span: Span::dummy(),
                        },
                        els: HirBlock {
                            stmts: Vec::new(),
                            span: Span::dummy(),
                        },
                        out: Some(temp),
                        span: Span::dummy(),
                    },
                ],
                span: Span::dummy(),
            },
            span: Span::dummy(),
        };
        assert_eq!(func.name, "main");
        assert_eq!(func.body.stmts.len(), 2);
        let HirStmt::If { out, .. } = &func.body.stmts[1] else {
            panic!("expected if, got {:?}", func.body.stmts[1]);
        };
        assert_eq!(*out, Some(temp));
        let rendered = format!("{func:?}");
        assert!(rendered.contains("If"));
        assert!(rendered.contains("Temp"));
    }
}
