//! M1 UI tests: ports check clean, `.lir` snapshots byte-stable, E052/E020.

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
    "function_returns.lilac",
    "int_addition.lilac",
    "references.lilac",
    "conversion.lilac",
];

#[test]
fn check_4_ports_clean() {
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
fn lir_4_snapshots_stable() {
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

#[test]
fn hook_generic_is_e052() {
    let output = lilac()
        .args(["check", &ui("FAILS_hook_generic.lilac")])
        .output()
        .expect("runs lilac");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("error[").count(), 1, "stderr:\n{stderr}");
    assert!(stderr.contains("error[E052]"), "stderr:\n{stderr}");
}

#[test]
fn duplicate_conversion_is_e020() {
    let output = lilac()
        .args(["check", &legacy("FAILS_conversion.lilac")])
        .output()
        .expect("runs lilac");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E020]"), "stderr:\n{stderr}");
}
