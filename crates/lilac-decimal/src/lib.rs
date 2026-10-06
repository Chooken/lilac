//! Exact decimal-literal folding: arbitrary-precision arithmetic on the
//! literal strings of `+ - * / %` when both operands are decimal number
//! literals (SPEC §2).
mod bigint;
mod value;

pub use value::eval;

/// A binary arithmetic operator that can be folded on decimal literals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

/// How a division result is rounded to [`Policy::scale`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Round {
    Trunc,
    HalfUp,
}

/// The evaluation policy for a folded operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub scale: u32,
    pub round: Round,
}

/// Errors produced while evaluating a folded decimal operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecimalError {
    DivideByZero,
    ModuloByZero,
    Malformed,
}

/// Whether `text` is a decimal number literal.
///
/// Accepts `-?[0-9][0-9_]*(\.[0-9][0-9_]*)?` (the leading `-` is present on
/// folded results) and rejects `0x`/`0X`/`0b`/`0B` prefixed forms.
#[must_use]
pub fn is_decimal_literal(text: &str) -> bool {
    if text.starts_with("0x")
        || text.starts_with("0X")
        || text.starts_with("0b")
        || text.starts_with("0B")
    {
        return false;
    }

    let body = text.strip_prefix('-').unwrap_or(text);
    let mut parts = body.split('.');
    let int_part = parts.next().unwrap_or("");
    match parts.next() {
        Some(frac_part) => parts.next().is_none() && digit_run(int_part) && digit_run(frac_part),
        None => digit_run(int_part),
    }
}

fn digit_run(part: &str) -> bool {
    let mut bytes = part.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_digit())
        && bytes.all(|b| b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::is_decimal_literal;

    #[test]
    fn is_decimal_literal_accepts_decimals() {
        for t in ["5", "10_000", "10.5", "-1", "0.5"] {
            assert!(is_decimal_literal(t), "{t}");
        }
    }

    #[test]
    fn is_decimal_literal_rejects_hex_and_bin() {
        for t in ["0x1", "0X1", "0b1", "0B1"] {
            assert!(!is_decimal_literal(t), "{t}");
        }
    }

    #[test]
    fn is_decimal_literal_rejects_malformed() {
        for t in ["", "-", "1.2.3", "1..2", "1.", ".5", "1a"] {
            assert!(!is_decimal_literal(t), "{t}");
        }
    }
}
