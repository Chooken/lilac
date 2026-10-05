//! M2 UI tests: control-flow ports check clean, `.lir` snapshots
//! byte-stable, FAILS pins hold their codes, fold/W020 fixtures silent.

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
    "enum_decl.lilac",
    "enum_implicit_member.lilac",
    "enum_use.lilac",
    "function_multi_returns.lilac",
    "function_multi_return_conversion.lilac",
    "function_types.lilac",
    "if_and.lilac",
    "if_condition.lilac",
    "if_else.lilac",
    "if_or.lilac",
    "inline_functions.lilac",
    "inline_function_w_variable.lilac",
    "inline_variables.lilac",
    "main_function.lilac",
    "references_2.lilac",
    "enum_payload_decl.lilac",
    "match_enum.lilac",
    "match_enum_payload.lilac",
    "object_instance_function.lilac",
    "enum_payload_use.lilac",
];

const M1_PORTS: &[&str] = &[
    "function_returns.lilac",
    "int_addition.lilac",
    "references.lilac",
    "conversion.lilac",
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
    for port in PORTS.iter().chain(M1_PORTS.iter()) {
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
fn function_multi_returns_is_e022() {
    fails_with("FAILS_function_multi_returns.lilac", "E022");
}

#[test]
fn function_multi_returns_2_is_e027() {
    fails_with("FAILS_function_multi_returns_2.lilac", "E027");
}

#[test]
fn function_returns_is_e043() {
    fails_with("FAILS_function_returns.lilac", "E043");
}

#[test]
fn function_returns_2_is_e043() {
    fails_with("FAILS_function_returns_2.lilac", "E043");
}

#[test]
fn if_as_value_is_e001() {
    fails_with("FAILS_if_as_value.lilac", "E001");
}

#[test]
fn match_returns_value_is_e001() {
    fails_with("FAILS_match_returns_value.lilac", "E001");
}

#[test]
fn compiler_error_is_e031() {
    fails_with("FAILS_compiler_error.lilac", "E031");
}

#[test]
fn object_member_is_e021() {
    fails_with("FAILS_object_member.lilac", "E021");
}

#[test]
fn object_setter_is_e022() {
    fails_with("FAILS_object_setter.lilac", "E022");
}

#[test]
fn object_setter_alt_is_e001() {
    let output = lilac()
        .args(["check", &ui("object_setter_alt.lilac")])
        .output()
        .expect("runs lilac");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E001]"), "stderr:\n{stderr}");
    assert!(
        stderr.contains("use `= Type { ... }` to construct a struct value"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn op_warn_is_warning_only() {
    let output = lilac()
        .args(["check", &ui("op_type_change_warn.lilac")])
        .output()
        .expect("runs lilac");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("warning[W020]"), "stderr:\n{stderr}");
    assert!(!stderr.contains("error["), "stderr:\n{stderr}");
}

#[test]
fn op_mixed_silent() {
    let output = lilac()
        .args(["check", &ui("op_bit_mixed_ok.lilac")])
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr:\n{stderr}");
    assert!(stderr.is_empty(), "stderr:\n{stderr}");
}

#[test]
fn cfold_dead_silent() {
    let output = lilac()
        .args(["check", &ui("cfold_dead_compile_error.lilac")])
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr:\n{stderr}");
    assert!(stderr.is_empty(), "stderr:\n{stderr}");
}

/// A `@compile_error` inside a hook that no code path invokes is silent: the
/// hook is unreachable and DCE drops it before the live-error check.
#[test]
fn unused_hook_compile_error_silent() {
    let output = lilac()
        .args(["check", &ui("hook_unused_compile_error.lilac")])
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr:\n{stderr}");
    assert!(stderr.is_empty(), "stderr:\n{stderr}");
}

/// The same hook, invoked by a live `@on_drop`, reports `E031`.
#[test]
fn used_hook_compile_error_is_e031() {
    let output = lilac()
        .args(["check", &ui("FAILS_hook_used_compile_error.lilac")])
        .output()
        .expect("runs lilac");
    assert!(!output.status.success(), "should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E031]"), "stderr:\n{stderr}");
}
