# Inline Control Flow & Stdlib Compile Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make inline `=>` bodies accept control flow (short-circuit `and`/`or`, `if`-expressions), stop `Cmp` from seeding operand width from its `@bit8` result, fix `lib/Standard`'s intrinsic typos, add the stdlib as a CLI test, and remove the VM integration/bench tests.

**Architecture:** An inline `=>` body is checked once into HIR; its control-flow statements are captured as a `prelude` alongside the value. At each call site the expander alpha-renames the body's temps to fresh ones and then substitutes the parameters, then emits the prelude into the caller's current block before the value is used — no HIR-shape, lowering, or backend changes. Separately, `arith_out` forwards the expected type into operand unification only for `Arith`/`Negate`, never `Cmp`.

**Tech Stack:** Rust (std-only), workspace crates `lilac-typeck`, `lilac-hir`, `lilac-cli`, `lilac-vm`; CLI integration tests use `std::process::Command`.

**Spec:** `docs/superpowers/specs/2026-10-07-lilac-inline-control-flow-and-stdlib-design.md`

## Global Constraints

- Compiler crates are Rust std-only; no new dependencies. `unsafe_code` and `clippy::pedantic` hold.
- `=>` bodies keep textual-substitution semantics: an argument used twice evaluates twice.
- No grammar, AST, HIR-shape, `lilac-lower`, `lilac-ir`, or backend changes.
- `and`/`or` stay short-circuit builtins (SPEC §8).
- `lib/Standard` intrinsic names are `@i_add`/`@i_sub`/`@i_mul`/`@i_div`/`@i_mod`/`@i_negate` (`@i_negate` negates bit containers).
- Snapshots and committed files are LF, no BOM.
- Gates per task commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (after Task 5, without the VM bench gates).

## Review Focus

Inputs/conditions the spec implies but no task test directly names — each gets a test in the owning task:

1. **Nested inline control flow in one argument list** (`f( a or b, c or d )`) — each expansion must use distinct join temps, or the two args alias. (Task 2.)
2. **Inline control flow inside a module constant** — must still report `E030` (`has_temp` on the value), never silently drop the prelude. (Task 2.)
3. **Overload probing of an inline-call argument** — `select_hook_overload` must not emit the probe's prelude a second time. (Task 2.)
4. **`Cmp` with unanchored all-literal operands** — `E024`, not a silent `@bit8`. (Task 1.)
5. **Inline control flow as an `or`/`and` right operand** — the prelude must land inside the branch block (`check_nested`), not the outer block. (Task 2.)

---

### Task 1: `Cmp` operand width

**Files:**
- Modify: `crates/lilac-typeck/src/check.rs` (`arith_out`, ~line 4030)
- Test: `crates/lilac-typeck/src/check.rs` (`mod tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `arith_out` no longer passes `expected` to `unify_width` for `Intrinsic::Cmp`; behavior for `Arith`/`Negate` unchanged.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/lilac-typeck/src/check.rs`:

```rust
#[test]
fn cmp_operands_ignore_expected_result_width() {
    // Return position expects `@bit8`; operands are `@bit32`.
    let diags = check_src("f: function (a: @bit32, b: @bit32) @bit8 => @i_less_than(a, b)\n");
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn cmp_unanchored_literals_is_e024() {
    let diags = check_src("f: function () @bit8 => @i_less_than(0x1, 0x2)\n");
    assert!(has_code(&diags, "E024"), "{diags:?}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lilac-typeck cmp_operands_ignore_expected_result_width cmp_unanchored_literals_is_e024`
Expected: `cmp_operands_ignore_expected_result_width` FAILS with `E010` (`expected @bit8, found @bit32`); `cmp_unanchored_literals_is_e024` FAILS (no `E024`).

- [ ] **Step 3: Implement**

In `arith_out` (`check.rs:4030`), change the `unify_width` call so `expected` is forwarded only for non-`Cmp`:

```rust
let operand_expected = if intrinsic == Intrinsic::Cmp { None } else { expected };
let (width, hir_args) = self.unify_width(fctx, args, operand_expected, span)?;
```

Leave the `if intrinsic == Intrinsic::Cmp { … @bit8 … }` result block and everything else unchanged.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p lilac-typeck cmp_`
Expected: PASS.

- [ ] **Step 5: Run the full typeck suite (catch regressions)**

Run: `cargo test -p lilac-typeck`
Expected: PASS. If any test pinned the old seeding, fix the test (not the fix) and note it.

- [ ] **Step 6: Commit**

```bash
git add crates/lilac-typeck/src/check.rs
git commit -m "typeck: Cmp operand width is inferred from operands, not the result type"
```

---

### Task 2: Inline `=>` bodies carry a statement prelude (concrete path)

**Files:**
- Modify: `crates/lilac-typeck/src/check.rs` (`CheckedBody` ~32, `Checker` ~251, `subst_ident` ~546, `fresh_temp` ~821, `check_func_body` ~1116, `select_hook_overload` ~2959, `check_inline_call` ~3053)
- Test: `crates/lilac-typeck/src/check.rs` (`mod tests`)

**Interfaces:**
- Consumes: `TempId` (`lilac_hir`), `HirStmt`, `HirBlock`, `HirExprKind`, `fresh_temp(&mut self) -> TempId` (`check.rs:821`).
- Produces (used by Task 3):
  - `CheckedBody::Inline { param_names: Vec<String>, prelude: Vec<HirStmt>, value: HirExpr }`
  - `Checker.inline_bodies: HashMap<DefId, (Vec<String>, Vec<HirStmt>, HirExpr)>`
  - `fn subst_stmts(stmts: &[HirStmt], map: &HashMap<String, HirExpr>) -> Vec<HirStmt>`
  - `fn rename_temps_in_stmts(stmts: &[HirStmt], map: &HashMap<TempId, TempId>) -> Vec<HirStmt>`
  - `fn rename_temps_in_expr(expr: &HirExpr, map: &HashMap<TempId, TempId>) -> HirExpr`
  - `CallOutcome` is unchanged: `Inline(HirExpr, TypeRef)`.

- [ ] **Step 1: Write the failing tests**

Add `use lilac_hir::{HirStmt, TempId};` to the test module, then add to `mod tests`:

```rust
#[test]
fn inline_or_body_checks_and_inlines() {
    let src = "leq: function (a: @bit8, b: @bit8) @bit8 => (a == b) or @i_less_than(a, b)\n\
               main: function () nothing {\n    r: @bit8 = leq(0x1, 0x2)\n}\n";
    let (diags, _) = check_hir(src);
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn inline_if_value_body_checks_and_inlines() {
    // Value-`if` branches are expressions: `if c a else b` (no braces).
    let src = "pick: function (a: @bit8, b: @bit8) @bit8 => if a b else a\n\
               main: function () nothing {\n    r: @bit8 = pick(0x1, 0x2)\n}\n";
    let (diags, _) = check_hir(src);
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn inline_two_or_bodies_use_distinct_temps() {
    let src = "or2: function (a: @bit8, b: @bit8) @bit8 => a or b\n\
               use2: function (p: @bit8, q: @bit8, r: @bit8, s: @bit8) @bit8 => or2(p, q) or or2(r, s)\n\
               main: function () nothing {\n    x: @bit8 = use2(0x1, 0x2, 0x3, 0x4)\n}\n";
    let (diags, hir) = check_hir(src);
    assert!(!diags.has_errors(), "{diags:?}");
    let main = hir.funcs.iter().find(|f| f.name == "main").expect("main");
    let outs: Vec<TempId> = main
        .body
        .stmts
        .iter()
        .filter_map(|s| match s {
            HirStmt::If { out: Some(t), .. } => Some(*t),
            _ => None,
        })
        .collect();
    assert!(outs.len() >= 2, "expected two inlined `or` joins, got {outs:?}");
    let unique: std::collections::HashSet<TempId> = outs.iter().copied().collect();
    assert_eq!(unique.len(), outs.len(), "inlined joins must use distinct temps: {outs:?}");
}

#[test]
fn inline_call_as_or_operand_places_prelude_in_branch() {
    // `a or inner(b, c)`: `inner`'s prelude must land in the outer `or`'s
    // branch block, not the outer block.
    let src = "inner: function (a: @bit8, b: @bit8) @bit8 => a or b\n\
               outer: function (a: @bit8, b: @bit8, c: @bit8) @bit8 => a or inner(b, c)\n\
               main: function () nothing {\n    r: @bit8 = outer(0x1, 0x2, 0x3)\n}\n";
    let (diags, hir) = check_hir(src);
    assert!(!diags.has_errors(), "{diags:?}");
    let outer = hir.funcs.iter().find(|f| f.name == "outer").expect("outer");
    let outer_if = outer
        .body
        .stmts
        .iter()
        .find_map(|s| match s {
            HirStmt::If { out: Some(t), then, els, .. } => Some((*t, then, els)),
            _ => None,
        })
        .expect("outer `or` join");
    let nested = |block: &lilac_hir::HirBlock| {
        block.stmts.iter().any(|s| matches!(s, HirStmt::If { out: Some(_), .. }))
    };
    assert!(
        nested(outer_if.1) || nested(outer_if.2),
        "inner prelude must be nested inside the outer `or` branch"
    );
}

#[test]
fn inline_control_flow_in_constant_is_e030() {
    let src = "or2: function (a: @bit8, b: @bit8) @bit8 => a or b\n\
               c: @bit8 = or2(0x1, 0x2)\n";
    let diags = check_src(src);
    assert!(has_code(&diags, "E030"), "{diags:?}");
}

#[test]
fn overloaded_hook_probe_does_not_double_emit_prelude() {
    // Two same-arity overloads force `select_hook_overload` to probe the
    // argument; the inline `or` prelude must be emitted exactly once.
    let src = "@pick: function (x: @bit8) @bit8 => x\n\
               @pick: function (x: @bit16) @bit16 => x\n\
               or2: function (a: @bit8, b: @bit8) @bit8 => a or b\n\
               main: function () nothing {\n    r: @bit8 = @pick(or2(0x1, 0x2))\n}\n";
    let (diags, hir) = check_hir(src);
    assert!(!diags.has_errors(), "{diags:?}");
    let main = hir.funcs.iter().find(|f| f.name == "main").expect("main");
    let joins = main
        .body
        .stmts
        .iter()
        .filter(|s| matches!(s, HirStmt::If { out: Some(_), .. }))
        .count();
    assert_eq!(joins, 1, "the inline `or` prelude must be emitted once");
}
```

(Note: overloaded direct non-operator hook calls do not lower today — they fail `E070` at IR verify. That is a separate, out-of-scope defect; this test is typeck-only via `check_hir`.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lilac-typeck inline_or_body_checks_and_inlines inline_if_value_body_checks_and_inlines inline_two_or_bodies_use_distinct_temps inline_control_flow_in_constant_is_e030 overloaded_hook_probe_does_not_double_emit_prelude`
Expected: the first two FAIL with `E030 "control flow in `=>` bodies"`; `inline_two_or_bodies_use_distinct_temps` FAILS with `E030`; `inline_control_flow_in_constant_is_e030` PASSES (already `E030`); `overloaded_hook_probe_does_not_double_emit_prelude` FAILS with `E030`.

- [ ] **Step 3: Extend the data model**

- `CheckedBody::Inline` (`check.rs:32`) becomes `{ param_names: Vec<String>, prelude: Vec<HirStmt>, value: HirExpr }`.
- `Checker.inline_bodies` (`check.rs:251`) becomes `HashMap<DefId, (Vec<String>, Vec<HirStmt>, HirExpr)>`.
- In `check_func` (`check.rs:1255`), store the prelude: `self.inline_bodies.insert(resolved.id, (param_names, prelude, value));`.

- [ ] **Step 4: Capture the prelude in `check_func_body`**

In the `FuncBody::Inline(expr)` arm (`check.rs:1131`), after the existing `expect_type`/`convert_if_needed`, replace the `has_temp` rejection with:

```rust
let prelude = std::mem::take(&mut fctx.blocks[0]);
if let Some((name, _, _)) = fctx.slots.first() {
    fctx.assigned.insert(name.clone());
}
let param_names: Vec<String> =
    decl.params.iter().map(|param| param.name.clone()).collect();
CheckedBody::Inline { param_names, prelude, value }
```

(`fctx.blocks` holds exactly one block after a balanced check; the prelude is the body block's statements.) Do **not** remove `has_temp` — the module-constant path (`check.rs:942`) still uses it for `E030`.

- [ ] **Step 5: Generalize substitution over statements**

Keep `subst_ident` and add a statement walker next to it (`check.rs:546`). `subst_stmts` maps each `HirStmt`, recursing into embedded `HirExpr`s (via `subst_ident`) and nested `HirBlock`s; `HirStmt::If`/`Match`/`Loop` recurse into their blocks, `Let`/`MultiLet`/`Assign`/`ExprStmt` substitute their expressions.

```rust
fn subst_stmts(stmts: &[HirStmt], map: &HashMap<String, HirExpr>) -> Vec<HirStmt>;
fn subst_block(block: &HirBlock, map: &HashMap<String, HirExpr>) -> HirBlock;
```

- [ ] **Step 6: Add temp renaming**

Add a walker that rewrites `TempId`s. Cover `HirExprKind::Temp`, `HirStmt::If { out }`, and `HirStmt::Match { out }`, recursing into all sub-expressions and nested blocks.

```rust
fn rename_temps_in_expr(expr: &HirExpr, map: &HashMap<TempId, TempId>) -> HirExpr;
fn rename_temps_in_stmts(stmts: &[HirStmt], map: &HashMap<TempId, TempId>) -> Vec<HirStmt>;
```

Add a collector that returns every `TempId` in a prelude+value (same coverage), so the expander can allocate one fresh temp per distinct id.

- [ ] **Step 7: Expand with renaming, substitution, and emission**

Rewrite `check_inline_call` (`check.rs:3053`). After `let Some((param_names, prelude, body_hir)) = self.inline_bodies.get(&def).cloned() else { … }` and `let hir_args = self.check_args(...)?;`:

```rust
// Rename the *un-substituted* body's temps first, then substitute: a
// substituted argument may itself be a `Temp` whose defining prelude the
// caller already emitted under its original id, so renaming after
// substitution would desynchronize the definition.
let temps: HashSet<TempId> = collect_temps(&prelude, &body_hir);
let mut ordered: Vec<TempId> = temps.into_iter().collect();
ordered.sort_unstable_by_key(|t| t.0);
let ren: HashMap<TempId, TempId> =
    ordered.into_iter().map(|t| (t, self.fresh_temp())).collect();
let prelude = rename_temps_in_stmts(&prelude, &ren);
let body_hir = rename_temps_in_expr(&body_hir, &ren);
let map: HashMap<String, HirExpr> = param_names.into_iter().zip(hir_args).collect();
let value = subst_ident(&body_hir, &map);
let prelude = subst_stmts(&prelude, &map);
if let Some(top) = fctx.blocks.last_mut() {
    top.extend(prelude);
}
Ok(CallOutcome::Inline(value, sig.returns[0]))
```

Emit the prelude into the current block at expansion time: this is the evaluation point, and `check_nested` (used for `or`/`and` right operands and `if`/`match` values) has already pushed the branch block, so a nested prelude lands in the right block.

- [ ] **Step 8: Stop the overload probe from emitting a prelude**

In `select_hook_overload` (`check.rs:2980`), probe with a discarded prelude so the real check emits it once:

```rust
let (got, _, _) = self.check_nested(fctx, arg, None);
```

- [ ] **Step 9: Run the tests**

Run: `cargo test -p lilac-typeck inline_ cmp_`
Expected: PASS.

- [ ] **Step 10: Run the full typeck suite**

Run: `cargo test -p lilac-typeck`
Expected: PASS.

- [ ] **Step 11: Update the `E030` doc**

In `docs/errors/E030.md`, remove the "control flow in constants and `=>` bodies"
trigger and its example (the `=>`-body half no longer fires; constants still do —
keep the constant trigger and reword it to name only constants).

- [ ] **Step 12: Commit**

```bash
git add crates/lilac-typeck/src/check.rs docs/errors/E030.md
git commit -m "typeck: inline => bodies carry a statement prelude (control flow inlines)"
```

---

### Task 3: Inline prelude for generic `=>` bodies

**Files:**
- Modify: `crates/lilac-typeck/src/check.rs` (`Checker.instance_inline` ~267), `crates/lilac-typeck/src/check/generics.rs` (capture ~522, inline path ~593)
- Test: `crates/lilac-typeck/src/check/generics.rs` (`mod tests`)

**Interfaces:**
- Consumes: `subst_stmts`, `rename_temps_in_stmts`, `rename_temps_in_expr`, `collect_temps`, `fresh_temp` (Task 2).
- Produces: `instance_inline: HashMap<(DefId, Vec<TypeRef>), (Vec<String>, Vec<HirStmt>, HirExpr)>`.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` in `crates/lilac-typeck/src/check/generics.rs` (use its existing `check_src`-style helper; if it lacks one, add the same helper as Task 2):

```rust
#[test]
fn generic_inline_control_flow_checks() {
    let src = "pick[T]: function (x: T, c: @bit8) T => if c x else x\n\
               main: function () nothing {\n    y: @bit32 = pick[@bit32](0x1, 0x1)\n}\n";
    let diags = check_src(src);
    assert!(!diags.has_errors(), "{diags:?}");
}
```

(Generic declaration syntax is `name[T]: function …`; instantiation is `name[@type](…)`. Value-`if` is `if c x else x`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-typeck generic_inline_control_flow_checks`
Expected: FAIL with `E030`.

- [ ] **Step 3: Store the prelude for instances**

In `check/generics.rs` where an inline instance is cached (`instance_inline.insert`, near `check.rs:532` region), capture the body block's statements exactly as Task 2 Step 4 and store `(param_names, prelude, value)`.

- [ ] **Step 4: Expand instances like the concrete path**

In the generic inline path (`check/generics.rs:593`), mirror Task 2 Step 7: fetch `(param_names, prelude, body)`, `check_args`, allocate fresh temps and rename the body first, then substitute params into prelude+value, emit the prelude into `fctx.blocks.last_mut()`, and return `CallOutcome::Inline(value, sig.returns[0])`.

- [ ] **Step 5: Run the test**

Run: `cargo test -p lilac-typeck generic_inline_control_flow_checks`
Expected: PASS.

- [ ] **Step 6: Update the pinned driver regression test**

`crates/lilac-driver/src/lib.rs:507` `generic_inline_control_flow_defers_without_ice`
asserts the generic `=>` body defers with `E030` (a guard against the lowering
`use of unassigned join temp` panic). Now that generic inline bodies carry a
prelude, rename it (e.g. `generic_inline_control_flow_inlines`) and assert the
opposite: `session.compile_lir(...)` succeeds and `render_diagnostics()` is
empty for the same source.

- [ ] **Step 7: Run the full suite**

Run: `cargo test -p lilac-typeck; cargo test -p lilac-driver`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/lilac-typeck/src/check.rs crates/lilac-typeck/src/check/generics.rs crates/lilac-driver/src/lib.rs
git commit -m "typeck: generic inline bodies carry a statement prelude too"
```

---

### Task 4: Fix `lib/Standard` and add the stdlib test

**Files:**
- Modify: `lib/Standard/int.lilac`, `int8.lilac`, `int16.lilac`, `int32.lilac`, `int64.lilac`
- Create: `crates/lilac-cli/tests/stdlib.rs`

**Interfaces:**
- Consumes: the two fixes (Tasks 1–3).
- Produces: a CLI test that fails if `lib/Standard` regresses.

- [ ] **Step 1: Write the failing test**

Create `crates/lilac-cli/tests/stdlib.rs`:

```rust
//! The standard library under `lib/Standard` must check and emit cleanly.
//! Folder mode is required: a single `int64.lilac` file cannot see `Bool`.

use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

#[test]
fn standard_library_checks_and_emits() {
    for sub in ["check", "emit-ir"] {
        let out = lilac()
            .args([sub, "../../lib/Standard"])
            .output()
            .expect("runs lilac");
        assert!(
            out.status.success(),
            "`lilac {sub} lib/Standard` failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-cli standard_library_checks_and_emits`
Expected: FAIL (intrinsic `E021` / `E010` / `E030`).

- [ ] **Step 3: Fix the stdlib intrinsic names**

In each of `lib/Standard/int.lilac`, `int8.lilac`, `int16.lilac`, `int32.lilac`, `int64.lilac`:

- Replace `@iadd` → `@i_add`, `@isub` → `@i_sub`, `@imul` → `@i_mul`, `@idiv` → `@i_div`, `@imod` → `@i_mod`.
- In `@negate`, replace `@f_negate` → `@i_negate`.

- [ ] **Step 4: Run the test; fix any further stdlib diagnostics**

Run: `cargo test -p lilac-cli standard_library_checks_and_emits`
Expected: PASS. If `check` still reports diagnostics, fix them **in `lib/Standard`** (the goal is a clean stdlib); if a diagnostic reveals a genuine compiler bug, stop and report it rather than masking it in the stdlib.

- [ ] **Step 5: Commit**

```bash
git add lib/Standard crates/lilac-cli/tests/stdlib.rs
git commit -m "stdlib: fix intrinsic names; add lib/Standard check+emit test"
```

---

### Task 5: Remove the VM integration and bench tests

**Files:**
- Delete: `crates/lilac-vm/tests/runner.rs`, `crates/lilac-vm/tests/bench.rs`, `crates/lilac-vm/src/treewalk.rs`
- Modify: `crates/lilac-vm/src/lib.rs`, `crates/lilac-vm/Cargo.toml`, `AGENTS.md`

**Interfaces:**
- Consumes: nothing.
- Produces: no VM integration/bench tests; no `bench-baseline` gate.

- [ ] **Step 1: Find remaining consumers of the bench scaffolding**

Run: `git grep -n "bench-baseline\|treewalk" -- crates`
Expected: only `crates/lilac-vm/tests/bench.rs`, `crates/lilac-vm/src/treewalk.rs`, `crates/lilac-vm/src/lib.rs`, `crates/lilac-vm/Cargo.toml` (docs under `docs/` are historical and untouched).

- [ ] **Step 2: Delete the files**

Delete `crates/lilac-vm/tests/runner.rs`, `crates/lilac-vm/tests/bench.rs`, and `crates/lilac-vm/src/treewalk.rs`.

- [ ] **Step 3: Drop the feature and export**

- `crates/lilac-vm/Cargo.toml`: remove the `[features]` table and its comment (the only feature is `bench-baseline`).
- `crates/lilac-vm/src/lib.rs`: remove the `#[cfg(feature = "bench-baseline")]` module/export lines (`~16`, `~25`). If `dev-dependencies` `lilac-driver` becomes unused, remove it too.

- [ ] **Step 4: Drop the AGENTS.md gate lines**

In `AGENTS.md`, remove the two release VM lines from the Gates block:

```
cargo clippy --release -p lilac-vm --all-targets --features bench-baseline -- -D warnings
cargo test --release -p lilac-vm --features bench-baseline
```

- [ ] **Step 5: Verify the workspace**

Run: `cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace`
Expected: all green; `git grep bench-baseline -- crates AGENTS.md` returns nothing.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "vm: remove integration and bench tests and the bench-baseline gate"
```

---

## Final verification

- [ ] Run the full gate set: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- [ ] Run `cargo run -p lilac-cli -- check lib/Standard` and `cargo run -p lilac-cli -- emit-ir lib/Standard`; both exit 0.
- [ ] Confirm the spec's non-goals were not crossed: no grammar/HIR-shape/lowering/backend edits; `and`/`or` still short-circuit; no C-ABI or numeric-type work.
