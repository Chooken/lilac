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
//! constant, so evaluation always terminates. `None` means "not constant",
//! which is never an error by itself; when the reason is a division that
//! panics, the evaluator says so ([`DivisionPanic`]), and lowering reports
//! the live ones as `E060`.

use std::collections::HashMap;

use lilac_hir::intrinsics::{Intrinsic, NumOp};
use lilac_hir::{
    FuncKey, HirCallee, HirExpr, HirExprKind, HirFunc, HirPattern, HirStmt, HirValueArm, LocalId,
};
use lilac_ir::{ArithOp, LirTy};
use lilac_resolve::DefId;
use lilac_ty::TypeRef;

use crate::{DivisionPanic, Lower};

/// A constant value.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Value {
    /// Integer bits plus the width in bits.
    Int(u64, u32),
    /// Object fields in layout order.
    Aggregate(Vec<Value>),
    /// Enum variant index and payload.
    Variant(u32, Option<Box<Value>>),
    /// A function's address (`ref f`). A global's initializer stores one;
    /// nothing computes with it.
    Func(FuncKey),
    /// A global's address (`ref G`), like [`Value::Func`].
    Global(DefId),
}

impl Value {
    /// Truthiness of a condition (non-zero, as `branch.if` tests it).
    pub(crate) fn truthy(&self) -> Option<bool> {
        match self {
            Self::Int(bits, _) => Some(*bits != 0),
            Self::Aggregate(_) | Self::Variant(..) | Self::Func(_) | Self::Global(_) => None,
        }
    }

    /// Whether this is an address.
    fn is_address(&self) -> bool {
        matches!(self, Self::Func(_) | Self::Global(_))
    }

    /// Whether this is or holds an address. Nothing compares one: an
    /// address is known only once the program is linked.
    fn has_address(&self) -> bool {
        match self {
            Self::Int(..) => false,
            Self::Func(_) | Self::Global(_) => true,
            Self::Aggregate(values) => values.iter().any(Self::has_address),
            Self::Variant(_, payload) => payload.as_deref().is_some_and(Self::has_address),
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

/// Why integer operation `op` on `left` (when known) and `right` at
/// `width` bits panics at run time: a zero divisor, or a signed `MIN`
/// divided by `-1`. The divisor alone decides a division by zero.
pub(crate) fn division_panic(
    op: &ArithOp,
    left: Option<u64>,
    right: u64,
    width: u32,
) -> Option<DivisionPanic> {
    if !is_division(op) {
        return None;
    }
    let mask = mask_of(width);
    if right & mask == 0 {
        return Some(DivisionPanic::ByZero);
    }
    let signed = matches!(op, ArithOp::IDiv | ArithOp::IMod);
    let min = 1u64 << (width - 1);
    let overflows = signed && right & mask == mask && left.is_some_and(|left| left & mask == min);
    overflows.then_some(DivisionPanic::Overflow)
}

/// Whether `op` is an integer division or remainder (the operations that
/// can panic).
pub(crate) fn is_division(op: &ArithOp) -> bool {
    matches!(
        op,
        ArithOp::IDiv | ArithOp::IDivU | ArithOp::IMod | ArithOp::IModU
    )
}

/// Integer arithmetic with LIR semantics (wrapping, signed and unsigned
/// variants) at operand width `width`; comparisons give `Int(0|1, 8)`.
/// `Ok(None)` is not constant (floats); a division or remainder that
/// panics at run time is `Err` with the reason.
pub(crate) fn eval_arith(
    op: &ArithOp,
    left: u64,
    right: u64,
    width: u32,
) -> Result<Option<Value>, DivisionPanic> {
    match division_panic(op, Some(left), right, width) {
        Some(panic) => Err(panic),
        None => Ok(fold_arith(op, left, right, width)),
    }
}

/// [`eval_arith`] once the operation is known not to panic.
fn fold_arith(op: &ArithOp, left: u64, right: u64, width: u32) -> Option<Value> {
    let mask = mask_of(width);
    let (left, right) = (left & mask, right & mask);
    let signed = |value: u64| sign_extend(value, width);
    let int = |value: u64| Some(Value::Int(value & mask, width));
    let flag = |value: bool| Some(Value::Int(u64::from(value), 8));
    match op {
        ArithOp::IAdd => int(left.wrapping_add(right)),
        ArithOp::ISub => int(left.wrapping_sub(right)),
        ArithOp::IMul => int(left.wrapping_mul(right)),
        // `division_panic` ruled out a zero divisor and `MIN / -1` (at 64
        // bits the only quotient `i64` cannot hold).
        ArithOp::IDiv => int(signed(left).checked_div(signed(right))?.cast_unsigned()),
        ArithOp::IMod => int(signed(left).checked_rem(signed(right))?.cast_unsigned()),
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
    /// Set when evaluation stopped at a division that panics: the reason
    /// the value is not constant.
    panic: Option<DivisionPanic>,
    /// A `=>` call's arguments that are not constant leave their
    /// parameters unbound instead of making the call not constant
    /// ([`Lower::inline_call_panic`]). Only [`Eval::panic`] is read then.
    partial: bool,
}

/// Which arm a constant scrutinee selects: an arm index, or `None` for
/// `else`.
pub(crate) type Choice = Option<usize>;

impl<'a> Lower<'a> {
    /// The value of `expr` if it is a constant expression.
    pub(crate) fn constant(&mut self, expr: &HirExpr) -> Option<Value> {
        self.constant_or_panic(expr).ok()
    }

    /// The value of `expr` if it is a constant expression, else why not:
    /// `Some` when evaluating it reaches a division that panics.
    pub(crate) fn constant_or_panic(
        &mut self,
        expr: &HirExpr,
    ) -> Result<Value, Option<DivisionPanic>> {
        let mut eval = Eval::default();
        eval.frames.push(HashMap::new());
        self.eval(&mut eval, expr).ok_or(eval.panic)
    }

    /// The panic a call to the `=>` function `key` reaches whatever its
    /// non-constant arguments hold: its body evaluated with the constant
    /// arguments bound and the others unknown. A panic in an argument is
    /// not the call's (each argument is lowered and checked on its own),
    /// nor is one behind a condition that reads an unknown, nor one the
    /// body reaches with no argument known (the body's own function
    /// reports that one, where it is lowered).
    pub(crate) fn inline_call_panic(
        &mut self,
        key: &FuncKey,
        args: &[HirExpr],
    ) -> Option<DivisionPanic> {
        let callee: &'a HirFunc = self.funcs.get(key).copied()?;
        let body = inline_body(callee)?;
        if self.partial_panic(key.def, HashMap::new(), body).is_some() {
            return None;
        }
        let mut frame = HashMap::new();
        for (param, arg) in callee.params.iter().zip(args) {
            if let Some(value) = self.constant(arg) {
                frame.insert(*param, value);
            }
        }
        self.partial_panic(key.def, frame, body)
    }

    /// The panic evaluating `def`'s `body` with `frame` bound (and every
    /// other parameter unknown) reaches.
    fn partial_panic(
        &mut self,
        def: DefId,
        frame: HashMap<LocalId, Value>,
        body: &HirExpr,
    ) -> Option<DivisionPanic> {
        let mut eval = Eval {
            partial: true,
            ..Eval::default()
        };
        self.expand(&mut eval, def, frame, body);
        eval.panic
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
                (HirPattern::Int(want), Value::Int(bits, _)) => want == bits,
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
            LirTy::BitNative => Some(self.native_bits()),
            _ => None,
        }
    }

    /// The value of `expr`, which takes an address (`ref`, or a `ref`
    /// cast): a function's address and a global's are constant, and so is
    /// one cast to another `ref` type; any other `ref` is not.
    fn eval_address(&mut self, eval: &mut Eval, expr: &HirExpr) -> Option<Value> {
        match &expr.kind {
            HirExprKind::FuncRef(key) => Some(Value::Func(key.clone())),
            HirExprKind::RefOf(place) => match place.kind {
                HirExprKind::Global(def) => Some(Value::Global(def)),
                _ => None,
            },
            HirExprKind::Retype(inner) => self.eval(eval, inner).filter(Value::is_address),
            _ => None,
        }
    }

    fn eval(&mut self, eval: &mut Eval, expr: &HirExpr) -> Option<Value> {
        if matches!(
            expr.kind,
            HirExprKind::FuncRef(_) | HirExprKind::RefOf(_) | HirExprKind::Retype(_)
        ) {
            return self.eval_address(eval, expr);
        }
        // A value with a drop or copy hook is never constant: skipping its
        // lowering would skip the hook.
        if expr.ty.is_ref || self.needs_drop(expr.ty) || self.needs_copy(expr.ty) {
            return None;
        }
        match &expr.kind {
            HirExprKind::Int(bits) => {
                let width = self.int_width(expr.ty)?;
                Some(Value::Int(bits & mask_of(width), width))
            }
            HirExprKind::SizeOf(ty) | HirExprKind::AlignOf(ty) => {
                let lir = self.lir_ty(*ty);
                let sized = self.lir_size_align(&lir);
                let value = if matches!(expr.kind, HirExprKind::SizeOf(_)) {
                    sized.size
                } else {
                    sized.align
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
            // Only values with no hooks get here, so a move reads like a copy.
            HirExprKind::Move(inner) => self.eval(eval, inner),
            HirExprKind::Field { base, index } => match self.eval(eval, base)? {
                Value::Aggregate(mut fields) if (*index as usize) < fields.len() => {
                    Some(fields.swap_remove(*index as usize))
                }
                _ => None,
            },
            HirExprKind::Eq(lhs, rhs) => self.eval_eq(eval, lhs, rhs),
            HirExprKind::Not(inner) => {
                let truthy = self.eval(eval, inner)?.truthy()?;
                Some(Value::Int(u64::from(!truthy), 8))
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
            } => self.eval_inline_call(eval, key, args),
            HirExprKind::Call { .. }
            | HirExprKind::Global(_)
            | HirExprKind::Deref(_)
            | HirExprKind::Float(_)
            | HirExprKind::Str(_)
            | HirExprKind::Format(_)
            | HirExprKind::CompileError { .. }
            | HirExprKind::Poison
            // Taken by `eval_address`.
            | HirExprKind::FuncRef(_)
            | HirExprKind::RefOf(_)
            | HirExprKind::Retype(_) => None,
        }
    }

    /// `lhs == rhs`, bit equality as lowered: both sides have one type, so
    /// equal values (an integer, the fields of an object, an enum's tag
    /// and payload) are equal bits. Not constant when either holds an
    /// address.
    fn eval_eq(&mut self, eval: &mut Eval, lhs: &HirExpr, rhs: &HirExpr) -> Option<Value> {
        let (left, right) = (self.eval(eval, lhs)?, self.eval(eval, rhs)?);
        if left.has_address() || right.has_address() {
            return None;
        }
        Some(Value::Int(u64::from(left == right), 8))
    }

    /// A call to the `=>` function `key`: its body with the arguments bound.
    fn eval_inline_call(
        &mut self,
        eval: &mut Eval,
        key: &FuncKey,
        args: &[HirExpr],
    ) -> Option<Value> {
        let callee: &'a HirFunc = self.funcs.get(key).copied()?;
        let body = inline_body(callee)?;
        let mut frame = HashMap::new();
        for (param, arg) in callee.params.iter().zip(args) {
            match self.eval(eval, arg) {
                Some(value) => {
                    frame.insert(*param, value);
                }
                // Every argument is evaluated before the call: an unknown
                // one leaves its parameter unknown, and a panic in one is
                // reached.
                None if eval.partial && eval.panic.is_none() => {}
                None => return None,
            }
        }
        self.expand(eval, key.def, frame, body)
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
            // An address into the matched value is no constant, like `ref`.
            if arm.by_ref {
                return None;
            }
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
        if let (Intrinsic::Int(op, _), [dividend, divisor]) = (intrinsic, args) {
            if let Some(op) = arith_op(op, false).filter(is_division) {
                return self.eval_division(eval, &op, dividend, divisor);
            }
        }
        let mut ints = Vec::with_capacity(args.len());
        for arg in args {
            let Value::Int(bits, arg_width) = self.eval(eval, arg)? else {
                return None;
            };
            ints.push((bits, arg_width));
        }
        match (intrinsic, ints.as_slice()) {
            (Intrinsic::Int(NumOp::Negate, _), [(bits, _)]) => {
                fold_arith(&ArithOp::ISub, 0, *bits, width)
            }
            (Intrinsic::Int(NumOp::Not, _), [(bits, _)]) => {
                fold_arith(&ArithOp::Xor, *bits, u64::MAX, width)
            }
            (Intrinsic::Int(op, _), [(left, operand_width), (right, _)]) => {
                fold_arith(&arith_op(op, false)?, *left, *right, *operand_width)
            }
            (Intrinsic::Truncate | Intrinsic::ExtendZero, [(bits, _)]) => {
                Some(Value::Int(bits & mask_of(width), width))
            }
            (Intrinsic::ExtendSign, [(bits, from)]) => {
                let sign = 1u64 << (from - 1);
                let extended = if bits & sign == 0 {
                    *bits
                } else {
                    bits | !mask_of(*from)
                };
                Some(Value::Int(extended & mask_of(width), width))
            }
            _ => None,
        }
    }

    /// A division or remainder: the divisor first, since a zero divisor
    /// panics whatever the dividend is, then the dividend. A panic is
    /// recorded in `eval` as the reason the value is not constant.
    fn eval_division(
        &mut self,
        eval: &mut Eval,
        op: &ArithOp,
        dividend: &HirExpr,
        divisor: &HirExpr,
    ) -> Option<Value> {
        let Value::Int(right, width) = self.eval(eval, divisor)? else {
            return None;
        };
        if let Some(panic) = division_panic(op, None, right, width) {
            eval.panic = Some(panic);
            return None;
        }
        let Value::Int(left, _) = self.eval(eval, dividend)? else {
            return None;
        };
        match eval_arith(op, left, right, width) {
            Ok(value) => value,
            Err(panic) => {
                eval.panic = Some(panic);
                None
            }
        }
    }
}

/// The value expression of a `=>` body (`{ out = expr }`).
fn inline_body(func: &HirFunc) -> Option<&HirExpr> {
    if !func.inline {
        return None;
    }
    let [HirStmt::Assign { place, value, .. }] = func.body.as_ref()?.stmts.as_slice() else {
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
            Ok(Some(Value::Int(1, 8)))
        );
        assert_eq!(
            eval_arith(&ArithOp::IDivU, u64::MAX, 2, 64),
            Ok(Some(Value::Int(0x7fff_ffff_ffff_ffff, 64)))
        );
        assert_eq!(
            eval_arith(&ArithOp::IModU, 7, 3, 64),
            Ok(Some(Value::Int(1, 64)))
        );
        // `MIN` and `-1` bit patterns are ordinary unsigned operands.
        assert_eq!(
            eval_arith(&ArithOp::IDivU, 0x80, 0xff, 8),
            Ok(Some(Value::Int(0, 8)))
        );
    }

    #[test]
    fn eval_arith_signed_and_wrapping() {
        // 0xff is -1 at 8 bits.
        assert_eq!(
            eval_arith(&ArithOp::ILt, 0xff, 0, 8),
            Ok(Some(Value::Int(1, 8)))
        );
        assert_eq!(
            eval_arith(&ArithOp::ILtU, 0xff, 0, 8),
            Ok(Some(Value::Int(0, 8)))
        );
        assert_eq!(
            eval_arith(&ArithOp::IAdd, 0xff, 2, 8),
            Ok(Some(Value::Int(1, 8)))
        );
        assert_eq!(
            eval_arith(&ArithOp::IDiv, 0xfe, 0xff, 8),
            Ok(Some(Value::Int(2, 8)))
        );
        // -7 % 2 is -1: the remainder takes the dividend's sign.
        assert_eq!(
            eval_arith(&ArithOp::IMod, 0xf9, 2, 8),
            Ok(Some(Value::Int(0xff, 8)))
        );
        assert_eq!(eval_arith(&ArithOp::FAdd, 1, 2, 32), Ok(None));
    }

    #[test]
    fn eval_arith_says_why_a_division_panics() {
        for op in [ArithOp::IDiv, ArithOp::IDivU, ArithOp::IMod, ArithOp::IModU] {
            assert_eq!(eval_arith(&op, 5, 0, 8), Err(DivisionPanic::ByZero));
            // Only the operand width's bits count.
            assert_eq!(eval_arith(&op, 5, 0x100, 8), Err(DivisionPanic::ByZero));
        }
        for (min, width) in [(0x80, 8), (0x8000, 16), (0x8000_0000, 32), (1 << 63, 64)] {
            let minus_one = mask_of(width);
            for op in [ArithOp::IDiv, ArithOp::IMod] {
                assert_eq!(
                    eval_arith(&op, min, minus_one, width),
                    Err(DivisionPanic::Overflow),
                    "{op:?} at {width} bits"
                );
            }
            // One more than `MIN`, or `MIN` by -2, is fine.
            assert_eq!(
                eval_arith(&ArithOp::IDiv, min + 1, minus_one, width),
                Ok(Some(Value::Int(min - 1, width)))
            );
            assert_eq!(
                eval_arith(&ArithOp::IDiv, min, minus_one - 1, width),
                Ok(Some(Value::Int(min >> 1, width)))
            );
        }
        // A zero divisor is known without the dividend; the overflow is not.
        assert_eq!(
            division_panic(&ArithOp::IDivU, None, 0, 32),
            Some(DivisionPanic::ByZero)
        );
        assert_eq!(division_panic(&ArithOp::IDiv, None, 0xffff_ffff, 32), None);
        assert_eq!(division_panic(&ArithOp::IAdd, Some(1), 0, 32), None);
    }
}
