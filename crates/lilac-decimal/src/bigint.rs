#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::cmp::Ordering;

const BASE: u64 = 1_000_000_000;
const LIMB_DIGITS: usize = 9;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Big {
    limbs: Vec<u32>,
}

#[allow(clippy::should_implement_trait)]
impl Big {
    pub(crate) fn zero() -> Big {
        Big { limbs: Vec::new() }
    }

    pub(crate) fn from_decimal_digits(digits: &str) -> Option<Big> {
        let bytes = digits.as_bytes();
        if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let mut limbs = Vec::with_capacity(bytes.len() / LIMB_DIGITS + 1);
        let mut end = bytes.len();
        while end > 0 {
            let start = end.saturating_sub(LIMB_DIGITS);
            let limb: u32 = digits[start..end].parse().ok()?;
            limbs.push(limb);
            end = start;
        }
        let mut value = Big { limbs };
        value.trim();
        Some(value)
    }

    pub(crate) fn to_decimal_string(&self) -> String {
        let Some((top, rest)) = self.limbs.split_last() else {
            return String::from("0");
        };
        let mut out = top.to_string();
        for &limb in rest.iter().rev() {
            let mut buf = [b'0'; LIMB_DIGITS];
            let mut value = limb;
            for slot in buf.iter_mut().rev() {
                *slot = b'0' + (value % 10) as u8;
                value /= 10;
            }
            out.push_str(std::str::from_utf8(&buf).expect("decimal digits are ASCII"));
        }
        out
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    pub(crate) fn cmp(&self, other: &Big) -> Ordering {
        match self.limbs.len().cmp(&other.limbs.len()) {
            Ordering::Equal => self
                .limbs
                .iter()
                .rev()
                .zip(other.limbs.iter().rev())
                .find_map(|(a, b)| match a.cmp(b) {
                    Ordering::Equal => None,
                    ord => Some(ord),
                })
                .unwrap_or(Ordering::Equal),
            ord => ord,
        }
    }

    pub(crate) fn add(&self, other: &Big) -> Big {
        let len = self.limbs.len().max(other.limbs.len());
        let mut limbs = Vec::with_capacity(len + 1);
        let mut carry = 0u64;
        for i in 0..len {
            let a = u64::from(self.limbs.get(i).copied().unwrap_or(0));
            let b = u64::from(other.limbs.get(i).copied().unwrap_or(0));
            let sum = a + b + carry;
            limbs.push((sum % BASE) as u32);
            carry = sum / BASE;
        }
        if carry > 0 {
            limbs.push(carry as u32);
        }
        Big { limbs }
    }

    pub(crate) fn sub(&self, other: &Big) -> Big {
        debug_assert_ne!(self.cmp(other), Ordering::Less);
        let base = 1_000_000_000i64;
        let mut limbs = Vec::with_capacity(self.limbs.len());
        let mut borrow = 0i64;
        for i in 0..self.limbs.len() {
            let a = i64::from(self.limbs[i]);
            let b = i64::from(other.limbs.get(i).copied().unwrap_or(0));
            let mut diff = a - b - borrow;
            if diff < 0 {
                diff += base;
                borrow = 1;
            } else {
                borrow = 0;
            }
            limbs.push(diff as u32);
        }
        let mut value = Big { limbs };
        value.trim();
        value
    }

    pub(crate) fn mul(&self, other: &Big) -> Big {
        if self.is_zero() || other.is_zero() {
            return Big::zero();
        }
        let (a, b) = (&self.limbs, &other.limbs);
        let mut limbs = vec![0u32; a.len() + b.len()];
        for (i, &ai) in a.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &bj) in b.iter().enumerate() {
                let cur = u64::from(limbs[i + j]) + u64::from(ai) * u64::from(bj) + carry;
                limbs[i + j] = (cur % BASE) as u32;
                carry = cur / BASE;
            }
            limbs[i + b.len()] = carry as u32;
        }
        let mut value = Big { limbs };
        value.trim();
        value
    }

    pub(crate) fn mul_pow10(&self, n: u32) -> Big {
        if self.is_zero() {
            return Big::zero();
        }
        let whole = n / LIMB_DIGITS as u32;
        let rem = n % LIMB_DIGITS as u32;
        let factor = 10u64.pow(rem);
        let mut limbs = Vec::with_capacity(self.limbs.len() + 1);
        let mut carry = 0u64;
        for &limb in &self.limbs {
            let cur = u64::from(limb) * factor + carry;
            limbs.push((cur % BASE) as u32);
            carry = cur / BASE;
        }
        if carry > 0 {
            limbs.push(carry as u32);
        }
        if whole > 0 {
            let mut shifted = vec![0u32; whole as usize];
            shifted.extend_from_slice(&limbs);
            limbs = shifted;
        }
        Big { limbs }
    }

    pub(crate) fn divmod(&self, divisor: &Big) -> Option<(Big, Big)> {
        if divisor.is_zero() {
            return None;
        }
        if self.cmp(divisor) == Ordering::Less {
            return Some((Big::zero(), self.clone()));
        }
        let mut quotient = vec![0u32; self.limbs.len()];
        let mut remainder = Big::zero();
        for i in (0..self.limbs.len()).rev() {
            remainder = remainder.mul_small(BASE as u32).add_small(self.limbs[i]);
            let (digit, rest) = divmod_limb(&remainder, divisor);
            quotient[i] = digit;
            remainder = rest;
        }
        let mut value = Big { limbs: quotient };
        value.trim();
        Some((value, remainder))
    }

    fn mul_small(&self, factor: u32) -> Big {
        if factor == 0 || self.is_zero() {
            return Big::zero();
        }
        let mut limbs = Vec::with_capacity(self.limbs.len() + 1);
        let mut carry = 0u64;
        for &limb in &self.limbs {
            let cur = u64::from(limb) * u64::from(factor) + carry;
            limbs.push((cur % BASE) as u32);
            carry = cur / BASE;
        }
        if carry > 0 {
            limbs.push(carry as u32);
        }
        Big { limbs }
    }

    fn add_small(&self, addend: u32) -> Big {
        let mut limbs = self.limbs.clone();
        let mut carry = u64::from(addend);
        let mut i = 0;
        while carry > 0 && i < limbs.len() {
            let cur = u64::from(limbs[i]) + carry;
            limbs[i] = (cur % BASE) as u32;
            carry = cur / BASE;
            i += 1;
        }
        if carry > 0 {
            limbs.push(carry as u32);
        }
        Big { limbs }
    }

    fn trim(&mut self) {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
    }
}

fn divmod_limb(n: &Big, divisor: &Big) -> (u32, Big) {
    let mut lo = 0u32;
    let mut hi = BASE as u32;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if divisor.mul_small(mid).cmp(n) == Ordering::Greater {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    let digit = lo - 1;
    let rest = n.sub(&divisor.mul_small(digit));
    (digit, rest)
}

#[cfg(test)]
mod tests {
    use super::Big;

    fn b(s: &str) -> Big {
        Big::from_decimal_digits(s).expect("valid digits")
    }

    #[test]
    fn from_and_to_decimal_digits_round_trip() {
        let digits = "1234567890".repeat(20); // 200 digits
        assert_eq!(b(&digits).to_decimal_string(), digits);
    }

    #[test]
    fn add_carries_across_limbs() {
        assert_eq!(
            b("999999999").add(&b("1")).to_decimal_string(),
            "1000000000"
        );
    }

    #[test]
    fn sub_borrows() {
        assert_eq!(
            b("1000000000").sub(&b("1")).to_decimal_string(),
            "999999999"
        );
    }

    #[test]
    fn mul_exact() {
        assert_eq!(
            b("123456789").mul(&b("987654321")).to_decimal_string(),
            "121932631112635269"
        );
    }

    #[test]
    fn mul_pow10_shifts() {
        assert_eq!(b("5").mul_pow10(3).to_decimal_string(), "5000");
    }

    #[test]
    fn divmod_round_trip() {
        let (q, r) = b("1000000007")
            .divmod(&b("99991"))
            .expect("nonzero divisor");
        // q*d + r == n
        assert_eq!(q.mul(&b("99991")).add(&r).to_decimal_string(), "1000000007");
    }

    #[test]
    fn cmp_orders() {
        assert_eq!(b("10").cmp(&b("9")), std::cmp::Ordering::Greater);
        assert_eq!(b("9").cmp(&b("10")), std::cmp::Ordering::Less);
        assert_eq!(b("007").cmp(&b("7")), std::cmp::Ordering::Equal);
    }

    #[test]
    fn zero_is_canonical() {
        assert!(Big::zero().is_zero());
        assert!(b("0").is_zero());
        assert!(b("000000000000").is_zero());
        assert_eq!(Big::zero().to_decimal_string(), "0");
        assert_eq!(b("0").to_decimal_string(), "0");
    }

    #[test]
    fn rejects_non_digits() {
        assert!(Big::from_decimal_digits("").is_none());
        assert!(Big::from_decimal_digits("12a3").is_none());
        assert!(Big::from_decimal_digits("+12").is_none());
    }

    #[test]
    fn sub_to_zero_trims() {
        let value = b("123456789012345678901234567890");
        assert!(value.sub(&value).is_zero());
    }

    #[test]
    fn mul_pow10_whole_limb_and_carry() {
        assert_eq!(
            b("123456789").mul_pow10(9).to_decimal_string(),
            "123456789000000000"
        );
        assert_eq!(
            b("999999999").mul_pow10(8).to_decimal_string(),
            "99999999900000000"
        );
        assert_eq!(
            b("123456789").mul_pow10(11).to_decimal_string(),
            "12345678900000000000"
        );
    }

    #[test]
    fn divmod_multi_limb_round_trip() {
        let n = b("123456789012345678901234567890123456789");
        let d = b("987654321987654321");
        let (q, r) = n.divmod(&d).expect("nonzero divisor");
        assert_eq!(r.cmp(&d), std::cmp::Ordering::Less);
        assert_eq!(q.mul(&d).add(&r).to_decimal_string(), n.to_decimal_string());
    }
}
