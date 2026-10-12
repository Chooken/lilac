//! A minimal unsigned big integer: just what exact rounding and exact
//! arithmetic need (build from decimal digits, add, multiply, shift,
//! compare, subtract).

use std::cmp::Ordering;
#[cfg(test)]
use std::fmt::Write as _;

/// An unsigned integer of any size: little-endian 64-bit limbs with no
/// zero limb at the top, so zero has no limbs and equal values have equal
/// limbs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Big {
    limbs: Vec<u64>,
}

/// `10^19`, the largest power of ten in a `u64`.
const TEN_POW_19: u64 = 10_000_000_000_000_000_000;

impl Big {
    pub(crate) fn from_u64(value: u64) -> Self {
        let mut big = Self { limbs: vec![value] };
        big.trim();
        big
    }

    pub(crate) fn from_u128(value: u128) -> Self {
        let mut big = Self {
            limbs: vec![low_half(value), low_half(value >> 64)],
        };
        big.trim();
        big
    }

    /// The integer an ASCII decimal digit string spells.
    pub(crate) fn from_decimal(digits: &[u8]) -> Self {
        let mut big = Self::from_u64(0);
        for chunk in digits.chunks(19) {
            let mut scale = 1u64;
            let mut value = 0u64;
            for digit in chunk {
                scale *= 10;
                value = value * 10 + u64::from(digit - b'0');
            }
            big.mul_small(scale);
            big.add_small(value);
        }
        big
    }

    /// `10^exponent`.
    pub(crate) fn pow10(exponent: usize) -> Self {
        let mut big = Self::from_u64(1);
        big.mul_pow10(exponent);
        big
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    /// The number of significant bits (0 for zero).
    pub(crate) fn bit_len(&self) -> usize {
        match self.limbs.last() {
            Some(top) => {
                // At most 64, so the cast is lossless.
                let top_bits = (u64::BITS - top.leading_zeros()) as usize;
                (self.limbs.len() - 1) * 64 + top_bits
            }
            None => 0,
        }
    }

    pub(crate) fn mul_small(&mut self, factor: u64) {
        let mut carry = 0u128;
        for limb in &mut self.limbs {
            let product = u128::from(*limb) * u128::from(factor) + carry;
            *limb = low_half(product);
            carry = product >> 64;
        }
        if carry != 0 {
            self.limbs.push(low_half(carry));
        }
        self.trim();
    }

    pub(crate) fn add_small(&mut self, addend: u64) {
        let mut carry = addend;
        for limb in &mut self.limbs {
            if carry == 0 {
                return;
            }
            let (sum, overflow) = limb.overflowing_add(carry);
            *limb = sum;
            carry = u64::from(overflow);
        }
        if carry != 0 {
            self.limbs.push(carry);
        }
    }

    /// `self + other`.
    pub(crate) fn add(&self, other: &Self) -> Self {
        let (long, short) = if self.limbs.len() >= other.limbs.len() {
            (self, other)
        } else {
            (other, self)
        };
        let mut limbs = Vec::with_capacity(long.limbs.len() + 1);
        let mut carry = false;
        for (index, limb) in long.limbs.iter().enumerate() {
            let addend = short.limbs.get(index).copied().unwrap_or(0);
            let (sum, over) = limb.overflowing_add(addend);
            let (sum, over_carry) = sum.overflowing_add(u64::from(carry));
            limbs.push(sum);
            carry = over || over_carry;
        }
        if carry {
            limbs.push(1);
        }
        let mut big = Self { limbs };
        big.trim();
        big
    }

    /// `self * other`.
    pub(crate) fn mul(&self, other: &Self) -> Self {
        let mut limbs = vec![0u64; self.limbs.len() + other.limbs.len()];
        for (row, left) in self.limbs.iter().enumerate() {
            let mut carry = 0u128;
            for (column, right) in other.limbs.iter().enumerate() {
                // At most `(2^64 - 1)^2 + 2 * (2^64 - 1)`, which is
                // `2^128 - 1`, so nothing overflows.
                let product = u128::from(*left) * u128::from(*right)
                    + u128::from(limbs[row + column])
                    + carry;
                limbs[row + column] = low_half(product);
                carry = product >> 64;
            }
            limbs[row + other.limbs.len()] = low_half(carry);
        }
        let mut big = Self { limbs };
        big.trim();
        big
    }

    /// Multiplies by `10^exponent`.
    pub(crate) fn mul_pow10(&mut self, exponent: usize) {
        for _ in 0..exponent / 19 {
            self.mul_small(TEN_POW_19);
        }
        for _ in 0..exponent % 19 {
            self.mul_small(10);
        }
    }

    /// `self * 2^bits`.
    pub(crate) fn shl(&self, bits: usize) -> Self {
        if self.is_zero() {
            return self.clone();
        }
        let (whole, part) = (bits / 64, bits % 64);
        let mut limbs = vec![0; whole];
        if part == 0 {
            limbs.extend_from_slice(&self.limbs);
        } else {
            let mut carry = 0u64;
            for limb in &self.limbs {
                limbs.push((limb << part) | carry);
                carry = limb >> (64 - part);
            }
            limbs.push(carry);
        }
        let mut big = Self { limbs };
        big.trim();
        big
    }

    /// `self -= other`; `other` must not be larger than `self`.
    pub(crate) fn sub_assign(&mut self, other: &Self) {
        debug_assert!(*self >= *other, "big subtraction would go negative");
        let mut borrow = false;
        for (index, limb) in self.limbs.iter_mut().enumerate() {
            let subtrahend = other.limbs.get(index).copied().unwrap_or(0);
            let (difference, under) = limb.overflowing_sub(subtrahend);
            let (difference, under_borrow) = difference.overflowing_sub(u64::from(borrow));
            *limb = difference;
            borrow = under || under_borrow;
        }
        self.trim();
    }

    fn trim(&mut self) {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
    }

    /// The decimal digits of the value (`"0"` for zero), for tests.
    #[cfg(test)]
    pub(crate) fn to_decimal(&self) -> String {
        let mut limbs = self.limbs.clone();
        let mut chunks = Vec::new();
        while !limbs.is_empty() {
            let mut remainder = 0u128;
            for limb in limbs.iter_mut().rev() {
                let value = (remainder << 64) | u128::from(*limb);
                *limb = low_half(value / u128::from(TEN_POW_19));
                remainder = value % u128::from(TEN_POW_19);
            }
            while limbs.last() == Some(&0) {
                limbs.pop();
            }
            chunks.push(low_half(remainder));
        }
        let Some((top, rest)) = chunks.split_last() else {
            return "0".to_owned();
        };
        let mut text = top.to_string();
        for chunk in rest.iter().rev() {
            write!(text, "{chunk:019}").expect("writing to a String cannot fail");
        }
        text
    }
}

/// The low 64 bits of `value`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "dropping the high half is the point"
)]
fn low_half(value: u128) -> u64 {
    value as u64
}

impl Ord for Big {
    fn cmp(&self, other: &Self) -> Ordering {
        self.limbs
            .len()
            .cmp(&other.limbs.len())
            .then_with(|| self.limbs.iter().rev().cmp(other.limbs.iter().rev()))
    }
}

impl PartialOrd for Big {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
