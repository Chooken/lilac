# Lilac VM Backend Removal

> Scope: delete the bytecode VM backend (`lilac-vm`) and the
> `lilacvm-unknown-none` target so the tree is LLVM-only, with nothing
> dangling. Branch `ai`. Prerequisite for the numeric-type refactor, which
> is then LLVM-only. Compiler crates only.

## 1. Outcome / constraints / success

**Outcome**

- `crates/lilac-vm/` no longer exists; the workspace is LLVM-only.
- The `lilacvm-unknown-none` target and `Arch::LilacVm` are gone.
- The CLI offers `check`, `emit-ir`, and `emit-llvm` only — no `emit-vm`,
  `run`, or `disasm`, and no debugger flags.
- No crate, doc, fixture, or manifest references the VM, `.lbc`, or the VM
  target.

**Constraints**

- Compiler crates stay Rust std-only; no new dependencies. `unsafe_code` and
  `clippy::pedantic` hold.
- No behavior change for the surviving LLVM pipeline.
- Historical `docs/superpowers/**` milestone docs are records of completed
  work and are left as-is (their `bench-baseline`/VM mentions are not chased).
- Gitignored local IDE metadata (`.idea/*`) is ignored.

**Success**

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, and `cargo test --workspace` are all green.
- `lilac emit-llvm <file>` still works end to end.
- `lilac --help` lists only the three remaining subcommands.
- A workspace grep for `lilac-vm`, `LilacVm`, `lilacvm`, `.lbc`, `VmBackend`,
  `emit_bytecode`, `run_lbc`, and `is_vm` finds no live (non-historical)
  references.

**Non-goals**

- Any change to LLVM codegen, the type system, intrinsics, or the language.
- Rewriting or deleting historical milestone specs/plans under
  `docs/superpowers/`.
- The numeric-type refactor (separate, following sub-project).

### 1.1 Decisions locked during brainstorming

1. **Remove the VM backend now, as its own short spec + plan cycle**, before
   the numeric refactor. The numeric refactor is then LLVM-only.
2. **Delete `docs/VM_SPEC.md`** (the live normative spec for the removed
   component) but **keep historical `docs/superpowers/**` milestone docs**.
3. **Remove the VM-only `Backend::emit_bytecode` hook** from `lilac-codegen`.
4. **Remove the dead `#ptr` reinterpret arms** in `lilac-codegen-llvm`
   (`lower.rs:208`, `:964`), which only the VM compiler ever produced.

## 2. Problem / rationale

The VM is a second backend (`crates/lilac-vm`) that lowers verified LIR to
canonical `.lbc` text and interprets it, selected by the
`lilacvm-unknown-none` target (`Arch::LilacVm`). It is now being phased out.
Keeping it would force the upcoming numeric refactor to teach the VM the new
`@fbit16/32/64/128` types and width-specialized intrinsics for no benefit.
Removing it first makes that refactor LLVM-only and eliminates a whole
class of duplicate work.

## 3. Removal map

### 3.1 Delete outright

| Path | Why |
| --- | --- |
| `crates/lilac-vm/` (whole crate: `Cargo.toml`, `src/`, incl. `src/bin/lilac-vm.rs` and all inline `#[cfg(test)]` modules) | The backend and its runner. |
| `crates/lilac-cli/tests/m5c.rs` | VM integration tests (`run`, `emit-vm`, `disasm`, VM-target rejection). |
| `tests/ui/vm_hello.lilac` | VM fixture source. |
| `tests/ui/vm_hello.lbc` | VM snapshot. |
| `tests/ui/vm_extern.lilac` | VM `extern`-error fixture. |
| `docs/VM_SPEC.md` | Live normative VM spec. |

The root `Cargo.toml` uses `members = ["crates/*"]` (a glob), so deleting the
crate directory removes it from the workspace; `Cargo.lock` regenerates on the
next build (drop the `lilac-vm` package entry and `lilac-cli`'s dep edge).

### 3.2 `lilac-cli` (`crates/lilac-cli/`)

- `Cargo.toml`: remove the `lilac-vm = { path = "../lilac-vm" }` dependency.
- `src/main.rs`: remove the `emit-vm`, `run`, and `disasm` dispatch arms and
  their usage-text entries; delete `cmd_emit_vm`, `cmd_run`, `cmd_disasm`,
  `run_debugger`, and `exit_code`; delete the debugger surface
  (`DebugOptions`, `parse_run_flag`, `Common.source`, `Common.debug`); remove
  the `if target.is_vm()` rejection in `cmd_emit_llvm`. Keep `check`,
  `emit-ir`, and `emit-llvm`.

### 3.3 `lilac-target` (`crates/lilac-target/src/lib.rs`)

- Remove the `Arch::LilacVm` variant, its `Arch::parse` arm (`"lilacvm"`), its
  `as_str` arm, and its `is_64_bit` arm.
- Remove `Target::is_vm`.
- Remove the VM test rows/cases (`width_agrees_for_every_arch` row,
  `lilacvm_arch_round_trips`, `non_vm_targets_report_false`). The real 64-bit
  arches (`X86_64`, `Aarch64`, `Riscv64`, `Wasm64`) remain.

### 3.4 `lilac-driver` (`crates/lilac-driver/src/lib.rs`)

- Make the `check_extern_signatures` call unconditional: drop the
  `if !self.target.is_vm()` gate and its "the VM keeps its clean runtime
  error for `extern`" comment. `extern` signatures are validated on every
  target now.

### 3.5 `lilac-codegen` (`crates/lilac-codegen/src/lib.rs`)

- Remove the VM-only `Backend::emit_bytecode` default method and its
  `default_emit_bytecode_errors_with_backend_name` test. `emit_llvm` and
  `name` remain. Update the trait doc that mentions "the VM build a `.lbc`
  line table".

### 3.6 `lilac-codegen-llvm` (`crates/lilac-codegen-llvm/src/lower.rs`)

- Remove the `#ptr` reinterpret arms at `:208` and `:964`, dead once the VM
  compiler is gone.

### 3.7 Docs & metadata

- `AGENTS.md`: delete the `## Bytecode VM (M5c)` section; remove the VM
  `extern` notes ("The VM is untouched…" and the "non-VM targets only"
  qualifier on `check_extern_signatures`).
- `SPEC.md`: delete §10 ("Targets & the bytecode VM") and the §9 sentence that
  an `extern` call on the VM target is a clean runtime error.
- `.gitattributes`: remove the `*.lbc text eol=lf` rule and its comment.
- `README.md`, `IR_SPEC.md`, `RUST_REWRITE_PLAN.md`, `STDLIB.md`, `.cargo/`,
  `.gitignore`, `lilac-lsp`, and `editors/` have no live VM references; leave
  them.

## 4. Testing

- Delete the VM fixtures and `m5c.rs` (above). No VM test remains.
- The remaining workspace tests (frontend, LLVM backend, CLI `check`/
  `emit-ir`/`emit-llvm`, LSP, stdlib) must stay green.
- Add/adjust no new tests: this sub-project only removes surface. Existing
  `emit-llvm` CLI tests continue to cover the surviving path.

## 5. Gates

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`

## 6. Maintenance / source of truth

- After this sub-project, `lilac-codegen::Backend` has a single implementor
  (`LlvmBackend`); there is no second backend.
- `lilac-target` recognizes only real hardware/WASM arches; `is_64_bit` is the
  single pointer-width rule and no longer special-cases a VM.

## 7. Follow-ups (separate specs)

- The numeric-type refactor: `@fbit16/32/64/128` float types,
  width-specialized builtins (`@i_add_8`…, `@f_add_16`…), fbit↔bit bitcasts,
  the `as` operator, real int/float literals with smallest-fit/grow-only
  typing, and C ABI float mapping — LLVM-only.
