//! Pending tests for the robustness plan (`docs/robustness-plan.md`): each
//! reproduces one problem of its "Problems this fixes" table through the
//! CLI and states the behavior the named phase delivers. They are ignored
//! until that phase lands; run them with
//! `cargo test -p lilac-cli --test robustness -- --ignored`.
//!
//! Every compile runs under a time limit and is killed past it, so a test
//! of an input that hangs today fails instead of hanging the suite.
//!
//! The run-test programs of the same plan are in `tests/run` (`// pending:`).

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// What a compile printed, and how it ended.
struct Finished {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Runs `lilac <args>` in `dir`; `None` when it is still running after
/// `limit` (it is then killed).
fn lilac_within(dir: &Path, args: &[&str], limit: Duration) -> Option<Finished> {
    let start = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lilac"))
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("runs lilac");
    // Drained on threads, so a full pipe never stalls the child.
    let stdout = drain(child.stdout.take().expect("piped stdout"));
    let stderr = drain(child.stderr.take().expect("piped stderr"));
    let status = loop {
        if let Some(status) = child.try_wait().expect("polls lilac") {
            break status;
        }
        if start.elapsed() > limit {
            kill(&mut child);
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Some(Finished {
        success: status.success(),
        stdout: stdout.join().expect("reads stdout"),
        stderr: stderr.join().expect("reads stderr"),
    })
}

fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        // A read error leaves what was read; the assertions report it.
        let _ = pipe.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

fn kill(child: &mut Child) {
    // It may have exited between the poll and the kill.
    let _ = child.kill();
    let _ = child.wait();
}

/// A fresh scratch folder for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("robustness")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creates scratch folder");
    dir
}

/// Writes `text` as `dir/name`.
fn write(dir: &Path, name: &str, text: impl AsRef<[u8]>) {
    std::fs::write(dir.join(name), text).expect("writes source");
}

/// `lilac check <file>` in `dir`, which must finish within `limit`.
fn check(dir: &Path, file: &str, limit: Duration) -> Finished {
    lilac_within(dir, &["check", file], limit)
        .unwrap_or_else(|| panic!("`lilac check {file}` ran past {limit:?}"))
}

/// The `error[...]` headers of a compile's output, in order.
fn error_codes(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix("error["))
        .filter_map(|rest| rest.split_once(']'))
        .map(|(code, _)| code)
        .collect()
}

/// Problem 1: a lone `}` or a bad escape in an f-string left the lexer at
/// the same position forever. Each input ends with `E000`.
#[test]
fn fstring_errors_end() {
    let dir = scratch("fstring_errors_end");
    let mut failures = Vec::new();
    for (index, literal) in [r#"$"a}b""#, r#"$"\q""#, r#"$"a\""#].iter().enumerate() {
        let file = format!("case{index}.lilac");
        write(
            &dir,
            &file,
            format!("main: function () nothing {{\n    s: @bit8 = {literal}\n}}\n"),
        );
        match lilac_within(&dir, &["check", &file], Duration::from_secs(5)) {
            None => failures.push(format!("{literal}: still lexing after 5 s")),
            Some(out) if !error_codes(&out.stderr).contains(&"E000") => {
                failures.push(format!("{literal}: no E000:\n{}", out.stderr));
            }
            Some(_) => {}
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Problem 11: a bad escape ends a string today, and the rest re-lexes into
/// more errors. It is one error, and the string still ends at its `"`.
#[test]
fn bad_string_escape_is_one_error() {
    let dir = scratch("bad_string_escape_is_one_error");
    write(
        &dir,
        "main.lilac",
        "main: function () nothing {\n    s: @bit8 = \"\\q\"\n}\n",
    );
    let out = check(&dir, "main.lilac", Duration::from_secs(5));
    assert_eq!(error_codes(&out.stderr), ["E000"], "{}", out.stderr);
}

/// Problem 12: a UTF-8 byte-order mark at the start of a file is trivia.
#[test]
fn byte_order_mark_is_trivia() {
    let dir = scratch("byte_order_mark_is_trivia");
    write(
        &dir,
        "main.lilac",
        b"\xEF\xBB\xBFmain: function () nothing {}\n",
    );
    let out = check(&dir, "main.lilac", Duration::from_secs(5));
    assert!(out.success, "{}", out.stderr);
}

/// Problem 7: a chain of 18 different generic functions, none recursive,
/// is no infinite recursion (`E051` counts recursion per definition).
#[test]
#[ignore = "robustness plan phase 6"]
fn long_chain_of_distinct_generics_checks() {
    let dir = scratch("long_chain_of_distinct_generics_checks");
    let mut text = String::new();
    for index in 1..18 {
        let next = index + 1;
        let _ = writeln!(
            text,
            "f{index}[T]: function (x: T) y: T {{ y = f{next}[T](x) }}"
        );
    }
    text += "f18[T]: function (x: T) y: T { y = x }\n\n";
    text += "main: function () nothing {\n    v: @bit32 = f1[@bit32](0x1)\n}\n";
    write(&dir, "main.lilac", text);
    let out = check(&dir, "main.lilac", Duration::from_secs(30));
    assert!(out.success, "{}", out.stderr);
}

/// Problem 8: a generic that instantiates itself twice with growing types
/// built 2^16 instances (16 s) before `E051`. It fails fast.
#[test]
#[ignore = "robustness plan phase 6"]
fn branching_generic_recursion_fails_fast() {
    let dir = scratch("branching_generic_recursion_fails_fast");
    write(
        &dir,
        "main.lilac",
        "W[T]: object {\n    value: T\n}\n\n\
         V[T]: object {\n    value: T\n}\n\n\
         grow[T]: function () nothing {\n    grow[W[T]]()\n    grow[V[T]]()\n}\n\n\
         main: function () nothing {\n    grow[@bit32]()\n}\n",
    );
    // Under a second in a debug build, process start included.
    let out = check(&dir, "main.lilac", Duration::from_secs(1));
    assert!(!out.success, "{}", out.stderr);
    assert!(error_codes(&out.stderr).contains(&"E051"), "{}", out.stderr);
}

/// Problem 9: `x86` is the user-facing name of `i686`; the triple LLVM gets
/// is `i686-unknown-linux`, not `x86-unknown-linux` (an internal error).
#[test]
#[ignore = "robustness plan phase 2"]
fn x86_target_emits_i686() {
    let dir = scratch("x86_target_emits_i686");
    write(&dir, "main.lilac", "main: function () nothing {}\n");
    let out = lilac_within(
        &dir,
        &["emit-llvm", "--target=x86-linux", "main.lilac"],
        Duration::from_secs(30),
    )
    .expect("emit-llvm finishes");
    assert!(out.success, "{}", out.stderr);
    assert!(
        out.stdout.contains("target triple = \"i686-"),
        "{}",
        out.stdout
    );
}

/// Problem 13: an enum that contains itself is `E053`, and its type is an
/// error type afterwards, so a use of `.B` adds no `E024`.
#[test]
#[ignore = "robustness plan phase 7"]
fn self_containing_enum_reports_once() {
    let dir = scratch("self_containing_enum_reports_once");
    write(
        &dir,
        "main.lilac",
        "E: enum {\n    A: E\n    B\n}\n\nmain: function () nothing {\n    e: E = .B\n}\n",
    );
    let out = check(&dir, "main.lilac", Duration::from_secs(30));
    assert_eq!(error_codes(&out.stderr), ["E053"], "{}", out.stderr);
}

/// Problem 14: a folder link back to its parent (`A/B -> A`) loaded the same
/// file once per level until the path limit. The second path to a loaded
/// folder is `E025`. Skipped where the OS refuses to create the link.
#[test]
#[ignore = "robustness plan phase 7"]
fn folder_link_cycle_is_e025() {
    let dir = scratch("folder_link_cycle_is_e025");
    std::fs::create_dir_all(dir.join("A")).expect("creates A");
    write(
        &dir,
        "main.lilac",
        "using A\n\nmain: function () nothing {\n    f()\n}\n",
    );
    write(&dir.join("A"), "a.lilac", "f: function () nothing {}\n");
    if let Err(error) = link_folder(&dir.join("A"), &dir.join("A").join("B")) {
        eprintln!("skipping: cannot create a folder link: {error}");
        return;
    }
    let out = check(&dir, ".", Duration::from_secs(30));
    assert!(!out.success, "{}", out.stderr);
    assert!(error_codes(&out.stderr).contains(&"E025"), "{}", out.stderr);
}

#[cfg(windows)]
fn link_folder(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(unix)]
fn link_folder(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(any(windows, unix)))]
fn link_folder(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}
