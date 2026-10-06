# Decimal Literal Folding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fold `+ - * / %` when both operands are decimal number literals, keeping the result a decimal literal, via a new exact-decimal engine and an AST fold pass.

**Architecture:** New std-only crate `lilac-decimal` holds arbitrary-precision decimal-string arithmetic and a post-order AST fold pass. `lilac-ast` gains a mutable visitor mirror. The driver runs the fold right after parsing; the CLI exposes scale/rounding flags.

**Tech Stack:** Rust 2021, std only, workspace lints (`unsafe_code = "forbid"`, clippy `all` + `pedantic` deny).

**Spec:** `docs/superpowers/specs/2026-10-07-lilac-decimal-literal-folding-design.md`

## Global Constraints

- std-only; no `serde`; no `unsafe`; `clippy::pedantic` deny; no panics on user input.
- Operators folded: `+ - * / %` only; **both** operands must be decimal literals.
- Result stays a decimal literal (`@byte_buffer`); no bit representation inferred.
- Defaults: `/` scale **10**, rounding **truncate toward zero**; `--decimal-scale` capped at **1000**; `HalfUp` = half **away from zero**.
- Canonical result: strip leading zeros; strip trailing fractional zeros; drop `.` when the fraction is empty; zero renders `0` (never `-0`); one leading `-` for negatives.
- `%` is **floored**: `a % b = a - b·floor(a/b)`, sign of the divisor, exact.
- `x / 0` and `x % 0` are `E060` compile errors.
- Non-(decimal, decimal) operator behavior is unchanged; no existing snapshot changes.
- Before every commit run the workspace gates: `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
  The release VM gates run in the final task.

## Review Focus

Inputs/conditions the spec implies but no single happy-path test covers; each gets a test in the owning task.

1. **Huge literals** (hundreds of digits) must stay exact with no overflow/panic — Task 1 & Task 3.
2. **Negative truncation direction**: `-1 / 3` truncates toward zero to `0`, not `-1` — Task 3.
3. **`_` separators and leading zeros** in operands (`10_000 + 1`, `007`) — Task 2 & Task 3.
4. **Decimal literals nested in control flow / f-string holes / struct literals / match arms** must all be reached by the mutable walk — Task 4 & Task 5.
5. **Floored `%` with a negative divisor** (`7 % -3 = -2`) — Task 3.

---

### Task 1: `lilac-decimal` crate scaffold and big-integer core

**Files:**
- Create: `crates/lilac-decimal/Cargo.toml`
- Create: `crates/lilac-decimal/src/lib.rs`
- Create: `crates/lilac-decimal/src/bigint.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: crate-private `bigint::Big` — an arbitrary-precision non-negative integer, base `1_000_000_000` limbs, little-endian, no trailing zero limbs.

- [ ] **Step 1: Write the failing tests** in `crates/lilac-decimal/src/bigint.rs`

```rust
#[cfg(test)]
mod tests {
    use super::Big;

    fn b(s: &str) -> Big {
        Big::from_decimal_digits(s).expect("valid digits")
    }

    #[test]
    fn from_and_to_decimal_digits_round_trip() {
        let digits = "1234567890".repeat(20); // 200 digits
        assert_eq!(b(&digits).to_decimal_string(), digits);
    }

    #[test]
    fn add_carries_across_limbs() {
        assert_eq!(b("999999999").add(&b("1")).to_decimal_string(), "1000000000");
    }

    #[test]
    fn sub_borrows() {
        assert_eq!(b("1000000000").sub(&b("1")).to_decimal_string(), "999999999");
    }

    #[test]
    fn mul_exact() {
        assert_eq!(b("123456789").mul(&b("987654321")).to_decimal_string(), "121932631112635269");
    }

    #[test]
    fn mul_pow10_shifts() {
        assert_eq!(b("5").mul_pow10(3).to_decimal_string(), "5000");
    }

    #[test]
    fn divmod_round_trip() {
        let (q, r) = b("1000000007").divmod(&b("99991")).expect("nonzero divisor");
        // q*d + r == n
        assert_eq!(q.mul(&b("99991")).add(&r).to_decimal_string(), "1000000007");
    }

    #[test]
    fn cmp_orders() {
        assert_eq!(b("10").cmp(&b("9")), std::cmp::Ordering::Greater);
        assert_eq!(b("9").cmp(&b("10")), std::cmp::Ordering::Less);
        assert_eq!(b("007").cmp(&b("7")), std::cmp::Ordering::Equal);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lilac-decimal`
Expected: FAIL — the crate/methods do not exist.

- [ ] **Step 3: Implement the crate and `Big`**

`Cargo.toml`:

```toml
[package]
name = "lilac-decimal"
version.workspace = true
edition.workspace = true

[dependencies]
lilac-ast = { path = "../lilac-ast" }
lilac-error = { path = "../lilac-error" }
lilac-span = { path = "../lilac-span" }

[dev-dependencies]
lilac-lex = { path = "../lilac-lex" }
lilac-parse = { path = "../lilac-parse" }

[lints]
workspace = true
```

`src/lib.rs` (this task only):

```rust
//! Exact decimal-literal folding: arbitrary-precision arithmetic on the
//! literal strings of `+ - * / %` when both operands are decimal number
//! literals (SPEC §2).
mod bigint;
```

`src/bigint.rs` implements:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Big { limbs: Vec<u32> } // base 1e9, little-endian

impl Big {
    pub(crate) fn zero() -> Big;
    pub(crate) fn from_decimal_digits(digits: &str) -> Option<Big>; // digits only, no sign; None on non-digit
    pub(crate) fn to_decimal_string(&self) -> String;
    pub(crate) fn is_zero(&self) -> bool;
    pub(crate) fn cmp(&self, other: &Big) -> std::cmp::Ordering;
    pub(crate) fn add(&self, other: &Big) -> Big;
    pub(crate) fn sub(&self, other: &Big) -> Big; // callers guarantee self >= other
    pub(crate) fn mul(&self, other: &Big) -> Big;
    pub(crate) fn mul_pow10(&self, n: u32) -> Big;
    pub(crate) fn divmod(&self, divisor: &Big) -> Option<(Big, Big)>; // None if divisor is zero
}
```

Notes: `from_decimal_digits` strips nothing (caller passes clean digits); group the
decimal string into 9-digit limbs from the right. `sub` uses a
`debug_assert!(self.cmp(other) != Ordering::Less)` and borrows. `divmod` is
schoolbook long division over limbs; `mul_pow10` shifts whole limbs by `n/9`
and multiplies the remainder limb by `10^(n%9)` (may need a carry limb).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p lilac-decimal`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-decimal
git commit -m "feat(decimal): add lilac-decimal crate and big-integer core"
```

---

### Task 2: Decimal value model, canonical rendering, literal predicate

**Files:**
- Create: `crates/lilac-decimal/src/value.rs`
- Modify: `crates/lilac-decimal/src/lib.rs`

**Interfaces:**
- Consumes: `bigint::Big`.
- Produces:
  - `pub enum Op { Add, Sub, Mul, Div, Mod }` (derive `Clone, Copy, Debug, PartialEq, Eq`)
  - `pub enum Round { Trunc, HalfUp }` (derive `Clone, Copy, Debug, PartialEq, Eq`)
  - `pub struct Policy { pub scale: u32, pub round: Round }` (derive `Clone, Copy, Debug, PartialEq, Eq`)
  - `pub enum DecimalError { DivideByZero, ModuloByZero, Malformed }` (derive `Clone, Copy, Debug, PartialEq, Eq`)
  - `pub fn is_decimal_literal(text: &str) -> bool`
  - crate-private `value::Decimal { negative: bool, digits: Big, scale: u32 }` with `parse(&str) -> Option<Decimal>` and `render(&self) -> String`.

- [ ] **Step 1: Write the failing tests** in `crates/lilac-decimal/src/value.rs` and `src/lib.rs`

```rust
// value.rs tests
#[test]
fn render_canonical() {
    for (input, want) in [
        ("2.50", "2.5"), ("6.0", "6"), ("007", "7"),
        ("-0", "0"), ("0.500", "0.5"), ("0.0", "0"),
    ] {
        assert_eq!(Decimal::parse(input).expect("parse").render(), want, "{input}");
    }
}

#[test]
fn parse_strips_underscores() {
    assert_eq!(Decimal::parse("1_0.0_1").expect("parse").render(), "10.01");
}

#[test]
fn parse_render_round_trips_huge_literal() {
    let digits = "9".repeat(200);
    assert_eq!(Decimal::parse(&digits).expect("parse").render(), digits);
}
```

```rust
// lib.rs tests
#[test]
fn is_decimal_literal_accepts_decimals() {
    for t in ["5", "10_000", "10.5", "-1", "0.5"] {
        assert!(is_decimal_literal(t), "{t}");
    }
}

#[test]
fn is_decimal_literal_rejects_hex_and_bin() {
    for t in ["0x1", "0X1", "0b1", "0B1"] {
        assert!(!is_decimal_literal(t), "{t}");
    }
}

#[test]
fn is_decimal_literal_rejects_malformed() {
    for t in ["", "-", "1.2.3", "1..2", "1.", ".5", "1a"] {
        assert!(!is_decimal_literal(t), "{t}");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lilac-decimal`
Expected: FAIL — `value`/`is_decimal_literal` missing.

- [ ] **Step 3: Implement the value model and predicate**

`is_decimal_literal(text)`: reject if it starts with `0x`/`0X`/`0b`/`0B`;
otherwise require the grammar `-?[0-9][0-9_]*(\.[0-9][0-9_]*)?` (accepting a
leading `-` for folded results). Empty or any other shape → `false`.

`Decimal::parse`: strip `_`; read an optional leading `-`; split on `.` into an
integer and optional fraction; every remaining char must be an ASCII digit;
build `digits = Big::from_decimal_digits(int ++ frac)` and `scale = frac.len()`;
normalize (strip trailing fraction zeros by reducing `scale` and dividing
`digits` by `10^(removed)` via `mul_pow10`/`divmod`, drop sign when zero).

`Decimal::render`: emit `-` when `negative && !digits.is_zero()`; then
`digits.to_decimal_string()`; if `scale > 0`, left-pad with zeros to
`scale + 1` chars, insert `.` before the last `scale`, then strip trailing
zeros and a trailing `.`.

Add to `src/lib.rs`:

```rust
mod bigint;
mod value;

pub enum Op { Add, Sub, Mul, Div, Mod }
pub enum Round { Trunc, HalfUp }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy { pub scale: u32, pub round: Round }
pub enum DecimalError { DivideByZero, ModuloByZero, Malformed }

pub fn is_decimal_literal(text: &str) -> bool { /* as above */ }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p lilac-decimal`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-decimal
git commit -m "feat(decimal): add value model, canonical rendering, literal predicate"
```

---

### Task 3: Arithmetic `eval`

**Files:**
- Modify: `crates/lilac-decimal/src/value.rs`
- Modify: `crates/lilac-decimal/src/lib.rs`

**Interfaces:**
- Consumes: Task 2 types.
- Produces: `pub fn eval(op: Op, lhs: &str, rhs: &str, policy: Policy) -> Result<String, DecimalError>` (re-exported from `value`).

- [ ] **Step 1: Write the failing tests** in `crates/lilac-decimal/src/value.rs`

```rust
fn ev(op: Op, l: &str, r: &str, scale: u32, round: Round) -> String {
    eval(op, l, r, Policy { scale, round }).expect("eval ok")
}
fn trunc(scale: u32) -> Policy { Policy { scale, round: Round::Trunc } }

#[test]
fn add_exact() { assert_eq!(ev(Op::Add, "1.5", "2.25", 10, Round::Trunc), "3.75"); }

#[test]
fn sub_yields_negative() { assert_eq!(ev(Op::Sub, "2", "3", 10, Round::Trunc), "-1"); }

#[test]
fn mul_exact() { assert_eq!(ev(Op::Mul, "10.5", "2", 10, Round::Trunc), "21"); }

#[test]
fn div_trunc_default_scale() { assert_eq!(ev(Op::Div, "1", "3", 10, Round::Trunc), "0.3333333333"); }

#[test]
fn div_exact_has_no_trailing_zeros() {
    assert_eq!(ev(Op::Div, "6", "3", 10, Round::Trunc), "2");
    assert_eq!(ev(Op::Div, "1", "2", 10, Round::Trunc), "0.5");
}

#[test]
fn div_trunc_toward_zero_for_negatives() { assert_eq!(ev(Op::Div, "-1", "3", 10, Round::Trunc), "0"); }

#[test]
fn div_half_up_away_from_zero() {
    assert_eq!(ev(Op::Div, "1", "8", 2, Round::HalfUp), "0.13");
    assert_eq!(ev(Op::Div, "-1", "8", 2, Round::HalfUp), "-0.13");
    assert_eq!(ev(Op::Div, "1", "3", 2, Round::HalfUp), "0.33");
}

#[test]
fn div_scale_zero() { assert_eq!(ev(Op::Div, "7", "2", 0, Round::Trunc), "3"); }

#[test]
fn mod_floored_positive_divisor() { assert_eq!(ev(Op::Mod, "-7", "3", 10, Round::Trunc), "2"); }

#[test]
fn mod_floored_negative_divisor() { assert_eq!(ev(Op::Mod, "7", "-3", 10, Round::Trunc), "-2"); }

#[test]
fn divide_by_zero_is_error() {
    assert_eq!(eval(Op::Div, "1", "0", trunc(10)), Err(DecimalError::DivideByZero));
}

#[test]
fn modulo_by_zero_is_error() {
    assert_eq!(eval(Op::Mod, "1", "0", trunc(10)), Err(DecimalError::ModuloByZero));
}

#[test]
fn large_operands_are_exact() {
    // (10^100 - 1)^2 = 99 nines, an 8, 99 zeros, then a 1.
    let big = "9".repeat(100);
    let want = format!("{}8{}1", "9".repeat(99), "0".repeat(99));
    assert_eq!(ev(Op::Mul, &big, &big, 10, Round::Trunc), want);
}

#[test]
fn underscored_operands() { assert_eq!(ev(Op::Add, "10_000", "1", 10, Round::Trunc), "10001"); }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lilac-decimal eval`
Expected: FAIL — `eval` missing.

- [ ] **Step 3: Implement `eval`**

Parse both operands (`None` → `Err(DecimalError::Malformed)`).

- **Add/Sub:** bring both to `scale = max(a.scale, b.scale)` with `mul_pow10`;
  for `Add`, if signs differ compare magnitudes and subtract the smaller from
  the larger; `Sub` is `Add` with `b.negative` flipped; set the result sign.
- **Mul:** `digits = a.digits.mul(&b.digits)`, `scale = a.scale + b.scale`,
  `negative = a.negative ^ b.negative`.
- **Div:** `rhs` zero → `DivideByZero`. Compute the magnitude quotient to
  `policy.scale` digits: `q = floor( (a.digits · 10^(b.scale + scale)) /
  (b.digits · 10^a.scale) )`, plus the remainder for rounding. For `HalfUp`,
  if `2·remainder >= divisor` (i.e. the next digit ≥ 5) increment `q` by one.
  `negative = a.negative ^ b.negative`. `scale = policy.scale`.
- **Mod:** `rhs` zero → `ModuloByZero`. `q = floor(a / b)` as an exact integer
  (floored division: truncate the magnitude quotient, then decrement when the
  signs differ and the division is not exact); `r = a - b·q` (exact, same
  scale as the operands' max). Result sign follows `b`.

Render the result with `Decimal::render`.

Re-export in `src/lib.rs`: `pub use value::eval;`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p lilac-decimal`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-decimal
git commit -m "feat(decimal): add exact arithmetic eval with div scale and floored mod"
```

---

### Task 4: AST `MutVisitor` and `walk_*_mut`

**Files:**
- Modify: `crates/lilac-ast/src/lib.rs`

**Interfaces:**
- Consumes: the existing `Visitor`/`walk_*` as the mirror template.
- Produces: `pub trait MutVisitor` with `visit_program_mut`/`visit_item_mut`/`visit_stmt_mut`/`visit_expr_mut` (default to the walks), and `pub fn walk_program_mut`/`walk_item_mut`/`walk_stmt_mut`/`walk_expr_mut`.

- [ ] **Step 1: Write the failing test** in the `tests` module of `crates/lilac-ast/src/lib.rs`

```rust
struct NumberRename;
impl MutVisitor for NumberRename {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        walk_expr_mut(self, expr);
        if let Expr::Number(text, _) = expr {
            if text == "0x1" {
                *text = "0x9".to_owned();
            }
        }
    }
}

#[test]
fn mut_visitor_rewrites_nested_exprs() {
    let span = Span::dummy();
    let mut expr = Expr::BinOp {
        lhs: Box::new(Expr::Number("0x1".to_owned(), span)),
        op: BinOp::Add,
        rhs: Box::new(Expr::Number("0x2".to_owned(), span)),
        span,
    };
    NumberRename.visit_expr_mut(&mut expr);
    let Expr::BinOp { lhs, .. } = expr else { panic!("still a binop") };
    assert!(matches!(*lhs, Expr::Number(ref t, _) if t == "0x9"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-ast mut_visitor_rewrites_nested_exprs`
Expected: FAIL — `MutVisitor` undefined.

- [ ] **Step 3: Implement `MutVisitor` + `walk_*_mut`**

Mirror the immutable `Visitor`/`walk_*` exactly, taking `&mut` and covering every
variant: `Program.items`; `ItemKind::{Func (FuncBody::Block/Inline), Object,
Private, Const}`; `Stmt::{Let, MultiLet, Assign, ExprStmt, If, Loop, Match}`;
`Expr::{Call (callee + args), Member, BinOp, IfValue, MatchValue, StructLit,
FString (Hole), RefOf}`; `MatchArmBody::{Block, Inline}`; `StructField.value`;
`FStrPart::Hole`. Enum variants and type positions carry no expressions.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lilac-ast`
Expected: PASS (existing `visitor_walks_nested_exprs` still green).

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-ast/src/lib.rs
git commit -m "feat(ast): add MutVisitor and walk_*_mut"
```

---

### Task 5: Fold pass and driver wiring

**Files:**
- Create: `crates/lilac-decimal/src/fold.rs`
- Modify: `crates/lilac-decimal/src/lib.rs`
- Modify: `crates/lilac-driver/Cargo.toml`
- Modify: `crates/lilac-driver/src/lib.rs`

**Interfaces:**
- Consumes: `value::eval`, `Op`, `Policy`, `DecimalError`, `is_decimal_literal`; `lilac_ast::{MutVisitor, walk_expr_mut, BinOp, Expr, Program}`; `lilac_error::{Diagnostic, Diags}`.
- Produces: `pub fn fold(program: &mut Program, policy: Policy, diags: &mut Diags)`; `Session.decimal: lilac_decimal::Policy`; driver re-exports `pub use lilac_decimal::{Policy as DecimalPolicy, Round as DecimalRound};`.

- [ ] **Step 1: Write the failing tests**

`crates/lilac-decimal/src/fold.rs`:

```rust
use super::*;
use lilac_ast::{walk_expr, walk_program, Expr, Program, Visitor};
use lilac_error::Diags;
use lilac_lex::Lexer;
use lilac_parse::Parser;
use lilac_span::SourceMap;

fn fold_src(src: &str) -> (Program, Diags) {
    let mut sm = SourceMap::new();
    let file = sm.add_file("t.lilac", src);
    let (tokens, _) = Lexer::lex_all(src, file);
    let mut diags = Diags::new();
    let mut parser = Parser::new(tokens, file);
    let mut program = parser.parse_program(&mut diags).expect("parses");
    fold(&mut program, Policy { scale: 10, round: Round::Trunc }, &mut diags);
    (program, diags)
}

struct Numbers(Vec<String>);
impl Visitor for Numbers {
    fn visit_expr(&mut self, expr: &Expr) {
        if let Expr::Number(t, _) = expr { self.0.push(t.clone()); }
        walk_expr(self, expr);
    }
}
fn numbers(p: &Program) -> Vec<String> {
    let mut n = Numbers(Vec::new());
    walk_program(&mut n, p);
    n.0
}

#[test]
fn folds_nested_literal_expression() {
    let (p, d) = fold_src("main: function () nothing {\n    x: @byte_buffer = 2 * 3 + 4\n}\n");
    assert!(!d.has_errors());
    assert_eq!(numbers(&p), vec!["10".to_owned()]);
}

#[test]
fn leaves_non_decimal_operands() {
    let (p, _) = fold_src("main: function () nothing {\n    a: @byte_buffer = 2\n    x: @byte_buffer = a + 2\n}\n");
    assert_eq!(numbers(&p), vec!["2".to_owned(), "2".to_owned()]); // no folded "4"
    let (p, _) = fold_src("main: function () nothing {\n    x: @byte_buffer = 2 + 0x3\n}\n");
    assert!(!numbers(&p).contains(&"5".to_owned()));
}

#[test]
fn leaves_comparisons() {
    let (p, _) = fold_src("main: function () nothing {\n    x: @bit8 = 2 < 3\n}\n");
    assert_eq!(numbers(&p), vec!["2".to_owned(), "3".to_owned()]);
}

#[test]
fn folds_inside_control_flow_and_fstring() {
    let (p, _) = fold_src("main: function () nothing {\n    x: @byte_buffer = $\"{2 * 3}\"\n}\n");
    assert_eq!(numbers(&p), vec!["6".to_owned()]);
}

#[test]
fn division_by_zero_reports_e060() {
    let (_, d) = fold_src("main: function () nothing {\n    x: @byte_buffer = 1 / 0\n}\n");
    assert!(d.diagnostics().iter().any(|e| e.code == "E060"));
}
```

`crates/lilac-driver/src/lib.rs` tests:

```rust
#[test]
fn session_decimal_defaults() {
    assert_eq!(Session::new().decimal, lilac_decimal::Policy { scale: 10, round: lilac_decimal::Round::Trunc });
}

#[test]
fn folded_decimal_literal_lowers_to_numlit() {
    let mut s = Session::new();
    let lir = s.compile_lir("d.lilac", "main: function () nothing {\n    x: @byte_buffer = 2 * 3\n}\n")
        .expect("compiles");
    assert!(lir.data.iter().any(|d| matches!(&d.kind, lilac_ir::LirDataKind::NumLit(t) if t == "6")));
}

#[test]
fn division_by_zero_is_e060() {
    let mut s = Session::new();
    assert!(s.compile_lir("d.lilac", "main: function () nothing {\n    x: @byte_buffer = 1 / 0\n}\n").is_err());
    assert!(s.render_diagnostics().contains("E060"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lilac-decimal -p lilac-driver`
Expected: FAIL — `fold`/`Session.decimal` missing.

- [ ] **Step 3: Implement the fold and wiring**

`fold.rs`:

```rust
struct Folder<'a> { policy: Policy, diags: &'a mut Diags }

impl MutVisitor for Folder<'_> {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        walk_expr_mut(self, expr); // post-order
        let Expr::BinOp { lhs, op, rhs, span } = expr else { return };
        let Some(op) = op_of(*op) else { return };
        let (Expr::Number(l, _), Expr::Number(r, _)) = (&**lhs, &**rhs) else { return };
        if !is_decimal_literal(l) || !is_decimal_literal(r) { return; }
        match eval(op, l, r, self.policy) {
            Ok(s) => *expr = Expr::Number(s, *span),
            Err(DecimalError::DivideByZero) => { self.diags.emit(Diagnostic::error("E060", "division by zero").label(*span, true, "in decimal literal")); }
            Err(DecimalError::ModuloByZero) => { self.diags.emit(Diagnostic::error("E060", "modulo by zero").label(*span, true, "in decimal literal")); }
            Err(DecimalError::Malformed) => {} // unreachable; leave node
        }
    }
}

fn op_of(op: BinOp) -> Option<Op> { /* Add/Sub/Mul/Div/Mod → Some; else None */ }

pub fn fold(program: &mut Program, policy: Policy, diags: &mut Diags) {
    let mut folder = Folder { policy, diags };
    folder.visit_program_mut(program);
}
```

`src/lib.rs`: add `mod fold; pub use fold::fold;`.

`crates/lilac-driver/Cargo.toml`: add `lilac-decimal = { path = "../lilac-decimal" }`.

`crates/lilac-driver/src/lib.rs`: add `pub decimal: lilac_decimal::Policy` to
`Session` (initialize `Policy { scale: 10, round: Round::Trunc }` in
`Session::new()`), add the re-export, and call
`lilac_decimal::fold(&mut program, self.decimal, &mut self.diags);`
immediately after each `parse_program` — in `compile_project_with_stages`
(inside the per-file loop, before merging into `merged`) and in `run_pipeline`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p lilac-decimal -p lilac-driver`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-decimal crates/lilac-driver
git commit -m "feat(decimal): fold decimal literals in the driver after parse"
```

---

### Task 6: `lilac-typeck` single-source literal predicate

**Files:**
- Modify: `crates/lilac-typeck/Cargo.toml`
- Modify: `crates/lilac-typeck/src/check.rs:610-617` (`is_lit_literal`)

**Interfaces:**
- Consumes: `lilac_decimal::is_decimal_literal`.
- Produces: `is_lit_literal(bytes) == !lilac_decimal::is_decimal_literal(bytes)`.

- [ ] **Step 1: Write the failing test** in `crates/lilac-typeck/src/check.rs` `tests`

```rust
#[test]
fn is_lit_literal_handles_uppercase_prefixes() {
    assert!(is_lit_literal("0X1"));
    assert!(is_lit_literal("0B1"));
    assert!(!is_lit_literal("5"));
    assert!(!is_lit_literal("10.5"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lilac-typeck is_lit_literal_handles_uppercase_prefixes`
Expected: FAIL — `lilac_decimal` not a dependency (compile error), or the delegation is absent.

- [ ] **Step 3: Implement the delegation**

Add `lilac-decimal = { path = "../lilac-decimal" }` to `crates/lilac-typeck/Cargo.toml`.
Replace the body of `is_lit_literal`:

```rust
fn is_lit_literal(bytes: &str) -> bool {
    !lilac_decimal::is_decimal_literal(bytes)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p lilac-typeck`
Expected: PASS (all existing literal tests still green).

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-typeck
git commit -m "refactor(typeck): delegate is_lit_literal to lilac-decimal"
```

---

### Task 7: CLI flags `--decimal-scale` / `--decimal-round`

**Files:**
- Modify: `crates/lilac-cli/src/main.rs` (`parse_common`, `usage`, tests)

**Interfaces:**
- Consumes: `lilac_driver::{DecimalPolicy, DecimalRound}`.
- Produces: `--decimal-scale=N` (u32, `0..=1000`) and `--decimal-round=trunc|half-up`, stored in `session.decimal`.

- [ ] **Step 1: Write the failing tests** in `crates/lilac-cli/src/main.rs` `tests`

```rust
#[test]
fn decimal_flags_default() {
    let c = parse_common(&args(&["f.lilac"]), false).expect("valid");
    assert_eq!(c.session.decimal, DecimalPolicy { scale: 10, round: DecimalRound::Trunc });
}

#[test]
fn decimal_flags_parse() {
    let c = parse_common(&args(&["f.lilac", "--decimal-scale=3", "--decimal-round=half-up"]), false).expect("valid");
    assert_eq!(c.session.decimal, DecimalPolicy { scale: 3, round: DecimalRound::HalfUp });
}

#[test]
fn decimal_scale_rejects_out_of_range() {
    assert!(parse_common(&args(&["f.lilac", "--decimal-scale=1001"]), false).is_err());
    assert!(parse_common(&args(&["f.lilac", "--decimal-scale=abc"]), false).is_err());
}

#[test]
fn decimal_round_rejects_unknown() {
    assert!(parse_common(&args(&["f.lilac", "--decimal-round=up"]), false).is_err());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lilac-cli decimal`
Expected: FAIL — flags unknown.

- [ ] **Step 3: Implement the flags and update `usage()`**

In `parse_common`, add branches (mirroring `--mode=`): parse
`--decimal-scale=` as `u32`, reject `> 1000` or non-numeric with exit 2; parse
`--decimal-round=` as `trunc`/`half-up` (else exit 2); set `session.decimal`.
Add `[--decimal-scale=N] [--decimal-round=trunc|half-up]` to each compiling
subcommand line in `usage()`. Import `DecimalPolicy, DecimalRound` from
`lilac_driver`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p lilac-cli decimal`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lilac-cli/src/main.rs
git commit -m "feat(cli): add --decimal-scale and --decimal-round"
```

---

### Task 8: Docs, end-to-end checks, full gates

**Files:**
- Modify: `docs/errors/E060.md`
- Modify: `SPEC.md` (§2)
- Create: `crates/lilac-cli/tests/decimal.rs`

**Interfaces:**
- Consumes: the CLI from Task 7 and the fold from Task 5.
- Produces: documented behavior and end-to-end coverage.

- [ ] **Step 1: Write the end-to-end tests** in `crates/lilac-cli/tests/decimal.rs`

```rust
// Reuse the existing CLI test harness pattern (spawn the built binary or call
// the driver directly, matching sibling tests in this directory).
#[test]
fn default_scale_is_ten() { /* `1 / 3` in a checked source yields the NumLit "0.3333333333" */ }

#[test]
fn scale_flag_changes_output() { /* `--decimal-scale=3` yields "0.333" */ }

#[test]
fn half_up_flag_rounds() { /* `1 / 8` with `--decimal-round=half-up --decimal-scale=2` yields "0.13" */ }

#[test]
fn division_by_zero_fails_with_e060() { /* `1 / 0` exits non-zero and prints E060 */ }
```

Run: `cargo test -p lilac-cli --test decimal`
Expected: PASS (Tasks 1–7 built this); if any fail, fix the owning task.

- [ ] **Step 2: Document `E060`**

Add a decimal section to `docs/errors/E060.md` with a bad example
(`x: @byte_buffer = 1 / 0` → `error[E060]: division by zero`) and a note that
decimal modulo by zero reads `modulo by zero`.

- [ ] **Step 3: Document `SPEC.md` §2**

Record: `+ - * / %` fold when both operands are decimal literals; the canonical
result form; `/` default scale 10 truncate; floored `%`; `x / 0`/`x % 0` are
`E060`; and the `--decimal-scale` / `--decimal-round` flags.

- [ ] **Step 4: Run the full gates**

Run each; all must pass:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo clippy --release -p lilac-vm --all-targets --features bench-baseline -- -D warnings
cargo test --release -p lilac-vm --features bench-baseline
```

- [ ] **Step 5: Commit**

```bash
git add docs/errors/E060.md SPEC.md crates/lilac-cli/tests/decimal.rs
git commit -m "docs(decimal): document literal folding and E060; add end-to-end tests"
```
