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
