# Lilac Hook Index — Global Signature-Indexed Hooks

> Scope: replace the split treatment of top-level vs object-member hooks with a
> single, unit-global hook index keyed by `(hook name, qualified signature)`.
> Object-member hooks (operators, `@conversion`, RAII) become indistinguishable
> from top-level hooks; a hook is unique by its input/output types, not by where
> it is declared. The `W020` operator-shape lint is removed entirely. Branch
> `ai`. Compiler crates only; `lilac-lsp` and the extension change only where
> they read object members for symbols.

## 1. Outcome / constraints / success

**Outcome**

- A hook declared at top level or inside any object is collected into one
  unit-global index and resolves identically.
- `@conversion` overloads by `(from, to)`: distinct pairs coexist; an identical
  pair anywhere in the unit is `E020`.
- `@init` / `@on_drop` are found by querying the index by signature
  ("is there an `@init` returning `Object`?"), not by object membership.
- Operators and direct hook calls resolve by exact type match; no operand or
  argument `@conversion` is applied.
- `W020` no longer exists — no lint, no flag, no diagnostic.

**Constraints**

- Compiler crates stay Rust std-only; no new dependencies. `unsafe_code` and
  `clippy::pedantic` hold.
- Ordinary object member lookup (fields, methods, `private` gating, `self` in
  methods) is behavior-preserving.
- All existing gates stay green, including the release VM bench gates.

**Success**

- `Int64: object { … three distinct @conversion pairs … }` compiles with no
  `E020`; two identical `(from, to)` pairs (same or different scope) report
  `E020`.
- A `@conversion`, operator, or RAII hook behaves the same at top level and
  inside an object.
- `lilac check` never emits `W020`, and `--allow operator-type-change` is gone.
- `a + b` with no exact `@add(lhs, rhs)` is `E043` even when an operand
  conversion exists; `b: @bit8 = a` (a typed context) still converts.
- The `Standard` int64 pattern (operators + three conversions) checks cleanly
  once the user's intrinsic names are corrected (`@i_add`, not `@iadd`).

**Non-goals**

- Type inference, new intrinsic names, or fixing the stdlib's `@iadd` typos.
- Transitive conversion chains, operator overloading across `ref`, or new hooks.
- Changing hook *bodies*, `ref` semantics, or the `Sigs` lookup algorithms.
- Multi-root projects or editor-side hook UI.
- Converting ordinary function-call arguments (that stays anchored on the
  parameter type).

### 1.1 Decisions locked during brainstorming

1. **Remove `W020` completely** — lint, `--allow operator-type-change`, docs,
   tests.
2. **One global hook index** for every hook (top-level, operator, conversion,
   RAII), keyed by name + qualified signature.
3. **Scope does not affect hook resolution.** A hook inside an object is
   equivalent to the same hook at top level.
4. **Uniqueness is by signature** — "unique for input and output pairs" for
   conversions; identical signatures anywhere are `E020`.
5. **RAII is included**: `self` resolves to `ref Parent`, so the parent type is
   part of the signature and `@init`/`@on_drop` are looked up globally.
6. **Operators and direct hook calls are direct-match only.** No operand or
   argument conversions: `a + b` requires an exact `@add(lhs, rhs)`, and a
   direct `@hook(a, b)` requires an exact overload. Ordinary function calls and
   typed contexts (let / assignment / return / f-string holes) still convert.

## 2. Problem

`crates/lilac-resolve/src/lib.rs`:

- Top-level hooks are merged into a unit-global overload set
  (`ResolvedProject.hooks: HashMap<String, Vec<ResItem>>`) and deduped by
  qualified signature (`add_hook` / `same_sig_resolved` / `sig_key`).
- Object-member hooks are stored in the object's `members:
  HashMap<String, ResItem>` via `collect_members`, which passes **no** overload
  set (`insert_overload(table, None, …)`). Same-name members therefore always
  report `E020` and only the first survives, regardless of signature.

Consequences observed in `Standard/int64.lilac`:

- Three distinct `@conversion` pairs inside `Int64` collide as `E020`
  (`docs/errors/E020.md` documents this as intentional).
- The second and third conversions are dropped, so they never reach the
  conversion table.
- `@init` / `@on_drop` are looked up per-object via `members.get("@init")`
  (`check.rs:2863`, `check.rs:925`), a different mechanism from operators.

Separately, `check_op_shape` (`typeck/sigs.rs`) emits `W020` on every operator
whose types are not all builtin `@bit` variants — including self-consistent
`(T, T) -> T` shapes — because it never implements the `(T, T) -> T` silent
case in `SPEC.md` §8.

## 3. Architecture / components

```
crates/lilac-resolve/src/lib.rs   # ResKind::Hook.parent; member hooks -> global index;
                                  #   self-aware signature dedup; members = fields+methods
crates/lilac-typeck/src/sigs.rs   # collect all hooks from the global index; drop W020;
                                  #   RAII validation on the global pass; parent-aware pair
crates/lilac-typeck/src/check.rs  # @init/@on_drop looked up by signature; hook bodies
                                  #   paired via the global index
crates/lilac-typeck/src/check/generics.rs  # hook pairing for generic instantiation
crates/lilac-typeck/src/ops.rs    # unchanged (Sigs.hooks/Sigs.conversions)
crates/lilac-driver/src/lib.rs    # drop warn_on_operator_type_change
crates/lilac-cli/src/main.rs      # drop --allow operator-type-change
crates/lilac-lsp/src/index.rs     # index member hooks from the global index
docs/errors/W020.md               # deleted
docs/errors/E020.md               # object-member hooks now overload by signature
SPEC.md                           # §7.2 hook scoping; §8 replace W020 with the hook index
```

**Untouched:** `lilac-ir`, `lilac-lower`, `lilac-codegen-llvm`, `lilac-vm`,
`lilac-hir`. No change to `Sigs.hooks`/`Sigs.conversions` shapes or to
`ops::resolve_binop`/`ops::convert`.

## 4. Global hook index (data model)

`ResKind::Hook` gains its owning object:

```rust
/// The object that owns a member hook, so `self` resolves to `ref Parent`.
#[derive(Clone, Debug)]
pub struct HookParent {
    /// The parent object's local name.
    pub name: String,
    /// The parent object's definition id.
    pub def: DefId,
    /// The parent object's namespace (for qualified-signature keys).
    pub ns: NsId,
    /// The parent object's generic parameter names (empty when concrete).
    pub generics: Vec<String>,
}

pub enum ResKind {
    // …
    Hook {
        generic: bool,
        generic_params: Vec<String>,
        params: Vec<Param>,
        returns: Vec<Param>,
        /// Owning object for a member hook (`None` at top level).
        parent: Option<HookParent>,
    },
    // …
}
```

`ResolvedProject.hooks: HashMap<String, Vec<ResItem>>` becomes the single
unit-global index for **every** hook. Order within a name is declaration order.
`ResKind::Object.members` now holds only fields and methods; hooks are not
members.

## 5. Collection & dedup (`lilac-resolve`)

- **`collect_members`** (per object):
  - Non-hook members: unchanged — inserted into the object's name map via
    `insert_overload(table, None, …)`, so a same-name duplicate is `E020`.
  - Hook members: construct the `ResItem` with `parent = Some(HookParent { … })`
    and push it into the file's hook set; **do not** insert it into the member
    name map and **do not** apply the name-only `E020`.
- **Threading:** `item`/`collect_members` currently run before the `Module`
  exists, so the collector threads the file's hook accumulator down through
  them; member hooks thus reach `Module.hooks` directly.
- **`Module.hooks`** now carries member hooks too (they flow to the global
  merge alongside top-level hooks).
- **`collect_project`**: the existing `pending_hooks` merge via `add_hook`
  handles both kinds. Dedup remains qualified-signature equality.
- **`self` in signature keys**: `TypeResolver::qualify` (used by
  `same_sig_resolved` → `sig_key`) maps a `self` type name to the hook's parent
  qualified name (`qualify_own(parent.name, path(parent.ns))`) so
  `@on_drop(ref self)` keys as `@on_drop(ref Parent)`. Hooks without a parent
  keep today's fallback.
- **`ResolvedProject::single`** is unchanged in shape; member hooks now arrive
  through `Module.hooks`.
- **`index_defs`** already walks `hooks.values().flatten()`; because member
  hooks now live there (and no longer in `members`), `def_ns` stays correct
  with no extra pass.

Resulting rule: **`E020` fires iff two hooks share name + qualified signature
anywhere in the unit**, or two non-hook members share a name in one object, or
two non-hook top-level items share a name.

## 6. Typeck collection & lookup (`lilac-typeck`)

**Collection (`sigs.rs`).** `collect_sigs` iterates `project.hooks` and calls
`collect_hook_sig` for every entry, deriving the parent tuple
`(parent.name, parent.def, parent.generics)` from `ResKind::Hook.parent`.
`collect_top_items` still walks the AST for functions and object methods; member
hooks are absent from `members`, so they are not double-collected. `register`
and the `Sigs` tables are unchanged. `W020` machinery is deleted (section 7).

**Body checking & generics (`check.rs`, `check/generics.rs`).** The AST walk
still visits hook members to check their bodies and instantiate generics.
Pairing to the resolved hook uses **(parent object, hook name, declaration
order)**: resolve collects an object's hook members in flattened source order,
and the AST walk visits them in the same order, so the k-th AST `@conversion`
of an object maps to the k-th `@conversion` in the global index for that parent.
A small helper resolves this pairing; it replaces `resolved_members.get(name)`
at the hook sites. Non-hook members keep pairing by name.

**Operators / conversions (`ops.rs`).** `resolve_binop` reads
`Sigs.hooks` / `Sigs.conversions`, which now contain member hooks, and becomes
**exact-match only**: it returns the overload with `params == (lhs, rhs)` and
drops the "convert the right side toward the left, then retry" step. A miss is
`E043` (message no longer mentions a conversion). The operator HIR no longer
wraps the right operand in a conversion (`check.rs` drops the
`convert_if_needed(right, left.id, …)` call; the matched overload already
accepts the operand type). `convert` itself is unchanged — it still serves typed
contexts.

**Direct hook calls (`check_hook_call`).** Overload selection stays exact
(probed argument types), and the chosen overload's arguments are checked
**without** inserting a `@conversion`: a non-`ref` argument whose type is not
the parameter type is a mismatch (`E010`) rather than a conversion. Numeric
literals still take the parameter type (literal typing, not conversion). The
shared `check_arg`/`check_args` used by ordinary functions keeps its converting
behavior.

**RAII lookup (`check.rs`).**
- `@init`: replace `members.get("@init")` with a search of `Sigs.hooks["@init"]`
  for the overload whose single return type is the object type; keep the
  existing `E022` "no `@init` constructor" diagnostics and the `private` gate.
- `@on_drop`: replace `members.get("@on_drop")` with a search of
  `Sigs.hooks["@on_drop"]` for the overload taking `ref Object`; keep the drop
  registration.

## 7. Removing `W020`

- `typeck/sigs.rs`: delete `check_op_shape`, the `SigCtx.warn_op_change` field
  and its use, and the `collect_sigs` `warn_op_change` parameter.
- `driver/lib.rs`: delete `Session.warn_on_operator_type_change` (field, doc,
  `Session::new`/`Default` init) and update the `collect_sigs` call; drop the
  warning-only test.
- `cli/main.rs`: delete `--allow` / `--allow=` handling (it only ever accepted
  `operator-type-change`) and remove it from the usage string.
- `docs/errors/W020.md`: delete. Update `SPEC.md` §8 (replace the W020 section
  with a short description of the global hook index) and any normative mention.
- Tests: remove `op_change_warns_matrix`, `op_change_suppressed_by_flag`,
  `op_bit_mixed_silent`, `cmp_shape_silent`, `cmp_shape_change_warns`, and the
  CLI `op_warn_is_warning_only` / `op_mixed_silent` cases and their fixtures.

## 8. Errors & docs

- `docs/errors/E020.md`: replace "object-member hooks may not [overload]" with
  the signature rule — hooks overload with distinct signatures anywhere in the
  unit (top level or object member); identical name + signature is `E020`.
- `SPEC.md` §7.2: state that hooks are unit-global and scope-independent, and
  that member-hook visibility still gates direct RAII access.
- `SPEC.md` §8: remove `W020`; describe hooks as globally indexed by
  `(name, signature)`.
- `RUST_REWRITE_PLAN.md` and the historical `docs/superpowers/…` M2/M5a docs
  describe past work and are left as historical record (no edit).

## 9. Testing

- **`lilac-resolve` unit:** object `@conversion` with distinct pairs — no
  `E020`, all overloads present in the global index; identical pair in one
  object and across top-level/object — `E020`; member hooks carry the right
  parent; `self` keys by the parent type; non-hook member duplicates still
  `E020`; ordinary member lookup unchanged.
- **`lilac-typeck` unit:** a member `@conversion` participates in conversion
  resolution; a member `@add` participates in operator resolution; `@init` and
  `@on_drop` resolve globally; hook bodies (all overloads) are checked; generic
  objects with in-body hooks still instantiate.
- **`lilac-cli` integration:** the `Standard` int64 shape (operators + three
  conversions) checks cleanly with corrected intrinsic names; `FAILS_conversion`
  (identical top-level pair) stays `E020`; no `warning[W020]` is ever emitted;
  `--allow operator-type-change` is rejected as unknown.
- **`lilac-lsp`:** symbols/definitions for member hooks still resolve (the
  index reads the global hook set).
- All existing gates stay green.

## 10. Gates

- `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, and the release VM gates
  (`cargo clippy --release -p lilac-vm --all-targets --features bench-baseline`
  / `cargo test --release -p lilac-vm --features bench-baseline`).

## 11. Maintenance / source of truth

- `ResolvedProject.hooks` is the single hook index; `add_hook` is the only
  dedup point. `Sigs.hooks`/`Sigs.conversions` are its resolved mirror.
- `HookParent` is the only place a hook records its owner; `self` and signature
  qualification both derive from it.
- `E020` has one rule for hooks: same name + qualified signature.
- W020 is gone; no code path may reintroduce an operator-shape warning.

## 12. Follow-ups (separate specs)

- Transitive conversion chains with a cycle guard.
- Operator/conversion overloads across `ref`.
- Surfacing the hook index to editor tooling (find-overloads, hover).
