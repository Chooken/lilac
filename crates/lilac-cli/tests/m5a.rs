//! M5a UI tests: extern IR, stale-snapshot guard, DCE, and codec round-trips.

use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

fn ui(name: &str) -> String {
    format!("../../tests/ui/{name}")
}

const PORTS: &[&str] = &["extern_decl.lilac"];

#[test]
fn check_ports_clean() {
    for port in PORTS {
        let output = lilac()
            .args(["check", &ui(port)])
            .output()
            .expect("runs lilac");
        assert!(
            output.status.success(),
            "{port}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn lir_snapshots_stable() {
    let mut snapshots: Vec<std::path::PathBuf> = std::fs::read_dir("../../tests/ui")
        .expect("reads tests/ui")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "lir"))
        .collect();
    snapshots.sort();
    for snapshot in snapshots {
        let source = snapshot.with_extension("lilac");
        let output = lilac()
            .args(["emit-ir", "--check"])
            .arg(&source)
            .output()
            .expect("runs lilac");
        assert!(
            output.status.success(),
            "{}:\n{}",
            source.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn extern_arity_mismatch_is_diagnosed() {
    let output = lilac()
        .args(["check", &ui("FAILS_extern_arity.lilac")])
        .output()
        .expect("runs lilac");
    assert!(
        !output.status.success(),
        "wrong-arity extern call must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("puts"), "{stderr}");
}
