//! Integration tests for the standalone `lilac-vm` runner binary.
//!
//! The valid artifact is produced through the real pipeline
//! (`lilac-driver` → `lilac_vm::compile` → `.lbc` text) rather than
//! hand-written, so these tests exercise the runner against exactly what
//! `lilac emit-vm` would write.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Compiles a tiny `.lilac` program to canonical `.lbc` text for the VM target.
fn valid_lbc() -> String {
    let mut session = lilac_driver::Session::new();
    session.target = lilac_driver::Target::parse(lilac_vm::VM_TARGET).expect("VM target");
    let module = session
        .compile_lir("t.lilac", "main: function () nothing {\n}\n")
        .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
    lilac_vm::compile(&module, &session.source_map)
        .expect("compile")
        .to_text()
}

/// Writes `body` to a uniquely named `.lbc` file under the system temp dir.
fn write_temp(name: &str, body: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("lilac-vm-runner-{}-{name}.lbc", std::process::id()));
    std::fs::write(&path, body).expect("write temp file");
    path
}

/// Runs the built `lilac-vm` binary on `path` with `extra` arguments.
fn run_bin(path: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lilac-vm"))
        .arg(path)
        .args(extra)
        .output()
        .expect("spawn lilac-vm")
}

#[test]
fn runs_lbc_file() {
    let path = write_temp("runs", &valid_lbc());
    let out = run_bin(&path, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn rejects_wrong_version() {
    let path = write_temp(
        "version",
        "lbc = \"9.9\"\ntarget = \"lilacvm-unknown-none\"\n",
    );
    let out = run_bin(&path, &[]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn rejects_non_vm_target() {
    let path = write_temp(
        "target",
        "lbc = \"0.1\"\ntarget = \"x86_64-unknown-none\"\n",
    );
    let out = run_bin(&path, &[]);
    assert_eq!(out.status.code(), Some(2));
}

/// A body whose last instruction is not a terminator fails verification (exit
/// 2), never an interpreter `bug!`/panic (exit 101).
#[test]
fn rejects_non_terminated_body() {
    let body = "lbc = \"0.1\"\ntarget = \"lilacvm-unknown-none\"\n\n\
                type !t0 = \"#bit_native\" size 8 ref 0\n\n\
                func @main () -> () regs 1 slots 0\n\
                \x20   0  &0: #bit_native = const 0x1\n";
    let path = write_temp("nonterm", body);
    let out = run_bin(&path, &[]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(out.status.code(), Some(101));
}

/// An `unreachable` terminator is a clean runtime error (exit 1), not a panic.
#[test]
fn unreachable_body_exits_one() {
    let body = "lbc = \"0.1\"\ntarget = \"lilacvm-unknown-none\"\n\n\
                func @main () -> () regs 1 slots 0\n\
                \x20   0  unreachable\n";
    let path = write_temp("unreachable", body);
    let out = run_bin(&path, &[]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(out.status.code(), Some(101));
}

/// A non-extern function with no code at all fails verification (exit 2).
#[test]
fn rejects_zero_length_body() {
    let body = "lbc = \"0.1\"\ntarget = \"lilacvm-unknown-none\"\n\n\
                func @main () -> () regs 1 slots 0\n";
    let path = write_temp("empty", body);
    let out = run_bin(&path, &[]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(out.status.code(), Some(101));
}

/// Debug flags enter the REPL; with no input it resumes to completion, so a
/// non-interactive invocation still exits with the program's code.
#[test]
fn debug_flags_enter_repl_and_run_to_completion() {
    let path = write_temp("debug", &valid_lbc());
    let out = run_bin(&path, &["--step", "--trace", "--break=main"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("lilac-vm"), "stdout: {stdout}");
}
