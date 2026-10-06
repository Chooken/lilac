//! M6a UI tests: the CLI compiles a whole folder project and reports `E025`
//! for a `using` whose target folder is missing.

use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

fn modules(name: &str) -> String {
    format!("../../tests/ui/modules/{name}")
}

#[test]
fn check_two_file_folder_succeeds() {
    let output = lilac()
        .arg("check")
        .arg(modules("two_files"))
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn check_missing_using_folder_is_e025() {
    let output = lilac()
        .arg("check")
        .arg(modules("FAILS_missing_using"))
        .output()
        .expect("runs lilac");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E025]"), "stderr:\n{stderr}");
}

/// Runs `emit-ir` over a project directory and returns its IR text.
fn emit_ir(name: &str) -> String {
    let output = lilac()
        .arg("emit-ir")
        .arg(modules(name))
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "{name} stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8 IR")
}

/// Two namespaces may declare same-named non-generic types: their layouts and
/// `@on_drop` hooks must stay distinct (Task 7 addendum).
#[test]
fn two_namespaces_same_named_types_do_not_collide() {
    let ir = emit_ir("two_ns_types");
    for needle in [
        "#a.Thing",
        "#b.Thing",
        "@a.Thing.@on_drop",
        "@b.Thing.@on_drop",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in:\n{ir}");
    }
}

/// Same-named generic objects in two namespaces must mangle to distinct
/// instances, and their instance methods must emit matching symbols without a
/// doubled namespace prefix (Task 7 addendum).
#[test]
fn two_namespaces_same_named_generics_do_not_collide() {
    let ir = emit_ir("two_ns_generics");
    for needle in [
        "#a.GenBox.1..bit32",
        "#b.GenBox.1..bit64",
        "@a.GenBox.1..bit32.get",
        "@b.GenBox.1..bit64.get",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in:\n{ir}");
    }
}
