//! `lilac.toml`: the CLI reads a folder's manifest, a flag overrides its
//! setting, and a manifest with a mistake stops the command before it
//! compiles.
//!
//! Every case runs in its own temporary folder. Cases that link use the
//! native development sysroot and are skipped without it.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Prints through `Standard.Io` (no C library), so it builds natively.
const HELLO: &str = "using Standard.Io\nusing Standard.Process\n\n\
                     main: function () nothing {\n    print(\"Hi\\n\")\n    exit(0x3)\n}\n";

/// The repository's `lib/`, the bundled libraries' folder for these tests.
fn lib_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib")
}

/// A fresh temporary folder for one case.
fn scratch(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lilac-manifest-{}-{case}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creates scratch dir");
    dir
}

/// Writes `text` to `dir/path`, creating its folder.
fn write(dir: &Path, path: &str, text: &str) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().expect("has a folder")).expect("creates folder");
    std::fs::write(path, text).expect("writes file");
}

/// `lilac <args>` in `dir`, with `lib/` as the bundled libraries' folder.
fn lilac(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lilac"));
    command
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("LILAC_LIB_DIR", lib_dir())
        .env_remove("LILAC_LINKER");
    command
}

fn run(dir: &Path, args: &[&str]) -> Output {
    lilac(dir, args).output().expect("runs lilac")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n")
}

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// The manifest's name, root, mode and build folder place the executable,
/// and each flag overrides its setting.
#[test]
fn the_manifest_places_the_executable() {
    if !cfg!(any(windows, target_os = "linux", target_os = "macos")) {
        return;
    }
    let Some(sysroot) = support::sysroot() else {
        eprintln!("skipping: no lld");
        return;
    };
    let dir = scratch("places");
    write(
        &dir,
        "app/lilac.toml",
        "[project]\nname = \"greeter\"\nroot = \"src\"\n\n[build]\nmode = \"release\"\ndir = \"out\"\n\n\
         [libraries]\nStandard = { bundled = true }\n",
    );
    write(&dir, "app/src/main.lilac", HELLO);
    let build = |args: &[&str]| {
        let mut all = vec!["build", "-q"];
        all.extend(args);
        let out = lilac(&dir, &all)
            .env("LILAC_SYSROOT", &sysroot)
            .output()
            .expect("runs lilac");
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
    };

    build(&["app"]);
    let program = dir.join("app/out/release").join(exe("greeter"));
    let ran = Command::new(&program).output().expect("runs greeter");
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "Hi\n");
    assert_eq!(ran.status.code(), Some(3));
    assert!(dir
        .join("app/out/release/obj")
        .read_dir()
        .is_ok_and(|mut entries| entries.next().is_some()));

    // A flag's folder is the working folder's.
    build(&["app", "--mode=debug", "--build-dir=elsewhere"]);
    assert!(dir.join("elsewhere/debug").join(exe("greeter")).is_file());

    // A cross target nests the mode folder under its triple.
    let triple = if cfg!(target_os = "linux") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    build(&["app", &format!("--target={triple}")]);
    assert!(dir
        .join("app/out")
        .join(triple)
        .join("release")
        .join("greeter")
        .is_file());

    // Run from inside the project, the folder defaults to `.`.
    let project = dir.join("app");
    let out = lilac(&project, &["build", "-q", "--release", "-o", "here"])
        .env("LILAC_SYSROOT", &sysroot)
        .output()
        .expect("runs lilac");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(project.join("here").is_file() || project.join(exe("here")).is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

/// `check` checks what `build` would build: here, for the manifest's
/// target.
#[test]
fn check_reads_the_manifest() {
    let dir = scratch("check");
    write(
        &dir,
        "app/main.lilac",
        "if @builtin.Target.os == .Linux {\n    only_linux: function () nothing {}\n}\n\n\
         main: function () nothing {\n    only_linux()\n}\n",
    );
    let out = run(&dir, &["check", "app"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("only_linux"), "{}", stderr(&out));

    write(
        &dir,
        "app/lilac.toml",
        "[build]\ntarget = \"x86_64-unknown-linux-gnu\"\n",
    );
    let out = run(&dir, &["check", "app"]);
    assert!(out.status.success(), "{}", stderr(&out));
    // A flag overrides it.
    let out = run(&dir, &["check", "app", "--target=x86_64-pc-windows-msvc"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A manifest mistake is reported like a compiler error, at
/// `lilac.toml:<line>:<col>`, and the command stops before compiling.
#[test]
fn a_manifest_mistake_stops_the_command() {
    let dir = scratch("mistake");
    write(
        &dir,
        "app/main.lilac",
        "main: function () nothing {\n    x: @bit8 = nope\n}\n",
    );
    write(&dir, "app/lilac.toml", "[build]\nstack_traces = true\n");
    for command in ["check", "emit-ir", "emit-llvm", "build"] {
        let out = run(&dir, &[command, "app"]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(1), "{command}: {err}");
        assert!(
            err.contains("error[E076]: unknown setting `stack_traces` in `[build]`"),
            "{command}: {err}"
        );
        assert!(err.contains("--> lilac.toml:2:1"), "{command}: {err}");
        assert!(
            err.contains("help: did you mean `stack-traces`?"),
            "{command}: {err}"
        );
        // Nothing compiled: the type error in `main.lilac` is not reported.
        assert!(!err.contains("E021"), "{command}: {err}");
        assert!(
            err.contains("aborting due to 1 previous error"),
            "{command}: {err}"
        );
    }
    assert!(!dir.join("app/build").exists());

    let out = run(&dir, &["check", "app", "--error-format=json"]);
    let err = stderr(&out);
    assert!(err.contains("\"code\":\"E076\""), "{err}");
    assert!(err.contains("\"replacement\":\"stack-traces\""), "{err}");

    write(&dir, "app/lilac.toml", "[build]\nmode = \"release\n");
    let out = run(&dir, &["check", "app", "--error-format=json"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("\"code\":\"E075\""), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file argument is single-file mode: it reads no manifest, even one
/// beside it.
#[test]
fn a_file_argument_ignores_the_manifest() {
    let dir = scratch("file");
    write(&dir, "hello.lilac", "main: function () nothing {\n}\n");
    write(&dir, "lilac.toml", "not toml at all\n");
    let out = run(&dir, &["check", "hello.lilac"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = run(&dir, &["check"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("E075"), "{}", stderr(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A folder below the root with its own `lilac.toml` is another project,
/// and is not loaded into this one.
#[test]
fn a_nested_project_is_not_loaded() {
    let dir = scratch("nested");
    write(&dir, "app/main.lilac", "main: function () nothing {\n}\n");
    write(
        &dir,
        "app/Inner/inner.lilac",
        "broken: function () nothing {\n    x: @bit8 = nope\n}\n",
    );
    let out = run(&dir, &["check", "app"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    write(&dir, "app/Inner/lilac.toml", "");
    let out = run(&dir, &["check", "app"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The manifest's build folder is skipped by the load, and a folder named
/// `build` is then an ordinary namespace.
#[test]
fn the_build_folder_is_the_manifests() {
    let dir = scratch("build-dir");
    write(
        &dir,
        "app/main.lilac",
        "using build\n\nmain: function () nothing {\n    helper()\n}\n",
    );
    write(
        &dir,
        "app/build/helper.lilac",
        "helper: function () nothing {\n}\n",
    );
    write(
        &dir,
        "app/out/stray.lilac",
        "broken: function () nothing {\n    x: @bit8 = nope\n}\n",
    );
    write(&dir, "app/lilac.toml", "[build]\ndir = \"out\"\n");
    let out = run(&dir, &["check", "app"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A relative link argument is relative to the manifest, wherever
/// `lilac build` runs.
#[test]
fn link_arguments_are_relative_to_the_manifest() {
    let (Some(clang), Some(sysroot)) = (support::clang(), support::sysroot()) else {
        eprintln!("skipping: no clang or lld");
        return;
    };
    let triple = if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let dir = scratch("link-args");
    let project = dir.join("app");
    write(&project, "greet.c", "int greet(int x) { return x + 1; }\n");
    let library = Command::new(&clang)
        .arg(format!("--target={triple}"))
        .args(["-shared", "-fPIC", "-nostdlib", "-fuse-ld=lld"])
        .args(["greet.c", "-o", "libgreet.so"])
        .current_dir(&project)
        .output()
        .expect("runs clang");
    if !library.status.success() {
        eprintln!("skipping: clang cannot link a {triple} shared library");
        return;
    }
    write(
        &project,
        "main.lilac",
        "using Standard.Process\ngreet: extern function (x: @bit32) result: @bit32\n\
         main: function () nothing {\n    r: @bit32 = greet(0x2)\n    exit(r)\n}\n",
    );
    write(
        &project,
        "lilac.toml",
        &format!(
            "[build]\ntarget = \"{triple}\"\n\n[libraries]\nStandard = {{ bundled = true }}\n\n\
             [link]\nargs = [\"libgreet.so\"]\n"
        ),
    );
    let out = lilac(&dir, &["build", "-q", "app", "-o", "greeter"])
        .env("LILAC_SYSROOT", &sysroot)
        .output()
        .expect("runs lilac");
    assert!(out.status.success(), "{}", stderr(&out));
    let bytes = std::fs::read(dir.join("greeter")).expect("reads the executable");
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    assert!(has(b"libgreet.so"), "needs the library");
    if cfg!(target_os = "linux") {
        let ran = Command::new(dir.join("greeter"))
            .env("LD_LIBRARY_PATH", &project)
            .output()
            .expect("runs greeter");
        assert_eq!(ran.status.code(), Some(3));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
