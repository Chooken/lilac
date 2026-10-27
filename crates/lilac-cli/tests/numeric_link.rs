//! Every numeric operation links on every native target (math plan phase
//! 0): LLVM may turn an operation into a call to a C runtime routine
//! (`fmod`, `__extendhfsf2`, `__divdi3`, …), which a native program does
//! not link, so the backend must define each one it can reach.
//!
//! The program has one `export` per intrinsic and width (an export is
//! kept and its arguments are unknown, so nothing folds away), and it is
//! built for each native target at `debug` and `release`, cross-linked
//! with the development sysroot's `lld`: an undefined symbol fails the
//! link. Each later phase adds its intrinsics to [`program`] (phase 1's
//! rounding, fused multiply-add, conversions and bit counts are there).

mod support;

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

/// The native targets: every `Target::link_info` pair.
const TARGETS: [&str; 6] = [
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];

const INT_WIDTHS: [&str; 5] = ["8", "16", "32", "64", "native"];

/// `@fbit128` arithmetic is `E078`, so it never reaches a backend.
const FLOAT_WIDTHS: [&str; 3] = ["16", "32", "64"];

const INT_BINARY: [&str; 7] = ["add", "sub", "mul", "div", "mod", "div_u", "mod_u"];

const BIT_BINARY: [&str; 6] = [
    "and",
    "or",
    "xor",
    "shift_left",
    "shift_right",
    "shift_right_u",
];

const INT_COMPARE: [&str; 8] = [
    "less_than",
    "greater_than",
    "less_than_or_equal",
    "greater_than_or_equal",
    "less_than_u",
    "greater_than_u",
    "less_than_or_equal_u",
    "greater_than_or_equal_u",
];

const FLOAT_BINARY: [&str; 8] = ["add", "sub", "mul", "div", "mod", "min", "max", "copy_sign"];

const FLOAT_UNARY: [&str; 8] = [
    "negate",
    "sqrt",
    "abs",
    "floor",
    "ceil",
    "trunc",
    "round",
    "round_even",
];

/// The phase 1 integer operations with two operands, and whether the
/// result is the `@bit8` flag.
const INT_PHASE1: [(&str, bool); 14] = [
    ("bit_rotate_left", false),
    ("bit_rotate_right", false),
    ("i_add_saturating", false),
    ("i_add_saturating_u", false),
    ("i_sub_saturating", false),
    ("i_sub_saturating_u", false),
    ("i_multiply_high", false),
    ("i_multiply_high_u", false),
    ("i_add_overflows", true),
    ("i_add_overflows_u", true),
    ("i_sub_overflows", true),
    ("i_sub_overflows_u", true),
    ("i_mul_overflows", true),
    ("i_mul_overflows_u", true),
];

const INT_UNARY: [&str; 7] = [
    "i_negate",
    "bit_not",
    "bit_count_ones",
    "bit_leading_zeros",
    "bit_trailing_zeros",
    "bit_swap_bytes",
    "bit_reverse",
];

const FLOAT_COMPARE: [&str; 4] = [
    "less_than",
    "greater_than",
    "less_than_or_equal",
    "greater_than_or_equal",
];

/// Phase 5's methods (`Standard`'s elementary functions): written in Lilac
/// on the operations above, so they must link wherever those do.
const MATH_UNARY: [&str; 28] = [
    "exp", "exp2", "exp10", "exp_m1", "ln", "log2", "log10", "ln_1p", "sin", "cos", "tan",
    "sin_pi", "cos_pi", "tan_pi", "atan", "asin", "acos", "sinh", "cosh", "tanh", "asinh", "acosh",
    "atanh", "erf", "erfc", "gamma", "ln_gamma", "cbrt",
];

const MATH_BINARY: [&str; 4] = ["pow", "hypot", "atan2", "log"];

/// The probe program: a `main` and one export per operation and width.
#[allow(clippy::too_many_lines, reason = "one template")]
fn program() -> String {
    let mut text = String::from("using Standard\n\nmain: function () nothing {\n}\n");
    for width in INT_WIDTHS {
        let ty = format!("@bit{width}");
        let ty = if width == "native" {
            "@bit_native".to_owned()
        } else {
            ty
        };
        let binary = INT_BINARY
            .iter()
            .map(|op| format!("i_{op}"))
            .chain(BIT_BINARY.iter().map(|op| format!("bit_{op}")));
        for name in binary {
            writeln!(
                text,
                "probe_{name}_{width}: export function (a: {ty}, b: {ty}) r: {ty} {{\n    r = @{name}_{width}(a, b)\n}}\n"
            )
            .expect("writes to a string");
        }
        for op in INT_COMPARE {
            writeln!(
                text,
                "probe_i_{op}_{width}: export function (a: {ty}, b: {ty}) r: @bit8 {{\n    r = @i_{op}_{width}(a, b)\n}}\n"
            )
            .expect("writes to a string");
        }
        for (name, flag) in INT_PHASE1 {
            let result = if flag { "@bit8" } else { &ty };
            writeln!(
                text,
                "probe_{name}_{width}: export function (a: {ty}, b: {ty}) r: {result} {{\n    r = @{name}_{width}(a, b)\n}}\n"
            )
            .expect("writes to a string");
        }
        for name in INT_UNARY {
            if name == "bit_swap_bytes" && width == "8" {
                continue;
            }
            writeln!(
                text,
                "probe_{name}_{width}: export function (a: {ty}) r: {ty} {{\n    r = @{name}_{width}(a)\n}}\n"
            )
            .expect("writes to a string");
        }
    }
    for width in FLOAT_WIDTHS {
        let ty = format!("@bit{width}");
        let float = |name: &str| format!("@bits_to_fbit_{width}({name})");
        for op in FLOAT_BINARY {
            writeln!(
                text,
                "probe_f_{op}_{width}: export function (a: {ty}, b: {ty}) r: {ty} {{\n    r = @fbit_to_bits_{width}(@f_{op}_{width}({}, {}))\n}}\n",
                float("a"),
                float("b")
            )
            .expect("writes to a string");
        }
        for op in FLOAT_COMPARE {
            writeln!(
                text,
                "probe_f_{op}_{width}: export function (a: {ty}, b: {ty}) r: @bit8 {{\n    r = @f_{op}_{width}({}, {})\n}}\n",
                float("a"),
                float("b")
            )
            .expect("writes to a string");
        }
        for op in FLOAT_UNARY {
            writeln!(
                text,
                "probe_f_{op}_{width}: export function (a: {ty}) r: {ty} {{\n    r = @fbit_to_bits_{width}(@f_{op}_{width}({}))\n}}\n",
                float("a")
            )
            .expect("writes to a string");
        }
        writeln!(
            text,
            "probe_f_multiply_add_{width}: export function (a: {ty}, b: {ty}, c: {ty}) r: {ty} {{\n    r = @fbit_to_bits_{width}(@f_multiply_add_{width}({}, {}, {}))\n}}\n",
            float("a"),
            float("b"),
            float("c")
        )
        .expect("writes to a string");
        for int in INT_WIDTHS {
            let int_ty = if int == "native" {
                "@bit_native".to_owned()
            } else {
                format!("@bit{int}")
            };
            for op in ["float_to_int", "float_to_uint"] {
                writeln!(
                    text,
                    "probe_{op}_{width}_{int}: export function (a: {ty}) r: {int_ty} {{\n    r = @{op}({})\n}}\n",
                    float("a")
                )
                .expect("writes to a string");
            }
            for op in ["int_to_float", "uint_to_float"] {
                writeln!(
                    text,
                    "probe_{op}_{int}_{width}: export function (a: {int_ty}) r: {ty} {{\n    value: @fbit{width} = @{op}(a)\n    r = @fbit_to_bits_{width}(value)\n}}\n"
                )
                .expect("writes to a string");
            }
        }
        for to in FLOAT_WIDTHS {
            if to != width {
                writeln!(
                    text,
                    "probe_float_convert_{width}_{to}: export function (a: {ty}) r: @bit{to} {{\n    value: @fbit{to} = @float_convert({})\n    r = @fbit_to_bits_{to}(value)\n}}\n",
                    float("a")
                )
                .expect("writes to a string");
            }
        }
    }
    for (float, bits) in [("Float16", 16), ("Float32", 32), ("Float64", 64)] {
        for (index, name) in MATH_UNARY.iter().enumerate() {
            writeln!(
                text,
                "probe_math_{name}_{float}_{index}: export function (a: @bit{bits}) r: @bit{bits} {{
    x: {float} = {float}.from_bits(a as Uint{bits})
    r = x.{name}().to_bits() as @bit{bits}
}}
"
            )
            .expect("writes to a string");
        }
        for (index, name) in MATH_BINARY.iter().enumerate() {
            writeln!(
                text,
                "probe_math_{name}_{float}_{index}: export function (a: @bit{bits}, b: @bit{bits}) r: @bit{bits} {{
    x: {float} = {float}.from_bits(a as Uint{bits})
    y: {float} = {float}.from_bits(b as Uint{bits})
    r = x.{name}(y).to_bits() as @bit{bits}
}}
"
            )
            .expect("writes to a string");
        }
        writeln!(
            text,
            "probe_math_sin_cos_{float}: export function (a: @bit{bits}) r: @bit{bits} {{
    x: {float} = {float}.from_bits(a as Uint{bits})
    s: {float}, c: {float} = x.sin_cos()
    r = (s + c).to_bits() as @bit{bits}
}}

probe_math_ints_{float}: export function (a: @bit{bits}, n: @bit32) r: @bit{bits} {{
    x: {float} = {float}.from_bits(a as Uint{bits})
    r = (x.pow_int(n as Int32) + x.root(n as Int32)).to_bits() as @bit{bits}
}}
"
        )
        .expect("writes to a string");
    }
    text
}

#[test]
fn every_numeric_operation_links_natively() {
    let Some(sysroot) = support::sysroot() else {
        eprintln!("skipping: no lld");
        return;
    };
    let dir = std::env::temp_dir().join(format!("lilac-numeric-link-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creates scratch dir");
    std::fs::write(dir.join("numeric.lilac"), program()).expect("writes the program");
    let standard = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/Standard");
    let builds: Vec<(&str, &str)> = TARGETS
        .iter()
        .flat_map(|triple| ["debug", "release"].map(|mode| (*triple, mode)))
        .collect();
    let failures: Vec<String> = support::parallel(&builds, |(triple, mode)| {
        let out = Command::new(env!("CARGO_BIN_EXE_lilac"))
            .args(["build", "-q"])
            .arg(format!("--lib=Standard={}", standard.display()))
            .arg(format!("--target={triple}"))
            .arg(format!("--mode={mode}"))
            .arg(format!(
                "--build-dir={}",
                dir.join(format!("build-{triple}-{mode}")).display()
            ))
            .arg("-o")
            .arg(dir.join(format!("{triple}-{mode}")))
            .arg("numeric.lilac")
            .current_dir(&dir)
            .env("LILAC_SYSROOT", &sysroot)
            .env("NO_COLOR", "1")
            .output()
            .expect("runs lilac");
        (!out.status.success()).then(|| {
            format!(
                "{triple} ({mode}):\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
    })
    .into_iter()
    .flatten()
    .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
