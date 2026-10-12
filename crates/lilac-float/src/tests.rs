use super::*;

fn parse(text: &str, width: FloatWidth) -> Result<u128, FloatError> {
    FloatBits::parse(text, width).map(FloatBits::to_bits)
}

fn half(text: &str) -> Result<u128, FloatError> {
    parse(text, FloatWidth::W16)
}

fn quad(text: &str) -> Result<u128, FloatError> {
    parse(text, FloatWidth::W128)
}

/// The exact converter on `text` at `width` (never Rust's parser).
fn exact(text: &str, width: FloatWidth) -> Result<u128, FloatError> {
    let decimal = Decimal::parse(text).ok_or(FloatError::Invalid)?;
    decimal.round(width).map(FloatBits::to_bits)
}

/// The exact decimal text of `m * 2^k`, nudged by `nudge` units in one
/// more decimal place than it needs: `-1` is just below the value, `1`
/// just above, `0` the value itself.
fn exact_text(m: u128, k: i64, nudge: i8) -> String {
    let shift = usize::try_from(k.unsigned_abs()).expect("small exponent");
    // `m * 2^k` is `digits * 10^exponent` with `exponent <= 0`.
    let (mut digits, exponent) = if k >= 0 {
        (Big::from_u128(m).shl(shift), 0)
    } else {
        // `2^-s = 5^s / 10^s`.
        let mut five = Big::from_u128(m);
        for _ in 0..shift {
            five.mul_small(5);
        }
        (five, -i64::try_from(shift).expect("small exponent"))
    };
    digits.mul_small(10);
    match nudge {
        1 => digits.add_small(1),
        -1 => digits.sub_assign(&Big::from_u64(1)),
        _ => {}
    }
    format!("{}e{}", digits.to_decimal(), exponent - 1)
}

#[test]
fn binary16_known_patterns() {
    assert_eq!(half("0.0"), Ok(0));
    assert_eq!(half("0.000e5"), Ok(0));
    assert_eq!(half("1.0"), Ok(0x3C00));
    assert_eq!(half("2.0"), Ok(0x4000));
    assert_eq!(half("0.5"), Ok(0x3800));
    assert_eq!(half("0.1"), Ok(0x2E66));
    assert_eq!(half("3.14159"), Ok(0x4248));
    assert_eq!(half("65504.0"), Ok(0x7BFF));
    // The smallest subnormal, the largest subnormal, the smallest normal.
    assert_eq!(half(&exact_text(1, -24, 0)), Ok(0x0001));
    assert_eq!(half("5.9604644775390625e-8"), Ok(0x0001));
    assert_eq!(half(&exact_text(1023, -24, 0)), Ok(0x03FF));
    assert_eq!(half("6.103515625e-5"), Ok(0x0400));
    assert_eq!(half(&exact_text(1, -14, 0)), Ok(0x0400));
    // Separators and long digit strings.
    assert_eq!(half("1_000.5"), Ok(0x63D1));
    assert_eq!(half("1_0.0_0e0_1"), Ok(0x5640));
    assert_eq!(
        half("1.000000000000000000000000000000000000000000000000000000000000000000000001"),
        Ok(0x3C00)
    );
    assert_eq!(
        half("0.99999999999999999999999999999999999999999999999999999999999999999999999"),
        Ok(0x3C00)
    );
    assert_eq!(
        half("00000000000000000000000000000000000000000000000000000000000001.5"),
        Ok(0x3E00)
    );
}

#[test]
fn binary16_rounds_half_to_even() {
    // `1 + 2^-11` lies halfway between `0x3C00` (even) and `0x3C01`.
    assert_eq!(half(&exact_text(2049, -11, 0)), Ok(0x3C00));
    assert_eq!(half(&exact_text(2049, -11, 1)), Ok(0x3C01));
    // `1 + 3 * 2^-11` lies halfway between `0x3C01` and `0x3C02` (even).
    assert_eq!(half(&exact_text(2051, -11, 0)), Ok(0x3C02));
    assert_eq!(half(&exact_text(2051, -11, -1)), Ok(0x3C01));
    // Half the smallest subnormal ties to zero; just above it does not.
    assert_eq!(half(&exact_text(1, -25, 0)), Ok(0));
    assert_eq!(half(&exact_text(1, -25, 1)), Ok(0x0001));
    assert_eq!(half("2.98e-8"), Ok(0));
    // One and a half subnormals tie to two.
    assert_eq!(half(&exact_text(3, -25, 0)), Ok(0x0002));
    // Halfway between the largest subnormal and the smallest normal.
    assert_eq!(half(&exact_text(2047, -25, 0)), Ok(0x0400));
    assert_eq!(half(&exact_text(2047, -25, -1)), Ok(0x03FF));
}

#[test]
fn binary16_overflow_threshold() {
    // `65520` is halfway between the largest finite value (odd) and
    // `2^16`, so it rounds to infinity.
    assert_eq!(half("65519.999"), Ok(0x7BFF));
    assert_eq!(half(&exact_text(65520, 0, -1)), Ok(0x7BFF));
    assert_eq!(half("65520.0"), Err(FloatError::Overflow));
    assert_eq!(half("70000.0"), Err(FloatError::Overflow));
    assert_eq!(half("1e5"), Err(FloatError::Overflow));
}

#[test]
fn absurd_exponents_are_decided_without_big_numbers() {
    for width in [
        FloatWidth::W16,
        FloatWidth::W32,
        FloatWidth::W64,
        FloatWidth::W128,
    ] {
        assert_eq!(parse("1e999999", width), Err(FloatError::Overflow));
        assert_eq!(
            parse("1e99999999999999999999999999", width),
            Err(FloatError::Overflow)
        );
        assert_eq!(parse("1e-999999", width), Ok(0));
        assert_eq!(parse("123456789e-999999999999", width), Ok(0));
        assert_eq!(parse("0.0e999999999999999999999", width), Ok(0));
        assert_eq!(exact("1e999999", width), Err(FloatError::Overflow));
        assert_eq!(exact("1e-999999", width), Ok(0));
    }
}

#[test]
fn malformed_text_is_invalid() {
    for text in [
        "", "1.", ".5", "1e", "1e+", "e5", "abc", "0x10", "1.5f", "-1.0",
    ] {
        assert_eq!(half(text), Err(FloatError::Invalid), "{text}");
        assert_eq!(
            parse(text, FloatWidth::W64),
            Err(FloatError::Invalid),
            "{text}"
        );
    }
}

#[test]
fn binary16_exhaustive_against_exact_midpoints() {
    // Every positive finite binary16 value and every midpoint between two
    // neighbours is exact in an `f64` (12 significant bits, exponents
    // from -25), so `{:.40e}` writes each one's exact decimal value.
    let value = |bits: u16| -> f64 {
        let exponent = i32::from((bits >> 10) & 0x1F);
        let fraction = f64::from(bits & 0x3FF);
        if exponent == 0 {
            fraction * 2f64.powi(-24)
        } else {
            (1024.0 + fraction) * 2f64.powi(exponent - 25)
        }
    };
    for bits in 0..0x7BFFu16 {
        let low = value(bits);
        let high = value(bits + 1);
        let mid = f64::midpoint(low, high);
        assert_eq!(half(&format!("{low:.40e}")), Ok(u128::from(bits)));
        let even = if bits % 2 == 0 { bits } else { bits + 1 };
        assert_eq!(
            half(&format!("{mid:.40e}")),
            Ok(u128::from(even)),
            "{mid:e}"
        );
        let below = f64::from_bits(mid.to_bits() - 1);
        let above = f64::from_bits(mid.to_bits() + 1);
        assert_eq!(half(&format!("{below:.60e}")), Ok(u128::from(bits)));
        assert_eq!(half(&format!("{above:.60e}")), Ok(u128::from(bits + 1)));
    }
}

#[test]
fn binary128_known_patterns() {
    let one = 0x3FFFu128 << 112;
    assert_eq!(quad("0.0"), Ok(0));
    assert_eq!(quad("1.0"), Ok(one));
    assert_eq!(quad("0.1"), Ok(0x3FFB_9999_9999_9999_9999_9999_9999_999A));
    assert_eq!(
        quad("3.14159265358979323846264338327950288419716939937510"),
        Ok(0x4000_921F_B544_42D1_8469_898C_C517_01B8)
    );
    // The smallest subnormal, the largest subnormal, the smallest normal.
    assert_eq!(quad(&exact_text(1, -16494, 0)), Ok(1));
    let fraction = (1u128 << 112) - 1;
    assert_eq!(quad(&exact_text(fraction, -16494, 0)), Ok(fraction));
    assert_eq!(quad(&exact_text(1, -16382, 0)), Ok(1 << 112));
    // The largest finite value, and the overflow threshold half an ulp
    // above it (the largest value is odd, so the tie overflows).
    let max = (1u128 << 113) - 1;
    let largest = 0x7FFE_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF;
    assert_eq!(quad(&exact_text(max, 16271, 0)), Ok(largest));
    let threshold = (1u128 << 114) - 1;
    assert_eq!(quad(&exact_text(threshold, 16270, -1)), Ok(largest));
    assert_eq!(
        quad(&exact_text(threshold, 16270, 0)),
        Err(FloatError::Overflow)
    );
    assert_eq!(quad("1.2e4932"), Err(FloatError::Overflow));
    // Halfway cases tie to even, in both directions.
    let halfway = (1u128 << 113) + 1;
    assert_eq!(quad(&exact_text(halfway, -113, 0)), Ok(one));
    assert_eq!(quad(&exact_text(halfway, -113, 1)), Ok(one + 1));
    assert_eq!(quad(&exact_text(halfway + 2, -113, 0)), Ok(one + 2));
    assert_eq!(quad(&exact_text(halfway + 2, -113, -1)), Ok(one + 1));
    assert_eq!(quad(&exact_text(1, -16495, 0)), Ok(0));
    assert_eq!(quad(&exact_text(1, -16495, 1)), Ok(1));
    assert_eq!(quad(&exact_text(3, -16495, 0)), Ok(2));
    assert_eq!(quad("1_000.000_1"), quad("1000.0001"));
}

/// A small xorshift, so the random cases are the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

#[test]
fn exact_converter_agrees_with_rust_at_32_and_64_bits() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut fixed = vec![
        "2.2250738585072011e-308".to_owned(),
        "2.2250738585072012e-308".to_owned(),
        "4.9406564584124654e-324".to_owned(),
        "2.4703282292062327e-324".to_owned(),
        "2.4703282292062328e-324".to_owned(),
        "1.7976931348623157e308".to_owned(),
        "1.7976931348623158e308".to_owned(),
        "1.7976931348623159e308".to_owned(),
        "3.4028235e38".to_owned(),
        "3.4028236e38".to_owned(),
        "1.4e-45".to_owned(),
        "7.0e-46".to_owned(),
        "9007199254740993.0".to_owned(),
        "16777217.0".to_owned(),
    ];
    for _ in 0..3000 {
        let count = 1 + rng.below(40);
        let digits: String = (0..count)
            .map(|_| char::from(b'0' + u8::try_from(rng.below(10)).unwrap_or(0)))
            .collect();
        let exponent = i64::try_from(rng.below(760)).unwrap_or(0) - 380;
        fixed.push(format!("{digits}.5e{exponent}"));
    }
    for text in &fixed {
        let ours = exact(text, FloatWidth::W64);
        let theirs: f64 = text.parse().expect("valid");
        let theirs = if theirs.is_finite() {
            Ok(u128::from(theirs.to_bits()))
        } else {
            Err(FloatError::Overflow)
        };
        assert_eq!(ours, theirs, "binary64 {text}");
        let ours = exact(text, FloatWidth::W32);
        let theirs: f32 = text.parse().expect("valid");
        let theirs = if theirs.is_finite() {
            Ok(u128::from(theirs.to_bits()))
        } else {
            Err(FloatError::Overflow)
        };
        assert_eq!(ours, theirs, "binary32 {text}");
    }
}

const OPS: [FloatOp; 5] = [
    FloatOp::Add,
    FloatOp::Sub,
    FloatOp::Mul,
    FloatOp::Div,
    FloatOp::Rem,
];

/// What `apply` must give for a native result: the bits when finite, else
/// the error IEEE's infinity or NaN stands for.
fn expected(finite: bool, bits: u128, rhs_is_zero: bool, op: FloatOp) -> Result<u128, FloatError> {
    if finite {
        Ok(bits)
    } else if rhs_is_zero && matches!(op, FloatOp::Div | FloatOp::Rem) {
        Err(FloatError::DivisionByZero)
    } else {
        Err(FloatError::Overflow)
    }
}

fn native64(left: f64, op: FloatOp, right: f64) -> f64 {
    match op {
        FloatOp::Add => left + right,
        FloatOp::Sub => left - right,
        FloatOp::Mul => left * right,
        FloatOp::Div => left / right,
        FloatOp::Rem => left % right,
    }
}

fn native32(left: f32, op: FloatOp, right: f32) -> f32 {
    match op {
        FloatOp::Add => left + right,
        FloatOp::Sub => left - right,
        FloatOp::Mul => left * right,
        FloatOp::Div => left / right,
        FloatOp::Rem => left % right,
    }
}

/// Finite `binary64` operands: the edges, then random patterns and close
/// neighbours of them (so subtraction cancels).
fn operands64(rng: &mut Rng) -> Vec<f64> {
    let mut values = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        3.0,
        0.1,
        f64::MAX,
        -f64::MAX,
        f64::MIN_POSITIVE,
        5e-324,
        -5e-324,
        2.5e-308,
        1e308,
    ];
    while values.len() < 400 {
        let value = f64::from_bits(rng.next());
        if value.is_finite() {
            values.push(value);
            values.push(f64::from_bits(value.to_bits() ^ (rng.next() & 0xFFFF)));
        }
    }
    values.retain(|value| value.is_finite());
    values
}

#[test]
fn arithmetic_agrees_with_native_binary64() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let values = operands64(&mut rng);
    for (index, left) in values.iter().enumerate() {
        // Each value meets a spread of others, edges included.
        for right in values.iter().skip(index % 7).step_by(7) {
            for op in OPS {
                let native = native64(*left, op, *right);
                let want = expected(
                    native.is_finite(),
                    u128::from(native.to_bits()),
                    *right == 0.0,
                    op,
                );
                let got = FloatBits::F64(left.to_bits())
                    .apply(op, FloatBits::F64(right.to_bits()))
                    .map(FloatBits::to_bits);
                assert_eq!(got, want, "{left:e} {op:?} {right:e}");
            }
        }
    }
}

#[test]
fn arithmetic_agrees_with_native_binary32() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut values = vec![
        0.0f32,
        -0.0,
        1.0,
        3.0,
        0.1,
        f32::MAX,
        -f32::MAX,
        f32::MIN_POSITIVE,
        1e-45,
        -1e-45,
    ];
    while values.len() < 600 {
        let value = f32::from_bits(u32::try_from(rng.next() >> 32).expect("32 bits"));
        if value.is_finite() {
            values.push(value);
            values.push(f32::from_bits(
                value.to_bits() ^ u32::try_from(rng.next() >> 52).expect("12 bits"),
            ));
        }
    }
    values.retain(|value| value.is_finite());
    for (index, left) in values.iter().enumerate() {
        for right in values.iter().skip(index % 5).step_by(5) {
            for op in OPS {
                let native = native32(*left, op, *right);
                let want = expected(
                    native.is_finite(),
                    u128::from(native.to_bits()),
                    *right == 0.0,
                    op,
                );
                let got = FloatBits::F32(left.to_bits())
                    .apply(op, FloatBits::F32(right.to_bits()))
                    .map(FloatBits::to_bits);
                assert_eq!(got, want, "{left:e} {op:?} {right:e}");
            }
        }
    }
}

/// A `binary16` pattern's exact value.
fn half_value(bits: u16) -> f64 {
    let exponent = i32::from((bits >> 10) & 0x1F);
    let fraction = f64::from(bits & 0x3FF);
    let magnitude = if exponent == 0 {
        fraction * 2f64.powi(-24)
    } else {
        (1024.0 + fraction) * 2f64.powi(exponent - 25)
    };
    if bits >> 15 == 1 {
        -magnitude
    } else {
        magnitude
    }
}

#[test]
fn arithmetic_agrees_with_binary64_rounded_once_at_binary16() {
    // `binary64` has more than `2 * 11 + 2` bits of precision, so an
    // operation on two `binary16` values computed in it and then rounded
    // once to `binary16` is the correctly rounded `binary16` result
    // (Figueroa, "When is double rounding innocuous?"); `%` is exact in
    // both. The rounding goes through the exact decimal converter.
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut values: Vec<u16> = vec![0, 0x8000, 0x3C00, 0x0001, 0x8001, 0x03FF, 0x0400, 0x7BFF];
    while values.len() < 500 {
        let bits = u16::try_from(rng.next() >> 48).expect("16 bits");
        if (bits >> 10) & 0x1F != 0x1F {
            values.push(bits);
        }
    }
    for (index, left) in values.iter().enumerate() {
        for right in values.iter().skip(index % 3).step_by(3) {
            for op in OPS {
                let native = native64(half_value(*left), op, half_value(*right));
                let want = if native.is_finite() {
                    let sign = if native.is_sign_negative() { 0x8000 } else { 0 };
                    half(&format!("{:.120e}", native.abs())).map(|bits| bits | sign)
                } else {
                    expected(false, 0, half_value(*right) == 0.0, op)
                };
                let got = FloatBits::F16(*left)
                    .apply(op, FloatBits::F16(*right))
                    .map(FloatBits::to_bits);
                assert_eq!(got, want, "{left:#06x} {op:?} {right:#06x}");
            }
        }
    }
}

#[test]
fn binary128_arithmetic() {
    let q = |text: &str| FloatBits::parse(text, FloatWidth::W128).expect("in range");
    let apply =
        |left: FloatBits, op, right: FloatBits| left.apply(op, right).map(FloatBits::to_bits);
    let bits = |value: FloatBits| Ok(value.to_bits());
    assert_eq!(apply(q("1.0"), FloatOp::Add, q("2.0")), bits(q("3.0")));
    assert_eq!(apply(q("1.5"), FloatOp::Mul, q("2.0")), bits(q("3.0")));
    assert_eq!(apply(q("5.5"), FloatOp::Rem, q("2.0")), bits(q("1.5")));
    // `1/3` is nowhere near a tie, so 50 digits of it round the same way.
    assert_eq!(
        apply(q("1.0"), FloatOp::Div, q("3.0")),
        bits(q("0.33333333333333333333333333333333333333333333333333"))
    );
    // `0.1 + 0.2` rounds once at 113 bits, not at 53.
    assert_eq!(
        apply(q("0.1"), FloatOp::Add, q("0.2")),
        Ok(0x3FFD_3333_3333_3333_3333_3333_3333_3334)
    );
    let max = FloatBits::F128(0x7FFE_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF);
    let tiny = FloatBits::F128(1);
    assert_eq!(apply(max, FloatOp::Add, max), Err(FloatError::Overflow));
    assert_eq!(
        apply(max, FloatOp::Mul, q("2.0")),
        Err(FloatError::Overflow)
    );
    // Half the smallest subnormal ties to zero; three halves tie to two.
    assert_eq!(apply(tiny, FloatOp::Div, q("2.0")), Ok(0));
    let three = apply(tiny, FloatOp::Mul, q("3.0")).expect("finite");
    assert_eq!(three, 3);
    assert_eq!(apply(FloatBits::F128(3), FloatOp::Div, q("2.0")), Ok(2));
    // The widest remainder: every finite value is a multiple of the
    // smallest subnormal.
    assert_eq!(apply(max, FloatOp::Rem, tiny), Ok(0));
    assert_eq!(
        apply(FloatBits::F128(3), FloatOp::Rem, FloatBits::F128(2)),
        Ok(1)
    );
    assert_eq!(apply(tiny, FloatOp::Rem, max), Ok(1));
    assert_eq!(
        apply(q("1.0"), FloatOp::Div, FloatBits::zero(FloatWidth::W128)),
        Err(FloatError::DivisionByZero)
    );
}

#[test]
fn zero_signs_follow_ieee() {
    let zero = FloatBits::F64(0);
    let negative_zero = FloatBits::F64(1 << 63);
    let one = FloatBits::F64(1f64.to_bits());
    let negative = |value: FloatBits| value.to_bits() >> 63 == 1;
    let apply = |left: FloatBits, op, right| left.apply(op, right).expect("finite");
    assert!(!negative(apply(zero, FloatOp::Sub, zero)));
    assert!(negative(apply(negative_zero, FloatOp::Sub, zero)));
    assert!(!negative(apply(one, FloatOp::Sub, one)));
    assert!(negative(apply(negative_zero, FloatOp::Mul, one)));
    let minus_four = FloatBits::F64((-4f64).to_bits());
    let two = FloatBits::F64(2f64.to_bits());
    assert!(negative(apply(minus_four, FloatOp::Rem, two)));
    assert_eq!(
        zero.apply(FloatOp::Rem, negative_zero),
        Err(FloatError::DivisionByZero)
    );
    assert_eq!(
        zero.apply(FloatOp::Div, zero),
        Err(FloatError::DivisionByZero)
    );
    let infinity = FloatBits::F64(f64::INFINITY.to_bits());
    assert_eq!(infinity.apply(FloatOp::Add, one), Err(FloatError::Overflow));
}

#[test]
fn canonical_text() {
    assert_eq!(FloatBits::F16(0x3C00).to_string(), "0xH3C00");
    assert_eq!(FloatBits::F16(0x0001).to_string(), "0xH0001");
    assert_eq!(
        FloatBits::F128(0x3FFF << 112).to_string(),
        "0xQ3FFF0000000000000000000000000000"
    );
    assert_eq!(FloatBits::F32(2.71f32.to_bits()).to_string(), "2.71");
    assert_eq!(FloatBits::F64(1000f64.to_bits()).to_string(), "1000.0");
    assert_eq!(FloatBits::F64(1.5e-4f64.to_bits()).to_string(), "0.00015");
    assert_eq!(FloatBits::zero(FloatWidth::W32).to_string(), "0.0");
}
