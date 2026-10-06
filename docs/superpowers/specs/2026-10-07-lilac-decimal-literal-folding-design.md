# Lilac — Decimal Literal Folding

> Scope: fold the arithmetic operators `+ - * / %` when both operands are
> decimal number literals, keeping the result a decimal literal. Branch `ai`.
> Adds a pure `lilac-decimal` crate (exact decimal-string arithmetic), a
> mutable AST visitor, and two CLI flags. Typeck, lowering, and all snapshots
> are otherwise untouched.

## 1. Outcome / constraints / success

**Outcome**

- `2 * 3` (two decimal literals) compiles to the single decimal literal `6`;
  `2 * 3 + 4` folds to `10`; `10.5 * 2` to `21`; `1 / 3` to
  `0.3333333333` (ten fractional digits, truncated by default).
- The folded value stays a decimal literal (a `@byte_buffer` `NumberLiteral`),
  exactly like the operands — no bit width is inferred.
- Division and modulo by zero are compile errors (`E060`).

**Constraints**

- Compiler crates stay std-only; no `serde`, no `unsafe`
  (`unsafe_code = "forbid"`), `clippy::pedantic` holds.
- Additive: existing operator behavior for every non-(decimal, decimal) pair is
  unchanged; no snapshot churn for programs that already compiled.
- No panics on user input.

**Success**

- The engine computes exactly (arbitrary precision) for `+ - * %`; `/` uses a
  configurable fixed scale.
- The canonical result string is deterministic (section 3.4).
- `--decimal-scale` and `--decimal-round` change output; defaults (10, trunc)
  are pinned by snapshot tests.
- The full gates pass (section 8).

**Non-goals**

- Folding hex/bin (`0x`/`0b`) literals, string literals, or any operand that is
  not a decimal number literal (including a variable typed `@byte_buffer`).
- Comparisons (`< > <= >= ==`), `and`/`or`, and unary operators.
- Runtime decimal arithmetic; inferring a bit representation for a folded
  literal; assigning a decimal literal to a `@bitN` (still `E043`/`E010`).
- Constant-folding through named `const`s or locals.

### 1.1 Decisions locked during brainstorming

1. **Literal-only, compile-time.** Both operands must be syntactic decimal
   number literals (`Expr::Number` whose text is not `0x`/`0b`).
2. **Result stays a decimal literal** (`@byte_buffer`), representation-less.
3. **Operators:** `+ - * / %` (`Add`, `Sub`, `Mul`, `Div`, `Mod`).
4. **`/`:** fixed scale, default **10**, default **truncate toward zero**;
   configurable via CLI (`--decimal-scale`, `--decimal-round=trunc|half-up`).
   `HalfUp` rounds half **away from zero**.
5. **`%`:** **floored** modulo — `a % b = a - b·floor(a/b)`, sign of the
   divisor, exact, no scale.
6. **Zero divisor:** `x / 0` and `x % 0` are compile errors (`E060`).
7. **Canonical rendering:** strip leading zeros, strip trailing fractional
   zeros, drop the point when the fraction is empty, render zero as `0` (never
   `-0`), one leading `-` for negatives.
8. **Approach A:** a dedicated `lilac-decimal` crate + a post-order AST fold
   pass run by the driver before resolution/typeck.

## 2. Architecture / components

```
crates/lilac-decimal/              # NEW crate (std-only)
  Cargo.toml                       # deps: lilac-ast, lilac-error, lilac-span
  src/lib.rs                       # engine (eval, Policy, DecimalError) + fold()

crates/lilac-ast/src/lib.rs        # + MutVisitor trait and walk_*_mut
crates/lilac-typeck/src/check.rs   # is_lit_literal delegates to lilac-decimal
crates/lilac-driver/src/lib.rs     # + Session.decimal; fold() after parse
crates/lilac-cli/src/main.rs       # + --decimal-scale / --decimal-round

docs/errors/E060.md                # + decimal divide/modulo-by-zero examples
SPEC.md                            # §2 decimal folding + flags
```

Data flow:

```
source -> lex -> parse -> Program
       -> lilac_decimal::fold(&mut program, policy, diags)   [rewrites AST]
       -> resolve -> collect_sigs -> check_bodies -> lower -> ...
```

The fold is purely syntactic and runs before name resolution, so the LSP (which
compiles through `Session`) inherits it with no change.

## 3. `lilac-decimal` engine

### 3.1 Value model

A value is `sign × digits / 10^scale`, where `sign ∈ {+, -}`, `digits` is a
non-negative arbitrary-precision integer (hand-rolled base-10 digit vector),
and `scale` is a `u32` fractional-digit count. Values are always normalized
(section 3.4).

Parsing an input literal string:

- Strip `_` separators (the lexer keeps them in `Expr::Number`'s text).
- Accept an optional leading `-` (produced by a prior fold; source literals are
  unsigned).
- Accept `digits`, `digits.digits`, with the integer part possibly empty only
  after a sign (not produced today); reject any non-digit character.
- Normalize.

### 3.2 Public API

```rust
pub enum Op { Add, Sub, Mul, Div, Mod }

pub enum Round { Trunc, HalfUp }

#[derive(Clone, Copy, Debug)]
pub struct Policy { pub scale: u32, pub round: Round }

pub enum DecimalError { DivideByZero, ModuloByZero, Malformed }

/// Evaluates `lhs <op> rhs` exactly and renders the canonical result string.
pub fn eval(op: Op, lhs: &str, rhs: &str, policy: Policy) -> Result<String, DecimalError>;

/// True iff `text` is a decimal literal (not `0x`/`0X`/`0b`/`0B` prefixed).
pub fn is_decimal_literal(text: &str) -> bool;
```

`is_decimal_literal` is the single source of truth for the decimal-vs-hex/bin
split; `lilac-typeck`'s `is_lit_literal` is redefined as
`!lilac_decimal::is_decimal_literal` so the two cannot drift. `lilac-typeck`
gains a `lilac-decimal` dependency.

### 3.3 Semantics

- **Add / Sub:** align fractional scales, signed big-integer add/sub, result
  scale = `max(scale_lhs, scale_rhs)`, normalize.
- **Mul:** unsigned big-integer product, scale = `scale_lhs + scale_rhs`, sign
  = XOR of the operand signs, normalize.
- **Div:** compute the magnitude quotient to `policy.scale` fractional digits.
  `Trunc` takes the magnitude floor (truncation toward zero); `HalfUp` inspects
  the first dropped digit/remainder and rounds the magnitude up at `≥ 0.5`
  (i.e. half away from zero). Sign = XOR. Normalize. `rhs == 0` →
  `DecimalError::DivideByZero`.
- **Mod:** floored — `q = floor(lhs / rhs)` as an exact big integer,
  `r = lhs - rhs·q`; result sign follows the divisor, `|r| < |rhs|`. Exact, no
  scale. `rhs == 0` → `DecimalError::ModuloByZero`.

`scale = 0` is legal (integer division/modulo).

`DecimalError::Malformed` is defensive: source operands are lexer-validated and
`is_decimal_literal` re-checks the grammar, so a `Malformed` result is
unreachable in the fold pass (which leaves the node unchanged). It exists so
the public `eval` never panics on arbitrary strings.

### 3.4 Canonical rendering

Given sign, `digits`, and `scale`:

1. Strip leading zeros from the integer part (empty → `0`).
2. If `scale > 0`, pad the fraction to `scale` digits; strip trailing zeros.
3. Emit `-` only when the value is negative and non-zero; emit `0` for zero.
4. Drop the `.` when the (post-strip) fraction is empty.

Examples: `2.50 → 2.5`, `6.0 → 6`, `007 → 7`, `2 - 3 → -1`, `0 - 0 → 0`,
`1 / 2 → 0.5`, `1 / 3 → 0.3333333333`, `6 / 3 → 2`.

## 4. AST mutable visitor + fold pass

### 4.1 `lilac-ast`: `MutVisitor`

Add a `MutVisitor` trait mirroring the existing immutable `Visitor`
(`crates/lilac-ast/src/lib.rs:460`), with `visit_program_mut`/`visit_item_mut`/
`visit_stmt_mut`/`visit_expr_mut` and matching `walk_*_mut` functions covering
every `Expr`, `Stmt`, `ItemKind`, `FStrPart`, `StructField`, and `MatchArm`
shape. Purely additive; no existing behavior changes.

### 4.2 `lilac_decimal::fold`

```rust
pub fn fold(program: &mut Program, policy: Policy, diags: &mut Diags);
```

Implements `MutVisitor`. `visit_expr_mut` calls `walk_expr_mut` first
(**post-order**, so inner folds have already become `Number` nodes), then:

- If the node is `Expr::BinOp { lhs, op, rhs, span }` with `op ∈
  {Add, Sub, Mul, Div, Mod}` and both `lhs`/`rhs` are `Expr::Number(text, _)`
  with `is_decimal_literal(text)`, map `op → Op` and call
  `eval(op, lhs_text, rhs_text, policy)`:
  - `Ok(folded)` → replace the node with `Expr::Number(folded, *span)`.
  - `Err(DecimalError::DivideByZero)` → emit `E060` "division by zero" at
    `*span`, leave the node unchanged.
  - `Err(DecimalError::ModuloByZero)` → emit `E060` "modulo by zero" at
    `*span`, leave the node unchanged.
- Otherwise do nothing.

Every other node (comparisons, `==`, `and`/`or`, hex/bin literals, non-literal
operands) is left untouched. Because the pass is post-order, nesting
(`2 * 3 + 4`) folds innermost-first.

## 5. Driver + CLI plumbing

### 5.1 Driver

- `Session` gains `pub decimal: lilac_decimal::Policy`, default
  `Policy { scale: 10, round: Round::Trunc }`.
- Fold immediately after `parse_program`, before resolution:
  - `compile_project_with_stages` (`crates/lilac-driver/src/lib.rs:218`): fold
    each per-file `program` inside the loop, before merging into `merged`, so
    `programs` (for `collect_project`) and `merged` (for typeck) agree.
  - `run_pipeline` (`crates/lilac-driver/src/lib.rs:259`): fold the single
    `program` after parse.
- The driver re-exports `Policy`/`Round` (e.g. as `DecimalPolicy`/`DecimalRound`)
  so `lilac-cli` need not depend on `lilac-decimal` directly.

### 5.2 CLI

`parse_common` (`crates/lilac-cli/src/main.rs:73`) accepts:

- `--decimal-scale=N` — `N` a `u32` in `0..=1000`; out-of-range or non-numeric
  is a usage error (exit 2).
- `--decimal-round=trunc|half-up` — anything else is a usage error.

Both follow the existing last-wins rule and are added to the `usage()` string
for the compiling subcommands (`check`, `emit-ir`, `emit-llvm`, `emit-vm`,
`run`).

## 6. Diagnostics & docs

- Reuse **E060** ("literal value error") for decimal division/modulo by zero;
  add good/bad examples to `docs/errors/E060.md`.
- Update `SPEC.md` §2 to document decimal literal folding, the canonical form,
  the `/` scale/rounding defaults, floored `%`, and the two flags.

## 7. Testing

**Engine (`lilac-decimal` unit tests)**

- `+ - *` exact, including carries and `_` separators (`10_000 + 1`).
- `/` at scale 0, 10, and a custom scale; truncate vs half-up; exact divisions
  render without trailing zeros (`6 / 3 → 2`, `1 / 2 → 0.5`).
- Negatives: `2 - 3 → -1`, `-2 * 3 → -6`, `-1 / 3` truncation direction.
- Floored `%` with positive and negative divisors (`-7 % 3 = 2`, `7 % -3 = -2`).
- Canonical rendering table (section 3.4).
- `1 / 0` → `DivideByZero`; `1 % 0` → `ModuloByZero`.
- `is_decimal_literal` accepts `5`/`10.5`, rejects `0x1`/`0b1`.

**Fold pass (`lilac-decimal`)**

- Nested: `2 * 3 + 4 → 10`.
- Non-both-decimal untouched: `a + 2` (variable), `2 + 0x3` (mixed).
- Comparisons untouched: `2 < 3` stays a `BinOp`.
- Span preservation: the folded `Number` carries the binop span.

**Driver / CLI**

- Default scale/round pinned (a `.lilac` with `1 / 3` lowers to the expected
  `NumLit` data const).
- `--decimal-scale=3` and `--decimal-round=half-up` change output.
- Invalid `--decimal-scale=abc` / `--decimal-scale=5000` /
  `--decimal-round=up` → exit 2.
- `1 / 0` and `1 % 0` produce `E060` through the driver.
- End-to-end: a folded literal lowers to `LirDataKind::NumLit` and an
  `emit-ir` snapshot is pinned.

## 8. Gates

Per change:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo clippy --release -p lilac-vm --all-targets --features bench-baseline -- -D warnings
cargo test --release -p lilac-vm --features bench-baseline
```

## 9. Flags raised for review (defaults chosen)

1. `Round::HalfUp` is defined as **half away from zero** (symmetric), not half
   toward `+∞`.
2. `--decimal-scale` is capped at **1000** to bound allocation.
