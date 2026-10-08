//! The standard library under `lib/Standard` must check and emit cleanly.
//! Folder mode is required: a single `int64.lilac` file cannot see `Bool`.

use std::path::PathBuf;
use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

/// `lib/Standard` at the repository root, resolved from the crate manifest
/// directory so the test does not depend on the process working directory.
fn standard_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("lib")
        .join("Standard")
}

/// The float objects expose only the `@fbitN -> FloatN` value conversion; the
/// `@bitN <-> FloatN` reinterpret pairs are removed, so an integer literal
/// cannot silently bitcast into a float (the intrinsics remain available).
#[test]
fn float_objects_do_not_expose_bit_reinterpret_conversions() {
    let standard = standard_dir();
    for file in ["float32.lilac", "float64.lilac"] {
        let text = std::fs::read_to_string(standard.join(file)).expect("reads stdlib file");
        assert!(
            !text.contains("@bits_to_fbit") && !text.contains("@fbit_to_bits"),
            "`{file}` must not wrap the reinterpret intrinsics in `@conversion`"
        );
    }
}

#[test]
#[ignore = "phase 7: lib/Standard needs library mounting and the Phase 1-5 fixes"]
fn standard_library_checks_and_emits() {
    let standard = standard_dir();
    for sub in ["check", "emit-ir"] {
        let out = lilac()
            .arg(sub)
            .arg(&standard)
            .output()
            .expect("runs lilac");
        assert!(
            out.status.success(),
            "`lilac {sub} lib/Standard` failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if sub == "check" {
            assert!(
                out.stderr.is_empty(),
                "`lilac check lib/Standard` wrote to stderr:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
