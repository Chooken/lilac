//! The hardware's float operations (math plan phase 1), exactly as IEEE
//! 754 defines them: square root and fused multiply-add rounded once,
//! rounding to an integral value, `minimum`/`maximum`, the sign
//! operations and the conversions. They fold the intrinsics of the same
//! names, so each gives the bits the run-time operation does.
//!
//! A result that is a NaN is `None`: hardware differs in the NaN it
//! makes, so it is never a constant. A finite result that rounds past the
//! largest finite value is the infinity of its sign, as the default
//! rounding mode gives.

use std::cmp::Ordering;

use crate::big::Big;
use crate::{exact_sum, pack, round_scaled, FloatBits, FloatError, FloatWidth};

/// How [`FloatBits::round_to_integral`] picks an integral value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rounding {
    /// Toward negative infinity (`@f_floor_W`).
    Floor,
    /// Toward positive infinity (`@f_ceil_W`).
    Ceil,
    /// Toward zero (`@f_trunc_W`).
    Trunc,
    /// To nearest, ties away from zero (`@f_round_W`).
    NearestAway,
    /// To nearest, ties to even (`@f_round_even_W`).
    NearestEven,
}

impl FloatBits {
    /// The infinity of `width`, negative when `negative`.
    #[must_use]
    pub fn infinity(width: FloatWidth, negative: bool) -> Self {
        let format = width.format();
        let exponent = (1u128 << format.exponent_bits) - 1;
        pack(
            width,
            u128::from(negative) << (width.bits() - 1) | exponent << (format.precision - 1),
        )
    }

    /// Whether the pattern is a NaN.
    #[must_use]
    pub fn is_nan(self) -> bool {
        self.magnitude() > Self::infinity(self.width(), false).to_bits()
    }

    /// Whether the sign bit is set.
    #[must_use]
    pub fn is_negative(self) -> bool {
        self.to_bits() >> (self.width().bits() - 1) == 1
    }

    /// The pattern without its sign bit.
    fn magnitude(self) -> u128 {
        self.to_bits() & !self.sign_bit()
    }

    fn sign_bit(self) -> u128 {
        1 << (self.width().bits() - 1)
    }

    /// The sign bit flipped (`@f_negate_W`; a NaN too).
    #[must_use]
    pub fn negate(self) -> Self {
        pack(self.width(), self.to_bits() ^ self.sign_bit())
    }

    /// The sign bit cleared (`@f_abs_W`; a NaN too).
    #[must_use]
    pub fn abs(self) -> Self {
        pack(self.width(), self.magnitude())
    }

    /// This magnitude with `sign`'s sign bit (`@f_copy_sign_W`; `sign` is
    /// at its own width, and either may be a NaN).
    #[must_use]
    pub fn copy_sign(self, sign: Self) -> Self {
        let negative = u128::from(sign.is_negative()) << (self.width().bits() - 1);
        pack(self.width(), self.magnitude() | negative)
    }

    /// IEEE comparison: `None` when either is a NaN, and `-0 == +0`. Both
    /// are taken at their exact values, so the widths may differ.
    #[must_use]
    pub fn compare(self, other: Self) -> Option<Ordering> {
        if self.is_nan() || other.is_nan() {
            return None;
        }
        if let (Some(left), Some(right)) = (self.decode(), other.decode()) {
            // The sign of `left - right`, exact.
            let term = |value: crate::Exact, negative: bool| {
                (negative, Big::from_u128(value.significand), value.exponent)
            };
            let (negative, magnitude, _) =
                exact_sum(&term(left, left.negative), &term(right, !right.negative));
            return Some(match (magnitude.is_zero(), negative) {
                (true, _) => Ordering::Equal,
                (false, true) => Ordering::Less,
                (false, false) => Ordering::Greater,
            });
        }
        // An infinity against a finite value or another infinity.
        let key = |value: Self| match value.decode() {
            Some(_) => 0,
            None if value.is_negative() => -1,
            None => 1,
        };
        Some(key(self).cmp(&key(other)))
    }

    /// IEEE 754-2019 `minimum` (`@f_min_W`): a NaN operand makes a NaN
    /// (`None`), and `-0` is below `+0`.
    #[must_use]
    pub fn minimum(self, other: Self) -> Option<Self> {
        self.pick(other, true)
    }

    /// IEEE 754-2019 `maximum` (`@f_max_W`), like [`Self::minimum`].
    #[must_use]
    pub fn maximum(self, other: Self) -> Option<Self> {
        self.pick(other, false)
    }

    fn pick(self, other: Self, min: bool) -> Option<Self> {
        Some(match self.compare(other)? {
            Ordering::Less => {
                if min {
                    self
                } else {
                    other
                }
            }
            Ordering::Greater => {
                if min {
                    other
                } else {
                    self
                }
            }
            // Equal values differ only in a zero's sign: the minimum of
            // `-0` and `+0` is `-0`, the maximum `+0`.
            Ordering::Equal => {
                let bits = if min {
                    self.to_bits() | other.to_bits()
                } else {
                    self.to_bits() & other.to_bits()
                };
                pack(self.width(), bits)
            }
        })
    }

    /// The square root rounded once (`@f_sqrt_W`): `None` (a NaN) below
    /// zero; `-0` is `-0`.
    #[must_use]
    pub fn sqrt(self) -> Option<Self> {
        if self.is_nan() || (self.is_negative() && self.magnitude() != 0) {
            return None;
        }
        let Some(value) = self.decode() else {
            return Some(self);
        };
        if value.significand == 0 {
            return Some(self);
        }
        let precision = self.width().format().precision;
        // An even exponent, then enough bits that the root has two more
        // than the format keeps: the root's bits and whether it is exact
        // then round as the real root does.
        let (mut significand, mut exponent) = (value.significand, value.exponent);
        if exponent % 2 != 0 {
            significand <<= 1;
            exponent -= 1;
        }
        let wanted = 2 * precision + 4;
        let have = usize::try_from(u128::BITS - significand.leading_zeros()).unwrap_or(0);
        let shift = wanted.saturating_sub(have).div_ceil(2) * 2;
        let square = Big::from_u128(significand).shl(shift);
        let root = isqrt(&square);
        let root_big = Big::from_u128(root);
        let inexact = root_big.mul(&root_big) != square;
        let magnitude = Big::from_u128(root << 1 | u128::from(inexact));
        let half_shift = i64::try_from(shift / 2).ok()?;
        round_scaled(
            false,
            &magnitude,
            exponent / 2 - half_shift - 1,
            self.width(),
        )
        .ok()
    }

    /// `self * b + c` rounded once (`@f_multiply_add_W`). An infinite or
    /// NaN operand is `None` (not folded).
    #[must_use]
    pub fn multiply_add(self, b: Self, c: Self) -> Option<Self> {
        let (left, right, addend) = (self.decode()?, b.decode()?, c.decode()?);
        let product = (
            left.negative != right.negative,
            Big::from_u128(left.significand).mul(&Big::from_u128(right.significand)),
            left.exponent + right.exponent,
        );
        let addend = (
            addend.negative,
            Big::from_u128(addend.significand),
            addend.exponent,
        );
        let (negative, magnitude, exponent) = exact_sum(&product, &addend);
        Some(rounded(negative, &magnitude, exponent, self.width()))
    }

    /// The integral value `rounding` picks (`@f_floor_W` and the rest).
    /// An infinity is itself; a zero result keeps the operand's sign.
    #[must_use]
    pub fn round_to_integral(self, rounding: Rounding) -> Option<Self> {
        if self.is_nan() {
            return None;
        }
        let Some(value) = self.decode() else {
            return Some(self);
        };
        if value.exponent >= 0 || value.significand == 0 {
            return Some(self);
        }
        // `significand = whole * 2^shift + rest`.
        let shift = u32::try_from(-value.exponent).unwrap_or(u32::MAX);
        let (whole, rest, half) = if shift >= u128::BITS {
            // The significand (under 2^113) is below half of `2^shift`.
            (0, value.significand, None)
        } else {
            let rest = value.significand & ((1 << shift) - 1);
            (value.significand >> shift, rest, Some(1u128 << (shift - 1)))
        };
        let above_half = half.is_some_and(|half| rest > half);
        let at_half = half.is_some_and(|half| rest == half);
        let up = match rounding {
            Rounding::Trunc => false,
            Rounding::Floor => value.negative && rest != 0,
            Rounding::Ceil => !value.negative && rest != 0,
            Rounding::NearestAway => above_half || at_half,
            Rounding::NearestEven => above_half || (at_half && whole & 1 == 1),
        };
        let whole = whole + u128::from(up);
        round_scaled(value.negative, &Big::from_u128(whole), 0, self.width()).ok()
    }

    /// The value at `width`, exact or rounded once (`@float_convert`); a
    /// NaN is `None`.
    #[must_use]
    pub fn convert(self, width: FloatWidth) -> Option<Self> {
        if self.is_nan() {
            return None;
        }
        Some(match self.decode() {
            Some(value) => rounded(
                value.negative,
                &Big::from_u128(value.significand),
                value.exponent,
                width,
            ),
            None => Self::infinity(width, self.is_negative()),
        })
    }

    /// The integer `±magnitude` rounded once to `width` (`@int_to_float`,
    /// `@uint_to_float`).
    #[must_use]
    pub fn from_int(negative: bool, magnitude: u128, width: FloatWidth) -> Self {
        rounded(
            negative && magnitude != 0,
            &Big::from_u128(magnitude),
            0,
            width,
        )
    }

    /// The value truncated toward zero and saturated to a `bits`-bit
    /// integer (`@float_to_int` when `signed`, else `@float_to_uint`), as
    /// its two's complement bits; a NaN is 0. `bits` is at most 64.
    #[must_use]
    pub fn to_int(self, bits: u32, signed: bool) -> u64 {
        debug_assert!((1..=64).contains(&bits), "an integer width");
        if self.is_nan() {
            return 0;
        }
        let max = if signed {
            (1u128 << (bits - 1)) - 1
        } else {
            (1u128 << bits) - 1
        };
        let min_magnitude = if signed { 1u128 << (bits - 1) } else { 0 };
        // The magnitude truncated, where anything from `2^64` (an infinity
        // too) saturates whatever the width.
        let magnitude = match self.decode() {
            Some(value) if value.exponent >= 64 => u128::MAX,
            Some(value) if value.exponent >= 0 => {
                // Under `2^113` shifted by under 64: at most 176 bits.
                let shift = u32::try_from(value.exponent).unwrap_or(0);
                if value.significand.leading_zeros() < shift {
                    u128::MAX
                } else {
                    value.significand << shift
                }
            }
            Some(value) => {
                let shift = u32::try_from(-value.exponent).unwrap_or(u32::MAX);
                value.significand.checked_shr(shift).unwrap_or(0)
            }
            None => u128::MAX,
        };
        let mask = u128::MAX >> (u128::BITS - bits);
        let result = if self.is_negative() {
            // `-magnitude`, at least `MIN`.
            magnitude.min(min_magnitude).wrapping_neg() & mask
        } else {
            magnitude.min(max)
        };
        // Within the mask of at most 64 bits.
        u64::try_from(result).unwrap_or(u64::MAX)
    }
}

/// `±magnitude * 2^exponent` rounded once to `width`, or the infinity of
/// its sign past the largest finite value.
fn rounded(negative: bool, magnitude: &Big, exponent: i64, width: FloatWidth) -> FloatBits {
    match round_scaled(negative, magnitude, exponent, width) {
        Ok(bits) => bits,
        Err(FloatError::Overflow | FloatError::DivisionByZero | FloatError::Invalid) => {
            FloatBits::infinity(width, negative)
        }
    }
}

/// The integer square root of `value` (below `2^256`).
fn isqrt(value: &Big) -> u128 {
    let mut root = 0u128;
    let top = value.bit_len().div_ceil(2).min(127);
    for bit in (0..=top).rev() {
        let candidate = root | 1 << bit;
        let big = Big::from_u128(candidate);
        if big.mul(&big) <= *value {
            root = candidate;
        }
    }
    root
}

#[cfg(test)]
mod tests;
