//! Links LLVM on macOS so `lilac` needs nothing outside macOS itself.
//!
//! Elsewhere `llvm-sys` links LLVM. On macOS it is built with
//! `no-llvm-linking` and this script does it instead, because what
//! `llvm-sys` does there ties the executable to the LLVM install (usually
//! Homebrew's): it links `-lzstd` as a dylib, and it puts LLVM's `lib`
//! folder on the linker's search path, where Apple's linker resolves
//! `libSystem`'s re-exported `libunwind` to LLVM's `libunwind.dylib`.
//!
//! So every archive LLVM needs (its own and `libzstd.a`) is linked into a
//! folder of the build's own, and that folder is the only one searched:
//! the system libraries come from the SDK.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const LLVM_MAJOR: &str = "18";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LLVM_SYS_181_PREFIX");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let config = llvm_config().unwrap_or_else(|| {
        panic!(
            "no llvm-config for LLVM {LLVM_MAJOR}: set LLVM_SYS_181_PREFIX or `brew install llvm@{LLVM_MAJOR}`"
        )
    });
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("llvm-libs");
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).expect("create the LLVM archive folder");

    for archive in run(&config, &["--link-static", "--libfiles"]).split_whitespace() {
        let name = place(Path::new(archive), &out);
        println!("cargo:rustc-link-lib=static={name}");
    }
    for flag in run(&config, &["--link-static", "--system-libs"]).split_whitespace() {
        let lib = flag
            .strip_prefix("-l")
            .unwrap_or_else(|| panic!("unexpected llvm-config system library `{flag}`"));
        if lib == "zstd" {
            let name = place(&zstd_archive(), &out);
            println!("cargo:rustc-link-lib=static={name}");
        } else {
            println!("cargo:rustc-link-lib=dylib={lib}");
        }
    }
    println!("cargo:rustc-link-lib=dylib=c++");
    println!("cargo:rustc-link-search=native={}", out.display());
}

/// `LLVM_SYS_181_PREFIX/bin/llvm-config`, else an `llvm-config` of the
/// right version on `PATH`, else Homebrew's `llvm@18`: what `llvm-sys`
/// finds, so both build against one LLVM.
fn llvm_config() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(prefix) = env::var_os("LLVM_SYS_181_PREFIX") {
        candidates.push(Path::new(&prefix).join("bin/llvm-config"));
    }
    candidates.push(PathBuf::from("llvm-config"));
    if let Some(prefix) = brew_prefix(&format!("llvm@{LLVM_MAJOR}")) {
        candidates.push(prefix.join("bin/llvm-config"));
    }
    candidates.into_iter().find(|config| {
        Command::new(config)
            .arg("--version")
            .output()
            .is_ok_and(|out| {
                String::from_utf8_lossy(&out.stdout).starts_with(&format!("{LLVM_MAJOR}."))
            })
    })
}

/// The static `libzstd.a` LLVM was built against.
fn zstd_archive() -> PathBuf {
    brew_prefix("zstd")
        .map(|prefix| prefix.join("lib/libzstd.a"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!("LLVM needs zstd, and no static libzstd.a was found (`brew install zstd`)")
        })
}

fn brew_prefix(formula: &str) -> Option<PathBuf> {
    let out = Command::new("brew")
        .args(["--prefix", formula])
        .output()
        .ok()?;
    let prefix = String::from_utf8(out.stdout).ok()?;
    let prefix = prefix.trim();
    (out.status.success() && !prefix.is_empty()).then(|| PathBuf::from(prefix))
}

/// Links `archive` into `folder` and returns its library name (`libX.a`
/// is `X`).
fn place(archive: &Path, folder: &Path) -> String {
    let file = archive
        .file_name()
        .and_then(|name| name.to_str())
        .expect("an archive file name");
    let name = file
        .strip_prefix("lib")
        .and_then(|name| name.strip_suffix(".a"))
        .unwrap_or_else(|| panic!("`{}` is not a static library", archive.display()));
    let dest = folder.join(file);
    if fs::hard_link(archive, &dest).is_err() {
        fs::copy(archive, &dest)
            .unwrap_or_else(|err| panic!("copy `{}`: {err}", archive.display()));
    }
    name.to_owned()
}

fn run(config: &Path, args: &[&str]) -> String {
    let out = Command::new(config)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("run `{}`: {err}", config.display()));
    assert!(
        out.status.success(),
        "`{} {}` failed",
        config.display(),
        args.join(" ")
    );
    String::from_utf8(out.stdout).expect("llvm-config prints UTF-8")
}
