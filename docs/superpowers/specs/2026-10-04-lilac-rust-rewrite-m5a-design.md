# Lilac Rust Rewrite — M5a Design (Session/target, portable LIR codec, extern IR, DCE)

> Scope: **M5a only** — the backend-agnostic slice of `RUST_REWRITE_PLAN.md` §7's M5
> bullet, plus the reserved `Session` shape from §10.4 and the extern coverage
> promised by §12. Normative language/IR docs: `SPEC.md`, `IR_SPEC.md` v0.2
> (`lilac_ir = "0.2"`). Prior records: M0 design
> (`docs/superpowers/specs/2026-10-01-lilac-rust-rewrite-m0-design.md`), M1 plan
> (`docs/superpowers/plans/2026-10-01-m1-types.md`), M2 design
> (`docs/superpowers/specs/2026-10-02-lilac-rust-rewrite-m2-design.md`), M3 design
> (`docs/superpowers/specs/2026-10-04-lilac-rust-rewrite-m3-design.md`), M4 design
> (`docs/superpowers/specs/2026-10-04-lilac-rust-rewrite-m4-design.md`).
> Work stays on branch `rust-rewrite-m0`. M5b (LLVM) and M5c (second backend) are
> separate spec→plan cycles.

## 1. Outcome / constraints / success

- Outcome: the compiler grows the **host/target + optimization + cache plumbing**
  that M5b/M5c need, gets a **dependency-free serialized form of `LirModule`** for
  future cache hits, actually **emits `extern` declarations into IR** (parsed and
  typechecked today but flattened to an empty body), and applies the **post-fold
  dead-code sweep** deferred from M3. No LLVM is linked.
- Constraints: (a) same gates as M0–M4 — `cargo fmt --check`,
  `clippy -D warnings`, `cargo test` green after every task; (b) frontend crates
  never import `inkwell`/`llvm-sys`; (c) no `unwrap`/`expect` on user input,
  `bug!()` for ICEs only; (d) **std only, and specifically no `serde`** — the
  language is intended to self-host, and a Rust-only derive would not port; (e)
  TDD per task, one commit per task; (f) no new IR instruction shapes.
- Success: §7 exit criteria all green.

### 1.1 Decisions locked during brainstorming

1. **M5 is split** into M5a/M5b/M5c, one spec→plan cycle each, in dependency
   order. This document owns M5a only.
2. **No `serde`.** Serialization is a **hand-written, deterministic,
   dependency-free binary codec** in `lilac-ir` (option A). A portable wire format
   can be reproduced in Lilac later; a proc-macro cannot.
3. **LLVM 18.1** (M5b) — `inkwell = "0.10"`, feature `llvm18-1`,
   `llvm-sys` `LLVM_SYS_181_PREFIX`. Recorded here so M5b does not re-litigate it.
4. **DCE is included in M5a**, not deferred. The deferred M3 reachability sweep
   lands now so LIR is clean before codegen; affected `.lir` snapshots are
   regenerated (and must round-trip through the codec).
5. **Extern is fixed now.** `IR_SPEC.md` already documents `extern @name in … out …`
   as normative, but lowering currently loses `is_extern` and emits a body; M5a
   makes the implementation match the spec.
6. **`Opt` is plumbed but inert in M5a.** The CLI accepts the flags and
   `Session.optimize`/`cache_dir` exist; only M5b consumes them.

### 1.2 What is already in place (verified before design)

- `Session` already has `host: Target`, `target: Target`, `opts: Opts{verbose,
  error_format}`, `warn_on_operator_type_change`. `Target::host()` currently
  hardcodes `x86_64-unknown-none`; `Session::new()` leaves `host`/`target` at
  their defaults, and `run_pipeline` passes `self.target.triple` to `lower`.
- `lilac-cli` parses `--target=`, `--verbose`, `--error-format=`,
  `--allow operator-type-change`; unknown `--` flags are rejected.
- `extern` is supported end-to-end in the frontend: lexer `KwExtern`, parser
  `is_extern` (with no-body enforcement), `ResKind::Func.is_extern`
  (`lilac-resolve`), `CheckedBody::Extern` (`lilac-typeck`). **`lilac-hir`'s
  `HirFunc` has no `is_extern`**, so the flag dies at the HIR boundary;
  `lilac-ir`'s `LirFunc`/printer have no extern form and
  `lilac-lower` never reads the flag. Net effect: an `extern` lowers to an
  ordinary empty-bodied function.
- `lilac-ir` is `#[derive(Clone, Debug)]` only — no serialization today.
  `LirModule.layouts` is a `HashMap<String, Vec<(String, String)>>` (the one
  unordered container).
- `lilac-const::fold` prunes dead *blocks*, dead `@compile_error`s, and
  unreachable *blocks within live functions*, but deliberately leaves dead
  functions and dead `data` consts (M3 plan note).
- `tests/ui/` holds 59 `.lilac` and 44 `.lir`; `emit-ir --check` is byte-exact.

## 2. Architecture (plan §3, M5a slice)

```text
crates/
  lilac-span/       # unchanged (Span is plain (u32,u32,u32); no derives added)
  lilac-hir/        # M5a: HirFunc.is_extern
  lilac-lower/      # M5a: propagate is_extern; extern funcs get no blocks
  lilac-ir/         # M5a: LirFunc.is_extern + printer/verifier; binary codec;
                    #      prune_unreachable()
  lilac-const/      # unchanged (fold/prune_dead stay block-level)
  lilac-driver/     # M5a: Opt, Session.optimize/cache_dir, host detection;
                    #      call prune_unreachable after fold, before verify
  lilac-cli/        # M5a: -O0..-O3/-Os, --release, --cache-dir=
  lilac-codegen*/   # untouched
```

- **No new pipeline stage.** DCE is a call the existing `run_pipeline` makes
  (`fold → prune_unreachable → verify`); extern is a field carried through
  existing passes; session/opts is configuration.
- **The codec is library-only.** No CLI surface; it is exercised by tests and
  reserved for the post-v1 content-hashed cache (`.lilac-cache/`, plan §10.4).

## 3. Components

### 3.1 Session, targets, options (`lilac-driver`, `lilac-cli`)

- New `pub enum Opt { O0, O1, O2, O3, Os }` with `Default = O0`. `Session` gains
  `optimize: Opt` and `cache_dir: Option<PathBuf>`. Neither changes today's
  output; `optimize` is read by M5b's LLVM pass pipeline, `cache_dir` by the
  post-v1 cache.
- **Host detection.** `Target::host()` builds a triple from
  `std::env::consts::{ARCH, OS}` (no deps):
  `x86_64|aarch64|x86` × `windows→pc-windows-msvc`, `linux→unknown-linux-gnu`,
  `macos→apple-darwin`, else `unknown-unknown-unknown`. `bit_native_width()` =
  64 for `x86_64`/`aarch64`, 32 otherwise (unchanged semantics, now derived from a
  real triple). `Session::new()` sets `host = Target::host()` (the real build
  host) but leaves `target = Target::default()` = the canonical
  `x86_64-unknown-none`, so default `emit-ir` output stays host-independent and
  committed snapshots remain portable; `--target=<triple>` overrides **`target`
  only** (host stays the build host). This is the plan's "never assume
  host == target".
- **CLI flags** (shared by `check`/`emit-ir`): `-O0`, `-O1`, `-O2`, `-O3`, `-Os`,
  `--release`, `--cache-dir=<path>`. Each optimization flag simply assigns
  `session.optimize`; the **last one parsed wins**, and `--release` is exactly
  `-O2` (a plain synonym, no separate "release" state). Usage string updated.
  `check`/`emit-ir` text output is unchanged.

### 3.2 Portable LIR codec (`lilac-ir`, no dependencies)

A hand-written binary codec over the verified `LirModule`:

- `pub const LIR_CODEC_VERSION: u32 = 1;`
- `impl LirModule { pub fn encode(&self, w: &mut impl Write) -> io::Result<()>;
  pub fn decode(r: &mut impl Read) -> Result<LirModule, CodecError>;
  pub fn content_hash(&self) -> u64; }`
- **Wire format** (all integers little-endian): magic `b"LILACIR\0"`, codec
  `u32`, IR version string, target string, then `data`, `funcs`, `layouts`.
  Strings are `u32` length + UTF-8 bytes; sequences are `u32` count + elements;
  `Span` is `file u32, lo u32, hi u32`; enums are a `u8` discriminant + payload;
  `bool` is `u8` (0/1). `layouts` is emitted **sorted by key** so the byte stream
  is deterministic regardless of `HashMap` iteration order.
- `content_hash` is a std-only FNV-1a 64-bit over the encoded bytes (documented
  as a cache key, not a security hash).
- `decode` must never panic on malformed input: bad magic/version/discriminants
  or truncated reads return `CodecError::Malformed(&'static str)` /
  `CodecError::Io`. It reconstructs a `LirModule` only; the caller runs
  `lilac_ir::verify` afterward for semantic checks.
- `to_text()` is unaffected; the binary form is **not** part of `IR_SPEC.md` (it
  is a `lilac-ir` API, not IR text).

### 3.3 Extern IR (`lilac-hir`, `lilac-lower`, `lilac-ir`)

- `HirFunc` gains `pub is_extern: bool`; `lilac-typeck` sets it from
  `CheckedBody::Extern` (`check.rs` and `check/generics.rs` both construct
  `HirFunc` and must pass it).
- `LirFunc` gains `pub is_extern: bool`. Lowering an extern emits only the
  signature — no `$entry` block. A non-extern function is unchanged.
- **Printer** (`LirModule::to_text`) emits, per `IR_SPEC.md`:
  `extern @name` followed by the `in`/`out` lines and **no braces**. Verifier
  gains the matching rule: an extern has no blocks and no terminator; a non-extern
  still requires exactly one terminator per block. Calls and `addr.func` to an
  extern keep today's arity/type rules (they read the signature only).
- `IR_SPEC.md` extern text is already normative; no wording change, hence no
  version bump. The change is behavior catching up to the spec.

### 3.4 Dead-code elimination (`lilac-ir`)

- New `pub fn prune_unreachable(module: &mut LirModule)`:
  - **Roots**: every function named `main`; plus every function target of
    `Call`/`AddrFunc` reachable from a root (worklist). `AddrFunc` targets are
    roots (indirect-call safety, per the M3 note). If no `main` exists, the pass
    is a no-op (conservative).
  - Remove unreachable `funcs`. Then remove `data` consts no surviving
    instruction references; **retain** any const still named by a surviving
    instruction.
  - `layouts` is verifier-only and never printed, so it is left untouched (no
    observable effect; avoids needing a layout-reference analysis).
- Called from `Session::run_pipeline` **after** `lilac_const::fold`, **before**
  `lilac_ir::verify`, so verification sees the pruned module.
- Non-normative: purely a size/cleanliness pass. The known caveat that an
  unfoldable `@as_int` in a *dead* function still reports `E060` (fold runs
  first) is unchanged and documented.

## 4. Spec decisions (normative patches)

- **`IR_SPEC.md`**: no change. Extern is already documented; this milestone
  implements it. The binary codec is a `lilac-ir` API and tooling detail, not IR
  text, so it is deliberately absent from `IR_SPEC.md`.
- **`SPEC.md`**: no change (targets/optimization are toolchain configuration, not
  language semantics).
- **`docs/errors/`**: no new codes. DCE/`Opt`/codec add no user-facing diagnostic.
- **No new IR instructions**; extern is a declaration form already specified.

## 5. Data flow / error handling / testing

- Flow: `read file → Session(host,target,optimize,cache_dir) → lex → parse →
  resolve → collect_sigs → check_bodies → lower (extern bodyless) → fold →
  prune_unreachable → verify → check|emit-ir`. Codec is out of band
  (`encode`/`decode`/`content_hash`), used by tests and the future cache.
- Errors: malformed codec input returns `CodecError` (never panics); extern
  misuse continues to use existing typecheck diagnostics; DCE is infallible.
- Tests:
  - **Driver/CLI unit tests**: `Opt` parsing (`-O0..-O3`, `-Os`, `--release`,
    last-wins), `--cache-dir=`; `Target::host()` triple/`bit_native_width` for a
    known arch; `--target=` overrides `target` not `host`.
  - **Codec unit tests** (`lilac-ir`): hand-built module round-trips
    (`decode(encode(m))` → equal `to_text()`); `content_hash` stable across two
    encodes; `decode` of truncated/bad-magic/bad-tag bytes returns `Malformed`
    without panicking; `layouts` key order does not affect bytes.
  - **Codec integration** (`lilac-cli/tests/m5a.rs`): for every success
    `tests/ui/*.lilac`, compile → `encode → decode → to_text()` byte-equal.
  - **Extern**: `tests/ui/extern_decl.lilac` + `.lir` (a declaration + a call)
    renders `extern @…` bodyless and round-trips `--check`; a verifier unit test
    rejects an extern that somehow carries a block, and confirms a call arity
    mismatch still errors.
  - **DCE**: a fixture with a folded-away caller leaving a dead function/data
    asserts they are gone; `AddrFunc` targets survive; a module without `main`
    is unchanged. Existing `.lir` snapshots that lose dead items are regenerated.
  - Harness `m5a.rs` mirrors `m4.rs` (`PORTS` + `check_ports_clean` +
    `lir_snapshots_stable` + `fails_with`).

## 6. Ports + exit criteria

- Add `tests/ui/extern_decl.lilac` + `.lir`; regenerate the `.lir` snapshots
  affected by DCE and extern; add `crates/lilac-cli/tests/m5a.rs`.
- Exit:
  1. `Session`/`Target`/`Opt`/`cache_dir` and the CLI flags exist; `check` and
     `emit-ir` human output is byte-identical to pre-M5a **except** where DCE
     removed dead items from snapshots.
  2. Codec round-trips every success LIR (encode→decode→`to_text()` equal);
     `content_hash` deterministic; malformed input returns an error, never
     panics.
  3. `extern_decl` emits a bodyless `extern @…`; verifier enforces the rule; the
     snapshot is byte-stable via `emit-ir --check`.
  4. DCE removes unreachable functions and unreferenced data; `AddrFunc` targets
     are retained; no-`main` modules are untouched.
  5. `cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace` green;
     frontend `inkwell` grep empty.
  6. Human review of this spec and its plan.

## 7. Self-review

- Placeholders: none — every field, function, format, flag, and test names its
  owner crate.
- Consistency: §1.1's no-`serde`/DCE/extern decisions match §3.1–3.4 and §6;
  §3.3's extern shape matches existing `IR_SPEC.md`; §3.4's roots match the M3
  note's `AddrFunc` caveat; §3.1's host/target split matches plan §10.4.
- Scope: strictly M5a — LLVM/`emit-llvm`/`-O` application (M5b), the second
  backend (M5c), and full `lilac build`/cache execution (post-v1) are out.
- Ambiguity: codec wire format, DCE roots and ordering, extern representation,
  `--release` mapping, and host-triple mapping are decided here, not left to the
  implementer.
