//! The native link stubs in `toolchain/` stay in sync with the `extern`s of
//! `Standard.Os.Windows` and `Standard.Os.MacOs`: a function missing from a
//! stub is an `undefined symbol` when a program first calls it, so it fails
//! here instead.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The link names of the `extern`s declared in the `*.lilac` files of
/// `folder`: `name: extern function`, or `name: extern "symbol" function`.
fn externs(folder: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let dir = repo().join(folder);
    for entry in std::fs::read_dir(&dir).expect("reads the namespace folder") {
        let path = entry.expect("reads an entry").path();
        if path.extension().is_none_or(|ext| ext != "lilac") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("reads a source file");
        for line in text.lines() {
            let Some((name, rest)) = line.split_once(": extern ") else {
                continue;
            };
            let symbol = rest
                .strip_prefix('"')
                .and_then(|quoted| quoted.split_once('"'))
                .map_or(name, |(symbol, _)| symbol);
            names.insert(symbol.trim().to_owned());
        }
    }
    assert!(!names.is_empty(), "{folder} declares no externs");
    names
}

/// The `EXPORTS` of every `toolchain/windows/*.def`.
fn def_exports() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let dir = repo().join("toolchain/windows");
    for entry in std::fs::read_dir(&dir).expect("reads toolchain/windows") {
        let path = entry.expect("reads an entry").path();
        if path.extension().is_none_or(|ext| ext != "def") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("reads a .def");
        let exports = text
            .split_once("EXPORTS")
            .map_or("", |(_, exports)| exports);
        names.extend(exports.split_whitespace().map(str::to_owned));
    }
    names
}

/// The symbols a `libSystem.tbd` exports.
fn tbd_symbols(triple: &str) -> BTreeSet<String> {
    let path = repo().join("toolchain").join(triple).join("libSystem.tbd");
    let text = std::fs::read_to_string(&path).expect("reads libSystem.tbd");
    let list = text
        .split_once("symbols:")
        .and_then(|(_, rest)| rest.split_once('['))
        .and_then(|(_, rest)| rest.split_once(']'))
        .map_or("", |(list, _)| list);
    list.split(',')
        .map(|symbol| symbol.trim().to_owned())
        .filter(|symbol| !symbol.is_empty())
        .collect()
}

#[test]
fn windows_externs_are_in_the_import_libraries() {
    let exports = def_exports();
    let missing: Vec<_> = externs("lib/Standard/Os/Windows")
        .into_iter()
        .filter(|name| !exports.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "add to toolchain/windows/*.def and regenerate the .lib files \
         (toolchain/README.md): {missing:?}"
    );
    for def in ["kernel32", "bcryptprimitives"] {
        for triple in ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"] {
            let lib = repo()
                .join("toolchain")
                .join(triple)
                .join(format!("{def}.lib"));
            assert!(lib.is_file(), "missing {}", lib.display());
        }
    }
}

#[test]
fn macos_externs_are_in_libsystem() {
    let x86 = tbd_symbols("x86_64-apple-darwin");
    let arm = tbd_symbols("aarch64-apple-darwin");
    assert_eq!(
        x86, arm,
        "the two libSystem.tbd files list different symbols"
    );
    // What the backend itself calls on macOS: the C memory functions
    // (aggregate `==` is `memcmp`; LLVM lowers copies and zeroing to the
    // rest), float `%`, the stack probe and the lazy binder.
    let backend = [
        "memcmp",
        "memcpy",
        "memmove",
        "memset",
        "bzero",
        "fmod",
        "fmodf",
        "__chkstk_darwin",
    ];
    // Mach-O symbols carry a leading `_`.
    let mut missing: Vec<_> = externs("lib/Standard/Os/MacOs")
        .into_iter()
        .chain(backend.map(str::to_owned))
        .filter(|name| !x86.contains(&format!("_{name}")))
        .collect();
    if !x86.contains("dyld_stub_binder") {
        missing.push("dyld_stub_binder".to_owned());
    }
    assert!(
        missing.is_empty(),
        "add to both toolchain/*-apple-darwin/libSystem.tbd: {missing:?}"
    );
}
