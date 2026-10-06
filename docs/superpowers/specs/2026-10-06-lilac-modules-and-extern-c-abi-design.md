# Lilac M6 — Folder Modules, `private` Visibility, and a Minimal `extern` C ABI

> Scope: implement `SPEC.md` §6 folder namespaces (multi-file folders, `using`,
> `private`) and give `extern` a real C ABI so a Lilac program can call libc.
> Branch `rust-rewrite-m0`. Constraints: stable Rust, std-only, no `serde`,
> workspace `unsafe_code = "forbid"`, `inkwell`/`llvm-sys` confined to
> `lilac-codegen-llvm`, no standard library. This is a language/compiler
> milestone; build tooling is deferred until after self-hosting.

## 1. Outcome / constraints / success

**Outcome**

- `lilac <cmd> <path>` compiles a whole folder namespace: every `*.lilac`
  directly in the root folder is one namespace; subfolders are imported with
  `using A.B`; `private { … }` hides declarations at namespace level and inside
  `object` bodies.
- `extern` declarations are lowered and called with the platform C ABI for
  scalars and pointers, so a Lilac program can call libc (`write`, `malloc`,
  `free`, `memcpy`) and observe real output.
- The entry function `main` lives in the root namespace; `--export` still seeds
  reachability roots.

**Constraints**

- (a) Stable Rust, std-only, no `serde`; (b) `unsafe_code = "forbid"` stays in
  force workspace-wide; (c) `inkwell`/`llvm-sys` only in `lilac-codegen-llvm`;
  (d) no standard library — this milestone makes the *language* complete, it does
  not add stdlib; (e) the VM stays `extern`-free (a call is still a clean runtime
  error) — the C ABI is LLVM-only; (f) `.lir`/`.ll`/`.lbc` snapshots stay
  byte-stable under the existing harness.

**Success**

- Multi-file folder fixtures compile to merged `.lir`/`.ll` snapshots.
- `using` resolves root-relative, per-file, non-transitively; a missing folder is
  `E025`.
- `private` gates namespace imports and object-member access; a violation is
  `E026`.
- A libc `write`/`malloc` program links with the toolchain `clang`, runs, and
  produces the expected stdout / exit code.

**Non-goals**

- Stdlib, `lilac.zon` manifest, build scripts, dependency fetching.
- Transitive imports, `using … as` aliases, folder-qualified paths in
  expressions, module re-exports.
- Aggregate (struct-by-value) C ABI, `export`/callbacks, variadic C calls.
- Auto-loading of subfolders no file `using`s.

### 1.1 Decisions locked during brainstorming

1. **Input/root.** A directory argument is the root namespace; a file argument
   means its parent folder is the root.
2. **Namespace = folder.** Files directly in a folder merge into one namespace;
   a top-level duplicate across files is `E020`.
3. **Compilation unit.** Root folder + every transitively-`using`d folder.
   Subfolders are never auto-loaded.
4. **`using A.B`.** Root-relative dotted path → `<root>/A/B/`; imports that
   namespace's public names into *this file's* scope only; non-transitive; for
   non-ancestor namespaces only (ancestors are visible automatically, decision 9).
5. **`private { … }`.** Allowed at file top-level and inside `object`; gates
   **every** member (fields, functions/methods, nested types, hooks, consts).
   - *namespace-private*: visible to the defining namespace and its descendant
     namespaces (Rust's rule), excluded from `using`; any other access → `E026`.
   - *object-private*: visible only within that object's own member bodies;
     hidden from `obj.m` / `Object.m`; violation → `E026`.
6. **Collisions.** A local name shadows an imported one; two imports exporting
   the same name is an ambiguity error only if referenced (`E029`).
7. **`extern` C ABI.** Scalars by value, `ref T` → pointer; 0 outs = `void`,
   1 out = the C return value, >1 out = `E028`; aggregate params/returns are
   `E028` (deferred).
8. **Entry.** `main` in the root namespace; `--export` seeds roots.
9. **Visibility reach (Rust-aligned).** A namespace automatically sees **all**
   items of its ancestor namespaces — public via ancestor reach, private via
   descendant reach — so the root namespace's declarations are usable everywhere
   with no import. `using` is only for non-ancestor namespaces, and imports only
   their public items.

### 1.2 Evidence (current behavior, before design)

- **Legacy modules were never implemented.** `using`/`private` are parsed and
  printed, then dropped by every semantic pass
  (`src/sema/analysis/id_collection_pass.zig:47-65`); `Scope.usings` is never
  appended to; `untyped.Module.submodules` is never populated; only one file is
  ever compiled (`src/root.zig:36-38`). `SPEC.md` §6 is the only contract.
- **The Rust resolver ignores `using`:** `ItemKind::Using { .. } => return None`
  (`crates/lilac-resolve/src/lib.rs:186`).
- **The Rust lexer has `KwPrivate`** (`crates/lilac-lex/src/lib.rs:42-43`) but
  the AST/parser have no `private` item; `ItemKind` has no private variant
  (`crates/lilac-ast/src/lib.rs:32-69`).
- **The pipeline is single-file:** `run_pipeline` calls `SourceMap::add_file`
  once and `collect(&program)` once (`crates/lilac-driver/src/lib.rs:156-199`);
  the CLI reads exactly one positional file
  (`crates/lilac-cli/src/main.rs:151-160`).
- **`extern` uses the internal pointer-place ABI, not C:**
  `tests/ui/extern_decl.ll:8` is `declare void @puts(ptr)` and `:15` passes a
  pointer to a `{i64,ptr}` byte_buffer. `lower_func` declares every function —
  externs included — as `void(ptr…)` and returns early for externs
  (`crates/lilac-codegen-llvm/src/lower.rs:280-321`); `lower_direct_call` passes
  every place by pointer (`:505-566`). So libc `puts` reads garbage.
- **User-callable intrinsics** `@byte_buffer_ptr`/`@byte_buffer_len` exist
  (`crates/lilac-lower/src/lib.rs:1502-1505`), so a libc
  `write(fd, ptr, len)` call is expressible in source.

## 2. Architecture

```text
crates/
  lilac-ast/          # + ItemKind::Private { items } ; Using already exists
  lilac-parse/        # parse `private { … }` at item and object-member position
  lilac-module/       # NEW: folder loader (root discovery + `using` fixpoint),
                      #      I/O injected via an Fs trait; pure logic + tests
  lilac-resolve/      # per-namespace Module merge, per-file visible scopes,
                      #      visibility, E020/E025/E026/E029
  lilac-typeck/       # resolve names against the referencing file's scope
  lilac-lower/        # consume the resolved project (per-file scopes)
  lilac-driver/       # compile_project(project) multi-file pipeline
  lilac-codegen-llvm/ # extern C ABI: signature + call lowering, E028
  lilac-cli/          # accept a directory (or a file → its parent)
tests/ui/modules/     # folder-layout fixtures + merged snapshots
```

Two invariants are preserved: `lilac-driver` still owns no filesystem I/O
(`lilac-cli` provides the `Fs`), and `inkwell` stays confined to
`lilac-codegen-llvm`.

## 3. Module model

- **Root folder.** All `*.lilac` files directly inside it (non-recursive), sorted
  by file name for determinism, form the root namespace. Each file is a
  namespace unit; together they are one namespace.
- **Subfolder `A/B`.** Namespace named by its dotted path from the root (`A.B`).
  It is loaded only when some file in an already-loaded namespace `using`s it,
  directly or transitively. Its own files are all `*.lilac` directly inside it.
- **Ancestors and descendants.** The folder tree gives every namespace an
  ancestor chain up to the root and a descendant set. Only namespaces in the
  compilation unit participate; the root is always in it.
- **Merge.** Every item from a namespace's files lands in one `Module`. A
  top-level duplicate name across files is `E020`, with the two spans in their
  respective files.
- **Compilation unit.** Root namespace plus every transitively-used folder. All
  their functions are lowered; DCE prunes anything not reachable from `main` /
  `--export`.
- **No folder-qualified paths.** Namespaces are compile-time scopes only; there
  is no runtime or expression-level `A.B` access. Imported names are unqualified.

## 4. `using`

- **Syntax.** `using A.B` is a top-level item, already parsed as
  `ItemKind::Using { path }` (`crates/lilac-parse/src/lib.rs:331-346`). No
  semicolon, no alias in v1.
- **Path base.** Root-relative: `using Standard` → `<root>/Standard/`;
  `using A.B` → `<root>/A/B/`.
- **Effect.** The public names exported by the target namespace become visible,
  unqualified, in the file that contains the directive. `using` is only needed
  for non-ancestor namespaces: ancestor namespaces are visible automatically
  (§5), so importing one is redundant but allowed.
- **Scope.** Per file. Sibling files in the same folder do not inherit it.
- **Non-transitive.** If `B` `using`s `C`, a file in `A` that `using`s `B` does
  not see `C`'s names. (`C` is still part of the compilation unit.)
- **No parent pull-through.** `using` never imports a *parent's* import set, and
  never imports private items; ancestor items are reached by visibility, not by
  `using`.
- **Errors.** A target that does not exist or is not a directory is `E025` at the
  `using` span. A folder is loaded once even if several files import it; cycles
  are allowed.

## 5. `private`

- **Syntax.** `private { <items> }` at (a) file top-level and (b) `object`
  member position. It wraps a list of items; nested `private` blocks flatten.
- **AST.** `ItemKind::Private { items: Vec<Item> }`.
- **Namespace-private.** Items in a top-level `private` block belong to the
  folder's namespace but are excluded from the namespace's `using` export. They
  are visible to the defining namespace and its descendant namespaces (Rust's
  rule: private = the module and its descendants) — never to ancestors, siblings,
  or importers.
- **Object-private.** Members in a `private` block inside an `object` are
  visible only within that object's own member bodies — i.e. functions whose
  enclosing scope is the object. They are hidden from external `obj.m` and
  `Object.m`.
- **Visibility reach.** For a namespace, "ancestor" and "descendant" are the
  folder-tree relationships among the namespaces in the compilation unit (§3);
  the root is always in it. A namespace sees all items of its ancestors (public
  by ancestor reach, private by descendant reach), its own items, and the public
  items of the namespaces it `using`s.
- **Lookup gating.** A lookup to a declaration outside its visibility reach —
  private from an ancestor, sibling, or importer; or any cross-object access to a
  private member — is `E026`, labelled at both the access site and the
  declaration.
- **Name occupancy.** A private declaration still occupies its name: a private
  and a public declaration of the same name in one namespace is `E020`.

## 6. Name collisions

- **Within a namespace:** `E020`, first binding wins, spans across files.
- **Local vs imported:** the local (same file/folder) name wins; no error.
- **Import vs import:** if two `using`d namespaces export the same name,
  referencing it is `E029` (ambiguous), labelled at the reference and both
  `using` sites. Unreferenced ambiguity is not an error.

## 7. Pipeline and data model

**Loader (`lilac-module`, new crate).** Injected I/O keeps it testable and keeps
the driver I/O-free:

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

`load` reads the root folder's files, lexes/parses each to extract `using`
paths, resolves them root-relative, reads newly-reached folders, and repeats to
a fixpoint (each folder once). A missing folder emits `E025`.

**Resolution (`lilac-resolve`).** Each `ResItem` gains visibility and namespace
identity:

```rust
pub enum Visibility { Public, Private }
pub struct ResItem { pub id: DefId, pub kind: ResKind, pub span: Span,
                     pub vis: Visibility, pub ns: u32 }

pub struct FileScope { pub file: FileId, pub visible: Module }
pub struct ResolvedProject { pub modules: Vec<Module>, pub files: Vec<FileScope> }
```

`collect_project` merges each namespace's files into a `Module` (`E020` across
files), then computes each file's `visible` module: its own namespace's items
(public **and** private), then each `using`d namespace's **public** items, with
local names shadowing imported ones and import-vs-import collisions recorded as
an ambiguous marker (`E029` on use). Object member scopes carry per-member
visibility for the member lookup gate.

**Typeck / lower.** Name lookup consults the *referencing function's file scope*
(`FileScope.visible`) and the enclosing object's member visibility, not a single
global module. Generic instantiation and hook overload sets stay global and
deduplicated, shared across files (a monomorphization cache), so cross-file
instances are built once. The exact threading of `FileId`/scope into
`check_bodies`/`lower` is the plan's work; the invariant is that a lookup can
only see names its file is allowed to see.

**Single-file compatibility.** `compile_source`/`compile_lir` remain for the
existing UI fixtures; a single file is a one-file project.

## 8. `extern` C ABI

Extern declarations and calls switch from the pointer-place ABI to the platform
C ABI. Internal (non-extern) functions are unchanged.

- **Declaration.** For an `extern` function, `lower_func` emits the C signature
  instead of `void(ptr…)` and still returns before emitting a body:
  - each `in` place: `ref T` → `ptr`; `#bit8/#bit16/#bit32/#bit64/#bit_native`
    → the matching C integer (`i8/i16/i32/i64`); anything else → `E028`.
  - returns: `0` outs → `void`; `1` out → the out's C type (scalar, or `ref T` →
    `ptr`); `>1` out → `E028`.
  - the symbol is the unmangled declared name.
- **Call.** `lower_direct_call` branches on the callee being `extern`:
  scalar inputs are loaded and passed by value; `ref T` inputs are passed as
  pointers; the single return value is stored into the out place.
- **VM.** `extern` remains a clean runtime error (`docs/VM_SPEC.md` §5); the C
  ABI is LLVM-only.
- **Fixture fix.** `tests/ui/extern_decl.{lilac,ll}` is rewritten to a C-correct
  declaration (its current `puts(@byte_buffer)` passes a struct pointer). New
  fixtures: `extern_write.lilac` (libc `write` end-to-end) and failing
  `FAILS_extern_multi_out.lilac`, `FAILS_extern_aggregate_param.lilac` → `E028`.

## 9. CLI

- `<path>` may be a directory (its files form the root namespace) or a file (its
  parent folder is the root).
- `check`, `emit-ir`, `emit-llvm` accept a project directory; `emit-vm`, `run`,
  `disasm` accept one too, but a project that calls `extern` fails cleanly on the
  VM as today.
- Entry is `main` in the root namespace; `--export=<name>` seeds roots, resolved
  in the root namespace. Exit codes are unchanged (0 / 1 / 2).

## 10. Diagnostics

| Code | Trigger | Labels |
|---|---|---|
| `E020` (extended) | duplicate top-level name across files in one namespace | redefined + first defined, different files |
| `E025` (new) | `using` target missing or not a directory | the `using` path |
| `E026` (new) | access to a private declaration across a scope boundary | access site + declaration |
| `E028` (new) | invalid `extern` signature (unsupported param/return, >1 out) | the offending slot |
| `E029` (new) | reference to a name exported by two `using`d namespaces | reference + both `using` sites |

New error docs: `docs/errors/E025.md`, `E026.md`, `E028.md`, `E029.md`. `SPEC.md`
§6 is updated to match the implemented semantics and its version is bumped;
`SPEC.md` §9 notes the extern C ABI.

## 11. Gate and process

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, and the release VM bench gate
  (`cargo test --release -p lilac-vm --features bench-baseline`) all green.
- Spec freeze: `SPEC.md` §6/§9 normative edits + version bump; `IR_SPEC.md` only
  if the extern ABI changes LIR text (it does not — extern already lowers to a
  bodyless `LirFunc`).
- Snapshots: merged `.lir`/`.ll` for every module fixture; `.lbc` unchanged.
- No stdlib; no new `unwrap`/`expect`/`panic!` on user input.

## 12. Testing

- **Loader (`lilac-module`):** root discovery, `using` fixpoint, cycles load
  once, missing folder → `E025`, deterministic ordering.
- **Parser:** `private { … }` at top level and in an `object`; malformed block →
  `E001`.
- **Resolver:** per-file `using` visibility, non-transitivity, ancestor-public
  reach (root items visible in a subfolder with no import), descendant-private
  reach (a subfolder sees a parent's private), namespace-private exclusion from
  importers/siblings/ancestors (`E026`), object-private member access (`E026`),
  import ambiguity (`E029`), cross-file `E020`.
- **Snapshots:** `tests/ui/modules/` fixtures produce merged `.lir`/`.ll`.
- **Extern:** `extern_write.lilac` links with clang, runs, and asserts stdout;
  `FAILS_extern_multi_out.lilac` / `FAILS_extern_aggregate_param.lilac` → `E028`.
- **Regression:** all existing single-file fixtures still pass unchanged.

## 13. Exit criteria

1. `lilac emit-llvm <dir>` over a multi-file folder emits merged IR; `main` in
   any root-namespace file is found.
2. `using A.B` imports public names, per-file, non-transitively; missing folder
   is `E025`.
3. `private` hides namespace items from ancestors, siblings, and importers (but
   descendants see them), and hides object members from external access;
   violations are `E026`. Ancestor items (including the root) are visible with no
   import.
4. A libc `write`/`malloc` program links and runs with correct output.
5. `cargo test --workspace` and the release bench gate pass; every existing
   snapshot is unchanged except the intentionally-rewritten `extern_decl`.

## 14. Risks / known divergences

- **Legacy never implemented modules.** We are defining behavior, not porting
  it; `SPEC.md` §6 is updated accordingly, and the legacy `using … as`/folder
  `.Module` types are explicitly not carried over.
- **Per-file scope threading is invasive.** Typeck/lower currently assume one
  global module; the refactor risks regressions — mitigated by keeping the
  single-file path and all existing snapshots green.
- **C ABI is scalar/pointer only.** Aggregate-by-value, callbacks, and variadics
  are deferred; a program that declares them gets `E028`.
- **`extern_decl` snapshot churn.** The fixture is intentionally rewritten, so
  its `.lir`/`.ll` change; the harness's "one `.ll` per success fixture" rule
  still holds.
- **Determinism.** Merged snapshots depend on stable file/namespace ordering,
  which the loader pins by sorting.
- **Root-private is program-visible.** Under descendant reach (Rust's rule)
  every namespace descends from the root, so a root `private` item is visible
  throughout the package — effectively public in a single-package program.
  Expected; `private` only bites in non-root namespaces.
- **Rust alignment.** Reach follows Rust's ancestor/descendant model; we
  deliberately keep opt-out `private`, implicit folders, no qualified paths, no
  `pub use`, and class-scoped object members as documented divergences.

## 15. Self-review

- **Placeholders:** none — every type, diagnostic, and fixture names its owner.
  The exact threading of file scopes through `check_bodies`/`lower` is delegated
  to the implementation plan as a concrete design task, not a TBD.
- **Consistency:** §1.1's nine decisions match §3–§9; the reach rules in §5
  implement decision 9 and are reflected in §4 and the `E026` row of §10; the
  loader (§7) is the input to resolution (§4–§6); the extern ABI (§8) matches the
  fixture fixes (§12); the diagnostics table (§10) matches the error sites named
  in §4–§6.
- **Scope:** strictly modules + visibility + a minimal extern C ABI — no stdlib,
  no build tooling, no IR format change.
- **Ambiguity:** root discovery, `using` base, visibility of private items,
  collision policy, extern signature rules, and the VM's extern behavior are all
  decided here.
