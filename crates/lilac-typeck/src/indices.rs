//! `E037` in generic templates: a literal `@array_at` index at or past the
//! array's length.
//!
//! Lowering finds `E037` in every lowered function with its constant
//! evaluator, but templates are never lowered. An array's length is a
//! literal even when its element is a parameter, so a literal (or literal
//! `const`) index is checked here. An arm a condition reading no local
//! guards is skipped: that condition may be constant (`@size_of(T)`, a
//! target flag), and lowering would then drop the arm for some arguments.

use lilac_error::{codes, Diagnostic};
use lilac_hir::intrinsics::Intrinsic;
use lilac_hir::{HirBlock, HirCallee, HirExpr, HirExprKind, HirFunc, HirStmt};
use lilac_span::Span;
use lilac_ty::TypeKind;

use crate::Checker;

impl Checker<'_> {
    /// Reports `E037` for each literal out-of-bounds index in `func` (array
    /// lengths are at least 1, `E010`).
    pub(crate) fn check_template_indices(&mut self, func: &HirFunc) {
        let Some(body) = &func.body else {
            return;
        };
        let mut bad = Vec::new();
        self.block_indices(body, false, &mut bad);
        for (span, index, len) in bad {
            self.diags.emit(
                Diagnostic::error(
                    codes::E037,
                    format!("index {index} is out of bounds for an array of length {len}"),
                )
                .label(span, true, format!("the last index is {}", len - 1)),
            );
        }
    }

    fn block_indices(&self, block: &HirBlock, guarded: bool, bad: &mut Vec<(Span, u64, u64)>) {
        for stmt in &block.stmts {
            match stmt {
                HirStmt::Let { value, .. } | HirStmt::Expr(value) => {
                    self.expr_indices(value, guarded, bad);
                }
                HirStmt::MultiLet { call, .. } => self.expr_indices(call, guarded, bad),
                HirStmt::Assign { place, value, .. } => {
                    self.expr_indices(place, guarded, bad);
                    self.expr_indices(value, guarded, bad);
                }
                HirStmt::Return(_)
                | HirStmt::Fail(_)
                | HirStmt::Break(_)
                | HirStmt::Continue(_)
                | HirStmt::Drop { .. } => {}
                HirStmt::If { cond, then, els } => {
                    self.expr_indices(cond, guarded, bad);
                    let arms = guarded || may_be_constant(cond);
                    self.block_indices(then, arms, bad);
                    self.block_indices(els, arms, bad);
                }
                HirStmt::Loop(body) | HirStmt::Block(body) => {
                    self.block_indices(body, guarded, bad);
                }
                HirStmt::Match {
                    scrutinee,
                    arms,
                    els,
                } => {
                    self.expr_indices(scrutinee, guarded, bad);
                    let guarded = guarded || may_be_constant(scrutinee);
                    for arm in arms {
                        self.block_indices(&arm.body, guarded, bad);
                    }
                    self.block_indices(els, guarded, bad);
                }
            }
        }
    }

    fn expr_indices(&self, expr: &HirExpr, guarded: bool, bad: &mut Vec<(Span, u64, u64)>) {
        match &expr.kind {
            HirExprKind::If { cond, then, els } => {
                self.expr_indices(cond, guarded, bad);
                let arms = guarded || may_be_constant(cond);
                self.expr_indices(then, arms, bad);
                self.expr_indices(els, arms, bad);
            }
            HirExprKind::Match {
                scrutinee,
                arms,
                els,
            } => {
                self.expr_indices(scrutinee, guarded, bad);
                let guarded = guarded || may_be_constant(scrutinee);
                for arm in arms {
                    self.expr_indices(&arm.value, guarded, bad);
                }
                self.expr_indices(els, guarded, bad);
            }
            HirExprKind::Logic { lhs, rhs, .. } => {
                self.expr_indices(lhs, guarded, bad);
                self.expr_indices(rhs, guarded || may_be_constant(lhs), bad);
            }
            HirExprKind::Call { callee, args, .. } => {
                if let (HirCallee::Intrinsic(Intrinsic::ArrayAt), [array, index]) =
                    (callee, args.as_slice())
                {
                    if !guarded {
                        self.check_index(array, index, bad);
                    }
                }
                if let HirCallee::Value(callee) = callee {
                    self.expr_indices(callee, guarded, bad);
                }
                for arg in args {
                    self.expr_indices(arg, guarded, bad);
                }
            }
            HirExprKind::Field { base, .. }
            | HirExprKind::Deref(base)
            | HirExprKind::RefOf(base)
            | HirExprKind::Move(base)
            | HirExprKind::Not(base)
            | HirExprKind::Retype(base)
            | HirExprKind::Assumed {
                base: Some(base), ..
            } => self.expr_indices(base, guarded, bad),
            HirExprKind::Struct(values) => {
                for value in values {
                    self.expr_indices(value, guarded, bad);
                }
            }
            HirExprKind::Format(format) => {
                for hole in &format.holes {
                    self.expr_indices(&hole.value, guarded, bad);
                }
            }
            HirExprKind::Variant {
                payload: Some(payload),
                ..
            }
            | HirExprKind::AssumedVariant {
                payload: Some(payload),
                ..
            } => self.expr_indices(payload, guarded, bad),
            HirExprKind::Eq(lhs, rhs) => {
                self.expr_indices(lhs, guarded, bad);
                self.expr_indices(rhs, guarded, bad);
            }
            HirExprKind::Variant { payload: None, .. }
            | HirExprKind::AssumedVariant { payload: None, .. }
            | HirExprKind::Local(_)
            | HirExprKind::Int(_)
            | HirExprKind::Float(_)
            | HirExprKind::Str(_)
            | HirExprKind::FuncRef(_)
            | HirExprKind::Const(_)
            | HirExprKind::Global(_)
            | HirExprKind::SizeOf(_)
            | HirExprKind::AlignOf(_)
            | HirExprKind::Unanswered
            | HirExprKind::Assumed { base: None, .. }
            | HirExprKind::CompileError { .. }
            | HirExprKind::Poison => {}
        }
    }

    fn check_index(&self, array: &HirExpr, index: &HirExpr, bad: &mut Vec<(Span, u64, u64)>) {
        let TypeKind::Array { len, .. } = *self.types.kind(array.ty.id) else {
            return;
        };
        let literal = match &index.kind {
            HirExprKind::Const(def) => self.out.consts.get(def),
            _ => Some(index),
        };
        let Some(HirExprKind::Int(value)) = literal.map(|expr| &expr.kind) else {
            return;
        };
        let value = *value;
        if value >= len {
            bad.push((index.span, value, len));
        }
    }
}

/// Whether `cond` might be a constant condition: it reads no local.
fn may_be_constant(cond: &HirExpr) -> bool {
    let mut reads_local = false;
    cond.walk(&mut |expr| {
        reads_local |= matches!(expr.kind, HirExprKind::Local(_) | HirExprKind::Global(_));
    });
    !reads_local
}
