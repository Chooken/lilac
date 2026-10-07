//! The standard library under `lib/Standard` must check and emit cleanly.
//! Folder mode is required: a single `int64.lilac` file cannot see `Bool`.

use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

#[test]
fn standard_library_checks_and_emits() {
    for sub in ["check", "emit-ir"] {
        let out = lilac()
            .args([sub, "../../lib/Standard"])
            .output()
            .expect("runs lilac");
        assert!(
            out.status.success(),
            "`lilac {sub} lib/Standard` failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
