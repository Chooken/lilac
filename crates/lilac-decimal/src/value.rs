use std::cmp::Ordering;

use crate::bigint::Big;
use crate::{DecimalError, Op, Policy, Round};

/// A parsed decimal literal: a sign, an unsigned integer coefficient, and the
/// number of fractional digits of that coefficient.
///
/// Invariants after [`Decimal::parse`]: `digits` carries no trailing zeros in
/// its fractional part, `scale` counts the remaining fractional digits, and a
/// zero value has `negative == false` and `scale == 0`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Decimal {
    negative: bool,
    digits: Big,
    scale: u32,
}

impl Decimal {
    /// Parse the decimal grammar `-?[0-9][0-9_]*(\.[0-9][0-9_]*)?` into a
    /// normalized value. Returns `None` for any malformed input.
    pub(crate) fn parse(text: &str) -> Option<Decimal> {
        let cleaned: String = text.chars().filter(|&c| c != '_').collect();
        let (negative, body) = match cleaned.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, cleaned.as_str()),
        };

        let mut parts = body.split('.');
        let int_part = parts.next().unwrap_or("");
        let frac_part = parts.next();
        if parts.next().is_some() {
            return None;
        }
        if int_part.is_empty() || !int_part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let frac = frac_part.unwrap_or("");
        if frac_part.is_some() && (frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit())) {
            return None;
        }

        let mut digits = Big::from_decimal_digits(&format!("{int_part}{frac}"))?;
        let mut scale = u32::try_from(frac.len()).ok()?;
        let trailing_zeros = frac.bytes().rev().take_while(|&b| b == b'0').count();
        if trailing_zeros > 0 {
            let shift = u32::try_from(trailing_zeros).ok()?;
            let divisor = Big::from_decimal_digits("1")?.mul_pow10(shift);
            let (quotient, _) = digits.divmod(&divisor)?;
            digits = quotient;
            scale -= shift;
        }

        if digits.is_zero() {
            Some(Decimal {
                negative: false,
                digits,
                scale: 0,
            })
        } else {
            Some(Decimal {
                negative,
                digits,
                scale,
            })
        }
    }

    /// Render in canonical form: no leading zeros, no trailing fractional
    /// zeros, no bare `.`, no `-0`.
    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        if self.negative && !self.digits.is_zero() {
            out.push('-');
        }
        let digits = self.digits.to_decimal_string();
        if self.scale == 0 {
            out.push_str(&digits);
            return out;
        }

        let scale = self.scale as usize;
        let width = scale + 1;
        let mut buffer = String::with_capacity(digits.len().max(width));
        for _ in digits.len()..width {
            buffer.push('0');
        }
        buffer.push_str(&digits);

        let split = buffer.len() - scale;
        let (int_part, frac_part) = buffer.split_at(split);
        out.push_str(int_part);
        let frac_part = frac_part.trim_end_matches('0');
        if !frac_part.is_empty() {
            out.push('.');
            out.push_str(frac_part);
        }
        out
    }

    /// Form a normalized value: a zero result is always `0` (no sign, no
    /// scale), matching [`Decimal::parse`]'s invariants.
    fn normalized(negative: bool, digits: Big, scale: u32) -> Decimal {
        if digits.is_zero() {
            Decimal {
                negative: false,
                digits,
                scale: 0,
            }
        } else {
            Decimal {
                negative,
                digits,
                scale,
            }
        }
    }
}

/// Fold a binary arithmetic operator over two decimal-literal strings.
///
/// `+ - *` are exact; `/` is computed to `policy.scale` fractional digits and
/// rounded per `policy.round`; `%` is floored (`a % b = a - b·floor(a/b)`),
/// takes the divisor's sign, and is exact. Malformed operands yield
/// [`DecimalError::Malformed`].
pub fn eval(op: Op, lhs: &str, rhs: &str, policy: Policy) -> Result<String, DecimalError> {
    let a = Decimal::parse(lhs).ok_or(DecimalError::Malformed)?;
    let b = Decimal::parse(rhs).ok_or(DecimalError::Malformed)?;
    let result = match op {
        Op::Add => add_sub(&a, &b, false),
        Op::Sub => add_sub(&a, &b, true),
        Op::Mul => mul(&a, &b)?,
        Op::Div => div(&a, &b, policy)?,
        Op::Mod => modulo(&a, &b)?,
    };
    Ok(result.render())
}

fn add_sub(a: &Decimal, b: &Decimal, subtract: bool) -> Decimal {
    let scale = a.scale.max(b.scale);
    let left = a.digits.mul_pow10(scale - a.scale);
    let right = b.digits.mul_pow10(scale - b.scale);
    let right_negative = b.negative ^ subtract;
    let (negative, digits) = signed_add(a.negative, &left, right_negative, &right);
    Decimal::normalized(negative, digits, scale)
}

fn mul(a: &Decimal, b: &Decimal) -> Result<Decimal, DecimalError> {
    let scale = a
        .scale
        .checked_add(b.scale)
        .ok_or(DecimalError::Malformed)?;
    Ok(Decimal::normalized(
        a.negative ^ b.negative,
        a.digits.mul(&b.digits),
        scale,
    ))
}

fn div(a: &Decimal, b: &Decimal, policy: Policy) -> Result<Decimal, DecimalError> {
    if b.digits.is_zero() {
        return Err(DecimalError::DivideByZero);
    }
    let scale = policy.scale;
    let shift = b.scale.checked_add(scale).ok_or(DecimalError::Malformed)?;
    let numerator = a.digits.mul_pow10(shift);
    let denominator = b.digits.mul_pow10(a.scale);
    let (mut quotient, remainder) = numerator
        .divmod(&denominator)
        .ok_or(DecimalError::DivideByZero)?;
    if policy.round == Round::HalfUp
        && remainder.add(&remainder).cmp(&denominator) != Ordering::Less
    {
        quotient = quotient.add(&one());
    }
    Ok(Decimal::normalized(
        a.negative ^ b.negative,
        quotient,
        scale,
    ))
}

fn modulo(a: &Decimal, b: &Decimal) -> Result<Decimal, DecimalError> {
    if b.digits.is_zero() {
        return Err(DecimalError::ModuloByZero);
    }
    // Magnitude quotient of `a / b`: floor(|a| / |b|) and its remainder.
    let numerator = a.digits.mul_pow10(b.scale);
    let denominator = b.digits.mul_pow10(a.scale);
    let (quotient_mag, remainder) = numerator
        .divmod(&denominator)
        .ok_or(DecimalError::ModuloByZero)?;
    // Floored division: when the signs differ and the division is inexact,
    // the signed quotient is one below the truncated magnitude quotient.
    let opposite = a.negative ^ b.negative;
    let (quotient_mag, quotient_negative) = if opposite && !remainder.is_zero() {
        (quotient_mag.add(&one()), true)
    } else {
        (quotient_mag, opposite)
    };
    // `r = a - b·q`, evaluated exactly at the operands' common scale.
    let scale = a.scale.max(b.scale);
    let left = a.digits.mul_pow10(scale - a.scale);
    let product = b.digits.mul_pow10(scale - b.scale).mul(&quotient_mag);
    let product_negative = b.negative ^ quotient_negative;
    let (negative, digits) = signed_add(a.negative, &left, !product_negative, &product);
    Ok(Decimal::normalized(negative, digits, scale))
}

/// Signed magnitude addition: `left ± right`, where the returned sign applies
/// to the returned magnitude. A zero result carries no sign.
fn signed_add(left_negative: bool, left: &Big, right_negative: bool, right: &Big) -> (bool, Big) {
    if left_negative == right_negative {
        (left_negative, left.add(right))
    } else {
        match left.cmp(right) {
            Ordering::Equal => (false, Big::zero()),
            Ordering::Greater => (left_negative, left.sub(right)),
            Ordering::Less => (right_negative, right.sub(left)),
        }
    }
}

fn one() -> Big {
    Big::from_decimal_digits("1").expect("`1` is valid decimal digits")
}

#[cfg(test)]
mod tests {
    use super::{eval, Decimal};
    use crate::{DecimalError, Op, Policy, Round};

    fn ev(op: Op, l: &str, r: &str, scale: u32, round: Round) -> String {
        eval(op, l, r, Policy { scale, round }).expect("eval ok")
    }
    fn trunc(scale: u32) -> Policy {
        Policy {
            scale,
            round: Round::Trunc,
        }
    }

    #[test]
    fn add_exact() {
        assert_eq!(ev(Op::Add, "1.5", "2.25", 10, Round::Trunc), "3.75");
    }

    #[test]
    fn sub_yields_negative() {
        assert_eq!(ev(Op::Sub, "2", "3", 10, Round::Trunc), "-1");
    }

    #[test]
    fn mul_exact() {
        assert_eq!(ev(Op::Mul, "10.5", "2", 10, Round::Trunc), "21");
    }

    #[test]
    fn div_trunc_default_scale() {
        assert_eq!(ev(Op::Div, "1", "3", 10, Round::Trunc), "0.3333333333");
    }

    #[test]
    fn div_exact_has_no_trailing_zeros() {
        assert_eq!(ev(Op::Div, "6", "3", 10, Round::Trunc), "2");
        assert_eq!(ev(Op::Div, "1", "2", 10, Round::Trunc), "0.5");
    }

    #[test]
    fn div_trunc_toward_zero_for_negatives() {
        // Truncation toward zero, and never a negative zero.
        assert_eq!(ev(Op::Div, "-1", "3", 0, Round::Trunc), "0");
        assert_eq!(ev(Op::Div, "-3", "2", 0, Round::Trunc), "-1"); // toward zero, not -2
    }

    #[test]
    fn div_half_up_away_from_zero() {
        assert_eq!(ev(Op::Div, "1", "8", 2, Round::HalfUp), "0.13");
        assert_eq!(ev(Op::Div, "-1", "8", 2, Round::HalfUp), "-0.13");
        assert_eq!(ev(Op::Div, "1", "3", 2, Round::HalfUp), "0.33");
    }

    #[test]
    fn div_scale_zero() {
        assert_eq!(ev(Op::Div, "7", "2", 0, Round::Trunc), "3");
    }

    #[test]
    fn mod_floored_positive_divisor() {
        assert_eq!(ev(Op::Mod, "-7", "3", 10, Round::Trunc), "2");
    }

    #[test]
    fn mod_floored_negative_divisor() {
        assert_eq!(ev(Op::Mod, "7", "-3", 10, Round::Trunc), "-2");
    }

    #[test]
    fn divide_by_zero_is_error() {
        assert_eq!(
            eval(Op::Div, "1", "0", trunc(10)),
            Err(DecimalError::DivideByZero)
        );
    }

    #[test]
    fn modulo_by_zero_is_error() {
        assert_eq!(
            eval(Op::Mod, "1", "0", trunc(10)),
            Err(DecimalError::ModuloByZero)
        );
    }

    #[test]
    fn large_operands_are_exact() {
        // (10^100 - 1)^2 = 99 nines, an 8, 99 zeros, then a 1.
        let big = "9".repeat(100);
        let want = format!("{}8{}1", "9".repeat(99), "0".repeat(99));
        assert_eq!(ev(Op::Mul, &big, &big, 10, Round::Trunc), want);
    }

    #[test]
    fn underscored_operands() {
        assert_eq!(ev(Op::Add, "10_000", "1", 10, Round::Trunc), "10001");
    }

    #[test]
    fn render_canonical() {
        for (input, want) in [
            ("2.50", "2.5"),
            ("6.0", "6"),
            ("007", "7"),
            ("-0", "0"),
            ("0.500", "0.5"),
            ("0.0", "0"),
        ] {
            assert_eq!(
                Decimal::parse(input).expect("parse").render(),
                want,
                "{input}"
            );
        }
    }

    #[test]
    fn parse_strips_underscores() {
        assert_eq!(Decimal::parse("1_0.0_1").expect("parse").render(), "10.01");
    }

    #[test]
    fn parse_render_round_trips_huge_literal() {
        let digits = "9".repeat(200);
        assert_eq!(Decimal::parse(&digits).expect("parse").render(), digits);
    }
}
