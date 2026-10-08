//! Toolchain lookup shared by the integration tests.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Locates the toolchain `clang`: `%LLVM_SYS_181_PREFIX%\bin\clang[.exe]`, else
/// `clang` on `PATH`. `None` when neither runs.
pub fn clang() -> Option<PathBuf> {
    if let Ok(prefix) = std::env::var("LLVM_SYS_181_PREFIX") {
        let name = format!("clang{}", std::env::consts::EXE_SUFFIX);
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

/// A development sysroot for native builds (`LILAC_SYSROOT`): the repository's
/// `toolchain/<triple>/` stubs plus the LLVM tree's `lld` as `bin/lld`,
/// assembled under cargo's test scratch folder. `None` when the LLVM tree has
/// no `lld`.
pub fn sysroot() -> Option<PathBuf> {
    let prefix = std::env::var_os("LLVM_SYS_181_PREFIX")?;
    let lld_name = format!("lld{}", std::env::consts::EXE_SUFFIX);
    let lld = Path::new(&prefix).join("bin").join(&lld_name);
    if !lld.is_file() {
        return None;
    }
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sysroot");
    let toolchain = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../toolchain");
    for entry in std::fs::read_dir(&toolchain).ok()?.filter_map(Result::ok) {
        let triple = entry.file_name();
        if !triple.to_string_lossy().contains('-') {
            continue;
        }
        for stub in std::fs::read_dir(entry.path()).ok()?.filter_map(Result::ok) {
            place(&stub.path(), &root.join(&triple).join(stub.file_name()))?;
        }
    }
    place(&lld, &root.join("bin").join(lld_name))?;
    Some(root)
}

/// Puts a copy of `src` at `dest` (a hard link when possible), unless a file
/// of the same size is already there. Test binaries run in parallel, so the
/// copy goes to a unique temporary name first and is renamed into place.
fn place(src: &Path, dest: &Path) -> Option<()> {
    let size = std::fs::metadata(src).ok()?.len();
    if std::fs::metadata(dest).is_ok_and(|meta| meta.len() == size) {
        return Some(());
    }
    std::fs::create_dir_all(dest.parent()?).ok()?;
    let tmp = dest.with_extension(format!("{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    if std::fs::hard_link(src, &tmp).is_err() {
        std::fs::copy(src, &tmp).ok()?;
    }
    if std::fs::rename(&tmp, dest).is_err() {
        let _ = std::fs::remove_file(&tmp);
        // Another test binary placed it first.
        std::fs::metadata(dest).ok()?;
    }
    Some(())
}
