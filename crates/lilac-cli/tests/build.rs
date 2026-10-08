//! `lilac build`: the build folder, progress output, and failure modes.
//!
//! Every case runs in its own temporary folder. Cases that link use
//! `--libc` with the toolchain `clang` (as `LILAC_LINKER`) and are skipped
//! when it is missing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const HELLO: &str = "putchar: extern function (c: @bit32) result: @bit32\n\
                     main: function () nothing {\n    r: @bit32 = putchar('H')\n    r = putchar(0xA)\n}\n";

/// Locates the toolchain `clang`, as `tests/run.rs` does.
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

/// A fresh temporary folder for one case.
fn scratch(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lilac-build-{}-{case}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creates scratch dir");
    dir
}

/// Runs `lilac build <args>` in `dir`, linking with `clang` when given.
fn build(dir: &Path, args: &[&str], clang: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lilac"));
    command.arg("build").args(args).current_dir(dir);
    command.env_remove("LILAC_LINKER").env("NO_COLOR", "1");
    if let Some(clang) = clang {
        command.env("LILAC_LINKER", clang);
    }
    command.output().expect("runs lilac")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn obj(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.obj")
    } else {
        format!("{name}.o")
    }
}

#[test]
fn builds_into_the_mode_folder_and_runs() {
    let Some(clang) = clang() else {
        eprintln!("skipping: no clang");
        return;
    };
    let dir = scratch("hello");
    std::fs::write(dir.join("hello.lilac"), HELLO).expect("writes source");
    let out = build(&dir, &["--libc", "hello.lilac"], Some(&clang));
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");

    let mode = dir.join("build").join("debug");
    assert!(mode.join("obj").join(obj("hello")).is_file(), "{err}");
    let run = Command::new(mode.join(exe("hello")))
        .output()
        .expect("runs hello");
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n"),
        "H\n"
    );

    let verbs: Vec<&str> = err
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(
        verbs,
        [
            "Compiling",
            "Loading",
            "Checking",
            "Lowering",
            "Optimizing",
            "Writing",
            "Linking",
            "Finished"
        ],
        "{err}"
    );

    let release = build(
        &dir,
        &["--libc", "--release", "-q", "hello.lilac"],
        Some(&clang),
    );
    assert!(release.status.success(), "{}", stderr(&release));
    assert!(dir
        .join("build")
        .join("release")
        .join(exe("hello"))
        .is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn quiet_and_json_print_no_progress() {
    let Some(clang) = clang() else {
        eprintln!("skipping: no clang");
        return;
    };
    let dir = scratch("quiet");
    std::fs::write(dir.join("hello.lilac"), HELLO).expect("writes source");
    let quiet = build(&dir, &["--libc", "-q", "hello.lilac"], Some(&clang));
    assert!(quiet.status.success(), "{}", stderr(&quiet));
    assert_eq!(stderr(&quiet), "");
    let json = build(
        &dir,
        &["--libc", "--error-format=json", "hello.lilac"],
        Some(&clang),
    );
    assert!(json.status.success(), "{}", stderr(&json));
    assert_eq!(stderr(&json), "");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overrides_place_the_output() {
    let Some(clang) = clang() else {
        eprintln!("skipping: no clang");
        return;
    };
    let dir = scratch("overrides");
    std::fs::write(dir.join("hello.lilac"), HELLO).expect("writes source");
    let out = build(
        &dir,
        &[
            "--libc",
            "-q",
            "--build-dir=out",
            "-o",
            "bin/h",
            "hello.lilac",
        ],
        Some(&clang),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(dir.join("bin").join("h").is_file() || dir.join("bin").join(exe("h")).is_file());
    assert!(dir
        .join("out")
        .join("debug")
        .join("obj")
        .join(obj("hello"))
        .is_file());
    assert!(!dir.join("build").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A folder project builds into `<project>/build`, named after the folder.
#[test]
fn folder_project_builds_into_its_own_folder() {
    let Some(clang) = clang() else {
        eprintln!("skipping: no clang");
        return;
    };
    let dir = scratch("project");
    let project = dir.join("app");
    std::fs::create_dir_all(&project).expect("creates project");
    std::fs::write(project.join("main.lilac"), HELLO).expect("writes source");
    let out = build(&dir, &["--libc", "-q", "app"], Some(&clang));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(project
        .join("build")
        .join("debug")
        .join(exe("app"))
        .is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A cross target nests the mode folder under the triple. The object is
/// written before linking, whether or not the host can link for it.
#[test]
fn cross_target_uses_a_triple_folder() {
    let dir = scratch("cross");
    std::fs::write(dir.join("hello.lilac"), HELLO).expect("writes source");
    let triple = if cfg!(target_os = "linux") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let _ = build(
        &dir,
        &[
            "--libc",
            "-q",
            &format!("--target={triple}"),
            "--linker=does-not-exist",
            "hello.lilac",
        ],
        None,
    );
    assert!(dir
        .join("build")
        .join(triple)
        .join("debug")
        .join("obj")
        .join("hello.o")
        .is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_main_is_e032_with_no_output() {
    let dir = scratch("e032");
    std::fs::write(dir.join("lib.lilac"), "helper: function () nothing {\n}\n")
        .expect("writes source");
    let out = build(&dir, &["--libc", "lib.lilac"], None);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("error[E032]"), "{err}");
    assert!(err.contains("could not compile `lib` (1 error)"), "{err}");
    assert!(!dir.join("build").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn type_error_could_not_compile() {
    let dir = scratch("type-error");
    std::fs::write(
        dir.join("bad.lilac"),
        "main: function () nothing {\n    x: @bit8 = nope\n}\n",
    )
    .expect("writes source");
    let out = build(&dir, &["--libc", "-q", "bad.lilac"], None);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("error[E021]"), "{err}");
    assert!(
        err.contains("error: could not compile `bad` (1 error)"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_linker_is_a_clear_error() {
    let dir = scratch("no-linker");
    std::fs::write(dir.join("hello.lilac"), HELLO).expect("writes source");
    let out = build(
        &dir,
        &["--libc", "-q", "--linker=does-not-exist", "hello.lilac"],
        None,
    );
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("cannot run linker does-not-exist"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Until the native runtime lands, a build without `--libc` says to use it.
#[test]
fn native_mode_points_at_libc() {
    let dir = scratch("native");
    std::fs::write(dir.join("hello.lilac"), HELLO).expect("writes source");
    let out = build(&dir, &["-q", "hello.lilac"], None);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("use --libc"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_usage_exits_2() {
    let dir = scratch("usage");
    for args in [&["-o"][..], &["--target=wasm32-unknown-unknown"][..]] {
        let out = build(&dir, args, None);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
