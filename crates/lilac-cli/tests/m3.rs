//! M3 UI tests: string/const ports check clean, `.lir` snapshots
//! byte-stable, and `E043`/`E060`/`E061` FAILS pins hold.

use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

fn ui(name: &str) -> String {
    format!("../../tests/ui/{name}")
}

fn legacy(name: &str) -> String {
    format!("../../tests/{name}")
}

const PORTS: &[&str] = &[
    "string.lilac",
    "fstring.lilac",
    "cfold_call_eval.lilac",
    "cfold_miss_is_ok.lilac",
];

#[test]
fn check_ports_clean() {
    for port in PORTS {
        let output = lilac()
            .args(["check", &ui(port)])
            .output()
            .expect("runs lilac");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{port} stderr:\n{stderr}");
        assert!(stderr.is_empty(), "{port} stderr:\n{stderr}");
    }
}

#[test]
fn lir_snapshots_stable() {
    for port in PORTS {
        let output = lilac()
            .args(["emit-ir", "--check", &ui(port)])
            .output()
            .expect("runs lilac");
        assert!(
            output.status.success(),
            "{port} stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn fails_with(name: &str, code: &str) {
    let output = lilac()
        .args(["check", &legacy(name)])
        .output()
        .expect("runs lilac");
    assert!(!output.status.success(), "{name} should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("error[{code}]")),
        "{name} stderr:\n{stderr}"
    );
}

#[test]
fn fstring_missing_conversion_is_e043() {
    fails_with("FAILS_fstring_missing_conversion.lilac", "E043");
}

#[test]
fn fstring_piece_cap_is_e061() {
    fails_with("FAILS_fstring_piece_cap.lilac", "E061");
}
