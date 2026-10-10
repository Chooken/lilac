//! M6a UI tests: the CLI compiles a whole folder project and reports `E025`
//! for a `using` whose target folder is missing. Module fixtures also have
//! merged `.lir`/`.ll` snapshots and a clang end-to-end run.

mod support;

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

/// A hook in a child namespace referencing an ancestor's type has the same
/// resolved signature as the ancestor's own hook, so it must be `E020` — not
/// silently accepted and then emitted twice as `E070`. Regression: `same_sig`
/// qualified by the *referencing* namespace (`b.Base` vs `Base`).
#[test]
fn ancestor_type_hook_collision_is_e020_not_e070() {
    let output = lilac()
        .arg("check")
        .arg(modules("FAILS_hook_ancestor_type"))
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
    assert!(!stderr.contains("error[E070]"), "stderr:\n{stderr}");
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
    // One snippet of main.lilac carries all three labels.
    assert!(
        stderr.contains("1 | using a\n  | ------- imported here\n")
            && stderr.contains("2 | using b\n  | ------- imported here\n"),
        "E029 must label both `using` sites in main.lilac:\n{stderr}"
    );
    assert!(
        stderr.contains("--> main.lilac:5:17"),
        "E029 must label the ambiguous reference:\n{stderr}"
    );
}

/// An alias named like an item of its file's namespace (a sibling file's
/// included), or like an earlier alias, is `E054` at the alias.
#[test]
fn alias_collisions_are_e054() {
    let output = lilac()
        .arg("check")
        .arg(modules("FAILS_alias_collision"))
        .output()
        .expect("runs lilac");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("error[E054]").count(),
        2,
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("--> main.lilac:1:12") && stderr.contains("helpers.lilac:1:1"),
        "E054 must label the alias and the sibling file's item:\n{stderr}"
    );
    assert!(
        stderr.contains("--> main.lilac:3:12") && stderr.contains("first aliased here"),
        "E054 must label both aliases:\n{stderr}"
    );
}

/// A local shadows an alias; a failed member access on it says so.
#[test]
fn local_shadowing_alias_is_noted() {
    let output = lilac()
        .arg("check")
        .arg(modules("FAILS_alias_shadowed_by_local"))
        .output()
        .expect("runs lilac");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E010]"), "stderr:\n{stderr}");
    assert!(
        stderr.contains("alias `B` declared here")
            && stderr.contains("the local `B` shadows the alias `B`"),
        "the error must point at the shadowed alias:\n{stderr}"
    );
}

/// A misspelled path segment (a type, a function, a namespace, or an
/// alias at the head) gets a did-you-mean whose fix covers just that
/// segment.
#[test]
fn misspelled_path_segments_suggest_the_closest_name() {
    let output = lilac()
        .args(["check", "--error-format=json"])
        .arg(modules("FAILS_path_did_you_mean"))
        .output()
        .expect("runs lilac");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    for (line, col, end_col, replacement) in [
        (5, 15, 20, "Square"),
        (6, 24, 28, "area"),
        (7, 21, 27, "shapes"),
        (8, 17, 23, "Shapes"),
    ] {
        let fix = format!(
            r#""line":{line},"col":{col},"end_line":{line},"end_col":{end_col},"replacement":"{replacement}""#
        );
        assert!(stderr.contains(&fix), "missing {fix} in:\n{stderr}");
    }
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

    let b_use = block_after(&ir, "function @.root.b_use");
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
    // The private declaration is labelled in the same snippet.
    assert!(
        stderr.contains("\n 5 |") && stderr.contains("declared private here"),
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

// --- extern C ABI signatures (E028) --------------------------------------

/// An `extern` signature the C ABI cannot represent is `E028`: a `>1` out, or a
/// parameter/return that is neither a bit integer nor `ref T`. The diagnostic
/// must reach the user as an ordinary `error[E028]` — never the
/// `internal error: LLVM codegen failed:` framing — and must never
/// panic/signal (the `m4_regressions` contract).
#[test]
fn unsupported_extern_signatures_are_e028() {
    for fixture in [
        "FAILS_extern_multi_out.lilac",
        "FAILS_extern_aggregate_param.lilac",
        "FAILS_extern_aggregate_return.lilac",
    ] {
        for cmd in ["check", "emit-llvm"] {
            let output = lilac()
                .args([cmd, &ui(fixture)])
                .output()
                .expect("runs lilac");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_ne!(
                output.status.code(),
                Some(101),
                "{fixture} (`{cmd}`) panicked:\n{stderr}"
            );
            assert_eq!(
                output.status.code(),
                Some(1),
                "{fixture} (`{cmd}`) stderr:\n{stderr}"
            );
            assert!(
                stderr.contains("error[E028]"),
                "{fixture} (`{cmd}`) stderr:\n{stderr}"
            );
            assert!(
                !stderr.contains("internal error:"),
                "{fixture} (`{cmd}`) leaked the internal-error framing:\n{stderr}"
            );
        }
    }
}

/// An `extern` named `main` keeps the unmangled C symbol `main`, which collides
/// with the entry wrapper the LLVM backend emits (the backend looks up
/// `.lilac.main` and used to ICE). It must be a clean pre-codegen `E028` for
/// both `check` and `emit-llvm`, never a panic/signal.
#[test]
fn extern_named_main_is_e028() {
    let fixture = ui("FAILS_extern_main.lilac");
    for cmd in ["check", "emit-llvm"] {
        let output = lilac().args([cmd, &fixture]).output().expect("runs lilac");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_ne!(
            output.status.code(),
            Some(101),
            "extern `main` (`{cmd}`) panicked:\n{stderr}"
        );
        assert_eq!(
            output.status.code(),
            Some(1),
            "extern `main` (`{cmd}`) stderr:\n{stderr}"
        );
        assert!(
            stderr.contains("error[E028]"),
            "extern `main` (`{cmd}`) stderr:\n{stderr}"
        );
        assert!(
            !stderr.contains("internal error:"),
            "extern `main` (`{cmd}`) leaked the internal-error framing:\n{stderr}"
        );
    }
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

/// Every symbol carries its namespace prefix, the root's being `.root`:
/// `private_reach`'s `util/write_byte` is `@util.write_byte`, its private
/// `secret` is `@util.secret`, the root `root_bump` is `@.root.root_bump`,
/// and no bare `@write_byte`/`@secret`/`@root_bump` is emitted.
#[test]
fn module_symbols_are_namespace_prefixed() {
    let ir = emit_ir("private_reach");
    for needle in ["@util.write_byte", "@util.secret", "@.root.root_bump"] {
        assert!(ir.contains(needle), "missing `{needle}` in:\n{ir}");
    }
    for bare in ["@write_byte", "@secret", "@root_bump"] {
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
/// private `secret`) builds with the toolchain `clang` and runs to exit 0.
#[test]
fn module_project_compiles_links_and_runs() {
    let scratch = std::env::temp_dir().join(format!("lilac-m6-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("creates scratch dir");
    let exe = scratch.join("private_reach.exe");
    let run = support::build_and_run(
        &clang(),
        Path::new(&modules("private_reach")),
        &exe,
        &["--mode=debug"],
    );
    assert_eq!(
        run.status.code(),
        Some(0),
        "private_reach exited with {:?}",
        run.status.code()
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

/// A libc `write(1, ptr, len)` program builds against the host C runtime
/// with the toolchain `clang`, runs, and prints exactly the bytes of its
/// `@byte_buffer` (`"hi\n"`). This exercises the extern C-ABI call end to
/// end: `write`'s return value lands in its out place, and the pointer/length
/// arguments come from the user-callable `@byte_buffer_ptr`/`@byte_buffer_len`
/// intrinsics.
#[test]
fn libc_write_prints_bytes_to_stdout() {
    let scratch = std::env::temp_dir().join(format!("lilac-m6-extern-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("creates scratch dir");
    let exe = scratch.join("extern_write.exe");
    let run = support::build_and_run(
        &clang(),
        Path::new(&ui("extern_write.lilac")),
        &exe,
        &["--mode=debug"],
    );
    assert!(
        run.status.success(),
        "extern_write exited with {:?}; stderr:\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );

    // On Windows the linked C runtime's `write` is in text mode, so the `\n`
    // the program wrote arrives as `\r\n` on the captured pipe; normalize it
    // before asserting the program produced exactly `hi\n`. Gate the strip on
    // Windows so a stray `\r` on Unix is not masked.
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stdout = if cfg!(windows) {
        stdout.replace("\r\n", "\n")
    } else {
        stdout.into_owned()
    };
    assert_eq!(stdout, "hi\n", "stdout was {stdout:?}");

    let _ = std::fs::remove_dir_all(&scratch);
}

/// Builds `source` with the toolchain `clang`, runs the executable, and
/// returns its output. `name` disambiguates the scratch directory across the
/// tests in this binary.
fn link_and_run(name: &str, source: &str) -> std::process::Output {
    let scratch = std::env::temp_dir().join(format!("lilac-m6-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("creates scratch dir");
    let exe = scratch.join(if cfg!(windows) { "out.exe" } else { "out" });
    let run = support::build_and_run(&clang(), Path::new(source), &exe, &["--mode=debug"]);
    let _ = std::fs::remove_dir_all(&scratch);
    run
}

/// A `void`-returning (`0` out) extern links and runs: `srand(seed)` has no C
/// return value, so its LIR has no out place and the call must be `void`.
#[test]
fn extern_void_links_and_runs() {
    let run = link_and_run("extern_void", &ui("extern_void.lilac"));
    assert!(
        run.status.success(),
        "extern_void exited with {:?}",
        run.status.code()
    );
}

/// A `ref T`-returning extern (`malloc`) links and runs; its pointer flows back
/// into a `ref T` parameter of another extern (`free`), exercising the pointer
/// return and pointer parameter paths end to end.
#[test]
fn extern_ref_return_links_and_runs() {
    let run = link_and_run("extern_ptr", &ui("extern_ptr.lilac"));
    assert!(
        run.status.success(),
        "extern_ptr exited with {:?}",
        run.status.code()
    );
}
