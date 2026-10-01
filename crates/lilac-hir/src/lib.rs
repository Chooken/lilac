//! Typed HIR (real lowering from AST lands in M1/M2).

use lilac_span::Span;
use lilac_ty::TypeId;

/// Typed expression placeholder.
#[derive(Clone, Debug)]
pub struct HirExpr {
    /// Checked type.
    pub ty: TypeId,
    /// Source span.
    pub span: Span,
}

/// Typed program placeholder.
#[derive(Clone, Debug, Default)]
pub struct HirProgram {
    /// Checked expressions.
    pub exprs: Vec<HirExpr>,
}
