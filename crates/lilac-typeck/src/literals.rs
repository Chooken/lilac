//! Number literals: typing (smallest fit, grown to the expected width),
//! reading each one once into the value the HIR holds
//! (`HirExprKind::Int`, `HirExprKind::Float`), and compile-time folding of
//! all-literal arithmetic (SPEC §2). A float is rounded once, to the type it
//! takes, and folds at that type's precision.

use lilac_ast::{BinOp, Expr, MatchArm, PatternLiteral};
use lilac_error::{codes, Diagnostic};
use lilac_float::{FloatBits, FloatError, FloatOp};
use lilac_hir::{HirExpr, HirExprKind, HirPattern};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeId, TypeRef};

use crate::sigs::NEGATE_HOOK;
use crate::Checker;

/// Whether a number literal's source text names a float (`3.14`, `1e3`). A
/// `0x`/`0b` form never does (`0xE` is an integer).
pub(crate) fn is_float_literal(bytes: &str) -> bool {
    let prefixed = ["0x", "0X", "0b", "0B"]
        .iter()
        .any(|p| bytes.starts_with(p));
    !prefixed && bytes.bytes().any(|byte| matches!(byte, b'.' | b'e' | b'E'))
}

/// The unsigned magnitude of an integer literal (`None` for empty digits).
/// A magnitude past `u128` reports `u128::MAX`, which trips every width check.
pub(crate) fn integer_magnitude(bytes: &str) -> Option<u128> {
    let digits = bytes.replace('_', "");
    let (radix, body) = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (16, hex)
    } else if let Some(bin) = digits
        .strip_prefix("0b")
        .or_else(|| digits.strip_prefix("0B"))
    {
        (2, bin)
    } else {
        (10, digits.as_str())
    };
    if body.is_empty() {
        return None;
    }
    Some(u128::from_str_radix(body, radix).unwrap_or(u128::MAX))
}

fn significant_bits(magnitude: u128) -> u32 {
    u128::BITS - magnitude.leading_zeros()
}

/// `value`, which a width check has already bounded to 64 bits.
fn word(value: u128, span: Span) -> u64 {
    u64::try_from(value)
        .unwrap_or_else(|_| lilac_error::bug!(span, "literal value {value:#x} past 64 bits"))
}

/// The smallest of 8/16/32/64 holding `bits` unsigned bits.
fn smallest_width(bits: u32) -> Option<u32> {
    match bits {
        0..=8 => Some(8),
        9..=16 => Some(16),
        17..=32 => Some(32),
        33..=64 => Some(64),
        _ => None,
    }
}

/// The smallest of 8/16/32/64 whose signed range holds `-magnitude`.
fn signed_smallest_width(magnitude: u128) -> Option<u32> {
    [8u32, 16, 32, 64]
        .into_iter()
        .find(|width| magnitude <= 1u128 << (width - 1))
}

/// Whether `op` is literal-foldable arithmetic.
pub(crate) fn is_arithmetic(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
    )
}

/// Whether `expr` is an all-literal arithmetic subtree.
pub(crate) fn is_foldable_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Number(..) => true,
        Expr::BinOp { lhs, op, rhs, .. } if is_arithmetic(*op) => {
            is_foldable_literal(lhs) && is_foldable_literal(rhs)
        }
        _ => false,
    }
}

/// A folded literal value.
#[derive(Clone, Copy)]
enum Value {
    Int {
        negative: bool,
        magnitude: u128,
    },
    /// Rounded to the fold's float type ([`Checker::float_target`]).
    Float(FloatBits),
}

/// The result of folding a subtree.
enum Folded {
    NotLiteral,
    Value(Value),
    Error,
}

impl Checker<'_> {
    fn int(ty: TypeId, value: u64, span: Span) -> HirExpr {
        HirExpr {
            ty: TypeRef {
                id: ty,
                is_ref: false,
            },
            span,
            kind: HirExprKind::Int(value),
        }
    }

    fn float(ty: TypeId, bits: FloatBits, span: Span) -> HirExpr {
        HirExpr {
            ty: TypeRef {
                id: ty,
                is_ref: false,
            },
            span,
            kind: HirExprKind::Float(bits),
        }
    }

    /// The single `@conversion(from: <bit|fbit>) -> expected` source width, so
    /// `x: Int16 = 5` grows the literal to `@bit16` (ambiguity yields `None`).
    fn conversion_source(&self, expected: TypeId, want_float: bool) -> Option<TypeId> {
        let mut found = None;
        for (from, to) in self.conversion_pairs() {
            if to != expected {
                continue;
            }
            let fits = if want_float {
                self.is_float(from)
            } else {
                self.is_bit(from)
            };
            if !fits {
                continue;
            }
            if found.is_some() {
                return None;
            }
            found = Some(from);
        }
        found
    }

    /// The float type a float literal takes under `expected`: an expected
    /// `@fbitN`, else a numeric object's single `@conversion(from: @fbitN)`,
    /// else `@fbit64`.
    fn float_target(&self, expected: Option<TypeId>) -> TypeId {
        let fbit64 = Builtin::FBit64.id();
        match expected {
            Some(ty) if self.is_float(ty) => ty,
            Some(ty) => self.conversion_source(ty, true).unwrap_or(fbit64),
            None => fbit64,
        }
    }

    /// `E010` when `expected` is a bit type: there is no implicit float to
    /// integer conversion. `what` names the float ("literal").
    fn float_in_bit_slot(&mut self, expected: Option<TypeId>, span: Span, what: &str) -> bool {
        if !expected.is_some_and(|ty| self.is_bit(ty)) {
            return false;
        }
        self.diags.emit(
            Diagnostic::error(
                codes::E010,
                "mismatched types: expected an integer, found a float",
            )
            .label(span, true, format!("float {what} here")),
        );
        true
    }

    /// Float literal text rounded once to `target` (`E060` past its largest
    /// finite value).
    fn float_literal(&mut self, bytes: &str, target: TypeId, span: Span) -> Option<FloatBits> {
        match FloatBits::parse(bytes, self.float_format(target)) {
            Ok(bits) => Some(bits),
            Err(FloatError::Invalid) => {
                // The lexer keeps a malformed number's well-formed part, so
                // this is only reachable through a lexer bug; still an error.
                self.diags.emit(
                    Diagnostic::error(codes::E060, "invalid float literal").label(
                        span,
                        true,
                        "not a valid float literal",
                    ),
                );
                None
            }
            Err(FloatError::Overflow | FloatError::DivisionByZero) => {
                self.diags.emit(
                    Diagnostic::error(
                        codes::E060,
                        format!("float literal does not fit in {}", self.show_id(target)),
                    )
                    .label(span, true, "literal out of range"),
                );
                None
            }
        }
    }

    /// Types a number literal under `expected`.
    pub(crate) fn check_number(
        &mut self,
        bytes: &str,
        span: Span,
        expected: Option<TypeId>,
    ) -> HirExpr {
        if is_float_literal(bytes) {
            if self.float_in_bit_slot(expected, span, "literal") {
                return HirExpr::poison(span);
            }
            let target = self.float_target(expected);
            return match self.float_literal(bytes, target, span) {
                Some(bits) => Self::float(target, bits, span),
                None => HirExpr::poison(span),
            };
        }
        let Some(magnitude) = integer_magnitude(bytes) else {
            self.diags.emit(
                Diagnostic::error(codes::E060, "invalid integer literal").label(
                    span,
                    true,
                    "not a valid integer literal",
                ),
            );
            return HirExpr::poison(span);
        };
        match self.int_literal(magnitude, span, expected) {
            Some((ty, value)) => Self::int(ty, value, span),
            None => HirExpr::poison(span),
        }
    }

    /// The bit type and value an unsigned integer `magnitude` takes under
    /// `expected`.
    fn int_literal(
        &mut self,
        magnitude: u128,
        span: Span,
        expected: Option<TypeId>,
    ) -> Option<(TypeId, u64)> {
        let bits = significant_bits(magnitude);
        let constrained = expected
            .filter(|id| self.is_bit(*id))
            .or_else(|| expected.and_then(|id| self.conversion_source(id, false)));
        if let Some(ty) = constrained {
            let width = self.bit_width(ty);
            if bits > width {
                self.diags.emit(
                    Diagnostic::error(
                        codes::E060,
                        format!(
                            "value needs {bits} bits but {} holds {width}",
                            self.show_id(ty)
                        ),
                    )
                    .label(span, true, "literal overflows"),
                );
                return None;
            }
            return Some((ty, word(magnitude, span)));
        }
        let Some(width) = smallest_width(bits) else {
            self.diags.emit(
                Diagnostic::error(codes::E060, "integer literal does not fit in 64 bits").label(
                    span,
                    true,
                    "literal too large",
                ),
            );
            return None;
        };
        Some((Self::bit_of_width(width), word(magnitude, span)))
    }

    /// Checks `arm`'s literal pattern against `scrutinee`, which must be a
    /// bit container (`E010`); a literal pattern captures nothing (`E022`).
    pub(crate) fn literal_pattern(
        &mut self,
        arm: &MatchArm,
        literal: &PatternLiteral,
        span: Span,
        scrutinee: TypeId,
    ) -> HirPattern {
        let bit = scrutinee != TypeId::ERROR && self.is_bit(scrutinee);
        if scrutinee != TypeId::ERROR && !bit {
            self.diags.emit(
                Diagnostic::error(
                    codes::E010,
                    format!(
                        "literal patterns need a bit container scrutinee, found {}",
                        self.show_id(scrutinee)
                    ),
                )
                .label(arm.span, true, "not a bit container"),
            );
        }
        if arm.capture.is_some() {
            self.diags.emit(
                Diagnostic::error(codes::E022, "literal patterns capture nothing").label(
                    arm.span,
                    true,
                    "captures need `.Tag` patterns",
                ),
            );
        }
        // A pattern that is not a value of the scrutinee has its error
        // (above, or from `pattern_value`), so the body is never lowered
        // and `0` stands in.
        let value = if bit {
            self.pattern_value(literal, span, scrutinee)
        } else {
            None
        };
        HirPattern::Int(value.unwrap_or(0))
    }

    /// The value of a literal pattern matched against bit type `scrutinee`:
    /// an integer or a character, `E060` when it needs more bits than the
    /// scrutinee holds. A float or a string is `E010`: a match compares bits,
    /// and neither is the bits of a bit container. `None` only after an
    /// error.
    fn pattern_value(
        &mut self,
        literal: &PatternLiteral,
        span: Span,
        scrutinee: TypeId,
    ) -> Option<u64> {
        let not_a_pattern = |what: &str| {
            Diagnostic::error(codes::E010, format!("a {what} is not a pattern"))
                .label(span, true, format!("a {what}"))
                .help("match a bit container against integers and characters")
        };
        let magnitude = match literal {
            PatternLiteral::Number(text) if is_float_literal(text) => {
                self.diags.emit(not_a_pattern("float literal"));
                return None;
            }
            PatternLiteral::Number(text) => {
                let Some(magnitude) = integer_magnitude(text) else {
                    self.diags.emit(
                        Diagnostic::error(codes::E060, "invalid integer literal").label(
                            span,
                            true,
                            "not a valid integer literal",
                        ),
                    );
                    return None;
                };
                magnitude
            }
            PatternLiteral::Char(scalar) => u128::from(u32::from(*scalar)),
            PatternLiteral::Str(_) => {
                self.diags.emit(not_a_pattern("string literal"));
                return None;
            }
        };
        let width = self.bit_width(scrutinee);
        let bits = significant_bits(magnitude);
        if bits > width {
            self.diags.emit(
                Diagnostic::error(
                    codes::E060,
                    format!(
                        "value needs {bits} bits but {} holds {width}",
                        self.show_id(scrutinee)
                    ),
                )
                .label(span, true, "pattern overflows"),
            );
            return None;
        }
        Some(word(magnitude, span))
    }

    /// Folds `-<integer literal>` into a two's-complement constant. `None` when
    /// the operand is not an integer literal (the `@negate` hook applies).
    pub(crate) fn fold_negative(
        &mut self,
        operand: &Expr,
        span: Span,
        expected: Option<TypeId>,
    ) -> Option<HirExpr> {
        let Expr::Number(bytes, lit_span) = operand else {
            return None;
        };
        // A numeric object takes a negative literal only when it has a
        // `@negate`: one without (an unsigned integer) reports the missing
        // hook, never wraps `-1` to its maximum.
        if let Some(id) = expected.filter(|id| !self.is_bit(*id)) {
            if self.conversion_source(id, false).is_some() && !self.can_negate(id) {
                let ty = self.show_id(id);
                self.diags.emit(
                    Diagnostic::error(codes::E043, format!("no @negate for {ty}"))
                        .label(span, true, format!("{ty} has no negative values"))
                        .help(format!(
                            "`-` needs a `@negate: function (value: {ty}) {ty}` hook"
                        )),
                );
                return Some(HirExpr::poison(span));
            }
        }
        if is_float_literal(bytes) {
            // A float literal is its magnitude at the slot's type, sign
            // flipped, so `-1.0` never picks some type's `@negate`.
            if self.float_in_bit_slot(expected, span, "literal") {
                return Some(HirExpr::poison(span));
            }
            let target = self.float_target(expected);
            return Some(match self.float_literal(bytes, target, *lit_span) {
                Some(bits) => Self::float(target, bits.negate(), span),
                None => HirExpr::poison(span),
            });
        }
        let Some(magnitude) = integer_magnitude(bytes) else {
            self.diags.emit(
                Diagnostic::error(codes::E060, "invalid integer literal").label(
                    *lit_span,
                    true,
                    "not a valid integer literal",
                ),
            );
            return Some(HirExpr::poison(span));
        };
        let Some(fit) = signed_smallest_width(magnitude) else {
            self.diags.emit(
                Diagnostic::error(
                    codes::E060,
                    "negative integer literal does not fit in 64 bits",
                )
                .label(*lit_span, true, "literal too large"),
            );
            return Some(HirExpr::poison(span));
        };
        Some(
            match self.signed_value(magnitude, fit, *lit_span, expected) {
                Some((ty, value)) => Self::int(ty, value, span),
                None => HirExpr::poison(span),
            },
        )
    }

    /// Whether `ty` has a `@negate` hook (taking it by value or by `ref`).
    fn can_negate(&self, ty: TypeId) -> bool {
        self.overloads(NEGATE_HOOK)
            .iter()
            .any(|hook| matches!(hook.sig.params.as_slice(), [param] if param.id == ty))
    }

    /// The bit type and two's-complement bits of `-magnitude`, grown to the
    /// expected width.
    fn signed_value(
        &mut self,
        magnitude: u128,
        fit: u32,
        span: Span,
        expected: Option<TypeId>,
    ) -> Option<(TypeId, u64)> {
        let constrained = expected
            .filter(|id| self.is_bit(*id))
            .or_else(|| expected.and_then(|id| self.conversion_source(id, false)));
        let (ty, width) = if let Some(ty) = constrained {
            let width = self.bit_width(ty);
            if fit > width {
                self.diags.emit(
                    Diagnostic::error(
                        codes::E060,
                        format!(
                            "value needs {fit} bits but {} holds {width}",
                            self.show_id(ty)
                        ),
                    )
                    .label(span, true, "literal overflows"),
                );
                return None;
            }
            (ty, width)
        } else {
            (Self::bit_of_width(fit), fit)
        };
        let mask = (1u128 << width) - 1;
        let value = ((1u128 << width) - magnitude) & mask;
        Some((ty, word(value, span)))
    }

    /// Folds an all-literal `+ - * / %`; `None` when an operand is not a
    /// literal (the operator hook applies).
    pub(crate) fn fold_binop(
        &mut self,
        lhs: &Expr,
        op: BinOp,
        rhs: &Expr,
        span: Span,
        expected: Option<TypeId>,
    ) -> Option<HirExpr> {
        if !is_arithmetic(op) {
            return None;
        }
        let float = self.float_target(expected);
        let left = self.fold(lhs, float);
        let right = self.fold(rhs, float);
        if matches!(left, Folded::Error) || matches!(right, Folded::Error) {
            return Some(HirExpr::poison(span));
        }
        let (Folded::Value(left), Folded::Value(right)) = (left, right) else {
            return None;
        };
        Some(match self.combine(left, op, right, span, float) {
            Some(value) => self.type_value(value, span, expected),
            None => HirExpr::poison(span),
        })
    }

    /// Folds a subtree; float literals round to `float`, the type the whole
    /// expression takes.
    fn fold(&mut self, expr: &Expr, float: TypeId) -> Folded {
        match expr {
            Expr::Number(bytes, span) => {
                if is_float_literal(bytes) {
                    return match self.float_literal(bytes, float, *span) {
                        Some(bits) => Folded::Value(Value::Float(bits)),
                        None => Folded::Error,
                    };
                }
                match integer_magnitude(bytes) {
                    Some(magnitude) if magnitude <= u128::from(u64::MAX) => {
                        Folded::Value(Value::Int {
                            negative: false,
                            magnitude,
                        })
                    }
                    Some(_) => {
                        self.diags.emit(
                            Diagnostic::error(
                                codes::E060,
                                "integer literal does not fit in 64 bits",
                            )
                            .label(*span, true, "literal too large"),
                        );
                        Folded::Error
                    }
                    None => {
                        self.diags.emit(
                            Diagnostic::error(codes::E060, "invalid integer literal").label(
                                *span,
                                true,
                                "not a valid integer literal",
                            ),
                        );
                        Folded::Error
                    }
                }
            }
            Expr::BinOp { lhs, op, rhs, span } if is_arithmetic(*op) => {
                let left = self.fold(lhs, float);
                let right = self.fold(rhs, float);
                match (left, right) {
                    (Folded::Error, _) | (_, Folded::Error) => Folded::Error,
                    (Folded::Value(left), Folded::Value(right)) => {
                        match self.combine(left, *op, right, *span, float) {
                            Some(value) => Folded::Value(value),
                            None => Folded::Error,
                        }
                    }
                    _ => Folded::NotLiteral,
                }
            }
            _ => Folded::NotLiteral,
        }
    }

    /// Two float literal values combined as IEEE 754 does at `float`'s
    /// precision: the exact result rounded once.
    fn combine_floats(
        &mut self,
        left: FloatBits,
        op: BinOp,
        right: FloatBits,
        span: Span,
        float: TypeId,
    ) -> Option<Value> {
        let op = match op {
            BinOp::Add => FloatOp::Add,
            BinOp::Sub => FloatOp::Sub,
            BinOp::Mul => FloatOp::Mul,
            BinOp::Div => FloatOp::Div,
            _ => FloatOp::Rem,
        };
        match left.apply(op, right) {
            Ok(bits) => Some(Value::Float(bits)),
            Err(FloatError::DivisionByZero) => {
                self.diags
                    .emit(Diagnostic::error(codes::E060, "division by zero").label(
                        span,
                        true,
                        "the divisor is zero",
                    ));
                None
            }
            Err(FloatError::Overflow | FloatError::Invalid) => {
                self.diags.emit(
                    Diagnostic::error(
                        codes::E060,
                        format!("float expression does not fit in {}", self.show_id(float)),
                    )
                    .label(span, true, "expression out of range"),
                );
                None
            }
        }
    }

    /// Combines two literal values: integers at 64 bits, floats as IEEE 754
    /// does at `float`'s precision (each result rounded once).
    fn combine(
        &mut self,
        left: Value,
        op: BinOp,
        right: Value,
        span: Span,
        float: TypeId,
    ) -> Option<Value> {
        match (left, right) {
            (Value::Float(left), Value::Float(right)) => {
                self.combine_floats(left, op, right, span, float)
            }
            (
                Value::Int {
                    negative: ln,
                    magnitude: lm,
                },
                Value::Int {
                    negative: rn,
                    magnitude: rm,
                },
            ) => {
                let signed = |negative: bool, magnitude: u128| {
                    let value = i128::try_from(magnitude).unwrap_or(i128::MAX);
                    if negative {
                        -value
                    } else {
                        value
                    }
                };
                let (left, right) = (signed(ln, lm), signed(rn, rm));
                // The same `E060` lowering reports for a constant zero
                // divisor (`lilac_lower::ConstantPanic`).
                if matches!(op, BinOp::Div | BinOp::Mod) && right == 0 {
                    self.diags
                        .emit(Diagnostic::error(codes::E060, "division by zero").label(
                            span,
                            true,
                            "the divisor is zero",
                        ));
                    return None;
                }
                let result = match op {
                    BinOp::Add => left.checked_add(right),
                    BinOp::Sub => left.checked_sub(right),
                    BinOp::Mul => left.checked_mul(right),
                    BinOp::Div => left.checked_div(right),
                    _ => left.checked_rem(right),
                };
                let fits = |value: i128| {
                    let magnitude = value.unsigned_abs();
                    if value < 0 {
                        magnitude <= 1u128 << 63
                    } else {
                        magnitude <= u128::from(u64::MAX)
                    }
                };
                match result {
                    Some(value) if fits(value) => Some(Value::Int {
                        negative: value < 0,
                        magnitude: value.unsigned_abs(),
                    }),
                    _ => {
                        self.diags.emit(
                            Diagnostic::error(
                                codes::E060,
                                "integer expression does not fit in 64 bits",
                            )
                            .label(span, true, "expression too large"),
                        );
                        None
                    }
                }
            }
            // A mixed pair is float: the integer is rounded to `float` once,
            // which loses less than truncating the float would.
            (
                Value::Int {
                    negative,
                    magnitude,
                },
                Value::Float(right),
            ) => {
                let left = self.int_to_float(negative, magnitude, span, float)?;
                self.combine_floats(left, op, right, span, float)
            }
            (
                Value::Float(left),
                Value::Int {
                    negative,
                    magnitude,
                },
            ) => {
                let right = self.int_to_float(negative, magnitude, span, float)?;
                self.combine_floats(left, op, right, span, float)
            }
        }
    }

    /// An integer literal value rounded once to `float`.
    fn int_to_float(
        &mut self,
        negative: bool,
        magnitude: u128,
        span: Span,
        float: TypeId,
    ) -> Option<FloatBits> {
        let width = self.float_format(float);
        let rounded = FloatBits::parse(&magnitude.to_string(), width).and_then(|bits| {
            if negative {
                FloatBits::zero(width).apply(FloatOp::Sub, bits)
            } else {
                Ok(bits)
            }
        });
        if let Ok(bits) = rounded {
            return Some(bits);
        }
        self.diags.emit(
            Diagnostic::error(
                codes::E060,
                format!("integer does not fit in {}", self.show_id(float)),
            )
            .label(span, true, "expression out of range"),
        );
        None
    }

    /// Types a folded value by the literal rules.
    fn type_value(&mut self, value: Value, span: Span, expected: Option<TypeId>) -> HirExpr {
        match value {
            Value::Int {
                negative: true,
                magnitude,
            } => {
                let Some(fit) = signed_smallest_width(magnitude) else {
                    self.diags.emit(
                        Diagnostic::error(
                            codes::E060,
                            "integer expression does not fit in 64 bits",
                        )
                        .label(span, true, "expression too large"),
                    );
                    return HirExpr::poison(span);
                };
                match self.signed_value(magnitude, fit, span, expected) {
                    Some((ty, value)) => Self::int(ty, value, span),
                    None => HirExpr::poison(span),
                }
            }
            Value::Int {
                negative: false,
                magnitude,
            } => match self.int_literal(magnitude, span, expected) {
                Some((ty, value)) => Self::int(ty, value, span),
                None => HirExpr::poison(span),
            },
            Value::Float(bits) => {
                if self.float_in_bit_slot(expected, span, "expression") {
                    return HirExpr::poison(span);
                }
                // `fold_binop` folded at this type.
                Self::float(self.float_target(expected), bits, span)
            }
        }
    }
}
