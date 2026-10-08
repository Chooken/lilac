# Lilac Error-Tolerant Analysis Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the front-end produce a partial, always-available analysis (project + sigs + checked HIR with name/type sites) from a buffer with lex/parse/type errors, so editor features keep working while typing.

**Architecture:** The parser recovers into error AST nodes and always returns a `Program`; a `Session` flag selects strict (CLI, unchanged) vs tolerant (LSP) mode; tolerant mode runs `collect_sigs`/`check_bodies` on the recovered AST and retains `Session::analysis`. Lowering still requires a clean compile, so no partial IR is emitted.

**Tech Stack:** Rust workspace; compiler crates std-only.

**Spec:** `docs/superpowers/specs/2026-10-08-lilac-error-tolerant-analysis-design.md`

## Global Constraints

- Compiler crates stay Rust std-only; add no dependencies. `unsafe_code = "forbid"` and `clippy::pedantic = deny` hold; no `unwrap`/`expect`/`panic!` on user input in production code (tests may).
- Strict mode (the CLI) must stay byte-identical: `check`/`emit-ir`/`emit-llvm` output and committed snapshots unchanged. No new diagnostic codes; every existing diagnostic keeps its code, message, and spans.
- Tolerant mode never lowers: when errors exist the pipeline stops before `lower` in both modes.
- Gates: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`.

## Review Focus

- A broken *item* must not discard the file's other items — pinned by the Task 1 parser test.
- A broken statement/expression inside a body must not discard the enclosing function — pinned by the Task 2 parser test.
- A single syntax error must stay a single diagnostic (no `E023`/`E043` cascade from error nodes) — pinned by the Task 3 test.
- Tolerant mode must reach `check_bodies` with a signatureless definition without `bug!`-panicking — pinned by the Task 3 test.
- Strict mode's diagnostics must be unchanged for a file with a syntax error — pinned by the Task 1 CLI test and the Task 4 driver test.

---

### Task 1: Item-level error recovery

**Files:**
- Modify: `crates/lilac-ast/src/lib.rs` (`ItemKind::Error`; visitors/walkers)
- Modify: every exhaustive `ItemKind` match the new variant forces (e.g. `lilac-resolve`, `lilac-typeck`, `lilac-lower`, `lilac-lsp`) — handle `Error` by skipping the item
- Modify: `crates/lilac-parse/src/lib.rs` (`parse_program`; remove `self.failed`)
- Modify: `crates/lilac-driver/src/lib.rs` (`Session::retain_partial_analysis`; post-parse gate)
- Test: `crates/lilac-parse/src/lib.rs`, `crates/lilac-cli/tests/*`

**Interfaces:**
- Consumes: nothing.
- Produces: `ItemKind::Error` (unit variant); `Session::retain_partial_analysis: bool` (public field, default `false`); `parse_program` always returns `Ok(Program)`.

- [ ] **Step 1: Write the failing parser test**

Add a test: parsing `"ok: function () nothing {\n}\nbad: function (\n"` returns `Ok`, has exactly one `ItemKind::Error` item and one good item, and records the same parse diagnostic as today.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p lilac-parse item_error_recovery`
Expected: FAIL (`parse_program` returns `Err`).

- [ ] **Step 3: Add the AST node and parser recovery**

Add `ItemKind::Error` to `lilac-ast` and an arm to the walkers. In `parse_program`, push `ItemKind::Error` (span = the failed item's tokens) instead of skipping, keep the existing `synchronize`, and return `Ok(Program { items, span })` unconditionally; replace the `self.failed`-gated `Err` return with a parser flag set whenever a construct fails, exposed as `Parser::had_errors()`. Update every exhaustive `ItemKind` match across the workspace to skip `Error` (no name/def recorded, no diagnostic).

- [ ] **Step 4: Run the parser test to verify it passes**

Run: `cargo test -p lilac-parse item_error_recovery`
Expected: PASS.

- [ ] **Step 5: Add the session flag and the post-parse strict gate**

Add `retain_partial_analysis: bool` (default `false`) to `Session` (and its `Default`). In `run_pipeline`, immediately after `parse_program`, add: if `parser.had_errors() && !self.retain_partial_analysis`, return `Err` — reproducing today's "a parse failure stops before `collect`". Gate on `parser.had_errors()`, **not** `has_errors()`: a lex error that still parses must keep today's behavior of running `collect`. `compile_project_with_stages` accumulates the same flag across its per-file parse loop.

- [ ] **Step 6: Write the CLI regression test**

Add a `lilac-cli` test that `check` on a file with one broken item exits non-zero and reports the same diagnostic (message/span) as before this change, and that the good item is not reported as an extra error.

- [ ] **Step 7: Run it to verify it passes, then the full gates**

Run: `cargo test -p lilac-cli item_error` then `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green (existing UI snapshots unchanged).

- [ ] **Step 8: Commit**

```bash
git add crates
git commit -m "parse: recover broken items into ItemKind::Error"
```

---

### Task 2: Statement / expression / type recovery

**Files:**
- Modify: `crates/lilac-ast/src/lib.rs` (`Stmt::Error { span }`, `Expr::Error { span }`; visitors/walkers)
- Modify: every exhaustive `Stmt`/`Expr` match the new variants force (skip/recurse/poison)
- Modify: `crates/lilac-parse/src/lib.rs` (block, statement, expression, and type recovery)
- Test: `crates/lilac-parse/src/lib.rs`

**Interfaces:**
- Consumes: Task 1 (`ItemKind::Error`).
- Produces: `Stmt::Error { span: Span }`; `Expr::Error { span: Span }`; a failed type parses to a **poison `Type`** (`name == ""`, empty `generic_args`, `func: None`), the empty name being the poison sentinel.

- [ ] **Step 1: Write the failing parser tests**

Add tests: (a) a function whose body has a broken statement (e.g. `"main: function () nothing {\n    x: @bit8 = \n}\n"`) still parses as a function item whose block contains a `Stmt::Error`, and the file reports the syntax diagnostic; (b) a statement with a broken type annotation parses with a poison `Type` (`name == ""`) and the statement survives; (c) the function's other statements are still parsed.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p lilac-parse stmt_error_recovery`
Expected: FAIL (the function becomes an `ItemKind::Error` or the parse aborts).

- [ ] **Step 3: Add the AST nodes and recovery**

Add `Stmt::Error { span }` and `Expr::Error { span }` to `lilac-ast` and the walkers. In the parser, recover at the construct boundaries: a failed statement in `parse_block` becomes `Stmt::Error` and parsing resumes at the next synchronization point; a failed expression in a statement/initializer position becomes `Expr::Error`; a failed `parse_type` yields the poison `Type`. Set the parser's `had_errors` flag on every recovered failure (so strict mode still stops before `collect`, as today). Update every exhaustive `Stmt`/`Expr` match to skip/poison the error nodes.

- [ ] **Step 4: Run the parser tests to verify they pass**

Run: `cargo test -p lilac-parse stmt_error_recovery`
Expected: PASS.

- [ ] **Step 5: Full gates and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green.

```bash
git add crates
git commit -m "parse: recover broken statements, expressions, and types"
```

---

### Task 3: Downstream tolerance (resolve, sigs, check_bodies)

**Files:**
- Modify: `crates/lilac-resolve/src/lib.rs` (skip `ItemKind::Error` items)
- Modify: `crates/lilac-typeck/src/sigs.rs` (poison an empty-name type; skip error items; poison a signature whose type is poison)
- Modify: `crates/lilac-typeck/src/check.rs` (tolerate a reachable def with no usable signature)
- Test: `crates/lilac-typeck/src/sigs.rs` / `check.rs`, `crates/lilac-resolve/src/lib.rs`

**Interfaces:**
- Consumes: Tasks 1–2 (error nodes).
- Produces: `collect`/`collect_sigs`/`check_bodies` run on a recovered AST without panicking and without extra diagnostics; error items define nothing.

- [ ] **Step 1: Write the failing tests**

Add: (a) a resolve/typeck test that a program with an `ItemKind::Error` item plus one good item collects the good item's name/sig and emits **no** `E023` for the error item or for a poison `Type`; (b) a `check_bodies` test that a definition whose signature is poison (or absent) is poisoned rather than `bug!`-panicking, while a sibling valid definition still yields HIR with non-empty `uses`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p lilac-typeck error_item` and `cargo test -p lilac-resolve error_item`
Expected: FAIL (spurious diagnostic, or `bug!` panic).

- [ ] **Step 3: Implement tolerance**

In `lilac-resolve`, skip `ItemKind::Error` items. In `sigs.rs` — where type names resolve and `E023` is emitted — map an empty-name `Type` to `TypeId::ERROR` with no diagnostic, skip error items, and record a poison-typed signature as `TypeId::ERROR` without a new diagnostic. In `check.rs`, when a reachable definition has no usable signature, poison its body (reuse `HirExprKind::Poison`) and continue instead of `bug!`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lilac-typeck error_item && cargo test -p lilac-resolve error_item`
Expected: PASS.

- [ ] **Step 5: Full gates and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green (strict mode never reaches these paths).

```bash
git add crates
git commit -m "typeck: tolerate error nodes in resolve, sigs, and body checking"
```

---

### Task 4: Tolerant retention in the driver

**Files:**
- Modify: `crates/lilac-driver/src/lib.rs` (`finish_pipeline`/`run_pipeline` gates)
- Test: `crates/lilac-driver/src/lib.rs`

**Interfaces:**
- Consumes: Tasks 1–3; `Session::retain_partial_analysis`.
- Produces: in tolerant mode, `Session::analysis()` is `Some` with non-empty `hir.uses` after a syntax-error or type-error compile; the pipeline still stops before `lower` when errors exist.

- [ ] **Step 1: Write the failing test**

Add a driver test: with `retain_partial_analysis = true`, compiling (a) a source whose one function has a syntax error and (b) a source whose one body has a type error yields `analysis()` `Some` with non-empty `hir.uses` from the surviving items. With the flag `false`, `check`'s behavior is unchanged.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p lilac-driver tolerant_analysis`
Expected: FAIL (`analysis` is `None` or `uses` empty).

- [ ] **Step 3: Gate the pipeline on the flag**

In `run_pipeline`/`finish_pipeline`, make the post-`collect_sigs` `has_errors()` gate conditional on `!retain_partial_analysis` (the post-parse gate is already gated in Task 1); always run `check_bodies` in tolerant mode and store its partial `HirProgram` on `analysis`; keep the pre-`lower` `has_errors()` gate unconditional in both modes.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p lilac-driver tolerant_analysis`
Expected: PASS.

- [ ] **Step 5: Full gates and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green.

```bash
git add crates/lilac-driver
git commit -m "driver: retain partial analysis in tolerant mode"
```

---

### Task 5: LSP opts into tolerant analysis

**Files:**
- Modify: `crates/lilac-lsp/src/server.rs` (`new_session`)
- Test: `crates/lilac-lsp/tests/diagnostics.rs` (or `hover.rs`/`definition.rs`)

**Interfaces:**
- Consumes: Task 4 (`retain_partial_analysis`).
- Produces: editor sessions run tolerant, so hover/definition (and later semantic tokens) work on a buffer with a syntax error.

- [ ] **Step 1: Write the failing test**

Add an LSP test: a project whose `main` contains a syntax error, but whose other function is valid and called from a resolvable site, still answers `textDocument/definition` (and hover) for the valid function's use — not `null`.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p lilac-lsp syntax_error`
Expected: FAIL (the index is empty, so the reply is `null`).

- [ ] **Step 3: Set the flag in `new_session`**

In `lilac-lsp/src/server.rs`, set `session.retain_partial_analysis = true` in the `new_session()` helper (both project and standalone paths already use it).

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p lilac-lsp syntax_error`
Expected: PASS.

- [ ] **Step 5: Full gates and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green.

```bash
git add crates/lilac-lsp
git commit -m "lsp: analyze tolerantly so features survive syntax errors"
```

---

## Self-Review

**Spec coverage:** §4 parser recovery → Tasks 1–2; §5 driver retention → Tasks 1, 4; §6 downstream → Task 3; §7 diagnostics → Tasks 1–3; §8 testing → each task; §9 gates → all; §10 maintenance → the flag and `HirExprKind::Poison` are named identically across tasks. No section unmapped.

**Ordering:** Task 1 keeps the CLI green (post-parse gate) while adding item recovery; Task 2 adds finer recovery; Task 3 makes the passes tolerate the nodes; Task 4 turns on retention; Task 5 flips the LSP. Each task ends green and independently testable.

**Type/name consistency:** `ItemKind::Error`, `Stmt::Error { span }`, `Expr::Error { span }`, poison `Type` (`name == ""`), `Session::retain_partial_analysis`, `HirExprKind::Poison`, `analysis()`, `hir.uses` are spelled identically wherever they appear.
