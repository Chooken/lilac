//! The standard library under `lib/Standard` must check and emit cleanly
//! when mounted as the `Standard` library.

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
/// cannot silently bitcast into a float. The intrinsics remain available, and
/// methods such as `to_bits` and `next_up` use them; only a `@conversion` hook
/// may not.
#[test]
fn float_objects_do_not_expose_bit_reinterpret_conversions() {
    let standard = standard_dir();
    for file in ["float16.lilac", "float32.lilac", "float64.lilac"] {
        let text = std::fs::read_to_string(standard.join(file)).expect("reads stdlib file");
        let hooks: Vec<&str> = text
            .lines()
            .filter(|line| line.contains("@conversion"))
            .collect();
        assert!(!hooks.is_empty(), "`{file}` has no `@conversion` hooks");
        for line in hooks {
            let signature = line.split("=>").next().unwrap_or(line);
            assert!(
                !line.contains("@bits_to_fbit")
                    && !line.contains("@fbit_to_bits")
                    && !signature.contains("@bit"),
                "`{file}` must not convert between `@bitN` and a float: {}",
                line.trim()
            );
        }
    }
}

/// `tests/run/stdlib` uses every `Standard` type through `--lib`, so the
/// whole library is instantiated; it must check silently and emit. (The run
/// harness also links and runs it.)
#[test]
fn standard_library_checks_and_emits() {
    let lib = format!("--lib=Standard={}", standard_dir().display());
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/run/stdlib");
    for sub in ["check", "emit-ir"] {
        let out = lilac()
            .arg(sub)
            .arg(&lib)
            .arg(&project)
            .output()
            .expect("runs lilac");
        assert!(
            out.status.success(),
            "`lilac {sub}` on the stdlib project failed:
{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if sub == "check" {
            assert!(
                out.stderr.is_empty(),
                "`lilac check` on the stdlib project wrote to stderr:
{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
