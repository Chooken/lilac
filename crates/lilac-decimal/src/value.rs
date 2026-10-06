#![allow(dead_code)]

use crate::bigint::Big;

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
}

#[cfg(test)]
mod tests {
    use super::Decimal;

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
