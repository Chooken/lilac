# Runtime-in-LIR Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the builtin runtime into portable LIR functions, culled by DCE and shared by all backends, keeping only three leaf intrinsics; make generated names collision-proof.

**Architecture:** Seven runtime functions become ordinary `LirFunc`s built by a new `lilac_ir::runtime` module and injected by the driver between `fold` and `prune_unreachable`. The frontend calls them as `LirCallee::Func(".lilac.<name>")`. Three leaves stay `LirCallee::Builtin`: `@ptr_add` (new), `@memcpy`, `@lilac_abort`. Generated names switch from `_`/`__` to the reserved `.` grammar; the verifier rejects duplicate function names.

**Tech Stack:** Rust (std only), LLVM 18.1.8 via `inkwell` (confined to `lilac-codegen-llvm`), hand-rolled binary LIR codec.

**Spec:** `docs/superpowers/specs/2026-10-05-lilac-runtime-in-lir-design.md`

## Global Constraints

- LLVM dev tree at `C:\Users\joshu\llvm-18.1.8`; `LLVM_SYS_181_PREFIX` set via `.cargo/config.toml`.
- Rust std only; no `serde`; no new third-party crates.
- `inkwell`/`llvm-sys` may appear only in `lilac-codegen-llvm`; workspace `unsafe_code = "forbid"`.
- No `unwrap`/`expect`/`panic!` on user input; use `lilac_error::bug!` for ICEs.
- Gates before each commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Snapshots are LF, no BOM: write with `[IO.File]::WriteAllText($p, $t, (New-Object Text.UTF8Encoding $false))`, never shell redirect.
- `.gitattributes` pins `*.lir`/`*.ll` to `text eol=lf`.
- One commit per task; do not commit until the task's tests pass.
- Generated names use `.` (reserved; identifiers are `[A-Za-z_][A-Za-z0-9_]*`). Compiler symbols use the `.lilac.` prefix.

## Review Focus

- A program that uses **no** runtime (e.g. `tests/ui/arithmetic.lilac`) must emit no runtime text and still verify, compile, and run.
- A program using **fstring / `@compile_error`** must keep const-evaluating its byte-buffer builtins after call sites become `.lilac.*` functions.
- Generic instantiations over **builtin and user types with `_` in names** must not collide, and DCE must keep every reachable instance.
- `#format_args` field indices in the verifier (`builtin_layout`) must match the LLVM struct codegen builds (16 pieces), or `field.addr`/`field` silently read the wrong slot.
- A **32-bit target** must use the 8-byte `#byte_buffer` stride, not the 64-bit 16.

---

### Task 1: Reserved `.` naming

**Files:**
- Modify: `crates/lilac-ty/src/lib.rs` (`mangle`, `mangle_ref`, tests ~:449, :457)
- Modify: `crates/lilac-typeck/src/check/generics.rs` (instance naming ~:652, :671; `owner_members` ~:898)
- Modify: `crates/lilac-codegen-llvm/src/lib.rs` (`symbol`, `builtin_symbol`)
- Modify: `crates/lilac-codegen-llvm/src/runtime.rs` (leaf/wrapper symbol names)
- Modify: `crates/lilac-codegen-llvm/src/lower.rs` (any `lilac_main`/`lilac_abort` references)
- Regenerate: all `tests/ui/*.lir`, `tests/ui/*.ll`

**Interfaces:**
- Produces: `mangle` spellings other tasks rely on: builtin `.bit32`; ref `ref..bit32`; instance `GenObj.1..bit32`; func type `function.2..bit32..bit8.returns.0`; param `param.0`; method `GenObject.1..bit32.get.1..bit64`; function instance `gen.1.Object`. Compiler symbols `.lilac.main`, `.lilac.memcpy`, `.lilac.abort`.

- [ ] **Step 1: Rewrite the mangle unit tests to the new spellings**

In `crates/lilac-ty/src/lib.rs` tests, assert:
`types.mangle(outer) == "GenObj.1..bit32"` and
`types.mangle(nested) == "GenObj.1..GenObj.1..bit32"`. Add a case proving `_` in a base name cannot be confused with a separator, e.g. an object `Gen_Obj` instanced with `@bit32` mangles to `Gen_Obj.1..bit32`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p lilac-ty mangle`
Expected: FAIL (old `GenObj__1__bit32`).

- [ ] **Step 3: Implement the `.` grammar**

In `mangle`: builtins become `.bit8`/`.bit16`/`.bit32`/`.bit64`/`.bitNative`/`.byte_buffer`/`.format_args`/`.unknown`/`.error`; `Param` → `format!("param.{index}")`; `Instance` → `format!("{base}.{argc}")` then for each arg `out.push('.'); out.push_str(&self.mangle_ref(*arg))`; `Func` → `format!("function.{}", params.len())`, then `.` + each param, then `.returns.{nrets}`, then `.` + each ret. In `mangle_ref`: ref prefix becomes `ref.` (so `ref..bit32`).

- [ ] **Step 4: Update typeck naming and `owner_members`**

In `check/generics.rs`: the own-args suffix uses `.` (e.g. `name.push('.'); name.push_str(&own.len().to_string())` then `.` + each arg); the function-instance name is `format!("{}.{}", self.function_name(def), args.len())` then `.` + each arg; `owner_members` uses `owner.split('.').next()`.

- [ ] **Step 5: Rename compiler symbols**

`symbol()` maps `"main"` → `".lilac.main"`. `builtin_symbol` maps `"@memcpy"` → `".lilac.memcpy"`. In `runtime.rs` rename the `lilac_abort` definition and its callers to `.lilac.abort`; update the M5b codegen unit tests that assert `@lilac_memcpy`/`@lilac_abort`/`@lilac_main`.

- [ ] **Step 6: Regenerate snapshots**

Rebuild `cargo build -p lilac-cli`, then for every `tests/ui/*.lilac` with a `.lir` and/or `.ll`:
```powershell
$enc = New-Object Text.UTF8Encoding $false
$lir = (& target\debug\lilac.exe emit-ir tests/ui/<name>.lilac) -join "`n"
[IO.File]::WriteAllText((Resolve-Path tests/ui/<name>.lir), $lir + "`n", $enc)
$ll = (& target\debug\lilac.exe emit-llvm --target=x86_64-unknown-none tests/ui/<name>.lilac) -join "`n"
[IO.File]::WriteAllText((Resolve-Path tests/ui/<name>.ll), $ll + "`n", $enc)
```
Inspect the diff: only names should change (e.g. `GenObj__1__bit32` → `GenObj.1..bit32`, `lilac_main` → `.lilac.main`).

- [ ] **Step 7: Run gates**

Run: `cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace`
Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "Switch generated names to reserved '.' grammar"
```

---

### Task 2: Verifier guards (duplicate names, ref-compatible copies)

**Files:**
- Modify: `crates/lilac-ir/src/lib.rs` (`Verifier::run`, `check_copy_place`; tests)
- Test: `crates/lilac-ir/src/lib.rs` (`mod tests`)

**Interfaces:**
- Consumes: `mangle` spelling (Task 1) only for test fixtures.
- Produces: `CopyPlace` accepts `ref X` ↔ `ref Y`; rejects `ref` ↔ value. Duplicate `LirFunc.name` → one `E070`.

- [ ] **Step 1: Write failing tests**

Add to `mod tests`:
```rust
#[test]
fn verify_rejects_duplicate_function_names() { /* two funcs named "dup"; expect E070 */ }

#[test]
fn verify_accepts_ref_to_ref_copy() { /* alloca ref #bit8 %a; alloca ref #byte_buffer %b; copy %a -> %b; expect Ok */ }
```
Keep `verify_rejects_copy_place_type_mismatch` (ref→value) passing.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p lilac-ir verify_rejects_duplicate_function_names verify_accepts_ref_to_ref_copy`
Expected: FAIL.

- [ ] **Step 3: Implement**

In `Verifier::run`, before/while iterating funcs, track seen names in a `HashSet`; a repeat emits `self.error(format!("duplicate function `@{}`", func.name), Span::dummy())`. In `check_copy_place`, compare references structurally: mismatch only when `is_ref(src_ty) != is_ref(dst_ty)`, or both non-ref and `src_ty != dst_ty`. Add a small `fn is_ref_ty(ty: &str) -> bool { ty.starts_with("ref ") }`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p lilac-ir`
Expected: PASS.

- [ ] **Step 5: Gates and commit**

Run: `cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace`
```bash
git add -A
git commit -m "Verify duplicate function names and ref-compatible copies"
```

---

### Task 3: `@ptr_add` leaf

**Files:**
- Modify: `crates/lilac-ir/src/lib.rs` (`builtin_sig`: add `@ptr_add`, `@lilac_abort`; tests)
- Modify: `crates/lilac-codegen-llvm/src/runtime.rs` (define `.lilac.ptr_add`)
- Modify: `crates/lilac-codegen-llvm/src/lib.rs` (`builtin_symbol`: `@ptr_add` → `.lilac.ptr_add`)
- Test: `crates/lilac-codegen-llvm/src/lib.rs` (unit test)

**Interfaces:**
- Consumes: `.lilac.` prefix (Task 1).
- Produces: `builtin_sig("@ptr_add") == Some(ins: ["ref #bit8", "#bitNative"], outs: ["ref #bit8"])`; `builtin_sig("@lilac_abort") == Some([], [])`. LLVM `.lilac.ptr_add` with signature `ptr(ptr, iNative)`.

- [ ] **Step 1: Write failing tests**

In `lilac-ir` tests: `builtin_sig("@ptr_add")` is `Some` with the given ins/outs. In `lilac-codegen-llvm` tests: emitted text contains `define ptr @.lilac.ptr_add(ptr %base, i64 %offset)` and a `ptrtoint`/`inttoptr` pair in its body.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p lilac-ir ptr_add; cargo test -p lilac-codegen-llvm ptr_add`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add `"@ptr_add" => (vec![Some("ref #bit8"), Some("#bitNative")], vec![Some("ref #bit8")])` and `"@lilac_abort" => (vec![], vec![])` to `builtin_sig`. In `runtime.rs` add a `ptr_add` emitter: `ptrtoint(base, native)` + `add(offset)` + `inttoptr(ptr)`, registered via `emit_runtime`. Map the symbol in `builtin_symbol`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p lilac-ir; cargo test -p lilac-codegen-llvm`
Expected: PASS.

- [ ] **Step 5: Gates and commit**

```bash
git add -A
git commit -m "Add @ptr_add leaf intrinsic"
```

---

### Task 4: `builtin_layout` and the runtime prelude

**Files:**
- Modify: `crates/lilac-ir/src/lib.rs` (`builtin_layout`; `check_field_layout` consults it; `pub mod runtime`)
- Create: `crates/lilac-ir/src/runtime.rs`
- Test: `crates/lilac-ir/src/runtime.rs` (`mod tests`), `crates/lilac-ir/src/lib.rs` (field-layout test)

**Interfaces:**
- Consumes: `@ptr_add` (Task 3), `.` names (Task 1).
- Produces: `pub fn builtin_layout(ty: &str) -> Option<&'static [(&'static str, &'static str)]>` returning
  `#byte_buffer`: `[("len", "#bitNative"), ("data", "ref #bit8")]`,
  `#format_args`: `[("count", "#bitNative"), ("p0", "#byte_buffer"), … ("p15", "#byte_buffer")]`.
  `pub fn build_runtime(ptr_size: u32) -> Vec<LirFunc>` returning the seven `.lilac.*` functions, each a valid `LirFunc` that passes `verify`.

- [ ] **Step 1: Write failing tests**

In `lilac-ir`: `builtin_layout("#byte_buffer").unwrap()[0] == ("len", "#bitNative")`; `builtin_layout("#format_args").unwrap().len() == 17` and `[1] == ("p0", "#byte_buffer")`; a `Field` on `#byte_buffer` at idx 1 named `data` of type `ref #bit8` verifies. In `runtime.rs`: `build_runtime(8)` returns 7 funcs with names `.lilac.byte_buffer_from` … `.lilac.format_args_piece`; placing them in a `LirModule` and calling `verify` yields no diagnostics; the `format_args_push`/`format_args_piece` bodies contain a `Const` of `0x10` for `ptr_size == 8` and `0x8` for `ptr_size == 4`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p lilac-ir builtin_layout; cargo test -p lilac-ir runtime`
Expected: FAIL.

- [ ] **Step 3: Implement `builtin_layout`**

Add the table and a `check_field_layout` fallback: when `module.layouts` lacks `owner_ty`, look it up in `builtin_layout` before erroring. `valid_type` already accepts these names.

- [ ] **Step 4: Implement the prelude builder**

In `runtime.rs`, build each function with `LirFunc { name: ".lilac.<name>", ins, outs, is_extern: false, blocks }`. Bodies:
- `byte_buffer_from`: `alloca #byte_buffer %tmp`, `copy %len -> %tmp.len`, `copy %data -> %tmp.data`, `copy %tmp -> %out`, `return`.
- `byte_buffer_ptr`: `field %buffer .data -> &r`, `copy &r -> %out`, `return`.
- `byte_buffer_len`: `field %buffer .len -> &r`, `copy &r -> %out`, `return`.
- `format_args_new`: `alloca #format_args %tmp`, `copy %tmp -> %out`, `return`.
- `format_args_count`: `field %table .count -> &r`, `copy &r -> %out`, `return`.
- `format_args_push`: bounds-check `count < 16` (`branch.if`), then `field.addr %table .p0 -> &p0`, `alloca ref #bit8 %base`, `copy &p0 -> %base` (ref↔ref), `arith i.mul count, stride -> &off`, `call @ptr_add (in %base, &off) (out %slot)`, `alloca ref #byte_buffer %s`, `copy %slot -> %s`, `copy %piece -> ref %s`, `arith i.add count, 1 -> &n`, `copy &n -> %table.count`, `branch`, `return`. (`stride = 2 * ptr_size` as a `Const` of `#bitNative`; `%slot` is a `ref #bit8` place.)
- `format_args_piece`: `field %table .count -> &c`, `arith i.cmp.lt %index, &c -> &ok`, `branch.if &ok, $load, $trap`; `$trap`: `call @lilac_abort (in) (out)`, `unreachable`; `$load`: compute the slot exactly as in push (`%base`, `&off`, `%slot`, `%s`), then `load %s -> &v`, `copy &v -> %out`, `return`.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p lilac-ir`
Expected: PASS (prelude verifies; strides correct).

- [ ] **Step 6: Gates and commit**

```bash
git add -A
git commit -m "Add builtin aggregate layouts and LIR runtime prelude"
```

---

### Task 5: Runtime swap (inject, call as functions, delete hand-written bodies)

**Files:**
- Modify: `crates/lilac-driver/src/lib.rs` (`run_pipeline`: inject between `fold` and `prune_unreachable`)
- Modify: `crates/lilac-lower/src/lib.rs` (emit `LirCallee::Func(".lilac.<name>")` for the seven + `load_u8`/`store_u8`; drop them from the builtin name list)
- Modify: `crates/lilac-const/src/lib.rs` (`exec_builtin`/dispatch: match `.lilac.*` runtime names before body lookup)
- Modify: `crates/lilac-codegen-llvm/src/runtime.rs` (delete the seven hand-written bodies; keep leaves)
- Modify: `crates/lilac-codegen-llvm/src/lower.rs` (`lir_type` builds `#byte_buffer`/`#format_args` from `builtin_layout`; `lower_builtin_call` handles only leaves)
- Modify: `crates/lilac-ir/src/lib.rs` (`builtin_sig` shrinks to `@ptr_add`, `@memcpy`, `@lilac_abort`)
- Regenerate: all `tests/ui/*.lir`, `tests/ui/*.ll`

**Interfaces:**
- Consumes: `build_runtime` (Task 4), verifier guards (Task 2).
- Produces: modules carry the runtime only when reachable; `#format_args` is a 17-field LLVM struct.

- [ ] **Step 1: Write failing tests**

- `lilac-cli/tests` (extend `m5b.rs` or a new `runtime.rs`): emitting `emit-ir` for `arithmetic.lilac` yields text with no `.lilac.byte_buffer` / `.lilac.format_args`; `string.lilac` yields `.lilac.byte_buffer_ptr`; `fstring.lilac` yields `.lilac.format_args_push` and `.lilac.format_args_piece`.
- `lilac-const` test: a `@compile_error` message built through `.lilac.byte_buffer_len` still folds.
- `lilac-driver` test: a module using no runtime verifies without the prelude surviving DCE.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p lilac-cli runtime; cargo test -p lilac-const; cargo test -p lilac-driver`
Expected: FAIL.

- [ ] **Step 3: Inject and convert call sites**

In `run_pipeline`, after `lilac_const::fold`, insert `lilac_ir::runtime::build_runtime(pointer_width)` into `lir.funcs`. Derive `pointer_width` the same way codegen does: 8 when the target triple's arch is `x86_64`/`aarch64`, else 4 (add a small helper on `Target` or reuse the driver's existing triple check). In `lilac-lower`, replace `LirCallee::Builtin(name)` for `@byte_buffer_*`, `@format_args_*`, `@load_u8`, `@store_u8` with `LirCallee::Func(format!(".lilac.{}", name.trim_start_matches('@')))`; keep `@memcpy`/`@lilac_abort`/`@ptr_add` as `Builtin`.

- [ ] **Step 4: Update the const interpreter**

Where the interpreter dispatches a `LirCallee::Func` call, match the `.lilac.byte_buffer_len`/`.lilac.byte_buffer_from`/`.lilac.byte_buffer_ptr`/`.lilac.load_u8`/`.lilac.store_u8` names to the existing byte-buffer logic (reusing the current `exec_builtin` bodies) **before** any function-body lookup, so it works during `fold` when the prelude is not yet injected. Only otherwise fall through to body interpretation.

- [ ] **Step 5: Delete hand-written runtime, rebuild aggregate types**

In `runtime.rs` keep only `ptr_add`, `memcpy`, `abort` and the `llvm.trap` declaration. In `lower.rs`, build `#byte_buffer`/`#format_args` LLVM types from `builtin_layout` (a 17-field struct for `#format_args`); `lower_builtin_call` handles only `@ptr_add`/`@memcpy`/`@lilac_abort`. Remove `@byte_buffer_*`, `@format_args_*`, `@load_u8`, and `@store_u8` from `builtin_sig`, leaving `@ptr_add`, `@memcpy`, `@lilac_abort`.

- [ ] **Step 6: Regenerate snapshots**

Rebuild `lilac-cli` and regenerate every `.lir`/`.ll` (same snippet as Task 1 Step 6). Confirm `arithmetic.*` lost all runtime text and `fstring.*`/`string.*` now show the LIR runtime bodies.

- [ ] **Step 7: Run gates**

Run: `cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace`
Expected: all pass, including the M5b host compile+run harness.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "Serve the runtime from LIR instead of the backend"
```

---

### Task 6: Version, spec, and integration

**Files:**
- Modify: `crates/lilac-ir/src/lib.rs` (`LILAC_IR_VERSION` → `"0.3"`)
- Modify: `IR_SPEC.md` (builtins, `copy` ref-compatibility, reserved names, runtime functions)
- Modify: `AGENTS.md` (note the `.lilac.` runtime namespace and that the runtime is LIR)
- Test: `crates/lilac-cli/tests/` (end-to-end: unused-runtime absent, used-runtime runs)

**Interfaces:**
- Consumes: everything above.
- Produces: IR version `0.3`; documented contract.

- [ ] **Step 1: Write the integration test**

Add a test asserting: `emit-llvm` for a no-runtime fixture contains no `.lilac.byte_buffer`/`.lilac.format_args`/`.lilac.ptr_add`; `emit-llvm` for `fstring.lilac` compiles and runs to exit 0 on the host (reuse `m5b.rs` helpers).

- [ ] **Step 2: Bump version and update docs**

Set `LILAC_IR_VERSION = "0.3"`. Update `IR_SPEC.md`: replace the builtin table with the three leaves; document `#byte_buffer`/`#format_args` layouts; state that `copy` permits `ref X`↔`ref Y`; document `.`-mangled reserved names and `.lilac.*` symbols. Update `AGENTS.md` toolchain/runtime note.

- [ ] **Step 3: Run gates**

Run: `cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Bump IR to 0.3 and document the LIR runtime"
```
