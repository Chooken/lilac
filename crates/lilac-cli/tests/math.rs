//! The elementary functions (math plan phase 5) against reference tables.
//!
//! `tools/math-reference/` holds a generator (`gen_tests.py`, standard
//! library only) and the tables it wrote: for each function and float
//! width, rows of argument bit patterns and the correctly rounded
//! result's, in hexadecimal. This test builds one Lilac program per family
//! that embeds those tables as string literals, checks every row against
//! the `Standard` method (a NaN expectation accepts any NaN), and prints
//! one line per failing row. The program runs in Debug and in Release.
//!
//! Regenerate a table with
//! `python tools/math-reference/gen_tests.py <function>`.

mod support;

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

/// A function under test: its table name, how a row's arguments reach it
/// (`x`, and `y` for two arguments), and the family program it belongs to.
struct Table {
    name: &'static str,
    call: &'static str,
    arity: u32,
    /// The second argument is an `Int32` (eight digits), not a float.
    int_second: bool,
}

macro_rules! data {
    ($name:literal) => {
        [
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tools/math-reference/data/",
                $name,
                ".16.txt"
            )),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tools/math-reference/data/",
                $name,
                ".32.txt"
            )),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tools/math-reference/data/",
                $name,
                ".64.txt"
            )),
        ]
    };
}

const fn t(name: &'static str, call: &'static str, arity: u32) -> Table {
    Table {
        name,
        call,
        arity,
        int_second: false,
    }
}

const fn ti(name: &'static str, call: &'static str) -> Table {
    Table {
        name,
        call,
        arity: 2,
        int_second: true,
    }
}

const EXPONENTIAL: &[Table] = &[
    t("exp", "x.exp()", 1),
    t("exp2", "x.exp2()", 1),
    t("exp10", "x.exp10()", 1),
    t("exp_m1", "x.exp_m1()", 1),
    t("ln", "x.ln()", 1),
    t("log2", "x.log2()", 1),
    t("log10", "x.log10()", 1),
    t("ln_1p", "x.ln_1p()", 1),
    t("log", "x.log(y)", 2),
];

fn exponential_data() -> Vec<[&'static str; 3]> {
    vec![
        data!("exp"),
        data!("exp2"),
        data!("exp10"),
        data!("exp_m1"),
        data!("ln"),
        data!("log2"),
        data!("log10"),
        data!("ln_1p"),
        data!("log"),
    ]
}

const POWER: &[Table] = &[
    t("pow", "x.pow(y)", 2),
    ti("pow_int", "x.pow_int(y)"),
    t("cbrt", "x.cbrt()", 1),
    t("hypot", "x.hypot(y)", 2),
    ti("root", "x.root(y)"),
];

fn power_data() -> Vec<[&'static str; 3]> {
    vec![
        data!("pow"),
        data!("pow_int"),
        data!("cbrt"),
        data!("hypot"),
        data!("root"),
    ]
}

const TRIGONOMETRY: &[Table] = &[
    t("sin", "x.sin()", 1),
    t("cos", "x.cos()", 1),
    t("tan", "x.tan()", 1),
    t("sin_pi", "x.sin_pi()", 1),
    t("cos_pi", "x.cos_pi()", 1),
    t("tan_pi", "x.tan_pi()", 1),
];

fn trigonometry_data() -> Vec<[&'static str; 3]> {
    vec![
        data!("sin"),
        data!("cos"),
        data!("tan"),
        data!("sin_pi"),
        data!("cos_pi"),
        data!("tan_pi"),
    ]
}

const INVERSE: &[Table] = &[
    t("atan", "x.atan()", 1),
    t("asin", "x.asin()", 1),
    t("acos", "x.acos()", 1),
    t("atan2", "x.atan2(y)", 2),
];

fn inverse_data() -> Vec<[&'static str; 3]> {
    vec![data!("atan"), data!("asin"), data!("acos"), data!("atan2")]
}

const HYPERBOLIC: &[Table] = &[
    t("sinh", "x.sinh()", 1),
    t("cosh", "x.cosh()", 1),
    t("tanh", "x.tanh()", 1),
    t("asinh", "x.asinh()", 1),
    t("acosh", "x.acosh()", 1),
    t("atanh", "x.atanh()", 1),
];

fn hyperbolic_data() -> Vec<[&'static str; 3]> {
    vec![
        data!("sinh"),
        data!("cosh"),
        data!("tanh"),
        data!("asinh"),
        data!("acosh"),
        data!("atanh"),
    ]
}

const SPECIAL: &[Table] = &[
    t("erf", "x.erf()", 1),
    t("erfc", "x.erfc()", 1),
    t("gamma", "x.gamma()", 1),
    t("ln_gamma", "x.ln_gamma()", 1),
];

fn special_data() -> Vec<[&'static str; 3]> {
    vec![
        data!("erf"),
        data!("erfc"),
        data!("gamma"),
        data!("ln_gamma"),
    ]
}

const WIDTHS: [(u32, &str, &str); 3] = [
    (16, "Float16", "Uint16"),
    (32, "Float32", "Uint32"),
    (64, "Float64", "Uint64"),
];

/// The Lilac program for a family: a checker per table and width.
fn program(tables: &[Table], data: &[[&'static str; 3]]) -> String {
    let mut text = String::from(HEADER);
    let mut calls = String::new();
    for (table, rows) in tables.iter().zip(data) {
        for (index, (bits, float, uint)) in WIDTHS.iter().enumerate() {
            let digits = bits / 4;
            let compact: String = rows[index]
                .lines()
                .map(|line| line.split_whitespace().collect::<String>())
                .collect();
            let second = if table.int_second { 8 } else { digits };
            let result_at =
                digits * (table.arity - 1) + if table.arity > 1 { second } else { digits };
            let width = (result_at + digits) as usize;
            assert_eq!(compact.len() % width, 0, "{} {bits}", table.name);
            let count = compact.len() / width;
            assert!(count > 20, "{} {bits}: {count} rows", table.name);
            let container = format!("@bit{bits}");
            let mut inputs = String::new();
            for argument in 0..table.arity {
                let name = ["x", "y"][argument as usize];
                let raw = ["a0", "a1"][argument as usize];
                let int_arg = table.int_second && argument == 1;
                let (arg_digits, arg_bits) = if int_arg { (8, 32) } else { (digits, *bits) };
                let _ = writeln!(
                    inputs,
                    "        {raw}: @bit64 = hex(data, @i_add_native(at, 0x{:X}), 0x{arg_digits:X})",
                    argument * digits
                );
                let _ = writeln!(
                    inputs,
                    "        {raw}t: @bit{arg_bits} = {}",
                    truncate(raw, arg_bits)
                );
                if int_arg {
                    let _ = writeln!(inputs, "        {name}: Int32 = {raw}t as Int32");
                } else {
                    let _ = writeln!(
                        inputs,
                        "        {name}: {float} = {float}.from_bits({raw}t as {uint})"
                    );
                }
            }
            let _ = write!(
                text,
                r#"
check_{name}_{bits}: function () bad: @bit_native {{
    data: @byte_buffer = "{compact}"
    row: @bit_native = 0x0
    bad = 0x0
    loop {{
        if row == 0x{count:X} {{ break }}
        at: @bit_native = @i_mul_native(row, 0x{width:X})
{inputs}        wb: @bit64 = hex(data, @i_add_native(at, 0x{result_at:X}), 0x{digits:X})
        wt: {container} = {want_truncate}
        want: {float} = {float}.from_bits(wt as {uint})
        got: {float} = {call}
        gb: {uint} = got.to_bits()
        if gb != (wt as {uint}) and not (got.is_nan() and want.is_nan()) {{
            bad = @i_add_native(bad, 0x1)
            fail_row("{name}.{bits}", row, a0, gb as Uint64, wb as Uint64)
        }}
        row = @i_add_native(row, 0x1)
    }}
    print_line($"checked {name}.{bits} {{row}} rows")
}}
"#,
                name = table.name,
                bits = bits,
                count = count,
                width = width,
                inputs = inputs,
                result_at = result_at,
                digits = digits,
                container = container,
                want_truncate = truncate("wb", *bits),
                float = float,
                uint = uint,
                call = table.call,
            );
            let _ = writeln!(
                calls,
                "    bad = @i_add_native(bad, check_{}_{bits}())",
                table.name
            );
        }
    }
    let _ = write!(
        text,
        r#"
main: function () nothing {{
    bad: @bit_native = 0x0
{calls}    if bad != 0x0 {{
        print_line($"FAILED {{bad}} rows")
        exit(0x1)
    }}
}}
"#
    );
    text
}

fn truncate(name: &str, bits: u32) -> String {
    if bits == 64 {
        name.to_string()
    } else {
        format!("@truncate({name})")
    }
}

const HEADER: &str = r#"using Standard
using Standard.Io
using Standard.Process

failures_shown: @bit_native

hex: function (data: @byte_buffer, at: @bit_native, count: @bit_native) value: @bit64 {
    ptr: ref @bit8 = @byte_buffer_ptr(data)
    value = 0x0
    i: @bit_native = 0x0
    loop {
        if i == count { break }
        c: @bit8 = @load_u8(ptr, @i_add_native(at, i))
        d: @bit8 = if @i_less_than_u_8(c, 0x3A) then @i_sub_8(c, 0x30) else @i_sub_8(c, 0x57)
        value = @bit_or_64(@bit_shift_left_64(value, 0x4), @extend_zero(d))
        i = @i_add_native(i, 0x1)
    }
}

fail_row: function (name: @byte_buffer, row: @bit_native, input: @bit64, got: Uint64, want: Uint64) nothing {
    if @i_less_than_u_native(failures_shown, 0x40) {
        failures_shown = @i_add_native(failures_shown, 0x1)
        print_line($"FAIL {name} row {row} input {(input as Uint64).hex()} got {got.hex()} want {want.hex()}")
    }
}
"#;

fn run_family(label: &str, tables: &[Table], data: &[[&'static str; 3]]) {
    let Some(sysroot) = support::sysroot() else {
        eprintln!("skipping: no lld");
        return;
    };
    let dir = std::env::temp_dir().join(format!("lilac-math-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creates scratch dir");
    let source = program(tables, data);
    std::fs::write(dir.join("math.lilac"), &source).expect("writes the program");
    let standard = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/Standard");
    let mut failures = Vec::new();
    for mode in ["debug", "release"] {
        let exe = dir.join(format!("math-{mode}{}", std::env::consts::EXE_SUFFIX));
        let build = Command::new(env!("CARGO_BIN_EXE_lilac"))
            .args(["build", "-q"])
            .arg(format!("--lib=Standard={}", standard.display()))
            .arg(format!("--mode={mode}"))
            .arg("-o")
            .arg(&exe)
            .arg("math.lilac")
            .current_dir(&dir)
            .env("LILAC_SYSROOT", &sysroot)
            .env("NO_COLOR", "1")
            .output()
            .expect("runs lilac");
        assert!(
            build.status.success(),
            "{label} {mode} build failed:\n{}",
            String::from_utf8_lossy(&build.stderr)
        );
        let run = Command::new(&exe).output().expect("runs the program");
        let output = String::from_utf8_lossy(&run.stdout);
        let checked = output.lines().filter(|l| l.starts_with("checked ")).count();
        for line in output.lines().filter(|l| l.starts_with("FAIL")) {
            failures.push(format!("{mode}: {line}"));
        }
        assert_eq!(
            checked,
            tables.len() * WIDTHS.len(),
            "{label} {mode}: not every table ran:\n{output}{}",
            String::from_utf8_lossy(&run.stderr)
        );
        if !run.status.success() && failures.is_empty() {
            failures.push(format!("{mode}: exit {:?}", run.status));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        failures.is_empty(),
        "{} wrong rows:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn exponential_functions_match_the_reference() {
    run_family("exponential", EXPONENTIAL, &exponential_data());
}

#[test]
fn power_and_root_functions_match_the_reference() {
    run_family("power", POWER, &power_data());
}

#[test]
fn trigonometric_functions_match_the_reference() {
    run_family("trigonometry", TRIGONOMETRY, &trigonometry_data());
}

#[test]
fn inverse_trigonometric_functions_match_the_reference() {
    run_family("inverse", INVERSE, &inverse_data());
}

#[test]
fn hyperbolic_functions_match_the_reference() {
    run_family("hyperbolic", HYPERBOLIC, &hyperbolic_data());
}

#[test]
fn special_functions_match_the_reference() {
    run_family("special", SPECIAL, &special_data());
}

/// The unary methods checked exhaustively on `Float32`.
const EXHAUSTIVE: &[&str] = &[
    "exp", "exp2", "exp10", "exp_m1", "ln", "log2", "log10", "ln_1p", "sin", "cos", "tan",
    "sin_pi", "cos_pi", "tan_pi", "atan", "asin", "acos", "sinh", "cosh", "tanh", "asinh", "acosh",
    "atanh", "erf", "erfc", "gamma", "ln_gamma", "cbrt",
];

/// A program that runs every `Float32` pattern (every `stride`-th) through
/// each method and through the `Float64` method rounded once, and prints the
/// disagreements. Where the `Float64` result is exactly half way between two
/// `Float32`s the second rounding is inherently a coin toss, and either
/// neighbour is accepted.
fn exhaustive_program(stride: u64) -> String {
    let mut text = String::from(
        r#"using Standard
using Standard.Io
using Standard.Process

shown: @bit_native

report: function (name: @byte_buffer, input: Uint32, got: Float32, want: Float32) nothing {
    if @i_less_than_u_native(shown, 0x40) {
        shown = @i_add_native(shown, 0x1)
        print_line($"FAIL {name} input {input.hex()} got {got.hex()} want {want.hex()}")
    }
}

"#,
    );
    let mut calls = String::new();
    for name in EXHAUSTIVE {
        let _ = write!(
            text,
            r#"
sweep_{name}: function () bad: @bit_native {{
    bad = 0x0
    i: @bit64 = 0x0
    loop {{
        if @i_greater_than_or_equal_u_64(i, 0x100000000) {{ break }}
        raw: @bit32 = @truncate(i)
        x: Float32 = Float32.from_bits(raw as Uint32)
        got: Float32 = x.{name}()
        wide: Float64 = x as Float64
        exact: Float64 = wide.{name}()
        want: Float32 = exact.to_float32_rounded()
        if got != want and not (got.is_nan() and want.is_nan()) {{
            // accept the other neighbour when `exact` is exactly half way
            other: Float64 = got as Float64
            near: Float64 = want as Float64
            if not ((exact - near).abs() == (other - exact).abs()) {{
                bad = @i_add_native(bad, 0x1)
                report("{name}", raw as Uint32, got, want)
            }}
        }}
        i = @i_add_64(i, 0x{stride:X})
    }}
}}
"#
        );
        let _ = writeln!(calls, "    bad = @i_add_native(bad, sweep_{name}())");
    }
    let _ = write!(
        text,
        r#"
main: function () nothing {{
    bad: @bit_native = 0x0
{calls}    print_line($"mismatches: {{bad}}")
    if bad != 0x0 {{ exit(0x1) }}
}}
"#
    );
    text
}

/// Every `Float32` input of every unary method: `cargo test --release
/// -p lilac-cli --test math -- --ignored` (hours). Set `LILAC_MATH_STRIDE`
/// to sample every n-th pattern instead.
#[test]
#[ignore = "runs 2^32 inputs per function: hours"]
fn float32_agrees_with_the_float64_path_on_every_input() {
    let Some(sysroot) = support::sysroot() else {
        eprintln!("skipping: no lld");
        return;
    };
    let stride: u64 = std::env::var("LILAC_MATH_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let dir = std::env::temp_dir().join(format!("lilac-math-exhaustive-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creates scratch dir");
    std::fs::write(dir.join("sweep.lilac"), exhaustive_program(stride)).expect("writes");
    let standard = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/Standard");
    let exe = dir.join(format!("sweep{}", std::env::consts::EXE_SUFFIX));
    let build = Command::new(env!("CARGO_BIN_EXE_lilac"))
        .args(["build", "-q", "--mode=release"])
        .arg(format!("--lib=Standard={}", standard.display()))
        .arg("-o")
        .arg(&exe)
        .arg("sweep.lilac")
        .current_dir(&dir)
        .env("LILAC_SYSROOT", &sysroot)
        .env("NO_COLOR", "1")
        .output()
        .expect("runs lilac");
    assert!(
        build.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&exe).output().expect("runs the sweep");
    let output = String::from_utf8_lossy(&run.stdout);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(run.status.success(), "disagreements:\n{output}");
}
