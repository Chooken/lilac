//! Every numeric operation links on every native target (math plan phase
//! 0): LLVM may turn an operation into a call to a C runtime routine
//! (`fmod`, `__extendhfsf2`, `__divdi3`, …), which a native program does
//! not link, so the backend must define each one it can reach.
//!
//! The program has one `export` per intrinsic and width (an export is
//! kept and its arguments are unknown, so nothing folds away), and it is
//! built for each native target at `debug` and `release`, cross-linked
//! with the development sysroot's `lld`: an undefined symbol fails the
//! link. Each later phase adds its intrinsics to [`program`].

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

const FLOAT_BINARY: [&str; 5] = ["add", "sub", "mul", "div", "mod"];

const FLOAT_COMPARE: [&str; 4] = [
    "less_than",
    "greater_than",
    "less_than_or_equal",
    "greater_than_or_equal",
];

/// The probe program: a `main` and one export per operation and width.
fn program() -> String {
    let mut text = String::from("main: function () nothing {\n}\n");
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
        for name in ["i_negate", "bit_not"] {
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
        writeln!(
            text,
            "probe_f_negate_{width}: export function (a: {ty}) r: {ty} {{\n    r = @fbit_to_bits_{width}(@f_negate_{width}({}))\n}}\n",
            float("a")
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
    let mut failures = Vec::new();
    for triple in TARGETS {
        for mode in ["debug", "release"] {
            let out = Command::new(env!("CARGO_BIN_EXE_lilac"))
                .args(["build", "-q"])
                .arg(format!("--lib=Standard={}", standard.display()))
                .arg(format!("--target={triple}"))
                .arg(format!("--mode={mode}"))
                .arg("-o")
                .arg(dir.join(format!("{triple}-{mode}")))
                .arg("numeric.lilac")
                .current_dir(&dir)
                .env("LILAC_SYSROOT", &sysroot)
                .env("NO_COLOR", "1")
                .output()
                .expect("runs lilac");
            if !out.status.success() {
                failures.push(format!(
                    "{triple} ({mode}):\n{}",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
