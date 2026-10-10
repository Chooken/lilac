# Global Signature-Indexed Hook Model Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every hook (top-level and object-member, operators, `@conversion`, RAII) resolve through one unit-global, signature-keyed index, and delete the `W020` operator-shape lint.

**Architecture:** `ResKind::Hook` gains an optional `HookParent`; object member hooks are grouped per object (`ResKind::Object.hooks`) and merged into the existing unit-global `ResolvedProject.hooks` set, deduped by qualified signature (with `self` qualifying to its parent). `members` shrinks to fields and methods. Typeck collects all hooks from the index, pairs AST member hooks to resolved hooks by name + declaration order, and looks up `@init`/`@on_drop` by signature. `W020` and its flag are removed.

**Tech Stack:** Rust (std only), the `lilac-*` compiler crates, `cargo` test/clippy/fmt.

**Spec:** `docs/superpowers/specs/2026-10-07-lilac-global-hook-index-design.md`

## Global Constraints

- Rust std only; no new dependencies. `unsafe_code = "forbid"`; `clippy::pedantic = deny`; no `unwrap`/`expect`/`panic!` on user input in production code.
- `lilac-lsp` remains the only crate permitted `lsp-server`/`lsp-types`/`serde_json`.
- Snapshot files are LF, no BOM.
- TDD: failing test first, minimal implementation, then commit. One commit per task.
- Gates (run before each task's commit, full set before the final commit): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo clippy --release -p lilac-vm --all-targets --features bench-baseline -- -D warnings`, `cargo test --release -p lilac-vm --features bench-baseline`.

## Review Focus

- **Operand/argument conversion regression.** `a + b` and a direct `@hook(a)` must not insert a `@conversion`; a mismatched operand/argument is an error, while `x: T = value` (typed context) still converts. Tests: Task 4 step 1.
- **Cross-scope duplicate signatures.** Two identical `@conversion(from,to)` pairs — same object, two different objects, or top-level vs member — must all report `E020` (dedup is unit-global, not per object). Tests: Task 2 step 1, Task 3 step 6.
- **`private` member hooks.** A hook inside `private { … }` must still resolve for operators/conversions (visibility does not gate resolution) while `private` still gates direct RAII access. Tests: Task 3 step 6.
- **Generic objects with in-body hooks.** A `@on_drop`/`@conversion` inside `GenObj[T]` must still instantiate/specialize and must not leak `Param` into HIR. Tests: Task 3 step 6.
- **A method and a hook sharing a base name.** `add: function …` plus `@add: function …` in one object must not collide (distinct namespaces in the member map). Tests: Task 2 step 1.
- **Every overload body is checked.** The second/third `@conversion` body must be type-checked; an error inside it must be reported. Tests: Task 3 step 6.

---

### Task 1: Remove the `W020` operator-shape lint

**Files:**
- Modify: `crates/lilac-typeck/src/sigs.rs` (`SigCtx`, `collect_sigs`, `register`, tests)
- Modify: `crates/lilac-driver/src/lib.rs:100-101,115-116,126,289-295,516-549`
- Modify: `crates/lilac-lower/src/lib.rs:2023` (drop the trailing `true` from `collect_sigs`)
- Modify: `crates/lilac-typeck/src/check.rs:5439,5481,5863` (drop the trailing `true`)
- Modify: `crates/lilac-typeck/src/check/generics.rs:1063` (drop the trailing `true`)
- Modify: `crates/lilac-typeck/src/ops.rs:141,165` (drop the trailing `true`)
- Modify: `crates/lilac-cli/src/main.rs:15,106-120`
- Modify: `crates/lilac-cli/tests/m2.rs:149-170`
- Delete: `docs/errors/W020.md`
- Delete: `crates/lilac-cli/tests/ui/op_type_change_warn.lilac`, `crates/lilac-cli/tests/ui/op_bit_mixed_ok.lilac`
- Modify: `SPEC.md:589-607`

**Interfaces:**
- Produces: `lilac_typeck::collect_sigs(project, program, types, diags) -> Sigs` (the trailing `warn_op_change: bool` parameter is gone).
- Produces: `Session` no longer has a `warn_on_operator_type_change` field.

- [ ] **Step 1: Delete the lint and its plumbing**

Remove `check_op_shape` (`sigs.rs:435-490`), `SigCtx.warn_op_change` (field + doc), the `if ctx.warn_op_change { check_op_shape(…) }` block in `register` (`sigs.rs:538-540`), and the `warn_op_change` parameter of `collect_sigs` (and its doc mention). In `sigs.rs` tests, drop `matrix_add_src`, `op_change_warns_matrix`, `op_change_suppressed_by_flag`, `op_bit_mixed_silent`, `cmp_shape_silent`, `cmp_shape_change_warns`; collapse the `sigs_of`/`sigs_of_warn` helpers into a single `sigs_of(src)` calling `collect_sigs(project, program, &mut types, &mut diags)`.

In `driver/lib.rs`: delete the `warn_on_operator_type_change` field (100-101), its `Default` initializer (126), the doc phrase (115-116), the `self.warn_on_operator_type_change` argument at the `collect_sigs` call (289-295), the `MATRIX_ADD` const and the `warnings_do_not_fail_check` (516-526) and `allow_flag_silences_w020` (542-549) tests.

Update **every** `collect_sigs` call site to drop the trailing `true`: `lilac-driver/src/lib.rs`, `lilac-lower/src/lib.rs`, `lilac-typeck/src/check.rs` (three), `lilac-typeck/src/check/generics.rs`, `lilac-typeck/src/ops.rs` (two), and the `sigs.rs` tests.

In `cli/main.rs`: remove the `--allow` (106-113) and `--allow=` (114-120) arms and the `--allow operator-type-change` text from the usage string (15).

Delete `docs/errors/W020.md` and the two `tests/ui` fixtures; delete the `op_warn_is_warning_only` and `op_mixed_silent` tests in `m2.rs` (149-170).

- [ ] **Step 2: Replace the `SPEC.md` W020 section**

Replace §8's `#### Operator type-change lint (W020, on by default)` section (589-607) with a one-paragraph note that operator/`@conversion`/RAII hooks are globally indexed by `(name, signature)` and that there is no operator-shape lint.

- [ ] **Step 3: Verify the workspace builds and tests pass**

Run: `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, no `W020` references remain (`rg "W020|warn_on_operator_type_change|operator-type-change"` is empty).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "typeck: remove the W020 operator-shape lint"
```

---

### Task 2: Resolve — global member hooks (`HookParent`, per-object overloads, unit-global merge)

**Files:**
- Modify: `crates/lilac-resolve/src/lib.rs` (`ResKind`, `HookParent`, `ResolvedProject`, `Collector`, `same_sig_resolved`, `TypeResolver`, tests)
- Modify: `crates/lilac-typeck/src/sigs.rs` (`collect_sigs` bridge; add `..` to the `ResKind::Object` destructure at `:582`)
- Modify: `crates/lilac-typeck/src/check.rs:2156,3356` (add `..` to the `ResKind::Object` destructures — the enum gains a field)

**Interfaces:**
- Produces:
  ```rust
  /// The object that owns a member hook, so `self` resolves to `ref Parent`.
  #[derive(Clone, Debug)]
  pub struct HookParent {
      pub name: String,      // parent object's local name
      pub def: DefId,        // parent object's definition id
      pub ns: NsId,          // parent object's namespace
      pub generics: Vec<String>, // parent object's generic params (empty when concrete)
  }
  ```
- Produces: `ResKind::Hook { generic, generic_params, params, returns, parent: Option<HookParent> }`.
- Produces: `ResKind::Object { members: HashMap<String, ResItem>, hooks: HashMap<String, Vec<ResItem>>, generic_params: Vec<String> }` — `hooks` holds this object's hook overloads in declaration order; `members` holds fields and methods **plus the first hook of each name** (bridge removed in Task 3).
- Produces: `ResolvedProject.hooks: HashMap<String, Vec<ResItem>>` now includes member hooks.

- [ ] **Step 1: Write failing resolve tests**

Add to `crates/lilac-resolve/src/lib.rs` tests:

```rust
#[test]
fn object_hook_overloads_by_signature() {
    let (object, diags) = resolve_object(
        "Int64: object {\n\
         @conversion: function (from: @byte_buffer) Int64 => Int64 { data = 0x0 }\n\
         @conversion: function (from: @bit64) Int64 => Int64 { data = 0x0 }\n\
         @conversion: function (from: Int64) @bit64 => from.data\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
    let ResKind::Object { hooks, .. } = &object.kind else { panic!("object") };
    assert_eq!(hooks["@conversion"].len(), 3);
}

#[test]
fn identical_conversion_pair_is_e020() {
    let (_, diags) = resolve_object(
        "Int64: object {\n\
         @conversion: function (from: @bit64) Int64 => Int64 { data = 0x0 }\n\
         @conversion: function (from: @bit64) Int64 => Int64 { data = 0x0 }\n}\n",
    );
    assert!(diags.diagnostics().iter().any(|d| d.code == "E020"), "{diags:?}");
}

#[test]
fn member_hook_records_parent_and_self_keys_by_parent() {
    // `@on_drop(ref self)` in two different objects must not collide, and each
    // hook's parent must be its own object.
    let (project, diags) = resolve_project(
        "A: object {\n    @on_drop: function (ref self) nothing => 0x0\n}\n\
         B: object {\n    @on_drop: function (ref self) nothing => 0x0\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
    let drops = &project.hooks["@on_drop"];
    assert_eq!(drops.len(), 2);
    let parents: Vec<_> = drops.iter().filter_map(|d| match &d.kind {
        ResKind::Hook { parent: Some(p), .. } => Some(p.name.clone()),
        _ => None,
    }).collect();
    assert!(parents.contains(&"A".to_owned()) && parents.contains(&"B".to_owned()), "{parents:?}");
}
```

Add the helpers `resolve_object` (resolve one source, return the first object item + diags) and `resolve_project` (return `ResolvedProject` + diags) alongside the existing test helpers (mirror `collect`/`ResolvedProject::single`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lilac-resolve object_hook_overloads_by_signature identical_conversion_pair_is_e020 member_hook_records_parent_and_self_keys_by_parent`
Expected: FAIL (compilation: no `hooks` field, no `parent` field) — or the first two fail with `E020`.

- [ ] **Step 3: Implement the resolve data model and collection**

- Add `HookParent` and the `ResKind::Hook.parent` / `ResKind::Object.hooks` fields. Update the two field-complete constructors in resolve `item()` (`ResKind::Hook` at `:532`, `ResKind::Object` at `:554`) and add `..` to the non-exhaustive destructures `sigs.rs:582`, `check.rs:2156`, `check.rs:3356`. Destructures that already use `..` need no change.
- `Collector` threads a `&mut HashMap<String, Vec<ResItem>>` hook accumulator into `item`/`collect_members` so member hooks reach `Module.hooks` (see `collect_program`/`collect_top_level`).
- Give `collect_members` the object's identity: change its signature to
  `fn collect_members(&mut self, items: &[Item], vis: Visibility, table: &mut HashMap<String, ResItem>, hooks: &mut HashMap<String, Vec<ResItem>>, file_hooks: &mut HashMap<String, Vec<ResItem>>, parent: &HookParent)`.
  The `ItemKind::Object` arm of `item()` builds `parent` from the fresh `id`, `item.name`, `self.ns`, and `generic_params`, and passes the object's own `hooks` map plus the file accumulator.
- `collect_members`: for a hook member, push into the object's `hooks` (in order) and into the file accumulator; insert the **first** occurrence into `members` (bridge for Task 3) and do **not** emit the name-only `E020` for later occurrences. Non-hooks keep the current `insert` behavior.
- `collect_project`: member hooks already arrive through `Module.hooks`; `add_hook` merges them with top-level hooks.
- `same_sig_resolved`/`sig_key`: when qualifying a type named `self` for a hook that has a `HookParent`, qualify to `qualify_own(parent.name, resolver.path(parent.ns))` instead of the hook's own namespace. Add a small `qualify_hook(item, name)` on `TypeResolver` used by the two `same_sig*` closures.

- [ ] **Step 4: Add the typeck bridge so the workspace stays green**

In `crates/lilac-typeck/src/sigs.rs` `collect_sigs`, when iterating `project.hooks`, skip overloads with `ResKind::Hook { parent: Some(_), .. }` (they are still collected from `members` by the AST walk until Task 3). One guarded `continue`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p lilac-resolve` then `cargo test --workspace`
Expected: PASS (resolve tests pass; the bridge keeps typeck/CLI green).

- [ ] **Step 6: Commit**

```bash
git add crates/lilac-resolve/src/lib.rs crates/lilac-typeck/src/sigs.rs
git commit -m "resolve: collect object-member hooks into the unit-global hook index"
```

---

### Task 3: Typeck — consume the hook index everywhere

**Files:**
- Modify: `crates/lilac-resolve/src/lib.rs` (`flat_member_pairs`, drop bridge support)
- Modify: `crates/lilac-typeck/src/sigs.rs` (`collect_sigs`, `collect_item`, `flat_members`, `validate_raii_hook`)
- Modify: `crates/lilac-typeck/src/check.rs` (`@init`, `@on_drop`, `check_member`/`check_item`, `find_item`, `find_parent_name`)
- Modify: `crates/lilac-typeck/src/check/generics.rs` (`res_item`, `member_name`, `function_name`, `instantiate_object` member loop)

**Interfaces:**
- Produces (in `lilac-resolve`):
  ```rust
  /// Pairs each flattened AST member (descending into `private`) with its
  /// resolved item: non-hooks by name, hooks by name + declaration order.
  pub fn flat_member_pairs<'a>(ast: &'a [Item], object: &'a ResKind) -> Vec<(&'a Item, ResItem)>;
  ```
- Consumes: `ResKind::Object.hooks`, `ResKind::Hook.parent`, `ResolvedProject.hooks` from Task 2.
- Produces: `ResolvedProject` no longer keeps hooks in `members` (bridge dropped).

- [ ] **Step 1: Write failing typeck tests**

Add to `crates/lilac-typeck/src/ops.rs` tests (or `sigs.rs`):

```rust
#[test]
fn member_conversions_all_enter_the_table() {
    let (_, _, _, sigs, diags) = check_src_sigs(
        "Int64: object {\n    data: @bit64\n\
         @conversion: function (from: @byte_buffer) Int64 => Int64 { data = @as_int(from) }\n\
         @conversion: function (from: @bit64) Int64 => Int64 { data = from }\n\
         @conversion: function (from: Int64) @bit64 => from.data\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
    assert_eq!(sigs.conversions.len(), 3);
}

#[test]
fn every_hook_overload_body_is_checked() {
    // The second conversion references an unknown name: it must be reported.
    let diags = check_src(
        "Int64: object {\n    data: @bit64\n\
         @conversion: function (from: @bit64) Int64 => Int64 { data = from }\n\
         @conversion: function (from: @byte_buffer) Int64 => Int64 { data = @nope(from) }\n}\n",
    );
    assert!(diags.diagnostics().iter().any(|d| d.code == "E021"), "{diags:?}");
}

#[test]
fn member_operator_resolves() {
    let diags = check_src(
        "Int64: object {\n    data: @bit64\n\
         @add: function (lhs: Int64, rhs: Int64) Int64 => Int64 { data = @i_add(lhs.data, rhs.data) }\n}\n\
         main: function () nothing {\n    a: Int64 = Int64 { data = 0x1 }\n    b: Int64 = a + a\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}
```

Add a `check_src_sigs` helper returning `Sigs` (mirror `check_src` but return the collected `sigs`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lilac-typeck member_conversions_all_enter_the_table every_hook_overload_body_is_checked member_operator_resolves`
Expected: FAIL (`conversions.len()` is 1; the bad body is not reported).

- [ ] **Step 3: Implement `flat_member_pairs` and migrate the AST walks**

Add `flat_member_pairs` to `lilac-resolve` (recursive; hooks pair by a per-name occurrence counter into `object.hooks`, non-hooks by `members.get`). Replace `flat_members` in `sigs.rs` with it; use it in `check.rs`'s `check_item` object arm (replacing the raw `members` loop + `check_member` name lookup for hooks) and in `generics.rs`'s `instantiate_object` member loop. `find_item`'s member closure (`check.rs:474`) becomes order-aware for hooks.

- [ ] **Step 4: Collect all hooks from the index**

In `sigs.rs` `collect_sigs`, remove the Task 2 bridge `continue` so every `project.hooks` entry is collected, passing the parent tuple derived from `ResKind::Hook.parent` as `Some((parent.name.as_str(), parent.def, parent.generics.as_slice()))`. `collect_item` must skip hook members (they no longer live in `members`). Move `validate_raii_hook` onto this global pass (it has `parent_name`/`parent_def` from the hook), removing its call from the object arm.

- [ ] **Step 5: Redirect RAII lookups to the index and update DefId scans**

- `check.rs` `@init` (`:2863`): find the `project.hooks["@init"]` entry whose `parent.def == item.id` (that gives the `ResItem` for `private_member_denied`/span), then require its `func_sig` to return the object's `intern_object` id; keep the existing `E022` messages and the `private` gate.
- `check.rs` `@on_drop` (`:925`): find the `project.hooks["@on_drop"]` entry whose `parent.def == item.id`; require its `func_sig` to take exactly one `ref` object parameter and no returns; keep drop registration.
- Add `object.hooks` to the DefId scans: `find_parent_name` (`check.rs:430`), `res_item` (`generics.rs:771`), `member_name` (`generics.rs:703`), `function_name` (`generics.rs:788`).
- Drop the bridge: `collect_members` no longer inserts hooks into `members`.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --workspace`
Expected: PASS, including `FAILS_conversion` still `E020`, all RAII and generics tests, and the new typeck tests. Add the Review-Focus tests here: cross-scope identical pair `E020`; `private` member hook resolves; generic object `@on_drop` instantiates; method + hook same base name coexist.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "typeck: collect and pair hooks through the global hook index"
```

---

### Task 4: Direct-match operators and hooks (no operand/argument conversion)

**Files:**
- Modify: `crates/lilac-typeck/src/ops.rs` (`resolve_binop`, tests)
- Modify: `crates/lilac-typeck/src/check.rs` (operator HIR arm at `:2495-2536`, `check_hook_call`/`check_arg` at `:2913`, `:3672`)
- Modify: `SPEC.md` §8 (resolution order)

**Interfaces:**
- Produces: `ops::resolve_binop(sigs, hook, lhs, rhs) -> Result<(DefId, FuncSig), BinopMiss>` returns only an exact `(lhs, rhs)` overload; the `BinopMiss` no longer implies a conversion attempt.
- Produces: hook-call argument checking that does not insert conversions (an exact-arg path or a flag on `check_arg`).

- [ ] **Step 1: Write failing tests**

Add to `crates/lilac-typeck/src/ops.rs` tests:

```rust
#[test]
fn operator_does_not_convert_operands() {
    // `@add(@bit32,@bit32)` + a `@bit8 -> @bit32` conversion: `b + a`
    // (b:@bit8, a:@bit32) must NOT convert `b`; it is E043.
    let diags = check_src(
        "@add: function (lhs: @bit32, rhs: @bit32) result: @bit32 { result = @i_add(lhs, rhs) }\n\
         @conversion: function (from: @bit8) to: @bit32 { to = @extend_zero(from) }\n\
         main: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit8 = 0x2\n    z: @bit32 = b + a\n}\n",
    );
    assert!(has_code(&diags, "E043"), "{diags:?}");
}

#[test]
fn hook_call_does_not_convert_arguments() {
    let diags = check_src(
        "@f: function (x: @bit32) @bit32 => @i_add(x, 0x1)\n\
         @conversion: function (from: @bit8) to: @bit32 { to = @extend_zero(from) }\n\
         main: function () nothing {\n    b: @bit8 = 0x2\n    z: @bit32 = @f(b)\n}\n",
    );
    assert!(has_code(&diags, "E010") || has_code(&diags, "E043"), "{diags:?}");
}

#[test]
fn typed_context_still_converts() {
    let diags = check_src(
        "@conversion: function (from: @bit32) to: @bit8 { to = @truncate(from) }\n\
         main: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit8 = a\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lilac-typeck operator_does_not_convert_operands hook_call_does_not_convert_arguments typed_context_still_converts`
Expected: the first two FAIL (currently the conversion succeeds); the third PASSES already.

- [ ] **Step 3: Implement exact-match resolution**

- `ops.rs` `resolve_binop`: keep only the exact `(lhs, rhs)` search; delete the `conversions.iter().any(...)` retry. Update the `BinopMiss` doc.
- `check.rs` operator arm (`:2495-2536`): drop `let right_hir = self.convert_if_needed(right, left.id, right_hir);` (use `right_hir` directly) and remove the "; no conversion from … to …" clause from the `E043` message.
- `check_hook_call` (`:2913`) / `check_arg` (`:3672`): check hook arguments without conversion — an argument whose non-`ref` type is not the parameter type is `E010` (reuse `mismatch`); do not call `convert_if_needed`. Keep `check_expr(fctx, arg, Some(param.id))` so numeric literals take the parameter type. Ordinary function calls keep the converting `check_arg`/`check_args` path.

- [ ] **Step 4: Update the spec text and affected tests**

In `SPEC.md` §8 "Type Conversions", rewrite resolution steps 2–3: no conversion is attempted for operator operands or hook-call arguments; typed contexts and ordinary call arguments still convert. Update `binop_anchors_left` (now two `E043`) and any other test that relied on operand conversion.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "typeck: operators and hook calls match exactly (no operand conversion)"
```

---

### Task 5: LSP symbols, docs, and the stdlib integration test

**Files:**
- Modify: `crates/lilac-lsp/src/index.rs` (`add_def`)
- Modify: `docs/errors/E020.md:43-46`
- Modify: `SPEC.md` (§7.2 hook scoping; §8 hook-index paragraph)
- Create: `crates/lilac-cli/tests/ui/hook_index_int64.lilac`
- Modify: `crates/lilac-cli/tests/` (one integration test)

**Interfaces:**
- Consumes: `ResKind::Object.hooks`, `ResolvedProject.hooks` from Tasks 2–3.

- [ ] **Step 1: Index member hooks in the LSP**

In `index.rs` `add_def`, after the `members.values()` loop, also walk `hooks.values().flatten()` for `ResKind::Object` so member hooks still produce hover/definition symbols (they no longer appear in `members`).

- [ ] **Step 2: Update the docs**

- `E020.md`: replace the sentence "object-member hooks may not [overload]" with the signature rule: hooks overload with distinct signatures anywhere in the unit; identical name + qualified signature is `E020`.
- `SPEC.md` §7.2: state that hooks are unit-global and scope-independent; member-hook visibility still gates direct RAII access.
- `SPEC.md` §8: describe hooks as globally indexed by `(name, signature)`.

- [ ] **Step 3: Add the stdlib integration test**

Create `crates/lilac-cli/tests/ui/hook_index_int64.lilac` with the `Standard` int64 shape — an object with operators and three distinct `@conversion` pairs (using the correct intrinsic names `@i_add`/`@i_sub`/…). Add a CLI test asserting `lilac check` exits 0 with empty stderr.

- [ ] **Step 4: Run the full gates**

Run: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo clippy --release -p lilac-vm --all-targets --features bench-baseline -- -D warnings`, `cargo test --release -p lilac-vm --features bench-baseline`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "lsp: index member hooks; docs: global hook model; stdlib integration test"
```

---

## Self-Review Notes

- **Spec coverage:** §4 data model → Task 2; §5 collection/dedup → Task 2; §6 typeck/lookup → Task 3; §6 direct-match operators/hooks → Task 4; §7 W020 removal → Task 1; §8 docs → Tasks 1, 4, 5; §9 testing → each task; LSP → Task 5.
- **Deferred detail:** the exact per-object `hooks` field vs a global-scan pairing is fixed here as `ResKind::Object.hooks` (spec §4 says only that `members` holds fields and methods; the extra grouping is an implementation aid and does not change resolution).
