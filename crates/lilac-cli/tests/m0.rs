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
fn check_broken_item_recovers_without_extra_errors() {
    // Item-level recovery: the file has one good item plus one broken item.
    // Strict mode still stops before `collect`, so `check` fails with exactly
    // the same single E001 (same message and span) as before recovery, and the
    // good item is not reported as an extra error.
    let output = lilac()
        .args(["check", &ui("recovers_parse.lilac")])
        .output()
        .expect("runs lilac");
    assert!(
        !output.status.success(),
        "the broken item must fail `check`"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error[E001]: unterminated parameter list; add a closing `)`"),
        "stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains("recovers_parse.lilac:4:1"),
        "stderr was:\n{stderr}"
    );
    assert_eq!(
        stderr.matches("error[").count(),
        1,
        "only the broken item is reported:\n{stderr}"
    );
    assert_eq!(
        stderr.matches("-->").count(),
        1,
        "only one diagnostic span is reported:\n{stderr}"
    );
}

#[test]
fn check_malformed_call_keeps_following_item() {
    // Critical: a malformed call argument must not consume the enclosing `}`
    // as its closing paren (which would lose the following item and cascade a
    // second E001). Strict mode sees exactly one diagnostic.
    let output = lilac()
        .args(["check", &ui("FAILS_call_recovery.lilac")])
        .output()
        .expect("runs lilac");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("error[").count(),
        1,
        "exactly one diagnostic:\n{stderr}"
    );
    assert_eq!(
        stderr.matches("-->").count(),
        1,
        "exactly one diagnostic span:\n{stderr}"
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
        stdout.contains("lilac_ir = \"0.3\""),
        "stdout was:\n{stdout}"
    );
    assert!(stdout.contains("target = "), "stdout was:\n{stdout}");
}
