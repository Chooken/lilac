//! Run tests: every `tests/run/*.lilac` is built with `lilac build`,
//! executed, and compared against its header.
//!
//! Programs build natively (no C library) on Windows, Linux and macOS, with a
//! development sysroot assembled from `toolchain/` and the LLVM tree's `lld`
//! (see `support::sysroot`). Elsewhere, and for `// libc` programs, they
//! build with `--libc`, linking through the toolchain `clang` (as
//! `LILAC_LINKER`).
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
//! - `// libc` — builds with `--libc` (the program uses `Standard.C`).
//!
//! A program is a single `*.lilac` file or a folder project, whose header is
//! read from its `main.lilac`.
//!
//! Programs print with `Standard.Io` and set the exit code with
//! `Standard.Process.exit`.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

fn run_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/run")
}

/// How programs are linked.
struct Toolchain {
    clang: PathBuf,
    /// The native sysroot; `None` builds every program with `--libc`.
    sysroot: Option<PathBuf>,
}

#[derive(Default)]
struct Expect {
    stdout: Vec<String>,
    exit: i32,
    error: Option<String>,
    pending: Option<String>,
    libs: Vec<String>,
    libc: bool,
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
        } else if comment.trim_end() == "libc" {
            expect.libc = true;
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
fn run_one(
    source: &Path,
    expect: &Expect,
    toolchain: &Toolchain,
    scratch: &Path,
) -> Result<(), String> {
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

    let exe = scratch.join(format!("{name}.exe"));
    let mut build = lilac();
    build
        .args(["build", "-q", "-o"])
        .arg(&exe)
        .arg(format!("--build-dir={}", scratch.join("build").display()))
        .args(&expect.libs)
        .arg(source)
        .env("LILAC_LINKER", &toolchain.clang);
    match &toolchain.sysroot {
        Some(sysroot) if !expect.libc => build.env("LILAC_SYSROOT", sysroot),
        _ => build.arg("--libc"),
    };
    let out = build.output().expect("runs lilac");
    if !out.status.success() {
        return Err(format!(
            "build failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
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
    let Some(clang) = support::clang() else {
        eprintln!("skipping run tests: no clang (set LLVM_SYS_181_PREFIX)");
        return;
    };
    let sysroot = if cfg!(any(windows, target_os = "linux", target_os = "macos")) {
        let Some(sysroot) = support::sysroot() else {
            eprintln!("skipping run tests: no lld in LLVM_SYS_181_PREFIX");
            return;
        };
        Some(sysroot)
    } else {
        None
    };
    let toolchain = Toolchain { clang, sysroot };
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
        if let Err(why) = run_one(&source, &expect, &toolchain, &scratch) {
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
