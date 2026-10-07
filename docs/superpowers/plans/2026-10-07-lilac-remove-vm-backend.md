# Lilac VM Backend Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the bytecode VM backend (`lilac-vm`) and the `lilacvm-unknown-none` target so the tree is LLVM-only with nothing dangling.

**Architecture:** Remove consumers before producers so every task leaves a green workspace. Strip the CLI's VM surface and its tests first, delete the crate, then remove the now-dead target/backend seams, then the docs and a dangling-reference sweep.

**Tech Stack:** Rust workspace; compiler crates are std-only; LLVM is confined to `lilac-codegen-llvm`.

**Spec:** `docs/superpowers/specs/2026-10-07-lilac-remove-vm-backend-design.md`

## Global Constraints

- Compiler crates stay Rust std-only; add no dependencies.
- `unsafe_code = "forbid"` and `clippy::pedantic = deny` hold; no new `unwrap`/`expect`/`panic!` on user input.
- No behavior change for the surviving LLVM pipeline (`check`, `emit-ir`, `emit-llvm`).
- Historical `docs/superpowers/**` milestone docs are records and must NOT be edited.
- Gitignored local metadata (`.idea/*`) is out of scope.
- Gates: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`.

## Review Focus

- A stale VM reference survives in a live file (doc, manifest, source, fixture) and misleads a reader — pinned by the Task 4 grep sweep over live files.
- `lilac --help` still advertises removed subcommands or debugger flags — pinned by Task 1's usage check.
- A workspace member is left half-registered (e.g. a dangling `lilac-vm` entry or a stale `Cargo.lock` edge) — pinned by Task 2's `cargo metadata` check.
- Historical milestone docs are edited by mistake — pinned by Task 4's diff check.
- `.lbc` fixtures or the `*.lbc` `.gitattributes` rule are left behind — pinned by Task 1's fixture deletion and Task 4's sweep.

---

### Task 1: Remove the VM CLI surface

**Files:**
- Modify: `crates/lilac-cli/Cargo.toml:18`
- Modify: `crates/lilac-cli/src/main.rs` (usage, dispatch, `Common`, debugger, `cmd_emit_vm`, `cmd_run`, `cmd_disasm`, `run_debugger`, `exit_code`, `cmd_emit_llvm` guard)
- Delete: `crates/lilac-cli/tests/m5c.rs`
- Delete: `tests/ui/vm_hello.lilac`, `tests/ui/vm_hello.lbc`, `tests/ui/vm_extern.lilac`

**Interfaces:**
- Consumes: nothing.
- Produces: a `lilac-cli` that depends only on LLVM and exposes `check`, `emit-ir`, `emit-llvm`.

- [ ] **Step 1: Delete the VM tests and fixtures**

```bash
git rm crates/lilac-cli/tests/m5c.rs tests/ui/vm_hello.lilac tests/ui/vm_hello.lbc tests/ui/vm_extern.lilac
```

- [ ] **Step 2: Drop the `lilac-vm` dependency**

In `crates/lilac-cli/Cargo.toml`, remove the line `lilac-vm = { path = "../lilac-vm" }`.

- [ ] **Step 3: Strip the VM surface from `crates/lilac-cli/src/main.rs`**

Remove:
- the `emit-vm`, `run`, and `disasm` entries from `usage()` and their dispatch arms in the subcommand `match`;
- `DebugOptions`, `parse_run_flag`, and the `Common.source` / `Common.debug` fields and their parsing;
- the functions `cmd_emit_vm`, `cmd_run`, `cmd_disasm`, `run_debugger`, and `exit_code`;
- the `if target.is_vm() { … }` rejection block in `cmd_emit_llvm`.

Keep `check`, `emit-ir`, `emit-llvm`, and the `use lilac_codegen::Backend as _;` import (`cmd_emit_llvm` needs the `Backend` trait).

- [ ] **Step 4: Verify the CLI builds and its tests pass**

Run: `cargo build -p lilac-cli`
Expected: success, no `lilac_vm` unresolved-import errors.

Run: `cargo test -p lilac-cli`
Expected: all tests pass (the deleted `m5c` tests are gone).

- [ ] **Step 5: Verify the usage text**

Run: `cargo run -p lilac-cli -- --help`
Expected: usage lists only `check`, `emit-ir`, `emit-llvm` — no `emit-vm`, `run`, or `disasm`.

- [ ] **Step 6: Commit**

```bash
git add crates/lilac-cli tests/ui
git commit -m "cli: remove the VM subcommands, debugger, and tests"
```

---

### Task 2: Delete the `lilac-vm` crate

**Files:**
- Delete: `crates/lilac-vm/` (entire directory)

**Interfaces:**
- Consumes: Task 1 (no crate depends on `lilac-vm`).
- Produces: a workspace with no `lilac-vm` member.

- [ ] **Step 1: Delete the crate**

```bash
git rm -r crates/lilac-vm
```

- [ ] **Step 2: Verify the workspace no longer contains the crate**

Run: `cargo metadata --no-deps --format-version 1`
Expected: no package named `lilac-vm` in the members list.

- [ ] **Step 3: Verify the workspace builds and tests pass**

Run: `cargo build --workspace`
Expected: success.

Run: `cargo test --workspace`
Expected: all tests pass; `Cargo.lock` regenerates without a `lilac-vm` package.

- [ ] **Step 4: Commit**

```bash
git add -A Cargo.lock
git commit -m "vm: delete the lilac-vm bytecode backend"
```

---

### Task 3: Remove the VM target and backend seams

**Files:**
- Modify: `crates/lilac-target/src/lib.rs` (`Arch::LilacVm` variant `:27`, `Arch::parse` `:43`, `Arch::as_str` `:60`, `is_64_bit` `:69`, `Target::is_vm` `:317-321`, tests `:497`, `:519-525`, `:527-530`)
- Modify: `crates/lilac-driver/src/lib.rs:310-318`
- Modify: `crates/lilac-codegen/src/lib.rs:38-55, 63-82`
- Modify: `crates/lilac-codegen-llvm/src/lower.rs:208, 964`

**Interfaces:**
- Consumes: Task 2 (the `lilac-vm` crate is gone, so `Backend::emit_bytecode` has no implementor).
- Produces: `lilac-target` with only real arches; a `Backend` trait with `name` + `emit_llvm`; an unconditional `check_extern_signatures`.

- [ ] **Step 1: Remove the VM target from `lilac-target`**

In `crates/lilac-target/src/lib.rs`, delete the `Arch::LilacVm` variant, its `"lilacvm"` arm in `Arch::parse`, its `"lilacvm"` arm in `Arch::as_str`, its `| Self::LilacVm` arm in `Arch::is_64_bit`, the `Target::is_vm` method, and the three VM tests (`lilacvm_arch_round_trips`, `non_vm_targets_report_false`, and the `Arch::LilacVm` row in `width_agrees_for_every_arch`).

- [ ] **Step 2: Make `check_extern_signatures` unconditional**

In `crates/lilac-driver/src/lib.rs`, replace the `if !self.target.is_vm() { … }` gate with the unconditional `lilac_lower::check_extern_signatures(...)` call, and delete the "the VM keeps its clean runtime error for `extern`" comment.

- [ ] **Step 3: Remove the VM-only `Backend::emit_bytecode` hook**

In `crates/lilac-codegen/src/lib.rs`, delete the `emit_bytecode` default method (and its doc comment referencing `.lbc`) and the `default_emit_bytecode_errors_with_backend_name` test.

- [ ] **Step 4: Remove the dead `#ptr` arms**

In `crates/lilac-codegen-llvm/src/lower.rs`, delete the `#ptr` handling at `:208` and `:964` (only the VM compiler ever produced `#ptr`).

- [ ] **Step 5: Verify the workspace builds and tests pass**

Run: `cargo build --workspace`
Expected: success.

Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 6: Verify no live VM seam remains**

Run: `git grep -n -E "is_vm|LilacVm|emit_bytecode|#ptr" -- crates/`
Expected: no matches.

- [ ] **Step 7: Commit**

```bash
git add crates/lilac-target crates/lilac-driver crates/lilac-codegen crates/lilac-codegen-llvm
git commit -m "target: remove the VM target and backend seams"
```

---

### Task 4: Remove VM docs and metadata, then sweep

**Files:**
- Delete: `docs/VM_SPEC.md`
- Modify: `AGENTS.md` (the `## Bytecode VM (M5c)` section and the two VM `extern` notes)
- Modify: `SPEC.md` (§10 and the §9 VM `extern` sentence)
- Modify: `.gitattributes` (the `*.lbc` rule and its comment)

**Interfaces:**
- Consumes: Tasks 1–3 (all live VM code is gone).
- Produces: documentation and metadata that match the LLVM-only tree.

- [ ] **Step 1: Delete the live VM spec**

```bash
git rm docs/VM_SPEC.md
```

- [ ] **Step 2: Clean `AGENTS.md`**

Delete the entire `## Bytecode VM (M5c)` section, the "The VM is untouched…" `extern` note, and the "non-VM targets only" qualifier on `check_extern_signatures`.

- [ ] **Step 3: Clean `SPEC.md`**

Delete §10 ("Targets & the bytecode VM") and the §9 sentence that an `extern` call on the VM target is a clean runtime error.

- [ ] **Step 4: Clean `.gitattributes`**

Remove the `*.lbc text eol=lf` rule and the "Bytecode snapshots…" comment line.

- [ ] **Step 5: Sweep for dangling references in live files**

Run: `git grep -n -i -E "lilac-vm|lilacvm|LilacVm|VmBackend|run_lbc|emit-vm|\.lbc|bytecode VM" -- ':!docs/superpowers' ':!Cargo.lock'`
Expected: no matches (historical `docs/superpowers/**` and `Cargo.lock` are excluded).

- [ ] **Step 6: Confirm historical docs were not touched**

Run: `git status --short docs/superpowers`
Expected: no modified files under `docs/superpowers/specs/2026-10-05-lilac-vm-*` or the matching plans (only the new VM-removal spec/plan may appear as added).

- [ ] **Step 7: Run the full gate set**

Run: `cargo fmt --check`
Expected: success.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: success.

Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 8: Commit**

```bash
git add AGENTS.md SPEC.md .gitattributes docs/VM_SPEC.md
git commit -m "docs: remove VM specification and references"
```

---

## Self-Review

**Spec coverage:** §3.1 deletions → Tasks 1 (CLI tests/fixtures) and 2 (crate) and 4 (`VM_SPEC.md`); §3.2 `lilac-cli` → Task 1; §3.3 `lilac-target` → Task 3; §3.4 `lilac-driver` → Task 3; §3.5 `lilac-codegen` → Task 3; §3.6 `lilac-codegen-llvm` → Task 3; §3.7 docs/metadata → Task 4; §4 testing → Tasks 1–4; §5 gates → Task 4 Step 7. No spec section is unmapped.

**Ordering:** Each task leaves a green workspace: Task 1 removes the only crate consumer before Task 2 deletes the crate; Task 3 removes seams that only the deleted crate used; Task 4 is docs-only.

**Type/name consistency:** `Arch::LilacVm`, `Target::is_vm`, `Backend::emit_bytecode`, `check_extern_signatures`, `#ptr` are named identically wherever they appear across tasks.
