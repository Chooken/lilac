use std::cmp::Ordering;

use crate::{FloatBits, FloatWidth, Rounding};

/// A small xorshift, so the random cases are the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

const ROUNDINGS: [Rounding; 5] = [
    Rounding::Floor,
    Rounding::Ceil,
    Rounding::Trunc,
    Rounding::NearestAway,
    Rounding::NearestEven,
];

fn native_round64(value: f64, rounding: Rounding) -> f64 {
    match rounding {
        Rounding::Floor => value.floor(),
        Rounding::Ceil => value.ceil(),
        Rounding::Trunc => value.trunc(),
        Rounding::NearestAway => value.round(),
        Rounding::NearestEven => value.round_ties_even(),
    }
}

fn native_round32(value: f32, rounding: Rounding) -> f32 {
    match rounding {
        Rounding::Floor => value.floor(),
        Rounding::Ceil => value.ceil(),
        Rounding::Trunc => value.trunc(),
        Rounding::NearestAway => value.round(),
        Rounding::NearestEven => value.round_ties_even(),
    }
}

/// `binary64` operands: the edges (infinities and NaNs too), random
/// patterns, values near integers and halves, and neighbours.
fn operands64(rng: &mut Rng) -> Vec<f64> {
    let mut values = vec![
        0.0,
        -0.0,
        0.5,
        -0.5,
        1.5,
        2.5,
        -2.5,
        0.499_999_999_999_999_94,
        4_503_599_627_370_495.5,
        4_503_599_627_370_496.0,
        f64::MAX,
        -f64::MAX,
        f64::MIN_POSITIVE,
        5e-324,
        -5e-324,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        -f64::NAN,
        9.223_372_036_854_775e18,
        -9.223_372_036_854_776e18,
        1.844_674_407_370_955_2e19,
        65504.0,
        65520.0,
        65519.999,
    ];
    while values.len() < 3000 {
        let bits = rng.next();
        values.push(f64::from_bits(bits));
        // An exponent near 1, where the rounding operations matter.
        let near = (bits & 0x800F_FFFF_FFFF_FFFF) | ((1021 + (bits >> 52 & 0x3F)) << 52);
        values.push(f64::from_bits(near));
    }
    values
}

fn same64(got: Option<FloatBits>, want: f64) -> bool {
    match got {
        None => want.is_nan(),
        Some(got) => got == FloatBits::F64(want.to_bits()),
    }
}

#[test]
fn unary_operations_agree_with_native_binary64() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for value in operands64(&mut rng) {
        let bits = FloatBits::F64(value.to_bits());
        assert!(same64(bits.sqrt(), value.sqrt()), "sqrt {value:e}");
        for rounding in ROUNDINGS {
            assert!(
                same64(
                    bits.round_to_integral(rounding),
                    native_round64(value, rounding)
                ),
                "{rounding:?} {value:e}"
            );
        }
        assert_eq!(bits.abs(), FloatBits::F64(value.abs().to_bits()));
        assert_eq!(bits.negate(), FloatBits::F64((-value).to_bits()));
        assert_eq!(bits.is_nan(), value.is_nan());
        let to32 = bits.convert(FloatWidth::W32);
        #[allow(clippy::cast_possible_truncation, reason = "the reference")]
        let want32 = value as f32;
        match to32 {
            None => assert!(want32.is_nan()),
            Some(got) => assert_eq!(got, FloatBits::F32(want32.to_bits()), "{value:e}"),
        }
        let from32 = FloatBits::F32(want32.to_bits()).convert(FloatWidth::W64);
        assert!(same64(from32, f64::from(want32)));
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "Rust's `as` saturates as the intrinsics do"
        )]
        let ints = [
            (8, true, u64::from(value as i8 as u8)),
            (32, true, u64::from(value as i32 as u32)),
            (64, true, (value as i64).cast_unsigned()),
            (8, false, u64::from(value as u8)),
            (32, false, u64::from(value as u32)),
            (64, false, value as u64),
        ];
        for (width, signed, want) in ints {
            assert_eq!(
                bits.to_int(width, signed),
                want,
                "{value:e} to {width} {signed}"
            );
        }
    }
}

#[test]
fn binary_operations_agree_with_native_binary64() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let values = operands64(&mut rng);
    for (index, left) in values.iter().enumerate().step_by(7) {
        for right in values.iter().skip(index % 5).step_by(23) {
            let (a, b) = (
                FloatBits::F64(left.to_bits()),
                FloatBits::F64(right.to_bits()),
            );
            assert_eq!(a.compare(b), left.partial_cmp(right), "{left:e} {right:e}");
            assert_eq!(
                a.copy_sign(b),
                FloatBits::F64(left.copysign(*right).to_bits())
            );
            let min = if left.is_nan() || right.is_nan() {
                f64::NAN
            } else if left.partial_cmp(right) == Some(Ordering::Equal) {
                f64::from_bits(left.to_bits() | right.to_bits())
            } else {
                left.min(*right)
            };
            let max = if left.is_nan() || right.is_nan() {
                f64::NAN
            } else if left.partial_cmp(right) == Some(Ordering::Equal) {
                f64::from_bits(left.to_bits() & right.to_bits())
            } else {
                left.max(*right)
            };
            assert!(same64(a.minimum(b), min), "min {left:e} {right:e}");
            assert!(same64(a.maximum(b), max), "max {left:e} {right:e}");
        }
    }
}

#[test]
fn multiply_add_agrees_with_native_binary64() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let values: Vec<f64> = operands64(&mut rng)
        .into_iter()
        .filter(|value| value.is_finite())
        .collect();
    for (index, a) in values.iter().enumerate().step_by(3) {
        let b = values[(index * 7 + 1) % values.len()];
        let product = a * b;
        // A drawn addend, and the product's negation nudged, where the
        // sum cancels.
        let nudged = f64::from_bits((-product).to_bits() ^ (rng.next() & 0x7));
        for c in [
            values[(index * 13 + 5) % values.len()],
            nudged,
            -product,
            0.0,
            -0.0,
        ] {
            if !c.is_finite() {
                continue;
            }
            let want = a.mul_add(b, c);
            let got = FloatBits::F64(a.to_bits())
                .multiply_add(FloatBits::F64(b.to_bits()), FloatBits::F64(c.to_bits()));
            assert!(same64(got, want), "{a:e} * {b:e} + {c:e}");
        }
    }
    // An infinite or NaN operand is not folded.
    let one = FloatBits::F64(1f64.to_bits());
    let inf = FloatBits::F64(f64::INFINITY.to_bits());
    assert_eq!(one.multiply_add(one, inf), None);
}

#[test]
fn operations_agree_with_native_binary32() {
    let mut rng = Rng(0x0123_4567_89AB_CDEF);
    let mut values: Vec<f32> = vec![0.0, -0.0, 0.5, -2.5, f32::INFINITY, f32::NAN, f32::MAX];
    while values.len() < 4000 {
        let bits = u32::try_from(rng.next() >> 32).expect("32 bits");
        values.push(f32::from_bits(bits));
        let near = (bits & 0x807F_FFFF) | ((125 + (bits >> 23 & 0x1F)) << 23);
        values.push(f32::from_bits(near));
    }
    let same = |got: Option<FloatBits>, want: f32| match got {
        None => want.is_nan(),
        Some(got) => got == FloatBits::F32(want.to_bits()),
    };
    for (index, value) in values.iter().enumerate() {
        let bits = FloatBits::F32(value.to_bits());
        assert!(same(bits.sqrt(), value.sqrt()), "sqrt {value:e}");
        for rounding in ROUNDINGS {
            assert!(
                same(
                    bits.round_to_integral(rounding),
                    native_round32(*value, rounding)
                ),
                "{rounding:?} {value:e}"
            );
        }
        let b = values[(index * 7 + 3) % values.len()];
        let c = values[(index * 11 + 1) % values.len()];
        if value.is_finite() && b.is_finite() && c.is_finite() {
            let got = bits.multiply_add(FloatBits::F32(b.to_bits()), FloatBits::F32(c.to_bits()));
            assert!(same(got, value.mul_add(b, c)), "{value:e} * {b:e} + {c:e}");
            let cancel = -(value * b);
            if !cancel.is_finite() {
                continue;
            }
            let got = bits.multiply_add(
                FloatBits::F32(b.to_bits()),
                FloatBits::F32(cancel.to_bits()),
            );
            assert!(
                same(got, value.mul_add(b, cancel)),
                "{value:e} * {b:e} + {cancel:e}"
            );
        }
    }
    // Integers to floats: Rust's `as` rounds to nearest even.
    for _ in 0..4000 {
        let int = rng.next() >> (rng.next() & 0x3F);
        #[allow(clippy::cast_precision_loss, reason = "the reference")]
        let (want32, want64) = (int as f32, int as f64);
        assert_eq!(
            FloatBits::from_int(false, u128::from(int), FloatWidth::W32),
            FloatBits::F32(want32.to_bits())
        );
        assert_eq!(
            FloatBits::from_int(false, u128::from(int), FloatWidth::W64),
            FloatBits::F64(want64.to_bits())
        );
        let signed = int.cast_signed();
        #[allow(clippy::cast_precision_loss, reason = "the reference")]
        let want = signed as f64;
        assert_eq!(
            FloatBits::from_int(
                signed < 0,
                u128::from(signed.unsigned_abs()),
                FloatWidth::W64
            ),
            FloatBits::F64(want.to_bits())
        );
    }
}

/// A `binary16` pattern's exact value.
fn half_value(bits: u16) -> f64 {
    let exponent = i32::from((bits >> 10) & 0x1F);
    let fraction = f64::from(bits & 0x3FF);
    let magnitude = match exponent {
        0 => fraction * 2f64.powi(-24),
        31 if fraction == 0.0 => f64::INFINITY,
        31 => f64::NAN,
        _ => (1024.0 + fraction) * 2f64.powi(exponent - 25),
    };
    if bits >> 15 == 1 {
        -magnitude
    } else {
        magnitude
    }
}

/// The `binary16` nearest the double `value`, rounded once (through the
/// exact decimal converter).
fn half_of(value: f64) -> Option<FloatBits> {
    if value.is_nan() {
        return None;
    }
    if value.is_infinite() {
        return Some(FloatBits::infinity(FloatWidth::W16, value < 0.0));
    }
    let bits = FloatBits::parse(&format!("{:.120e}", value.abs()), FloatWidth::W16)
        .unwrap_or(FloatBits::infinity(FloatWidth::W16, false));
    Some(if value.is_sign_negative() {
        bits.negate()
    } else {
        bits
    })
}

#[test]
fn every_binary16_pattern() {
    // `binary64` keeps more than `2 * 11 + 2` bits, so a root computed in
    // it and rounded once more is the correctly rounded `binary16` root;
    // the rounding operations are exact in it.
    for bits in 0..=u16::MAX {
        let value = half_value(bits);
        let half = FloatBits::F16(bits);
        assert_eq!(half.sqrt(), half_of(value.sqrt()), "sqrt {bits:#06x}");
        for rounding in ROUNDINGS {
            assert_eq!(
                half.round_to_integral(rounding),
                half_of(native_round64(value, rounding)),
                "{rounding:?} {bits:#06x}"
            );
        }
        assert_eq!(half.convert(FloatWidth::W64).map(FloatBits::to_bits), {
            (!value.is_nan()).then(|| u128::from(value.to_bits()))
        });
        assert_eq!(
            half.convert(FloatWidth::W64)
                .and_then(|wide| wide.convert(FloatWidth::W16)),
            (!value.is_nan()).then_some(half)
        );
    }
    // Narrowing rounds once: just above a tie of `binary16` (but a tie
    // after rounding to `binary32` first).
    let narrow = FloatBits::F64(0x3FF0_0200_0000_1000).convert(FloatWidth::W16);
    assert_eq!(narrow, Some(FloatBits::F16(0x3C01)));
    // Past the largest finite half is infinite.
    let big = FloatBits::F32(65520f32.to_bits()).convert(FloatWidth::W16);
    assert_eq!(big, Some(FloatBits::F16(0x7C00)));
    assert_eq!(
        FloatBits::from_int(true, u128::from(u64::MAX), FloatWidth::W16),
        FloatBits::F16(0xFC00)
    );
    // `0.1` is `0x2E66`, so `0.1 * 10 - 1` is exactly `-2^-12`, where a
    // separately rounded product would be `1` and the sum `0`.
    let fma = |width, a: &str, b: &str, c: &str| {
        let parse = |text: &str| match text.strip_prefix('-') {
            Some(text) => FloatBits::parse(text, width).expect("a literal").negate(),
            None => FloatBits::parse(text, width).expect("a literal"),
        };
        parse(a).multiply_add(parse(b), parse(c))
    };
    assert_eq!(
        fma(FloatWidth::W16, "0.1", "10.0", "-1.0"),
        Some(FloatBits::F16(0x8C00))
    );
}

#[test]
fn binary128_operations() {
    let quad = |text: &str| match text.strip_prefix('-') {
        Some(text) => FloatBits::parse(text, FloatWidth::W128)
            .expect("a literal")
            .negate(),
        None => FloatBits::parse(text, FloatWidth::W128).expect("a literal"),
    };
    assert_eq!(quad("4.0").sqrt(), Some(quad("2.0")));
    assert_eq!(
        quad("2.0").sqrt(),
        Some(FloatBits::F128(0x3FFF_6A09_E667_F3BC_C908_B2FB_1366_EA95))
    );
    assert_eq!(quad("-1.0").sqrt(), None);
    assert_eq!(
        quad("-0.0").sqrt(),
        Some(FloatBits::negative_zero(FloatWidth::W128))
    );
    assert_eq!(
        quad("2.5").round_to_integral(Rounding::NearestEven),
        Some(quad("2.0"))
    );
    assert_eq!(
        quad("2.5").round_to_integral(Rounding::NearestAway),
        Some(quad("3.0"))
    );
    assert_eq!(
        quad("-0.25").round_to_integral(Rounding::Ceil),
        Some(FloatBits::negative_zero(FloatWidth::W128))
    );
    assert_eq!(
        quad("-0.25").round_to_integral(Rounding::Floor),
        Some(quad("-1.0"))
    );
    assert_eq!(quad("1e40").to_int(64, true), i64::MAX.cast_unsigned());
    assert_eq!(quad("-1e40").to_int(64, false), 0);
    assert_eq!(quad("-7.9").to_int(8, true), 0xF9);
    assert_eq!(
        quad("3.0").multiply_add(quad("5.0"), quad("-15.0")),
        Some(FloatBits::zero(FloatWidth::W128))
    );
    assert_eq!(quad("1.0").compare(quad("1.5")), Some(Ordering::Less));
    let inf = FloatBits::infinity(FloatWidth::W128, true);
    assert_eq!(inf.compare(quad("-1e4000")), Some(Ordering::Less));
    assert_eq!(inf.convert(FloatWidth::W16), Some(FloatBits::F16(0xFC00)));
}
