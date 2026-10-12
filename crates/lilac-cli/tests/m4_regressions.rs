//! Task 3 regression: unused generic object members must not reach IR.
//!
//! A non-generic method or in-body hook of a generic object carries `Param`
//! slots in its template signature. Before the fix these were checked and
//! emitted as concrete `HirFunc`s, lowering to `#T` and tripping the verifier
//! with a false `E070` compiler bug on otherwise-valid, unused generics.

use std::fmt::Write as _;
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
fn e051_chain_notes_stay_human_readable() {
    // The E051 instantiation chain must never leak mangled IR symbols
    // (`grow[m]`); it reads as the source form (`grow[@bit32]`).
    for fixture in [
        "FAILS_generic_cyclic.lilac",
        "FAILS_generic_object_cycle.lilac",
    ] {
        let output = lilac()
            .args(["check", &ui(fixture)])
            .output()
            .expect("runs lilac");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{fixture} should fail:\n{stderr}");
        assert!(
            stderr.contains("note: instantiation chain: "),
            "{fixture}:\n{stderr}"
        );
        assert!(
            lilac_ty::demangle_text(&stderr) == stderr,
            "mangled symbol leaked into {fixture} diagnostics:\n{stderr}"
        );
    }
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

/// The terminal shows only the first ten errors, but every error is still
/// found: a failing generic instance after ten unrelated errors reports its
/// `E042` (in full in JSON, counted in the human summary).
#[test]
fn errors_past_the_shown_ten_are_still_found() {
    let mut body = String::new();
    for index in 0..11 {
        writeln!(body, "    x{index}: @bit32 = missing{index}").expect("writes");
    }
    let source = format!(
        "gen_function[T]: function () out: T {{\n    out = T.other()\n}}\n\nmain: function () nothing {{\n{body}    y: @bit32 = gen_function[@bit32]()\n}}\n"
    );
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("many_errors.lilac");
    std::fs::write(&path, source).expect("writes");

    let json = lilac()
        .args(["check", "--error-format=json"])
        .arg(&path)
        .output()
        .expect("runs lilac");
    let stdout = String::from_utf8_lossy(&json.stdout);
    let stderr = String::from_utf8_lossy(&json.stderr);
    let all = format!("{stdout}{stderr}");
    assert_eq!(all.matches("\"code\":\"E021\"").count(), 11, "{all}");
    assert_eq!(all.matches("\"code\":\"E042\"").count(), 1, "{all}");

    let human = lilac()
        .arg("check")
        .arg(&path)
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&human.stderr);
    assert_eq!(stderr.matches("error[").count(), 10, "{stderr}");
    assert!(stderr.contains("12 previous errors"), "{stderr}");
}

/// A callee that failed, by its body or its name, still has its call
/// checked: the arity, the result type and every argument.
#[test]
fn failed_callees_do_not_hide_their_calls() {
    let source = "\
broken: function (a: @bit32) r: @bit32 {
    r = nope
}

main: function () nothing {
    x: @bit32 = undefined_fn(vlaue)
    y: @bit32 = broken(vlaue2, 0x2)
    z: @bit64 = broken(0x1)
}
";
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("failed_callees.lilac");
    std::fs::write(&path, source).expect("writes");
    let output = lilac()
        .arg("check")
        .arg(&path)
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&output.stderr);
    for expected in [
        "unresolved name `nope`",
        "unresolved name `undefined_fn`",
        "unresolved name `vlaue`",
        "wrong number of arguments: expected 1, found 2",
        "unresolved name `vlaue2`",
        "no conversion from @bit32 to @bit64",
    ] {
        assert!(stderr.contains(expected), "missing `{expected}`:\n{stderr}");
    }
    assert!(stderr.contains("6 previous errors"), "{stderr}");
}

/// Unused names and unreachable statements are editor hints only: the
/// command line prints nothing for them.
#[test]
fn hints_print_nothing_on_the_command_line() {
    let source = "\
private {
    unused_helper: function () nothing {
    }
}

main: function () nothing {
    unused: @bit32 = 0x1
    return
    later: @bit32 = 0x4
}
";
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("hints_only.lilac");
    std::fs::write(&path, source).expect("writes");
    let output = lilac()
        .arg("check")
        .arg(&path)
        .output()
        .expect("runs lilac");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}
