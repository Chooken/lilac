//! `lilac build`: the build folder, progress output, and failure modes.
//!
//! Every case runs in its own temporary folder. Cases that link use
//! `--libc` with the toolchain `clang` (as `LILAC_LINKER`), or the native
//! development sysroot, and are skipped when it is missing.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use support::clang;

const HELLO: &str = "putchar: extern function (c: @bit32) result: @bit32\n\
                     main: function () nothing {\n    r: @bit32 = putchar('H')\n    r = putchar(0xA)\n}\n";

/// Prints through the OS layer, so it builds natively.
const NATIVE_HELLO: &str = "main: function () nothing {\n    s: @byte_buffer = \"Hi\\n\"\n    \
                            @os_write(0x1, @byte_buffer_ptr(s), @byte_buffer_len(s))\n    @os_exit(0x3)\n}\n";

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

/// A native build links with the sysroot's `lld` and needs no C library.
#[test]
fn native_build_runs_without_libc() {
    if !cfg!(any(windows, target_os = "linux", target_os = "macos")) {
        return;
    }
    let Some(sysroot) = support::sysroot() else {
        eprintln!("skipping: no lld");
        return;
    };
    let dir = scratch("native");
    std::fs::write(dir.join("hi.lilac"), NATIVE_HELLO).expect("writes source");
    let exe = format!("hi{}", std::env::consts::EXE_SUFFIX);
    for mode in ["--mode=debug", "--release"] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lilac"));
        command
            .args(["build", mode, "hi.lilac"])
            .current_dir(&dir)
            .env("NO_COLOR", "1")
            .env("LILAC_SYSROOT", &sysroot);
        let out = command.output().expect("runs lilac");
        let err = stderr(&out);
        assert!(out.status.success(), "{err}");
        assert!(err.contains(&format!("Linking {exe} (lld)")), "{err}");
    }
    for mode in ["debug", "release"] {
        let run = Command::new(dir.join("build").join(mode).join(&exe))
            .output()
            .expect("runs hi");
        assert_eq!(String::from_utf8_lossy(&run.stdout), "Hi\n");
        assert_eq!(run.status.code(), Some(3));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A native build with no sysroot says where it looked.
#[test]
fn missing_sysroot_is_a_clear_error() {
    if !cfg!(any(windows, target_os = "linux", target_os = "macos")) {
        return;
    }
    let dir = scratch("no-sysroot");
    std::fs::write(dir.join("hi.lilac"), NATIVE_HELLO).expect("writes source");
    let mut command = Command::new(env!("CARGO_BIN_EXE_lilac"));
    command
        .args(["build", "-q", "hi.lilac"])
        .current_dir(&dir)
        .env("LILAC_SYSROOT", dir.join("missing"));
    let out = command.output().expect("runs lilac");
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("LILAC_SYSROOT"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Linux executables link from any host with the sysroot's `lld`: static,
/// with no C library and no loader.
#[test]
fn native_linux_cross_links() {
    let Some(sysroot) = support::sysroot() else {
        eprintln!("skipping: no lld");
        return;
    };
    let dir = scratch("native-linux");
    std::fs::write(dir.join("hi.lilac"), NATIVE_HELLO).expect("writes source");
    for triple in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let exe = dir.join(triple);
        let mut command = Command::new(env!("CARGO_BIN_EXE_lilac"));
        command
            .args(["build", "-q", "--release", "-o"])
            .arg(&exe)
            .arg(format!("--target={triple}"))
            .arg("hi.lilac")
            .current_dir(&dir)
            .env("LILAC_SYSROOT", &sysroot);
        let out = command.output().expect("runs lilac");
        assert!(out.status.success(), "{triple}: {}", stderr(&out));
        let bytes = std::fs::read(&exe).expect("reads the executable");
        assert!(bytes.starts_with(b"ELF"), "{triple}: not ELF");
        let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
        assert!(!has(b"ld-linux"), "{triple}: names a loader");
        if cfg!(target_os = "linux") && triple.starts_with(std::env::consts::ARCH) {
            let run = Command::new(&exe).output().expect("runs hi");
            assert_eq!(
                String::from_utf8_lossy(&run.stdout),
                "Hi
"
            );
            assert_eq!(run.status.code(), Some(3));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A native Linux program that links a shared library is dynamically
/// linked: it names the loader, which brings in the library (and, for a
/// real library, the C library it needs). It runs on a Linux host.
#[test]
fn native_linux_links_shared_libraries() {
    let (Some(clang), Some(sysroot)) = (clang(), support::sysroot()) else {
        eprintln!("skipping: no clang or lld");
        return;
    };
    let triple = if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let dir = scratch("native-shared");
    std::fs::write(
        dir.join("greet.c"),
        "int greet(int x) { return x + 1; }
",
    )
    .expect("writes greet.c");
    let library = Command::new(&clang)
        .arg(format!("--target={triple}"))
        .args(["-shared", "-fPIC", "-nostdlib", "-fuse-ld=lld"])
        .args(["greet.c", "-o", "libgreet.so"])
        .current_dir(&dir)
        .output()
        .expect("runs clang");
    if !library.status.success() {
        eprintln!("skipping: clang cannot link a {triple} shared library");
        return;
    }
    std::fs::write(
        dir.join("main.lilac"),
        "greet: extern function (x: @bit32) result: @bit32
         main: function () nothing {
    r: @bit32 = greet(0x2)
    @os_exit(r)
}
",
    )
    .expect("writes main.lilac");
    let out = Command::new(env!("CARGO_BIN_EXE_lilac"))
        .args(["build", "-q", "-o", "greeter", "--link-arg=libgreet.so"])
        .arg(format!("--target={triple}"))
        .arg("main.lilac")
        .current_dir(&dir)
        .env("LILAC_SYSROOT", &sysroot)
        .output()
        .expect("runs lilac");
    assert!(out.status.success(), "{}", stderr(&out));
    let bytes = std::fs::read(dir.join("greeter")).expect("reads the executable");
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    assert!(has(b"ld-linux"), "names the loader");
    assert!(has(b"libgreet.so"), "needs the library");
    if cfg!(target_os = "linux") {
        let run = Command::new(dir.join("greeter"))
            .env("LD_LIBRARY_PATH", &dir)
            .output()
            .expect("runs greeter");
        assert_eq!(run.status.code(), Some(3));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// macOS executables link from any host with the sysroot's `lld` and the
/// `libSystem.tbd` stub: entered through `LC_MAIN`, importing only from
/// `libSystem`, and ad-hoc signed on Apple silicon. They run on a macOS host.
#[test]
fn native_macos_cross_links() {
    let Some(sysroot) = support::sysroot() else {
        eprintln!("skipping: no lld");
        return;
    };
    let dir = scratch("native-macos");
    std::fs::write(dir.join("hi.lilac"), NATIVE_HELLO).expect("writes source");
    for triple in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
        let exe = dir.join(triple);
        let mut command = Command::new(env!("CARGO_BIN_EXE_lilac"));
        command
            .args(["build", "-q", "--release", "-o"])
            .arg(&exe)
            .arg(format!("--target={triple}"))
            .arg("hi.lilac")
            .current_dir(&dir)
            .env("LILAC_SYSROOT", &sysroot);
        let out = command.output().expect("runs lilac");
        assert!(out.status.success(), "{triple}: {}", stderr(&out));
        let bytes = std::fs::read(&exe).expect("reads the executable");
        assert!(
            bytes.starts_with(&[0xcf, 0xfa, 0xed, 0xfe]),
            "{triple}: not Mach-O"
        );
        let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"/usr/lib/libSystem.B.dylib"), "{triple}: no libSystem");
        if triple.starts_with("aarch64") {
            // `LC_CODE_SIGNATURE` (0x1d) as a little-endian load command.
            assert!(has(&[0x1d, 0, 0, 0, 0x10, 0, 0, 0]), "{triple}: unsigned");
        }
        let arch = if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "x86_64"
        };
        if cfg!(target_os = "macos") && triple.starts_with(arch) {
            let run = Command::new(&exe).output().expect("runs hi");
            assert_eq!(
                String::from_utf8_lossy(&run.stdout),
                "Hi
"
            );
            assert_eq!(run.status.code(), Some(3));
        }
    }
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
