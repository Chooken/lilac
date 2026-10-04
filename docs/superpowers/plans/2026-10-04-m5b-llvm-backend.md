# M5b — LLVM backend + `emit-llvm` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement a real LLVM backend for verified LIR with `inkwell` (LLVM 18.1.8), expose it as `lilac emit-llvm`, and prove every success program lowers, links via the toolchain `clang`, and runs to exit 0, with committed `.ll` snapshots.

**Architecture:** `lilac-codegen-llvm` consumes a verified `LirModule`, builds one LLVM module (types, places, instructions, an in-module runtime for the builtins, and a `main` entry wrapper), verifies it, optionally runs `opt` passes, and returns the module text. Only this crate links LLVM; the backend's product is IR text, so it is replaceable when self-hosting.

**Tech Stack:** Rust (stdlib + `inkwell 0.10`/`llvm-sys 181` in `lilac-codegen-llvm` only), LLVM 18.1.8, the toolchain `clang` for link/run. Branch `rust-rewrite-m0`.

**Spec:** `docs/superpowers/specs/2026-10-04-lilac-rust-rewrite-m5b-design.md`

## Global Constraints

- std only; the **only** new dependency is `inkwell 0.10` (feature `llvm18-1`) in `lilac-codegen-llvm`. No `serde`, no other crates.
- **Only `lilac-codegen-llvm` may link LLVM**; `grep -r inkwell crates/lilac-{span,error,lex,ast,parse,resolve,ty,hir,typeck,ir,const,lower,driver}` must be empty.
- Workspace `unsafe_code = "forbid"` must stay in force (inkwell's API is safe); `clippy::pedantic` is denied.
- Gates after every task: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- No `unwrap`/`expect`/`panic!` on user input; `lilac_error::bug!` for ICEs only (verified LIR must lower, so a lowering failure is an ICE).
- TDD; one commit per task. `.lir` snapshots stay byte-stable; `.ll` snapshots are LF, no BOM.
- Toolchain: LLVM 18.1.8 dev tree at `%USERPROFILE%\llvm-18.1.8`; cargo finds it via `LLVM_SYS_181_PREFIX`. Builds must run with that env var set.
- Type map (fixed): `#bit8/16/32/64`→`i8/16/32/64`; `#bitNative`→`i64` on 64-bit triples else `i32`; `ref #t`/`#ptr`→`ptr`; `#byte_buffer`→`{ iNative, ptr }`; `#format_args`→`{ iNative, [16 x { iNative, ptr }] }`; user `#Name`→named struct from `module.layouts["#Name"]` in order.
- Optimization map (fixed): `O0`→none, `O1`→`default<O1>`, `O2`→`default<O2>`, `O3`→`default<O3>`, `Os`→`default<Os>`. Snapshots are `O0`.
- Entry (fixed): LIR `@main` emits as `lilac_main`; append `define i32 @main() { call void @lilac_main(); ret i32 0 }` only when a `main` exists.

## Review Focus

1. A success program whose body is a bare effect-statement call to a `nothing` function must emit valid IR (one terminator, no phantom out). (Task 4)
2. `#format_args` is copied by value and pushed by `ref` (f-strings); its inline layout must be identical everywhere. (Tasks 2, 5)
3. `ref` parameters, `store` through refs, and field stores must address places correctly. (Tasks 3, 4)
4. A module with no `main` must emit no entry wrapper yet still verify. (Task 6)
5. `--target` overrides the host default, and `-O2` output must still run to exit 0. (Tasks 7, 8)

---

### Task 1: Backend skeleton, dependency, and build config

**Files:**
- Modify: `crates/lilac-codegen/src/lib.rs` (add `emit_llvm`, `OptLevel`)
- Modify: `crates/lilac-codegen-llvm/Cargo.toml` (add inkwell), `crates/lilac-codegen-llvm/src/lib.rs`
- Create: `.cargo/config.toml` (gitignored), modify `.gitignore`, `AGENTS.md`
- Test: `crates/lilac-codegen-llvm/src/lib.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `pub enum OptLevel { O0, O1, O2, O3, Os }` (in `lilac-codegen`); `Backend::emit_llvm(&self, &LirModule) -> Result<String, String>` (default `Err`); `LlvmBackend::new(triple: &str, optimize: OptLevel) -> Self`; `impl Backend for LlvmBackend` with `name → "llvm"`.

- [ ] **Step 1: Build config**

Create `.cargo/config.toml`:
```toml
[env]
LLVM_SYS_181_PREFIX = 'C:\Users\joshu\llvm-18.1.8'
```
Add `.cargo/` to `.gitignore`. Add an `AGENTS.md` note: LLVM 18.1.8 required, `LLVM_SYS_181_PREFIX` set via `.cargo/config.toml`, the `libxml2s.lib` stub in the LLVM tree, and that `inkwell` is confined to `lilac-codegen-llvm`.

- [ ] **Step 2: Write the failing tests**

In `lilac-codegen` (or `llvm`) test module:
```rust
#[test]
fn default_emit_llvm_is_unsupported() {
    struct Noop;
    impl lilac_codegen::Backend for Noop {
        fn name(&self) -> &'static str { "noop" }
        fn codegen(&self, _m: &lilac_ir::LirModule) -> Result<(), String> { Ok(()) }
    }
    let m = lilac_ir::LirModule::new("x86_64-pc-windows-msvc");
    assert!(Noop.emit_llvm(&m).is_err());
}

#[test]
fn empty_module_emits_verifyable_ir() {
    let backend = lilac_codegen_llvm::LlvmBackend::new("x86_64-pc-windows-msvc", lilac_codegen::OptLevel::O0);
    let module = lilac_ir::LirModule::new("x86_64-pc-windows-msvc");
    let text = backend.emit_llvm(&module).expect("emits");
    assert!(text.contains("target triple = \"x86_64-pc-windows-msvc\""), "{text}");
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p lilac-codegen-llvm`
Expected: FAIL to compile (`emit_llvm`, `OptLevel`, `LlvmBackend::new` missing) or, if the dep is absent, a missing-crate error.

- [ ] **Step 4: Implement**

- `Backend::emit_llvm` default method as in the spec.
- `OptLevel` enum.
- `LlvmBackend::new` stores `triple` + `optimize`; `emit_llvm` creates a `Context`, a `Module`, sets the triple and data layout from the triple (initialize the native target), calls `module.verify()`, and returns `module.print_to_string().to_string()`. Empty module (no runtime yet).
- `Cargo.toml`: `inkwell = { version = "0.10", features = ["llvm18-1"] }`.

- [ ] **Step 5: Run to verify it passes, then gates**

Run: `cargo test -p lilac-codegen-llvm; cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS; `unsafe_code` still forbidden.

- [ ] **Step 6: Commit**
```bash
git add crates/lilac-codegen/src/lib.rs crates/lilac-codegen-llvm/Cargo.toml \
  crates/lilac-codegen-llvm/src/lib.rs .gitignore AGENTS.md Cargo.lock
git commit -m "M5b: inkwell backend skeleton, emit_llvm trait hook, LLVM build config"
```

---

### Task 2: Type mapping, data globals, places, and zero-init

**Files:**
- Modify: `crates/lilac-codegen-llvm/src/lib.rs` (add a `lower` module)
- Test: same file's test module

**Interfaces:**
- Consumes: `LlvmBackend`, `LirModule`.
- Produces (internal): `fn lir_type(&self, ty: &str) -> BasicTypeEnum`; `fn define_layouts(&self, module)`; a `Lower` context that allocates places for `alloca … : zeroed` and zero-initializes them.

- [ ] **Step 1: Write the failing tests**

Build a `LirModule` with one function `f` whose `ins`/`outs` cover every scalar type (`#bit8/16/32/64`, `#bitNative`, `ref #bit32`), a `#byte_buffer`/`#format_args` place, and one `layouts` entry `#Pair { a: #bit32, b: #bit8 }`; lower it and assert the printed IR contains:
- `i8`, `i16`, `i32`, `i64`, `ptr`
- a named struct `%Pair = type { i32, i8 }`
- the `#byte_buffer`/`#format_args` struct types (`{ i64, ptr }`, `{ i64, [16 x { i64, ptr }] }` on this 64-bit host)
- for each `alloca … : zeroed`, a `store <ty> zeroinitializer`

Also assert an unknown type string is an ICE (test via `#[should_panic]` on the backend helper, not on user input).

- [ ] **Step 2: Run to verify it fails** — `cargo test -p lilac-codegen-llvm` → FAIL (no lowering).

- [ ] **Step 3: Implement** the type map and place allocation per Global Constraints. Function params (`in`) and returns (`out`) become zeroed allocas; each `LirInst::Alloca` yields an alloca of its type followed by a zero store. Emit data globals (`!c: str|numlit`) as private `[N x i8]` constants in a fixed order.

- [ ] **Step 4: Run to verify it passes; gates; commit**
```bash
git commit -am "M5b: LLVM type/layout lowering with zero-initialized places and data globals"
```

---

### Task 3: Memory, arithmetic, conversion, and constant instructions

**Files:**
- Modify: `crates/lilac-codegen-llvm/src/lib.rs`
- Test: same file

**Interfaces:**
- Consumes: Task 2's `Lower`.
- Produces: lowering for `CopyReg`, `CopyPlace`, `Const`, `Load`, `Addr`, `AddrFunc`, `Field`, `StoreField`, `Store`, `FieldAddr`, `Convert`, `Arith`.

- [ ] **Step 1: Write the failing tests**

One hand-built function exercising each instruction; assert the IR contains the expected opcode for each:
- `CopyReg`/`CopyPlace` → `load`/`store`
- `Const { lit: "0x2a" }` → `i32 42`; `Const { lit: "!c0" }` with `ty = "#byte_buffer"` → a `ptr` into `@c0` wrapped in a `{ i64, ptr }`
- `Load` → `load`; `Addr`/`FieldAddr` → `getelementptr`; `AddrFunc` → the function's `ptr`
- `Field` → `getelementptr` + `load` at the recorded `idx`; `StoreField`/`Store` → `getelementptr`/`store`
- `Convert` `Trunc` → `trunc`; `ExtendZero` → `zext`
- `Arith` `IAdd`→`add`, `ISub`→`sub`, `IMul`→`mul`, `IDiv`→`sdiv`, `IMod`→`srem`, `Eq`→`icmp eq`, `ILt`→`icmp slt`, `IGt`→`icmp sgt`; float ops to `fadd`/`fsub`/`fmul`/`fdiv`/`frem`/`fcmp oeq|olt|ogt`

- [ ] **Step 2: Run to verify it fails; Step 3: Implement; Step 4: verify + gates; Step 5: commit**
```bash
git commit -am "M5b: lower LIR memory, arithmetic, conversion, and constant instructions"
```

---

### Task 4: Calls, control flow, and terminators

**Files:**
- Modify: `crates/lilac-codegen-llvm/src/lib.rs`
- Test: same file

**Interfaces:**
- Consumes: Tasks 2–3.
- Produces: lowering for `Call` (direct/indirect/builtin), `Drop`, `branch`/`branch.if`/`return`/`panic`/`unreachable`.

- [ ] **Step 1: Write the failing tests**

- A direct call to a `nothing` function used as an effect statement lowers to `call void @g()` then the block's `ret` — exactly one terminator, no phantom out. (Review Focus 1)
- An indirect call through an `addr.func` register lowers to a `ptr` call.
- `branch`/`branch.if` produce labeled blocks and `br`/`br i1`; `branch.if` with no `els` targets the fall-through.
- `return` → `ret void`; `unreachable` → `unreachable`; `panic !c0` → a call to `@lilac_abort`; `drop %p` → no emitted instruction.

- [ ] **Step 2-4: RED → implement → GREEN + gates.**

- [ ] **Step 5: Commit**
```bash
git commit -am "M5b: lower calls, control flow, and terminators"
```

---

### Task 5: In-module runtime builtins

**Files:**
- Modify: `crates/lilac-codegen-llvm/src/lib.rs` (add `runtime` module)
- Test: same file

**Interfaces:**
- Produces: `fn emit_runtime(&self, context, module, native_i32?)` defining the fixed set of runtime functions; `LirCallee::Builtin(name)` resolves to these symbols.

- [ ] **Step 1: Write the failing tests**

- Lowering a module that calls each of `@byte_buffer_from/ptr/len`, `@load_u8`, `@store_u8`, `@memcpy`, `@format_args_new/push/count/piece` produces a module whose IR defines each runtime symbol once, with the `IR_SPEC.md` signature.
- `@format_args` value layout is `{ iNative, [16 x { iNative, ptr }] }` and `@format_args_push` compares the count against 16.

- [ ] **Step 2-4: RED → implement → GREEN + gates.**

- [ ] **Step 5: Commit**
```bash
git commit -am "M5b: emit in-module LLVM runtime for byte_buffer/format_args/mem builtins"
```

---

### Task 6: Entry wrapper, target resolution, and optimization

**Files:**
- Modify: `crates/lilac-codegen-llvm/src/lib.rs`
- Test: `crates/lilac-codegen-llvm/src/lib.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: entry wrapper (`lilac_main` + `i32 @main`) when a `main` function exists; `LlvmBackend::emit_llvm` runs the pass pipeline selected by `OptLevel`.
- The `lilac_driver::Opt` → `lilac_codegen::OptLevel` mapping lives in the CLI (Task 7), not the driver, to avoid a driver→codegen dependency.

- [ ] **Step 1: Write the failing tests**

- A module with `@main` prints `lilac_main`, a `define i32 @main()` wrapper calling it, and `ret i32 0`.
- A module without `main` prints neither. (Review Focus 4)
- `emit_llvm` with `OptLevel::O2` on a function with redundant arithmetic differs from `O0` (passes ran).

- [ ] **Step 2-4: RED → implement → GREEN + gates.**

- [ ] **Step 5: Commit**
```bash
git commit -am "M5b: main entry wrapper and optimization-level pass pipeline"
```

---

### Task 7: `emit-llvm` CLI subcommand

**Files:**
- Modify: `crates/lilac-cli/src/main.rs`, `crates/lilac-cli/Cargo.toml` (add `lilac-codegen`, `lilac-codegen-llvm`)
- Create: `tests/ui/hello.lilac`? (exists) — use existing fixtures
- Test: `crates/lilac-cli/src/main.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `Session::compile_lir`, `Session.host`, `LlvmBackend::emit_llvm`.
- Produces: `cmd_emit_llvm`; `Common` gains `target_set: bool`.

- [ ] **Step 1: Write the failing tests**

- `parse_common(["--target=foo", "f.lilac"], true).unwrap().target_set == true`; without `--target`, `false`.
- Mapping `Opt::O3 → OptLevel::O3`, `Opt::Os → OptLevel::Os`.

- [ ] **Step 2: Implement** `emit-llvm <file> [--check] [--target=] [-O*]`. Without `--check`, print the IR. With `--check`, byte-compare against `<file>.ll` (mirror `cmd_emit_ir`, including the no-BOM/LF snapshot). Default target (when `target_set` is false) is `session.host.triple`, and the session target is set to it before `compile_lir` so `@bitNative` matches. Update `usage()`.

- [ ] **Step 3: Run** `cargo run -q -p lilac-cli -- emit-llvm tests/ui/fstring.lilac` and confirm it prints IR; then gates.

- [ ] **Step 4: Commit**
```bash
git commit -am "M5b: add emit-llvm subcommand with --check and host target default"
```

---

### Task 8: Snapshots + compile+run harness

**Files:**
- Create: `tests/ui/*.ll` (one per success `.lilac`)
- Create: `crates/lilac-cli/tests/m5b.rs`
- Test: `m5b.rs`

**Interfaces:**
- Consumes: `emit-llvm`, `emit-llvm --check`, `LLVM_SYS_181_PREFIX\bin\clang.exe`.

- [ ] **Step 1: Write the failing harness**

`m5b.rs` mirrors `m5a.rs`:
```rust
const SUCCESS_FIXTURES: &[&str] = &[ /* every tests/ui/*.lilac paired with a .lir */ ];

#[test]
fn ll_snapshots_stable() { /* run `emit-llvm --check` over every tests/ui/*.ll */ }

#[test]
fn compiles_and_runs_on_host() {
    // for each success fixture: emit-llvm to a temp .ll,
    // clang <ll> -o <exe> -Wl,... ; run <exe>; assert exit 0.
    // Repeat at -O2.
}

#[test]
fn drops_and_zero_init_visible() {
    // `emit-llvm generic_hook_drop.lilac` contains `zeroinitializer`
    // and a call to the instance's `@on_drop`.
}
```

- [ ] **Step 2: Generate `.ll` snapshots**

For each success fixture run `emit-llvm <file>`, inspect, and write `<file>.ll` no-BOM/LF. Confirm `emit-llvm --check` exits 0 for all.

- [ ] **Step 3: Run the harness and gates**

Run: `cargo test -p lilac-cli --test m5b; cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace`
Expected: all green; every success program exits 0 at `-O0` and `-O2`.

- [ ] **Step 4: Commit**
```bash
git add tests/ui crates/lilac-cli/tests/m5b.rs
git commit -m "M5b: LLVM IR snapshots and host compile+run harness"
```

---

## Final verification (after all tasks)

- [ ] `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` green.
- [ ] `grep -r inkwell crates/lilac-{span,error,lex,ast,parse,resolve,ty,hir,typeck,ir,const,lower,driver}` is empty.
- [ ] `ll_snapshots_stable` and `compiles_and_runs_on_host` pass; `-O2` runs exit 0.
- [ ] `.lir` snapshots unchanged; `unsafe_code = forbid` intact.
- [ ] Each Review Focus item has a passing test.

## Self-Review

- **Spec coverage:** §3.1 → Task 1; §3.2 → Tasks 2–4; §3.3 → Task 5; §3.4 → Task 6; §3.5 → Task 7; §3.6 → Task 1; §5 tests → Task 8.
- **Type consistency:** `OptLevel`, `emit_llvm`, `LlvmBackend::new`, `lir_type`, `emit_runtime`, `Common.target_set` are named identically across tasks and the spec.
- **Review Focus:** each of the five lines names its owning task's test.
- **Proportion:** the type map, layouts, runtime set, entry, triple, opt pipeline, and CLI surface are pinned; inkwell call bodies are left to the implementer.
