# M5a — Session/target, portable LIR codec, extern IR, DCE — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the compiler the host/target + optimization + cache plumbing, a dependency-free serialized form of `LirModule`, real bodyless `extern` IR, and the post-fold dead-code sweep deferred from M3 — all without linking LLVM.

**Architecture:** No new pipeline stage. Session/opts is configuration; `extern` is a flag threaded HIR→LIR plus printer/verifier rules; DCE is one `lilac-ir` function the driver calls between `fold` and `verify`; the codec is a std-only binary format implemented over the existing `lilac-ir` types.

**Tech Stack:** Rust (std only — **no new crates, and specifically no `serde`**), existing workspace crates. Branch `rust-rewrite-m0`.

**Spec:** `docs/superpowers/specs/2026-10-04-lilac-rust-rewrite-m5a-design.md`

## Global Constraints

- **std only. No new dependencies, and no `serde`** — the language is intended to self-host; a Rust-only derive would not port.
- Gates after every task: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- No `unwrap`/`expect`/`panic!` on user input; `lilac_error::bug!` for ICEs only.
- TDD per task; one commit per task. Frontend crates never import `inkwell`/`llvm-sys`.
- `.lir` snapshots are LF and byte-exact via `emit-ir --check`; write them with `[IO.File]::WriteAllText($path, $text, (New-Object Text.UTF8Encoding $false))` (no BOM), never shell redirection.
- `IR_SPEC.md` and `SPEC.md` need no wording change (extern is already documented; targets/opts/codec are not language semantics).

## Review Focus

1. **Malformed/adversarial bytes to `LirModule::decode`** must return `CodecError`, never panic, loop, or allocate unboundedly. (Task 4)
2. **A module with no `main`** run through `prune_unreachable` must be left unchanged, not emptied. (Task 3)
3. **A function reached only through `addr.func`** (indirect, no direct `call`) must survive DCE. (Task 3)
4. **`-O`/`--release`/`--cache-dir` in odd orders or with bad values**: last optimization flag wins; unknown flags/values are rejected with exit 2. (Task 1)
5. **Extern misuse** (a parsed body, or a call with wrong arity/type) must be diagnosed, not silently accepted. (Task 2)

---

### Task 1: Session config, host detection, CLI flags

**Files:**
- Modify: `crates/lilac-driver/src/lib.rs`
- Modify: `crates/lilac-cli/src/main.rs`
- Test: `crates/lilac-driver/src/lib.rs` (`#[cfg(test)] mod tests`), `crates/lilac-cli/src/main.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing (first task).
- Produces:
  - `pub enum Opt { O0, O1, O2, O3, Os }` — `#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]` with `#[default] O0`; `pub fn Opt::parse(flag: &str) -> Option<Opt>` mapping `"-O0".."-O3"` and `"-Os"`.
  - `Session.optimize: Opt` and `Session.cache_dir: Option<std::path::PathBuf>`.
  - `Target::from_arch_os(arch: &str, os: &str) -> Target`; `Target::host()` delegates to it with `std::env::consts::{ARCH, OS}`.

- [ ] **Step 1: Write the failing driver tests**

Add to `crates/lilac-driver/src/lib.rs`'s test module:

```rust
#[test]
fn opt_parses_known_flags_only() {
    assert_eq!(Opt::parse("-O0"), Some(Opt::O0));
    assert_eq!(Opt::parse("-O2"), Some(Opt::O2));
    assert_eq!(Opt::parse("-O3"), Some(Opt::O3));
    assert_eq!(Opt::parse("-Os"), Some(Opt::Os));
    assert_eq!(Opt::parse("--release"), None);
    assert_eq!(Opt::default(), Opt::O0);
}

#[test]
fn target_maps_arch_and_os() {
    let win = Target::from_arch_os("x86_64", "windows");
    assert_eq!(win.triple, "x86_64-pc-windows-msvc");
    assert_eq!(win.bit_native_width(), 64);

    let linux32 = Target::from_arch_os("x86", "linux");
    assert_eq!(linux32.triple, "x86-unknown-linux-gnu");
    assert_eq!(linux32.bit_native_width(), 32);

    let mac = Target::from_arch_os("aarch64", "macos");
    assert_eq!(mac.triple, "aarch64-apple-darwin");
    assert_eq!(mac.bit_native_width(), 64);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lilac-driver opt_parses_known_flags_only target_maps_arch_and_os`
Expected: FAIL to compile — `Opt` and `Target::from_arch_os` don't exist.

- [ ] **Step 3: Implement `Opt`, the new `Session` fields, and host detection**

- `Opt::parse` returns `Some` only for the five flags; `Default` is `O0`.
- `Target::from_arch_os`: triple = `"{arch}-{vendor-os}"` where `os` maps `windows→pc-windows-msvc`, `linux→unknown-linux-gnu`, `macos→apple-darwin`, anything else → `unknown-unknown-unknown`. `bit_native_width()` = `64` for `x86_64`/`aarch64`, else `32`.
- `Target::host()` = `Self::from_arch_os(std::env::consts::ARCH, std::env::consts::OS)` (delete the hardcoded `x86_64-unknown-none`).
- `Session` gains `optimize: Opt` and `cache_dir: Option<PathBuf>`; `Session::new()` sets `host = target = Target::host()` (keep `warn_on_operator_type_change: true`).

- [ ] **Step 4: Run the driver tests to verify they pass**

Run: `cargo test -p lilac-driver`
Expected: PASS.

- [ ] **Step 5: Write the failing CLI-parse tests**

Add to `crates/lilac-cli/src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::{parse_common, Opt};

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn optimization_flags_last_wins() {
        let common = parse_common(&args(&["f.lilac", "-O1", "-O3"]), false).expect("valid args");
        assert_eq!(common.session.optimize, Opt::O3);
    }

    #[test]
    fn release_is_o2_and_cache_dir_parses() {
        let common = parse_common(
            &args(&["f.lilac", "--release", "--cache-dir=out/cache"]),
            false,
        )
        .expect("valid args");
        assert_eq!(common.session.optimize, Opt::O2);
        assert_eq!(
            common.session.cache_dir,
            Some(std::path::PathBuf::from("out/cache"))
        );
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(parse_common(&args(&["f.lilac", "-O9"]), false).is_err());
    }
}
```

If `ExitCode` does not implement `Debug` (so `.expect` won't compile), replace `.expect("valid args")` with `match ... { Ok(c) => c, Err(_) => panic!("expected valid args") }` inside the test.

- [ ] **Step 6: Run the CLI tests to verify they fail**

Run: `cargo test -p lilac-cli optimization_flags_last_wins`
Expected: FAIL (flags unhandled / unknown `--` rejected).

- [ ] **Step 7: Implement the CLI flags**

In `parse_common`, before the `arg.starts_with("--")` rejection arm, add: `-O0`/`-O1`/`-O2`/`-O3`/`-Os` set `session.optimize` via `Opt::parse`; `--release` sets `Opt::O2`; `--cache-dir=<path>` sets `session.cache_dir`. `-O9` and friends fall through to the unknown-flag error (exit 2). Update `usage()` to list `[-O0|-O1|-O2|-O3|-Os] [--release] [--cache-dir=<path>]` on the `check` and `emit-ir` lines.

- [ ] **Step 8: Run the full gates and commit**

Run: `cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace`
Expected: all PASS; `emit-ir --check` snapshots unchanged (none of this affects output).

```bash
git add crates/lilac-driver/src/lib.rs crates/lilac-cli/src/main.rs
git commit -m "M5a: Session optimize/cache_dir, real host target, -O/--release/--cache-dir flags"
```

---

### Task 2: Extern IR end-to-end

**Files:**
- Modify: `crates/lilac-hir/src/lib.rs` (`HirFunc`)
- Modify: `crates/lilac-typeck/src/check.rs:1020,1031`
- Modify: `crates/lilac-typeck/src/check/generics.rs:499,510`
- Modify: `crates/lilac-lower/src/lib.rs` (`lower_func`, around line 406)
- Modify: `crates/lilac-ir/src/lib.rs` (`LirFunc`, `print_func`, `verify_func`, all `LirFunc {…}` test literals)
- Create: `tests/ui/extern_decl.lilac`, `tests/ui/extern_decl.lir`
- Create: `tests/ui/FAILS_extern_arity.lilac`
- Create: `crates/lilac-cli/tests/m5a.rs`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces:
  - `HirFunc.is_extern: bool`; `LirFunc.is_extern: bool`.
  - `print_func` renders an extern as `extern @name` + `in`/`out` lines with **no** braces.
  - `Verifier` accepts externs with zero blocks and skips the "out never assigned" check for them.

- [ ] **Step 1: Thread `is_extern` through HIR and typeck**

Add `pub is_extern: bool` to `HirFunc`. Run `cargo check -p lilac-typeck -p lilac-hir` and set it at every construction site: `true` in the `CheckedBody::Extern` arms (`check.rs:1020`, `check/generics.rs:499`), `false` in the `CheckedBody::Block` arms and the `lilac-hir` test literal.

- [ ] **Step 2: Add `LirFunc.is_extern`, lower it, and print it**

Add `pub is_extern: bool` to `LirFunc`; run `cargo check -p lilac-lower -p lilac-ir` and set `false` at every non-extern `LirFunc {…}` literal (the `lilac-ir` and `lilac-const` test modules). In `lower_func`, when `func.is_extern`, push `LirFunc { name, ins, outs, is_extern: true, blocks: Vec::new() }` and return early (do not lower the empty body). In `print_func`, if `is_extern`, emit `extern @{name}` then the `in`/`out` lines and return — no `{`.

- [ ] **Step 3: Add the verifier rule and the failing unit tests**

In `verify_func`, first branch on `func.is_extern`: if true, an extern must have **zero** blocks (otherwise `E070`); skip the block walk and the "out place never assigned" check. A non-extern keeps the existing "has no blocks" error.

Add to the `lilac-ir` test module:

```rust
#[test]
fn printer_emits_bodyless_extern() {
    let module = LirModule {
        version: LILAC_IR_VERSION.to_owned(),
        target: "x86_64-unknown-none".to_owned(),
        data: Vec::new(),
        funcs: vec![LirFunc {
            name: "puts".to_owned(),
            ins: vec![("s".to_owned(), "#byte_buffer".to_owned())],
            outs: Vec::new(),
            is_extern: true,
            blocks: Vec::new(),
        }],
        layouts: HashMap::new(),
    };
    let text = module.to_text();
    assert!(text.contains("extern @puts\n  in #byte_buffer %s\n"), "{text}");
    assert!(!text.contains("{\n"), "extern must print no body: {text}");
    let mut diags = lilac_error::Diags::new();
    assert!(verify(&module, &mut diags).is_ok(), "clean extern verifies");
}

#[test]
fn verifier_rejects_extern_with_block() {
    let mut module = canonical_module();
    module.funcs[0].is_extern = true;
    let mut diags = lilac_error::Diags::new();
    assert!(verify(&module, &mut diags).is_err());
    assert_eq!(e070_count(&diags), 1);
}
```

- [ ] **Step 4: Run the unit tests to verify they fail, then pass**

Run: `cargo test -p lilac-ir printer_emits_bodyless_extern verifier_rejects_extern_with_block`
Expected: initially FAIL (no extern support) → after Steps 1–3, PASS.

- [ ] **Step 5: Add the fixtures**

`tests/ui/extern_decl.lilac`:

```lilac
puts: extern function (s: @byte_buffer) nothing

main: function () nothing {
    puts("hi")
}
```

`tests/ui/FAILS_extern_arity.lilac` (calls `puts` with the wrong arity/type):

```lilac
puts: extern function (s: @byte_buffer) nothing

main: function () nothing {
    puts()
}
```

- [ ] **Step 6: Generate `extern_decl.lir` and check it**

Run: `cargo run -q -p lilac-cli -- emit-ir tests/ui/extern_decl.lilac`
Confirm the output contains `extern @puts` with no braces; write it to `tests/ui/extern_decl.lir` with the no-BOM/LF method in Global Constraints. Then `cargo run -q -p lilac-cli -- emit-ir --check tests/ui/extern_decl.lilac` → exit 0.

- [ ] **Step 7: Create the M5a harness test**

`crates/lilac-cli/tests/m5a.rs` mirrors `m4.rs`:

```rust
//! M5a UI tests: extern IR, stale-snapshot guard, DCE, and codec round-trips.

use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

fn ui(name: &str) -> String {
    format!("../../tests/ui/{name}")
}

const PORTS: &[&str] = &["extern_decl.lilac"];

#[test]
fn check_ports_clean() {
    for port in PORTS {
        let output = lilac().args(["check", &ui(port)]).output().expect("runs lilac");
        assert!(output.status.success(), "{port}:\n{}", String::from_utf8_lossy(&output.stderr));
    }
}

#[test]
fn lir_snapshots_stable() {
    let mut snapshots: Vec<std::path::PathBuf> = std::fs::read_dir("../../tests/ui")
        .expect("reads tests/ui")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "lir"))
        .collect();
    snapshots.sort();
    for snapshot in snapshots {
        let source = snapshot.with_extension("lilac");
        let output = lilac().args(["emit-ir", "--check"]).arg(&source).output().expect("runs lilac");
        assert!(output.status.success(), "{}:\n{}", source.display(), String::from_utf8_lossy(&output.stderr));
    }
}

#[test]
fn extern_arity_mismatch_is_diagnosed() {
    let output = lilac().args(["check", &ui("FAILS_extern_arity.lilac")]).output().expect("runs lilac");
    assert!(!output.status.success(), "wrong-arity extern call must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("puts"), "{stderr}");
}
```

- [ ] **Step 8: Run the full gates and commit**

Run: `cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace`
Expected: all PASS.

```bash
git add crates/lilac-hir/src/lib.rs crates/lilac-typeck/src/check.rs \
  crates/lilac-typeck/src/check/generics.rs crates/lilac-lower/src/lib.rs \
  crates/lilac-ir/src/lib.rs crates/lilac-cli/tests/m5a.rs \
  tests/ui/extern_decl.lilac tests/ui/extern_decl.lir tests/ui/FAILS_extern_arity.lilac
git commit -m "M5a: emit bodyless extern declarations through HIR/lower/IR, with verifier rule"
```

---

### Task 3: Post-fold dead-code sweep

**Files:**
- Create: `crates/lilac-ir/src/dce.rs`
- Modify: `crates/lilac-ir/src/lib.rs` (`mod dce; pub use dce::prune_unreachable;`)
- Modify: `crates/lilac-driver/src/lib.rs` (`run_pipeline`, after `lilac_const::fold`)
- Modify: `tests/ui/*.lir` (regenerate those that lose dead items)
- Test: `crates/lilac-ir/src/dce.rs` (`#[cfg(test)] mod tests`), `crates/lilac-cli/tests/m5a.rs`

**Interfaces:**
- Consumes: `LirFunc.is_extern` (Task 2) — externs are swept like any unreferenced function.
- Produces: `pub fn prune_unreachable(module: &mut LirModule)`.

- [ ] **Step 1: Write the failing unit tests**

In `crates/lilac-ir/src/dce.rs`'s test module, build small modules (reuse a local `fn func(name, calls: &[&str], addr: &[&str]) -> LirFunc` helper):

```rust
#[test]
fn sweeps_unreachable_funcs_and_data() {
    // main -> a; b is dead; !c0 is referenced only by b's Const.
    let mut module = module_with(&["main", "a", "b"], &["c0"]);
    prune_unreachable(&mut module);
    let names: Vec<&str> = module.funcs.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["main", "a"]);
    assert!(module.data.iter().all(|d| d.name != "c0"));
}

#[test]
fn addr_func_target_survives() {
    // main takes `addr.func @only_indirect` but never `call`s it.
    let mut module = module_with(&["main", "only_indirect"], &[]);
    prune_unreachable(&mut module);
    let names: Vec<&str> = module.funcs.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["main", "only_indirect"]);
}

#[test]
fn no_main_is_a_noop() {
    let mut module = module_with(&["a", "b"], &[]);
    let before = module.funcs.len();
    prune_unreachable(&mut module);
    assert_eq!(module.funcs.len(), before);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lilac-ir dce`
Expected: FAIL to compile — `prune_unreachable` doesn't exist.

- [ ] **Step 3: Implement `prune_unreachable`**

- Roots: every function named `main`. If none exist, return without changing the module.
- Worklist over function **names**: for each visited function, scan every block/instruction for `LirInst::Call { func: LirCallee::Func(name) }` and `LirInst::AddrFunc { func: name }`; push unseen names that exist in the module.
- Remove `funcs` whose name was never visited, preserving order.
- Collect referenced data names from surviving instructions: `LirInst::Const { lit }` where `lit` starts with `'!'` → `lit[1..]`; `LirTerm::Panic { msg }` → `msg`. Remove `data` entries not in that set, preserving order.
- Leave `layouts` untouched (verifier-only, never printed).

- [ ] **Step 4: Run the unit tests to verify they pass**

Run: `cargo test -p lilac-ir dce`
Expected: PASS.

- [ ] **Step 5: Wire the driver and regenerate snapshots**

In `run_pipeline`, insert `lilac_ir::prune_unreachable(&mut lir);` between `lilac_const::fold(...)` and `lilac_ir::verify(...)`. Run `cargo test -p lilac-cli lir_snapshots_stable`; for each failure, inspect that the only diff is removed dead functions/data, then regenerate that `.lir` with the no-BOM/LF method.

- [ ] **Step 6: Add the DCE integration test**

Append to `crates/lilac-cli/tests/m5a.rs`:

```rust
#[test]
fn dce_removes_dead_after_fold() {
    // An uncalled helper has no path from `main` and is swept.
    let source = "dead: function () @bit32 => @as_int(1)\n\
                  main: function () nothing {\n\
                      x: @bit32 = @as_int(2)\n\
                  }\n";
    let mut session = lilac_driver::Session::new();
    let module = session.compile_lir("dce_probe.lilac", source).expect("compiles");
    assert!(module.funcs.iter().all(|f| f.name != "dead"), "{:?}", module.funcs.iter().map(|f| &f.name).collect::<Vec<_>>());
}
```

Adjust the fixture if `@as_int` on a literal does not fold as expected — the point is a function with no surviving callers.

- [ ] **Step 7: Run the full gates and commit**

Run: `cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace`
Expected: all PASS.

```bash
git add crates/lilac-ir/src/dce.rs crates/lilac-ir/src/lib.rs \
  crates/lilac-driver/src/lib.rs crates/lilac-cli/tests/m5a.rs tests/ui
git commit -m "M5a: post-fold reachability sweep removes dead functions and data"
```

---

### Task 4: Portable binary LIR codec (no serde)

**Files:**
- Create: `crates/lilac-ir/src/codec.rs`
- Modify: `crates/lilac-ir/src/lib.rs` (`mod codec; pub use codec::{CodecError, LIR_CODEC_VERSION};`)
- Test: `crates/lilac-ir/src/codec.rs` (`#[cfg(test)] mod tests`), `crates/lilac-cli/tests/m5a.rs`

**Interfaces:**
- Consumes: the `lilac-ir` types (including `LirFunc.is_extern` from Task 2).
- Produces:
  - `pub const LIR_CODEC_VERSION: u32 = 1;`
  - `impl LirModule { pub fn encode(&self, w: &mut impl std::io::Write) -> std::io::Result<()>; pub fn decode(r: &mut impl std::io::Read) -> Result<LirModule, CodecError>; pub fn content_hash(&self) -> u64; }`
  - `pub enum CodecError { Io(std::io::Error), Malformed(&'static str) }` implementing `Debug`/`Display`/`std::error::Error`, with `From<std::io::Error>`.

**Wire format (all integers little-endian):**
- Header: magic `b"LILACIR\0"`, `u32` codec version, `u32`-length UTF-8 `version` string, `u32`-length UTF-8 `target` string.
- Strings: `u32` byte length + UTF-8 bytes. Sequences: `u32` count + elements.
- `Span`: `file u32, lo u32, hi u32`. `bool`: one `u8` (0/1). `Option<T>`: tag `u8` (0=None,1=Some) + payload.
- `LirDataKind`: tag `u8` (0=Str,1=NumLit) + string.
- `LirOperand`/`LirCallee`: tag `u8` + string. `ArithOp`/`ConvertOp`: tag `u8`.
- `LirInst`/`LirTerm`: tag `u8` + fields, in declaration order — an **exhaustive** match so the compiler flags any missed variant.
- `layouts`: `u32` count, entries **sorted by key**, each = key string + `u32` member count + `(name,ty)` string pairs.

- [ ] **Step 1: Write the failing codec tests**

In `crates/lilac-ir/src/codec.rs`'s test module:

```rust
fn sample() -> LirModule {
    // Reuse crate::tests' canonical_module shape, plus a `type #Number` layout
    // and a data const, so data/layout round-tripping is exercised.
    let mut module = crate::tests::canonical_module();
    module.data.push(LirData {
        name: "c0".to_owned(),
        kind: LirDataKind::Str("hi".to_owned()),
        span: Span::dummy(),
    });
    module.layouts.insert(
        "#Number".to_owned(),
        vec![("tag".to_owned(), "#bit8".to_owned()), ("val".to_owned(), "#bit32".to_owned())],
    );
    module
}

#[test]
fn round_trips_to_identical_text() {
    let module = sample();
    let mut bytes = Vec::new();
    module.encode(&mut bytes).expect("encodes");
    let decoded = LirModule::decode(&mut &bytes[..]).expect("decodes");
    assert_eq!(decoded.to_text(), module.to_text());
}

#[test]
fn hash_is_stable_and_layout_order_independent() {
    let a = sample();
    let mut b = sample();
    // Reinsert the layout after an unrelated key to perturb HashMap order.
    b.layouts.insert("#Extra".to_owned(), Vec::new());
    b.layouts.remove("#Extra");
    assert_eq!(a.content_hash(), b.content_hash());
    assert_eq!(a.content_hash(), a.content_hash());
}

#[test]
fn malformed_input_errors_without_panicking() {
    // At least 8 bytes so the magic is read and rejected (a short read is `Io`).
    assert!(matches!(
        LirModule::decode(&mut &b"XXXXXXXXgarbage"[..]),
        Err(CodecError::Malformed(_))
    ));
    let mut bytes = Vec::new();
    sample().encode(&mut bytes).expect("encodes");
    bytes.truncate(bytes.len() / 2);
    assert!(LirModule::decode(&mut &bytes[..]).is_err());
}
```

`crate::tests::canonical_module` is private to `lib.rs`; make it `pub(crate)` (and its `test_span` helper if needed) so `codec::tests` can call it, or duplicate a small module inline.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lilac-ir codec`
Expected: FAIL to compile — `codec` module doesn't exist.

- [ ] **Step 3: Implement the codec**

Add `codec.rs` with `put_u32`/`get_u32`/`put_str`/`get_str`/`put_span`/`get_span` helpers over `std::io::{Read, Write}` (use `read_exact`, map EOF to `CodecError::Io`). Implement `encode`, `decode`, and `content_hash` (FNV-1a 64-bit over the encoded bytes; document it as a cache key, not a security hash). `decode` rejects bad magic/version/discriminant tags with `CodecError::Malformed`. Register the module in `lib.rs`.

- [ ] **Step 4: Run the unit tests to verify they pass**

Run: `cargo test -p lilac-ir codec`
Expected: PASS.

- [ ] **Step 5: Add the drop-in integration round-trip test**

Append to `crates/lilac-cli/tests/m5a.rs`:

```rust
#[test]
fn every_known_good_lir_round_trips() {
    let mut sources: Vec<std::path::PathBuf> = std::fs::read_dir("../../tests/ui")
        .expect("reads tests/ui")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "lir"))
        .map(|p| p.with_extension("lilac"))
        .collect();
    sources.sort();
    assert!(!sources.is_empty());
    for source in sources {
        let name = source.file_name().and_then(|s| s.to_str()).expect("utf8").to_owned();
        let src = std::fs::read_to_string(&source).expect("read source");
        let mut session = lilac_driver::Session::new();
        let module = session.compile_lir(&name, &src).expect("compiles");
        let mut bytes = Vec::new();
        module.encode(&mut bytes).expect("encodes");
        let decoded = lilac_ir::LirModule::decode(&mut &bytes[..]).expect("decodes");
        assert_eq!(decoded.to_text(), module.to_text(), "{name} round-trip");
        assert_eq!(decoded.content_hash(), module.content_hash(), "{name} hash");
    }
}
```

`lilac-driver` and `lilac-ir` are already dependencies of `lilac-cli` (used by the binary), so integration tests can use them and **no `Cargo.toml` change is needed**.

- [ ] **Step 6: Run the full gates and commit**

Run: `cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace`
Expected: all PASS.

```bash
git add crates/lilac-ir/src/codec.rs crates/lilac-ir/src/lib.rs crates/lilac-cli/tests/m5a.rs
git commit -m "M5a: dependency-free binary LIR codec (encode/decode/content_hash)"
```

---

## Final verification (after all tasks)

- [ ] `cargo fmt --check` clean.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [ ] `cargo test --workspace` green, including every `.lir` snapshot byte-stable via `emit-ir --check`.
- [ ] `grep -r inkwell crates/lilac-{span,error,lex,ast,parse,resolve,ty,hir,typeck,ir,const,lower}` is empty.
- [ ] `Cargo.toml`/`Cargo.lock` show **no new external crate**.
- [ ] Confirm each Review Focus item has a passing test.

## Self-Review

- **Spec coverage:** §3.1 → Task 1; §3.2 → Task 4; §3.3 → Task 2; §3.4 → Task 3; §4 (no doc changes) → Global Constraints; §5 tests → per-task tests + `m5a.rs`; §6 exit → Final verification.
- **Type consistency:** `Opt`, `Session.optimize`/`cache_dir`, `Target::from_arch_os`, `HirFunc.is_extern`, `LirFunc.is_extern`, `prune_unreachable`, `LIR_CODEC_VERSION`, `CodecError` are named identically across tasks and the spec.
- **Review Focus:** each of the five lines names its owning task's test (Task 1 ×2 flags, Task 2 extern misuse, Task 3 no-main + `AddrFunc`, Task 4 malformed bytes).
- **Proportion:** decisions (wire format, DCE roots/order, extern shape, host mapping, last-wins) are pinned; bodies are left to the implementer except where the spec fixes exact values.
