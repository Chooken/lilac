//! M2 UI tests: control-flow ports check clean, `.lir` snapshots
//! byte-stable, FAILS pins hold their codes, dead `@compile_error`s silent.

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

/// Legacy `tests/*.lilac` programs that still check cleanly.
const LEGACY_CLEAN: &[&str] = &[
    "match.lilac",
    "match_else.lilac",
    "match_enum_2.lilac",
    "match_return_value.lilac",
    "object_declaration.lilac",
    "object_member.lilac",
    "object_setter.lilac",
    "op_add.lilac",
    "op_div.lilac",
    "op_mod.lilac",
    "op_mul.lilac",
    "op_sub.lilac",
    "raii_init_decl.lilac",
    "raii_init_use.lilac",
    "raii_on_copy_decl.lilac",
    "raii_on_copy_use.lilac",
    "raii_on_drop_decl.lilac",
    "raii_on_drop_if.lilac",
    "raii_on_drop_use.lilac",
    "raii_on_override_decl.lilac",
    "raii_on_override_use.lilac",
];

#[test]
fn legacy_programs_check_clean() {
    for name in LEGACY_CLEAN {
        let output = lilac()
            .args(["check", &legacy(name)])
            .output()
            .expect("runs lilac");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{name} stderr:
{stderr}"
        );
    }
}

#[test]
fn unknown_type_is_e023() {
    fails_with("FAILS_unknown_type.lilac", "E023");
}

#[test]
fn unknown_variable_is_e021() {
    fails_with("FAILS_unknown_variable.lilac", "E021");
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
fn object_setter_shorthand_checks() {
    // `variable: Object { ... }` is shorthand for `= Object { ... }`.
    let output = lilac()
        .args(["check", &ui("object_setter_alt.lilac")])
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr:\n{stderr}");
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

/// The `Standard` int64 shape — operators plus three distinct `@conversion`
/// pairs, all as object-member hooks — checks cleanly through the unit-global
/// hook index (no operand conversion, no `W020`).
#[test]
fn hook_index_int64_checks_clean() {
    let output = lilac()
        .args(["check", &ui("hook_index_int64.lilac")])
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr:\n{stderr}");
    assert!(stderr.is_empty(), "stderr:\n{stderr}");
}

/// The same hook, invoked by a live `@on_drop`, reports `E031`; the chain
/// hides the `.lilac.drop.*` glue between the hook and `main`.
#[test]
fn used_hook_compile_error_is_e031() {
    let output = lilac()
        .args(["check", &ui("FAILS_hook_used_compile_error.lilac")])
        .output()
        .expect("runs lilac");
    assert!(!output.status.success(), "should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E031]"), "stderr:\n{stderr}");
    assert!(stderr.contains("at Object.@on_drop ("), "stderr:\n{stderr}");
    // The drop runs at `main`'s closing `}`.
    assert!(stderr.contains("at main ("), "stderr:\n{stderr}");
    assert!(
        stderr.contains("FAILS_hook_used_compile_error.lilac:12:1)"),
        "stderr:\n{stderr}"
    );
    assert!(!stderr.contains(".lilac."), "stderr:\n{stderr}");
}

/// A `@compile_error` in a generic body names the live instance and the
/// shortest call chain to it.
#[test]
fn generic_compile_error_chain_names_the_instance() {
    let output = lilac()
        .args(["check", &ui("FAILS_compile_error_generic_chain.lilac")])
        .output()
        .expect("runs lilac");
    assert!(!output.status.success(), "should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let frames = [
        "at Holder[@bit32].make (",
        "FAILS_compile_error_generic_chain.lilac:5:9)",
        "at helper (",
        "FAILS_compile_error_generic_chain.lilac:10:25)",
        "at main (",
        "FAILS_compile_error_generic_chain.lilac:14:5)",
    ];
    let mut rest = stderr.as_ref();
    for frame in frames {
        let at = rest
            .find(frame)
            .unwrap_or_else(|| panic!("missing `{frame}` in order:\n{stderr}"));
        rest = &rest[at + frame.len()..];
    }
}

/// By default an unreachable `api` is swept from `export_entry.lilac`.
#[test]
fn unexported_function_is_pruned() {
    let output = lilac()
        .args(["emit-ir", &ui("export_entry.lilac")])
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).expect("utf8 IR");
    assert!(!text.contains("function @api"), "{text}");
}

/// `--export=api` roots `api`, so it survives reachability pruning.
#[test]
fn export_flag_roots_named_function() {
    let output = lilac()
        .args(["emit-ir", "--export=api", &ui("export_entry.lilac")])
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).expect("utf8 IR");
    assert!(text.contains("function @api"), "{text}");
}

/// `--export=` with no name is a usage error (exit 2).
#[test]
fn empty_export_is_rejected() {
    let output = lilac()
        .args(["check", "--export=", &ui("export_entry.lilac")])
        .output()
        .expect("runs lilac");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--export= needs a name"),
        "stderr:\n{stderr}"
    );
}
