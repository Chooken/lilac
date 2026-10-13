//! The settings: each key's values, each `E076` with its message, span and
//! fix, the defaults, and the flags over the manifest.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use lilac_driver::{Entry, OptimizeMode, Session};
use lilac_error::{Diags, Emitter as _, EmitterHuman};
use lilac_module::Fs;
use lilac_span::SourceMap;
use lilac_target::Target;

use crate::settings::{
    bundled_dir, bundled_libraries, default_name, is_program_name, Manifest, Overrides, Settings,
    TargetChoice, SECTIONS, SETTINGS,
};

/// An in-memory file system: files, and every folder above them.
#[derive(Default)]
struct MemFs {
    files: BTreeMap<PathBuf, String>,
    dirs: BTreeSet<PathBuf>,
}

impl MemFs {
    fn with(paths: &[&str]) -> Self {
        let mut fs = Self::default();
        for path in paths {
            fs.add(path, "");
        }
        fs
    }

    /// Adds a file, or a folder when `path` ends in `/`.
    fn add(&mut self, path: &str, text: &str) {
        let full = PathBuf::from(path.trim_end_matches('/'));
        if path.ends_with('/') {
            self.dirs.insert(full.clone());
        } else {
            self.files.insert(full.clone(), text.to_owned());
        }
        let mut dir = full.parent();
        while let Some(parent) = dir.filter(|dir| !dir.as_os_str().is_empty()) {
            self.dirs.insert(parent.to_path_buf());
            dir = parent.parent();
        }
    }
}

impl Fs for MemFs {
    fn read(&mut self, path: &Path) -> io::Result<String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))
    }

    fn list_dir(&mut self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        if !self.dirs.contains(dir) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such folder"));
        }
        Ok(self
            .files
            .keys()
            .chain(&self.dirs)
            .filter(|path| path.parent() == Some(dir))
            .cloned()
            .collect())
    }

    fn canonicalize(&mut self, path: &Path) -> io::Result<PathBuf> {
        Ok(path.to_path_buf())
    }
}

/// The folder every test's manifest is in.
const DIR: &str = "proj";

/// What reading `text` as `proj/lilac.toml` gives, with `folders` (paths
/// under `proj/`) existing and bundled libraries in `bundled/`.
struct Read {
    manifest: Option<Manifest>,
    errors: Vec<Error>,
}

/// A diagnostic: its code, message, the text at its primary label, and its
/// fixes as (text replaced, replacement).
type Error = (String, String, String, Vec<(String, String)>);

fn read_in(text: &str, folders: &[&str]) -> Read {
    let mut fs = MemFs::default();
    fs.add("proj/lilac.toml", text);
    for folder in folders {
        fs.add(&format!("{folder}/"), "");
    }
    fs.add("bundled/Standard/", "");
    let mut sources = SourceMap::new();
    let mut diags = Diags::new();
    let manifest = Manifest::read(
        Path::new(DIR),
        Some(Path::new("bundled")),
        &mut fs,
        &mut sources,
        &mut diags,
    )
    .expect("there is a manifest");
    let slice = |span: lilac_span::Span| text[span.lo.0 as usize..span.hi.0 as usize].to_owned();
    let errors = diags
        .diagnostics()
        .iter()
        .map(|diagnostic| {
            let primary = diagnostic
                .labels
                .iter()
                .find(|label| label.primary)
                .map_or_else(String::new, |label| slice(label.span));
            let fixes = diagnostic
                .suggestions()
                .map(|fix| (slice(fix.span), fix.replacement.clone()))
                .collect();
            (
                diagnostic.code.to_owned(),
                diagnostic.msg.to_string(),
                primary,
                fixes,
            )
        })
        .collect();
    Read {
        manifest: manifest.ok(),
        errors,
    }
}

fn read(text: &str) -> Read {
    read_in(text, &[])
}

/// A manifest with no error.
fn manifest(text: &str, folders: &[&str]) -> Manifest {
    let read = read_in(text, folders);
    assert!(read.errors.is_empty(), "{:?}", read.errors);
    read.manifest.expect("a manifest")
}

/// The one `E076` reading `text` gives: its message and the text at it.
fn error(text: &str) -> (String, String) {
    let read = read(text);
    assert_eq!(read.errors.len(), 1, "{:?}", read.errors);
    let (code, message, at, _) = read.errors.into_iter().next().expect("one error");
    assert_eq!(code, "E076");
    (message, at)
}

/// The fix of the one error reading `text` gives.
fn fix(text: &str) -> (String, String) {
    let read = read(text);
    assert_eq!(read.errors.len(), 1, "{:?}", read.errors);
    let mut fixes = read.errors.into_iter().next().expect("one error").3;
    assert_eq!(fixes.len(), 1);
    fixes.pop().expect("one fix")
}

fn pair(message: &str, at: &str) -> (String, String) {
    (message.to_owned(), at.to_owned())
}

fn host() -> Target {
    Target::parse("x86_64-pc-windows-msvc").expect("a target")
}

#[test]
fn no_manifest_is_none() {
    let mut fs = MemFs::with(&["proj/main.lilac"]);
    let mut diags = Diags::new();
    let read = Manifest::read(
        Path::new(DIR),
        None,
        &mut fs,
        &mut SourceMap::new(),
        &mut diags,
    );
    assert!(read.is_none());
    assert!(diags.diagnostics().is_empty());
    // Nor is there one in a folder that does not exist (the loader says so).
    let read = Manifest::read(
        Path::new("nowhere"),
        None,
        &mut fs,
        &mut SourceMap::new(),
        &mut diags,
    );
    assert!(read.is_none());
}

#[test]
fn a_toml_mistake_is_e075_and_no_manifest() {
    let read = read("[build]\nmode = release\n");
    assert!(read.manifest.is_none());
    assert_eq!(read.errors.len(), 1);
    assert_eq!(read.errors[0].0, "E075");
}

#[test]
fn every_key_reads() {
    let text = "\
[project]
name = \"hello\"
root = \"src\"

[build]
mode = \"release\"
target = \"x86_64-unknown-linux-gnu\"
stack-traces = true
dir = \"out\"

[libraries]
Standard = { bundled = true }
Util = \"vendor/Util\"
Json = { path = \"json\" }

[link]
libc = true
linker = \"clang\"
args = [\"-lm\", \"vendor/libgreet.a\"]
";
    let manifest = manifest(text, &["proj/src", "proj/vendor/Util", "proj/json"]);
    assert_eq!(manifest.name.expect("name").value, "hello");
    assert_eq!(manifest.root.expect("root").value, PathBuf::from("src"));
    assert_eq!(manifest.mode.expect("mode").value, OptimizeMode::Release);
    assert_eq!(
        manifest.target.expect("target").value,
        TargetChoice::Triple(Target::parse("x86_64-unknown-linux-gnu").expect("a target"))
    );
    assert!(manifest.stack_traces.expect("stack-traces").value);
    assert_eq!(manifest.build_dir.expect("dir").value, PathBuf::from("out"));
    let libraries: Vec<(&str, PathBuf, bool)> = manifest
        .libraries
        .iter()
        .map(|library| (library.name.as_str(), library.dir.clone(), library.bundled))
        .collect();
    assert_eq!(
        libraries,
        [
            ("Standard", Path::new("bundled").join("Standard"), true),
            ("Util", PathBuf::from("vendor/Util"), false),
            ("Json", PathBuf::from("json"), false),
        ]
    );
    assert!(manifest.libc.expect("libc").value);
    assert_eq!(
        manifest.linker.expect("linker").value,
        PathBuf::from("clang")
    );
    assert_eq!(
        manifest.link_args.expect("args").value,
        ["-lm", "vendor/libgreet.a"]
    );
}

#[test]
fn each_mode_and_host_read() {
    for (name, mode) in [
        ("debug", OptimizeMode::Debug),
        ("release", OptimizeMode::Release),
        ("release-small", OptimizeMode::ReleaseSmall),
    ] {
        let text = format!("[build]\nmode = \"{name}\"\ntarget = \"host\"\n");
        let manifest = manifest(&text, &[]);
        assert_eq!(manifest.mode.expect("mode").value, mode);
        assert_eq!(manifest.target.expect("target").value, TargetChoice::Host);
    }
}

#[test]
fn a_header_library_reads_like_an_inline_one() {
    let manifest = manifest("[libraries.Util]\npath = \"util\"\n", &["proj/util"]);
    assert_eq!(manifest.libraries[0].name, "Util");
    assert_eq!(manifest.libraries[0].dir, PathBuf::from("util"));
}

#[test]
fn spans_are_kept() {
    let text = "[build]\nmode = \"release\"\n[libraries]\nUtil = \"util\"\n";
    let manifest = manifest(text, &["proj/util"]);
    let slice = |span: lilac_span::Span| &text[span.lo.0 as usize..span.hi.0 as usize];
    assert_eq!(slice(manifest.mode.expect("mode").span), "\"release\"");
    assert_eq!(slice(manifest.libraries[0].name_span), "Util");
    assert_eq!(slice(manifest.libraries[0].span), "\"util\"");
}

#[test]
fn unknown_sections_and_settings() {
    assert_eq!(
        error("[biuld]\nmode = \"debug\"\n"),
        pair("unknown section `biuld`", "biuld")
    );
    assert_eq!(fix("[biuld]\n"), pair("biuld", "build"));
    assert_eq!(
        error("[package]\n"),
        pair("unknown section `package`", "package")
    );
    assert_eq!(
        error("mode = \"debug\"\n"),
        pair("setting `mode` is outside its section", "mode")
    );
    assert_eq!(
        error("[build]\nstack_traces = true\n"),
        pair(
            "unknown setting `stack_traces` in `[build]`",
            "stack_traces"
        )
    );
    assert_eq!(
        fix("[build]\nstack_traces = true\n"),
        pair("stack_traces", "stack-traces")
    );
    // A setting of another section says where it goes.
    assert_eq!(
        error("[build]\nlibc = true\n"),
        pair("unknown setting `libc` in `[build]`", "libc")
    );
    // A dotted key names its section too.
    assert_eq!(
        error("build.mdoe = \"debug\"\n"),
        pair("unknown setting `mdoe` in `[build]`", "mdoe")
    );
    assert_eq!(fix("build.mdoe = \"debug\"\n"), pair("mdoe", "mode"));
}

#[test]
fn every_mistake_is_reported_and_the_rest_apply() {
    let read = read("[build]\nmode = true\ntarget = \"host\"\n[link]\nlibc = \"yes\"\n");
    let messages: Vec<&str> = read.errors.iter().map(|error| error.1.as_str()).collect();
    assert_eq!(
        messages,
        [
            "`build.mode` takes a string",
            "`link.libc` takes `true` or `false`"
        ]
    );
    let manifest = read.manifest.expect("a manifest");
    assert!(manifest.mode.is_none());
    assert_eq!(manifest.target.expect("target").value, TargetChoice::Host);
}

#[test]
fn wrong_types() {
    assert_eq!(
        error("[build]\nmode = true\n"),
        pair("`build.mode` takes a string", "true")
    );
    assert_eq!(
        error("[build]\nstack-traces = \"on\"\n"),
        pair("`build.stack-traces` takes `true` or `false`", "\"on\"")
    );
    assert_eq!(
        error("[link]\nargs = \"-lm\"\n"),
        pair("`link.args` takes an array of strings", "\"-lm\"")
    );
    assert_eq!(
        error("[link]\nargs = [\"-lm\", true]\n"),
        pair("`link.args` takes an array of strings", "true")
    );
    assert_eq!(
        error("build = \"release\"\n"),
        pair("`build` takes a table of settings", "\"release\"")
    );
    assert_eq!(
        error("[libraries]\nUtil = true\n"),
        pair("`libraries.Util` takes a folder or a table", "true")
    );
    assert_eq!(
        error("[project]\nroot = \"\"\n"),
        pair("`project.root` takes a folder, not \"\"", "\"\"")
    );
    assert_eq!(
        error("[link]\nlinker = ''\n"),
        pair("`link.linker` takes a program, not \"\"", "''")
    );
}

#[test]
fn modes_and_targets() {
    assert_eq!(
        error("[build]\nmode = \"relase\"\n"),
        pair("unknown mode `relase`", "\"relase\"")
    );
    // The fix replaces the text inside the quotes.
    assert_eq!(
        fix("[build]\nmode = \"relase\"\n"),
        pair("relase", "release")
    );
    assert_eq!(
        fix("[build]\nmode = 'release_small'\n"),
        pair("release_small", "release-small")
    );
    // Written with an escape, there is no span to fix.
    assert!(read("[build]\nmode = \"rel\\u0061se \"\n").errors[0]
        .3
        .is_empty());
    let (message, at) = error("[build]\ntarget = \"x86_64-nope\"\n");
    assert!(
        message.starts_with("invalid target `x86_64-nope`: "),
        "{message}"
    );
    assert_eq!(at, "\"x86_64-nope\"");
    let (message, _) = error("[build]\ntarget = \"wasm32-pc-windows-msvc\"\n");
    assert!(message.starts_with("invalid target `wasm32-pc-windows-msvc`: "));
}

#[test]
fn project_names() {
    for good in ["hello", "my-app", "app_2", "v1.0", "_x", "9lives"] {
        assert!(is_program_name(good), "{good}");
        manifest(&format!("[project]\nname = \"{good}\"\n"), &[]);
    }
    for bad in ["", "my app", "-x", ".hidden", "a/b", "caf\u{e9}"] {
        assert!(!is_program_name(bad), "{bad}");
        assert_eq!(
            error(&format!("[project]\nname = \"{bad}\"\n")).0,
            format!("invalid project name `{bad}`")
        );
    }
    assert_eq!(default_name("hello"), "hello");
    assert_eq!(default_name("my app"), "my-app");
    assert_eq!(default_name(".dots"), "_dots");
    assert_eq!(default_name("caf\u{e9}"), "caf-");
    assert_eq!(default_name(""), "main");
    for folder in ["hello", "my app", ".dots", "-x", "caf\u{e9}", ""] {
        assert!(is_program_name(&default_name(folder)), "{folder}");
    }
}

#[test]
fn library_names() {
    assert_eq!(
        error("[libraries]\nmy-json = \"json\"\n"),
        pair("library name `my-json` is not an identifier", "my-json")
    );
    assert_eq!(
        error("[libraries]\n\"2d\" = \"json\"\n"),
        pair("library name `2d` is not an identifier", "\"2d\"")
    );
    assert_eq!(
        error("[libraries]\n\"@builtin\" = \"json\"\n"),
        pair(
            "library name `@builtin` is reserved for the compiler",
            "\"@builtin\""
        )
    );
}

#[test]
fn library_tables() {
    assert_eq!(
        error("[libraries]\nUtil = { path = \"util\", bundled = true }\n"),
        pair(
            "library `Util` has both `path` and `bundled`",
            "{ path = \"util\", bundled = true }"
        )
    );
    assert_eq!(
        error("[libraries]\nUtil = {}\n"),
        pair("library `Util` needs `path` or `bundled = true`", "{}")
    );
    assert_eq!(
        error("[libraries]\nUtil = { bundled = false }\n"),
        pair("`bundled = false` mounts nothing", "false")
    );
    assert_eq!(
        error("[libraries]\nUtil = { pth = \"util\" }\n"),
        pair("unknown key `pth` in library `Util`", "pth")
    );
    assert_eq!(
        fix("[libraries]\nUtil = { pth = \"util\" }\n"),
        pair("pth", "path")
    );
    assert_eq!(
        error("[libraries]\nUtil = { path = true }\n"),
        pair("`libraries.Util.path` takes a string", "true")
    );
}

#[test]
fn folders_must_exist() {
    assert_eq!(
        error("[project]\nroot = \"src\"\n"),
        pair("cannot find folder `src`", "\"src\"")
    );
    assert_eq!(
        error("[libraries]\nUtil = \"vendor/Util\"\n"),
        pair(
            "cannot find library folder `vendor/Util`",
            "\"vendor/Util\""
        )
    );
    assert_eq!(
        error("[libraries]\nJson = { bundled = true }\n"),
        pair("no bundled library `Json`", "{ bundled = true }")
    );
    // The build folder need not exist yet.
    manifest("[build]\ndir = \"out\"\n", &[]);
}

#[test]
fn bundled_libraries_need_the_folder() {
    let mut fs = MemFs::default();
    fs.add(
        "proj/lilac.toml",
        "[libraries]\nStandard = { bundled = true }\n",
    );
    let mut diags = Diags::new();
    let read = Manifest::read(
        Path::new(DIR),
        None,
        &mut fs,
        &mut SourceMap::new(),
        &mut diags,
    )
    .expect("a manifest")
    .expect("read");
    assert!(read.libraries.is_empty());
    assert_eq!(
        &*diags.diagnostics()[0].msg,
        "cannot find Lilac's bundled libraries"
    );
}

#[test]
fn finding_the_bundled_folder() {
    let exe = Path::new("install").join("bin");
    assert_eq!(
        bundled_dir(Some(PathBuf::from("libs")), Some(&exe)),
        Some(PathBuf::from("libs"))
    );
    assert_eq!(
        bundled_dir(None, Some(&exe)),
        Some(exe.join("..").join("lib"))
    );
    // An empty variable is unset.
    assert_eq!(
        bundled_dir(Some(PathBuf::new()), Some(&exe)),
        Some(exe.join("..").join("lib"))
    );
    assert_eq!(bundled_dir(None, None), None);

    let mut fs = MemFs::default();
    for path in [
        "lib/Standard/",
        "lib/Json/",
        "lib/lilac/",
        "lib/not-a-name/",
        "lib/README.md",
    ] {
        fs.add(path, "");
    }
    assert_eq!(
        bundled_libraries(Path::new("lib"), &mut fs),
        ["Json", "Standard"]
    );
    assert!(bundled_libraries(Path::new("missing"), &mut fs).is_empty());
}

#[test]
fn an_empty_manifest_is_the_defaults() {
    let manifest = manifest("", &[]);
    assert_eq!(manifest, Manifest::empty(Path::new(DIR)));
    let settings = Settings::resolve(Some(&manifest), &Overrides::default(), host());
    let dir = PathBuf::from(DIR);
    assert_eq!(
        settings,
        Settings {
            manifest_dir: Some(dir.clone()),
            name: Some("proj".to_owned()),
            root: Some(dir.clone()),
            mode: OptimizeMode::Debug,
            target: Some(host()),
            stack_traces: None,
            build_dir: Some(dir.join("build")),
            libraries: Vec::new(),
            libc: Some(false),
            linker: None,
            link_args: Vec::new(),
        }
    );
}

#[test]
fn no_manifest_keeps_the_commands_defaults() {
    let settings = Settings::resolve(None, &Overrides::default(), host());
    assert_eq!(settings, Settings::default());
    let mut session = Session::new();
    let before = (session.target, session.entry, session.optimize);
    settings.apply(&mut session);
    assert_eq!((session.target, session.entry, session.optimize), before);
    assert!(session.build_dir.is_none());
}

/// The manifest every override test starts from.
const FULL: &str = "\
[project]
name = \"hello\"
root = \"src\"
[build]
mode = \"release\"
target = \"x86_64-unknown-linux-gnu\"
stack-traces = false
dir = \"out\"
[libraries]
Util = \"util\"
Json = \"json\"
[link]
libc = true
linker = \"tools/cc\"
args = [\"-lm\"]
";

fn full() -> Manifest {
    manifest(FULL, &["proj/src", "proj/util", "proj/json"])
}

#[test]
fn the_manifest_over_the_defaults() {
    let settings = Settings::resolve(Some(&full()), &Overrides::default(), host());
    let dir = PathBuf::from(DIR);
    assert_eq!(settings.name.as_deref(), Some("hello"));
    assert_eq!(settings.root, Some(dir.join("src")));
    assert_eq!(settings.mode, OptimizeMode::Release);
    assert_eq!(
        settings.target,
        Some(Target::parse("x86_64-unknown-linux-gnu").expect("a target"))
    );
    assert_eq!(settings.stack_traces, Some(false));
    assert_eq!(settings.build_dir, Some(dir.join("out")));
    assert_eq!(
        settings.libraries,
        [
            ("Util".to_owned(), dir.join("util")),
            ("Json".to_owned(), dir.join("json")),
        ]
    );
    assert_eq!(settings.libc, Some(true));
    assert_eq!(settings.linker, Some(dir.join("tools/cc")));
    assert_eq!(settings.link_args, ["-lm"]);

    let mut session = Session::new();
    settings.apply(&mut session);
    assert_eq!(session.libraries, settings.libraries);
    assert_eq!(session.target, settings.target.expect("a target"));
    assert_eq!(session.optimize, OptimizeMode::Release);
    assert_eq!(session.entry, Entry::CMain);
    assert_eq!(session.build_dir, Some(dir.join("out")));
}

#[test]
fn every_flag_overrides_its_key() {
    let overrides = Overrides {
        mode: Some(OptimizeMode::ReleaseSmall),
        target: Some(TargetChoice::Host),
        stack_traces: Some(true),
        build_dir: Some(PathBuf::from("elsewhere")),
        libraries: vec![
            ("Json".to_owned(), PathBuf::from("other/json")),
            ("Extra".to_owned(), PathBuf::from("extra")),
        ],
        libc: Some(false),
        linker: Some(PathBuf::from("gcc")),
        link_args: vec!["-lz".to_owned(), "-lm".to_owned()],
        env_linker: None,
    };
    let settings = Settings::resolve(Some(&full()), &overrides, host());
    let dir = PathBuf::from(DIR);
    assert_eq!(settings.mode, OptimizeMode::ReleaseSmall);
    assert_eq!(settings.target, Some(host()));
    assert_eq!(settings.stack_traces, Some(true));
    // A flag's path is the working folder's, not the manifest's.
    assert_eq!(settings.build_dir, Some(PathBuf::from("elsewhere")));
    // `--lib` replaces a name in place and adds the others.
    assert_eq!(
        settings.libraries,
        [
            ("Util".to_owned(), dir.join("util")),
            ("Json".to_owned(), PathBuf::from("other/json")),
            ("Extra".to_owned(), PathBuf::from("extra")),
        ]
    );
    assert_eq!(settings.libc, Some(false));
    assert_eq!(settings.linker, Some(PathBuf::from("gcc")));
    // `--link-arg`s come after `link.args`, in order.
    assert_eq!(settings.link_args, ["-lm", "-lz", "-lm"]);

    let mut session = Session::new();
    settings.apply(&mut session);
    assert_eq!(session.entry, Entry::Native);
    assert_eq!(session.target, host());

    let triple = Target::parse("aarch64-apple-darwin").expect("a target");
    let overrides = Overrides {
        target: Some(TargetChoice::Triple(triple)),
        ..Overrides::default()
    };
    let settings = Settings::resolve(Some(&full()), &overrides, host());
    assert_eq!(settings.target, Some(triple));
}

#[test]
fn flags_without_a_manifest() {
    let overrides = Overrides {
        mode: Some(OptimizeMode::Release),
        libraries: vec![("Util".to_owned(), PathBuf::from("util"))],
        libc: Some(true),
        link_args: vec!["-lm".to_owned()],
        build_dir: Some(PathBuf::from("out")),
        ..Overrides::default()
    };
    let settings = Settings::resolve(None, &overrides, host());
    assert_eq!(settings.mode, OptimizeMode::Release);
    assert_eq!(settings.target, None);
    assert_eq!(settings.name, None);
    assert_eq!(settings.build_dir, Some(PathBuf::from("out")));
    // Without a manifest the loader still skips `<root>/build`.
    let mut session = Session::new();
    settings.apply(&mut session);
    assert_eq!(session.build_dir, None);
    assert_eq!(
        settings.libraries,
        [("Util".to_owned(), PathBuf::from("util"))]
    );
    assert_eq!(settings.libc, Some(true));
    assert_eq!(settings.link_args, ["-lm"]);
}

#[test]
fn the_linker_order() {
    let resolve = |manifest: Option<&Manifest>, flag: Option<&str>, env: Option<&str>| {
        let overrides = Overrides {
            linker: flag.map(PathBuf::from),
            env_linker: env.map(PathBuf::from),
            ..Overrides::default()
        };
        Settings::resolve(manifest, &overrides, host()).linker
    };
    let full = full();
    let dir = PathBuf::from(DIR);
    assert_eq!(
        resolve(Some(&full), Some("gcc"), Some("env-cc")),
        Some(PathBuf::from("gcc"))
    );
    assert_eq!(
        resolve(Some(&full), None, Some("env-cc")),
        Some(PathBuf::from("env-cc"))
    );
    assert_eq!(resolve(Some(&full), None, None), Some(dir.join("tools/cc")));
    assert_eq!(
        resolve(None, None, Some("env-cc")),
        Some(PathBuf::from("env-cc"))
    );
    assert_eq!(resolve(None, None, None), None);
    // A bare program name is looked up on `PATH`, so it stays bare.
    let bare = manifest("[link]\nlinker = \"clang\"\n", &[]);
    assert_eq!(
        resolve(Some(&bare), None, None),
        Some(PathBuf::from("clang"))
    );
}

#[test]
fn relative_and_absolute_paths() {
    let absolute = if cfg!(windows) {
        "C:\\libs\\Util"
    } else {
        "/libs/Util"
    };
    let text = format!("[libraries]\nUtil = '{absolute}'\nNear = \"../near\"\n");
    let manifest = manifest(&text, &[absolute, "proj/../near"]);
    let settings = Settings::resolve(Some(&manifest), &Overrides::default(), host());
    assert_eq!(
        settings.libraries,
        [
            ("Util".to_owned(), PathBuf::from(absolute)),
            ("Near".to_owned(), Path::new(DIR).join("../near")),
        ]
    );
}

#[test]
fn the_table_is_whole() {
    for setting in SETTINGS {
        assert!(SECTIONS.contains(&setting.section), "{}", setting.key);
        assert!(!setting.about.is_empty());
    }
    for section in SECTIONS {
        assert!(SETTINGS.iter().any(|setting| setting.section == *section));
    }
    // Each default reads back as the setting's value.
    let mut text = String::new();
    for section in SECTIONS {
        let _ = writeln!(text, "[{section}]");
        for setting in SETTINGS
            .iter()
            .filter(|setting| setting.section == *section)
        {
            if let Some(default) = setting.default {
                let _ = writeln!(text, "{} = {default}", setting.key);
            }
        }
    }
    let read = manifest(&text, &[]);
    let settings = Settings::resolve(Some(&read), &Overrides::default(), host());
    let empty = Settings::resolve(
        Some(&Manifest::empty(Path::new(DIR))),
        &Overrides::default(),
        host(),
    );
    assert_eq!(settings, empty);
}

#[test]
fn error_page_example() {
    let text = "[build]\nmode = \"release\"\nstack_traces = true\n";
    let mut fs = MemFs::default();
    fs.add("proj/lilac.toml", text);
    let mut sources = SourceMap::new();
    let mut diags = Diags::new();
    let _ = Manifest::read(Path::new(DIR), None, &mut fs, &mut sources, &mut diags);
    let rendered = EmitterHuman::new().render(&diags.diagnostics()[0], &sources);
    let page = lilac_error::codes::E076.doc();
    assert!(page.contains(rendered.trim_end()), "{rendered}");
}

/// The project template's `lilac.toml`, as the repository has it.
const TEMPLATE: &str = include_str!("../../../templates/project/lilac.toml");

/// The template filled for `proj` (with `\n` line ends, however git
/// checked it out).
fn filled(libraries: &[String]) -> String {
    crate::fill(
        &TEMPLATE.replace("\r\n", "\n"),
        &crate::Fill {
            name: "proj",
            host: "x86_64-pc-windows-msvc",
            libraries,
        },
    )
}

/// The template's manifest reads back with no diagnostic and states the
/// true defaults: it resolves like an empty manifest, but for its root.
#[test]
fn the_template_is_the_defaults() {
    let text = filled(&[]);
    let read = manifest(&text, &["proj/src"]);
    let settings = Settings::resolve(Some(&read), &Overrides::default(), host());
    let empty = Settings::resolve(
        Some(&Manifest::empty(Path::new(DIR))),
        &Overrides::default(),
        host(),
    );
    assert_eq!(
        settings,
        Settings {
            root: Some(Path::new(DIR).join("src")),
            ..empty
        }
    );
    assert!(text.contains("# is x86_64-pc-windows-msvc.\n"), "{text}");
    assert!(
        text.contains("\n# Standard = { bundled = true }\n"),
        "{text}"
    );
    assert!(!text.contains("{{"), "{text}");
}

/// Every key of [`SETTINGS`] is in the template, under its section, with
/// its flag named, and no line is wider than 72 columns.
#[test]
fn the_template_has_every_setting() {
    let libraries = ["Json".to_owned(), "Standard".to_owned()];
    let text = filled(&libraries);
    for setting in SETTINGS {
        let key = if setting.is_library() {
            "Standard"
        } else {
            setting.key
        };
        let section = text
            .find(&format!("\n[{}]\n", setting.section))
            .unwrap_or_else(|| panic!("[{}] in {text}", setting.section));
        let at = text[section..]
            .lines()
            .position(|line| {
                line.trim_start_matches("# ")
                    .starts_with(&format!("{key} = "))
            })
            .unwrap_or_else(|| panic!("`{key}` in {text}"));
        let before = text[section..]
            .lines()
            .take(at)
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(flag) = setting.flag {
            assert!(before.contains(&format!("({flag})")), "{key}: {text}");
        }
    }
    for line in TEMPLATE.lines() {
        assert!(line.len() <= 72, "{line}");
    }
    assert!(
        text.contains("\nJson = { bundled = true }\nStandard = { bundled = true }\n"),
        "{text}"
    );
    let mut fs = MemFs::default();
    fs.add("proj/lilac.toml", &text);
    fs.add("proj/src/", "");
    fs.add("bundled/Json/", "");
    fs.add("bundled/Standard/", "");
    let mut sources = SourceMap::new();
    let mut diags = Diags::new();
    let read = Manifest::read(
        Path::new(DIR),
        Some(Path::new("bundled")),
        &mut fs,
        &mut sources,
        &mut diags,
    );
    assert_eq!(diags.error_count(), 0);
    let read = read.expect("a manifest").expect("no error");
    assert_eq!(read.libraries.len(), 2);
    assert!(read.libraries.iter().all(|library| library.bundled));
}

/// Placeholders fill on any line, keeping its line ending.
#[test]
fn filling_placeholders() {
    let fill = crate::Fill {
        name: "app",
        host: "aarch64-apple-darwin",
        libraries: &["A".to_owned(), "B".to_owned()],
    };
    assert_eq!(
        crate::fill(
            "a {{name}} {{host}} {{other}}\r\n  # {{libraries}}\r\nend",
            &fill
        ),
        "a app aarch64-apple-darwin {{other}}\r\nA = { bundled = true }\r\n\
         B = { bundled = true }\r\nend"
    );
    assert_eq!(
        crate::fill("{{libraries}}", &fill),
        "A = { bundled = true }\nB = { bundled = true }"
    );
}

/// A template is every file under its folder, relative and sorted.
#[test]
fn reading_a_template() {
    let mut fs = MemFs::default();
    fs.add("tpl/src/main.lilac", "main");
    fs.add("tpl/lilac.toml", "toml");
    fs.add("tpl/empty/", "");
    assert_eq!(
        crate::read_template(Path::new("tpl"), &mut fs).expect("reads"),
        [
            (PathBuf::from("lilac.toml"), "toml".to_owned()),
            (Path::new("src").join("main.lilac"), "main".to_owned()),
        ]
    );
    assert!(crate::read_template(Path::new("missing"), &mut fs).is_err());
    assert_eq!(
        crate::template_dir(None, Some(Path::new("bin"))),
        Some(Path::new("bin").join("..").join("templates"))
    );
    assert_eq!(
        crate::template_dir(Some(PathBuf::from("t")), Some(Path::new("bin"))),
        Some(PathBuf::from("t"))
    );
}
