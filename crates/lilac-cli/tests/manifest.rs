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

/// The repository's `templates/`, the project templates' folder.
fn template_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates")
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

/// `lilac <args>` in `dir`, with the repository's bundled libraries and
/// templates.
fn lilac(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lilac"));
    command
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("LILAC_LIB_DIR", lib_dir())
        .env("LILAC_TEMPLATE_DIR", template_dir())
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

/// `project.usings` are every project file's `using`s; one naming no
/// folder is `E025` at its place in `lilac.toml`.
#[test]
fn global_usings_reach_every_project_file() {
    let dir = scratch("usings");
    write(
        &dir,
        "app/Util/util.lilac",
        "helper: function () nothing {\n}\n",
    );
    write(
        &dir,
        "app/Util/Deep/deep.lilac",
        "inner: function () nothing {\n}\n",
    );
    write(
        &dir,
        "app/main.lilac",
        "main: function () nothing {\n    helper()\n    D.inner()\n}\n",
    );
    let out = run(&dir, &["check", "app"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));

    write(
        &dir,
        "app/lilac.toml",
        "[project]\nusings = [\"Util\", \"Util.Deep as D\"]\n",
    );
    let out = run(&dir, &["check", "app"]);
    assert!(out.status.success(), "{}", stderr(&out));

    write(
        &dir,
        "app/lilac.toml",
        "[project]\nusings = [\"Util\", \"Missing\"]\n",
    );
    let out = run(&dir, &["check", "app"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("error[E025]"), "{err}");
    assert!(err.contains("--> lilac.toml:2:19"), "{err}");
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

/// `lilac init` copies the project template, naming the folder and
/// mounting the bundled libraries, and never changes a `lilac.toml` (or
/// any other file) that is there.
#[test]
fn init_copies_the_template_once() {
    let dir = scratch("init");
    let bundled = dir.join("install");
    for folder in ["Standard", "lilac", "not a library"] {
        std::fs::create_dir_all(bundled.join(folder)).expect("creates folder");
    }
    let init = |args: &[&str]| {
        let mut all = vec!["init"];
        all.extend(args);
        lilac(&dir, &all)
            .env("LILAC_LIB_DIR", &bundled)
            .output()
            .expect("runs lilac")
    };

    let out = init(&["my app"]);
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("Created `my-app` in my app (lilac.toml, src"),
        "{err}"
    );
    let path = dir.join("my app/lilac.toml");
    let text = std::fs::read_to_string(&path)
        .expect("reads the manifest")
        .replace("\r\n", "\n");
    assert!(text.contains("\nname = \"my-app\"\n"), "{text}");
    assert!(text.contains("\nroot = \"src\"\n"), "{text}");
    assert!(text.contains("\nStandard = { bundled = true }\n"), "{text}");
    assert!(!text.contains("lilac = "), "{text}");
    assert!(!text.contains("not a library"), "{text}");
    assert!(!text.contains("{{"), "{text}");
    let main = std::fs::read_to_string(dir.join("my app/src/main.lilac")).expect("reads main");
    assert!(main.contains("Hello, world!"), "{main}");

    std::fs::write(&path, "# mine\n").expect("writes the manifest");
    std::fs::remove_file(dir.join("my app/src/main.lilac")).expect("removes main");
    let out = init(&["my app", "-q"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("already exists"), "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(&path).expect("reads"), "# mine\n");
    assert!(!dir.join("my app/src/main.lilac").exists());

    // With nothing bundled, `[libraries]` holds an example and `init`
    // says where it looked; a `lilac.json` gets a note, and code already
    // there is kept.
    std::fs::remove_dir_all(&bundled).expect("removes the libraries");
    write(&dir, "old/lilac.json", "{}");
    write(&dir, "old/src/main.lilac", "// mine\n");
    let out = init(&["old"]);
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("found no bundled library"), "{err}");
    assert!(err.contains("lilac.json"), "{err}");
    assert!(err.contains("note: kept the existing"), "{err}");
    let text = std::fs::read_to_string(dir.join("old/lilac.toml"))
        .expect("reads")
        .replace("\r\n", "\n");
    assert!(
        text.contains("\n# Standard = { bundled = true }\n"),
        "{text}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("old/src/main.lilac")).expect("reads"),
        "// mine\n"
    );

    // `-q` prints nothing; a stray flag or a second folder is a usage error.
    let out = init(&["quiet", "-q"]);
    assert!(out.status.success());
    assert_eq!(stderr(&out), "");
    assert_eq!(init(&["--force"]).status.code(), Some(2));
    assert_eq!(init(&["a", "b"]).status.code(), Some(2));

    // Without the template, `init` says where to point it.
    let out = lilac(&dir, &["init", "none"])
        .env("LILAC_TEMPLATE_DIR", dir.join("missing"))
        .output()
        .expect("runs lilac");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("project template"),
        "{}",
        stderr(&out)
    );
    assert!(!dir.join("none").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// What `lilac init` makes builds and runs as it is.
#[test]
fn init_then_build() {
    if !cfg!(any(windows, target_os = "linux", target_os = "macos")) {
        return;
    }
    let Some(sysroot) = support::sysroot() else {
        eprintln!("skipping: no lld");
        return;
    };
    let dir = scratch("init-build");
    let project = dir.join("hello");
    let out = run(&dir, &["init", "hello", "-q"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = lilac(&project, &["build", "-q"])
        .env("LILAC_SYSROOT", &sysroot)
        .output()
        .expect("runs lilac");
    assert!(out.status.success(), "{}", stderr(&out));
    let ran = Command::new(project.join("build/debug").join(exe("hello")))
        .output()
        .expect("runs hello");
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n"),
        "Hello, world!\n"
    );
    assert!(ran.status.success());
    let _ = std::fs::remove_dir_all(&dir);
}
