//! M5b UI tests: `.ll` snapshots are byte-stable under the canonical
//! `x86_64-unknown-none` triple, every success program links and runs on the
//! host in the debug and release modes (built with `--libc` and `Standard`), and the zero-init plus RAII `@on_drop` call is
//! visible in `generic_hook_drop.ll`.

mod support;

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
        // Run in `tests/ui` with the bare name, so panic sites read the same
        // on every machine.
        let output = lilac()
            .current_dir(ui_dir())
            .args([
                "emit-llvm",
                "--check",
                &format!("--target={CANONICAL_TARGET}"),
            ])
            .arg(source.file_name().expect("a file name"))
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

/// Every success program builds (`--libc`, toolchain `clang`) and runs to
/// exit 0 on the host, in the debug and release modes. A failure here is a
/// real backend bug, so it is reported rather than skipped.
#[test]
fn compiles_and_runs_on_host() {
    let clang = clang();
    let scratch = scratch_dir();
    std::fs::create_dir_all(&scratch).expect("creates scratch dir");

    for source in success_fixtures() {
        let name = stem(&source);
        for (label, opt) in [("debug", "--mode=debug"), ("release", "--mode=release")] {
            let exe = scratch.join(format!("{name}-{label}.exe"));
            let run = support::build_and_run(&clang, &source, &exe, &[opt]);
            assert!(
                run.status.success(),
                "{name} ({label}) exited with {:?}",
                run.status.code()
            );
        }
    }

    let _ = std::fs::remove_dir_all(&scratch);
}

/// The LIR runtime is culled when unused: `emit-llvm` for a program that
/// reaches none of it contains no runtime bodies, no runtime calls, and none of
/// the backend's leaf definitions.
#[test]
fn no_runtime_fixture_emits_no_runtime_text() {
    let output = lilac()
        .args(["emit-llvm", &ui("int_addition.lilac")])
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "emit-llvm failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).expect("utf8 IR");
    // The LIR runtime is culled entirely when unreached.
    for needle in [
        ".lilac.byte_buffer",
        ".lilac.format_args",
        ".lilac.load_u8",
        ".lilac.store_u8",
    ] {
        assert!(!text.contains(needle), "unexpected `{needle}` in:\n{text}");
    }
    // The backend hand-writes the three leaf definitions only when a program
    // reaches them; this program reaches none, so they are culled too.
    for def in [
        "define ptr @.lilac.ptr_add(ptr %base, i64 %offset)",
        "define void @.lilac.memcpy(ptr %dest, ptr %src, i64 %len)",
        "define void @.lilac.abort()",
        "declare void @llvm.trap()",
    ] {
        assert!(!text.contains(def), "unexpected `{def}` in:\n{text}");
    }
    // This program calls none of the leaves.
    for call in [
        "call ptr @.lilac.ptr_add",
        "call void @.lilac.memcpy",
        "call void @.lilac.abort",
    ] {
        assert!(!text.contains(call), "unexpected `{call}` in:\n{text}");
    }
}

/// `format_args_use.lilac` reads an f-string back through the runtime
/// (`.lilac.format_args_hole` calls the formatter through its pointer); it
/// builds with the toolchain clang and runs to exit 0 on the host.
#[test]
fn fstring_runtime_compiles_and_runs_on_host() {
    let clang = clang();
    let scratch = std::env::temp_dir().join(format!("lilac-task6-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("creates scratch dir");

    let source = Path::new(&ui("format_args_use.lilac")).to_path_buf();
    let ll = scratch.join("format_args_use.ll");
    emit_host_ir(&source, "--mode=debug", &ll);
    let text = std::fs::read_to_string(&ll).expect("reads IR");
    assert!(text.contains(".lilac.format_args_hole"), "{text}");

    let exe = scratch.join("format_args_use.exe");
    let run = support::build_and_run(&clang, &source, &exe, &["--mode=debug"]);
    assert!(
        run.status.success(),
        "format_args_use exited with {:?}",
        run.status.code()
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// On a 64-bit target that is neither `x86_64` nor `aarch64`, a template
/// part is 24 bytes (a 16-byte `#byte_buffer` and a pointer), in the
/// runtime and in the template constant alike. If this LLVM build lacks the
/// target the assertion reports the skip and returns; the driver-level test
/// still covers the runtime's offsets.
#[test]
fn riscv64_template_parts_match_the_layout() {
    let output = lilac()
        .args([
            "emit-llvm",
            "--target=riscv64-unknown-none",
            &ui("format_args_use.lilac"),
        ])
        .output()
        .expect("runs lilac");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("unknown target triple") {
            eprintln!("skipping riscv64 assertion; LLVM build lacks the target:\n{stderr}");
            return;
        }
        panic!("riscv64 emit-llvm failed:\n{stderr}");
    }
    let text = String::from_utf8(output.stdout).expect("utf8 IR");

    // `#byte_buffer` is 16 bytes aligned to 8: an 8-byte length plus an
    // 8-byte pointer.
    assert!(text.contains("alloca [16 x i8], align 8"), "{text}");
    // The template: the hole count, then each part's text and formatter.
    assert!(
        text.contains(
            "@fmt0 = private unnamed_addr constant <{ i64, i64, ptr, ptr, i64, ptr, ptr }>"
        ),
        "{text}"
    );
    let name = ".lilac.format_args_piece";
    let body = text
        .split("\ndefine ")
        .find(|segment| segment.starts_with(&format!("void @{name}")))
        .unwrap_or_else(|| panic!("missing `{name}` in:\n{text}"));
    let mul = body
        .lines()
        .find(|line| line.contains("mul i64"))
        .unwrap_or_else(|| panic!("no stride multiply in `{name}`:\n{body}"));
    assert!(mul.trim_end().ends_with(", 24"), "{name}: {mul}\n{body}");
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
    // Aggregate places are zeroed with `memset`.
    assert!(text.contains("call void @llvm.memset."), "{text}");
    assert!(
        text.contains("call void @\"GenBox[m].@on_drop(RI6GenBoxmEE)\""),
        "{text}"
    );
}

/// A function reference is the function's identity. The backend never
/// marks a defined function `unnamed_addr` (which would let LLVM merge two
/// with the same body): no snapshot has one. An optimized build of
/// `function_refs.lilac` keeps both of its identical functions (LLVM folds
/// `a == b` to false because they are distinct; `compiles_and_runs_on_host`
/// runs it in release, where it traps if they compare equal).
#[test]
fn functions_keep_their_addresses() {
    for entry in std::fs::read_dir(ui_dir()).expect("reads tests/ui") {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_some_and(|ext| ext == "ll") {
            let text = std::fs::read_to_string(&path).expect("reads a snapshot");
            for line in text.lines().filter(|line| line.starts_with("define")) {
                assert!(!line.contains("unnamed_addr"), "{}: {line}", path.display());
            }
        }
    }
    let output = lilac()
        .args(["emit-llvm", "--mode=release", &ui("function_refs.lilac")])
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let release = String::from_utf8_lossy(&output.stdout);
    for name in ["@.root.one(", "@.root.same("] {
        assert!(
            release
                .lines()
                .any(|line| line.starts_with("define") && line.contains(name)),
            "{name} was folded away:\n{release}"
        );
    }
}
