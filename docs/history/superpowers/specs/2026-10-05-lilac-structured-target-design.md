# Lilac — Structured Target Design (`lilac-target`)

> Scope: replace the stringly-typed target with a Zig-style structured `Target`
> (arch/vendor/os/abi enums) in a new leaf crate `lilac-target`, and move the
> pointer-width rule there. Driver/CLI-only reach: the `.lir` header and the LLVM
> backend keep receiving a canonical triple **string**. This is the first of three
> independent follow-ups (structured targets → pass-boundary LIR dumps →
> opt-level naming). Work stays on branch `rust-rewrite-m0`; no codec/snapshot
> change. Normative IR unchanged (`IR_SPEC.md` v0.3, `lilac_ir = "0.3"`).

## 1. Outcome / constraints / success

- Outcome: `--target=` parses into a typed `Target { arch, vendor, os, abi }` of
  `Copy` enums; unknown components are rejected up front (exit 2) instead of
  being handed to LLVM. A single width rule (`Arch::is_64_bit`) drives both the
  driver-baked `format_args` stride and the backend's `@bit_native` width.
- Constraints: (a) same gates as M0–M5b (`fmt`, `clippy -D warnings`,
  `test --workspace`); (b) std-only — the new crate adds no dependency; (c) no
  `unwrap`/`expect`/`panic!` on user input (`bug!` for ICEs); (d) `unsafe_code =
  "forbid"` and the workspace lints apply to the new crate; (e) TDD per task, one
  commit per task; (f) `inkwell`/`llvm-sys` stay confined to
  `lilac-codegen-llvm`.
- Success: §6 exit criteria green.
- **Snapshot stability (binding):** no committed `.lir` or `.ll` changes. The
  `.lir` header (`target = "..."`) and `LlvmBackend::new(&str)` still take a
  string; `to_triple()` reproduces every triple the repo currently emits.

### 1.1 Decisions locked during brainstorming

1. **New crate, driver/CLI-only reach.** The structured type lives in a new
   `crates/lilac-target`; it does not flow into the LIR header or the backend API
   (LLVM needs a string regardless).
2. **Zig-faithful fields:** `arch`, `vendor`, `os`, `abi`.
3. **Strict parsing.** An unrecognized component is a `ParseError`; the CLI
   reports it and exits 2. No `Other(String)` round-trip variants.
4. **Enumerated, closed sets** (extendable later): see §3.2.
5. **Single width rule** moves to `lilac-target` (`is_64_bit_triple` leaves
   `lilac-ir`), keeping AGENTS.md's invariant.
6. **String boundary unchanged:** `.lir` header and LLVM backend keep strings;
   no `LIR_CODEC_VERSION` bump, no snapshot regeneration.

### 1.2 What is already in place (verified before design)

- `lilac_driver::Target` is `{ triple: String }` with `from_arch_os`,
  `host()`, `default()` (canonical `x86_64-unknown-none`), and
  `bit_native_width()` delegating to `lilac_ir::is_64_bit_triple`.
- `lilac_ir::is_64_bit_triple(&str)` (`lilac-ir/src/lib.rs:743`) is the single
  width rule: arch before the first `-`, 64-bit iff it contains `"64"`.
- `LirModule { target: String }` serializes `target = "..."` in the `.lir`
  header (`lilac-ir/src/lib.rs:33,410,424`).
- `LlvmBackend::new(triple: &str, optimize: OptLevel)` stores the string and
  calls `lilac_ir::is_64_bit_triple` (`lilac-codegen-llvm/src/lib.rs:26,96`).
- CLI `parse_common` copies `--target=<value>` verbatim into
  `session.target.triple` (`lilac-cli/src/main.rs:69-71`); `--target=foo` is
  currently accepted and only fails later inside LLVM.
- Triples the repo emits: `x86_64-unknown-none` (default), and via
  `from_arch_os`: `x86_64-pc-windows-msvc`, `x86-unknown-linux-gnu`,
  `aarch64-apple-darwin`. Width tests also use `riscv64-unknown-none` /
  `riscv32-unknown-none`.

## 2. Architecture

```text
crates/
  lilac-target/         # NEW leaf crate: Target + enums + parse/to_triple/host
                        #   + is_64_bit_triple (the width rule)
  lilac-driver/         # Target moves out; Session.target/host: lilac_target::Target
  lilac-cli/            # --target= parses via lilac_target::Target::parse
  lilac-codegen-llvm/   # imports is_64_bit_triple from lilac-target (was lilac-ir)
  lilac-ir/             # drops is_64_bit_triple; header/backends still string
```

Dependency edges (no cycles): `lilac-driver → lilac-target`,
`lilac-codegen-llvm → lilac-target (+ lilac-ir)`, `lilac-ir → (unchanged)`.
`lilac-target` is a leaf: std only.

## 3. Components

### 3.1 `lilac-target` crate

- New workspace member `crates/lilac-target` (`Cargo.toml` inherits the
  workspace `[lints]`). Single module `src/lib.rs` (splitting later if it grows).
- Public exports: `Target`, `Arch`, `Vendor`, `Os`, `Abi`, `ParseError`, and
  `is_64_bit_triple`.
- No dependency on any other Lilac crate.

### 3.2 Types

```rust
pub struct Target { pub arch: Arch, pub vendor: Vendor, pub os: Os, pub abi: Abi }
```

All four enums and `Target` derive `Copy, Clone, PartialEq, Eq, Hash, Debug`.

| Enum | Variants (→ rendered component) |
|------|----------------------------------|
| `Arch` | `X86`(x86), `X86_64`(x86_64), `Arm`(arm), `Aarch64`(aarch64), `Riscv32`(riscv32), `Riscv64`(riscv64), `Wasm32`(wasm32), `Wasm64`(wasm64) |
| `Vendor` | `Unknown`(unknown), `Pc`(pc), `Apple`(apple) |
| `Os` | `None`(none), `Unknown`(unknown), `Linux`(linux), `Windows`(windows), `Macos`(darwin) |
| `Abi` | `None`(none), `Gnu`(gnu), `Msvc`(msvc), `Musl`(musl) |

- `Os::Macos` renders the LLVM-canonical component `darwin` (Zig's tag name is
  `macos`); this keeps `aarch64-apple-darwin` byte-identical to today.
- Each enum has `parse(&str) -> Option<Self>` and `as_str(&self) -> &'static str`
  (the latter used by `to_triple`).
- `Target::to_triple()` renders `arch-vendor-os`, appending `-abi` only when
  `abi != Abi::None`. Examples: `x86_64-unknown-none`,
  `x86_64-pc-windows-msvc`, `x86-unknown-linux-gnu`, `aarch64-apple-darwin`.

### 3.3 Parsing & errors

`Target::parse(&str) -> Result<Target, ParseError>`, strict on every component.

- Split on `-`; reject empty input and more than four components.
- By component count, matching components against the known sets:
  - **1** `arch` → vendor `Unknown`, os `None`, abi `None`.
  - **2** `arch-os` (vendor `Unknown`) — Zig's shorthand (`x86_64-linux`).
  - **3** try `arch-vendor-os` (comp1 a `Vendor` **and** comp2 an `Os`); else
    try `arch-os-abi` (comp1 an `Os` **and** comp2 an `Abi`, vendor `Unknown`).
    Trying vendor-os first resolves the `unknown` overlap.
  - **4** `arch-vendor-os-abi`.
- `ParseError` is an enum — `Empty`, `TooManyComponents`, `UnknownArch(String)`,
  `UnknownVendor(String)`, `UnknownOs(String)`, `UnknownAbi(String)` —
  implementing `Display`, naming the offending value.

### 3.4 Width rule (single source of truth)

- `Arch::is_64_bit()` is the only implementation: `X86_64`/`Aarch64`/`Riscv64`/
  `Wasm64` → `true`; `X86`/`Arm`/`Riscv32`/`Wasm32` → `false`.
- `is_64_bit_triple(&str)`: take the component before the first `-`, parse it
  with `Arch::parse`, return `is_64_bit()`; an unparseable arch falls back to the
  `"64"` substring heuristic (robustness only — `Target` always renders a
  parseable arch).
- `Target::bit_native_width()` = `self.arch.is_64_bit() ? 64 : 32`.
- The driver uses `bit_native_width()`; `lilac-codegen-llvm` keeps calling
  `lilac_target::is_64_bit_triple(&self.triple)` on the canonical string.

### 3.5 Host & default

- `Target::default()` = `{ X86_64, Unknown, None, None }` → `x86_64-unknown-none`
  (unchanged; keeps default `emit-ir` host-independent).
- `Target::host()` maps `std::env::consts::{ARCH, OS}`:
  - arch `x86_64`→`X86_64`, `x86`→`X86`, `aarch64`→`Aarch64`, `arm`→`Arm`;
  - os `windows`→`{Pc, Windows, Msvc}`, `linux`→`{Unknown, Linux, Gnu}`,
    `macos`→`{Apple, Macos, None}`.
  - An unrecognized host arch/os falls back to `Target::default()` (documented;
    CI hosts are covered).
- `Target::from_arch_os` is removed; its behavior folds into `host()`.

### 3.6 Driver & CLI integration

- `lilac-driver`: `Session.target`/`Session.host` become `lilac_target::Target`;
  `run_pipeline` passes `self.target.to_triple()` to `lilac_lower::lower` (which
  still takes `&str` for `LirModule::new`); the driver's `Target` module code is
  deleted and its tests move to `lilac-target`.
- `lilac-cli`: `parse_common` calls `Target::parse` for `--target=<value>`; on
  `Err`, prints `invalid --target=<value>: <msg>` and returns exit 2. `emit-llvm`
  still defaults to `session.host.to_triple()` when `--target` is absent.
  `usage()` text is unchanged (`--target=<triple>`).

## 4. Spec decisions (normative patches)

- **`AGENTS.md`:** update the pointer-width bullet — `is_64_bit_triple` now lives
  in `lilac-target` (not `lilac-ir`); the "single width rule" invariant stands.
- **`IR_SPEC.md`:** update any reference to `is_64_bit_triple`'s home. No version
  bump; the `.lir` header format is unchanged.
- **`SPEC.md`:** no change (no language semantics).
- **`docs/errors/`:** no new codes; a bad `--target` is a CLI usage error
  (exit 2), consistent with the other flag errors.
- No `LIR_CODEC_VERSION` bump and no snapshot regeneration.

## 5. Data flow / error handling / testing

- Flow: `--target=<s> → Target::parse (exit 2 on error) → Session.target →
  to_triple() → lower → fold → prune → verify → LlvmBackend::new(to_triple(), …)
  → is_64_bit_triple(&str)`.
- Errors: unknown target components are a CLI usage error (exit 2); internal
  inconsistencies remain `bug!`.
- Tests:
  - **`lilac-target` unit tests:** round-trip `parse(to_triple(t)) == t` over
    `x86_64-unknown-none`, `x86_64-pc-windows-msvc`, `x86-unknown-linux-gnu`,
    `aarch64-apple-darwin`, `x86_64-linux-gnu`, `x86_64-linux`,
    `riscv32-unknown-none`; the 3-component disambiguation cases; strict
    rejections (`foo`, `x86_64-foo-linux`, five components, empty); the all-`Arch`
    width-consistency test (`is_64_bit_triple(render(arch)) == arch.is_64_bit()`);
    `host()`/`default()` mappings.
  - **Moved tests:** driver `from_arch_os`/width/`riscv*` tests and
    `lilac-ir`'s `is_64_bit_triple` tests relocate to `lilac-target`.
  - **CLI:** `target_flag_overrides_target_not_host` switches to a valid triple;
    a new test asserts `--target=foo` exits 2.
  - **`lilac-codegen-llvm`:** only the import changes.
  - **Snapshot stability:** `cargo test --workspace` (m4/m5a/m5b checks) proves
    no `.lir`/`.ll` drift.

## 6. Ports + exit criteria

- Add `crates/lilac-target`; move `Target` out of `lilac-driver`; move
  `is_64_bit_triple` out of `lilac-ir`; rewire the CLI, driver, and backend.
- Exit:
  1. `--target=` parses into the typed `Target`; valid triples round-trip
     byte-identically; unknown components exit 2 with a clear message.
  2. Exactly one width rule; the all-`Arch` consistency test passes; `riscv32`
     vs `riscv64` widths unchanged.
  3. No `.lir`/`.ll` snapshot changes.
  4. `cargo fmt --check`, `clippy --workspace --all-targets -- -D warnings`,
     `cargo test --workspace` green; `unsafe_code = forbid` intact; no new
     dependency; `inkwell` grep clean outside `lilac-codegen-llvm`.
  5. `AGENTS.md`/`IR_SPEC.md` updated for the new home of the width rule.
  6. Human review of this spec and its plan.

## 7. Self-review

- Placeholders: none — every enum, method, error variant, test, and doc edit
  names its owner.
- Consistency: §1.1's decisions match §3.2–3.6; §3.4's single rule matches the
  AGENTS.md invariant; §3.2's rendering reproduces every triple in §1.2; §4's
  "no snapshot change" matches §1's binding success criterion.
- Scope: strictly the structured-target change — pass-boundary LIR dumps and
  opt-level naming are the next two independent cycles, not here.
- Ambiguity: the component-count rule, the `unknown` vendor/os overlap, the
  `Macos`→`darwin` rendering, strict-vs-lenient handling, and the host fallback
  are all decided here.
