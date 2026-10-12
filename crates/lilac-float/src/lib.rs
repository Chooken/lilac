//! IEEE 754 binary floats as the compiler holds them: the exact bit pattern
//! of a value in one of the four interchange formats Lilac has (`@fbit16`,
//! `@fbit32`, `@fbit64`, `@fbit128`), the one conversion from a decimal
//! literal to it, and the arithmetic typeck folds literals with.
//!
//! A literal is rounded once, to nearest with ties to even, from its exact
//! decimal value: `binary32` and `binary64` through Rust's correctly rounded
//! `str::parse`, `binary16` and `binary128` through [`round`], an exact
//! converter over big integers (a rational `num / den` compared exactly).
//! Subnormals come out as IEEE defines them, a nonzero value too small for
//! the smallest subnormal rounds to zero, and a value that rounds past the
//! largest finite one is [`FloatError::Overflow`] (there are no infinite
//! literals). The converter bounds its work by the literal's length: an
//! exponent far outside the format (`1e999999`) is decided from the digit
//! count alone, before any big number is built.
//!
//! [`FloatBits::apply`] is IEEE 754 arithmetic at one width: the exact
//! result of `+ - * /`, rounded once by the same [`round`], and `%` as C's
//! `fmod` (LLVM's `frem`), which is exact. So a folded literal expression
//! has the bits the same operations give at run time.
//!
//! Std-only and LLVM-free: typeck, the HIR, the LIR and every backend share
//! [`FloatBits`].

mod big;
#[cfg(test)]
mod tests;

use std::fmt;

use big::Big;

/// The width of an IEEE 754 binary interchange format.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FloatWidth {
    /// `binary16` (`@fbit16`).
    W16,
    /// `binary32` (`@fbit32`).
    W32,
    /// `binary64` (`@fbit64`).
    W64,
    /// `binary128` (`@fbit128`).
    W128,
}

impl FloatWidth {
    /// The format of `bits` bits (16, 32, 64 or 128).
    #[must_use]
    pub fn from_bits(bits: u32) -> Option<Self> {
        Some(match bits {
            16 => Self::W16,
            32 => Self::W32,
            64 => Self::W64,
            128 => Self::W128,
            _ => return None,
        })
    }

    /// The width in bits.
    #[must_use]
    pub fn bits(self) -> u32 {
        match self {
            Self::W16 => 16,
            Self::W32 => 32,
            Self::W64 => 64,
            Self::W128 => 128,
        }
    }

    /// The format's parameters.
    fn format(self) -> Format {
        match self {
            Self::W16 => Format {
                exponent_bits: 5,
                precision: 11,
            },
            Self::W32 => Format {
                exponent_bits: 8,
                precision: 24,
            },
            Self::W64 => Format {
                exponent_bits: 11,
                precision: 53,
            },
            Self::W128 => Format {
                exponent_bits: 15,
                precision: 113,
            },
        }
    }
}

/// The exact bit pattern of a float of one width.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FloatBits {
    /// A `binary16` pattern.
    F16(u16),
    /// A `binary32` pattern.
    F32(u32),
    /// A `binary64` pattern.
    F64(u64),
    /// A `binary128` pattern.
    F128(u128),
}

/// Why a literal or an operation has no finite bit pattern.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FloatError {
    /// The text is not a decimal float literal.
    Invalid,
    /// The value rounds past the largest finite value of the width (or is
    /// not finite).
    Overflow,
    /// A division or remainder by zero.
    DivisionByZero,
}

impl FloatBits {
    /// Positive zero of `width`.
    #[must_use]
    pub fn zero(width: FloatWidth) -> Self {
        pack(width, 0)
    }

    /// Negative zero of `width`: `-0.0 - x` is IEEE negation (a sign flip),
    /// where `0.0 - x` gives `+0.0` for `x = +0.0`.
    #[must_use]
    pub fn negative_zero(width: FloatWidth) -> Self {
        pack(width, 1 << (width.bits() - 1))
    }

    /// The width of the pattern.
    #[must_use]
    pub fn width(self) -> FloatWidth {
        match self {
            Self::F16(_) => FloatWidth::W16,
            Self::F32(_) => FloatWidth::W32,
            Self::F64(_) => FloatWidth::W64,
            Self::F128(_) => FloatWidth::W128,
        }
    }

    /// The pattern, zero-extended.
    #[must_use]
    pub fn to_bits(self) -> u128 {
        match self {
            Self::F16(bits) => u128::from(bits),
            Self::F32(bits) => u128::from(bits),
            Self::F64(bits) => u128::from(bits),
            Self::F128(bits) => bits,
        }
    }

    /// The literal `text` (`3.14`, `1e3`, `1.5e-4`, `_` separators allowed)
    /// rounded once to `width`.
    pub fn parse(text: &str, width: FloatWidth) -> Result<Self, FloatError> {
        let decimal = Decimal::parse(text).ok_or(FloatError::Invalid)?;
        match width {
            FloatWidth::W32 => {
                let value: f32 = decimal.text.parse().map_err(|_| FloatError::Invalid)?;
                finite(value.is_finite(), Self::F32(value.to_bits()))
            }
            FloatWidth::W64 => {
                let value: f64 = decimal.text.parse().map_err(|_| FloatError::Invalid)?;
                finite(value.is_finite(), Self::F64(value.to_bits()))
            }
            FloatWidth::W16 | FloatWidth::W128 => decimal.round(width),
        }
    }

    /// `self op rhs` as IEEE 754 computes it at `self`'s width, in the
    /// default rounding mode: the exact result rounded once, to nearest with
    /// ties to even (`rhs` is taken at its exact value, whatever its width).
    /// `Rem` is C's `fmod`: `self - n * rhs` for `n` the quotient truncated
    /// toward zero, exact, with `self`'s sign. A zero sum of opposite signs
    /// is `+0`. A result past the largest finite value is
    /// [`FloatError::Overflow`], and so is an operand that is not finite; a
    /// zero divisor is [`FloatError::DivisionByZero`] (IEEE's infinity or
    /// NaN).
    pub fn apply(self, op: FloatOp, rhs: Self) -> Result<Self, FloatError> {
        let width = self.width();
        let (Some(left), Some(right)) = (self.decode(), rhs.decode()) else {
            return Err(FloatError::Overflow);
        };
        match op {
            FloatOp::Add => Ok(sum(left, right, width)?),
            FloatOp::Sub => Ok(sum(
                left,
                Exact {
                    negative: !right.negative,
                    ..right
                },
                width,
            )?),
            FloatOp::Mul => {
                let product =
                    Big::from_u128(left.significand).mul(&Big::from_u128(right.significand));
                let exponent = left.exponent + right.exponent;
                round_scaled(left.negative != right.negative, &product, exponent, width)
            }
            FloatOp::Div => {
                if right.significand == 0 {
                    return Err(FloatError::DivisionByZero);
                }
                // `(a * 2^ea) / (b * 2^eb)` is `a / (b * 2^(eb - ea))`.
                let negative = left.negative != right.negative;
                let (num, den) = scale(
                    &Big::from_u128(left.significand),
                    &Big::from_u128(right.significand),
                    right.exponent - left.exponent,
                );
                round(negative, &num, &den, width)
            }
            FloatOp::Rem => {
                if right.significand == 0 {
                    return Err(FloatError::DivisionByZero);
                }
                let (remainder, exponent) = remainder(left, right);
                round_scaled(left.negative, &Big::from_u128(remainder), exponent, width)
            }
        }
    }

    /// The finite value as `significand * 2^exponent`, signed; `None` for an
    /// infinity or a NaN.
    fn decode(self) -> Option<Exact> {
        let format = self.width().format();
        let bits = self.to_bits();
        let stored = format.precision - 1;
        let exponent_mask = (1u128 << format.exponent_bits) - 1;
        let biased = (bits >> stored) & exponent_mask;
        if biased == exponent_mask {
            return None;
        }
        let fraction = bits & ((1u128 << stored) - 1);
        let negative = (bits >> (self.width().bits() - 1)) & 1 == 1;
        let lowest = format.min_exponent() - (format.precision() - 1);
        let (significand, exponent) = if biased == 0 {
            (fraction, lowest)
        } else {
            // The biased exponent is at most 15 bits.
            let biased = i64::try_from(biased).ok()?;
            (fraction | 1 << stored, lowest + biased - 1)
        };
        Some(Exact {
            negative,
            significand,
            exponent,
        })
    }
}

/// An IEEE 754 arithmetic operation ([`FloatBits::apply`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FloatOp {
    /// `+`.
    Add,
    /// `-`.
    Sub,
    /// `*`.
    Mul,
    /// `/`.
    Div,
    /// `%`, C's `fmod`.
    Rem,
}

/// A finite value, `(-1)^negative * significand * 2^exponent`.
#[derive(Clone, Copy)]
struct Exact {
    negative: bool,
    significand: u128,
    exponent: i64,
}

/// `left + right` rounded once to `width`.
fn sum(left: Exact, right: Exact, width: FloatWidth) -> Result<FloatBits, FloatError> {
    // Both over the smaller exponent: integers, so the sum is exact.
    let low = left.exponent.min(right.exponent);
    let widen = |value: Exact| {
        let shift = usize::try_from(value.exponent - low).unwrap_or(usize::MAX);
        Big::from_u128(value.significand).shl(shift)
    };
    let (a, b) = (widen(left), widen(right));
    let (negative, magnitude) = if left.negative == right.negative {
        (left.negative, a.add(&b))
    } else {
        match a.cmp(&b) {
            std::cmp::Ordering::Greater => (left.negative, difference(a, &b)),
            std::cmp::Ordering::Less => (right.negative, difference(b, &a)),
            // Round to nearest: an exact zero sum of opposite signs is `+0`.
            std::cmp::Ordering::Equal => (false, Big::from_u64(0)),
        }
    };
    round_scaled(negative, &magnitude, low, width)
}

/// `a - b`, for `a >= b`.
fn difference(mut a: Big, b: &Big) -> Big {
    a.sub_assign(b);
    a
}

/// `fmod(left, right)`'s magnitude as `remainder * 2^exponent` (`right` is
/// nonzero). Over the smaller exponent both are integers, and one of them
/// is the significand itself, so the work stays in `u128`.
fn remainder(left: Exact, right: Exact) -> (u128, i64) {
    if left.exponent >= right.exponent {
        // `(a * 2^k) mod b`, one doubling at a time: `2r < 2b < 2^114`.
        let mut rest = left.significand % right.significand;
        for _ in 0..left.exponent - right.exponent {
            rest <<= 1;
            if rest >= right.significand {
                rest -= right.significand;
            }
        }
        (rest, right.exponent)
    } else {
        // `a mod (b * 2^k)`: `b * 2^k` past `u128` is past `a`, which stays.
        let shift = u32::try_from(right.exponent - left.exponent).unwrap_or(u32::MAX);
        let divisor = if shift <= right.significand.leading_zeros() {
            right.significand << shift
        } else {
            0
        };
        if divisor == 0 {
            (left.significand, left.exponent)
        } else {
            (left.significand % divisor, left.exponent)
        }
    }
}

/// `magnitude * 2^exponent`, negated when `negative`, rounded once to
/// `width`.
fn round_scaled(
    negative: bool,
    magnitude: &Big,
    exponent: i64,
    width: FloatWidth,
) -> Result<FloatBits, FloatError> {
    let (num, den) = scale(magnitude, &Big::from_u64(1), -exponent);
    round(negative, &num, &den, width)
}

/// `Ok(bits)` when the parsed value is finite.
fn finite(is_finite: bool, bits: FloatBits) -> Result<FloatBits, FloatError> {
    if is_finite {
        Ok(bits)
    } else {
        Err(FloatError::Overflow)
    }
}

/// The canonical text: `binary32` and `binary64` as the shortest decimal
/// that reads back as the same value (`5.25`, `1000.0`, `1e20`); `binary16`
/// and `binary128` as their bit pattern in fixed-width uppercase hex after
/// `0xH` and `0xQ` (`0xH3C00`, `0xQ3FFF0000000000000000000000000000`).
impl fmt::Display for FloatBits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::F16(bits) => write!(f, "0xH{bits:04X}"),
            Self::F32(bits) => write!(f, "{:?}", f32::from_bits(bits)),
            Self::F64(bits) => write!(f, "{:?}", f64::from_bits(bits)),
            Self::F128(bits) => write!(f, "0xQ{bits:032X}"),
        }
    }
}

/// The parameters of a binary interchange format.
#[derive(Clone, Copy)]
struct Format {
    exponent_bits: u32,
    /// Significand bits, the implicit leading one included.
    precision: usize,
}

impl Format {
    fn bias(self) -> i64 {
        (1 << (self.exponent_bits - 1)) - 1
    }

    /// The exponent of the smallest normal value (`2^min_exponent`).
    fn min_exponent(self) -> i64 {
        1 - self.bias()
    }

    /// The exponent of the largest finite value's leading bit.
    fn max_exponent(self) -> i64 {
        self.bias()
    }

    fn precision(self) -> i64 {
        to_i64(self.precision)
    }
}

/// A validated decimal literal: `digits * 10^exponent`, the digits without
/// leading or trailing zeros (none for zero).
struct Decimal {
    digits: Vec<u8>,
    exponent: i64,
    /// The literal without separators, for Rust's parser.
    text: String,
}

/// Largest decimal exponent kept exactly; larger ones saturate here, which
/// is far past every format's range, so nothing they decide changes.
const EXPONENT_CAP: i64 = 1 << 40;

impl Decimal {
    /// Reads `digits [. digits] [(e|E) [+|-] digits]`, `_` anywhere
    /// among them (the lexer has already placed them).
    fn parse(text: &str) -> Option<Self> {
        let cleaned: String = text.chars().filter(|&ch| ch != '_').collect();
        let bytes = cleaned.as_bytes();
        let digit_run = |from: usize| {
            bytes[from..]
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count()
        };
        let whole = digit_run(0);
        if whole == 0 {
            return None;
        }
        let mut at = whole;
        let mut fraction = 0;
        if bytes.get(at) == Some(&b'.') {
            fraction = digit_run(at + 1);
            if fraction == 0 {
                return None;
            }
            at += 1 + fraction;
        }
        let mut written = 0i64;
        if matches!(bytes.get(at), Some(b'e' | b'E')) {
            at += 1;
            let negative = match bytes.get(at) {
                Some(b'-') => {
                    at += 1;
                    true
                }
                Some(b'+') => {
                    at += 1;
                    false
                }
                _ => false,
            };
            let count = digit_run(at);
            if count == 0 {
                return None;
            }
            for digit in &bytes[at..at + count] {
                written = (written * 10 + i64::from(digit - b'0')).min(EXPONENT_CAP);
            }
            if negative {
                written = -written;
            }
            at += count;
        }
        if at != bytes.len() {
            return None;
        }
        let mut digits: Vec<u8> = bytes[..whole].to_vec();
        if fraction > 0 {
            digits.extend_from_slice(&bytes[whole + 1..whole + 1 + fraction]);
        }
        let mut exponent = written - i64::try_from(fraction).ok()?;
        let leading = digits.iter().take_while(|&&digit| digit == b'0').count();
        digits.drain(..leading);
        while digits.last() == Some(&b'0') {
            digits.pop();
            exponent += 1;
        }
        Some(Self {
            digits,
            exponent,
            text: cleaned,
        })
    }

    /// The value rounded once to `width` by the exact converter.
    fn round(&self, width: FloatWidth) -> Result<FloatBits, FloatError> {
        if self.digits.is_empty() {
            return Ok(FloatBits::zero(width));
        }
        let format = width.format();
        // The value is in `[10^(magnitude - 1), 10^magnitude)`.
        let count = i64::try_from(self.digits.len()).map_err(|_| FloatError::Overflow)?;
        let magnitude = count + self.exponent;
        // `log10(2) < 0.30103`, so these bounds sit just outside the format:
        // past `high` the value is at least `2^(max_exponent + 1)`, below
        // `low` it is under half the smallest subnormal.
        let high = ((format.max_exponent() + 1) * 30103 + 99_999) / 100_000 + 1;
        let low = -((format.precision() - format.min_exponent()) * 30103 / 100_000) - 2;
        if magnitude - 1 > high {
            return Err(FloatError::Overflow);
        }
        if magnitude < low {
            return Ok(FloatBits::zero(width));
        }
        // Within the bounds `|exponent|` is at most the digit count plus a
        // few thousand, so the powers of ten stay small.
        let mut num = Big::from_decimal(&self.digits);
        let scale =
            usize::try_from(self.exponent.unsigned_abs()).map_err(|_| FloatError::Overflow)?;
        if self.exponent >= 0 {
            num.mul_pow10(scale);
            round(false, &num, &Big::from_u64(1), width)
        } else {
            round(false, &num, &Big::pow10(scale), width)
        }
    }
}

/// `num / den` (both nonzero, or `num` zero) rounded once, to nearest with
/// ties to even, to `width`, negated when `negative`.
fn round(negative: bool, num: &Big, den: &Big, width: FloatWidth) -> Result<FloatBits, FloatError> {
    let format = width.format();
    let sign = u128::from(negative) << (width.bits() - 1);
    if num.is_zero() {
        return Ok(pack(width, sign));
    }
    let precision = format.precision();
    // `2^(k - 1) < num / den < 2^(k + 1)`; one comparison picks the floor
    // of its base-2 logarithm.
    let k = to_i64(num.bit_len()) - to_i64(den.bit_len());
    let (scaled_num, scaled_den) = scale(num, den, k);
    let log2 = if scaled_num >= scaled_den { k } else { k - 1 };
    // `num / den = q * 2^exponent`, with `q` below `2^precision`; below
    // the normal range the exponent stays at the subnormals' one.
    let lowest = format.min_exponent() - (precision - 1);
    let mut exponent = (log2 - (precision - 1)).max(lowest);
    let (mut remainder, divisor) = scale(num, den, exponent);
    let mut q = 0u128;
    for bit in (0..format.precision).rev() {
        let step = divisor.shl(bit);
        if remainder >= step {
            remainder.sub_assign(&step);
            q |= 1 << bit;
        }
    }
    let twice = remainder.shl(1);
    if twice > divisor || (twice == divisor && q & 1 == 1) {
        q += 1;
    }
    let hidden = 1u128 << (format.precision - 1);
    if q == hidden << 1 {
        q = hidden;
        exponent += 1;
    }
    if exponent > format.max_exponent() - (precision - 1) {
        return Err(FloatError::Overflow);
    }
    let bits = if q < hidden {
        // Subnormal (or zero): the biased exponent is 0.
        q
    } else {
        let biased = exponent + (precision - 1) + format.bias();
        let biased = u128::try_from(biased).map_err(|_| FloatError::Overflow)?;
        biased << (format.precision - 1) | (q - hidden)
    };
    Ok(pack(width, sign | bits))
}

/// `(num * 2^-exponent, den)` or `(num, den * 2^exponent)`: the two sides
/// of `num / (den * 2^exponent)` as integers.
fn scale(num: &Big, den: &Big, exponent: i64) -> (Big, Big) {
    let shift = usize::try_from(exponent.unsigned_abs()).unwrap_or(usize::MAX);
    if exponent >= 0 {
        (num.clone(), den.shl(shift))
    } else {
        (num.shl(shift), den.clone())
    }
}

/// A bit length as an `i64` (big numbers here are far below `2^63` bits).
fn to_i64(bits: usize) -> i64 {
    i64::try_from(bits).unwrap_or(i64::MAX)
}

/// The pattern `bits` of `width`, which holds it.
fn pack(width: FloatWidth, bits: u128) -> FloatBits {
    // `round` builds at most `width` bits: a sign, the exponent field and
    // the stored significand.
    const FITS: &str = "the pattern has the format's width";
    match width {
        FloatWidth::W16 => FloatBits::F16(u16::try_from(bits).expect(FITS)),
        FloatWidth::W32 => FloatBits::F32(u32::try_from(bits).expect(FITS)),
        FloatWidth::W64 => FloatBits::F64(u64::try_from(bits).expect(FITS)),
        FloatWidth::W128 => FloatBits::F128(bits),
    }
}
