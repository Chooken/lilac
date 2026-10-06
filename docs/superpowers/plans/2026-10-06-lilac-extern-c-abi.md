# Lilac M6b — Minimal `extern` C ABI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lower and call `extern` declarations with the platform C ABI for scalars and pointers, so a Lilac program can call libc (`write`, `malloc`, `free`, `memcpy`) and observe real output.

**Architecture:** Only `lilac-codegen-llvm` changes. `lower_func` emits a C signature for an `extern` `LirFunc` (still bodyless); `lower_direct_call` branches to a C-ABI argument path for extern callees. Internal functions keep the pointer-place ABI. The VM is untouched (`extern` stays a clean runtime error).

**Tech Stack:** Rust (stable, std-only), `inkwell`/LLVM 18.1.8, toolchain `clang` for end-to-end linking.

**Spec:** `docs/superpowers/specs/2026-10-06-lilac-modules-and-extern-c-abi-design.md` (§8)

## Global Constraints

- `inkwell`/`llvm-sys` stay confined to `lilac-codegen-llvm`; `unsafe_code = "forbid"` elsewhere.
- Extern symbols are the **unmangled declared name** (never namespace-prefixed).
- Supported C types: `#bit8/#bit16/#bit32/#bit64/#bit_native` by value, `ref T` → pointer. Anything else is `E028`.
- Returns: `0` outs = `void`, `1` out = the C return value, `>1` = `E028`.
- New code `E028`; doc `docs/errors/E028.md`.
- Gates per commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.

## Review Focus

- A libc `write(1, ptr, len)` call must print the bytes and return the written count.
- An `extern` with two return slots must be `E028`, not a mis-compiled call.
- An `extern` taking a `@byte_buffer` (aggregate) must be `E028`, not a struct pointer passed to C.
- An internal call must still use the pointer-place ABI (no regression).
- `main` and all existing single-file `.ll` snapshots except the intentionally-rewritten `extern_decl` stay byte-identical.

---

### Task 1: Extern C-ABI signature

**Files:**
- Modify: `crates/lilac-codegen-llvm/src/lower.rs` (`lower_func`, the `void(ptr…)` declaration path at `:280-321`)
- Test: `crates/lilac-codegen-llvm/src/lib.rs` (module tests)

**Interfaces:**
- Consumes: `LirFunc { ins: Vec<(String,String)>, outs, is_extern }`.
- Produces: an `extern` `LirFunc` declared with the C signature; internal funcs unchanged.

- [ ] **Step 1: Write the failing test** — lower an `extern write(fd: @bit32, buf: ref @bit8, n: @bit_native) -> @bit_native` and assert the emitted declaration is `declare i64 @write(i32, ptr, i64)` (exact type per target: `#bit_native` → `i64` on a 64-bit triple).

- [ ] **Step 2: Run it to verify it fails** — `cargo test -p lilac-codegen-llvm extern_decl_uses_c_signature` → FAIL (`declare void @write(ptr, ptr, ptr, ptr)`).

- [ ] **Step 3: Implement** — in `lower_func`, when `func.is_extern`, map each `in`/`out` slot to a C type (`ref T` → `ptr`; bit widths → the integer type) and declare `fn_type(returns, params, false)`; emit `E028` for an unsupported slot or `>1` out. Keep the early return (no body).

- [ ] **Step 4: Run** — `cargo test -p lilac-codegen-llvm` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "M6b: declare extern functions with the C ABI"`.

---

### Task 2: Extern C-ABI call

**Files:**
- Modify: `crates/lilac-codegen-llvm/src/lower.rs` (`lower_direct_call`, `direct_call_args` at `:505-566`)
- Test: `crates/lilac-codegen-llvm/src/lib.rs` (module tests)

**Interfaces:**
- Consumes: Task 1's declaration.
- Produces: an extern call passing scalars by value and `ref T` as pointers, storing the single return into the out place.

- [ ] **Step 1: Write the failing test** — lower a call to the Task 1 extern with places `(fd=1, buf=@data, n=2)` and assert the call is `call i64 @write(i32 1, ptr @data, i64 2)` and the result is stored to the out place.

- [ ] **Step 2: Run it to verify it fails** — `cargo test -p lilac-codegen-llvm extern_call_uses_c_abi` → FAIL.

- [ ] **Step 3: Implement** — branch in `lower_direct_call` on the callee `LirFunc.is_extern`: load scalar args, pass `ref` args as pointers, call, and store the return into the out place; otherwise keep `direct_call_args`.

- [ ] **Step 4: Run** — `cargo test -p lilac-codegen-llvm` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "M6b: call extern functions with the C ABI"`.

---

### Task 3: `E028` validation

**Files:**
- Modify: `crates/lilac-codegen-llvm/src/lower.rs` (error emission)
- Test: `crates/lilac-cli/tests/m6.rs`; `tests/ui/FAILS_extern_multi_out.lilac`, `tests/ui/FAILS_extern_aggregate_param.lilac`

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces: `E028` for an unsupported extern signature, with a label on the offending slot.

- [ ] **Step 1: Write the failing test** — the two `FAILS_` fixtures exit 1 with `error[E028]` and never panic/signal (the `m4_regressions` contract).

- [ ] **Step 2: Run it to verify it fails** — `cargo test -p lilac-cli --test m6` → FAIL.

- [ ] **Step 3: Implement** — emit `E028` (with the slot span) for a `>1`-out extern, or a param/return that is neither a bit width nor `ref T`.

- [ ] **Step 4: Run** — `cargo test --workspace` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "M6b: diagnose unsupported extern signatures with E028"`.

---

### Task 4: Fix `extern_decl` and add libc end-to-end tests

**Files:**
- Modify: `tests/ui/extern_decl.lilac`, `tests/ui/extern_decl.lir`, `tests/ui/extern_decl.ll`
- Create: `tests/ui/extern_write.lilac` + `.lir`/`.ll`; `crates/lilac-cli/tests/m6.rs`
- Test: `crates/lilac-cli/tests/m6.rs`

**Interfaces:**
- Consumes: Tasks 1–3; `@byte_buffer_ptr`/`@byte_buffer_len` (already user-callable, `crates/lilac-lower/src/lib.rs:1502`).

- [ ] **Step 1: Rewrite `extern_decl`** — replace `puts(s: @byte_buffer)` with a C-correct declaration (e.g. `write(fd: @bit32, buf: ref @bit8, n: @bit_native) -> @bit_native`) and regenerate `.lir`/`.ll`.
- [ ] **Step 2: Add `extern_write.lilac`** — declare libc `write`, and in `main` call `write(0x1, @byte_buffer_ptr(s), @byte_buffer_len(s))` for `s: @byte_buffer = "hi\n"`; regenerate snapshots.
- [ ] **Step 3: Write the end-to-end test** — `emit-llvm --target=<host>`, link with the toolchain `clang` (reuse `m5b.rs`'s `clang()` helper), run, and assert stdout is exactly `hi\n` and the exit status is success.
- [ ] **Step 4: Run** — `cargo test -p lilac-cli` and `cargo test --workspace` → PASS (the `extern_decl` snapshot is the only intentional `.ll` change).
- [ ] **Step 5: Commit** — `git commit -m "M6b: fix extern_decl and add libc write end-to-end test"`.

---

### Task 5: Docs

**Files:**
- Create: `docs/errors/E028.md`
- Modify: `SPEC.md` §9 (extern C ABI), `docs/VM_SPEC.md` §5 (extern stays VM-unsupported), `AGENTS.md`

- [ ] **Step 1: Write `E028.md`** matching the house format.
- [ ] **Step 2: Update `SPEC.md` §9** to state the C-ABI mapping (scalars by value, `ref T` → pointer, 0/1 out) and bump the version; confirm `docs/VM_SPEC.md` still documents `extern` as a clean VM error.
- [ ] **Step 3: Run** — `cargo test --workspace` → PASS.
- [ ] **Step 4: Commit** — `git commit -m "M6b: document the extern C ABI and E028"`.

---

## Self-Review

- **Spec coverage:** §8 declaration → Task 1; §8 call → Task 2; `E028` → Task 3; §8 fixture fix + libc tests → Task 4; §10 doc → Task 5.
- **Step scan:** each step is one action with a checkable result; the C-type mapping is fixed by the spec, the codegen bodies are left to the implementer.
- **Type consistency:** the extern signature types (Tasks 1–2) match the `E028` rules (Task 3) and the fixtures (Task 4).
- **Review Focus:** libc output (Task 4), `E028` cases (Task 3), internal-ABI regression and snapshot stability (Tasks 2, 4).
- **Proportion:** the plan names the mapping and the tests; it does not transcribe the codegen.
