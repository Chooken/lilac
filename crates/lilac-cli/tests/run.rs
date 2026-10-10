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
//! - `// expect-stderr: <line>` — one expected stderr line, like
//!   `expect-stdout`. With none, stderr is not checked.
//! - `// expect-exit: <n>` — the expected exit code (default `0`).
//!   `expect-exit: trap` instead expects the program to stop on a trap
//!   instruction (a signal on Unix, an exception status on Windows).
//! - `// expect-error: <code>` — the program must fail `lilac check` with this
//!   diagnostic code; nothing is linked or run.
//! - `// pending: <reason>` — a known bug: skipped by `run_programs` and
//!   exercised by the ignored `run_pending_programs` instead. Remove the line
//!   when the fix lands.
//! - `// lib: <Name>=<dir>` — mounts a library (`--lib`); `<dir>` is relative
//!   to the repository root (repeatable). `Standard` (`lib/Standard`) is
//!   mounted for every program, so it supplies the platform hooks.
//! - `// no-standard` — does not mount `Standard` (the program brings its own
//!   runtime).
//! - `// libc` — builds with `--libc` (the program uses `Standard.C`).
//! - `// build-arg: <flag>` — an extra `lilac build` flag (repeatable).
//!
//! `lilac` runs in `tests/run` and is given the program's bare name, so
//! source locations (in diagnostics and stack traces) read the same on every
//! machine. The program runs in a scratch folder, where any files it
//! creates land.

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
    stderr: Vec<String>,
    build_args: Vec<String>,
    exit: i32,
    trap: bool,
    error: Option<String>,
    pending: Option<String>,
    libs: Vec<String>,
    libc: bool,
    no_standard: bool,
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
        } else if let Some(rest) = comment.strip_prefix("expect-stderr:") {
            expect
                .stderr
                .push(rest.strip_prefix(' ').unwrap_or(rest).to_owned());
        } else if let Some(rest) = comment.strip_prefix("build-arg:") {
            expect.build_args.push(rest.trim().to_owned());
        } else if let Some(rest) = comment.strip_prefix("expect-exit:") {
            if rest.trim() == "trap" {
                expect.trap = true;
            } else {
                expect.exit = rest.trim().parse().expect("expect-exit is an integer");
            }
        } else if let Some(rest) = comment.strip_prefix("expect-error:") {
            expect.error = Some(rest.trim().to_owned());
        } else if comment.trim_end() == "libc" {
            expect.libc = true;
        } else if comment.trim_end() == "no-standard" {
            expect.no_standard = true;
        } else if let Some(rest) = comment.strip_prefix("pending:") {
            expect.pending = Some(rest.trim().to_owned());
        } else if let Some(rest) = comment.strip_prefix("lib:") {
            let (name, dir) = rest.trim().split_once('=').expect("lib: is Name=dir");
            expect.libs.push(lib_flag(name, dir));
        }
    }
    if !expect.no_standard {
        expect.libs.push(lib_flag("Standard", "lib/Standard"));
    }
    expect
}

/// `--lib=<name>=<dir>`, with `dir` relative to the repository root.
fn lib_flag(name: &str, dir: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(dir);
    format!("--lib={name}={}", dir.display())
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
    let file = source.file_name().expect("program has a name");
    if let Some(code) = &expect.error {
        let out = lilac()
            .current_dir(run_dir())
            .arg("check")
            .args(&expect.libs)
            .arg(file)
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
        .current_dir(run_dir())
        .args(["build", "-q", "-o"])
        .arg(&exe)
        .arg(format!("--build-dir={}", scratch.join("build").display()))
        .args(&expect.libs)
        .args(&expect.build_args)
        .arg(file)
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
    // In the scratch folder, so a program's files land there.
    let run = Command::new(&exe)
        .current_dir(scratch)
        .output()
        .expect("runs executable");

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
    if !expect.stderr.is_empty() {
        let stderr = String::from_utf8_lossy(&run.stderr).replace("\r\n", "\n");
        let want = expect.stderr.join("\n") + "\n";
        if stderr != want {
            return Err(format!(
                "stderr mismatch:\n got: {stderr:?}\nwant: {want:?}"
            ));
        }
    }
    if expect.trap {
        if !ended_by_trap(run.status) {
            return Err(format!("exit code {:?}, want a trap", run.status.code()));
        }
        return Ok(());
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

/// Whether a process stopped on a trap instruction rather than exiting:
/// killed by a signal on Unix, or ended with an exception status (a negative
/// code, such as `STATUS_ILLEGAL_INSTRUCTION`) on Windows.
fn ended_by_trap(status: std::process::ExitStatus) -> bool {
    if cfg!(windows) {
        status.code().is_some_and(|code| code < 0)
    } else {
        status.code().is_none()
    }
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
