//! M4 UI tests: duck-typed generics monomorphize to concrete IR, `.lir`
//! snapshots are byte-stable, and the `E042`/`E044`/`E051`/`E068` FAILS pins hold.
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
    // stale non-generic snapshot (e.g. `references.lir`) is caught.
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
        // Run in `tests/ui` with the bare name, so panic sites read the same
        // on every machine.
        let output = lilac()
            .current_dir("../../tests/ui")
            .args(["emit-ir", "--check"])
            .arg(source.file_name().expect("a file name"))
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
fn fails_generic_instance_budget_is_e068() {
    // Wide fan-out with no recursion crosses the instance budget: one
    // `E068` (no `E051`, since no generic recurses), naming the definitions
    // with the most instances and how their arguments grow.
    let (ok, stderr) = check("FAILS_generic_instance_budget.lilac", &[]);
    assert!(!ok, "should fail");
    assert_eq!(
        stderr.matches("error[").count(),
        1,
        "exactly one error:\n{stderr}"
    );
    assert!(
        stderr.contains("error[E068]: too many generic instances"),
        "{stderr}"
    );
    for note in [
        "= note: `level16` has ",
        "= note: `level15` has 16384 instances, from level15[A[A[A[A[A[A[A[A[A[A[A[A[A[A[@bit32]]]]]]]]]]]]]]] to level15[B[B[B[B[B[B[B[B[B[B[B[B[B[B[@bit32]]]]]]]]]]]]]]]",
        "= note: `level14` has 8192 instances, from ",
        "= note: instantiation chain: level1[@bit32] -> ",
    ] {
        assert!(stderr.contains(note), "missing `{note}`:\n{stderr}");
    }
}
#[test]
fn no_cascade_single_e042() {
    // One failed instantiation reached from two call sites is exactly one
    // `E042`, whose trace lists both sites.
    let (ok, stderr) = check("FAILS_generic_no_cascade.lilac", &[]);
    assert!(!ok, "should fail");
    assert_eq!(
        stderr.matches("error[E042]").count(),
        1,
        "expected exactly one E042:\n{stderr}"
    );
    assert!(
        stderr.contains("FAILS_generic_no_cascade.lilac:6:21)")
            && stderr.contains("FAILS_generic_no_cascade.lilac:7:22)"),
        "both call sites traced:\n{stderr}"
    );
}

/// The short-form `FAILS_generic_duck_typing` render: one `E042` whose primary
/// label is the failing use inside the generic body, with a sourced
/// instantiation note (pretty instance name) and the inferred required method
/// signature.
const DUCK_TYPING_SHORT: &str = concat!(
    "error[E042]: type `@bit32` does not satisfy `gen_function[T]` requirements\n",
    "  --> ../../tests/ui/FAILS_generic_duck_typing.lilac:2:11\n",
    "   |\n",
    " 2 |     out = T.other()\n",
    "   |           ^^^^^^^^^ missing method `other` on `@bit32`\n",
    "...\n",
    "12 |     variable: @bit32 = gen_function[@bit32]()\n",
    "   |                        ---------------------- required by instantiation of gen_function[@bit32] here\n",
    "   |\n",
    "   = note: reached from:\n",
    "             at gen_function[@bit32] (../../tests/ui/FAILS_generic_duck_typing.lilac:2:11)\n",
    "               at main (../../tests/ui/FAILS_generic_duck_typing.lilac:12:24)\n",
    "   = note: based on this usage, the generic body requires a method matching:\n",
    "               other: function () T\n",
    "\n",
    "error: aborting due to 1 previous error\n",
);

#[test]
fn fails_generic_duck_typing_short_snapshot() {
    let (ok, stderr) = check("FAILS_generic_duck_typing.lilac", &[]);
    assert!(!ok, "should fail");
    assert_eq!(stderr, DUCK_TYPING_SHORT);
}

#[test]
fn fails_generic_duck_typing_verbose_snapshot() {
    // A two-frame trace is under the elision depth, so the `--verbose`
    // render is identical.
    let (ok, stderr) = check("FAILS_generic_duck_typing.lilac", &["--verbose"]);
    assert!(!ok, "should fail");
    assert_eq!(stderr, DUCK_TYPING_SHORT);
    assert!(stderr.contains("required by instantiation of gen_function[@bit32]"));
    assert!(!stderr.contains("note: in instantiation of gen_function[@bit32]"));
}

/// A >5-level trace (`main` -> `level0` -> ... -> `level5`): the
/// `--verbose` render prints every frame while the default elides the
/// deepest ones.
const DEEP_CHAIN_VERBOSE: &str = concat!(
    "error[E042]: type `@bit32` does not satisfy `level5[T]` requirements\n",
    "  --> ../../tests/ui/FAILS_generic_deep_chain.lilac:22:11\n",
    "   |\n",
    "18 |     out = level5[T](x)\n",
    "   |           ------------ required by instantiation of level5[@bit32] here\n",
    "...\n",
    "22 |     out = x.missing()\n",
    "   |           ^^^^^^^^^^^ missing method `missing` on `@bit32`\n",
    "   |\n",
    "   = note: reached from:\n",
    "             at level5[@bit32] (../../tests/ui/FAILS_generic_deep_chain.lilac:22:11)\n",
    "               at level4[@bit32] (../../tests/ui/FAILS_generic_deep_chain.lilac:18:11)\n",
    "                 at level3[@bit32] (../../tests/ui/FAILS_generic_deep_chain.lilac:14:11)\n",
    "                   at level2[@bit32] (../../tests/ui/FAILS_generic_deep_chain.lilac:10:11)\n",
    "                     at level1[@bit32] (../../tests/ui/FAILS_generic_deep_chain.lilac:6:11)\n",
    "                       at level0[@bit32] (../../tests/ui/FAILS_generic_deep_chain.lilac:2:11)\n",
    "                         at main (../../tests/ui/FAILS_generic_deep_chain.lilac:26:17)\n",
    "   = note: based on this usage, the generic body requires a method matching:\n",
    "               missing: function (self) T\n",
    "\n",
    "error: aborting due to 1 previous error\n",
);

#[test]
fn fails_generic_deep_chain_verbose_snapshot() {
    let (ok, verbose) = check("FAILS_generic_deep_chain.lilac", &["--verbose"]);
    assert!(!ok, "should fail");
    assert_eq!(verbose, DEEP_CHAIN_VERBOSE);
    assert!(verbose.contains("level5[@bit32]"), "{verbose}");
    assert!(!verbose.contains("elided"), "{verbose}");

    let (_, short) = check("FAILS_generic_deep_chain.lilac", &[]);
    assert_ne!(short, verbose, "verbose must differ from default:\n{short}");
    assert!(
        short.contains("2 more frames elided (use --verbose)"),
        "{short}"
    );
}

/// `E010` always prints both sides: the expected annotation label and the
/// found value label (the M4 both-sides fix).
const E010_BOTH_SIDES: &str = concat!(
    "error[E010]: mismatched types: expected ref @bit8, found @bit8\n",
    " --> ../../tests/ui/FAILS_ref_argument.lilac:6:9\n",
    "  |\n",
    "6 |     set(v)\n",
    "  |     ----^-\n",
    "  |     |   |\n",
    "  |     |   found `@bit8` here\n",
    "  |     expected `ref @bit8` here\n",
    "\n",
    "error: aborting due to 1 previous error\n",
);

#[test]
fn e010_shows_both_sides() {
    let (ok, stderr) = check("FAILS_ref_argument.lilac", &[]);
    assert!(!ok, "should fail");
    assert_eq!(stderr, E010_BOTH_SIDES);
}
