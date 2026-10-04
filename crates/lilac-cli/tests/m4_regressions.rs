//! Task 3 regression: unused generic object members must not reach IR.
//!
//! A non-generic method or in-body hook of a generic object carries `Param`
//! slots in its template signature. Before the fix these were checked and
//! emitted as concrete `HirFunc`s, lowering to `#T` and tripping the verifier
//! with a false `E070` compiler bug on otherwise-valid, unused generics.

use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

fn ui(name: &str) -> String {
    format!("../../tests/ui/{name}")
}

#[test]
fn unused_generic_object_members_do_not_lower() {
    let output = lilac()
        .args(["check", &ui("generic_unused.lilac")])
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr:\n{stderr}");
    assert!(!stderr.contains("E070"), "stderr:\n{stderr}");
    assert!(stderr.is_empty(), "stderr:\n{stderr}");
}
