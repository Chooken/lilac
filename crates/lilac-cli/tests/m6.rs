//! M6a UI tests: the CLI compiles a whole folder project and reports `E025`
//! for a `using` whose target folder is missing. Module fixtures also have
//! merged `.lir`/`.ll` snapshots and a clang end-to-end run.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The canonical triple the committed `.ll` snapshots are pinned to (M5a/M5b
/// portability precedent).
const CANONICAL_TARGET: &str = "x86_64-unknown-none";

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

fn modules(name: &str) -> String {
    format!("../../tests/ui/modules/{name}")
}

fn ui(name: &str) -> String {
    format!("../../tests/ui/{name}")
}

#[test]
fn check_two_file_folder_succeeds() {
    let output = lilac()
        .arg("check")
        .arg(modules("two_files"))
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn check_missing_using_folder_is_e025() {
    let output = lilac()
        .arg("check")
        .arg(modules("FAILS_missing_using"))
        .output()
        .expect("runs lilac");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E025]"), "stderr:\n{stderr}");
    // The loader's `using` span must render a source location (spec §10),
    // not just the headline: `main.lilac:1` on the `using missing` directive.
    assert!(
        stderr.contains("-->") && stderr.contains("main.lilac:1"),
        "E025 label has no source location:\n{stderr}"
    );
}

/// Two namespaces may each declare a same-named type and an identically shaped
/// top-level hook over their *own* type. The signatures are distinct once the
/// type names are namespace-qualified, so this must compile cleanly — never
/// ICE with a signatureless `DefId` (regression: unqualified `same_sig`).
#[test]
fn cross_namespace_same_named_hooks_do_not_ice() {
    let output = lilac()
        .arg("check")
        .arg(modules("two_ns_hooks"))
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_ne!(
        output.status.code(),
        Some(101),
        "compiler panicked:\n{stderr}"
    );
    assert!(
        output.status.success(),
        "expected a clean compile, stderr:\n{stderr}"
    );
    assert!(stderr.is_empty(), "stderr:\n{stderr}");
}

/// A genuinely duplicate hook signature across namespaces is `E020`, and the
/// driver must stop before checking bodies rather than ICE (defense in depth).
#[test]
fn duplicate_cross_namespace_hooks_are_e020_not_ice() {
    let output = lilac()
        .arg("check")
        .arg(modules("FAILS_dup_hooks"))
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_ne!(
        output.status.code(),
        Some(101),
        "compiler panicked:\n{stderr}"
    );
    assert_eq!(output.status.code(), Some(1), "stderr:\n{stderr}");
    assert!(stderr.contains("error[E020]"), "stderr:\n{stderr}");
}

/// A const name declared in two namespaces with different types resolves to the
/// const the *referring file* can see, not the first name match in program
/// order (regression: `find_const_type` ignored namespaces → spurious `E043`).
#[test]
fn cross_namespace_same_named_consts_resolve_per_file() {
    let output = lilac()
        .arg("check")
        .arg(modules("const_ns"))
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected a clean compile, stderr:\n{stderr}"
    );
}

/// A namespace whose name equals one of its object names must still emit the
/// fully-qualified member symbol (`foo.foo.bar`), not the mis-stripped
/// `foo.bar` (regression: `already_prefixed`).
#[test]
fn namespace_named_after_its_object_qualifies_member() {
    let ir = emit_ir("self_named");
    assert!(
        ir.contains("function @foo.foo.bar"),
        "missing `@foo.foo.bar` in:\n{ir}"
    );
}

/// An import-vs-import collision is `E029` labelled at the reference and both
/// `using` sites (spec §10), not at the competing declarations.
#[test]
fn ambiguous_import_labels_both_using_sites() {
    let output = lilac()
        .arg("check")
        .arg(modules("FAILS_ambiguous_using"))
        .output()
        .expect("runs lilac");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E029]"), "stderr:\n{stderr}");
    assert!(
        stderr.contains("main.lilac:1") && stderr.contains("main.lilac:2"),
        "E029 must label both `using` sites in main.lilac:\n{stderr}"
    );
    assert!(
        stderr.contains("main.lilac:5"),
        "E029 must label the ambiguous reference:\n{stderr}"
    );
}

/// Runs `emit-ir` over a project directory and returns its IR text.
fn emit_ir(name: &str) -> String {
    let output = lilac()
        .arg("emit-ir")
        .arg(modules(name))
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "{name} stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8 IR")
}

/// Two namespaces may declare same-named non-generic types: their layouts and
/// `@on_drop` hooks must stay distinct (Task 7 addendum).
#[test]
fn two_namespaces_same_named_types_do_not_collide() {
    let ir = emit_ir("two_ns_types");
    for needle in [
        "#a.Thing",
        "#b.Thing",
        "@a.Thing.@on_drop",
        "@b.Thing.@on_drop",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in:\n{ir}");
    }
}

/// Same-named generic objects in two namespaces must mangle to distinct
/// instances, and their instance methods must emit matching symbols without a
/// doubled namespace prefix (Task 7 addendum).
#[test]
fn two_namespaces_same_named_generics_do_not_collide() {
    let ir = emit_ir("two_ns_generics");
    for needle in [
        "#a.GenBox.1..bit32",
        "#b.GenBox.1..bit64",
        "@a.GenBox.1..bit32.get",
        "@b.GenBox.1..bit64.get",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in:\n{ir}");
    }
}

/// Returns the braced body following `header` (a `type #X {` or `function @f`).
fn block_after<'a>(ir: &'a str, header: &str) -> &'a str {
    let start = ir
        .find(header)
        .unwrap_or_else(|| panic!("missing `{header}` in:\n{ir}"));
    let rest = &ir[start..];
    let open = rest.find('{').expect("open brace");
    let close = rest[open..].find('}').expect("close brace");
    &rest[open..open + close]
}

/// Two namespaces defining the **same-named generic object with different
/// shapes** must instantiate from their own AST: each namespace's layout and
/// generic function body must match its declaration, never the other's.
/// Regression: `find_item` paired a def with the first same-named AST item in
/// program order, so a later namespace's instance was built from an earlier
/// namespace's members (previously an ICE / silently wrong IR).
#[test]
fn two_namespaces_same_named_generics_keep_their_own_shapes() {
    let ir = emit_ir("two_ns_gen_shapes");

    let a_layout = block_after(&ir, "type #a.GenBox.1..bit32");
    assert!(a_layout.contains("a_value: #bit32"), "{a_layout}");
    assert!(
        !a_layout.contains("b_value"),
        "a reused b's shape:\n{a_layout}"
    );

    let b_layout = block_after(&ir, "type #b.GenBox.1..bit64");
    assert!(b_layout.contains("b_value: #bit64"), "{b_layout}");
    assert!(b_layout.contains("b_extra: #bit64"), "{b_layout}");
    assert!(
        !b_layout.contains("a_value"),
        "b reused a's shape:\n{b_layout}"
    );

    // The generic `tag[T]` bodies differ per namespace (0x1 vs 0x2); each call
    // must fold from its own namespace's specialization.
    let main = block_after(&ir, "function @main");
    assert!(main.contains("const 0x1"), "{main}");
    assert!(
        !main.contains("const 0x2"),
        "main used b's tag body:\n{main}"
    );

    let b_use = block_after(&ir, "function @b_use");
    assert!(b_use.contains("const 0x2"), "{b_use}");
    assert!(
        !b_use.contains("const 0x1"),
        "b_use used a's tag body:\n{b_use}"
    );
}

/// An object-private member called from outside the object is `E026`, labelled
/// at the access site and the declaration.
#[test]
fn private_member_denied_is_e026() {
    let output = lilac()
        .arg("check")
        .arg(ui("FAILS_private_member.lilac"))
        .output()
        .expect("runs lilac");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E026]"), "stderr:\n{stderr}");
    assert!(
        stderr.contains("FAILS_private_member.lilac:16"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("FAILS_private_member.lilac:5"),
        "stderr:\n{stderr}"
    );
}

/// A `private` member is visible inside the owning object's own member bodies.
#[test]
fn private_member_allowed_inside_object() {
    let output = lilac()
        .arg("check")
        .arg(ui("private_member_ok.lilac"))
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr:\n{stderr}");
    assert!(stderr.is_empty(), "stderr:\n{stderr}");
}

/// A namespace-private function is checked and lowered with its own namespace
/// (typeck must descend into `ItemKind::Private`, not skip it).
#[test]
fn namespace_private_function_compiles() {
    let output = lilac()
        .arg("check")
        .arg(ui("private_namespace_ok.lilac"))
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr:\n{stderr}");
    assert!(stderr.is_empty(), "stderr:\n{stderr}");
}

/// An external static `Object.member()` call to a private member is `E026`.
#[test]
fn private_static_member_denied_is_e026() {
    let output = lilac()
        .arg("check")
        .arg(ui("FAILS_private_static_member.lilac"))
        .output()
        .expect("runs lilac");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E026]"), "stderr:\n{stderr}");
}

/// `Object.member()` to a private member is allowed inside the owning object.
#[test]
fn private_static_member_allowed_inside_object() {
    let output = lilac()
        .arg("check")
        .arg(ui("private_static_member_ok.lilac"))
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr:\n{stderr}");
    assert!(stderr.is_empty(), "stderr:\n{stderr}");
}

/// A field wrapped in nested `private { private { … } }` still resolves as an
/// object member field from inside the object (no spurious `E021`).
#[test]
fn nested_private_field_resolves_inside_object() {
    let output = lilac()
        .arg("check")
        .arg(ui("private_nested_ok.lilac"))
        .output()
        .expect("runs lilac");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr:\n{stderr}");
    assert!(stderr.is_empty(), "stderr:\n{stderr}");
}

/// External access to a field wrapped in nested `private` blocks is `E026`
/// (not a spurious missing-field `E021`).
#[test]
fn nested_private_field_denied_is_e026() {
    let output = lilac()
        .arg("check")
        .arg(ui("FAILS_private_nested_member.lilac"))
        .output()
        .expect("runs lilac");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E026]"), "stderr:\n{stderr}");
    assert!(!stderr.contains("error[E021]"), "stderr:\n{stderr}");
}

// --- Module project snapshots and end-to-end run -------------------------

fn modules_dir() -> PathBuf {
    Path::new("../../tests/ui/modules").to_path_buf()
}

/// Every committed snapshot directly under `tests/ui/modules` with extension
/// `ext`, sorted. These sit *beside* their project directory (the CLI maps a
/// directory argument to `<dir>.<ext>`).
fn module_snapshots(ext: &str) -> Vec<PathBuf> {
    let mut snapshots: Vec<PathBuf> = std::fs::read_dir(modules_dir())
        .expect("reads tests/ui/modules")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == ext))
        .collect();
    snapshots.sort();
    assert!(!snapshots.is_empty(), "no module `.{ext}` snapshots found");
    snapshots
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .expect("utf8 stem")
        .to_owned()
}

/// `emit-ir --check` over every module project snapshot. The snapshot's stem is
/// the project directory (`two_files.lir` checks `two_files/`), so a failing
/// project's stale snapshot is caught here.
#[test]
fn module_lir_snapshots_stable() {
    for snapshot in module_snapshots("lir") {
        let project = snapshot.with_extension("");
        assert!(
            project.is_dir(),
            "{} has no project directory",
            snapshot.display()
        );
        let output = lilac()
            .args(["emit-ir", "--check"])
            .arg(&project)
            .output()
            .expect("runs lilac");
        assert!(
            output.status.success(),
            "{}:\n{}",
            project.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// There is exactly one canonical-triple `.ll` per module `.lir`, and each one
/// is byte-stable under `emit-llvm --check`.
#[test]
fn module_ll_snapshots_stable() {
    let lir_stems: Vec<String> = module_snapshots("lir").iter().map(|p| stem(p)).collect();
    let ll = module_snapshots("ll");
    let ll_stems: Vec<String> = ll.iter().map(|p| stem(p)).collect();
    assert_eq!(
        ll_stems, lir_stems,
        "there must be exactly one `.ll` snapshot per module `.lir`"
    );

    for snapshot in ll {
        let project = snapshot.with_extension("");
        let output = lilac()
            .args([
                "emit-llvm",
                "--check",
                &format!("--target={CANONICAL_TARGET}"),
            ])
            .arg(&project)
            .output()
            .expect("runs lilac");
        assert!(
            output.status.success(),
            "{}:\n{}",
            project.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Non-root symbols carry their namespace prefix while root symbols stay bare:
/// `private_reach`'s `util/write_byte` is `@util.write_byte`, its private
/// `secret` is `@util.secret`, but the root `root_bump` is `@root_bump`, and no
/// bare `@write_byte`/`@secret` is emitted.
#[test]
fn module_symbols_are_namespace_prefixed() {
    let ir = emit_ir("private_reach");
    for needle in ["@util.write_byte", "@util.secret", "@root_bump"] {
        assert!(ir.contains(needle), "missing `{needle}` in:\n{ir}");
    }
    for bare in ["@write_byte", "@secret"] {
        assert!(!ir.contains(bare), "unprefixed `{bare}` in:\n{ir}");
    }
}

/// Locates the toolchain `clang`: `%LLVM_SYS_181_PREFIX%\bin\clang[.exe]`, else
/// `clang` on `PATH` (same lookup as `m5b.rs`).
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

/// A `private_reach` project whose root `main` reaches `util.write_byte` (which
/// itself reaches the root `root_bump` by ancestor reach and the namespace-
/// private `secret`) compiles to host IR, links with the toolchain `clang`, and
/// runs to exit 0.
#[test]
fn module_project_compiles_links_and_runs() {
    let clang = clang();
    let scratch = std::env::temp_dir().join(format!("lilac-m6-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("creates scratch dir");

    let output = lilac()
        .args(["emit-llvm", "--mode=debug"])
        .arg(modules("private_reach"))
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "emit-llvm failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let ll = scratch.join("private_reach.ll");
    std::fs::write(&ll, &output.stdout).expect("writes IR");

    let exe = scratch.join("private_reach.exe");
    let link = Command::new(&clang)
        .arg(&ll)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("runs clang");
    assert!(
        link.status.success(),
        "private_reach failed to link:\n{}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("runs executable");
    assert_eq!(
        run.status.code(),
        Some(0),
        "private_reach exited with {:?}",
        run.status.code()
    );

    let _ = std::fs::remove_dir_all(&scratch);
}
