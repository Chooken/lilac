//! Number literals: typing (smallest fit, grown to the expected width) and
//! compile-time folding of all-literal arithmetic (SPEC §2).

use lilac_ast::{BinOp, Expr};
use lilac_error::{codes, Diagnostic};
use lilac_hir::{HirExpr, HirExprKind};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeId, TypeRef};

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

/// Largest finite value of an `@fbitN` (16/32/64).
fn max_finite_float(width: u32) -> f64 {
    match width {
        16 => 65504.0,
        32 => f64::from(f32::MAX),
        _ => f64::MAX,
    }
}

fn parse_float(bytes: &str) -> f64 {
    let cleaned: String = bytes.chars().filter(|c| *c != '_').collect();
    cleaned.parse::<f64>().unwrap_or(f64::NAN)
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
    Int { negative: bool, magnitude: u128 },
    Float(f64),
}

/// The result of folding a subtree.
enum Folded {
    NotLiteral,
    Value(Value),
    Error,
}

impl Checker<'_> {
    fn number(ty: TypeId, bytes: String, span: Span) -> HirExpr {
        HirExpr {
            ty: TypeRef {
                id: ty,
                is_ref: false,
            },
            span,
            kind: HirExprKind::Number(bytes),
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

    /// Types a number literal under `expected`.
    pub(crate) fn check_number(
        &mut self,
        bytes: &str,
        span: Span,
        expected: Option<TypeId>,
    ) -> HirExpr {
        if is_float_literal(bytes) {
            return self.type_float(parse_float(bytes), Some(bytes), span, expected);
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
        let Some(ty) = self.int_literal_ty(significant_bits(magnitude), span, expected) else {
            return HirExpr::poison(span);
        };
        Self::number(ty, bytes.to_owned(), span)
    }

    /// The bit type an unsigned integer of `bits` bits takes under `expected`.
    fn int_literal_ty(
        &mut self,
        bits: u32,
        span: Span,
        expected: Option<TypeId>,
    ) -> Option<TypeId> {
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
                            self.types.display(ty)
                        ),
                    )
                    .label(span, true, "literal overflows"),
                );
                return None;
            }
            return Some(ty);
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
        Some(Self::bit_of_width(width))
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
        if is_float_literal(bytes) {
            return None;
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
                Some((ty, bytes)) => Self::number(ty, bytes, span),
                None => HirExpr::poison(span),
            },
        )
    }

    /// The bit type and two's-complement bits of `-magnitude`, grown to the
    /// expected width.
    fn signed_value(
        &mut self,
        magnitude: u128,
        fit: u32,
        span: Span,
        expected: Option<TypeId>,
    ) -> Option<(TypeId, String)> {
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
                            self.types.display(ty)
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
        Some((ty, format!("0x{value:X}")))
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
        let left = self.fold(lhs);
        let right = self.fold(rhs);
        if matches!(left, Folded::Error) || matches!(right, Folded::Error) {
            return Some(HirExpr::poison(span));
        }
        let (Folded::Value(left), Folded::Value(right)) = (left, right) else {
            return None;
        };
        Some(match self.combine(left, op, right, span) {
            Some(value) => self.type_value(value, span, expected),
            None => HirExpr::poison(span),
        })
    }

    fn fold(&mut self, expr: &Expr) -> Folded {
        match expr {
            Expr::Number(bytes, span) => {
                if is_float_literal(bytes) {
                    return Folded::Value(Value::Float(parse_float(bytes)));
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
                let left = self.fold(lhs);
                let right = self.fold(rhs);
                match (left, right) {
                    (Folded::Error, _) | (_, Folded::Error) => Folded::Error,
                    (Folded::Value(left), Folded::Value(right)) => {
                        match self.combine(left, *op, right, *span) {
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

    /// Combines two literal values at 64 bits.
    fn combine(&mut self, left: Value, op: BinOp, right: Value, span: Span) -> Option<Value> {
        match (left, right) {
            (Value::Float(left), Value::Float(right)) => {
                let value = match op {
                    BinOp::Add => left + right,
                    BinOp::Sub => left - right,
                    BinOp::Mul => left * right,
                    BinOp::Div => left / right,
                    _ => left % right,
                };
                if value.is_finite() {
                    return Some(Value::Float(value));
                }
                self.diags.emit(
                    Diagnostic::error(codes::E060, "float expression does not fit in @fbit64")
                        .label(span, true, "expression out of range"),
                );
                None
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
                // divisor (`lilac_lower::DivisionPanic`).
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
            _ => {
                self.diags.emit(
                    Diagnostic::error(
                        codes::E010,
                        "mismatched types: cannot mix an integer and a float literal",
                    )
                    .label(span, true, "mixed int/float operands"),
                );
                None
            }
        }
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
                    Some((ty, bytes)) => Self::number(ty, bytes, span),
                    None => HirExpr::poison(span),
                }
            }
            Value::Int {
                negative: false,
                magnitude,
            } => match self.int_literal_ty(significant_bits(magnitude), span, expected) {
                Some(ty) => Self::number(ty, format!("0x{magnitude:X}"), span),
                None => HirExpr::poison(span),
            },
            Value::Float(value) => self.type_float(value, None, span, expected),
        }
    }

    /// Types a float value: `@fbit64` by default, pinned by an expected
    /// `@fbitN` (or a conversion source); `E010` in a bit slot or `@fbit128`;
    /// `E060` past the target's largest finite value. `source` is the literal
    /// text (kept verbatim) or `None` for a folded value.
    fn type_float(
        &mut self,
        value: f64,
        source: Option<&str>,
        span: Span,
        expected: Option<TypeId>,
    ) -> HirExpr {
        let what = if source.is_some() {
            "literal"
        } else {
            "expression"
        };
        if let Some(ty) = expected {
            if self.is_bit(ty) {
                self.diags.emit(
                    Diagnostic::error(
                        codes::E010,
                        "mismatched types: expected an integer, found a float",
                    )
                    .label(span, true, format!("float {what} here")),
                );
                return HirExpr::poison(span);
            }
            if self.is_float(ty) && self.float_width(ty) == 128 {
                self.diags.emit(
                    Diagnostic::error(codes::E010, "`@fbit128` literals are not supported yet")
                        .label(span, true, "only 16/32/64-bit float literals can be typed"),
                );
                return HirExpr::poison(span);
            }
        }
        let fbit64 = Builtin::FBit64.id();
        let target = match expected {
            Some(ty) if self.is_float(ty) => ty,
            Some(ty) => self.conversion_source(ty, true).unwrap_or(fbit64),
            None => fbit64,
        };
        let width = self.float_width(target);
        if !value.is_finite() || value.abs() > max_finite_float(width) {
            let label = if source.is_some() {
                "literal out of range"
            } else {
                "expression out of range"
            };
            self.diags.emit(
                Diagnostic::error(
                    codes::E060,
                    format!(
                        "float {what} does not fit in {}",
                        self.types.display(target)
                    ),
                )
                .label(span, true, label),
            );
            return HirExpr::poison(span);
        }
        let bytes = match source {
            Some(text) => text.chars().filter(|c| *c != '_').collect(),
            None => format!("{value:?}"),
        };
        Self::number(target, bytes, span)
    }
}
