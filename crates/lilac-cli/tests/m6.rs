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

/// Returns the braced body following `header` (a `type #X {` or `function @f`).
fn block_after<'a>(ir: &'a str, header: &str) -> &'a str {
    let start = ir
        .find(header)
        .unwrap_or_else(|| panic!("missing `{header}` in:\n{ir}"));
    let rest = &ir[start..];
    let open = rest.find('{').expect("open brace");
    let close = rest[open..].find('}').expect("close brace");
    &rest[open..open + close]
}

/// Two namespaces defining the **same-named generic object with different
/// shapes** must instantiate from their own AST: each namespace's layout and
/// generic function body must match its declaration, never the other's.
/// Regression: `find_item` paired a def with the first same-named AST item in
/// program order, so a later namespace's instance was built from an earlier
/// namespace's members (previously an ICE / silently wrong IR).
#[test]
fn two_namespaces_same_named_generics_keep_their_own_shapes() {
    let ir = emit_ir("two_ns_gen_shapes");

    let a_layout = block_after(&ir, "type #a.GenBox.1..bit32");
    assert!(a_layout.contains("a_value: #bit32"), "{a_layout}");
    assert!(
        !a_layout.contains("b_value"),
        "a reused b's shape:\n{a_layout}"
    );

    let b_layout = block_after(&ir, "type #b.GenBox.1..bit64");
    assert!(b_layout.contains("b_value: #bit64"), "{b_layout}");
    assert!(b_layout.contains("b_extra: #bit64"), "{b_layout}");
    assert!(
        !b_layout.contains("a_value"),
        "b reused a's shape:\n{b_layout}"
    );

    // The generic `tag[T]` bodies differ per namespace (0x1 vs 0x2); each call
    // must fold from its own namespace's specialization.
    let main = block_after(&ir, "function @main");
    assert!(main.contains("const 0x1"), "{main}");
    assert!(
        !main.contains("const 0x2"),
        "main used b's tag body:\n{main}"
    );

    let b_use = block_after(&ir, "function @b_use");
    assert!(b_use.contains("const 0x2"), "{b_use}");
    assert!(
        !b_use.contains("const 0x1"),
        "b_use used a's tag body:\n{b_use}"
    );
}
