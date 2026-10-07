# Lilac `as` Operator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the postfix `as` cast — the explicit form of the implicit `@conversion` path, usable in any expression position — with the builtin `ref unknown` ↔ any `ref T` exception.

**Architecture:** Reserve the keyword, add an `Expr::Cast` AST node, parse `as Type` in the postfix loop, and resolve it in typeck by threading the target as the operand's expected type (literals pin; typed values take one `@conversion`; the ref-unknown case retypes). No new HIR variant and no lowering/codegen change.

**Tech Stack:** Rust workspace; compiler crates std-only.

**Spec:** `docs/superpowers/specs/2026-10-07-lilac-as-operator-design.md`

## Global Constraints

- Compiler crates stay Rust std-only; add no dependencies. `unsafe_code = "forbid"` and `clippy::pedantic = deny` hold; no `unwrap`/`expect`/`panic!` on user input in production code (tests may).
- `as` uses the same one-step `@conversion` machinery as the implicit path; no chaining; `E043` on a missing conversion; `E010` on a non-exception ref mismatch.
- No new HIR variant and no `lilac-lower`/`lilac-codegen-llvm` change.
- Gates: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`.

## Review Focus

- `as` binds looser than binary operators, so `a + b as T` is `(a + b) as T` — pinned by the Task 2 parser test.
- A literal operand does not pin (a conversion is demanded for `0x1 as @bit16`) — pinned by the Task 2 typeck test.
- The `ref unknown` exception is not handled before the expected-type check, so `p as ref Foo` is `E010` instead of a pointer cast — pinned by the Task 2 ref tests.
- `as` is reserved but still lexes as an identifier — pinned by the Task 1 lexer test.
- A new HIR variant is added (the plan forbids it) — pinned by the Task 2 diff (no `lilac-hir` change) and the emit-llvm fixture.

---

### Task 1: Reserve the `as` keyword

**Files:**
- Modify: `crates/lilac-lex/src/lib.rs` (`TokenKind`, the keyword table)
- Test: `crates/lilac-lex/src/lib.rs` tests

**Interfaces:**
- Consumes: nothing.
- Produces: `TokenKind::KwAs`; `as` is no longer lexed as `Ident`.

- [ ] **Step 1: Write the failing test**

Add a test asserting `lex("as")` yields one `KwAs` token, and that a source using `as` as an identifier (e.g. `as = 1`) no longer lexes `as` as an `Ident`.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p lilac-lex kw_as`
Expected: FAIL (`as` lexes as `Ident`).

- [ ] **Step 3: Add the keyword**

Add `KwAs` to `TokenKind` and map `"as"` to it in the keyword table (`lex_word`), following the existing `Kw*` pattern.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p lilac-lex kw_as`
Expected: PASS.

- [ ] **Step 5: Full gates and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green (no grammar consumes `KwAs` yet; no existing source uses `as` as an identifier).

```bash
git add crates/lilac-lex
git commit -m "lex: reserve the as keyword"
```

---

### Task 2: Parse and type-check `as`

**Files:**
- Modify: `crates/lilac-ast/src/lib.rs` (`Expr::Cast`)
- Modify: `crates/lilac-parse/src/lib.rs` (`parse_postfix`)
- Modify: `crates/lilac-typeck/src/check.rs` (`check_cast`; the `Expr::Cast` arm)
- Modify: any other exhaustive `Expr` matcher the new variant forces (e.g. `lilac-resolve`, `lilac-lsp`) — handle `Cast` by recursing into the operand
- Test: `crates/lilac-parse/src/lib.rs`, `crates/lilac-typeck/src/check.rs`

**Interfaces:**
- Consumes: Task 1 (`KwAs`).
- Produces: `Expr::Cast(Box<Expr>, Type)`; `as T` supplies `T` as the operand's expected type (literal pins/grows; a typed operand not already `T` takes one `@conversion`, else `E043`; a redundant cast is identity); the `ref unknown` ↔ `ref T` builtin cast.

- [ ] **Step 1: Write the failing parser test**

Add parser tests: `x as T` → `Cast(Ident(x), T)`; `a.b as T` → `Cast(Field(a,b), T)`; `a + b as T` → `Add(a, Cast(b, T))`; `x as A as B` → `Cast(Cast(x, A), B)`; `x as ref @bit8` parses.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p lilac-parse as_cast`
Expected: FAIL (no `as` grammar).

- [ ] **Step 3: Add the AST node and parser arm**

Add `Expr::Cast(Box<Expr>, Type)` to `lilac-ast`. In `parse_postfix`, add an `as` arm to the postfix loop that consumes `KwAs` and a `parse_type` (which already accepts `ref T`), wrapping the current expression. Update every exhaustive `Expr` match the new variant forces to recurse into the operand.

- [ ] **Step 4: Run the parser test to verify it passes**

Run: `cargo test -p lilac-parse as_cast`
Expected: PASS.

- [ ] **Step 5: Write the failing typeck tests**

Add typeck tests: `@bit8 as @bit16` with a `@bit8 → @bit16` hook compiles and emits the conversion; without a hook is `E043`; `0x1 as @bit16` is `@bit16`; `x as @bit16` (already `@bit16`) is identity; `3.14 as @fbit32` is `@fbit32`; `ref unknown as ref Foo` and `ref Foo as ref unknown` compile with no hook; a non-`unknown` ref mismatch is `E010`.

- [ ] **Step 6: Run them to verify they fail**

Run: `cargo test -p lilac-typeck as_cast`
Expected: FAIL (`Expr::Cast` is unhandled or unresolvable).

- [ ] **Step 7: Implement `check_cast`**

In `check.rs`, handle `Expr::Cast`: branch on whether the target is a ref type; for a ref target, check the operand with no expected type and apply the `ref unknown` ↔ `ref T` retype (else `E010`); otherwise check the operand with the target as its expected type, return identity when the type already matches, and otherwise resolve one `@conversion` (`ops::convert` + `convert_if_needed`, `E043` on failure). Emit the operand HIR (identity / ref cast) or the conversion call HIR (real cast); add no HIR variant.

- [ ] **Step 8: Run the typeck tests to verify they pass**

Run: `cargo test -p lilac-typeck as_cast`
Expected: PASS.

- [ ] **Step 9: Full gates and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green.

```bash
git add crates
git commit -m "typeck: implement the as cast operator"
```

---

### Task 3: Document `as` and add an end-to-end fixture

**Files:**
- Modify: `SPEC.md` (an `as` section)
- Modify: `docs/errors/E043.md`, `docs/errors/E010.md` (an `as` example)
- Create: `tests/ui/as_cast.lilac` + `.lir`/`.ll` snapshots (a real cast and a ref-unknown cast through codegen)

**Interfaces:**
- Consumes: Task 2.
- Produces: documented `as`; a codegen-level regression fixture.

- [ ] **Step 1: Add the fixture**

Add `tests/ui/as_cast.lilac` exercising a real `@conversion`-backed cast and a `ref unknown` ↔ `ref T` cast, then generate its `.lir`/`.ll` snapshots by running it through `lilac emit-ir`/`emit-llvm` without `--check`.

- [ ] **Step 2: Document `as`**

Add an `as` section to `SPEC.md` (surface/precedence; `as T` semantics; the ref-unknown exception; `as` reserved) and `as` examples to `docs/errors/E043.md`/`E010.md`.

- [ ] **Step 3: Verify and run the full gates**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green (the fixture is auto-discovered by the glob-based snapshot/codegen tests).

- [ ] **Step 4: Commit**

```bash
git add SPEC.md docs/errors tests/ui
git commit -m "docs: document the as operator"
```

---

## Self-Review

**Spec coverage:** §4 surface → Tasks 1–2; §5 `as T` → Task 2; §6 ref-unknown → Task 2; §7 no lowering change → Task 2 (constraint) + Task 3 fixture; §8 docs → Task 3; §9 testing → Tasks 1–3; §10 gates → all. No section unmapped.

**Ordering:** Task 1 reserves the keyword with no grammar (green). Task 2 adds the AST node, parser arm, and typeck resolution together (the new variant forces the exhaustive matches). Task 3 is docs + a codegen fixture.

**Type/name consistency:** `Expr::Cast(Box<Expr>, Type)`, `KwAs`, `check_cast`, `E043`/`E010`, `@conversion`, `ref unknown` are spelled identically wherever they appear.
