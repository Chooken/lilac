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

#[test]
fn fails_fixtures_do_not_panic() {
    // Regression guard for the M4 crash classes (ICE on a bare generic-object
    // static call; stack overflow on a growing object layout): every
    // `FAILS_*` fixture must fail with the ordinary exit code, never a panic
    // (`101`) or a signal (stack overflow).
    let mut fixtures: Vec<std::path::PathBuf> = std::fs::read_dir("../../tests/ui")
        .expect("reads tests/ui")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "lilac")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("FAILS_"))
        })
        .collect();
    fixtures.sort();
    assert!(!fixtures.is_empty(), "no FAILS_* fixtures found");
    for fixture in fixtures {
        let output = lilac()
            .args(["check"])
            .arg(&fixture)
            .output()
            .expect("runs lilac");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{} should fail cleanly, not panic/signal: status={:?}\n{}",
            fixture.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
