# Lilac M6a — Folder Modules & `private` Visibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compile a whole folder namespace — multi-file folders, root-relative `using` imports, and `private` visibility at namespace and object-member level — with per-namespace symbol/type qualification.

**Architecture:** A new `lilac-module` loader walks the folder tree into the already multi-file `SourceMap`. `lilac-resolve` becomes a namespace tree (`ResolvedProject`) with per-file visible scopes and unit-global hooks, replacing the single global `Module` threaded through typeck and lowering. Names are qualified per namespace (§7.1 of the spec) so the flat LIR/type interners stay unique. Privacy is a check where member resolution happens, not a lookup filter (rustc's model).

**Tech Stack:** Rust (stable, std-only), the existing `lilac-*` crates, `cargo test --workspace`.

**Spec:** `docs/superpowers/specs/2026-10-06-lilac-modules-and-extern-c-abi-design.md`

## Global Constraints

- Stable Rust, std-only, no `serde`; workspace `unsafe_code = "forbid"`; `inkwell`/`llvm-sys` only in `lilac-codegen-llvm`.
- No stdlib; this plan is language/toolchain only.
- Root namespace adds **no** symbol/type prefix, so every existing single-file `.lir`/`.ll`/`.lbc` snapshot stays byte-identical.
- New diagnostic codes: `E025` (unresolved `using`), `E026` (private object member), `E029` (ambiguous imported name); `E020` extends to cross-file duplicates.
- No `unwrap`/`expect`/`panic!` on user input (`lilac_error::bug!` ICEs only).
- Gates per commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo test --release -p lilac-vm --features bench-baseline`.

## Review Focus

- **A file in a subfolder reading a root declaration** must work with no import (ancestor reach); a root file reading a subfolder declaration must not (no descendant reach upward).
- **Two namespaces declaring the same function/type name** must compile to distinct symbols/type names, not `E070` or a merged type.
- **An importer naming a namespace-private item** must be an unresolved name (Rust glob behavior), not a privacy error and not a silent success.
- **A private object member used from inside the object** must be allowed; from outside must be `E026`.
- **A `using` of a missing folder** must be `E025`, not a panic.
- **Import-vs-import ambiguity** must fire only on use (`E029`), and an unreferenced ambiguous name must not error.

---

### Task 1: `private` block syntax (AST + parser)

**Files:**
- Modify: `crates/lilac-ast/src/lib.rs` (`ItemKind`, `walk_item`)
- Modify: `crates/lilac-parse/src/lib.rs` (`parse_item`/`parse_item_tail`)
- Test: `crates/lilac-parse/src/lib.rs` (module tests)

**Interfaces:**
- Consumes: `TokenKind::KwPrivate` (already lexed, `crates/lilac-lex/src/lib.rs:42-43`).
- Produces: `ItemKind::Private { items: Vec<Item> }`.

- [ ] **Step 1: Write the failing test** — parse `private { helper: function () nothing {} }` at top level and inside `object { … }`, asserting `ItemKind::Private { items }` with one `Func`; and `private helper: function () nothing {}` (no brace) is `E001`.

```rust
#[test]
fn private_block_parses_items() {
    let (program, diags) = parse_src("private {\n    helper: function () nothing {}\n}\n");
    assert!(!diags.has_errors());
    let ItemKind::Private { items } = &program.items[0].kind else { panic!() };
    assert_eq!(items.len(), 1);
}
```

- [ ] **Step 2: Run it to verify it fails** — `cargo test -p lilac-parse private_block_parses_items` → FAIL (no `Private` variant).

- [ ] **Step 3: Implement** — add `ItemKind::Private { items: Vec<Item> }`; in the parser, when the next token is `KwPrivate`, expect `{`, parse items until `}`, and build the variant (mirroring `parse_block`'s brace handling and `parse_item`'s recursion); handle `KwPrivate` in `walk_item` by walking each contained item.

- [ ] **Step 4: Run** — `cargo test -p lilac-parse` → PASS; `cargo test --workspace` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "M6a: parse private blocks at item and object-member position"`.

---

### Task 2: `ResolvedProject` skeleton with a single-namespace constructor

**Files:**
- Modify: `crates/lilac-resolve/src/lib.rs`
- Test: `crates/lilac-resolve/src/lib.rs` (module tests)

**Interfaces:**
- Consumes: existing `Module`, `ResItem`, `collect`.
- Produces:

```rust
pub struct NsId(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility { Public, Private }
#[derive(Clone, Debug)]
pub enum Lookup { One(ResItem), Ambiguous(Vec<Span>) }
pub struct NamespaceData { pub path: String, pub parent: Option<NsId>, pub items: HashMap<String, ResItem> }
pub struct FileScope { pub file: FileId, pub ns: NsId, pub visible: HashMap<String, Lookup> }
pub struct ResolvedProject {
    pub namespaces: Vec<NamespaceData>,        // [0] = root
    pub hooks: HashMap<String, Vec<ResItem>>,
    pub files: Vec<FileScope>,
}
impl ResolvedProject {
    pub fn single(module: Module, file: FileId) -> Self;
    pub fn lookup(&self, file: FileId, name: &str) -> Option<&Lookup>;
    pub fn all_items(&self) -> impl Iterator<Item = (&str, &ResItem)>;
    pub fn symbol_of(&self, def: DefId) -> Option<String>;
}
```

- [ ] **Step 1: Write the failing test** — `ResolvedProject::single(collect(&program), file).lookup(file, "main")` returns `Lookup::One` for a one-file program, and `symbol_of(def)` returns the bare name (root, no prefix).

- [ ] **Step 2: Run it to verify it fails** — `cargo test -p lilac-resolve single_project_lookup` → FAIL.

- [ ] **Step 3: Implement** — add `name` to `ResItem` (needed by `symbol_of`) or index `(DefId → (NsId, name))` in `ResolvedProject`; `single` builds one root `NamespaceData` whose `visible` map has every item as `Lookup::One`; `symbol_of` returns the qualified name (root ⇒ bare).

- [ ] **Step 4: Run** — `cargo test -p lilac-resolve` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "M6a: add ResolvedProject with a single-namespace constructor"`.

---

### Task 3: Thread `ResolvedProject` through typeck (item lookup + type qualification)

**Files:**
- Modify: `crates/lilac-typeck/src/sigs.rs` (`collect_sigs`, `resolve_type`)
- Modify: `crates/lilac-typeck/src/check.rs` (`check_bodies`, `Checker`, `find_item`, `find_parent_name`)
- Modify: `crates/lilac-typeck/src/check/generics.rs` (the `module.items.values()` scans)
- Test: existing `cargo test --workspace` snapshots (unchanged) + one new unit test

**Interfaces:**
- Consumes: `ResolvedProject`, `Lookup` (Task 2).
- Produces: `collect_sigs(project: &ResolvedProject, program: &Program, types, diags, warn) -> Sigs`; `check_bodies(program: &Program, project: &ResolvedProject, sigs, types, diags) -> HirProgram`. `Checker` gains `project: &ResolvedProject` and `file: FileId` (set from each item's `span`).

- [ ] **Step 1: Write the failing test** — a driver-level test asserting a one-file program still compiles with no diagnostics after the signature change (the existing `hello_compiles` in `lilac-driver` is the canary; run it before/after).

- [ ] **Step 2: Run it to verify it fails** — `cargo build -p lilac-typeck` → FAIL (signature mismatch).

- [ ] **Step 3: Implement** — replace every `module.items.get(name)` with `self.lookup(name)` (returns `Option<&Lookup>`; `Lookup::One` behaves as before, `Lookup::Ambiguous` emits `E029` and poisons), replace `module.items.values()` scans in `generics.rs` with `project.all_items()`, and qualify a resolved type name via `project` before interning. Use `project.symbol_of(def)` where a definition's name is needed.

- [ ] **Step 4: Run** — `cargo test -p lilac-typeck` and `cargo test --workspace` → all existing snapshots PASS.

- [ ] **Step 5: Commit** — `git commit -m "M6a: resolve typeck names through ResolvedProject"`.

---

### Task 4: Thread `ResolvedProject` through lowering (symbol + layout qualification)

**Files:**
- Modify: `crates/lilac-lower/src/lib.rs` (`lower`, layout build, `func_names`)
- Modify: `crates/lilac-driver/src/lib.rs` (`run_pipeline` call sites)
- Test: existing `cargo test --workspace` snapshots (unchanged)

**Interfaces:**
- Consumes: `ResolvedProject::symbol_of`, `all_items` (Task 2).
- Produces: `lower(hir: &HirProgram, project: &ResolvedProject, types, diags, triple) -> Result<LirModule, ErrorGuaranteed>`.

- [ ] **Step 1: Write the failing test** — the canary is the existing `m5b`/`m5a` snapshot suites; build first.

- [ ] **Step 2: Run it to verify it fails** — `cargo build -p lilac-lower` → FAIL.

- [ ] **Step 3: Implement** — replace `module.items.get(name)` (enum detection, `lower.rs:42`) with a `project`-based kind lookup; replace `lower.func_names.insert(func.def, func.name.clone())` (`lower.rs:70`) with `project.symbol_of(func.def)`; key `hir.layouts`/`hir.drops` by qualified names (they already are, once Task 3 qualifies them).

- [ ] **Step 4: Run** — `cargo test --workspace` → all snapshots PASS.

- [ ] **Step 5: Commit** — `git commit -m "M6a: lower symbols and layouts through ResolvedProject"`.

---

### Task 5: `lilac-module` loader (root discovery + `using` fixpoint)

**Files:**
- Create: `crates/lilac-module/Cargo.toml`, `crates/lilac-module/src/lib.rs`
- Test: `crates/lilac-module/src/lib.rs` (module tests, in-memory `Fs`)

**Interfaces:**
- Consumes: `lilac-lex`, `lilac-parse`, `lilac-ast` (`ItemKind::Using`), `lilac-error`.
- Produces:

```rust
pub trait Fs {
    fn read(&mut self, path: &Path) -> io::Result<String>;
    fn list_dir(&mut self, dir: &Path) -> io::Result<Vec<PathBuf>>;
}
pub struct FileUnit { pub name: String, pub ns: usize, pub src: String, pub usings: Vec<String> }
pub struct Namespace { pub path: String, pub dir: PathBuf, pub files: Vec<usize> }
pub struct Project { pub root: PathBuf, pub namespaces: Vec<Namespace>, pub files: Vec<FileUnit> }
pub fn load(root: &Path, fs: &mut dyn Fs) -> Result<Project, ErrorGuaranteed>;
```

- [ ] **Step 1: Write the failing test** — a fake `Fs` with `main.lilac` (`using util`) and `util/helper.lilac`; assert `Project.namespaces` has root + `util`, `util` is reachable, and file order is sorted; a second test with a missing `using missing` returns an `E025` diagnostic and `Err`.

- [ ] **Step 2: Run it to verify it fails** — `cargo test -p lilac-module` → FAIL (crate absent).

- [ ] **Step 3: Implement** — read the root folder's `*.lilac` (sorted), lex/parse each only far enough to collect `ItemKind::Using` paths, resolve each root-relative (`A.B` → `<root>/A/B`), read newly-reached folders, repeat to a fixpoint (visit each folder once; cycles allowed). Missing/not-a-directory target emits `E025` at the `using` span.

- [ ] **Step 4: Run** — `cargo test -p lilac-module` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "M6a: add lilac-module folder loader with using fixpoint"`.

---

### Task 6: `collect_project` — namespace tree, per-file scopes, qualification

**Files:**
- Modify: `crates/lilac-resolve/src/lib.rs`
- Test: `crates/lilac-resolve/src/lib.rs` (module tests)

**Interfaces:**
- Consumes: `Project` (Task 5), `ResolvedProject` (Task 2).
- Produces: `collect_project(project: &Project, programs: &[(FileId, Program)], diags) -> ResolvedProject`.

- [ ] **Step 1: Write the failing tests** — (a) two namespaces each declare `foo`; each file's `lookup` sees its own `foo`, and `symbol_of` yields `foo` and `util.foo`; (b) a subfolder file sees a root `foo` with no import (ancestor reach); (c) a root file does **not** see `util.foo`; (d) an importer of a namespace-private item gets no binding (unresolved); (e) two `using`s exporting the same name record `Lookup::Ambiguous`.

- [ ] **Step 2: Run them to verify they fail** — `cargo test -p lilac-resolve` → FAIL.

- [ ] **Step 3: Implement** — build the namespace tree (path + parent), merge each namespace's files into `items` (cross-file duplicate → `E020`), collect hooks unit-globally (`E020` on identical signature), and compute each file's `visible` map: own ns (all) + ancestors (all) + `using`d (public only), with local shadowing and import-vs-import → `Lookup::Ambiguous`. Record `(DefId → (NsId, name))` for `symbol_of`.

- [ ] **Step 4: Run** — `cargo test -p lilac-resolve` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "M6a: build namespace scopes and qualification in collect_project"`.

---

### Task 7: Driver `compile_project` + CLI directory input

**Files:**
- Modify: `crates/lilac-driver/src/lib.rs` (add `compile_project`; keep `run_pipeline`)
- Modify: `crates/lilac-cli/src/main.rs` (accept a directory; resolve a file's parent)
- Create: `crates/lilac-cli/src/std_fs.rs` (production `Fs` over `std::fs`)
- Test: `crates/lilac-cli/tests/m6.rs`

**Interfaces:**
- Consumes: `lilac_module::load`, `collect_project`, `collect_sigs`, `check_bodies`, `lower`.
- Produces: `Session::compile_project(&mut self, root: &Path, fs: &mut dyn Fs) -> Result<LirModule, ErrorGuaranteed>`.

- [ ] **Step 1: Write the failing test** — run `lilac check <dir>` over a two-file folder fixture and assert exit 0; run it over a folder whose `using` names a missing folder and assert `error[E025]`.

- [ ] **Step 2: Run it to verify it fails** — `cargo test -p lilac-cli --test m6` → FAIL.

- [ ] **Step 3: Implement** — `compile_project` loads the project, `add_file`s each source into `SourceMap`, lexes/parses each, builds one merged `Program` (concatenated items) and `collect_project`, then runs `collect_sigs`/`check_bodies`/`lower`/`fold`/runtime/DCE/verify as `run_pipeline` does. CLI: if `<path>` is a directory use it as root, else use its parent; pass a `StdFs`.

- [ ] **Step 4: Run** — `cargo test -p lilac-cli` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "M6a: compile a project directory end to end"`.

---

### Task 8: Object-member `private` and `E026`

**Files:**
- Modify: `crates/lilac-resolve/src/lib.rs` (member visibility in `ResKind::Object`)
- Modify: `crates/lilac-typeck/src/check.rs` (member lookup gate)
- Test: `crates/lilac-cli/tests/m6.rs`; `tests/ui/FAILS_private_member.lilac`

**Interfaces:**
- Consumes: `ItemKind::Private` (Task 1), member `ResItem`s.
- Produces: `E026` at an external member access; allowed inside the object's own member bodies.

- [ ] **Step 1: Write the failing test** — `FAILS_private_member.lilac`: an object with a `private { secret: function … }` member called as `obj.secret()` from outside → exit 1 with `error[E026]`; a companion fixture calling it from inside the object → exit 0.

- [ ] **Step 2: Run it to verify it fails** — `cargo test -p lilac-cli --test m6` → FAIL.

- [ ] **Step 3: Implement** — record member visibility when collecting `object` members (flatten nested `private` blocks); in type-relative member resolution, allow a private member only when the access is lexically inside the owning object, else emit `E026` labelled at the access and the declaration.

- [ ] **Step 4: Run** — `cargo test --workspace` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "M6a: gate object-member privacy with E026"`.

---

### Task 9: Module fixtures, snapshots, and end-to-end tests

**Files:**
- Create: `tests/ui/modules/*.lilac` + sibling `.lir`/`.ll`; `crates/lilac-cli/tests/m6.rs`
- Test: the snapshot + run harness

**Interfaces:**
- Consumes: Tasks 5–8.

- [ ] **Step 1: Add fixtures** — a root + `util/` folder exercising a cross-namespace call, ancestor reach, and a `private` namespace item; generate `.lir`/`.ll` by running the CLI (the existing regeneration snippet).
- [ ] **Step 2: Assert snapshots** — extend the `.lir`/`.ll` stability tests to cover `tests/ui/modules/**`; confirm non-root symbols are prefixed (`util.foo`).
- [ ] **Step 3: End-to-end** — a project whose `main` calls into a subfolder compiles with clang, runs, and exits as expected.
- [ ] **Step 4: Run** — `cargo test --workspace` → PASS.
- [ ] **Step 5: Commit** — `git commit -m "M6a: add module fixtures and end-to-end coverage"`.

---

### Task 10: Docs and spec freeze

**Files:**
- Modify: `SPEC.md` (§6 rewrite to the implemented semantics; version bump), `docs/errors/E025.md`, `E026.md`, `E029.md`
- Modify: `AGENTS.md` (module/loader/qualification notes)

- [ ] **Step 1: Write the error docs** — one file per new code, matching the house format.
- [ ] **Step 2: Update `SPEC.md` §6** to the implemented model (implicit folders, ancestor/descendant reach, root-relative non-transitive `using`, `private` at namespace and object scope, qualification) and bump the version.
- [ ] **Step 3: Note the architecture in `AGENTS.md`** (loader crate, `ResolvedProject`, root-unprefixed qualification).
- [ ] **Step 4: Run** — `cargo test --workspace` and the release bench gate → PASS.
- [ ] **Step 5: Commit** — `git commit -m "M6a: document modules, visibility, and qualification"`.

---

## Self-Review

- **Spec coverage:** §3 module model → Tasks 5–6; §4 `using` → Tasks 5–6; §5 `private` → Tasks 1, 6, 8; §6 collisions → Task 6; §7 loader → Task 5; §7.1–7.3 qualification/resolution → Tasks 2–4, 6; §9 CLI → Task 7; §10 diagnostics → Tasks 5–8, 10; §12 tests → Tasks 5–9. Extern (§8) is plan M6b.
- **Step scan:** each step names one action and a checkable result; bodies are left to the implementer except the resolution rules, which the spec fixes.
- **Type consistency:** `ResolvedProject`/`Lookup`/`symbol_of`/`all_items`/`collect_project`/`Project`/`Fs` are used with the same names across Tasks 2–8.
- **Review Focus:** each line maps to a Task 6 or Task 8 test (ancestor reach, name collision, private-as-unresolved, object `E026`, missing-folder `E025`, ambiguity-on-use).
- **Proportion:** the plan names interfaces and tests, not bodies; the invasive migrations (Tasks 3–4) are specified as a mechanical substitution plus the existing snapshot suite as the regression net.
