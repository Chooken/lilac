//! M0 UI tests: `lilac check` over `tests/ui/*.lilac` snapshots.

use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

fn ui(name: &str) -> String {
    format!("../../tests/ui/{name}")
}

#[test]
fn check_hello_succeeds() {
    let output = lilac()
        .args(["check", &ui("hello.lilac")])
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn check_broken_parse_fails_with_e001() {
    let output = lilac()
        .args(["check", &ui("fails_parse.lilac")])
        .output()
        .expect("runs lilac");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E001]"), "stderr was:\n{stderr}");
    assert!(
        stderr.contains("fails_parse.lilac:3"),
        "stderr was:\n{stderr}"
    );
}

#[test]
fn check_broken_lex_fails_with_e000() {
    let output = lilac()
        .args(["check", &ui("fails_lex.lilac")])
        .output()
        .expect("runs lilac");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E000]"), "stderr was:\n{stderr}");
}

#[test]
fn check_json_format_carries_code() {
    let output = lilac()
        .args(["check", "--error-format=json", &ui("fails_parse.lilac")])
        .output()
        .expect("runs lilac");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("\"code\":\"E001\""),
        "stderr was:\n{stderr}"
    );
}

#[test]
fn check_object_fields_succeeds() {
    let output = lilac()
        .args(["check", &ui("object_fields.lilac")])
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn emit_ir_prints_version_header() {
    let output = lilac()
        .args(["emit-ir", &ui("hello.lilac")])
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("lilac_ir = \"0.2\""),
        "stdout was:\n{stdout}"
    );
    assert!(stdout.contains("target = "), "stdout was:\n{stdout}");
}
