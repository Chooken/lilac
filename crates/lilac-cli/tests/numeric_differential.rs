//! The hardware's numeric operations (math plan phase 1) against Rust's,
//! bit for bit, on many inputs.
//!
//! The program draws its inputs from its own generator (random bits,
//! special values, values near integers, products and their negations for
//! the fused multiply-add) and prints each input with the result, one case
//! a line: `name input... result`, as unsigned decimal bits. This test
//! recomputes every result from the printed inputs: Rust's `sqrt`,
//! `mul_add`, rounding and `as` conversions at 32 and 64 bits, and at 16
//! bits exact arithmetic rounded once ([`half`]). A NaN result only has
//! to be a NaN. On x86-64 the float operations run the backend's own
//! routines, on aarch64 its instructions.

mod support;

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

/// Cases per operation.
const COUNT: u32 = 2000;

/// A float width's tokens for the templates: the width, its exponent bias,
/// fraction bits, and the two exponent ranges inputs are drawn from
/// (`NEAR`, from `BIAS - 2`, where rounding to an integer matters; and
/// `SPREAD`, from `SPREAD_LOW`).
struct Float {
    bits: u32,
    bias: u32,
    fraction: u32,
    near: u32,
    spread: u32,
    spread_low: u32,
}

const FLOATS: [Float; 3] = [
    Float {
        bits: 16,
        bias: 15,
        fraction: 10,
        near: 0xF,
        spread: 0x1F,
        spread_low: 0,
    },
    Float {
        bits: 32,
        bias: 127,
        fraction: 23,
        near: 0x1F,
        spread: 0x3F,
        spread_low: 95,
    },
    Float {
        bits: 64,
        bias: 1023,
        fraction: 52,
        near: 0x3F,
        spread: 0x7F,
        spread_low: 959,
    },
];

const INT_WIDTHS: [u32; 4] = [8, 16, 32, 64];

const FLOAT_UNARY: [&str; 7] = [
    "sqrt",
    "abs",
    "floor",
    "ceil",
    "trunc",
    "round",
    "round_even",
];
const FLOAT_BINARY: [&str; 3] = ["min", "max", "copy_sign"];
const INT_BINARY: [&str; 14] = [
    "bit_rotate_left",
    "bit_rotate_right",
    "i_add_saturating",
    "i_add_saturating_u",
    "i_sub_saturating",
    "i_sub_saturating_u",
    "i_multiply_high",
    "i_multiply_high_u",
    "i_add_overflows",
    "i_add_overflows_u",
    "i_sub_overflows",
    "i_sub_overflows_u",
    "i_mul_overflows",
    "i_mul_overflows_u",
];
const INT_UNARY: [&str; 5] = [
    "bit_count_ones",
    "bit_leading_zeros",
    "bit_trailing_zeros",
    "bit_swap_bytes",
    "bit_reverse",
];

/// Replaces each `%KEY%` in `template`.
fn fill(template: &str, pairs: &[(&str, String)]) -> String {
    let mut text = template.to_owned();
    for (key, value) in pairs {
        text = text.replace(&format!("%{key}%"), value);
    }
    text
}

/// A loop printing `COUNT` cases of `call` on inputs `inputs` (each a
/// name and its generator call).
fn runner(name: &str, inputs: &[(&str, String, String)], result: &str, call: &str) -> String {
    let mut body = String::new();
    let mut holes = String::new();
    for (input, ty, generator) in inputs {
        writeln!(body, "            {input}: {ty} = {generator}").expect("writes");
        write!(holes, " {{{input}}}").expect("writes");
    }
    format!(
        "run_{name}: function () nothing {{
    i: @bit32 = 0x0
    loop {{
        if @i_less_than_u_32(i, {COUNT:#X}) {{
{body}            r: {result} = {call}
            print_line($\"{name}{holes} {{r}}\")
            i = @i_add_32(i, 0x1)
        }} else break
    }}
}}
"
    )
    .replace("0X", "0x")
}

const PRELUDE: &str = r"using Standard
using Standard.Io

seed: @bit64 = 0x9E3779B97F4A7C15

// xorshift64.
next: function () r: @bit64 {
    x: @bit64 = seed
    x = @bit_xor_64(x, @bit_shift_left_64(x, 0xD))
    x = @bit_xor_64(x, @bit_shift_right_u_64(x, 0x7))
    x = @bit_xor_64(x, @bit_shift_left_64(x, 0x11))
    seed = x
    r = x
}

int_special: function (index: @bit64) r: @bit64 {
    if index == 0x0 {
        r = 0x0
    } else if index == 0x1 {
        r = 0x1
    } else if index == 0x2 {
        r = 0xFFFFFFFFFFFFFFFF
    } else if index == 0x3 {
        r = 0x7FFFFFFFFFFFFFFF
    } else {
        r = 0x8000000000000000
    }
}
";

/// The integer input generator at `width`: random bits, shifted down,
/// negated, or a special value (shifted to the width's `MIN`/`MAX`).
fn int_input(width: u32) -> String {
    let shift = 64 - width;
    let truncate = if width == 64 {
        "x".to_owned()
    } else {
        "@truncate(x)".to_owned()
    };
    fill(
        r"int_input%W%: function () r: @bit%W% {
    pick: @bit64 = @bit_and_64(next(), 0x3)
    x: @bit64 = next()
    if pick == 0x1 {
        x = @bit_shift_right_u_64(x, @bit_and_64(next(), 0x3F))
    } else if pick == 0x2 {
        x = @i_negate_64(@bit_shift_right_u_64(x, @bit_and_64(next(), 0x3F)))
    } else if pick == 0x3 {
        index: @bit64 = @i_mod_u_64(next(), 0x5)
        x = int_special(index)
        if @i_greater_than_u_64(index, 0x2) {
            x = @bit_shift_right_64(x, %SHIFT%)
        }
    }
    r = %TRUNCATE%
}
",
        &[
            ("W", width.to_string()),
            ("SHIFT", format!("{shift:#x}")),
            ("TRUNCATE", truncate),
        ],
    )
}

/// The float input generator at one width: random bits, a special value,
/// a quarter of a small integer, or random fraction bits under an
/// exponent near 1 or spread wider.
#[allow(clippy::too_many_lines, reason = "one template")]
fn float_input(float: &Float) -> String {
    let w = float.bits;
    let specials: Vec<u64> = match w {
        16 => vec![
            0x0, 0x8000, 0x7C00, 0xFC00, 0x7E00, 0x1, 0x3FF, 0x400, 0x7BFF, 0x3C00, 0xBC00, 0x3800,
            0x3E00, 0x4100, 0xC100, 0x6400,
        ],
        32 => vec![
            0x0,
            0x8000_0000,
            0x7F80_0000,
            0xFF80_0000,
            0x7FC0_0000,
            0x1,
            0x7F_FFFF,
            0x80_0000,
            0x7F7F_FFFF,
            0x3F80_0000,
            0xBF80_0000,
            0x3F00_0000,
            0x3FC0_0000,
            0x4020_0000,
            0xC020_0000,
            0x4B00_0000,
        ],
        _ => vec![
            0x0,
            0x8000_0000_0000_0000,
            0x7FF0_0000_0000_0000,
            0xFFF0_0000_0000_0000,
            0x7FF8_0000_0000_0000,
            0x1,
            0xF_FFFF_FFFF_FFFF,
            0x10_0000_0000_0000,
            0x7FEF_FFFF_FFFF_FFFF,
            0x3FF0_0000_0000_0000,
            0xBFF0_0000_0000_0000,
            0x3FE0_0000_0000_0000,
            0x3FF8_0000_0000_0000,
            0x4004_0000_0000_0000,
            0xC004_0000_0000_0000,
            0x4330_0000_0000_0000,
        ],
    };
    let mut special = String::new();
    for (index, value) in specials.iter().enumerate() {
        let keyword = if index == 0 { "    if" } else { " else if" };
        write!(
            special,
            "{keyword} index == {index:#x} {{\n        r = {value:#x}\n    }}"
        )
        .expect("writes");
    }
    let mut text = fill(
        r"float_special%W%: function (index: @bit%W%) r: @bit%W% {
%SPECIAL% else {
        r = 0x0
    }
}

with_exponent%W%: function (x: @bit%W%, e: @bit%W%) r: @bit%W% {
    kept: @bit%W% = @bit_and_%W%(x, %KEEP%)
    r = @bit_or_%W%(kept, @bit_shift_left_%W%(e, %FRACTION%))
}

input%W%: function () r: @bit%W% {
    pick: @bit64 = @bit_and_64(next(), 0x7)
    x: @bit%W% = %RAW%
    if pick == 0x1 {
        r = float_special%W%(@bit_and_%W%(x, 0xF))
    } else if pick == 0x2 {
        k: @bit%W% = @i_sub_%W%(@bit_and_%W%(x, 0x7F), 0x40)
        whole: @fbit%W% = @int_to_float(k)
        r = @fbit_to_bits_%W%(@f_mul_%W%(whole, 0.25))
    } else if pick == 0x3 or pick == 0x4 {
        e: @bit%W% = @i_add_%W%(@bit_and_%W%(@bit_shift_right_u_%W%(x, %FRACTION%), %NEAR%), %NEAR_LOW%)
        r = with_exponent%W%(x, e)
    } else if pick == 0x5 or pick == 0x6 {
        e: @bit%W% = @i_add_%W%(@bit_and_%W%(@bit_shift_right_u_%W%(x, %FRACTION%), %SPREAD%), %SPREAD_LOW%)
        r = with_exponent%W%(x, e)
    } else {
        r = x
    }
}
",
        &[
            ("SPECIAL", special),
            (
                "KEEP",
                format!("{:#x}", (1u64 << (w - 1)) | ((1u64 << float.fraction) - 1)),
            ),
            ("FRACTION", format!("{:#x}", float.fraction)),
            ("NEAR", format!("{:#x}", float.near)),
            ("NEAR_LOW", format!("{:#x}", float.bias - 2)),
            ("SPREAD", format!("{:#x}", float.spread)),
            ("SPREAD_LOW", format!("{:#x}", float.spread_low)),
            (
                "RAW",
                if w == 64 {
                    "next()".to_owned()
                } else {
                    "@truncate(next())".to_owned()
                },
            ),
        ],
    );
    text = text.replace("%W%", &w.to_string());
    text
}

/// The whole program: the generators, one wrapper and runner per
/// operation and width, and a `main` calling every runner.
#[allow(clippy::too_many_lines, reason = "one template")]
fn program() -> String {
    let mut text = PRELUDE.to_owned();
    let mut runs = Vec::new();
    for width in INT_WIDTHS {
        text.push_str(&int_input(width));
        let ty = format!("@bit{width}");
        let input = || format!("int_input{width}()");
        for op in INT_BINARY {
            let name = format!("{op}_{width}");
            let result = if op.contains("overflows") {
                "@bit8".to_owned()
            } else {
                ty.clone()
            };
            writeln!(
                text,
                "w_{name}: function (a: {ty}, b: {ty}) r: {result} {{ r = @{op}_{width}(a, b) }}"
            )
            .expect("writes");
            text.push_str(&runner(
                &name,
                &[("a", ty.clone(), input()), ("b", ty.clone(), input())],
                &result,
                &format!("w_{name}(a, b)"),
            ));
            runs.push(name);
        }
        for op in INT_UNARY {
            if op == "bit_swap_bytes" && width == 8 {
                continue;
            }
            let name = format!("{op}_{width}");
            writeln!(
                text,
                "w_{name}: function (a: {ty}) r: {ty} {{ r = @{op}_{width}(a) }}"
            )
            .expect("writes");
            text.push_str(&runner(
                &name,
                &[("a", ty.clone(), input())],
                &ty,
                &format!("w_{name}(a)"),
            ));
            runs.push(name);
        }
    }
    for float in &FLOATS {
        text.push_str(&float_input(float));
    }
    for float in &FLOATS {
        let w = float.bits;
        let ty = format!("@bit{w}");
        let input = || format!("input{w}()");
        let value = |name: &str| format!("@bits_to_fbit_{w}({name})");
        for op in FLOAT_UNARY {
            let name = format!("f_{op}_{w}");
            writeln!(
                text,
                "w_{name}: function (a: {ty}) r: {ty} {{ r = @fbit_to_bits_{w}(@{name}({})) }}",
                value("a")
            )
            .expect("writes");
            text.push_str(&runner(
                &name,
                &[("a", ty.clone(), input())],
                &ty,
                &format!("w_{name}(a)"),
            ));
            runs.push(name);
        }
        for op in FLOAT_BINARY {
            let name = format!("f_{op}_{w}");
            writeln!(
                text,
                "w_{name}: function (a: {ty}, b: {ty}) r: {ty} {{ r = @fbit_to_bits_{w}(@{name}({}, {})) }}",
                value("a"),
                value("b")
            )
            .expect("writes");
            text.push_str(&runner(
                &name,
                &[("a", ty.clone(), input()), ("b", ty.clone(), input())],
                &ty,
                &format!("w_{name}(a, b)"),
            ));
            runs.push(name);
        }
        // The addend is drawn, or is the product negated with its low
        // bits disturbed, where the sum cancels.
        let name = format!("f_multiply_add_{w}");
        writeln!(
            text,
            "w_{name}: function (a: {ty}, b: {ty}, c: {ty}) r: {ty} {{ r = @fbit_to_bits_{w}(@{name}({}, {}, {})) }}
addend{w}: function (a: {ty}, b: {ty}) r: {ty} {{
    if @bit_and_64(next(), 0x1) == 0x0 {{
        r = input{w}()
    }} else {{
        product: {ty} = @fbit_to_bits_{w}(@f_negate_{w}(@f_mul_{w}({}, {})))
        noise: @bit64 = @bit_and_64(next(), 0x7)
        r = @bit_xor_{w}(product, {})
    }}
}}",
            value("a"),
            value("b"),
            value("c"),
            value("a"),
            value("b"),
            if w == 64 { "noise".to_owned() } else { "@truncate(noise)".to_owned() }
        )
        .expect("writes");
        text.push_str(&runner(
            &name,
            &[
                ("a", ty.clone(), input()),
                ("b", ty.clone(), input()),
                ("c", ty.clone(), format!("addend{w}(a, b)")),
            ],
            &ty,
            &format!("w_{name}(a, b, c)"),
        ));
        runs.push(name);
        // Conversions from this width.
        for int in [8, 32, 64] {
            for (op, tag) in [("float_to_int", "s"), ("float_to_uint", "u")] {
                let name = format!("f{w}_to_{tag}{int}");
                writeln!(
                    text,
                    "w_{name}: function (a: {ty}) r: @bit{int} {{ r = @{op}({}) }}",
                    value("a")
                )
                .expect("writes");
                text.push_str(&runner(
                    &name,
                    &[("a", ty.clone(), input())],
                    &format!("@bit{int}"),
                    &format!("w_{name}(a)"),
                ));
                runs.push(name);
            }
        }
        for int in [32, 64] {
            for (op, tag) in [("int_to_float", "s"), ("uint_to_float", "u")] {
                let name = format!("{tag}{int}_to_f{w}");
                writeln!(
                    text,
                    "w_{name}: function (a: @bit{int}) r: {ty} {{\n    value: @fbit{w} = @{op}(a)\n    r = @fbit_to_bits_{w}(value)\n}}"
                )
                .expect("writes");
                text.push_str(&runner(
                    &name,
                    &[("a", format!("@bit{int}"), format!("int_input{int}()"))],
                    &ty,
                    &format!("w_{name}(a)"),
                ));
                runs.push(name);
            }
        }
        for to in [16, 32, 64] {
            if to == w {
                continue;
            }
            let name = format!("f{w}_to_f{to}");
            writeln!(
                text,
                "w_{name}: function (a: {ty}) r: @bit{to} {{\n    value: @fbit{to} = @float_convert({})\n    r = @fbit_to_bits_{to}(value)\n}}",
                value("a")
            )
            .expect("writes");
            text.push_str(&runner(
                &name,
                &[("a", ty.clone(), input())],
                &format!("@bit{to}"),
                &format!("w_{name}(a)"),
            ));
            runs.push(name);
        }
    }
    text.push_str("main: function () nothing {\n");
    for name in &runs {
        writeln!(text, "    run_{name}()").expect("writes");
    }
    text.push_str("}\n");
    text
}

/// Exact 16-bit float arithmetic, rounded once.
mod half {
    /// The exact value of a half: sign, significand and exponent, or
    /// `None` for an infinity or NaN.
    pub fn decode(bits: u16) -> Option<(bool, u128, i32)> {
        let negative = bits & 0x8000 != 0;
        let field = i32::from((bits >> 10) & 0x1F);
        let fraction = u128::from(bits & 0x3FF);
        match field {
            31 => None,
            0 => Some((negative, fraction, -24)),
            _ => Some((negative, fraction | 0x400, field - 25)),
        }
    }

    /// The exact value of a double, like [`decode`].
    pub fn decode64(bits: u64) -> Option<(bool, u128, i32)> {
        let negative = bits >> 63 != 0;
        let field = i32::try_from((bits >> 52) & 0x7FF).expect("11 bits");
        let fraction = u128::from(bits & 0xF_FFFF_FFFF_FFFF);
        match field {
            0x7FF => None,
            0 => Some((negative, fraction, -1074)),
            _ => Some((negative, fraction | 1 << 52, field - 1075)),
        }
    }

    /// `±magnitude * 2^exponent` rounded to the nearest half, ties to even.
    pub fn round(negative: bool, magnitude: u128, exponent: i32) -> u16 {
        let sign = if negative { 0x8000 } else { 0 };
        if magnitude == 0 {
            return sign;
        }
        let top = 127 - i32::try_from(magnitude.leading_zeros()).expect("small") + exponent;
        let lsb = (top - 10).max(-24);
        let shift = lsb - exponent;
        let q = if shift <= 0 {
            magnitude << -shift
        } else if shift > 127 {
            0
        } else {
            let q = magnitude >> shift;
            let rest = magnitude & ((1 << shift) - 1);
            let half = 1u128 << (shift - 1);
            if rest > half || (rest == half && q & 1 == 1) {
                q + 1
            } else {
                q
            }
        };
        let biased = i64::from(lsb + 24);
        let bits = (biased << 10) + i64::try_from(q).unwrap_or(i64::MAX >> 1);
        let bits = u16::try_from(bits.min(0x7C00)).expect("clamped");
        bits | sign
    }

    /// The half nearest `value`, rounded once.
    pub fn from_f64(value: f64) -> u16 {
        match decode64(value.to_bits()) {
            Some((negative, magnitude, exponent)) => round(negative, magnitude, exponent),
            None if value.is_nan() => 0x7E00,
            None => {
                if value > 0.0 {
                    0x7C00
                } else {
                    0xFC00
                }
            }
        }
    }

    pub fn to_f64(bits: u16) -> f64 {
        match decode(bits) {
            Some((negative, magnitude, exponent)) => {
                #[allow(clippy::cast_precision_loss, reason = "11 bits")]
                let value = magnitude as f64 * 2f64.powi(exponent);
                if negative {
                    -value
                } else {
                    value
                }
            }
            None if bits & 0x3FF != 0 => f64::NAN,
            None if bits & 0x8000 != 0 => f64::NEG_INFINITY,
            None => f64::INFINITY,
        }
    }

    /// `a * b + c` rounded once.
    #[allow(clippy::many_single_char_names, reason = "the formula's names")]
    pub fn fma(a: u16, b: u16, c: u16) -> u16 {
        let (Some(x), Some(y), Some(z)) = (decode(a), decode(b), decode(c)) else {
            return from_f64(to_f64(a) * to_f64(b) + to_f64(c));
        };
        let product = i128::try_from(x.1 * y.1).expect("22 bits");
        let product = if x.0 == y.0 { product } else { -product };
        let addend = i128::try_from(z.1).expect("11 bits");
        let addend = if z.0 { -addend } else { addend };
        let product_exponent = x.2 + y.2;
        let low = product_exponent.min(z.2);
        let sum = (product << (product_exponent - low)) + (addend << (z.2 - low));
        if sum == 0 {
            // An exact zero is `+0` unless both terms are `-0`-like.
            let product_negative = x.0 != y.0;
            let both_negative = product_negative && z.0;
            let product_zero = x.1 == 0 || y.1 == 0;
            if product_zero && z.1 == 0 {
                return if both_negative { 0x8000 } else { 0 };
            }
            return 0;
        }
        round(sum < 0, sum.unsigned_abs(), low)
    }
}

fn is_nan(bits: u64, width: u32) -> bool {
    match width {
        16 => bits & 0x7C00 == 0x7C00 && bits & 0x3FF != 0,
        32 => f32::from_bits(u32::try_from(bits).expect("32 bits")).is_nan(),
        _ => f64::from_bits(bits).is_nan(),
    }
}

/// IEEE `minimum`/`maximum` on bits, with `less` the order.
fn min_max(
    a: u64,
    b: u64,
    min: bool,
    nan: impl Fn(u64) -> bool,
    less: impl Fn(u64, u64) -> bool,
) -> Option<u64> {
    if nan(a) || nan(b) {
        return None;
    }
    Some(if less(a, b) {
        if min {
            a
        } else {
            b
        }
    } else if less(b, a) {
        if min {
            b
        } else {
            a
        }
    } else if min {
        a | b
    } else {
        a & b
    })
}

/// The expected result of float operation `op` at `width` on `args`, or
/// `None` for a NaN.
#[allow(clippy::too_many_lines, reason = "one table of references")]
fn float_reference(op: &str, width: u32, args: &[u64]) -> Option<u64> {
    let result = match width {
        64 => {
            let x = f64::from_bits(args[0]);
            let y = || f64::from_bits(args[1]);
            let value = match op {
                "sqrt" => x.sqrt(),
                "abs" => x.abs(),
                "floor" => x.floor(),
                "ceil" => x.ceil(),
                "trunc" => x.trunc(),
                "round" => x.round(),
                "round_even" => x.round_ties_even(),
                "copy_sign" => x.copysign(y()),
                "multiply_add" => x.mul_add(y(), f64::from_bits(args[2])),
                "min" | "max" => {
                    let less = |a: u64, b: u64| f64::from_bits(a) < f64::from_bits(b);
                    return min_max(
                        args[0],
                        args[1],
                        op == "min",
                        |a| f64::from_bits(a).is_nan(),
                        less,
                    );
                }
                _ => unreachable!("{op}"),
            };
            value.to_bits()
        }
        32 => {
            let bits = |index: usize| u32::try_from(args[index]).expect("32 bits");
            let x = f32::from_bits(bits(0));
            let y = || f32::from_bits(bits(1));
            let value = match op {
                "sqrt" => x.sqrt(),
                "abs" => x.abs(),
                "floor" => x.floor(),
                "ceil" => x.ceil(),
                "trunc" => x.trunc(),
                "round" => x.round(),
                "round_even" => x.round_ties_even(),
                "copy_sign" => x.copysign(y()),
                "multiply_add" => x.mul_add(y(), f32::from_bits(bits(2))),
                "min" | "max" => {
                    let less = |a: u64, b: u64| {
                        f32::from_bits(u32::try_from(a).expect("32 bits"))
                            < f32::from_bits(u32::try_from(b).expect("32 bits"))
                    };
                    return min_max(args[0], args[1], op == "min", |a| is_nan(a, 32), less);
                }
                _ => unreachable!("{op}"),
            };
            u64::from(value.to_bits())
        }
        _ => {
            let bits = |index: usize| u16::try_from(args[index]).expect("16 bits");
            let x = half::to_f64(bits(0));
            let result = match op {
                "abs" => bits(0) & 0x7FFF,
                "copy_sign" => (bits(0) & 0x7FFF) | (bits(1) & 0x8000),
                "multiply_add" => half::fma(bits(0), bits(1), bits(2)),
                "min" | "max" => {
                    let less = |a: u64, b: u64| {
                        half::to_f64(u16::try_from(a).expect("16 bits"))
                            < half::to_f64(u16::try_from(b).expect("16 bits"))
                    };
                    return min_max(args[0], args[1], op == "min", |a| is_nan(a, 16), less);
                }
                // Each is exact in a double, or (`sqrt`) rounded to 53
                // bits, which then rounds once to 11.
                _ => half::from_f64(match op {
                    "sqrt" => x.sqrt(),
                    "floor" => x.floor(),
                    "ceil" => x.ceil(),
                    "trunc" => x.trunc(),
                    "round" => x.round(),
                    "round_even" => x.round_ties_even(),
                    _ => unreachable!("{op}"),
                }),
            };
            u64::from(result)
        }
    };
    (!is_nan(result, width)).then_some(result)
}

/// A float input as a double (exact).
fn float_value(bits: u64, width: u32) -> f64 {
    match width {
        16 => half::to_f64(u16::try_from(bits).expect("16 bits")),
        32 => f64::from(f32::from_bits(u32::try_from(bits).expect("32 bits"))),
        _ => f64::from_bits(bits),
    }
}

/// `value` (a double holding a value of width `from`) at float width `to`.
#[allow(clippy::cast_possible_truncation, reason = "rounding is the point")]
fn float_bits(value: f64, to: u32) -> u64 {
    match to {
        16 => u64::from(half::from_f64(value)),
        32 => u64::from((value as f32).to_bits()),
        _ => value.to_bits(),
    }
}

/// The expected result of a conversion named like `f32_to_s8`,
/// `u64_to_f16` or `f64_to_f16`, or `None` for a NaN.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    reason = "Rust's `as` is the reference"
)]
fn convert_reference(name: &str, input: u64) -> Option<u64> {
    let (from, to) = name.split_once("_to_").expect("a conversion");
    let width = |text: &str| text[1..].parse::<u32>().expect("a width");
    match (&from[..1], &to[..1]) {
        ("f", "f") => {
            let value = float_value(input, width(from));
            let out = width(to);
            let bits = float_bits(value, out);
            (!is_nan(bits, out)).then_some(bits)
        }
        ("f", kind) => {
            let value = float_value(input, width(from));
            Some(match (kind, width(to)) {
                ("s", 8) => u64::from(value as i8 as u8),
                ("s", 32) => u64::from(value as i32 as u32),
                ("s", _) => value as i64 as u64,
                ("u", 8) => u64::from(value as u8),
                ("u", 32) => u64::from(value as u32),
                _ => value as u64,
            })
        }
        (kind, _) => {
            let out = width(to);
            let signed = kind == "s";
            let int = width(from);
            let value: i128 = match (signed, int) {
                (true, 32) => i128::from(input as u32 as i32),
                (true, _) => i128::from(input as i64),
                (false, _) => i128::from(input),
            };
            Some(match out {
                16 => u64::from(half::round(value < 0, value.unsigned_abs(), 0)),
                32 => u64::from(if signed {
                    (value as i64 as f32).to_bits()
                } else {
                    (value as u64 as f32).to_bits()
                }),
                _ => {
                    if signed {
                        (value as i64 as f64).to_bits()
                    } else {
                        (value as u64 as f64).to_bits()
                    }
                }
            })
        }
    }
}

macro_rules! int_reference {
    ($op:expr, $a:expr, $b:expr, $u:ty, $i:ty, $wide:ty, $uwide:ty) => {{
        let (a, b) = ($a as $u, $b as $u);
        let (sa, sb) = (a as $i, b as $i);
        let bits = <$u>::BITS;
        let value: u64 = match $op {
            "bit_rotate_left" => a.rotate_left((u64::from(b) % u64::from(bits)) as u32) as u64,
            "bit_rotate_right" => a.rotate_right((u64::from(b) % u64::from(bits)) as u32) as u64,
            "i_add_saturating" => sa.saturating_add(sb) as $u as u64,
            "i_add_saturating_u" => a.saturating_add(b) as u64,
            "i_sub_saturating" => sa.saturating_sub(sb) as $u as u64,
            "i_sub_saturating_u" => a.saturating_sub(b) as u64,
            "i_multiply_high" => ((sa as $wide * sb as $wide) >> bits) as $u as u64,
            "i_multiply_high_u" => ((a as $uwide * b as $uwide) >> bits) as $u as u64,
            "i_add_overflows" => u64::from(sa.overflowing_add(sb).1),
            "i_add_overflows_u" => u64::from(a.overflowing_add(b).1),
            "i_sub_overflows" => u64::from(sa.overflowing_sub(sb).1),
            "i_sub_overflows_u" => u64::from(a.overflowing_sub(b).1),
            "i_mul_overflows" => u64::from(sa.overflowing_mul(sb).1),
            "i_mul_overflows_u" => u64::from(a.overflowing_mul(b).1),
            "bit_count_ones" => u64::from(a.count_ones()),
            "bit_leading_zeros" => u64::from(a.leading_zeros()),
            "bit_trailing_zeros" => u64::from(a.trailing_zeros()),
            "bit_swap_bytes" => a.swap_bytes() as u64,
            "bit_reverse" => a.reverse_bits() as u64,
            other => unreachable!("{other}"),
        };
        value
    }};
}

/// The expected result of integer operation `op` at `width`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    reason = "the operands are bits of the width"
)]
fn int_reference(op: &str, width: u32, a: u64, b: u64) -> u64 {
    match width {
        8 => int_reference!(op, a, b, u8, i8, i16, u16),
        16 => int_reference!(op, a, b, u16, i16, i32, u32),
        32 => int_reference!(op, a, b, u32, i32, i64, u64),
        _ => int_reference!(op, a, b, u64, i64, i128, u128),
    }
}

/// Checks one printed case, or says why it is wrong.
fn check(line: &str) -> Result<(), String> {
    let mut parts = line.split(' ');
    let name = parts.next().unwrap_or_default();
    let numbers: Vec<u64> = parts
        .map(|part| part.parse().map_err(|_| format!("bad number in `{line}`")))
        .collect::<Result<_, _>>()?;
    let Some((&actual, args)) = numbers.split_last() else {
        return Err(format!("no result in `{line}`"));
    };
    let (expected, width) = if let Some(rest) = name.strip_prefix("f_") {
        let (op, width) = rest.rsplit_once('_').expect("a width");
        let width: u32 = width.parse().expect("a width");
        (float_reference(op, width, args), width)
    } else if name.contains("_to_") {
        let to = name.rsplit_once('_').expect("a target").1;
        let width: u32 = to[1..].parse().expect("a width");
        (convert_reference(name, args[0]), width)
    } else {
        let (op, width) = name.rsplit_once('_').expect("a width");
        let width: u32 = width.parse().expect("a width");
        let b = args.get(1).copied().unwrap_or(0);
        (Some(int_reference(op, width, args[0], b)), 64)
    };
    match expected {
        Some(expected) if expected == actual => Ok(()),
        None if is_nan(actual, width) => Ok(()),
        Some(expected) => Err(format!("{line}: expected {expected:#x}, got {actual:#x}")),
        None => Err(format!("{line}: expected a NaN, got {actual:#x}")),
    }
}

#[test]
fn numeric_operations_match_rust() {
    let Some(sysroot) = support::sysroot() else {
        eprintln!("skipping: no lld");
        return;
    };
    let dir = std::env::temp_dir().join(format!("lilac-numeric-diff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creates scratch dir");
    std::fs::write(dir.join("numeric.lilac"), program()).expect("writes the program");
    let standard = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/Standard");
    let failures = support::parallel(&["debug", "release"], |mode| {
        let mut failures = Vec::new();
        let exe = dir.join(format!("numeric-{mode}{}", std::env::consts::EXE_SUFFIX));
        let build = Command::new(env!("CARGO_BIN_EXE_lilac"))
            .args(["build", "-q"])
            .arg(format!("--lib=Standard={}", standard.display()))
            .arg(format!("--mode={mode}"))
            .arg(format!(
                "--build-dir={}",
                dir.join(format!("build-{mode}")).display()
            ))
            .arg("-o")
            .arg(&exe)
            .arg("numeric.lilac")
            .current_dir(&dir)
            .env("LILAC_SYSROOT", &sysroot)
            .env("NO_COLOR", "1")
            .output()
            .expect("runs lilac");
        assert!(
            build.status.success(),
            "{mode} build failed:\n{}",
            String::from_utf8_lossy(&build.stderr)
        );
        let run = Command::new(&exe).output().expect("runs the program");
        assert!(run.status.success(), "{mode} run failed: {:?}", run.status);
        let output = String::from_utf8_lossy(&run.stdout);
        let mut cases = 0;
        for line in output.lines() {
            cases += 1;
            if let Err(error) = check(line.trim_end()) {
                failures.push(format!("{mode}: {error}"));
            }
        }
        assert!(cases > 100_000, "{mode}: only {cases} cases ran");
        failures
    })
    .concat();
    let _ = std::fs::remove_dir_all(&dir);
    let shown: Vec<_> = failures.iter().take(40).cloned().collect();
    assert!(
        failures.is_empty(),
        "{} wrong results:\n{}",
        failures.len(),
        shown.join("\n")
    );
}

#[test]
fn the_checker_catches_a_wrong_result() {
    // sqrt(4.0) is 2.0 (0x4000000000000000), not 4.0.
    assert!(check("f_sqrt_64 4616189618054758400 4611686018427387904").is_ok());
    assert!(check("f_sqrt_64 4616189618054758400 4616189618054758400").is_err());
    assert!(check("f_round_even_16 15872 16384").is_ok());
    assert!(check("f_round_even_16 15872 15360").is_err());
    assert!(check("i_mul_overflows_8 16 16 1").is_ok());
    assert!(check("f64_to_s8 9218868437227405312 127").is_ok());
}
