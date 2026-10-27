//! The test intrinsics: `@test_count()`, `@test_name(index)` and
//! `@test_function(index)` read the run set, the `test "name" { … }`
//! blocks with [`lilac_resolve::Test::run`] set, in declaration order. A
//! runtime library's `@test_entry` runs them.

use lilac_ast::Expr;
use lilac_error::ErrorGuaranteed;
use lilac_hir::intrinsics::{Intrinsic, Slot};
use lilac_hir::{HirCallee, HirExpr, HirExprKind, HirPattern, HirValueArm};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeRef};

use crate::body::FnCx;
use crate::calls::Called;
use crate::expr::Naming;
use crate::Checker;

impl Checker<'_> {
    /// A test intrinsic: the count folds to a constant, and a name or
    /// function is a `match` of the index over every test, trapping past
    /// the last.
    pub(crate) fn check_test_intrinsic(
        &mut self,
        fcx: &mut FnCx,
        intrinsic: Intrinsic,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Called, ErrorGuaranteed> {
        let run: Vec<_> = self
            .res
            .tests
            .iter()
            .filter(|test| test.run)
            .map(|test| (test.def, test.name.clone()))
            .collect();
        if intrinsic == Intrinsic::TestCount {
            self.intrinsic_arity(name, 0, args.len(), span)?;
            return Ok(Called::Value(HirExpr {
                ty: Builtin::BitNative.value(),
                span,
                kind: HirExprKind::Int(run.len() as u64),
            }));
        }
        let arity = self.intrinsic_arity(name, 1, args.len(), span);
        self.callee(fcx, args, arity)?;
        let Some(index) = self.intrinsic_arg(
            fcx,
            &args[0],
            Slot::Builtin(Builtin::BitNative),
            false,
            span,
        ) else {
            // `intrinsic_arg` reported it.
            return Err(self.diags.already_reported());
        };
        let mut arms = Vec::with_capacity(run.len());
        let mut ty = None;
        for (position, (def, test)) in run.into_iter().enumerate() {
            let value = if intrinsic == Intrinsic::TestName {
                HirExpr {
                    ty: Builtin::ByteBuffer.value(),
                    span,
                    kind: HirExprKind::Str(test.into_bytes()),
                }
            } else {
                let naming = Naming {
                    behind_ref: true,
                    generic_args: &[],
                };
                self.func_value(def, Vec::new(), &test, span, naming)
            };
            if value.ty.is_error() {
                // `func_value` reported it.
                return Err(self.diags.already_reported());
            }
            ty = Some(value.ty);
            arms.push(HirValueArm {
                pattern: HirPattern::Int(position as u64),
                capture: None,
                moves: false,
                by_ref: false,
                value,
                drops: Vec::new(),
            });
        }
        let ty = match (ty, intrinsic) {
            (Some(ty), _) => ty,
            (None, Intrinsic::TestName) => Builtin::ByteBuffer.value(),
            (None, _) => {
                let id = self.types.intern_func_abi(Vec::new(), Vec::new(), false);
                TypeRef::reference(id)
            }
        };
        let trap = HirExpr {
            ty,
            span,
            kind: HirExprKind::Call {
                callee: HirCallee::Intrinsic(Intrinsic::Trap),
                args: Vec::new(),
                rest: Vec::new(),
            },
        };
        Ok(Called::Value(HirExpr {
            ty,
            span,
            kind: HirExprKind::Match {
                scrutinee: Box::new(index),
                arms,
                els: Box::new(trap),
            },
        }))
    }
}
