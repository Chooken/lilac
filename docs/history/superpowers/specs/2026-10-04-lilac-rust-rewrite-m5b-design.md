# Lilac Rust Rewrite — M5b Design (LLVM backend + `emit-llvm`)

> Scope: **M5b only** — the LLVM codegen core of `RUST_REWRITE_PLAN.md` §7's M5
> bullet. Normative IR: `IR_SPEC.md` v0.2 (`lilac_ir = "0.2"`). Prior records: M5a
> design (`docs/superpowers/specs/2026-10-04-lilac-rust-rewrite-m5a-design.md`);
> M0–M4 designs/plans under `docs/superpowers/`. Work stays on branch
> `rust-rewrite-m0`. M5c (second backend) is a separate spec→plan cycle.

## 1. Outcome / constraints / success

- Outcome: `lilac-codegen-llvm` implements a real LLVM backend with `inkwell`
  (`llvm18-1`, LLVM 18.1.8) over verified LIR. A new `lilac emit-llvm <file>
  [--check] [--target=] [-O*]` path prints (or snapshot-compares) textual LLVM IR.
  Every success `tests/ui/*.lilac` lowers, links via the toolchain `clang`, and
  **runs to exit 0** on the host; `.ll` snapshots pin the emitted IR, including
  zero-init and RAII drop calls. Optimization from `Session.optimize` is applied
  at lowering time.
- Constraints: (a) same gates as M0–M5a; (b) **only `lilac-codegen-llvm` links
  LLVM** — every other crate stays LLVM-free (`grep -r inkwell` clean outside
  it); (c) `unsafe_code = "forbid"` still holds (inkwell's public API is safe);
  (d) no `unwrap`/`expect`/`panic!` on user input (`bug!` for ICEs); (e) TDD per
  task, one commit per task; (f) std-only in all crates except the LLVM backend.
- Success: §7 exit criteria green.
- **Self-host readiness (binding):** the backend's *product* is **LLVM IR**, not
  "calls into inkwell". inkwell is confined to `lilac-codegen-llvm` and used only
  as a type-correct IR builder + `opt` pass runner; no JIT, no inkwell-only
  features. When the compiler is rewritten in Lilac, this crate is replaced by
  either (i) a Lilac backend that declares LLVM's stable **C API** through
  `extern`, or (ii) a Lilac `.ll`-text emitter that shells out to `clang`/`llc`.
  Neither touches the front end or `lilac-ir`.

### 1.1 Decisions locked during brainstorming

1. **Acceptance = compile+run AND `.ll` snapshots.** Every success program
   compiles to a host executable, runs to exit 0, and has a committed `.ll`.
2. **In-module LLVM runtime.** `@byte_buffer_*`, `@format_args_*`, `@load_u8`,
   `@store_u8`, `@memcpy` are emitted as LLVM functions into each module
   (`@memcpy` → `llvm.memcpy`). No external runtime library.
3. **Surface.** `Backend` gains `emit_llvm(&self, &LirModule) -> Result<String,
   String>` (default `Err`); `LlvmBackend` implements it; `codegen` stays
   reserved. Tests link via the toolchain `clang`.
4. **Target + entry.** Effective triple is `--target` if given, else
   `Session.host`; lowering uses it so `@bitNative` matches. LIR `@main` is
   emitted as `lilac_main`; a generated `i32 @main()` calls it and returns 0.
5. **inkwell now, replaceable later** (self-host readiness above).
6. **Optimization.** `Session.optimize` (`O0` default … `Os`) drives LLVM's
   `default<O*>` pass pipeline. Snapshots are always `-O0`.

### 1.2 What is already in place (verified before design)

- `lilac-codegen::Backend` exists as a 3-method trait; `lilac-codegen-llvm` is a
  `LlvmBackend` stub whose `codegen` returns `Err("LLVM codegen lands in M5"…)`.
  Neither links LLVM yet; both crates are dependency-light.
- Verified LIR is concrete, string-typed, and carries everything codegen needs:
  `LirModule{version,target,data,funcs,layouts}`; `LirFunc{name,ins,outs,blocks,
  is_extern}`; `LirInst` (17 variants) with per-instruction spans; `LirTerm`
  (5 variants). `alloca … : zeroed` is the only place allocation appears; `drop`
  is a marker and the `@on_drop` call precedes it (M4/M5a).
- `IR_SPEC.md` fixes builtin aggregate meanings: `#byte_buffer` =
  `{len: #bitNative, ptr}`; `#format_args` = an inline piece table of at most 16
  pieces; and the builtin signature table (`IR_SPEC.md` §"Builtins").
- `tests/ui/` holds 45 success `.lilac`/`.lir` pairs; every success program
  defines `main: function () nothing`. Success `.lir` calls only
  `@format_args_new`/`@format_args_push`/`@byte_buffer_ptr` (the rest of the
  builtin set must still exist for completeness/future tests).
- Toolchain: LLVM 18.1.8 dev tree extracted to `%USERPROFILE%\llvm-18.1.8`
  (has `bin\llvm-config.exe`, `include\llvm`, component `.lib`s); `clang.exe` and
  `lld` present. `inkwell 0.10`/`llvm-sys 181.3.0` were probed end-to-end. The
  release omits `libxml2s.lib` (referenced only by `LLVMWindowsManifest`); a
  link-time stub was added at `<tree>\lib\libxml2s.lib`.

## 2. Architecture (plan §3, M5b slice)

```text
crates/
  lilac-codegen/        # M5b: Backend gains emit_llvm (default Err); no LLVM dep
  lilac-codegen-llvm/   # M5b: inkwell impl: types, places, insts, runtime, entry
  lilac-cli/            # M5b: `emit-llvm` subcommand
  lilac-{span,..,ir,const,lower,driver}/  # unchanged
```

- **Single new implementation: `lilac-codegen-llvm`.** It consumes a verified
  `LirModule` and produces one LLVM `Context`/`Module`. No pipeline changes; the
  CLI calls `Session::compile_lir` then the backend.
- **Product is IR text.** `emit_llvm` builds the module, runs `Module::verify`,
  optionally runs passes, and returns `module.print_to_string()`. A future Lilac
  backend can return the same string.

## 3. Components

### 3.1 `Backend` trait and `LlvmBackend` (`lilac-codegen`, `lilac-codegen-llvm`)

- `Backend` gains:
  `fn emit_llvm(&self, _module: &LirModule) -> Result<String, String> { Err(format!(\"backend `{}` cannot emit LLVM IR\", self.name())) }`.
  Existing `name`/`codegen`/`build_exe_and_run` are unchanged.
- `LlvmBackend { triple: String, optimize: Opt }` with
  `LlvmBackend::new(triple: &str, optimize: Opt)`, and `impl Backend`:
  `name → "llvm"`, `emit_llvm → build + verify + print`.
- `Cargo.toml`: `inkwell = { version = "0.10", features = ["llvm18-1"] }`.
  This is the sole LLVM linkage in the workspace.

### 3.2 Type, place, and instruction lowering (`lilac-codegen-llvm`)

- **Types** (`lir_ty` string → `inkwell` type):
  - `#bit8/16/32/64` → `i8/16/32/64`; `#bitNative` → `i64` when the triple is
    64-bit (x86_64/aarch64), else `i32`; `#ptr` / `ref #t` → `ptr`.
  - `#byte_buffer` → `{ iNative, ptr }`; `#format_args` →
    `{ iNative, [16 x { iNative, ptr }] }` (a fixed 264-byte value on 64-bit).
  - user `#Name` → a named struct built from `module.layouts["#Name"]` in order
    (enum layouts already carry a leading `tag: #bit8` entry).
  - `#str`/`#numlit` never appear as value types in verified IR (they are `data`
    kinds); encountering one is an ICE.
- **Places, registers, and the calling convention.** Every LIR place is a
  `ptr`. A local `alloca #t %p : zeroed` becomes an `alloca t` plus a zero store
  (`store zeroinitializer` — observable zero-init). A function's `in`/`out`
  places are the **incoming pointer arguments** of
  `define void @f(ptr %in0, …, ptr %out0, …)`, in declaration order (all places
  passed by address). `&r` names are SSA values from the defining instruction.
  `addr %p` is the pointer `%p` itself; `copy … -> %p` stores; reading a place
  loads. At a `call`, the caller passes the address of each listed `in` then
  `out` place, in order. This pointer ABI applies uniformly to direct, indirect,
  and `lilac_main` functions.
- **Instructions** (all 17 variants): `CopyReg`/`CopyPlace` → `load`/`store` as
  needed; `Const` → integer/float/immediate or, for `lit` beginning `!`, a
  data-global address; `Load`/`Addr`/`AddrFunc` → load/GEP/function pointer;
  `Call` → `build_call` (direct `LirCallee::Func`, indirect `Reg`, `Builtin` →
  the runtime symbol); `Field`/`StoreField`/`FieldAddr` → GEP + load/store at the
  recorded field `idx`; `Convert` → `trunc`/`zext`; `Arith` → the matching
  `build_int_*`/`build_float_*`/`build_int_compare`/`build_float_compare`;
  `Drop` → no-op (its `@on_drop` call is a preceding real `Call`); `Alloca` as
  above; `CompileError`/`FoldIntrinsic` are post-fold ICEs.
- **Terminators:** `return` → `ret void`; `branch`/`branch.if` → `build_conditional_branch`
  / `build_unconditional_branch`; `panic` → `call @lilac_abort` (an emitted
  `llvm.trap`/`abort` shim) with the message global; `unreachable` → `build_unreachable`.
- **Data:** each `!c: str|numlit "…"` becomes a private global byte array; a
  `const !c` of `#byte_buffer` yields `{len, ptr @global}`.
- **Determinism:** data globals, functions (source order), and runtime functions
  are emitted in a fixed order, so `print_to_string()` is byte-stable for a
  pinned LLVM.

### 3.3 In-module runtime (`lilac-codegen-llvm`)

Emitted once per module, in fixed order, matching `IR_SPEC.md` signatures:

| function | LLVM body |
|----------|-----------|
| `@byte_buffer_from(ref i8, iNative) -> {iNative, ptr}` | build the struct |
| `@byte_buffer_ptr({iNative,ptr}) -> ref i8` | extract field 1 |
| `@byte_buffer_len({iNative,ptr}) -> iNative` | extract field 0 |
| `@load_u8(ref i8, iNative) -> i8` | GEP + load |
| `@store_u8(ref i8, iNative, i8)` | GEP + store |
| `@memcpy(ref i8, ref i8, iNative)` | `llvm.memcpy` |
| `@format_args_new() -> #format_args` | zero the table, `count = 0` |
| `@format_args_push(ref #format_args, #byte_buffer)` | append if `count < 16` |
| `@format_args_count(#format_args) -> iNative` | return `count` |
| `@format_args_piece(#format_args, iNative) -> #byte_buffer` | `pieces[i]` |
| `@lilac_abort()` | `llvm.trap` |

### 3.4 Entry, target, optimization

- **Effective triple:** `emit-llvm` uses `--target` if the user passed it, else
  `Session.host.triple` (on this host `x86_64-pc-windows-msvc`). The CLI lowers
  (`compile_lir`) with that same target so `@bitNative` width matches the
  backend's data layout. `check`/`emit-ir` keep the canonical
  `x86_64-unknown-none` header unchanged.
- **Entry:** when the module has a function named `main`, emit it as
  `lilac_main` and append `define i32 @main() { call void @lilac_main(); ret i32 0 }`;
  a program without `main` gets no wrapper (snapshot only).
- **Optimization:** map `Opt` to inkwell's `OptimizationLevel`/pass pipeline
  (`O0` = none, `O1..O3`, `Os`), run `Module::run_passes` before printing. Every
  `.ll` snapshot is `-O0`; the run tests additionally exercise `-O2` (exit 0).

### 3.5 CLI (`lilac-cli`)

- New subcommand `emit-llvm <file> [--check] [--target=] [-O*] [--release]`.
  Behavior mirrors `emit-ir`: without `--check` it prints the IR to stdout;
  with `--check` it byte-compares against `<file>.ll` and exits 1 on mismatch.
- `parse_common` records whether `--target` was explicitly set, so the
  `emit-llvm` default (`Session.host`) does not affect `check`/`emit-ir`.
- Codegen invariants are ICEs (`lilac_error::bug!` / an internal-error message),
  never user diagnostics: verified LIR must lower.

### 3.6 Build/toolchain configuration

- A **gitignored `.cargo/config.toml`** sets
  `[env] LLVM_SYS_181_PREFIX = "<LLVM 18.1.8 tree>"` so cargo finds LLVM from any
  shell (env vars set after a shell starts are not inherited by it). `AGENTS.md`
  documents this, the required LLVM 18.1.8 install, and the `libxml2s.lib` stub.
- `lilac-codegen-llvm` has a `build.rs` only if needed to surface a clear error
  when `LLVM_SYS_181_PREFIX` is unset; otherwise llvm-sys' own error suffices.

## 4. Spec decisions (normative patches)

- **`IR_SPEC.md`** stated `#byte_buffer`/`#format_args` abstractly; M5b fixes the
  concrete LLVM layout in this spec (not in `IR_SPEC.md`, which stays
  language-level). No `IR_SPEC.md` version bump.
- **`SPEC.md`**: no change (no language semantics).
- **`docs/errors/`**: no new codes. Codegen failures are ICEs.
- Backend output is LLVM IR text; `LIR_CODEC_VERSION` and `.lir` snapshots unaffected.

## 5. Data flow / error handling / testing

- Flow: `read file → Session(target=host unless --target) → lex…→ lower → fold →
  prune → verify → LlvmBackend::emit_llvm → verify module → [opt] → print/snapshot
  → (tests) clang link+run`.
- Errors: lowering/verification failures are ICEs; missing toolchain is a clear
  build-time error. No new user diagnostics.
- Tests:
  - **`.ll` snapshots** for every success `tests/ui/*.lilac`; a `m5b.rs` harness
    mirrors `m4.rs`/`m5a.rs` with `emit-llvm --check` over all `*.ll`.
  - **Compile+run:** for each success program, write the emitted `.ll`, invoke
    the toolchain `clang` to produce an executable, run it, assert exit 0. Also
    run the `-O2` variant.
  - **zero-init + drops:** a snapshot (e.g. `generic_hook_drop.ll`) showing the
    zero store and the `@on_drop` call.
  - **`lilac-codegen-llvm` unit tests:** `lir_ty` mapping for every LIR type;
    place allocation; a minimal hand-built module lowering to expected IR
    fragments; the runtime helper set present.
  - **No-LLVM-leak gate:** `grep -r inkwell crates/lilac-{span,…,typeck,ir,const,lower,driver}` empty.

## 6. Ports + exit criteria

- Generate `tests/ui/*.ll` snapshots for the 45 success programs; add
  `crates/lilac-cli/tests/m5b.rs` and backend unit tests.
- Exit:
  1. `lilac emit-llvm <file>` prints verifiable LLVM IR; `--check` is byte-stable
     for every success program.
  2. Every success program links (toolchain `clang`) and runs to exit 0 at `-O0`
     and `-O2`.
  3. `generic_hook_drop.ll` shows zero-init and the `@on_drop` call.
  4. The runtime builtin set is emitted and complete.
  5. `cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace` green;
     `unsafe_code = forbid` intact; `inkwell` grep clean outside
     `lilac-codegen-llvm`.
  6. Human review of this spec and its plan.

## 7. Self-review

- Placeholders: none — every type mapping, runtime function, CLI flag, and test
  names its owner.
- Consistency: §1.1's decisions match §3.1–3.6; §3.2's layouts match the real
  `fstring.lir` usage; §3.4's host-triple default matches M5a's canonical
  `emit-ir`; §1's self-host note matches §3.1's "product is IR text".
- Scope: strictly M5b — the second backend (M5c), `lilac build`/caching, and a
  Lilac re-implementation are out.
- Ambiguity: type map, `#format_args` layout, entry wrapper, triple resolution,
  opt mapping, and test harness are decided here.
