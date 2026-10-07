# Lilac Error-Tolerant Analysis

> Scope: make the front-end produce a partial, always-available analysis
> (resolved project + signatures + checked HIR carrying name/type sites) from a
> buffer that has lex, parse, or type errors, so editor features — hover,
> definition, and the upcoming semantic tokens — keep working while you type.
> Sub-project 1 of the semantic-highlighting work. Branch `ai`. LLVM-only.

## 1. Outcome / constraints / success

**Outcome**

- The parser always returns a `Program`. A construct that fails to parse
  becomes an **error AST node** rather than aborting the file or dropping the
  enclosing item, so a broken function keeps its partial body and the rest of
  the file keeps its items.
- The driver runs `collect` → `collect_sigs` → `check_bodies` even when errors
  exist and retains `Session::analysis` (project, types, sigs, and HIR carrying
  `uses`) after each pass. `analysis` is therefore available for a buffer with
  lex/parse/type errors.
- Editor tooling (the LSP) consumes this partial analysis; the CLI keeps
  today's strict, stop-at-the-first-failing-stage behavior and byte-identical
  diagnostics/snapshots.
- No lowering/codegen change: the pipeline still refuses to lower when errors
  exist, so partial IR is never produced.

**Constraints**

- Compiler crates stay Rust std-only; no new dependencies. `unsafe_code` and
  `clippy::pedantic` stay in force.
- `check` / `emit-ir` / `emit-llvm` output and committed snapshots are
  unchanged (strict mode).
- No new diagnostic codes; every existing diagnostic is preserved with the same
  code, message, and spans.
- One pipeline serves both modes; a session flag selects strict vs tolerant.

**Success**

- `parse_program` returns `Ok` for a file with a syntax error, containing the
  good items plus error nodes, and emits exactly the diagnostics it does today.
- In tolerant mode, `Session::analysis()` is `Some` with non-empty `hir.uses`
  for (a) a file whose one function has a syntax error and (b) a file whose one
  body has a type error — the other items still contribute name/type sites.
- In strict mode, `check` output is byte-identical to today for the existing
  UI fixtures.
- `cargo test --workspace` is green.

**Non-goals**

- Semantic tokens and any LSP/VSCode change (sub-project 2).
- Completion and rename themselves (this foundation enables them later).
- Incremental parsing; the whole file is re-lexed/re-parsed each compile.
- New diagnostics for recovered input beyond the syntax/type error that caused
  the node.

### 1.1 Decisions locked during brainstorming

1. **Full error-node AST (Approach C)**, not merely a tolerant side path,
   because completion/rename need the parser to preserve the enclosing context
   around the cursor.
2. **The CLI stays strict**; only editor sessions run tolerant, selected by a
   session flag.
3. **One pipeline**: tolerant mode removes the early bails and retains the
   analysis; lowering still requires a clean compile.
4. Keywords/operators stay with the TextMate grammar (sub-project 2); no new
   keyword/operator spans are added here.

## 2. Problem

The pipeline is all-or-nothing per stage. `parse_program` recovers past broken
items but returns `Err` if any item failed (`lilac-parse/src/lib.rs:78`), so
`run_pipeline` discards the partial `Program`. `finish_pipeline` then bails
whenever `self.diags.has_errors()` after `collect_sigs`
(`lilac-driver/src/lib.rs:285`), so `check_bodies` — which is already
error-tolerant and returns a partial `HirProgram` (`check.rs:158`) — never runs
on a buffer with errors. The retained `Analysis` is consequently empty (no
`uses`, no `exprs`), and every editor feature that depends on it (hover,
definition, future semantic tokens) goes dark exactly when the user is typing.

## 3. Architecture / components

```
crates/lilac-ast/src/lib.rs        # ItemKind::Error, Stmt::Error, Expr::Error; poison Type
crates/lilac-parse/src/lib.rs      # recover into error nodes; parse_program always Ok
crates/lilac-resolve/src/lib.rs    # skip error items; poison an empty type name
crates/lilac-typeck/src/sigs.rs    # skip error items; poison a missing/empty sig
crates/lilac-typeck/src/check.rs   # tolerate defs without a signature (poison, no bug!)
crates/lilac-driver/src/lib.rs     # session flag; run passes + retain analysis when tolerant
crates/lilac-lsp/src/server.rs     # set the tolerant flag for editor sessions
```

**Untouched:** `lilac-lex`, `lilac-hir` (its `HirExprKind::Poison` already
exists), `lilac-lower`, `lilac-ir`, `lilac-codegen-llvm`, `lilac-const`,
`lilac-module`, and the VSCode extension.

## 4. Parser error recovery

**New AST nodes** (`lilac-ast`):

- `ItemKind::Error` — a top-level/member item that failed; `Item.span` covers
  the consumed tokens. No payload (other `ItemKind`s carry no span either).
- `Stmt::Error { span }` — a failed statement.
- `Expr::Error { span }` — a failed expression.
- A failed type is a **poison `Type`** with an empty `name` (`Type` stays a
  struct; a user type name is never empty, so the empty name is the poison
  sentinel). Resolve/typeck map it to `TypeId::ERROR`.

The AST `Visitor`/`MutVisitor` and `walk_*` functions gain arms for the three
new variants; the poison `Type` needs no walker change.

**Parser behavior** (`lilac-parse`):

- `parse_program` returns `Ok(Program)` unconditionally. The existing
  item-level `synchronize` still resynchronizes after a broken item, but the
  broken item is now pushed as `ItemKind::Error` instead of being skipped.
- Within a body, a failed statement or expression yields `Stmt::Error` /
  `Expr::Error` and parsing continues at the next synchronization point, so the
  enclosing function body survives.
- `parse_type` failure yields the poison `Type` and continues; the enclosing
  item/statement is not discarded.
- Diagnostics are emitted exactly as today (same code/message/span); the parser
  only changes what it *returns*, never what it *reports*. The `self.failed`
  flag is removed (its only use was the `Err` return).

## 5. Driver analysis retention

`Session` gains a boolean flag `retain_partial_analysis` (default `false`).
Strict mode (the CLI) reproduces today's stage gates exactly; tolerant mode
(the LSP) continues past errors to build the analysis. In `run_pipeline`:

1. Parse. In strict mode, if `has_errors()` after parsing, return `Err`
   (today's behavior — a parse error stops before `collect`). In tolerant mode,
   continue.
2. `collect`, then `collect_sigs`. In strict mode, if `has_errors()`, return
   `Err` (today's behavior). In tolerant mode, continue.
3. `check_bodies` runs always (tolerant mode may reach it with errors). The
   partial `HirProgram` (with `uses`) is stored on `analysis` after the call,
   as today.
4. If `has_errors()`, return `Err` **before** `lower` in both modes — no
   partial IR is ever lowered, so `check`/`emit-*` still fail and the CLI stays
   strict.
5. On a clean compile, the pipeline proceeds unchanged through lowering,
   folding, runtime injection, pruning, and verification.

`run_pipeline` no longer `?`-bails on a parser `Err` (there is none now); the
parse diagnostics are still emitted.

The CLI is unchanged: `Session::new()` leaves the flag `false`. The LSP sets it
`true` in its `new_session()` helper (`lilac-lsp/src/server.rs`).

## 6. Downstream passes

- **`lilac-resolve`:** skip `ItemKind::Error` items when collecting names (they
  define nothing); an empty-name `Type` resolves to `TypeId::ERROR` with no
  `E023` (the parser already reported the syntax error). This keeps error items
  from producing spurious "unknown type" cascades.
- **`lilac-typeck::collect_sigs`:** skip error items; a signature whose type is
  poison is recorded with `TypeId::ERROR` (as it already does for poisoned
  types) and does not itself emit a new diagnostic.
- **`lilac-typeck::check_bodies`:** tolerate a reachable definition that has no
  usable signature (because its item was an error or its sig is poison): poison
  that definition's body instead of `bug!`-panicking. `HirExprKind::Poison`
  already models the poison result.
- **`lilac-lower` / codegen:** unreached in tolerant mode when errors exist, so
  no change. Error nodes never reach lowering.

## 7. Diagnostics

- No new codes. The parser emits the same diagnostics it does today; the new
  error nodes are not themselves diagnostic-bearing.
- Error items and poison types are silent downstream (no `E023`/`E043`
  cascades), so a single syntax error stays a single diagnostic.

## 8. Testing

- **`lilac-parse`:** a file with one broken item and one good item returns
  `Ok`, contains `ItemKind::Error` plus the good item, and reports the same
  diagnostic as today; a broken statement/expression inside a function yields
  `Stmt::Error`/`Expr::Error` while the function and its siblings survive; a
  broken type yields a poison `Type`.
- **`lilac-driver`:** in tolerant mode, `analysis()` is `Some` with non-empty
  `hir.uses` after (a) a file with a syntax error in one function and (b) a
  file with a type error in one body; in strict mode, `analysis` is not
  required and `check` behavior is unchanged.
- **`lilac-cli`:** the existing UI fixtures' `check`/`emit-ir`/`emit-llvm`
  snapshots are unchanged (strict mode).
- **`lilac-lsp`:** hover/definition still resolve on a project whose body has a
  type error (existing test) and now also on one whose function has a syntax
  error.
- All gates stay green.

## 9. Gates

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`

## 10. Maintenance / source of truth

- The parser's error nodes are the single representation of "unparseable
  input"; every downstream pass keys off them (skip or poison), never off a
  re-parse.
- The strict/tolerant split is one flag on `Session`; strict mode must remain
  byte-identical to today, so any new early-continue belongs behind the flag.
- `HirExprKind::Poison` is the single HIR poison; error nodes map onto it.

## 11. Follow-ups

- **Sub-project 2:** LSP semantic tokens (capability + derivation from the
  retained index) and the VSCode legend/theme.
- Later, enabled by this foundation: completion and rename/references, which
  can now rely on an analysis that survives broken input.
