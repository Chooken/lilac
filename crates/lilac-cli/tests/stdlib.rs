//! The standard library under `lib/Standard` must check and emit cleanly.
//! Folder mode is required: a single `int64.lilac` file cannot see `Bool`.

use std::path::PathBuf;
use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

/// `lib/Standard` at the repository root, resolved from the crate manifest
/// directory so the test does not depend on the process working directory.
fn standard_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("lib")
        .join("Standard")
}

#[test]
fn standard_library_checks_and_emits() {
    let standard = standard_dir();
    for sub in ["check", "emit-ir"] {
        let out = lilac()
            .arg(sub)
            .arg(&standard)
            .output()
            .expect("runs lilac");
        assert!(
            out.status.success(),
            "`lilac {sub} lib/Standard` failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if sub == "check" {
            assert!(
                out.stderr.is_empty(),
                "`lilac check lib/Standard` wrote to stderr:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
