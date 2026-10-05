//! M5c UI tests: `emit-vm` compiles to the VM target and emits canonical `.lbc`
//! text; the snapshot is byte-stable under `--check`, and a non-VM target is a
//! usage error (exit 2).

use std::io::Write as _;
use std::process::{Command, ExitStatus, Stdio};

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

/// Captures one `lilac` invocation the way the M2/M4 tests do, decoding stdout
/// as UTF-8 so it can be compared byte-for-byte against a `.lbc` snapshot.
struct Run {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

fn run<const N: usize>(args: [&str; N]) -> Run {
    let output = lilac().args(args).output().expect("runs lilac");
    Run {
        status: output.status,
        stdout: String::from_utf8(output.stdout).expect("utf8 stdout"),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Runs `lilac` with `input` on stdin, capturing stdout/stderr.
fn run_with_stdin<const N: usize>(args: [&str; N], input: &str) -> Run {
    let mut child = lilac()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawns lilac");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("writes stdin");
    let output = child.wait_with_output().expect("waits for lilac");
    Run {
        status: output.status,
        stdout: String::from_utf8(output.stdout).expect("utf8 stdout"),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

const SOURCE: &str = "../../tests/ui/vm_hello.lilac";
const SNAPSHOT: &str = "../../tests/ui/vm_hello.lbc";
const EXTERN_SOURCE: &str = "../../tests/ui/vm_extern.lilac";

#[test]
fn emit_vm_matches_snapshot() {
    let out = run(["emit-vm", SOURCE]);
    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", out.stderr);
    let expected = std::fs::read_to_string(SNAPSHOT).expect("snapshot");
    assert_eq!(out.stdout, expected);
}

#[test]
fn emit_vm_check_passes_on_committed_snapshot() {
    let out = run(["emit-vm", "--check", SOURCE]);
    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", out.stderr);
}

#[test]
fn emit_vm_check_detects_drift() {
    let dir = std::env::temp_dir().join(format!("lilac-m5c-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("creates scratch dir");
    let source = dir.join("vm_hello.lilac");
    std::fs::write(
        &source,
        "main: function () nothing {\n    x: @bit_native = @i_add(0x1, 0x2)\n}\n",
    )
    .expect("writes source");
    std::fs::write(dir.join("vm_hello.lbc"), "lbc = \"0.9\"\n").expect("writes stale snapshot");

    let source = source.to_str().expect("utf8 path");
    let out = run(["emit-vm", "--check", source]);
    assert_eq!(out.status.code(), Some(1), "stderr:\n{}", out.stderr);
    assert!(
        out.stderr.contains("match emit-vm output"),
        "stderr:\n{}",
        out.stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn emit_vm_rejects_non_vm_target() {
    let out = run(["emit-vm", SOURCE, "--target=x86_64-unknown-none"]);
    assert_eq!(out.status.code(), Some(2), "stderr:\n{}", out.stderr);
    assert!(
        out.stderr.contains("emit-vm requires a VM target"),
        "stderr:\n{}",
        out.stderr
    );
}

#[test]
fn run_executes_and_exits_zero() {
    let out = run(["run", SOURCE]);
    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", out.stderr);
}

/// `--step` drops into the debugger; `stepi` and `info regs` drive the REPL.
#[test]
fn run_step_enters_debugger() {
    let out = run_with_stdin(["run", "--step", SOURCE], "stepi\ninfo regs\nquit\n");
    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", out.stderr);
    assert!(out.stdout.contains("lilac-vm"), "stdout:\n{}", out.stdout);
    assert!(out.stdout.contains("regs"), "stdout:\n{}", out.stdout);
}

#[test]
fn run_reports_extern_error() {
    let out = run(["run", EXTERN_SOURCE]);
    assert_eq!(out.status.code(), Some(1), "stderr:\n{}", out.stderr);
    assert!(out.stderr.contains("extern"), "stderr:\n{}", out.stderr);
}

#[test]
fn emit_llvm_rejects_vm_target() {
    let out = run(["emit-llvm", SOURCE, "--target=lilacvm-unknown-none"]);
    assert_eq!(out.status.code(), Some(2), "stderr:\n{}", out.stderr);
}

/// `lilac disasm` prints the committed artifact's canonical instruction text.
#[test]
fn disasm_prints_canonical_instructions() {
    let out = run(["disasm", SNAPSHOT]);
    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", out.stderr);
    assert!(
        out.stdout.contains("&0: #bit_native = const 0x1"),
        "stdout:\n{}",
        out.stdout
    );
    assert!(out.stdout.contains("return"), "stdout:\n{}", out.stdout);
}

/// A malformed or unreadable `.lbc` is a clean data error (exit 2).
#[test]
fn disasm_bad_file_exits_two() {
    let bad = std::env::temp_dir().join(format!("lilac-m5c-bad-{}.lbc", std::process::id()));
    std::fs::write(&bad, "not an artifact\n").expect("writes bad file");
    let out = run(["disasm", bad.to_str().expect("utf8 path")]);
    assert_eq!(out.status.code(), Some(2), "stderr:\n{}", out.stderr);
    let _ = std::fs::remove_file(&bad);

    let missing =
        std::env::temp_dir().join(format!("lilac-m5c-missing-{}.lbc", std::process::id()));
    let out = run(["disasm", missing.to_str().expect("utf8 path")]);
    assert_eq!(out.status.code(), Some(2), "stderr:\n{}", out.stderr);
}

/// Writes a bodyless-`main` library to a per-test temporary file and returns
/// its path. The stem keeps parallel tests from colliding.
fn write_library(stem: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("lilac-m5c-{}-{stem}.lilac", std::process::id()));
    std::fs::write(&path, "api: function () nothing {\n}\n").expect("writes library");
    path
}

/// A single `--export` names the entry point, so a library with no `main` runs.
#[test]
fn run_with_single_export_uses_it_as_entry() {
    let path = write_library("export");
    let source = path.to_str().expect("utf8 path");
    let out = run(["run", "--export=api", source]);
    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", out.stderr);
    let _ = std::fs::remove_file(&path);
}

/// With no export, `run` falls back to `main`; a library without one is a clean
/// no-entry error (exit 1).
#[test]
fn run_without_export_needs_main() {
    let path = write_library("noexport");
    let source = path.to_str().expect("utf8 path");
    let out = run(["run", source]);
    assert_eq!(out.status.code(), Some(1), "stderr:\n{}", out.stderr);
    assert!(
        out.stderr.contains("no entry point"),
        "stderr:\n{}",
        out.stderr
    );
    let _ = std::fs::remove_file(&path);
}
