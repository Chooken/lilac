//! Float formatting and parsing (math plan phase 6) against Rust's, on
//! many inputs.
//!
//! The program draws `Float64` and `Float32` bit patterns from its own
//! generator (uniform bits, special values, and exponents within 2^16 of
//! 1.0) and a precision (mostly 0 to 20, sometimes 300 to 1099, past every
//! exact expansion), and prints one case a line: the bits, the precision,
//! `{x}`, `{x.fixed(p)}`, `{x.scientific(p)}`, then what `parse` makes of
//! the shortest and the scientific text (`1` and the bits, or `2` for
//! `Overflow`, which a finite text rounding past the largest finite
//! value is, where Rust's is an infinity). Then it prints every `Float16`
//! with its shortest text and that text parsed. This test checks each
//! line against Rust: the shortest text reads back to the same bits and
//! has the digits of Rust's `{:e}` (on an exact decimal tie Lilac takes
//! the even digit, as Ryū and JavaScript do, where Rust rounds up), fixed
//! is `{:.p$}`, scientific is `{:.p$e}`, and `parse` is `str::parse`. A
//! `Float16`'s text is read exactly ([`half_of_decimal`]) and must be as
//! short as the shortest of Rust's correctly rounded `{:.n$e}` that reads
//! back.

mod support;

use std::path::Path;
use std::process::Command;

/// `Float64` and `Float32` cases each.
const COUNT: u32 = 3000;

const PROGRAM: &str = r#"using Standard
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

// Formatted text kept for parsing.
Text: structure {
    used: @bit_native
    bytes: @array(@bit8, 0x1000)

    push: function (self, text: @byte_buffer) nothing {
        data: ref @bit8 = @byte_buffer_ptr(text)
        length: @bit_native = @byte_buffer_len(text)
        index: @bit_native = 0x0
        loop {
            if index == length or self.used == @array_len(self.bytes) { break }
            @store_u8(@array_at(ref self.bytes, 0x0), self.used, @load_u8(data, index))
            self.used = @i_add_native(self.used, 0x1)
            index = @i_add_native(index, 0x1)
        }
    }

    view: function (self) @byte_buffer => @byte_buffer_from(@array_at(ref self.bytes, 0x0), self.used)
}

empty_text: function () Text => Text { used = 0x0, bytes = @array(@bit8, 0x1000) {} }

// Mostly 0 to 20 digits, sometimes 300 to 1099 (past every exact expansion).
precision: function () p: Uint32 {
    x: @bit64 = next()
    rest: @bit64 = @bit_shift_right_u_64(x, 0x8)
    small: @bit32 = @truncate(if @bit_and_64(x, 0xF) == 0x0 then @i_add_64(0x12C, @i_mod_u_64(rest, 0x320)) else @i_mod_u_64(rest, 0x15))
    p = small as Uint32
}

special64: function (index: @bit64) r: @bit64 {
    if index == 0x0 { r = 0x0 }
    else if index == 0x1 { r = 0x8000000000000000 }
    else if index == 0x2 { r = 0x7FF0000000000000 }
    else if index == 0x3 { r = 0xFFF0000000000000 }
    else if index == 0x4 { r = 0x7FF8000000000000 }
    else if index == 0x5 { r = 0x1 }
    else if index == 0x6 { r = 0x7FEFFFFFFFFFFFFF }
    else { r = 0x10000000000000 }
}

special32: function (index: @bit64) r: @bit32 {
    if index == 0x0 { r = 0x0 }
    else if index == 0x1 { r = 0x80000000 }
    else if index == 0x2 { r = 0x7F800000 }
    else if index == 0x3 { r = 0xFF800000 }
    else if index == 0x4 { r = 0x7FC00000 }
    else if index == 0x5 { r = 0x1 }
    else if index == 0x6 { r = 0x7F7FFFFF }
    else { r = 0x800000 }
}

// Uniform bits, a special value, or an exponent within 2^16 of 1.0.
input64: function () r: @bit64 {
    pick: @bit64 = @bit_and_64(next(), 0x7)
    x: @bit64 = next()
    if pick == 0x0 {
        r = special64(@bit_and_64(x, 0x7))
    } else if @i_less_than_u_64(pick, 0x4) {
        e: @bit64 = @i_add_64(@bit_and_64(@bit_shift_right_u_64(x, 0x34), 0x1F), 0x3EF)
        r = @bit_or_64(@bit_and_64(x, 0x800FFFFFFFFFFFFF), @bit_shift_left_64(e, 0x34))
    } else {
        r = x
    }
}

input32: function () r: @bit32 {
    pick: @bit64 = @bit_and_64(next(), 0x7)
    x: @bit32 = @truncate(next())
    if pick == 0x0 {
        r = special32(@bit_and_64(next(), 0x7))
    } else if @i_less_than_u_64(pick, 0x4) {
        e: @bit32 = @i_add_32(@bit_and_32(@bit_shift_right_u_32(x, 0x17), 0x1F), 0x6F)
        r = @bit_or_32(@bit_and_32(x, 0x807FFFFF), @bit_shift_left_32(e, 0x17))
    } else {
        r = x
    }
}

// What a failed `parse` prints: 2 for `Overflow`, else 0 (1 is success).
failure: function (error: ParseError) ok: @bit8 {
    match error {
        .Overflow => ok = 0x2
        .Empty => ok = 0x0
        .InvalidDigit => ok = 0x0
        .Trailing => ok = 0x0
    }
}

parse64: function (text: @byte_buffer) (ok: @bit8, bits: Uint64) {
    if Float64.parse(text) -> value: Float64 {
        ok = 0x1
        bits = value.to_bits()
    } else -> error: ParseError {
        ok = failure(error)
        bits = 0
    }
}

parse32: function (text: @byte_buffer) (ok: @bit8, bits: Uint32) {
    if Float32.parse(text) -> value: Float32 {
        ok = 0x1
        bits = value.to_bits()
    } else -> error: ParseError {
        ok = failure(error)
        bits = 0
    }
}

parse16: function (text: @byte_buffer) (ok: @bit8, bits: Uint16) {
    if Float16.parse(text) -> value: Float16 {
        ok = 0x1
        bits = value.to_bits()
    } else -> error: ParseError {
        ok = failure(error)
        bits = 0
    }
}

// `f64 bits precision shortest fixed scientific`, then whether Lilac
// parses the shortest text and the scientific text, and to what bits.
case64: function (raw: @bit64) nothing {
    x: Float64 = Float64.from_bits(raw as Uint64)
    p: Uint32 = precision()
    shortest: Text = empty_text()
    out: Formatter = Formatter.over[Text](ref shortest, ref Text.push)
    out.write($"{x}")
    scientific: Text = empty_text()
    out = Formatter.over[Text](ref scientific, ref Text.push)
    out.write($"{x.scientific(p)}")
    ok_short: @bit8, short_bits: Uint64 = parse64(shortest.view())
    ok_sci: @bit8, sci_bits: Uint64 = parse64(scientific.view())
    print_line($"f64 {raw} {p} {x} {x.fixed(p)} {x.scientific(p)} {ok_short} {short_bits} {ok_sci} {sci_bits}")
}

case32: function (raw: @bit32) nothing {
    x: Float32 = Float32.from_bits(raw as Uint32)
    p: Uint32 = precision()
    shortest: Text = empty_text()
    out: Formatter = Formatter.over[Text](ref shortest, ref Text.push)
    out.write($"{x}")
    scientific: Text = empty_text()
    out = Formatter.over[Text](ref scientific, ref Text.push)
    out.write($"{x.scientific(p)}")
    ok_short: @bit8, short_bits: Uint32 = parse32(shortest.view())
    ok_sci: @bit8, sci_bits: Uint32 = parse32(scientific.view())
    print_line($"f32 {raw} {p} {x} {x.fixed(p)} {x.scientific(p)} {ok_short} {short_bits} {ok_sci} {sci_bits}")
}

// `f16 bits shortest`, then whether Lilac parses it and to what bits.
case16: function (raw: @bit16) nothing {
    x: Float16 = Float16.from_bits(raw as Uint16)
    shortest: Text = empty_text()
    out: Formatter = Formatter.over[Text](ref shortest, ref Text.push)
    out.write($"{x}")
    ok: @bit8, bits: Uint16 = parse16(shortest.view())
    print_line($"f16 {raw} {x} {ok} {bits}")
}

main: function () nothing {
    i: @bit32 = 0x0
    loop {
        if i == %COUNT% { break }
        case64(input64())
        case32(input32())
        i = @i_add_32(i, 0x1)
    }
    h: @bit32 = 0x0
    loop {
        if h == 0x10000 { break }
        case16(@truncate(h))
        h = @i_add_32(h, 0x1)
    }
}
"#;

/// A `Float64` or `Float32` with Rust's formatting and parsing.
#[derive(Clone, Copy)]
enum Float {
    F64(f64),
    F32(f32),
}

impl Float {
    fn from_bits(width: u32, bits: u64) -> Result<Self, String> {
        Ok(match width {
            64 => Self::F64(f64::from_bits(bits)),
            _ => Self::F32(f32::from_bits(
                u32::try_from(bits).map_err(|_| format!("{bits} is not 32 bits"))?,
            )),
        })
    }

    fn is_nan(self) -> bool {
        match self {
            Self::F64(x) => x.is_nan(),
            Self::F32(x) => x.is_nan(),
        }
    }

    /// Rust's shortest scientific text, `{:e}`.
    fn shortest(self) -> String {
        match self {
            Self::F64(x) => format!("{x:e}"),
            Self::F32(x) => format!("{x:e}"),
        }
    }

    fn fixed(self, precision: usize) -> String {
        match self {
            _ if self.is_nan() => "nan".to_owned(),
            Self::F64(x) => format!("{x:.precision$}"),
            Self::F32(x) => format!("{x:.precision$}"),
        }
    }

    fn scientific(self, precision: usize) -> String {
        match self {
            _ if self.is_nan() => "nan".to_owned(),
            Self::F64(x) => format!("{x:.precision$e}"),
            Self::F32(x) => format!("{x:.precision$e}"),
        }
    }

    /// `text` parsed at this width, as bits.
    fn parse(self, text: &str) -> Option<u64> {
        match self {
            Self::F64(_) => text.parse::<f64>().ok().map(f64::to_bits),
            Self::F32(_) => text.parse::<f32>().ok().map(|x| u64::from(x.to_bits())),
        }
    }

    fn bits_are_nan(self, bits: u64) -> bool {
        Self::from_bits(self.width(), bits).is_ok_and(Self::is_nan)
    }

    /// `|x|` as `m * 2^e`, for a finite `x`.
    fn parts(self) -> (u64, i32) {
        let (magnitude, fraction, bias) = match self {
            Self::F64(x) => (x.to_bits() & !(1 << 63), 52, 1075),
            Self::F32(x) => (u64::from(x.to_bits() & !(1 << 31)), 23, 150),
        };
        let field = i32::try_from(magnitude >> fraction).expect("small");
        let m = magnitude & ((1 << fraction) - 1);
        if field == 0 {
            (m, 1 - bias)
        } else {
            (m | 1 << fraction, field - bias)
        }
    }

    fn bits_are_infinite(self, bits: u64) -> bool {
        Self::from_bits(self.width(), bits).is_ok_and(|x| match x {
            Self::F64(x) => x.is_infinite(),
            Self::F32(x) => x.is_infinite(),
        })
    }

    fn width(self) -> u32 {
        match self {
            Self::F64(_) => 64,
            Self::F32(_) => 32,
        }
    }
}

/// A finite nonzero decimal as its significant digits (no leading or
/// trailing zeros) and the power of ten of the first digit (the value is
/// `d.ddd` times ten to it). Reads Rust's `{:e}` and Lilac's plain and
/// scientific layouts; the sign is dropped.
fn significand(text: &str) -> Option<(String, i32)> {
    let text = text.strip_prefix('-').unwrap_or(text);
    let (mantissa, exponent) = match text.split_once('e') {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().ok()?),
        None => (text, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if !whole
        .bytes()
        .chain(fraction.bytes())
        .all(|b| b.is_ascii_digit())
        || whole.is_empty()
    {
        return None;
    }
    let all = format!("{whole}{fraction}");
    let lead = all.bytes().take_while(|&b| b == b'0').count();
    let digits = all[lead..].trim_end_matches('0');
    if digits.is_empty() {
        return None;
    }
    let point = i32::try_from(whole.len()).ok()? - i32::try_from(lead).ok()? - 1;
    Some((digits.to_owned(), exponent + point))
}

/// Lilac's shortest layout of `digits * 10^exponent` (see
/// `write_float_shortest`): plain for exponents 0 to 15, `0.000ddd` for -1
/// to -4, else scientific.
fn lilac_layout(negative: bool, digits: &str, exponent: i32) -> String {
    let sign = if negative { "-" } else { "" };
    let count = i32::try_from(digits.len()).expect("short");
    let body = if (0..16).contains(&exponent) {
        if exponent + 1 >= count {
            format!(
                "{digits}{}",
                "0".repeat(usize::try_from(exponent + 1 - count).expect("small"))
            )
        } else {
            let (whole, fraction) = digits.split_at(usize::try_from(exponent + 1).expect("small"));
            format!("{whole}.{fraction}")
        }
    } else if (-4..0).contains(&exponent) {
        format!(
            "0.{}{digits}",
            "0".repeat(usize::try_from(-exponent - 1).expect("small"))
        )
    } else if digits.len() > 1 {
        format!("{}.{}e{exponent}", &digits[..1], &digits[1..])
    } else {
        format!("{digits}e{exponent}")
    };
    format!("{sign}{body}")
}

/// Checks Lilac's shortest text `text` for `x`.
fn check_shortest(x: Float, bits: u64, text: &str) -> Result<(), String> {
    let rust = x.shortest();
    let expected = match rust.as_str() {
        _ if x.is_nan() => "nan".to_owned(),
        "inf" | "-inf" => rust.clone(),
        "0e0" => "0".to_owned(),
        "-0e0" => "-0".to_owned(),
        _ => {
            let (digits, exponent) = significand(&rust).ok_or("bad Rust text")?;
            lilac_layout(rust.starts_with('-'), &digits, exponent)
        }
    };
    if text == expected {
        return Ok(());
    }
    // An exact tie between two shortest decimals: Lilac takes the even
    // one, Rust the one above.
    let tie = (|| {
        let (lilac_digits, lilac_exponent) = significand(text)?;
        let (rust_digits, rust_exponent) = significand(&rust)?;
        let lilac_value: u64 = lilac_digits.parse().ok()?;
        let rust_value: u64 = rust_digits.parse().ok()?;
        let even = lilac_value.is_multiple_of(2);
        let neighbour = lilac_digits.len() == rust_digits.len()
            && lilac_exponent == rust_exponent
            && lilac_value + 1 == rust_value;
        let laid_out = lilac_layout(rust.starts_with('-'), &lilac_digits, lilac_exponent);
        let power = lilac_exponent - i32::try_from(lilac_digits.len()).ok()? + 1;
        Some(
            even && neighbour
                && laid_out == text
                && x.parse(text) == Some(bits)
                && is_midpoint(x, lilac_value, power),
        )
    })();
    if tie == Some(true) {
        Ok(())
    } else {
        Err(format!("shortest `{text}`, expected `{expected}`"))
    }
}

/// Whether `|x|` is exactly `(value + 1/2) * 10^power`, halfway between
/// `value` and `value + 1` at that power: with `|x| = m * 2^e` and `m`'s odd
/// part `odd * 2^a`, `odd * 2^(a + e + 1) = (2 value + 1) * 2^power *
/// 5^power`, so the twos and the odd parts match.
fn is_midpoint(x: Float, value: u64, power: i32) -> bool {
    let (m, e) = x.parts();
    if m == 0 {
        return false;
    }
    let a = i32::try_from(m.trailing_zeros()).expect("small");
    let odd = u128::from(m >> a);
    let twice = 2 * u128::from(value) + 1;
    let fives = |n: i32| 5u128.checked_pow(n.unsigned_abs());
    let odd_parts = if power >= 0 {
        fives(power).and_then(|f| f.checked_mul(twice)) == Some(odd)
    } else {
        fives(power).and_then(|f| f.checked_mul(odd)) == Some(twice)
    };
    a + e + 1 == power && odd_parts
}

/// Checks that Lilac parsed `text` (`ok`, to `actual`) as Rust does. Where
/// Rust's is an infinity from a finite text, Lilac's is `Overflow` (2).
fn check_parse(x: Float, text: &str, ok: &str, actual: u64) -> Result<(), String> {
    let Some(expected) = x.parse(text) else {
        return Err(format!("Rust cannot parse `{text}`"));
    };
    let finite_text = !text.trim_start_matches('-').starts_with("inf");
    let overflows = finite_text && x.bits_are_infinite(expected);
    match ok {
        "2" if overflows => Ok(()),
        _ if overflows => Err(format!("parse `{text}` did not overflow")),
        "1" if expected == actual => Ok(()),
        "1" if x.bits_are_nan(expected) && x.bits_are_nan(actual) => Ok(()),
        "1" => Err(format!(
            "parse `{text}` gave {actual:#x}, expected {expected:#x}"
        )),
        _ => Err(format!("parse `{text}` failed, expected {expected:#x}")),
    }
}

/// Checks one `f64`/`f32` line.
fn check_float(fields: &[&str]) -> Result<(), String> {
    let [name, bits, precision, shortest, fixed, scientific, ok_short, short_bits, ok_sci, sci_bits] =
        fields
    else {
        return Err("wrong field count".to_owned());
    };
    let number = |text: &str| {
        text.parse::<u64>()
            .map_err(|_| format!("bad number `{text}`"))
    };
    let width = if *name == "f64" { 64 } else { 32 };
    let bits = number(bits)?;
    let x = Float::from_bits(width, bits)?;
    let precision = usize::try_from(number(precision)?).map_err(|_| "precision")?;
    check_shortest(x, bits, shortest)?;
    let expected = x.fixed(precision);
    if *fixed != expected {
        return Err(format!(
            "fixed({precision}) `{fixed}`, expected `{expected}`"
        ));
    }
    let expected = x.scientific(precision);
    if *scientific != expected {
        return Err(format!(
            "scientific({precision}) `{scientific}`, expected `{expected}`"
        ));
    }
    check_parse(x, shortest, ok_short, number(short_bits)?)?;
    check_parse(x, scientific, ok_sci, number(sci_bits)?)
}

/// Exact 16-bit float rounding.
mod half {
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

    /// The exact value of a finite half as a double.
    pub fn to_f64(bits: u16) -> f64 {
        let field = i32::from((bits >> 10) & 0x1F);
        let fraction = f64::from(bits & 0x3FF);
        let value = if field == 0 {
            fraction * 2f64.powi(-24)
        } else {
            (fraction + 1024.0) * 2f64.powi(field - 25)
        };
        if bits & 0x8000 != 0 {
            -value
        } else {
            value
        }
    }
}

/// The decimal `text` (Lilac's or Rust's layout, `inf` or `nan`) rounded
/// exactly to the nearest half, or `None` when it is no number.
fn half_of_decimal(text: &str) -> Option<u16> {
    let negative = text.starts_with('-');
    let sign = if negative { 0x8000 } else { 0 };
    match text.trim_start_matches('-') {
        "nan" => return Some(0x7E00),
        "inf" => return Some(0x7C00 | sign),
        "0" | "0e0" => return Some(sign),
        _ => {}
    }
    let (digits, exponent) = significand(text)?;
    let value: u128 = digits.parse().ok()?;
    // `value * 10^power`, the digits read as an integer.
    let power = exponent - (i32::try_from(digits.len()).ok()? - 1);
    if power >= 0 {
        let scale = 10u128.checked_pow(u32::try_from(power).ok()?)?;
        return Some(half::round(negative, value.checked_mul(scale)?, 0));
    }
    // `value / 10^-power` to 64 more bits, with a sticky bit below them
    // for any remainder, so it rounds as the exact quotient does.
    let divisor = 10u128.checked_pow(u32::try_from(-power).ok()?)?;
    let scaled = value.checked_mul(1 << 64)?;
    let quotient = (scaled / divisor) << 1 | u128::from(scaled % divisor != 0);
    Some(half::round(negative, quotient, -65))
}

/// Checks one `f16` line: the text reads back, through Lilac's `parse`
/// and exactly, and no correctly rounded shorter decimal does.
fn check_half(fields: &[&str]) -> Result<(), String> {
    let [_, bits, text, ok, parsed] = fields else {
        return Err("wrong field count".to_owned());
    };
    let bits: u16 = bits.parse().map_err(|_| "bad bits")?;
    let nan = |b: u16| b & 0x7C00 == 0x7C00 && b & 0x3FF != 0;
    let same = |b: u16| b == bits || (nan(b) && nan(bits));
    let parsed: u16 = parsed.parse().map_err(|_| "bad parsed bits")?;
    if *ok != "1" || !same(parsed) {
        return Err(format!("`{text}` parsed by Lilac to {parsed:#x}"));
    }
    let exact = half_of_decimal(text).ok_or_else(|| format!("`{text}` is no decimal"))?;
    if !same(exact) {
        return Err(format!("`{text}` is {exact:#x}"));
    }
    if bits & 0x7C00 == 0x7C00 || bits.trailing_zeros() >= 15 {
        return Ok(());
    }
    let value = half::to_f64(bits);
    let shortest = (1..=5)
        .find(|&n| half_of_decimal(&format!("{value:.*e}", n - 1)) == Some(bits))
        .ok_or("no Rust decimal reads back")?;
    let (digits, _) = significand(text).ok_or("bad text")?;
    if digits.len() > shortest {
        return Err(format!("`{text}` is longer than {shortest} digits"));
    }
    Ok(())
}

/// Checks one printed line, or says why it is wrong.
fn check(line: &str) -> Result<(), String> {
    let fields: Vec<&str> = line.split(' ').collect();
    match fields.first() {
        Some(&"f64" | &"f32") => check_float(&fields),
        Some(&"f16") => check_half(&fields),
        _ => Err("unknown case".to_owned()),
    }
    .map_err(|error| format!("{}: {error}", line.chars().take(200).collect::<String>()))
}

#[test]
fn float_formatting_matches_rust() {
    let Some(sysroot) = support::sysroot() else {
        eprintln!("skipping: no lld");
        return;
    };
    let dir = std::env::temp_dir().join(format!("lilac-float-format-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creates scratch dir");
    let program = PROGRAM.replace("%COUNT%", &format!("{COUNT:#x}"));
    std::fs::write(dir.join("format.lilac"), program).expect("writes the program");
    let standard = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/Standard");
    let mut failures = Vec::new();
    for mode in ["debug", "release"] {
        let exe = dir.join(format!("format-{mode}{}", std::env::consts::EXE_SUFFIX));
        let build = Command::new(env!("CARGO_BIN_EXE_lilac"))
            .args(["build", "-q"])
            .arg(format!("--lib=Standard={}", standard.display()))
            .arg(format!("--mode={mode}"))
            .arg("-o")
            .arg(&exe)
            .arg("format.lilac")
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
        let (mut floats, mut halves) = (0, 0);
        for line in output.lines() {
            if line.starts_with("f16") {
                halves += 1;
            } else {
                floats += 1;
            }
            if let Err(error) = check(line.trim_end()) {
                failures.push(format!("{mode}: {error}"));
            }
        }
        assert_eq!(floats, 2 * COUNT, "{mode}: float cases");
        assert_eq!(halves, 0x10000, "{mode}: Float16 cases");
    }
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
    // 0.1: shortest `0.1`, fixed(2) `0.10`, scientific(2) `1.00e-1`.
    let good =
        "f64 4591870180066957722 2 0.1 0.10 1.00e-1 1 4591870180066957722 1 4591870180066957722";
    assert_eq!(check(good), Ok(()));
    // Past the largest finite value, Lilac's `parse` is `Overflow`.
    let max = "f64 9218868437227405311 0 1.7976931348623157e308";
    let fixed = format!("{:.0}", f64::MAX);
    assert_eq!(
        check(&format!("{max} {fixed} 2e308 1 9218868437227405311 2 0")),
        Ok(())
    );
    assert!(check(&format!(
        "{max} {fixed} 2e308 1 9218868437227405311 1 9218868437227405311"
    ))
    .is_err());
    for bad in [
        "f64 4591870180066957722 2 0.10000000000000001 0.10 1.00e-1 1 4591870180066957722 1 4591870180066957722",
        "f64 4591870180066957722 2 0.1 0.11 1.00e-1 1 4591870180066957722 1 4591870180066957722",
        "f64 4591870180066957722 2 0.1 0.10 1.0e-1 1 4591870180066957722 1 4591870180066957722",
        "f64 4591870180066957722 2 0.1 0.10 1.00e-1 1 4591870180066957723 1 4591870180066957722",
        "f64 4591870180066957722 2 0.1 0.10 1.00e-1 1 4591870180066957722 0 0",
        "f64 4591870180066957722 2 1e-1 0.10 1.00e-1 1 4591870180066957722 1 4591870180066957722",
    ] {
        assert!(check(bad).is_err(), "{bad}");
    }
    // 2^-24 is 5.9604645e-8; `6e-8` reads back as the same half.
    assert_eq!(check("f16 1 6e-8 1 1"), Ok(()));
    assert!(check("f16 1 5.96e-8 1 1").is_err());
    assert!(check("f16 1 1e-7 1 1").is_err());
    assert!(check("f16 15360 1 1 15361").is_err());
    // An even neighbour that reads back is no tie unless the value is
    // exactly halfway: 2^-1074 is 4.94...e-324.
    assert!(check_shortest(Float::F64(5e-324), 1, "4e-324").is_err());
    // 514845.625 (0x48FB63B4) is exactly halfway: Lilac takes `514845.62`,
    // Rust `514845.63`.
    let tie_bits = 0x48FB_63B4;
    let tie = Float::from_bits(32, tie_bits).expect("32 bits");
    assert_eq!(check_shortest(tie, tie_bits, "514845.62"), Ok(()));
    assert!(check_shortest(tie, tie_bits, "514845.64").is_err());
    assert!(is_midpoint(Float::F64(2.5), 2, 0));
    assert!(is_midpoint(Float::F32(0.125), 12, -2));
    assert!(!is_midpoint(Float::F64(2.5), 3, 0));
    assert!(!is_midpoint(Float::F64(0.1), 0, -1));
}
