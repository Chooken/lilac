# Lilac Rust Rewrite — M3 Design (strings, literals, const call-eval)

> Scope: M3 only (`RUST_REWRITE_PLAN.md` §7 M3 bullet; plan §9–§10). Normative
> language/ir docs: `SPEC.md` v0.2-draft, `IR_SPEC.md` v0.2. Prior records: M0
> design (`docs/superpowers/specs/2026-10-01-lilac-rust-rewrite-m0-design.md`),
> M1 plan (`docs/superpowers/plans/2026-10-01-m1-types.md`), M2 design
> (`docs/superpowers/specs/2026-10-02-lilac-rust-rewrite-m2-design.md`).
> Work stays on branch `rust-rewrite-m0`.

## 1. Outcome / constraints / success

- Outcome: `lilac check`/`emit-ir` typecheck and lower string literals,
  decimal number literals, and f-strings; `@as_int`/`@as_float` fold at compile
  time; the folder performs best-effort call evaluation of pure functions with
  bounded loops. New value error `E060` and piece-cap error `E061`.
- Constraints: (a) same gates as M0–M2 — `cargo fmt --check`,
  `clippy -D warnings`, `cargo test` green after every task; (b) frontend
  crates never import `inkwell`/`llvm-sys`; (c) no `unwrap`/`expect` on user
  input, `bug!()` for ICEs only; (d) std only, no new crates beyond the
  already-scaffolded ones; (e) TDD per task, one commit per task, an error doc
  for every new code.
- Success: §6 exit criteria all green; **no new surviving IR instruction
  shapes** (builtin operations reuse `call`, data addressing reuses `const`).

### 1.1 What is already in place (verified before design)

- Lexer emits `FStrStart/FStrMid/FStrEnd` with brace-depth tracking; parser
  builds `Expr::FString { parts: Vec<FStrPart> }`; `FStrPart::{Lit, Hole}`.
- `lilac-ty` pre-interns `@byte_buffer` and `@format_args` builtins.
- `lilac-typeck` already knows the `@as_int`, `@as_float`, `@byte_buffer_*`,
  `@format_args_*` intrinsics and their shapes; `@conversion` search and
  `convert_if_needed` exist from M1/M2.
- `lilac-ir` already has `LirDataKind::{Str, NumLit}`, `#byte_buffer`/
  `#format_args` in the verifier's builtin type list, and the frozen v2 op set.
- `lilac-const` has `ConstValue` (`Int`/`Bool`/`Unknown`), straight-line
  per-block peephole folding, constant-branch folding, dead-block pruning, and
  live-`@compile_error` `E031`.
- `inline` `=>` functions were fully substituted in typeck during M2 (inline
  functions never enter `HirProgram`), so M3 needs **no separate inline pass**.

## 2. Architecture (plan §3, M3 slice)

```text
crates/
  lilac-lex/      # unchanged (FStr tokens already exist)
  lilac-ast/      # unchanged (FString/FStrPart already exist)
  lilac-parse/    # unchanged (fstring parse already exists)
  lilac-hir/      # M3: no new node; f-strings desugar to existing call nodes
  lilac-typeck/   # M3: literal classification, @as_* fold-only typing,
                  #     f-string desugar, E024/E043/E061
  lilac-ty/       # unchanged (@byte_buffer/@format_args already interned)
  lilac-ir/       # M3: LirCallee::Builtin, builtin signature table, data-symbol
                  #     const operands, verify rejecting surviving fold-only
  lilac-const/    # M3: full call-eval (cross-block consts, fuel/depth), @as_*
                  #     folding, E060
  lilac-lower/    # M3: string/Number lowering, builtin-call emission,
                  #     fold-only markers; E030 retired for strings/fstrings
  lilac-driver/   # unchanged pipeline (lex -> ... -> fold -> verify)
  lilac-cli/      # unchanged (emit-ir prints to_text())
```

- f-string desugaring lives in **typeck** (the M2 `if`-value/`match`-value
  desugar pattern: extend the enclosing block and return a temp/ident), not in
  the parser or lower. This keeps expected-type conversion (and therefore
  hole `E043` diagnostics) in the checker. Plan §9.3's "lowers in
  `lilac-parse`" is superseded by this decision and the SPEC patches say so.
- The compiler's only f-string responsibility is to build the `@format_args`
  value. Conversion to `String`, streaming into handlers, allocation, and UTF-8
  validation are ordinary stdlib code (`@conversion` + builtins), never
  compiler special cases.

## 3. Components

### 3.1 Literal model (SPEC §2 amendment)

Two literal classes, both stored in the existing `Expr::Number(bytes, span)`
AST node and distinguished in typeck by inspecting `bytes` (no lexer/AST
churn):

- **LitLiteral**: `0x…`/`0X…`/`0b…`/`0B…`. The bit representation is fully
  known. Typed by the expected bit type, or the unified width supplied by
  sibling operands (e.g. `@iadd(0x1, 0x2)`); no expected/unified bit width →
  `E024` as today. Materialized by lower as a direct `const LIT:#bitN`. Once
  typeck has fixed the target width, a value wider than it → `E060` (checked in
  typeck, where the width is known, so the diagnosis precedes lowering).
- **NumberLiteral**: every decimal form (`5`, `1.5`, `2e3`). Its type is always
  `@byte_buffer` holding the source bytes. It is **not** implicitly converted
  to a bit type: `a: @bit8 = 5` is `E043` (a failed conversion search, matching
  the frozen `SPEC.md`); the spelling is
  `a: @bit8 = @as_int(5)`. It may be consumed by anything expecting
  `@byte_buffer` (including `@as_int`/`@as_float` and conversion hooks).

`@as_int`/`@as_float`:

- Typeck: exactly one argument, which must be `@byte_buffer`; the result width
  comes from the expected bit type (`E024` when absent).
- Lower: emitted as a **fold-only marker** (a builtin call `@as_int`/`@as_float`
  that the fold pass must consume).
- Fold: parses the constant bytes into `Int(value, width)` /
  `Float(value, width)`; bad digits, a value that does not fit the width, or a
  non-constant operand → `E060`. The runtime-parse form never reaches the
  backend (SPEC §2: "only a semantic model, never emitted code"). This
  supersedes `RUST_REWRITE_PLAN.md` §10.2's "keeps the runtime call".

### 3.2 Strings (compiler construction only)

- `"hi"` has type `@byte_buffer`. HIR keeps `HirExprKind::Str { value }`.
- Lower interns a `data` const (`!cN: str = "…"`, UTF-8, `"`→`\22`,
  `\`→`\5C`) and lowers the literal directly to a data-backed buffer value: a
  single `const !cN:#byte_buffer` register (the `lilac-ir` verifier resolves the
  symbol against `module.data`). The earlier illustrative
  `@byte_buffer_from(addr !str_N, len)` sequence is superseded —
  `@byte_buffer_from` remains a builtin for user code but is not used by
  literal lowering.
- A decimal NumberLiteral lowers the same way with a `numlit` data const
  (`!cN: numlit = "5"`) plus `const !cN:#byte_buffer` instead of `str`; hex/bin
  LitLiterals stay a direct `const LIT:#bitN`.
- Data consts are deduplicated by payload so identical literals share one
  `!cN`; names are assigned in first-use order for deterministic snapshots.

### 3.3 F-strings (desugar in typeck)

`$"Hello {name}!"` checks as an ordinary expression with this desugar (the
same machinery M2 uses for `if`-values and `match`-values):

```lilac
__f: @format_args = @format_args_new()
@format_args_push(ref __f, "Hello ")     // literal chunk -> @byte_buffer view
@format_args_push(ref __f, @convert(name)) // hole -> @conversion(T -> @byte_buffer)
@format_args_push(ref __f, "!")
// result expression: __f, typed @format_args
```

- Literal chunks reuse the §3.2 string path.
- Each hole is checked against `@byte_buffer` via `convert_if_needed`; a
  missing conversion is the ordinary `E043` at the hole span.
- `{{`/`}}` are resolved to `{`/`}` by the lexer before typeck.
- More than 16 pieces (chunks + holes) → `E061` at the f-string span.
- The result is `@format_args`. Consumption is **ordinary expected-type
  conversion** (`@conversion(from: @format_args)`); no f-string-specific
  dispatch. Streaming into `append`-shaped handlers is stdlib code and is out
  of M3 scope (it needs generic `append[T]` from M4).

### 3.4 LIR representation (Approach A: builtin calls)

- `LirCallee` gains a `Builtin(String)` variant, printed `@name`:
  `call @format_args_new () (out %f)`,
  `call @format_args_push (in %ref_f) (in %piece) (out)`,
  `call @byte_buffer_from (in %p) (in %n) (out %buf)`,
  `@byte_buffer_ptr`, `@byte_buffer_len`, `@load_u8`, `@store_u8`, `@memcpy`.
- A builtin signature table verifies builtin calls. Like the existing `Func`
  check it validates arity (builtins have no `LirFunc` body); where a builtin's
  `in`/`out` shape is fixed it also validates the operand/place types, and
  width-parameterized forms (`@load_u8`, `@store_u8`) validate against the
  destination/pointee types. The table gives `@load_u8` two in-operands
  (`ref #bit8`, `#bitNative`; result `#bit8`) and `@store_u8` three
  (`ref #bit8`, `#bitNative`, `#bit8`).
- Data addressing reuses `const` with a data-symbol literal:
  `&1: ref #bit8 = const !c0`; the verifier resolves `!c0` against
  `module.data` and checks the data kind (`str`/`numlit`) matches the const's
  use.
- The folder evaluates `@byte_buffer_from` with a constant data-symbol pointer
  and length into a `Bytes` value, so a downstream fold-only `@as_int` sees the
  literal's bytes.
- Fold-required intrinsics (`@as_int`, `@as_float`, `@size_of`) are pre-fold
  **markers** (like `@compile_error`), not builtin calls: the front-end emits a
  marker node the folder must consume, and the verifier rejects any that
  survive (same discipline as `branch.if`-on-constant).
- No new surviving instruction shape is introduced. `@byte_buffer` and
  `@format_args` remain opaque builtin value types with no user-visible layout.

### 3.5 Call-eval (`lilac-const`)

- `ConstValue` grows to `Int(u64, Width) | Float(f64, Width) | Bytes(Vec<u8>)
  | Bool(bool) | Unknown`.
- Folding tracks constants across blocks (a block's entry state is the join of
  its predecessors; a non-equal join drops to `Unknown`), not just
  straight-line within a block.
- When a `call` to a pure user function has all-constant `in` arguments, the
  folder interprets the body: `MAX_FOLD_STEPS = 100_000` LIR steps and
  `MAX_CALL_DEPTH = 16`. It interprets `@load_u8`/`@store_u8`/`@memcpy` over a
  simple const-bytes memory model plus the ordinary `const`/`copy`/`arith`/
  `convert`/`field`/`branch`/`return` ops.
- Purity: loads are fine; stores to local places (params, allocas) are fine;
  observable effects — impure builtins, `panic`, live `@compile_error` — stop
  evaluation. A miss, exhaustion, or depth overflow leaves the runtime `call`
  in place: a missed fold costs performance only, never correctness.
- Fold-only intrinsics have no fallback: if an `@as_int`/`@as_float` cannot be
  evaluated (non-constant, bad digits, overflow) it is `E060`.

## 4. Spec decisions (normative patches)

- **Literal classes.** `SPEC.md` §2 is amended to define LitLiteral (hex/bin,
  bits known) vs NumberLiteral (decimal, `@byte_buffer` source bytes), and to
  state that decimal literals require an explicit `@as_int`/`@as_float` to
  become bits. Version header bumps per the §11 freeze rule, with paired UI
  tests.
- **`@as_*` are fold-only.** `SPEC.md` §2/§3 and `RUST_REWRITE_PLAN.md` §10.2
  are reconciled to "must be constant; non-constant is `E060`", removing the
  "keep the runtime parse" sentence.
- **`E061` format-piece cap.** New code for >16 f-string pieces, documented in
  `docs/errors/E061.md`.
- **Builtin calls + data addressing.** `IR_SPEC.md` gains a "Builtin calls"
  subsection (the forms in §3.4), the `const !cN` data-address form, and the
  rule that fold-only builtins never survive verification.

### 4.1 Deviations from the original plan

The M3 whole-branch review recorded these decisions where the shipped binding
behavior differs from (or sharpens) the original task text. They are binding;
the plan doc (`docs/superpowers/plans/2026-10-04-m3-strings-const.md`) is the
historical argument and is left unchanged.

- **R1 — literal lowering.** Literals lower directly to a `data` const plus
  `const !cN:#byte_buffer` (a data-backed buffer value); the illustrative
  `@byte_buffer_from(addr !str_N, len)` sequence is not emitted.
  `@byte_buffer_from` remains a builtin for user code only.
- **R2 — fold-only intrinsics are markers.** `@as_int`/`@as_float`/`@size_of`
  are pre-fold **markers** like `@compile_error`, not builtin calls; the
  verifier rejects any that survive folding.
- **R6 — decimal-to-bit diagnostic.** `a: @bit8 = 5` is `E043` (failed
  conversion search) and not `E010`, as an earlier draft/plan text said;
  `SPEC.md` already froze `E043`.
- **R8 — builtin offset arity.** The builtin table gives `@load_u8` two
  in-operands (`ref #bit8`, `#bitNative`; result `#bit8`) and `@store_u8` three
  (`ref #bit8`, `#bitNative`, `#bit8`).

## 5. Data flow / error handling / testing

- Flow: `read file -> Session -> lex -> parse -> collect -> sigs -> check+
  desugar (typeck, f-strings/literals) -> lower (data + builtin calls + fold-only
  markers) -> fold (peephole + call-eval; E060) -> verify (builtin table; reject
  surviving fold-only) -> check|print`.
- Errors: user failures return `Result<_, ErrorGuaranteed>`; poison propagates.
  New codes: `E060` (literal value error: bad digits, overflow, non-constant
  `@as_*`), `E061` (>16 format pieces). Retired: `E030` for string literals and
  f-strings. Each new code frozen by its `docs/errors/` doc with good/bad
  examples before the owning task commits.
- Tests (all as generated `*.lilac` + `*.lir` snapshots under `tests/ui/`):
  - `as_int_8/16/32/64.lilac`, `as_float_16/32/64.lilac` (ported from legacy,
    source `@as_int(5)` / `@as_float(5.25)`); snapshots show folded constants
    with no `@as_*` call.
  - `string.lilac` (literal + a user `@conversion` to a local `String`-shaped
    object), `fstring.lilac` (construction + conversion),
    `FAILS_fstring_missing_conversion.lilac` (hole `E043`),
    `fstring_piece_cap` (`E061`).
  - `as_int_overflow` (`E060`, `expected @bit8 (max 0xFF), found 9 bits`).
  - `cfold_call_eval.lilac` (a hand-written `@load_u8` parser over const bytes
    folds), `cfold_miss_is_ok.lilac` (non-constant stays a runtime call).
  - Fold unit tests: cross-block constant tracking, fuel exhaustion,
    depth cap, `@as_int` fold/`E060`.
  - IR unit tests: builtin-call arity/type verification; `const !cN` resolution;
    surviving fold-only builtin rejected.

## 6. Ports + exit criteria

- Port the seven legacy `as_*` sources (they currently use the old
  `@asInt8`/`@asFloat32` spellings) to `tests/ui/*.lilac` using the rewrite
  spellings, generating snapshots from `emit-ir`.
- Exit: (1) string and f-string sources check and emit IR (no `E030` for them);
  (2) `@as_int`/`@as_float` never appear in verified IR, folded to constants;
  (3) `E060` and `E061` snapshots green; (4) call-eval folds the hand-written
  parser and a miss stays a call; (5) no new surviving instruction shape;
  (6) `fmt`/`clippy`/`test` green; (7) all ported snapshots byte-stable via
  `emit-ir --check`.

## 7. Self-review

- Placeholders: none — every literal class, pass, code, and builtin form names
  its owner (typeck: literals/f-strings; lower: data/builtin calls; const:
  fold/call-eval; ir: builtin table/verifier).
- Consistency: §3.1's fold-only `@as_*` matches §3.5's "no fallback" and §4's
  patch; §3.2/§3.3 reuse the M2 desugar pattern and one string path; §3.2's
  `ref #bit8` pointer avoids inventing a `#ptr` type the IR lacks; §3.4's
  builtin calls satisfy §1's "no new surviving shapes" and `IR_SPEC` §9.5.
- Refinements applied during self-review: pointer type is `ref #bit8` (not
  `#ptr`); LitLiteral range error is checked in typeck where width is fixed;
  fold traces `@byte_buffer_from` on a data pointer so fold-only `@as_*` sees
  bytes.
- Scope: strictly M3; LLVM (M5), generics (M4), and stdlib `String`/streaming
  are explicitly out.
- Ambiguity: literal classes, `@as_*` constancy, `E061`, and the typeck desugar
  location are decided in §3–§4, not left to the implementer.
