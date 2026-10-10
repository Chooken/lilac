//! Mounted libraries load and check whole: `using` only decides what a file
//! can see, and DCE keeps unused library code out of the output.

use std::path::{Path, PathBuf};
use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

fn standard_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/Standard")
}

/// A fresh folder under cargo's test scratch area.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creates scratch folder");
    dir
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("has a parent")).expect("creates folder");
    std::fs::write(path, text).expect("writes file");
}

/// An error in a library namespace nothing `using`s is still reported.
#[test]
fn an_error_in_an_unused_library_namespace_is_reported() {
    let dir = scratch("unused_library_error");
    write(&dir.join("main.lilac"), "main: function () nothing {\n}\n");
    write(
        &dir.join("lib/Unused/bad.lilac"),
        "broken: function () nothing {\n    value: @bit32 = missing\n}\n",
    );
    let out = lilac()
        .arg("check")
        .arg(format!("--lib=Lib={}", dir.join("lib").display()))
        .arg(dir.join("main.lilac"))
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check passed:\n{stderr}");
    assert!(stderr.contains("Lib/Unused/bad.lilac"), "{stderr}");
}

/// Mounting `Standard` adds only its exports (the thread start, and what
/// it calls) to a program that does not use it: every `export` is kept.
#[test]
fn an_unused_library_adds_only_its_exports_to_the_ir() {
    let dir = scratch("unused_library_ir");
    let main = dir.join("main.lilac");
    write(&main, "main: function () nothing {\n}\n");
    let emit = |lib: Option<String>| {
        let mut command = lilac();
        command.arg("emit-ir");
        command.args(lib);
        let out = command.arg(&main).output().expect("runs lilac");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    };
    let functions = |ir: Vec<u8>| -> Vec<String> {
        String::from_utf8_lossy(&ir)
            .lines()
            .filter(|line| line.contains("function @") || line.starts_with("extern "))
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(functions(emit(None)), ["function @main"]);
    let mounted = emit(Some(format!("--lib=Standard={}", standard_dir().display())));
    assert_eq!(
        functions(mounted),
        [
            "function @main",
            "inline function @Standard.Os.null",
            "function @Standard.Os.thread_start",
        ]
    );
}

/// All of `Standard` checks silently on every target it supports.
#[test]
fn standard_checks_whole_on_every_target() {
    let dir = scratch("standard_whole");
    let main = dir.join("main.lilac");
    write(&main, "main: function () nothing {\n}\n");
    for target in [
        "x86_64-pc-windows-msvc",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-unknown-none",
    ] {
        let out = lilac()
            .arg("check")
            .arg(format!("--target={target}"))
            .arg(format!("--lib=Standard={}", standard_dir().display()))
            .arg(&main)
            .output()
            .expect("runs lilac");
        assert!(
            out.status.success() && out.stderr.is_empty(),
            "{target}:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
