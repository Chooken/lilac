//! Run tests: every `tests/run/*.lilac` is compiled with `emit-llvm`, linked
//! with the toolchain `clang`, executed, and compared against its header.
//!
//! Header comments (any order, before or between items):
//!
//! - `// expect-stdout: <line>` — one expected stdout line (repeatable; lines
//!   are joined with `\n`, and the program's `\r\n` is normalized to `\n`).
//! - `// expect-exit: <n>` — the expected exit code (default `0`).
//! - `// expect-error: <code>` — the program must fail `lilac check` with this
//!   diagnostic code; nothing is linked or run.
//! - `// pending: <reason>` — a known bug: skipped by `run_programs` and
//!   exercised by the ignored `run_pending_programs` instead. Remove the line
//!   when the fix lands.
//! - `// lib: <Name>=<dir>` — mounts a library (`--lib`); `<dir>` is relative
//!   to the repository root (repeatable).
//!
//! A program is a single `*.lilac` file or a folder project, whose header is
//! read from its `main.lilac`.
//!
//! Programs print with `extern putchar` and set the exit code with
//! `extern exit`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

fn run_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/run")
}

/// Locates the toolchain `clang`: `%LLVM_SYS_181_PREFIX%\bin\clang[.exe]`, else
/// `clang` on `PATH`. `None` when neither runs.
fn clang() -> Option<PathBuf> {
    if let Ok(prefix) = std::env::var("LLVM_SYS_181_PREFIX") {
        let name = if cfg!(windows) { "clang.exe" } else { "clang" };
        let candidate = Path::new(&prefix).join("bin").join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    let path = PathBuf::from("clang");
    Command::new(&path)
        .arg("--version")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|_| path)
}

#[derive(Default)]
struct Expect {
    stdout: Vec<String>,
    exit: i32,
    error: Option<String>,
    pending: Option<String>,
    libs: Vec<String>,
}

fn parse_header(text: &str) -> Expect {
    let mut expect = Expect::default();
    for line in text.lines() {
        let Some(comment) = line.trim().strip_prefix("//") else {
            continue;
        };
        let comment = comment.trim_start();
        if let Some(rest) = comment.strip_prefix("expect-stdout:") {
            expect
                .stdout
                .push(rest.strip_prefix(' ').unwrap_or(rest).to_owned());
        } else if let Some(rest) = comment.strip_prefix("expect-exit:") {
            expect.exit = rest.trim().parse().expect("expect-exit is an integer");
        } else if let Some(rest) = comment.strip_prefix("expect-error:") {
            expect.error = Some(rest.trim().to_owned());
        } else if let Some(rest) = comment.strip_prefix("pending:") {
            expect.pending = Some(rest.trim().to_owned());
        } else if let Some(rest) = comment.strip_prefix("lib:") {
            let (name, dir) = rest.trim().split_once('=').expect("lib: is Name=dir");
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(dir);
            expect.libs.push(format!("--lib={name}={}", dir.display()));
        }
    }
    expect
}

fn programs() -> Vec<PathBuf> {
    let mut sources: Vec<PathBuf> = std::fs::read_dir(run_dir())
        .expect("reads tests/run")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "lilac") || path.join("main.lilac").is_file()
        })
        .collect();
    sources.sort();
    sources
}

/// Compiles, links and runs one program; `Err` describes the first mismatch.
fn run_one(source: &Path, expect: &Expect, clang: &Path, scratch: &Path) -> Result<(), String> {
    let name = source.file_stem().unwrap().to_string_lossy().into_owned();
    if let Some(code) = &expect.error {
        let out = lilac()
            .arg("check")
            .args(&expect.libs)
            .arg(source)
            .output()
            .expect("runs lilac");
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            return Err(format!("expected {code}, but `check` succeeded"));
        }
        if !stderr.contains(&format!("[{code}]")) {
            return Err(format!("expected {code}, got:\n{stderr}"));
        }
        return Ok(());
    }

    let out = lilac()
        .arg("emit-llvm")
        .args(&expect.libs)
        .arg(source)
        .output()
        .expect("runs lilac");
    if !out.status.success() {
        return Err(format!(
            "emit-llvm failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let ll = scratch.join(format!("{name}.ll"));
    std::fs::write(&ll, &out.stdout).expect("writes IR");
    let exe = scratch.join(format!("{name}.exe"));
    let link = Command::new(clang)
        .arg("-Wno-override-module")
        .arg(&ll)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("runs clang");
    if !link.status.success() {
        return Err(format!(
            "link failed:\n{}",
            String::from_utf8_lossy(&link.stderr)
        ));
    }
    let run = Command::new(&exe).output().expect("runs executable");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    let want = if expect.stdout.is_empty() {
        String::new()
    } else {
        expect.stdout.join("\n") + "\n"
    };
    if stdout != want {
        return Err(format!(
            "stdout mismatch:\n got: {stdout:?}\nwant: {want:?}"
        ));
    }
    if run.status.code() != Some(expect.exit) {
        return Err(format!(
            "exit code {:?}, want {}",
            run.status.code(),
            expect.exit
        ));
    }
    Ok(())
}

fn run_all(pending: bool) {
    let Some(clang) = clang() else {
        eprintln!("skipping run tests: no clang (set LLVM_SYS_181_PREFIX)");
        return;
    };
    let scratch = std::env::temp_dir().join(format!(
        "lilac-run-{}-{}",
        std::process::id(),
        u8::from(pending)
    ));
    std::fs::create_dir_all(&scratch).expect("creates scratch dir");
    let mut failures = Vec::new();
    let mut ran = 0;
    for source in programs() {
        let header = if source.is_dir() {
            source.join("main.lilac")
        } else {
            source.clone()
        };
        let text = std::fs::read_to_string(&header).expect("reads program");
        let expect = parse_header(&text);
        if expect.pending.is_some() != pending {
            continue;
        }
        ran += 1;
        if let Err(why) = run_one(&source, &expect, &clang, &scratch) {
            let note = expect
                .pending
                .as_deref()
                .map(|p| format!(" (pending: {p})"))
                .unwrap_or_default();
            failures.push(format!("{}{note}: {why}", source.display()));
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        failures.is_empty(),
        "{} of {ran} programs failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn run_programs() {
    run_all(false);
}

/// Known bugs. Run with `cargo test -p lilac-cli --test run -- --ignored` to
/// see which still fail.
#[test]
#[ignore = "known bugs; each program names the phase that fixes it"]
fn run_pending_programs() {
    run_all(true);
}
