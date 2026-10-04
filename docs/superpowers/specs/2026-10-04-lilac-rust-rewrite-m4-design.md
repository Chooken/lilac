# Lilac Rust Rewrite — M4 Design (duck-typed generics + rustc-grade generic errors)

> Scope: M4 only (`RUST_REWRITE_PLAN.md` §4, §7 M4 bullet). Normative language/IR
> docs: `SPEC.md` §7 (v0.3-draft), `IR_SPEC.md` v0.2. Prior records: M0 design
> (`docs/superpowers/specs/2026-10-01-lilac-rust-rewrite-m0-design.md`), M1 plan
> (`docs/superpowers/plans/2026-10-01-m1-types.md`), M2 design
> (`docs/superpowers/specs/2026-10-02-lilac-rust-rewrite-m2-design.md`), M3 design
> (`docs/superpowers/specs/2026-10-04-lilac-rust-rewrite-m3-design.md`).
> Work stays on branch `rust-rewrite-m0`.

## 1. Outcome / constraints / success

- Outcome: generic functions, generic objects, and generic object methods with
  **explicit** type arguments compile end-to-end — each use monomorphizes to
  concrete HIR and then to concrete LIR — and duck-typed requirements are proven
  at instantiation, failing with rustc-style **use-site** errors
  (`E040`/`E041`/`E042`/`E044`/`E051`, reusing `E043`/`E052`). Hooks declared
  inside a generic body register once per concrete instantiation. The milestone's
  acceptance gate is a **human error-tone review sign-off**.
- Constraints: (a) same gates as M0–M3 — `cargo fmt --check`,
  `clippy -D warnings`, `cargo test` green after every task; (b) frontend crates
  never import `inkwell`/`llvm-sys`; (c) no `unwrap`/`expect` on user input,
  `bug!()` for ICEs only; (d) std only, no new crates; (e) TDD per task, one
  commit per task, an error doc for every new code; (f) no new surviving IR
  instruction shapes (verified IR stays concrete).
- Success: §6 exit criteria all green.

### 1.1 Decisions locked during brainstorming

1. **Full monomorphization incl. concrete LIR** (not check-only).
2. **Core generics**: generic functions, objects + object methods, nested
   instantiations, in-body hook registration, and the E04x error machinery.
   **Deferred out of M4**: generic enums (the `generic_union` port — the `union`
   keyword was removed in M2), and the f-string streaming `append[T]` handler
   punted from M3.
3. **Approach A — HIR specialization inside `lilac-typeck`.** `lilac-lower`,
   `lilac-const`, `lilac-ir`, and the verifier are untouched.
4. **Arity is checked before interning** an applied type: a new `E044`
   (wrong number of generic arguments) with expected/found and the decl's param
   list labeled.
5. **Generic `self` is an applied instance**: `ref Instance { base, args:
   [Param(0)..Param(n-1)] }`, so substitution recursively fixes field types.
6. **Requirement errors reuse the existing concrete checker**, captured and
   re-attributed to the call site with an instantiation chain (no separate
   symbolic "requirement collection" pass; see §4.1).
7. **Arity-tagged name mangling** `__` for monomorphized symbols.

### 1.2 What is already in place (verified before design)

- `lilac-ast` already carries `generic_params: Vec<String>` (function/object
  declarations) and `generic_args: Vec<Type>` (type applications and explicit
  call arguments). The parser produces both; `T.get()` and `GenObj[@bit32]` parse
  today.
- `lilac-resolve` records only `generic: bool` on `Func`/`Hook` (bodies check in
  M4); objects have no param list yet.
- `lilac-typeck` defers every generic path with `E030`: `sigs` skips generic
  declarations, `check_call` rejects generic callees / non-empty `generic_args`,
  and `check_bodies` skips generic item bodies. `collect_sigs` already builds a
  `conversions` registry and generic hooks are rejected with `E052`.
- `lilac-lower` already emits RAII `drop` + `@on_drop` calls from a drops
  registry (`HirProgram.drops`, keyed by type display name), and `lilac-hir`
  carries `layouts: HashMap<String, Vec<(String, Option<TypeRef>)>>` keyed by
  display name. Both extend naturally to concrete instance keys.
- `lilac-cli` already accepts `--verbose` and `--error-format=human|json`.
- Legacy tests exist but are unported: `generic_function.lilac` /
  `generic_function.lilac-ir` (stale), `generic_object.lilac`,
  `generic_object_functions.lilac`,
  `generic_duck_typing.lilac`, `FAILS_generic_duck_typing.lilac`,
  `generic_union.lilac` (stale — `union` removed). `generic_function.lilac-ir`
  is stale (it calls `@get`; the source does not) and must be regenerated, not
  copied.

## 2. Architecture (plan §3, M4 slice)

```text
crates/
  lilac-ast/      # unchanged (generic_params/generic_args already exist)
  lilac-resolve/  # M4: per-declaration param name lists (funcs, hooks, objects)
  lilac-ty/       # M4: TypeKind::{Param, Instance}; intern_instance (dedup);
                  #     substitute; pretty-printing of instances
  lilac-typeck/   # M4: template sigs for generic decls; E044 arity; E030 retired;
                  #     instantiate-on-first-reference; specialization = substitute
                  #     + run the concrete checker; memo/poison/depth/cycle;
                  #     in-body hook registration; E04x rendering
  lilac-hir/      # unchanged shape; M4 fills it with concrete items only
  lilac-lower/    # unchanged (sees concrete HIR only)
  lilac-const/    # unchanged
  lilac-ir/       # unchanged (verified IR is concrete-only)
  lilac-driver/   # unchanged pipeline
  lilac-cli/      # unchanged flags
```

- **Single new pass location: `check_bodies`.** It is already the pass that
  builds `HirProgram`; in M4 it also drives monomorphization, so the returned
  program contains one `HirFunc` per reached `(DefId, args)` instance and
  concrete `layouts`/`drops` entries. Unreached/never-instantiated generics
  vanish. No new pipeline stage; `RUST_REWRITE_PLAN.md` §3's "monomorphize
  generics" is realized inside this existing pass.
- **Nothing downstream changes.** `lilac-lower`, `lilac-const`, `lilac-ir`, and
  the verifier only ever see concrete types, so their M2/M3 code is untouched.

## 3. Components

### 3.1 Type model (`lilac-ty`)

Two new kinds:

- `TypeKind::Param { index: u32 }` — a declaration's generic parameter
  placeholder (`T` is index 0). It appears only in **templates** (stored
  signatures, object layout templates, and generic hook signatures) and while
  resolving a generic declaration's own types. It must never appear in a
  concrete check environment or in `HirProgram` output; an assertion enforces
  this (see §5).
- `TypeKind::Instance { base: DefId, args: Vec<TypeRef> }` — an applied user
  type (`GenObject[@bit32]`). Interned canonically by
  `intern_instance(base, args)`, keyed on `(DefId, Vec<TypeRef>)`, so two
  spellings of the same instantiation share one `TypeId`; **dedup is a property
  of the interner**, not a separate pass. Args may be builtins, `ref`s, or nested
  `Instance`s.

`Types` also gains:

- a `param_names` table so `display(Param(i))` prints the source name (`T`);
- `display(Instance)` → `GenObject[@bit32]` (readable, for diagnostics);
- `mangle(Instance)` → the deterministic IR symbol form (§3.4);
- `substitute(ty, args)` — recursively rewrites `Param(i)` → `args[i]` and
  `Instance{base, args}` → `intern_instance(base, substitute(args))`.

`FuncSig`/`TypeKind::Func` are unchanged; template sigs simply contain `Param`
ids until substituted.

### 3.2 Resolution and signatures (`lilac-resolve`, `lilac-typeck::sigs`)

- `ResKind::Func`, `ResKind::Hook`, and `ResKind::Object` gain a
  `params: Vec<String>` list (preserving declaration order); `generic: bool` is
  kept as a derived convenience.
- The type-resolution routine threads a **param scope**: a name bound to a
  generic parameter resolves to `Param(index)`; a user type with
  `generic_args` resolves to `Instance { base, args }` where each arg is
  resolved by the same routine (so `GenObj[ref @bit8]` and
  `GenObj[GenObj[@bit32]]` work). This replaces today's `E030` for generic type
  arguments.
- **Arity is enforced before interning.** When resolving `Name[arg…]`, compare
  `args.len()` against the declaration's `params.len()`; on mismatch emit
  **`E044`** (expected/found; the declaration's param list is a labeled note) and
  return poison — `intern_instance` is **not** called. This fires for type-level
  `GenObj[...]`, call-level `gen_function[...]()`, zero-args-on-a-generic
  (`gen_function()`), and args-on-a-non-generic.
- `collect_sigs` builds **template signatures** for generic declarations in
  generic mode (slots may be `Param`) instead of skipping them. Its output is
  otherwise unchanged: concrete declarations get concrete sigs, `conversions`
  and operators are registered, and top-level generic hooks still fail with
  `E052`.
- **Generic `self`.** In an object method, the `self` parameter is rewritten to
  `ref Instance { base: <object DefId>, args: [Param(0) … Param(n-1)] }`
  (`n` = the object's param count; empty for a non-generic object, which stays
  `ref Object(def)` as today). Since `substitute` recurses through `Instance`
  args, specializing the method with `[@bit32]` yields `ref GenObject[@bit32]`,
  and the base's field `value: Param(0)` becomes `@bit32`, so `self.value`
  resolves.

### 3.3 Instantiation and specialization (`lilac-typeck`)

- **Templates, not eager bodies.** Generic declarations are not checked as
  top-level work. `check_bodies` walks concrete declarations (roots: `@main` and
  every concrete function) and, on encountering a concrete instantiation,
  specializes it.
- **Instantiate-on-first-reference.** A concrete type application
  (`GenObj[@bit32]`) or a generic call (`gen_function[@bit32](…)`) computes the
  key `(DefId, args)`. Because a value/type must be written before it is used,
  this guarantees an instance (and its in-body hooks) is registered before any
  code that depends on it is checked.
- **Memo table**: success stores the concrete sig/type/items; failure stores
  poison and returns `ErrorGuaranteed` with **no new diagnostic** (cascade
  suppression). A second reference to a failed instantiation is silent.
- **Specialization = substitute + run the existing concrete checker.** The
  generic body's AST/types are rewritten with `args`, producing a concrete
  environment in which no `Param` remains; the normal checker then runs on that
  body. On failure the captured diagnostics provide the def-site spans and
  messages; they are consumed by the renderer (§3.5) and dropped so nothing
  leaks from generic innards.
- **Objects and methods.** `GenObj[@bit32]` produces a concrete object type with
  substituted field types; each method specializes per `(method DefId, object
  args)`. Method bodies are checked in the concrete environment of their
  instance. A method with its own generic parameters is supported by the same
  function-instantiation path (it is just an instantiated function whose `self`
  is already concrete); M4 requires no dedicated port for that case.
- **In-body hooks.** A hook declared inside a generic body (e.g. `GenBox[T]`'s
  `@on_drop: function (from: ref GenBox[T])`) is, when the parent is
  instantiated, registered as a **concrete** entry in the existing
  conversion/operator/drop registries, keyed by the substituted signature, once
  per `(DefId, args)`. The drop registry (`HirProgram.drops`, keyed by the
  instance's display name) then lets the M2/M3 RAII lowering emit the `drop` +
  `@on_drop` call unchanged.
- **Depth and cycles.** An `active` stack of keys guards recursion. Re-entering
  the same key with the same args is a true cycle → **`E051`** (the loop is
  printed). Differing/lengthening chains are bounded by `MAX_DEPTH`; the full
  chain is shown under `--verbose`, elided after 5 frames otherwise.

### 3.4 Concrete HIR, mangling, pipeline wiring

- `check_bodies` returns a `HirProgram` with **concrete items only**:
  - `funcs`: one `HirFunc` per reached function instance, plus all concrete
    (non-generic) functions. No generic template functions.
  - `layouts`: a concrete entry per reached object instance, keyed by the
    instance's mangled/display name.
  - `drops`: a concrete entry per instantiated dropping type.
  - `consts`: unchanged (module constants that reference instantiations use the
    concrete items).
- **Mangling.** Diagnostics keep readable `GenObject[@bit32]`. IR symbols use a
  deterministic, collision-free, **arity-tagged** form:
  `<Name>__<arity>__<arg1>__…`, each arg mangled recursively; builtins drop the
  `@`; `ref X` → `ref_X`; an `Instance` arg is its own mangled form. Examples:
  `gen_function__1__bit32`, `GenObject__1__bit32`, method
  `GenObject__1__bit32.get`, nested `GenObj__1__GenObj__1__bit32`. The arity tag
  is required: a naive `__`-join is ambiguous for
  `GenObj[GenObj[@bit32]]` vs `GenObj[GenObj][@bit32…]`. `layouts`/`drops` keys
  use the same mangled name.
- **Pipeline unchanged**: `resolve → collect_sigs → check_bodies (now also
  monomorphizes) → lower → const-fold → verify`. No new driver stage.
- **No `Param` in output.** Because verified IR is concrete-only, `lilac-ir`,
  the verifier, and every backend remain generic-agnostic.

### 3.5 Error model and renderer (`lilac-typeck`, `lilac-error`, docs)

- **New codes**, each with `docs/errors/E0xx.md` (good/bad examples):
  `E040` unknown member, `E041` signature mismatch, `E042` requirements
  unsatisfied (the one-per-failed-instantiation top-level code), `E044` wrong
  number of generic arguments, `E051` cyclic/recursive generic. `E043`
  (conversion missing) and `E052` (generic hook) are reused.
- **Capture, then re-attribute.** On a failed instantiation, synthesize **one**
  top-level `E042`:
  - primary label at the **call site** ("instantiated here");
  - notes: the def site ("required by `gen_function[T]` at gen.lilac:1:1") and
    the instantiation chain (elided after 5 frames, full under `--verbose`);
  - labels: each captured generic-body failure at its `def_span` (e.g. "missing
    method `other` on `@bit32`") — the `FAILS_generic_duck_typing` shape from
    `RUST_REWRITE_PLAN.md` §4.3;
  - helps: edit-distance `did-you-mean` over the concrete type's members, arity
    ("`get` takes 0 args, you passed 1"), `ref`-vs-value, missing `using`, and a
    discovered `@conversion`.
- **Dedup and cap.** Dedup by `(generic DefId, args, call-site file+line)`;
  cap at 10 primary errors. `--error-format=json` keeps its current fields (full
  JSON polish stays M6).
- **Non-generic improvement folded in (`§4.3`).** `E010` is fixed to print
  **both** sides every time — the expected annotation span and the found value
  span, "expected `@bit32`, found `@bit16`" — as part of the same error-tone goal.

## 4. Spec decisions (normative patches)

- **`SPEC.md` §7** is clarified: type arguments are explicit (no inference in
  v1); wrong arity is `E044`; a generic object method's `self` is the applied
  instance `ref GenObj[T…]`; hooks declared inside a generic body register once
  per concrete instantiation while top-level generic hooks stay `E052`. The
  version header bumps per the §11 freeze rule, with paired UI tests.
- **`IR_SPEC.md`** gains the monomorphized name-mangling rule and the statement
  that verified IR is **concrete-only** (no generic declarations or `Param`).
- **`docs/errors/`**: add `E040`/`E041`/`E042`/`E044`/`E051.md`; retire generic
  triggers from `E030.md`; update `E010.md` to the both-sides form.

### 4.1 Deviations from the original plan (binding)

- **Requirement collection is derived, not a separate phase.**
  `RUST_REWRITE_PLAN.md` §4 sketches `Requirement`/`Obligation`/`Instantiation`
  structs and a collect→prove flow. M4 instead **specializes and runs the
  existing concrete checker**, then re-attributes the captured diagnostics
  (Approach A, sub-choice (b)). This yields the same use-site rendering and
  all-failures-at-once listing with less machinery; the "requirement inventory"
  is the captured diagnostic list.
- **`lilac-resolve` does not intern `Instance`.** Resolution has no `Types`
  table; the arity check and `intern_instance` live in the `lilac-typeck` type
  application routine (used by both `sigs` and body checking), which is the
  "whichever pass interns the Instance" from the review note.
- **`Session{host,target}` + serializable LIR follow §7 (M5)**, not the loose
  "M4" mention in §10.4.

## 5. Data flow / error handling / testing

- Flow: `read file → Session → lex → parse → resolve (params) → collect_sigs
  (template sigs, E044/E052) → check_bodies (instantiate-on-first-reference;
  specialize = substitute + concrete check; memo/poison; E04x; in-body hooks) →
  concrete HirProgram → lower (unchanged) → const-fold (unchanged) → verify
  (unchanged) → check|print`.
- Errors: user failures return `Result<_, ErrorGuaranteed>`; poison propagates
  without new diagnostics. New codes `E044`, `E040`, `E041`, `E042`, `E051`;
  `E030` retired for generics; `E010` improved. Each new code frozen by its
  `docs/errors/` doc before the owning task commits.
- **No-`Param`-leak assertion.** A debug assertion / unit test asserts that
  `check_bodies`'s output contains no `TypeKind::Param` (templates never reach
  `HirProgram`).
- Tests (all as generated `*.lilac` + `*.lir` snapshots under `tests/ui/`):
  - `generic_function` (emits monomorphized `@gen_function__1__bit32`, no
    template), `generic_object`, `generic_object_functions` (generic `self`),
    `generic_duck_typing`.
  - `FAILS_generic_duck_typing` short-form snapshot **and** a `--verbose`
    full obligation-tree snapshot; `FAILS_generic_arity` (`E044`);
    `FAILS_generic_cyclic` (`E051`).
  - No-cascade regression: one failed instantiation referenced from two call
    sites emits exactly one `E042`.
  - In-body hook: `GenBox[T]` with `@on_drop`, consumed at one instantiation →
    LIR shows the `drop` + `@on_drop` call for `GenBox__1__bit32`.
  - Type-unit tests: `intern_instance` dedup; `substitute` recursion through
    `Instance`/`ref`; `display`/`mangle` of instances.

## 6. Ports + exit criteria

- Port `generic_function` (regenerate `.lilac-ir` → `tests/ui/*.lir`),
  `generic_object`, `generic_object_functions`, `generic_duck_typing`, and
  `FAILS_generic_duck_typing` to `tests/ui/`; add `FAILS_generic_arity`,
  `FAILS_generic_cyclic`, the no-cascade case, and the `GenBox[T]` in-body-hook
  case. **Not ported**: `generic_union` (the `union` keyword was removed in M2;
  generic enums are deferred) and the f-string streaming handler (deferred).
  Add `crates/lilac-cli/tests/m4.rs` mirroring the `m2.rs`/`m3.rs` harness
  (`PORTS` + `check_ports_clean` + `lir_snapshots_stable` + `fails_with` and a
  `--verbose` snapshot test).
- Exit: (1) all success generics check and emit concrete monomorphized IR (no
  `E030`, no template functions, no `Param`); (2) `FAILS_generic_duck_typing`
  short + `--verbose` snapshots match §4.3's shape; (3) `E044`/`E051` and the
  no-cascade tests green; (4) in-body hook emits `drop` + `@on_drop` for the
  instance; (5) `E010` both-sides snapshot green; (6) `cargo fmt --check`,
  `clippy -D warnings`, `cargo test --workspace` green; frontend `inkwell` grep
  empty; (7) all ported snapshots byte-stable via `emit-ir --check`;
  (8) **error-tone review sign-off** by the human partner.

## 7. Self-review

- Placeholders: none — every new kind, error, mangling rule, and test names its
  owner crate.
- Consistency: §3.1's `Param`/`Instance` match §3.3's substitution and §5's
  no-leak assertion; §3.2's `E044`-before-intern matches §1.1/§4.1; §3.2's
  generic `self` matches the review requirement and §3.3's field resolution;
  §3.4's arity-tagged mangling answers the collision risk; §3.5's re-attribution
  matches §4.1's deviation.
- Scope: strictly M4; generic enums, the f-string streaming handler, type-arg
  inference, surface `where` bounds, and M5 LLVM/serialization are explicitly
  out.
- Ambiguity: monomorphization placement, arity timing, `self` shape, mangling,
  and error ownership are decided in §3–§4, not left to the implementer.
