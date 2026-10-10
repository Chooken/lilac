//! Constant expressions (SPEC "Constant conditions"): a structural evaluator
//! over HIR deciding which arm of an `if`, `and`/`or` or `match` is live.
//!
//! Lowering lowers only the live arm of a constant condition, so the dead
//! arm's calls never enter the call graph. Everything else about constants
//! (arithmetic on runtime values, inlining) is LLVM's job.
//!
//! The evaluator walks the expression tree with an environment of `=>`
//! parameters and `match` captures. It has no memory model, no loops and no
//! fuel: a `=>` call whose definition is already being expanded is not
//! constant, so evaluation always terminates. `None` means "not constant";
//! it is never an error.

use std::collections::HashMap;

use lilac_hir::intrinsics::{Intrinsic, NumOp};
use lilac_hir::{
    HirCallee, HirExpr, HirExprKind, HirFunc, HirPattern, HirStmt, HirValueArm, LocalId,
};
use lilac_ir::{ArithOp, LirConst, LirTy};
use lilac_resolve::DefId;
use lilac_span::Span;
use lilac_ty::TypeRef;

use crate::Lower;

/// A constant value.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Value {
    /// Integer bits plus the width in bits.
    Int(u64, u32),
    /// Object fields in layout order.
    Aggregate(Vec<Value>),
    /// Enum variant index and payload.
    Variant(u32, Option<Box<Value>>),
}

impl Value {
    /// Truthiness of a condition (non-zero, as `branch.if` tests it).
    pub(crate) fn truthy(&self) -> Option<bool> {
        match self {
            Self::Int(bits, _) => Some(*bits != 0),
            Self::Aggregate(_) | Self::Variant(..) => None,
        }
    }
}

/// The IR operation of a binary numeric intrinsic (`None` for `negate`, or an
/// unsigned operation on floats). Lowering and evaluation share it.
pub(crate) fn arith_op(op: NumOp, float: bool) -> Option<ArithOp> {
    Some(match (op, float) {
        (NumOp::Add, false) => ArithOp::IAdd,
        (NumOp::Sub, false) => ArithOp::ISub,
        (NumOp::Mul, false) => ArithOp::IMul,
        (NumOp::Div, false) => ArithOp::IDiv,
        (NumOp::DivU, false) => ArithOp::IDivU,
        (NumOp::Mod, false) => ArithOp::IMod,
        (NumOp::ModU, false) => ArithOp::IModU,
        (NumOp::Lt, false) => ArithOp::ILt,
        (NumOp::Gt, false) => ArithOp::IGt,
        (NumOp::Le, false) => ArithOp::ILe,
        (NumOp::Ge, false) => ArithOp::IGe,
        (NumOp::LtU, false) => ArithOp::ILtU,
        (NumOp::GtU, false) => ArithOp::IGtU,
        (NumOp::LeU, false) => ArithOp::ILeU,
        (NumOp::GeU, false) => ArithOp::IGeU,
        (NumOp::And, false) => ArithOp::And,
        (NumOp::Or, false) => ArithOp::Or,
        (NumOp::Xor, false) => ArithOp::Xor,
        (NumOp::Shl, false) => ArithOp::Shl,
        (NumOp::Shr, false) => ArithOp::Shr,
        (NumOp::ShrU, false) => ArithOp::ShrU,
        (NumOp::Add, true) => ArithOp::FAdd,
        (NumOp::Sub, true) => ArithOp::FSub,
        (NumOp::Mul, true) => ArithOp::FMul,
        (NumOp::Div, true) => ArithOp::FDiv,
        (NumOp::Mod, true) => ArithOp::FMod,
        (NumOp::Lt, true) => ArithOp::FLt,
        (NumOp::Gt, true) => ArithOp::FGt,
        (NumOp::Le, true) => ArithOp::FLe,
        (NumOp::Ge, true) => ArithOp::FGe,
        _ => return None,
    })
}

/// Number literal text as a constant of `lir`: a float for a float type,
/// else an integer.
pub(crate) fn number_const(text: &str, lir: &LirTy, span: Span) -> LirConst {
    if lir.is_float() {
        match text.replace('_', "").parse::<f64>() {
            Ok(value) => LirConst::Float(value),
            Err(_) => lilac_error::bug!(span, "invalid float literal `{text}`"),
        }
    } else {
        match parse_int(text) {
            Some(value) => LirConst::Int(value),
            None => lilac_error::bug!(span, "invalid integer literal `{text}`"),
        }
    }
}

/// Parses integer literal text (`0x`, `0b`, decimal, `_` separators).
pub(crate) fn parse_int(text: &str) -> Option<u64> {
    let digits = text.trim().replace('_', "");
    if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(bin) = digits
        .strip_prefix("0b")
        .or_else(|| digits.strip_prefix("0B"))
    {
        u64::from_str_radix(bin, 2).ok()
    } else if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
        digits.parse::<u64>().ok()
    } else {
        None
    }
}

/// All-ones mask of `width` bits.
pub(crate) fn mask_of(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1 << width) - 1
    }
}

/// Two's-complement value of the low `width` bits.
pub(crate) fn sign_extend(value: u64, width: u32) -> i64 {
    if width >= 64 {
        return value.cast_signed();
    }
    let shift = 64 - width;
    (value << shift).cast_signed() >> shift
}

/// Integer arithmetic with LIR semantics (wrapping, signed and unsigned
/// variants) at operand width `width`; comparisons give `Int(0|1, 8)`.
/// Floats and division or modulo by zero are not constant.
pub(crate) fn eval_arith(op: &ArithOp, left: u64, right: u64, width: u32) -> Option<Value> {
    let mask = mask_of(width);
    let (left, right) = (left & mask, right & mask);
    let signed = |value: u64| sign_extend(value, width);
    let int = |value: u64| Some(Value::Int(value & mask, width));
    let flag = |value: bool| Some(Value::Int(u64::from(value), 8));
    match op {
        ArithOp::IAdd => int(left.wrapping_add(right)),
        ArithOp::ISub => int(left.wrapping_sub(right)),
        ArithOp::IMul => int(left.wrapping_mul(right)),
        ArithOp::IDiv if right != 0 => {
            int(signed(left).wrapping_div(signed(right)).cast_unsigned())
        }
        ArithOp::IMod if right != 0 => {
            int(signed(left).wrapping_rem(signed(right)).cast_unsigned())
        }
        ArithOp::IDivU => int(left.checked_div(right)?),
        ArithOp::IModU => int(left.checked_rem(right)?),
        ArithOp::Eq => flag(left == right),
        ArithOp::ILt => flag(signed(left) < signed(right)),
        ArithOp::IGt => flag(signed(left) > signed(right)),
        ArithOp::ILe => flag(signed(left) <= signed(right)),
        ArithOp::IGe => flag(signed(left) >= signed(right)),
        ArithOp::ILtU => flag(left < right),
        ArithOp::IGtU => flag(left > right),
        ArithOp::ILeU => flag(left <= right),
        ArithOp::IGeU => flag(left >= right),
        ArithOp::And => int(left & right),
        ArithOp::Or => int(left | right),
        ArithOp::Xor => int(left ^ right),
        ArithOp::Shl => int(left << (right % u64::from(width))),
        ArithOp::Shr => int((signed(left) >> (right % u64::from(width))).cast_unsigned()),
        ArithOp::ShrU => int(left >> (right % u64::from(width))),
        _ => None,
    }
}

/// Evaluation state: the `=>` definitions being expanded and one binding
/// frame per expansion.
#[derive(Default)]
pub(crate) struct Eval {
    stack: Vec<DefId>,
    frames: Vec<HashMap<LocalId, Value>>,
}

/// Which arm a constant scrutinee selects: an arm index, or `None` for
/// `else`.
pub(crate) type Choice = Option<usize>;

impl<'a> Lower<'a> {
    /// The value of `expr` if it is a constant expression.
    pub(crate) fn constant(&mut self, expr: &HirExpr) -> Option<Value> {
        let mut eval = Eval::default();
        eval.frames.push(HashMap::new());
        self.eval(&mut eval, expr)
    }

    /// The arm a `match` on `scrutinee` takes, when the scrutinee is constant,
    /// plus the scrutinee's value.
    pub(crate) fn constant_choice<'p>(
        &mut self,
        scrutinee: &HirExpr,
        patterns: impl Iterator<Item = &'p HirPattern>,
    ) -> Option<(Choice, Value)> {
        let value = self.constant(scrutinee)?;
        let choice = Self::choose(&value, patterns)?;
        Some((choice, value))
    }

    /// The first pattern `value` matches (`None` when undecidable).
    fn choose<'p>(value: &Value, patterns: impl Iterator<Item = &'p HirPattern>) -> Option<Choice> {
        for (index, pattern) in patterns.enumerate() {
            let hit = match (pattern, value) {
                (HirPattern::Variant(want), Value::Variant(have, _)) => want == have,
                (HirPattern::Const(lit), Value::Int(bits, width)) => {
                    parse_int(lit)? & mask_of(*width) == *bits
                }
                _ => return None,
            };
            if hit {
                return Some(Some(index));
            }
        }
        Some(None)
    }

    /// Width of an integer type, or `None` for anything else.
    fn int_width(&self, ty: TypeRef) -> Option<u32> {
        match self.lir_ty(ty) {
            LirTy::Bit(bits) => Some(u32::from(bits)),
            LirTy::BitNative => u32::try_from(self.ptr_bytes * 8).ok(),
            _ => None,
        }
    }

    fn eval(&mut self, eval: &mut Eval, expr: &HirExpr) -> Option<Value> {
        // A value with a drop or copy hook is never constant: skipping its
        // lowering would skip the hook.
        if expr.ty.is_ref || self.needs_drop(expr.ty) || self.needs_copy(expr.ty) {
            return None;
        }
        match &expr.kind {
            HirExprKind::Number(text) => {
                let width = self.int_width(expr.ty)?;
                Some(Value::Int(parse_int(text)? & mask_of(width), width))
            }
            HirExprKind::SizeOf(ty) | HirExprKind::AlignOf(ty) => {
                let lir = self.lir_ty(*ty);
                let (size, align) =
                    lilac_ir::size_align(&self.module.layouts, &lir, self.ptr_bytes);
                let value = if matches!(expr.kind, HirExprKind::SizeOf(_)) {
                    size
                } else {
                    align
                };
                let width = self.int_width(expr.ty)?;
                Some(Value::Int(value & mask_of(width), width))
            }
            HirExprKind::Const(def) => {
                let hir = self.hir;
                self.expand(eval, *def, HashMap::new(), hir.consts.get(def)?)
            }
            HirExprKind::Struct(fields) => fields
                .iter()
                .map(|field| self.eval(eval, field))
                .collect::<Option<Vec<_>>>()
                .map(Value::Aggregate),
            HirExprKind::Variant { index, payload } => {
                let payload = match payload {
                    Some(payload) => Some(Box::new(self.eval(eval, payload)?)),
                    None => None,
                };
                Some(Value::Variant(*index, payload))
            }
            HirExprKind::Field { base, index } => match self.eval(eval, base)? {
                Value::Aggregate(mut fields) if (*index as usize) < fields.len() => {
                    Some(fields.swap_remove(*index as usize))
                }
                _ => None,
            },
            HirExprKind::Eq(lhs, rhs) => {
                let (Value::Int(left, _), Value::Int(right, _)) =
                    (self.eval(eval, lhs)?, self.eval(eval, rhs)?)
                else {
                    return None;
                };
                Some(Value::Int(u64::from(left == right), 8))
            }
            HirExprKind::If { cond, then, els } => {
                if self.eval(eval, cond)?.truthy()? {
                    self.eval(eval, then)
                } else {
                    self.eval(eval, els)
                }
            }
            HirExprKind::Logic { and, lhs, rhs } => {
                // Like the lowered form: `rhs` itself when it decides.
                if self.eval(eval, lhs)?.truthy()? == *and {
                    self.eval(eval, rhs)
                } else {
                    Some(Value::Int(u64::from(!*and), 8))
                }
            }
            HirExprKind::Match {
                scrutinee,
                arms,
                els,
            } => self.eval_match(eval, scrutinee, arms, els),
            HirExprKind::Local(id) => eval.frames.last()?.get(id).cloned(),
            HirExprKind::Call {
                callee: HirCallee::Intrinsic(intrinsic),
                args,
                ..
            } => self.eval_intrinsic(eval, *intrinsic, args, expr.ty),
            HirExprKind::Call {
                callee: HirCallee::Func(key),
                args,
                ..
            } => {
                let callee: &'a HirFunc = self.funcs.get(key).copied()?;
                let body = inline_body(callee)?;
                let mut frame = HashMap::new();
                for (param, arg) in callee.params.iter().zip(args) {
                    frame.insert(*param, self.eval(eval, arg)?);
                }
                self.expand(eval, key.def, frame, body)
            }
            HirExprKind::Call { .. }
            | HirExprKind::Global(_)
            | HirExprKind::Deref(_)
            | HirExprKind::Str(_)
            | HirExprKind::FuncRef(_)
            | HirExprKind::RefOf(_)
            | HirExprKind::Retype(_)
            | HirExprKind::FormatArgs(_)
            | HirExprKind::CompileError { .. }
            | HirExprKind::Poison => None,
        }
    }

    /// Evaluates the body of `def` (an inline constant or `=>` function)
    /// with `frame` bound; a definition already being expanded is not
    /// constant.
    fn expand(
        &mut self,
        eval: &mut Eval,
        def: DefId,
        frame: HashMap<LocalId, Value>,
        body: &HirExpr,
    ) -> Option<Value> {
        if eval.stack.contains(&def) {
            return None;
        }
        eval.stack.push(def);
        eval.frames.push(frame);
        let value = self.eval(eval, body);
        eval.frames.pop();
        eval.stack.pop();
        value
    }

    /// A value `match`: the chosen arm (its capture bound), else `els`.
    fn eval_match(
        &mut self,
        eval: &mut Eval,
        scrutinee: &HirExpr,
        arms: &[HirValueArm],
        els: &HirExpr,
    ) -> Option<Value> {
        let value = self.eval(eval, scrutinee)?;
        let Some(index) = Self::choose(&value, arms.iter().map(|arm| &arm.pattern))? else {
            return self.eval(eval, els);
        };
        let arm = &arms[index];
        if let Some(capture) = arm.capture {
            let Value::Variant(_, Some(payload)) = value else {
                return None;
            };
            eval.frames.last_mut()?.insert(capture, *payload);
        }
        self.eval(eval, &arm.value)
    }

    fn eval_intrinsic(
        &mut self,
        eval: &mut Eval,
        intrinsic: Intrinsic,
        args: &[HirExpr],
        ty: TypeRef,
    ) -> Option<Value> {
        let width = self.int_width(ty)?;
        let mut ints = Vec::with_capacity(args.len());
        for arg in args {
            let Value::Int(bits, arg_width) = self.eval(eval, arg)? else {
                return None;
            };
            ints.push((bits, arg_width));
        }
        match (intrinsic, ints.as_slice()) {
            (Intrinsic::Int(NumOp::Negate, _), [(bits, _)]) => {
                eval_arith(&ArithOp::ISub, 0, *bits, width)
            }
            (Intrinsic::Int(NumOp::Not, _), [(bits, _)]) => {
                eval_arith(&ArithOp::Xor, *bits, u64::MAX, width)
            }
            (Intrinsic::Int(op, _), [(left, operand_width), (right, _)]) => {
                eval_arith(&arith_op(op, false)?, *left, *right, *operand_width)
            }
            (Intrinsic::Truncate | Intrinsic::ExtendZero, [(bits, _)]) => {
                Some(Value::Int(bits & mask_of(width), width))
            }
            _ => None,
        }
    }
}

/// The value expression of a `=>` body (`{ out = expr }`).
fn inline_body(func: &HirFunc) -> Option<&HirExpr> {
    if !func.inline {
        return None;
    }
    let [HirStmt::Assign { place, value }] = func.body.as_ref()?.stmts.as_slice() else {
        return None;
    };
    match (&place.kind, func.returns.as_slice()) {
        (HirExprKind::Local(id), [out]) if id == out => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_arith_unsigned_semantics() {
        assert_eq!(
            eval_arith(&ArithOp::IGtU, u64::MAX, 1, 64),
            Some(Value::Int(1, 8))
        );
        assert_eq!(
            eval_arith(&ArithOp::IDivU, u64::MAX, 2, 64),
            Some(Value::Int(0x7fff_ffff_ffff_ffff, 64))
        );
        assert_eq!(
            eval_arith(&ArithOp::IModU, 7, 3, 64),
            Some(Value::Int(1, 64))
        );
        assert_eq!(eval_arith(&ArithOp::IDivU, 1, 0, 64), None);
    }

    #[test]
    fn eval_arith_signed_and_wrapping() {
        // 0xff is -1 at 8 bits.
        assert_eq!(
            eval_arith(&ArithOp::ILt, 0xff, 0, 8),
            Some(Value::Int(1, 8))
        );
        assert_eq!(
            eval_arith(&ArithOp::ILtU, 0xff, 0, 8),
            Some(Value::Int(0, 8))
        );
        assert_eq!(
            eval_arith(&ArithOp::IAdd, 0xff, 2, 8),
            Some(Value::Int(1, 8))
        );
        assert_eq!(
            eval_arith(&ArithOp::IDiv, 0xfe, 0xff, 8),
            Some(Value::Int(2, 8))
        );
        assert_eq!(eval_arith(&ArithOp::IMod, 5, 0, 8), None);
        assert_eq!(eval_arith(&ArithOp::FAdd, 1, 2, 32), None);
    }
}
