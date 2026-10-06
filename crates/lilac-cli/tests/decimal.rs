//! End-to-end coverage for exact decimal-literal folding (SPEC §2): a `+ - * / %`
//! over two decimal literals folds at parse time, honors `--decimal-scale` /
//! `--decimal-round`, and division/modulo by zero is `E060`. The folded result
//! lowers to a `numlit` data const whose payload appears verbatim in `emit-ir`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

/// A temporary `.lilac` source file that removes itself when dropped, so every
/// test cleans up its own file (matching the M5c UI tests' hygiene).
struct TempSrc(PathBuf);

impl TempSrc {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempSrc {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Writes `src` to a unique temp `.lilac` file so these tests can run in
/// parallel without sharing a path; the file is deleted when the guard drops.
fn write_src(name: &str, src: &str) -> TempSrc {
    let mut p = std::env::temp_dir();
    p.push(format!("lilac_decimal_{}_{name}.lilac", std::process::id()));
    std::fs::write(&p, src).expect("write temp source");
    TempSrc(p)
}

/// Runs `emit-ir <extra> <src>` and returns the emitted LIR text, asserting the
/// compile succeeded.
fn emit_ir(name: &str, src: &str, extra: &[&str]) -> String {
    let path = write_src(name, src);
    let output = lilac()
        .arg("emit-ir")
        .args(extra)
        .arg(path.path())
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "emit-ir failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8 LIR")
}

/// `1 / 3` folds at the default scale of 10, truncating to `0.3333333333`.
#[test]
fn default_scale_is_ten() {
    let ir = emit_ir(
        "default_scale",
        "main: function () nothing {\n    x: @byte_buffer = 1 / 3\n}\n",
        &[],
    );
    assert!(ir.contains("0.3333333333"), "LIR:\n{ir}");
}

/// `--decimal-scale=3` overrides the default, truncating `1 / 3` to `0.333`.
#[test]
fn scale_flag_changes_output() {
    let ir = emit_ir(
        "scale_flag",
        "main: function () nothing {\n    x: @byte_buffer = 1 / 3\n}\n",
        &["--decimal-scale=3"],
    );
    assert!(ir.contains("0.333"), "LIR:\n{ir}");
    assert!(
        !ir.contains("0.3333"),
        "scale=3 must not emit more than 3 fractional digits:\n{ir}"
    );
}

/// `--decimal-round=half-up` rounds `1 / 8` up at scale 2: `0.13`.
#[test]
fn half_up_flag_rounds() {
    let ir = emit_ir(
        "half_up",
        "main: function () nothing {\n    x: @byte_buffer = 1 / 8\n}\n",
        &["--decimal-scale=2", "--decimal-round=half-up"],
    );
    assert!(ir.contains("0.13"), "LIR:\n{ir}");
}

/// A decimal division by zero is a hard `E060` (exit non-zero), reported by
/// `check` on stderr.
#[test]
fn division_by_zero_fails_with_e060() {
    let path = write_src(
        "div_zero",
        "main: function () nothing {\n    x: @byte_buffer = 1 / 0\n}\n",
    );
    let output = lilac()
        .arg("check")
        .arg(path.path())
        .output()
        .expect("runs lilac");
    assert_ne!(
        output.status.code(),
        Some(0),
        "expected `1 / 0` to fail; stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E060]"), "stderr:\n{stderr}");
    assert!(stderr.contains("division by zero"), "stderr:\n{stderr}");
}

/// A decimal modulo by zero is a hard `E060` too, and it must survive the full
/// `check` path (not just the `eval` unit test).
#[test]
fn modulo_by_zero_fails_with_e060() {
    let path = write_src(
        "mod_zero",
        "main: function () nothing {\n    x: @byte_buffer = 1 % 0\n}\n",
    );
    let output = lilac()
        .arg("check")
        .arg(path.path())
        .output()
        .expect("runs lilac");
    assert_ne!(
        output.status.code(),
        Some(0),
        "expected `1 % 0` to fail; stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E060]"), "stderr:\n{stderr}");
    assert!(stderr.contains("modulo by zero"), "stderr:\n{stderr}");
}
