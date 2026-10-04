//! M4 UI tests: duck-typed generics monomorphize to concrete IR, `.lir`
//! snapshots are byte-stable, and the `E042`/`E044`/`E051` FAILS pins hold.
//! The short-form and `--verbose` `FAILS_generic_duck_typing` renders are
//! pinned as exact snapshots.

use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

fn ui(name: &str) -> String {
    format!("../../tests/ui/{name}")
}

const PORTS: &[&str] = &[
    "generic_function.lilac",
    "generic_object.lilac",
    "generic_object_functions.lilac",
    "generic_duck_typing_value.lilac",
    "generic_duck_typing_static.lilac",
    "generic_hook_drop.lilac",
];

#[test]
fn check_ports_clean() {
    for port in PORTS {
        let output = lilac()
            .args(["check", &ui(port)])
            .output()
            .expect("runs lilac");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{port} stderr:\n{stderr}");
        assert!(stderr.is_empty(), "{port} stderr:\n{stderr}");
    }
}

#[test]
fn lir_snapshots_stable() {
    // Every `*.lir` snapshot under `tests/ui`, not just the M4 `PORTS`, so a
    // stale non-generic snapshot (e.g. `op_bit_mixed_ok.lir`) is caught.
    let mut snapshots: Vec<std::path::PathBuf> = std::fs::read_dir("../../tests/ui")
        .expect("reads tests/ui")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "lir"))
        .collect();
    snapshots.sort();
    assert!(!snapshots.is_empty(), "no .lir snapshots found");
    for snapshot in snapshots {
        let source = snapshot.with_extension("lilac");
        let output = lilac()
            .args(["emit-ir", "--check"])
            .arg(&source)
            .output()
            .expect("runs lilac");
        assert!(
            output.status.success(),
            "{} stderr:\n{}",
            source.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn check(name: &str, extra: &[&str]) -> (bool, String) {
    let mut cmd = lilac();
    cmd.args(["check", &ui(name)]);
    cmd.args(extra);
    let output = cmd.output().expect("runs lilac");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn fails_with(name: &str, code: &str) {
    let (ok, stderr) = check(name, &[]);
    assert!(!ok, "{name} should fail");
    assert!(
        stderr.contains(&format!("error[{code}]")),
        "{name} stderr:\n{stderr}"
    );
}

#[test]
fn fails_generic_duck_typing_is_e042() {
    fails_with("FAILS_generic_duck_typing.lilac", "E042");
}

#[test]
fn fails_generic_arity_is_e044() {
    fails_with("FAILS_generic_arity.lilac", "E044");
}

#[test]
fn fails_generic_cyclic_is_e051() {
    fails_with("FAILS_generic_cyclic.lilac", "E051");
}

#[test]
fn fails_generic_static_bare_is_e044() {
    // A bare generic-object receiver (`GenObj.ping()`) must not fall through to
    // the unmonomorphized template (that was an ICE); it is a clean `E044`.
    fails_with("FAILS_generic_static_bare.lilac", "E044");
}

#[test]
fn fails_generic_object_cycle_is_e051() {
    // A growing generic object layout terminates with `E051`, never a stack
    // overflow.
    fails_with("FAILS_generic_object_cycle.lilac", "E051");
}

#[test]
fn no_cascade_single_e042() {
    // One failed instantiation reached from two call sites is exactly one
    // `E042`: the second site reuses the poisoned memo and stays silent.
    let (ok, stderr) = check("FAILS_generic_no_cascade.lilac", &[]);
    assert!(!ok, "should fail");
    assert_eq!(
        stderr.matches("error[E042]").count(),
        1,
        "expected exactly one E042:\n{stderr}"
    );
}

/// The short-form `FAILS_generic_duck_typing` render: one use-site `E042`
/// with the call-site primary label, the definition note, the captured
/// missing-member label, and the elided single-frame instantiation chain.
const DUCK_TYPING_SHORT: &str = concat!(
    "error[E042]: type `@bit32` does not satisfy `gen_function[T]` requirements\n",
    "  --> ../../tests/ui/FAILS_generic_duck_typing.lilac:12:24\n",
    "   |\n",
    "12 |     variable: @bit32 = gen_function[@bit32]()\n",
    "   |                        ^^^^^^^^^^^^^^^^^^^^^^\n",
    "   = instantiated here\n",
    "note: ../../tests/ui/FAILS_generic_duck_typing.lilac:1:1 required by `gen_function[T]` here\n",
    "note: ../../tests/ui/FAILS_generic_duck_typing.lilac:2:11 missing method `other` on `@bit32`\n",
    "note: in instantiation of gen_function__1__bit32\n",
);

#[test]
fn fails_generic_duck_typing_short_snapshot() {
    let (ok, stderr) = check("FAILS_generic_duck_typing.lilac", &[]);
    assert!(!ok, "should fail");
    assert_eq!(stderr, DUCK_TYPING_SHORT);
}

#[test]
fn fails_generic_duck_typing_verbose_snapshot() {
    // With a single instantiation frame the `--verbose` render is identical,
    // but it must still carry the full obligation tree (def note + frame).
    let (ok, stderr) = check("FAILS_generic_duck_typing.lilac", &["--verbose"]);
    assert!(!ok, "should fail");
    assert_eq!(stderr, DUCK_TYPING_SHORT);
    assert!(stderr.contains("note: in instantiation of gen_function__1__bit32"));
}

/// A >5-frame instantiation chain (`level0` -> ... -> `level5`): the
/// `--verbose` render prints every frame while the default elides the
/// trailing ones.
const DEEP_CHAIN_VERBOSE: &str = concat!(
    "error[E042]: type `@bit32` does not satisfy `level5[T]` requirements\n",
    "  --> ../../tests/ui/FAILS_generic_deep_chain.lilac:18:11\n",
    "   |\n",
    "18 |     out = level5[T](x)\n",
    "   |           ^^^^^^^^^^^^\n",
    "   = instantiated here\n",
    "note: ../../tests/ui/FAILS_generic_deep_chain.lilac:21:1 required by `level5[T]` here\n",
    "note: ../../tests/ui/FAILS_generic_deep_chain.lilac:22:11 missing method `missing` on `@bit32`\n",
    "note: in instantiation of level0__1__bit32\n",
    "note: in instantiation of level1__1__bit32\n",
    "note: in instantiation of level2__1__bit32\n",
    "note: in instantiation of level3__1__bit32\n",
    "note: in instantiation of level4__1__bit32\n",
    "note: in instantiation of level5__1__bit32\n",
);

#[test]
fn fails_generic_deep_chain_verbose_snapshot() {
    let (ok, verbose) = check("FAILS_generic_deep_chain.lilac", &["--verbose"]);
    assert!(!ok, "should fail");
    assert_eq!(verbose, DEEP_CHAIN_VERBOSE);
    assert!(verbose.contains("level5__1__bit32"), "{verbose}");
    assert!(!verbose.contains("elided"), "{verbose}");

    let (_, short) = check("FAILS_generic_deep_chain.lilac", &[]);
    assert_ne!(short, verbose, "verbose must differ from default:\n{short}");
    assert!(
        short.contains("1 more frame elided (use --verbose)"),
        "{short}"
    );
}

/// `E010` always prints both sides: the expected annotation label and the
/// found value label (the M4 both-sides fix).
const E010_BOTH_SIDES: &str = concat!(
    "error[E010]: mismatched types: expected ref @bit8, found @bit8\n",
    "  --> ../../tests/ui/FAILS_ref_argument.lilac:6:9\n",
    "   |\n",
    "6 |     set(v)\n",
    "   |         ^\n",
    "   = found `@bit8` here\n",
    "note: ../../tests/ui/FAILS_ref_argument.lilac:6:5 expected `ref @bit8` here\n",
);

#[test]
fn e010_shows_both_sides() {
    let (ok, stderr) = check("FAILS_ref_argument.lilac", &[]);
    assert!(!ok, "should fail");
    assert_eq!(stderr, E010_BOTH_SIDES);
}
