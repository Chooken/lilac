//! M5b UI tests: `.ll` snapshots are byte-stable under the canonical
//! `x86_64-unknown-none` triple, every success program links and runs on the
//! host at `-O0` and `-O2`, and the zero-init plus RAII `@on_drop` call is
//! visible in `generic_hook_drop.ll`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The canonical triple the committed `.ll` snapshots are pinned to, so they do
/// not embed the build host's triple (M5a portability precedent).
const CANONICAL_TARGET: &str = "x86_64-unknown-none";

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

fn ui_dir() -> PathBuf {
    Path::new("../../tests/ui").to_path_buf()
}

fn ui(name: &str) -> String {
    format!("../../tests/ui/{name}")
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .expect("utf8 stem")
        .to_owned()
}

/// Every `tests/ui/*.lilac` with a sibling `.lir` (the success set), sorted.
fn success_fixtures() -> Vec<PathBuf> {
    let mut sources: Vec<PathBuf> = std::fs::read_dir(ui_dir())
        .expect("reads tests/ui")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "lilac")
                && path.with_extension("lir").exists()
        })
        .collect();
    sources.sort();
    assert!(!sources.is_empty(), "no success fixtures found");
    sources
}

/// `emit-llvm --check` over every committed `.ll`, using the canonical triple.
#[test]
fn ll_snapshots_stable() {
    let mut snapshots: Vec<PathBuf> = std::fs::read_dir(ui_dir())
        .expect("reads tests/ui")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ll"))
        .collect();
    snapshots.sort();

    let snapshot_stems: Vec<String> = snapshots.iter().map(|path| stem(path)).collect();
    let fixture_stems: Vec<String> = success_fixtures().iter().map(|path| stem(path)).collect();
    assert_eq!(
        snapshot_stems, fixture_stems,
        "there must be exactly one `.ll` snapshot per success fixture"
    );

    for snapshot in snapshots {
        let source = snapshot.with_extension("lilac");
        let output = lilac()
            .args([
                "emit-llvm",
                "--check",
                &format!("--target={CANONICAL_TARGET}"),
            ])
            .arg(&source)
            .output()
            .expect("runs lilac");
        assert!(
            output.status.success(),
            "{}:\n{}",
            source.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Locates the toolchain `clang`: `%LLVM_SYS_181_PREFIX%\bin\clang[.exe]`, else
/// `clang` on `PATH`.
fn clang() -> PathBuf {
    if let Ok(prefix) = std::env::var("LLVM_SYS_181_PREFIX") {
        let name = if cfg!(windows) { "clang.exe" } else { "clang" };
        let candidate = Path::new(&prefix).join("bin").join(name);
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("clang")
}

/// A per-process scratch directory so parallel test binaries do not collide.
fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!("lilac-m5b-{}", std::process::id()))
}

/// Emits host-triple IR for `source` at `opt` and writes it to `dest`.
fn emit_host_ir(source: &Path, opt: &str, dest: &Path) {
    let output = lilac()
        .args(["emit-llvm", opt])
        .arg(source)
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "{} emit-llvm {opt} failed:\n{}",
        source.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(dest, &output.stdout).expect("writes IR");
}

/// Every success program links (toolchain `clang`) and runs to exit 0 on the
/// host, at `-O0` and `-O2`. A failure here is a real backend bug, so it is
/// reported rather than skipped.
#[test]
fn compiles_and_runs_on_host() {
    let clang = clang();
    let scratch = scratch_dir();
    std::fs::create_dir_all(&scratch).expect("creates scratch dir");

    for source in success_fixtures() {
        let name = stem(&source);
        for (label, opt) in [("O0", "-O0"), ("O2", "-O2")] {
            let ll = scratch.join(format!("{name}-{label}.ll"));
            emit_host_ir(&source, opt, &ll);

            let exe = scratch.join(format!("{name}-{label}.exe"));
            let link = Command::new(&clang)
                .arg(&ll)
                .arg("-o")
                .arg(&exe)
                .output()
                .expect("runs clang");
            assert!(
                link.status.success(),
                "{name} ({label}) failed to link:\n{}",
                String::from_utf8_lossy(&link.stderr)
            );

            let run = Command::new(&exe).output().expect("runs executable");
            assert!(
                run.status.success(),
                "{name} ({label}) exited with {:?}",
                run.status.code()
            );
        }
    }

    let _ = std::fs::remove_dir_all(&scratch);
}

/// `generic_hook_drop.ll` shows both zero-initialization and the RAII
/// `@on_drop` call the drop marker is paired with.
#[test]
fn drops_and_zero_init_visible() {
    let output = lilac()
        .args(["emit-llvm", &ui("generic_hook_drop.lilac")])
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "emit-llvm failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).expect("utf8 IR");
    assert!(text.contains("zeroinitializer"), "{text}");
    assert!(
        text.contains("call void @\"GenBox.1..bit32.@on_drop\""),
        "{text}"
    );
}
