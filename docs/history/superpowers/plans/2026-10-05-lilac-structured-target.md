# Structured Target (`lilac-target`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the stringly-typed target with a Zig-style `Target { arch, vendor, os, abi }` of `Copy` enums in a new leaf crate `lilac-target`, parsed strictly from `--target=`, while the `.lir` header and LLVM backend keep receiving a canonical triple string.

**Architecture:** A new std-only crate `lilac-target` owns target identity, canonical triple rendering, host/default detection, and the single pointer-width rule (`is_64_bit_triple` moves there from `lilac-ir`). `lilac-driver` depends on it and re-exports `Target`; `lilac-cli` parses `--target=` through `Target::parse`; `lilac-codegen-llvm` imports the width rule from `lilac-target`. No codec or snapshot change.

**Tech Stack:** Rust (edition 2021), workspace lints (`unsafe_code = "forbid"`, clippy `all`+`pedantic` deny). No new external dependencies.

**Spec:** `docs/superpowers/specs/2026-10-05-lilac-structured-target-design.md`

## Global Constraints

- Rust std only; no new external dependency (`lilac-target` is a leaf crate).
- Workspace lints inherit: `unsafe_code = "forbid"`; `clippy::all` and `clippy::pedantic` deny (`large_enum_variant`/`missing_errors_doc`/`must_use_candidate` allowed).
- No `unwrap`/`expect`/`panic!` on user input; `bug!` only for ICEs (the driver/CLI error paths use `Result`).
- `inkwell`/`llvm-sys` stay confined to `lilac-codegen-llvm`.
- No committed `.lir`/`.ll` snapshot changes; no `LIR_CODEC_VERSION` bump.
- Gates before each commit: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`.
- TDD per task; one commit per task. Work on branch `rust-rewrite-m0`.

## Review Focus

Input classes the spec implies that are most likely to bite a user (each pinned by a test in the task that owns the code):

1. **Vendor-omitted 3-component triples** (`x86_64-linux-gnu`) must parse as arch-os-abi, not be misread or rejected. — Task 1, Task 2.
2. **`unknown` as both vendor and os** (`x86_64-unknown-none` vs `x86_64-unknown-gnu`) must resolve vendor-os first. — Task 1.
3. **`Os::Macos` renders `darwin`**, preserving `aarch64-apple-darwin` byte-for-byte. — Task 1.
4. **Width for non-`x86_64`/`aarch64` 64-bit arches** (`riscv64`) and agreement across every `Arch` variant. — Task 1, Task 2.
5. **Strict rejection surfaces at the CLI as exit 2**, not a late LLVM error (`--target=foo`). — Task 2.

---

### Task 1: Create the `lilac-target` crate

**Files:**
- Create: `crates/lilac-target/Cargo.toml`
- Create: `crates/lilac-target/src/lib.rs` (includes the unit tests)
- Workspace: no edit — root `Cargo.toml` uses `members = ["crates/*"]`

**Interfaces:**
- Consumes: nothing (leaf crate).
- Produces (used by Tasks 2 and 3):
  - `pub struct Target { pub arch: Arch, pub vendor: Vendor, pub os: Os, pub abi: Abi }` deriving `Copy, Clone, PartialEq, Eq, Hash, Debug`.
  - `pub enum Arch { X86, X86_64, Arm, Aarch64, Riscv32, Riscv64, Wasm32, Wasm64 }` with `pub fn parse(s: &str) -> Option<Self>`, `pub fn as_str(self) -> &'static str`, `pub fn is_64_bit(self) -> bool`.
  - `pub enum Vendor { Unknown, Pc, Apple }`, `pub enum Os { None, Unknown, Linux, Windows, Macos }`, `pub enum Abi { None, Gnu, Msvc, Musl }` — each with `parse(&str) -> Option<Self>` and `as_str(self) -> &'static str` (all derive the same traits).
  - `pub enum ParseError { Empty, TooManyComponents, UnknownArch(String), UnknownVendor(String), UnknownOs(String), UnknownAbi(String) }` implementing `std::fmt::Display`.
  - `impl Target { pub fn parse(s: &str) -> Result<Self, ParseError>; pub fn to_triple(&self) -> String; pub fn host() -> Self; pub fn bit_native_width(&self) -> u32 }` and `impl Default for Target` (= `X86_64/Unknown/None/None`).
  - `pub fn is_64_bit_triple(s: &str) -> bool`.

- [ ] **Step 1: Create the crate manifest**

`crates/lilac-target/Cargo.toml`:

```toml
[package]
name = "lilac-target"
version.workspace = true
edition.workspace = true

[dependencies]

[lints]
workspace = true
```

- [ ] **Step 2: Write the failing tests**

In `crates/lilac-target/src/lib.rs`, add a `#[cfg(test)] mod tests` with these tests (the module will not compile until Step 4 defines the types — that is the RED):

```rust
#[test]
fn round_trips_canonical_triples() {
    for t in [
        "x86_64-unknown-none",
        "x86_64-pc-windows-msvc",
        "x86-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "riscv32-unknown-none",
    ] {
        let parsed = Target::parse(t).unwrap_or_else(|e| panic!("{t}: {e}"));
        assert_eq!(parsed.to_triple(), t, "{t}");
        assert_eq!(Target::parse(&parsed.to_triple()).expect("reparse"), parsed, "{t}");
    }
}

#[test]
fn vendor_omitted_forms_normalize() {
    // Vendor-omitted inputs parse correctly and render the canonical vendor.
    assert_eq!(
        Target::parse("x86_64-linux-gnu").expect("valid").to_triple(),
        "x86_64-unknown-linux-gnu"
    );
    assert_eq!(
        Target::parse("x86_64-linux").expect("valid").to_triple(),
        "x86_64-unknown-linux"
    );
}

#[test]
fn unknown_is_vendor_os_first() {
    let t = Target::parse("x86_64-unknown-none").expect("valid");
    assert_eq!((t.vendor, t.os), (Vendor::Unknown, Os::None));
    assert_eq!(t.to_triple(), "x86_64-unknown-none");
}

#[test]
fn macos_renders_darwin() {
    let t = Target::parse("aarch64-apple-darwin").expect("valid");
    assert_eq!(t.os, Os::Macos);
    assert_eq!(t.to_triple(), "aarch64-apple-darwin");
}

#[test]
fn rejects_unknown_and_malformed() {
    assert!(matches!(Target::parse("foo"), Err(ParseError::UnknownArch(_))));
    assert!(matches!(Target::parse("x86_64-foo-linux"), Err(ParseError::UnknownVendor(_))));
    assert!(matches!(Target::parse("x86_64-linux-foo"), Err(ParseError::UnknownAbi(_))));
    assert!(matches!(Target::parse("a-b-c-d-e"), Err(ParseError::TooManyComponents)));
    assert!(matches!(Target::parse(""), Err(ParseError::Empty)));
}

#[test]
fn width_agrees_for_every_arch() {
    for arch in [
        Arch::X86, Arch::X86_64, Arch::Arm, Arch::Aarch64,
        Arch::Riscv32, Arch::Riscv64, Arch::Wasm32, Arch::Wasm64,
    ] {
        let t = Target { arch, vendor: Vendor::Unknown, os: Os::None, abi: Abi::None };
        assert_eq!(
            is_64_bit_triple(&t.to_triple()),
            arch.is_64_bit(),
            "{arch:?}"
        );
        assert_eq!(t.bit_native_width(), if arch.is_64_bit() { 64 } else { 32 });
    }
    assert_eq!(is_64_bit_triple("riscv64-unknown-none"), true);
    assert_eq!(is_64_bit_triple("riscv32-unknown-none"), false);
}

#[test]
fn default_is_canonical_bare_metal() {
    assert_eq!(Target::default().to_triple(), "x86_64-unknown-none");
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p lilac-target`
Expected: FAIL to compile (`Target`, `Arch`, … not found).

- [ ] **Step 4: Implement the types and functions**

Define the structs/enums and the derives from the Interfaces block. Then:

- `Arch::is_64_bit(self)` → `matches!(self, Arch::X86_64 | Arch::Aarch64 | Arch::Riscv64 | Arch::Wasm64)`.
- `Target::to_triple(&self)` → `arch-vendor-os`, appending `-abi` only when `self.abi != Abi::None` (so `aarch64-apple-darwin` stays 3 components).
- `Target::bit_native_width(&self)` → `if self.arch.is_64_bit() { 64 } else { 32 }`.
- `Target::default()` → `{ Arch::X86_64, Vendor::Unknown, Os::None, Abi::None }`.
- `Target::host()` → match `std::env::consts::ARCH` (`x86_64`→`X86_64`, `x86`→`X86`, `aarch64`→`Aarch64`, `arm`→`Arm`) and `std::env::consts::OS` (`windows`→`{Pc, Windows, Msvc}`, `linux`→`{Unknown, Linux, Gnu}`, `macos`→`{Apple, Macos, None}`); any other arch or os returns `Target::default()`.
- `is_64_bit_triple(s)` → take the component before the first `-`; if `Arch::parse` yields `Some`, return `is_64_bit()`, else `a.contains("64")`.

`Target::parse` (the non-obvious algorithm — implement exactly):

```rust
let parts: Vec<&str> = s.split('-').collect();
if s.is_empty() { return Err(ParseError::Empty); }
if parts.len() > 4 { return Err(ParseError::TooManyComponents); }
let arch = Arch::parse(parts[0]).ok_or_else(|| ParseError::UnknownArch(parts[0].to_owned()))?;
match parts.len() {
    1 => Ok(Target { arch, vendor: Vendor::Unknown, os: Os::None, abi: Abi::None }),
    2 => {
        let os = Os::parse(parts[1]).ok_or_else(|| ParseError::UnknownOs(parts[1].to_owned()))?;
        Ok(Target { arch, vendor: Vendor::Unknown, os, abi: Abi::None })
    }
    3 => {
        if let (Some(v), Some(o)) = (Vendor::parse(parts[1]), Os::parse(parts[2])) {
            Ok(Target { arch, vendor: v, os: o, abi: Abi::None })
        } else if let Some(o) = Os::parse(parts[1]) {
            let abi = Abi::parse(parts[2]).ok_or_else(|| ParseError::UnknownAbi(parts[2].to_owned()))?;
            Ok(Target { arch, vendor: Vendor::Unknown, os: o, abi })
        } else {
            Err(ParseError::UnknownVendor(parts[1].to_owned()))
        }
    }
    _ => {
        let vendor = Vendor::parse(parts[1]).ok_or_else(|| ParseError::UnknownVendor(parts[1].to_owned()))?;
        let os = Os::parse(parts[2]).ok_or_else(|| ParseError::UnknownOs(parts[2].to_owned()))?;
        let abi = Abi::parse(parts[3]).ok_or_else(|| ParseError::UnknownAbi(parts[3].to_owned()))?;
        Ok(Target { arch, vendor, os, abi })
    }
}
```

`ParseError`'s `Display` names the offending value (e.g. `unknown architecture \`foo\``, `too many target components`, `empty target`).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p lilac-target`
Expected: PASS (all seven tests).

- [ ] **Step 6: Gates and commit**

```bash
cargo fmt --check
cargo clippy -p lilac-target --all-targets -- -D warnings
git add crates/lilac-target
git commit -m "Add lilac-target crate with structured Target"
```

---

### Task 2: Adopt the typed target in `lilac-driver` and `lilac-cli`

**Files:**
- Modify: `crates/lilac-driver/Cargo.toml` (add `lilac-target` dependency)
- Modify: `crates/lilac-driver/src/lib.rs` (delete the `Target` struct/impl/`Default`, lines ~17-69; re-export the crate type; use `to_triple()`; update tests)
- Modify: `crates/lilac-cli/src/main.rs` (`--target=` parses via `Target::parse`; `emit-llvm` host default uses `to_triple()`; update/add tests)

**Interfaces:**
- Consumes: `lilac_target::Target` and its `parse`/`to_triple`/`host`/`bit_native_width` (Task 1).
- Produces: `lilac_driver::Target` re-exported (`pub use lilac_target::Target;`); `Session.target`/`Session.host: lilac_target::Target`.

- [ ] **Step 1: Add the dependency and re-export**

Add to `crates/lilac-driver/Cargo.toml` under `[dependencies]`:

```toml
lilac-target = { path = "../lilac-target" }
```

In `crates/lilac-driver/src/lib.rs`, delete the `Target` struct, its `impl`, and `impl Default for Target`; add `pub use lilac_target::Target;` near the other `use` items.

- [ ] **Step 2: Rewire the pipeline and update driver tests**

- `run_pipeline`: replace `let triple = self.target.triple.clone();` with `let triple = self.target.to_triple();`.
- Update `format_args_stride_follows_target_width` to build the target via
  `session.target = Target::parse(triple).expect("valid triple");` (drop the old struct literal).
- Replace `target_maps_arch_and_os` (which used `from_arch_os`) with a driver test that asserts `Target::host().to_triple()` is non-empty and that `Target::parse("x86_64-pc-windows-msvc").expect("valid").bit_native_width() == 64` — the detailed host/parse/width coverage lives in Task 1.

- [ ] **Step 3: Rewire the CLI and update its tests**

In `crates/lilac-cli/src/main.rs`:

- Import `Target` from `lilac_driver` (e.g. `use lilac_driver::{ErrorFormat, Opt, Session, Target};`).
- Replace the `--target=` arm with:

```rust
} else if let Some(value) = arg.strip_prefix("--target=") {
    match Target::parse(value) {
        Ok(target) => {
            session.target = target;
            target_set = true;
        }
        Err(err) => {
            eprintln!("invalid --target={value}: {err}");
            return Err(ExitCode::from(2));
        }
    }
}
```

- In `cmd_emit_llvm`, replace the triple-selection block with a `Copy` of the typed target, then render it once (no re-parsing):

```rust
let target = if common.target_set { session.target } else { session.host };
session.target = target;
let triple = target.to_triple();
```

`LlvmBackend::new(&triple, opt_level(session.optimize))` is unchanged.
- Update `target_flag_overrides_target_not_host` to use a valid triple (e.g. `--target=x86_64-pc-windows-msvc`) and assert `common.session.target.to_triple() == "x86_64-pc-windows-msvc"` and `common.session.host.to_triple() == lilac_driver::Target::host().to_triple()`.
- Add a new test `unknown_target_is_rejected` asserting `parse_common(&args(&["f.lilac", "--target=foo"]), false).is_err()`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p lilac-driver -p lilac-cli`
Expected: PASS.

- [ ] **Step 5: Gates and commit**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add crates/lilac-driver crates/lilac-cli
git commit -m "Adopt structured Target in driver and CLI"
```

---

### Task 3: Retire `lilac-ir::is_64_bit_triple` and update docs

**Files:**
- Modify: `crates/lilac-codegen-llvm/Cargo.toml` (add `lilac-target` dependency)
- Modify: `crates/lilac-codegen-llvm/src/lib.rs` (import the width rule from `lilac-target`)
- Modify: `crates/lilac-ir/src/lib.rs` (delete `is_64_bit_triple`, lines ~733-748)
- Modify: `AGENTS.md` (width-rule home)
- Modify: `IR_SPEC.md` (any `is_64_bit_triple` reference)

**Interfaces:**
- Consumes: `lilac_target::is_64_bit_triple` (Task 1).
- Produces: nothing new; removes a public function from `lilac-ir`.

- [ ] **Step 1: Switch the backend import**

Add to `crates/lilac-codegen-llvm/Cargo.toml` under `[dependencies]`:

```toml
lilac-target = { path = "../lilac-target" }
```

In `crates/lilac-codegen-llvm/src/lib.rs`, change
`use lilac_ir::{is_64_bit_triple, LirModule};` to
`use lilac_ir::LirModule;` plus `use lilac_target::is_64_bit_triple;`.
No other backend code changes (it still calls `is_64_bit_triple(&self.triple)`).

- [ ] **Step 2: Delete the function from `lilac-ir`**

Remove `pub fn is_64_bit_triple` and its doc comment from `crates/lilac-ir/src/lib.rs`. Confirm no remaining callers:

Run: `cargo build --workspace`
Expected: builds (Task 2 already moved the driver off it; Task 3 Step 1 moved the backend).

- [ ] **Step 3: Update the docs**

- `AGENTS.md`: change the pointer-width bullet so it says `is_64_bit_triple` now lives in `lilac-target` (the single-width-rule invariant is unchanged).
- Run `rg -n "is_64_bit_triple" IR_SPEC.md` and update any hit to name `lilac-target`.

- [ ] **Step 4: Verify no snapshot drift and run the suite**

Run: `cargo test --workspace`
Expected: PASS. Then:

Run: `git status --short tests/ui`
Expected: empty (no `.lir`/`.ll` changed).

- [ ] **Step 5: Gates and commit**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add crates/lilac-codegen-llvm crates/lilac-ir AGENTS.md IR_SPEC.md
git commit -m "Move pointer-width rule to lilac-target"
```
