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

/// Problem 7: a chain of 120 different generic functions, none recursive,
/// is no infinite recursion (`E051` counts recursion per definition).
#[test]
fn long_chain_of_distinct_generics_checks() {
    let dir = scratch("long_chain_of_distinct_generics_checks");
    let mut text = String::new();
    for index in 1..120 {
        let next = index + 1;
        let _ = writeln!(
            text,
            "f{index}[T]: function (x: T) y: T {{ y = f{next}[T](x) }}"
        );
    }
    text += "f120[T]: function (x: T) y: T { y = x }\n\n";
    text += "main: function () nothing {\n    v: @bit32 = f1[@bit32](0x1)\n}\n";
    write(&dir, "main.lilac", text);
    let out = check(&dir, "main.lilac", Duration::from_secs(30));
    assert!(out.success, "{}", out.stderr);
}

/// Problem 8: a generic that instantiates itself twice with growing types
/// built 2^16 instances (16 s) before `E051`. Recursion is followed depth
/// first, so it fails fast, and once: every other branch stops at the
/// definition that already failed.
#[test]
fn branching_generic_recursion_fails_fast() {
    let dir = scratch("branching_generic_recursion_fails_fast");
    write(
        &dir,
        "main.lilac",
        "W[T]: structure {\n    value: T\n}\n\n\
         V[T]: structure {\n    value: T\n}\n\n\
         grow[T]: function () nothing {\n    grow[W[T]]()\n    grow[V[T]]()\n}\n\n\
         main: function () nothing {\n    grow[@bit32]()\n}\n",
    );
    // Under a second in a debug build, process start included.
    let out = check(&dir, "main.lilac", Duration::from_secs(1));
    assert!(!out.success, "{}", out.stderr);
    assert_eq!(error_codes(&out.stderr), ["E051"], "{}", out.stderr);
    assert!(
        out.stderr
            .contains("note: the type arguments grow each time: grow[@bit32] -> grow[W[@bit32]]"),
        "{}",
        out.stderr
    );
}

/// Problem 8 with three branches per level (3^16 instances before), and with
/// arguments whose text and tree grow 4^n (`P[T, T, T, T]`): each ends with
/// `E051` within a second. When `P` holds all four, its size grows 4^n too,
/// and the recursion ends sooner, at the object size limit (`E067`, folded
/// into `E042`).
#[test]
fn wider_generic_recursion_fails_fast() {
    let dir = scratch("wider_generic_recursion_fails_fast");
    write(
        &dir,
        "three.lilac",
        "W[T]: structure {\n    value: T\n}\n\n\
         V[T]: structure {\n    value: T\n}\n\n\
         U[T]: structure {\n    value: T\n}\n\n\
         grow[T]: function () nothing {\n    grow[W[T]]()\n    grow[V[T]]()\n    grow[U[T]]()\n}\n\n\
         main: function () nothing {\n    grow[@bit32]()\n}\n",
    );
    let quad = |fields: &str| {
        format!(
            "P[A, B, C, D]: structure {{\n{fields}}}\n\n\
             grow[T]: function () nothing {{\n    grow[P[T, T, T, T]]()\n}}\n\n\
             main: function () nothing {{\n    grow[@bit32]()\n}}\n"
        )
    };
    write(&dir, "quad.lilac", quad("    a: A\n"));
    write(
        &dir,
        "wide.lilac",
        quad("    a: A\n    b: B\n    c: C\n    d: D\n"),
    );
    for (file, code) in [
        ("three.lilac", "E051"),
        ("quad.lilac", "E051"),
        ("wide.lilac", "E042"),
    ] {
        let out = check(&dir, file, Duration::from_secs(1));
        assert!(!out.success, "{file}: {}", out.stderr);
        assert_eq!(error_codes(&out.stderr), [code], "{file}: {}", out.stderr);
    }
}

/// Types nested by value through distinct definitions are no recursion
/// either: 1,024 levels (the deepest accepted) check in a debug build, and
/// one more is `E051`, never a stack overflow.
#[test]
fn deep_type_nesting_is_bounded() {
    let dir = scratch("deep_type_nesting_is_bounded");
    let nested = |depth: usize| {
        let mut text = String::new();
        for index in 1..depth {
            let _ = writeln!(text, "O{index}: structure {{\n    a: O{}\n}}", index + 1);
        }
        let _ = writeln!(text, "O{depth}: structure {{\n    a: @bit32\n}}\n");
        text + "g: O1\n\nmain: function () nothing {\n    x: O1 = g\n}\n"
    };
    write(&dir, "deepest.lilac", nested(1024));
    write(&dir, "deeper.lilac", nested(1025));
    let out = check(&dir, "deepest.lilac", Duration::from_secs(30));
    assert!(out.success, "{}", out.stderr);
    let out = check(&dir, "deeper.lilac", Duration::from_secs(30));
    assert_eq!(error_codes(&out.stderr), ["E051"], "{}", out.stderr);
    assert!(
        out.stderr.contains("types nest too deeply to lay out"),
        "{}",
        out.stderr
    );
}

/// Problem 9: `x86` is the user-facing name of `i686`; the triple LLVM gets
/// is `i686-unknown-linux`, not `x86-unknown-linux` (an internal error).
#[test]
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
fn self_containing_enum_reports_once() {
    let dir = scratch("self_containing_enum_reports_once");
    write(
        &dir,
        "main.lilac",
        "E: either {\n    A: E\n    B\n}\n\nmain: function () nothing {\n    e: E = .B\n}\n",
    );
    let out = check(&dir, "main.lilac", Duration::from_secs(30));
    assert_eq!(error_codes(&out.stderr), ["E053"], "{}", out.stderr);
}

/// Problem 13 at every kind of use (`tests/ui/FAILS_failed_type_cascade`):
/// bindings, assignments, a constant and a global, a comparison, an `if`
/// value, a cast, an inferred struct literal and a call through a local.
#[test]
fn failed_type_cascade_reports_once() {
    let ui = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/ui");
    let out = check(
        &ui,
        "FAILS_failed_type_cascade.lilac",
        Duration::from_secs(30),
    );
    assert_eq!(error_codes(&out.stderr), ["E053"], "{}", out.stderr);
}

/// Problem 14: a folder link back to its parent (`A/B -> A`) loaded the same
/// file once per level until the path limit. The second path to a loaded
/// folder is `E025`. Skipped where the OS refuses to create the link.
#[test]
fn folder_link_cycle_is_e025() {
    link_cycle_is_e025("folder_link_cycle_is_e025", link_folder);
}

/// Problem 14 through a directory junction (Windows), which a user creates
/// without any privilege.
#[cfg(windows)]
#[test]
fn folder_junction_cycle_is_e025() {
    link_cycle_is_e025("folder_junction_cycle_is_e025", junction);
}

/// `A/B` made a link to `A` by `link`, below a project using `A`, checks
/// with one `E025`.
fn link_cycle_is_e025(name: &str, link: fn(&Path, &Path) -> std::io::Result<()>) {
    let dir = scratch(name);
    std::fs::create_dir_all(dir.join("A")).expect("creates A");
    write(
        &dir,
        "main.lilac",
        "using A\n\nmain: function () nothing {\n    f()\n}\n",
    );
    write(&dir.join("A"), "a.lilac", "f: function () nothing {}\n");
    if let Err(error) = link(&dir.join("A"), &dir.join("A").join("B")) {
        eprintln!("skipping: cannot create a folder link: {error}");
        return;
    }
    let out = check(&dir, ".", Duration::from_secs(30));
    assert!(!out.success, "{}", out.stderr);
    assert_eq!(error_codes(&out.stderr), ["E025"], "{}", out.stderr);
    assert!(
        out.stderr
            .contains("error[E025]: folder `A.B` is `A` again (through a link)"),
        "{}",
        out.stderr
    );
}

/// A directory symbolic link, else (it needs a privilege Windows grants only
/// in developer mode) a directory junction, which needs none.
#[cfg(windows)]
fn link_folder(target: &Path, link: &Path) -> std::io::Result<()> {
    if std::os::windows::fs::symlink_dir(target, link).is_ok() {
        return Ok(());
    }
    junction(target, link)
}

/// A directory junction at `link` to `target`.
#[cfg(windows)]
fn junction(target: &Path, link: &Path) -> std::io::Result<()> {
    let status = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "`mklink /J` failed: {status}"
        )))
    }
}

#[cfg(unix)]
fn link_folder(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(any(windows, unix)))]
fn link_folder(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// A library vendored inside the project (`--lib=Util=proj/vendor/Util`)
/// loads once, under its mount name: the project's own walk skips its
/// folder, so the project checks clean.
#[test]
fn vendored_library_checks_clean() {
    let dir = scratch("vendored_library_checks_clean");
    let util = dir.join("proj").join("vendor").join("Util");
    std::fs::create_dir_all(&util).expect("creates vendor/Util");
    write(
        &dir.join("proj"),
        "main.lilac",
        "using Util\n\nmain: function () nothing {\n    helper()\n}\n",
    );
    write(&util, "util.lilac", "helper: function () nothing {\n}\n");
    let out = lilac_within(
        &dir,
        &["check", "proj", "--lib=Util=proj/vendor/Util"],
        Duration::from_secs(30),
    )
    .expect("finishes");
    assert!(out.success, "{}", out.stderr);
    assert!(error_codes(&out.stderr).is_empty(), "{}", out.stderr);
}

/// The program of `tests/run/wide_generic_types` with `n` distinct generic
/// functions, each wrapping its argument in `P[T, T, T, T]`: the type
/// `f{n}` takes, written out, is `4^(n-1)` `Empty`s. The last one panics,
/// so with stack traces on, every call site and the panic name the types.
fn wide_generics(n: usize) -> String {
    let mut text = String::from(
        "Empty: structure {\n}\n\n\
         P[A, B, C, D]: structure {\n    a: A\n    b: B\n    c: C\n    d: D\n}\n\n",
    );
    for index in 1..n {
        let _ = writeln!(
            text,
            "f{index}[T]: function (x: T) nothing {{\n    \
             y: P[T, T, T, T] = P {{\n        a = x\n        b = x\n        c = x\n        d = x\n    }}\n    \
             f{}[P[T, T, T, T]](y)\n}}\n",
            index + 1
        );
    }
    let _ = writeln!(
        text,
        "f{n}[T]: function (x: T) nothing {{\n    @panic(\"too deep\")\n}}\n"
    );
    text + "main: function () nothing {\n    f1[Empty](Empty {})\n}\n"
}

/// Phase 10: types whose written form grows 4^n through `n` distinct
/// generics (no recursion, so no limit applies) compile in linear time and
/// space. At n = 20 the program emits IR within 2 s, process start
/// included; before, n = 11 took 2.2 s and wrote 82 MB. Its longest symbol
/// grows linearly from n = 10 to n = 20, and the names it stores for
/// traces are bounded, so no line of the IR is long.
#[test]
fn wide_generic_types_stay_linear() {
    let dir = scratch("wide_generic_types_stay_linear");
    write(&dir, "ten.lilac", wide_generics(10));
    write(&dir, "twenty.lilac", wide_generics(20));
    let limit = Duration::from_secs(2);
    let longest_symbol = |file: &str| {
        let out = lilac_within(&dir, &["emit-ir", file, "--stack-traces=on"], limit)
            .unwrap_or_else(|| panic!("`lilac emit-ir {file}` ran past {limit:?}"));
        assert!(out.success, "{file}: {}", out.stderr);
        let longest_line = out.stdout.lines().map(str::len).max().unwrap_or(0);
        assert!(
            longest_line < 1_000,
            "{file}: a line of {longest_line} bytes"
        );
        out.stdout
            .split_whitespace()
            .filter(|word| word.starts_with("@.root.f"))
            .map(str::len)
            .max()
            .unwrap_or(0)
    };
    let ten = longest_symbol("ten.lilac");
    let twenty = longest_symbol("twenty.lilac");
    assert!(
        ten < twenty && twenty < 5 * ten / 2,
        "longest symbol: {ten} bytes at n = 10, {twenty} at n = 20"
    );
    let out = lilac_within(&dir, &["emit-llvm", "twenty.lilac"], limit)
        .unwrap_or_else(|| panic!("`lilac emit-llvm twenty.lilac` ran past {limit:?}"));
    assert!(out.success, "{}", out.stderr);
}

/// Phase 10: `lilac demangle` writes the types in each symbol as Lilac
/// types, and leaves one with no types, or a malformed one, as it is.
/// With no symbol it filters its input, so `lilac emit-ir … | lilac
/// demangle` reads like source.
#[test]
fn demangle_reads_symbols() {
    let dir = scratch("demangle_reads_symbols");
    let out = lilac_within(
        &dir,
        &[
            "demangle",
            "GenBox[m].@on_drop(RI6GenBoxmEE)",
            ".root.id[I1PNN1A1B6HolderB2_E]",
            "@conversion(mEh)",
            "plain.name",
            "bad[Q]",
        ],
        Duration::from_secs(30),
    )
    .expect("finishes");
    assert!(out.success, "{}", out.stderr);
    assert_eq!(
        out.stdout.lines().collect::<Vec<_>>(),
        [
            "GenBox[@bit32].@on_drop(ref GenBox[@bit32])",
            ".root.id[P[A.B.Holder, A.B.Holder]]",
            "@conversion(@bit32) -> @bit8",
            "plain.name",
            "bad[Q]",
        ]
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_lilac"))
        .arg("demangle")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("runs lilac");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || {
        use std::io::Write as _;
        stdin
            .write_all(
                b"    call @Box[m].get (in %a) (out)\n  alloca [4 x #bit32] %b\nno symbols\n",
            )
            .expect("writes input");
    });
    let output = child.wait_with_output().expect("waits for lilac");
    writer.join().expect("input written");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "    call @Box[@bit32].get (in %a) (out)\n  alloca [4 x #bit32] %b\nno symbols\n"
    );

    let out =
        lilac_within(&dir, &["demangle", "--nope"], Duration::from_secs(30)).expect("finishes");
    assert!(!out.success);
    assert!(
        out.stderr.contains("unknown flag: --nope"),
        "{}",
        out.stderr
    );
}
