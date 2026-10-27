//! `lilac test`: `Standard`'s own tests (`tests/standard`, a project of
//! `test` blocks) pass in Debug and Release, and a test program's results,
//! failures, filters and libraries are reported as they should be.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The linking toolchain: the native sysroot where programs build natively,
/// else `clang` for `--libc`.
struct Toolchain {
    clang: PathBuf,
    sysroot: Option<PathBuf>,
}

fn toolchain() -> Option<Toolchain> {
    let Some(clang) = support::clang() else {
        eprintln!("skipping: no clang (set LLVM_SYS_181_PREFIX)");
        return None;
    };
    let sysroot = if cfg!(any(windows, target_os = "linux", target_os = "macos")) {
        let Some(sysroot) = support::sysroot() else {
            eprintln!("skipping: no lld in LLVM_SYS_181_PREFIX");
            return None;
        };
        Some(sysroot)
    } else {
        None
    };
    Some(Toolchain { clang, sysroot })
}

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A fresh scratch folder for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("lilac-test")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creates scratch folder");
    dir
}

/// Runs `lilac <args>` in `dir`, linking with `tools`, its build folder
/// `dir/build`.
fn lilac(tools: &Toolchain, dir: &Path, args: &[&str]) -> Output {
    let build = dir.join("build");
    let mut command = Command::new(env!("CARGO_BIN_EXE_lilac"));
    command
        .current_dir(dir)
        .args(args)
        .arg(format!("--build-dir={}", build.display()))
        .env("LILAC_LINKER", &tools.clang);
    match &tools.sysroot {
        Some(sysroot) => command.env("LILAC_SYSROOT", sysroot),
        None => command.arg("--libc"),
    };
    command.output().expect("runs lilac")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

fn standard_flag() -> String {
    format!(
        "--lib=Standard={}",
        repository().join("lib/Standard").display()
    )
}

#[test]
fn standard_tests_pass() {
    let Some(tools) = toolchain() else { return };
    let project = repository().join("tests/standard");
    let project = project.to_str().expect("UTF-8 path");
    for mode in ["--mode=debug", "--mode=release"] {
        let dir = scratch(&format!("standard{mode}"));
        let out = lilac(&tools, &dir, &["test", "-q", mode, project]);
        let stdout = text(&out.stdout);
        assert!(
            out.status.success(),
            "`lilac test {mode} tests/standard` failed:\n{stdout}{}",
            text(&out.stderr)
        );
        assert!(stdout.contains("test result: ok."), "{stdout}");
    }
}

const PROGRAM: &str = r#"using Standard
using Standard.Testing

main: function () nothing {
    Standard.Io.print_line("main ran")
}

test "adds" {
    x: Int32 = 1
    expect_equal[Int32](x + 2, 3)
}

test "expects the wrong sum" {
    x: Int32 = 1
    expect_equal[Int32](x + 2, 4)
}

test "writes text" {
    a: Int32 = -5
    expect_text($"{a} {a.compare(5)}", "-5 Less")
}

test "panics" {
    @panic("on purpose")
}
"#;

#[test]
fn results_failures_and_filters_are_reported() {
    let Some(tools) = toolchain() else { return };
    let dir = scratch("program");
    std::fs::write(dir.join("program.lilac"), PROGRAM).expect("writes program");
    let file = "program.lilac";
    let standard = standard_flag();

    let out = lilac(&tools, &dir, &["test", "-q", &standard, file]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}{}", text(&out.stderr));
    for line in [
        "test \"adds\" ... ok",
        "test \"expects the wrong sum\" ... FAILED",
        "test \"writes text\" ... ok",
        "test \"panics\" ... FAILED",
        "---- test \"expects the wrong sum\" at program.lilac:13:6 ----",
        "panic: expected 4, found 3",
        "panic: on purpose",
        "(exit code 101)",
        "test result: FAILED. 2 passed; 2 failed; 0 filtered out",
    ] {
        assert!(stdout.contains(line), "no `{line}` in:\n{stdout}");
    }
    assert!(!stdout.contains("main ran"), "{stdout}");

    let out = lilac(
        &tools,
        &dir,
        &[
            "test",
            "-q",
            "--filter=add",
            "--filter=text",
            &standard,
            file,
        ],
    );
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", text(&out.stderr));
    assert!(
        stdout.contains("test result: ok. 2 passed; 0 failed; 2 filtered out"),
        "{stdout}"
    );

    // `lilac build` leaves the tests out: `main` runs.
    let exe = dir.join(format!("program{}", std::env::consts::EXE_SUFFIX));
    let exe_arg = exe.to_str().expect("UTF-8 path");
    let out = lilac(
        &tools,
        &dir,
        &["build", "-q", &standard, "-o", exe_arg, file],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let run = Command::new(&exe).output().expect("runs the program");
    assert_eq!(text(&run.stdout), "main ran\n");
}

#[test]
fn a_test_program_needs_no_main_and_skips_library_tests() {
    let Some(tools) = toolchain() else { return };
    let dir = scratch("library");
    let library = dir.join("Helpers");
    std::fs::create_dir_all(&library).expect("creates library");
    std::fs::write(
        library.join("helpers.lilac"),
        "double: function (x: @bit32) y: @bit32 {\n    y = @i_add_32(x, x)\n}\n\ntest \"a library test\" {\n    @panic(\"library tests do not run\")\n}\n",
    )
    .expect("writes library");
    std::fs::write(
        dir.join("uses.lilac"),
        "using Helpers\nusing Standard.Testing\n\ntest \"doubles\" {\n    expect(double(0x2) == 0x4)\n}\n",
    )
    .expect("writes program");
    let helpers = format!("--lib=Helpers={}", library.display());
    let out = lilac(
        &tools,
        &dir,
        &["test", "-q", &standard_flag(), &helpers, "uses.lilac"],
    );
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", text(&out.stderr));
    assert!(stdout.contains("test \"doubles\" ... ok"), "{stdout}");
    assert!(!stdout.contains("library test"), "{stdout}");
    assert!(
        stdout.contains("test result: ok. 1 passed; 0 failed; 0 filtered out"),
        "{stdout}"
    );
}
