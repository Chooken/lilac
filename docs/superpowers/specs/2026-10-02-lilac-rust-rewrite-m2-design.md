# Lilac Rust Rewrite — M2 Design (control flow + IR v2 freeze)

> Scope: M2 only (`RUST_REWRITE_PLAN.md` §7 M2 bullet). Normative language/ir docs:
> `SPEC.md` v0.2-draft, `IR_SPEC.md` v0.2. Prior records: M0 design
> (`docs/superpowers/specs/2026-10-01-lilac-rust-rewrite-m0-design.md`),
> M1 plan (`docs/superpowers/plans/2026-10-01-m1-types.md`).
> Approach chosen: full typed HIR tree (plan-faithful; side-table and skip-HIR
> rejected during brainstorming). Work stays on branch `rust-rewrite-m0`.

## 1. Outcome / constraints / success

- Outcome: `lilac check` typechecks `if`/`match`/`loop`, multi-return,
  method calls, and `enum`/`union` bodies; `emit-ir` prints full v2 LIR
  (header + bodies) through a verifier; peephole folding drops dead
  `@compile_error`; `W020` warns on operator shape changes.
- Constraints: (a) same gates as M0/M1 — `cargo fmt --check`,
  `clippy -D warnings`, `cargo test` green after every task;
  (b) frontend crates never import `inkwell`/`llvm-sys`;
  (c) no `unwrap`/`expect` on user input — `bug!()` for ICEs only;
  (d) std only, no new crates beyond the already-scaffolded ones;
  (e) TDD per task, one commit per task, error docs for every new code.
- Success: exit criteria in §6 all green; generic `*.lilac-ir` explicitly
  NOT ported (M4 owns monomorphization).

## 2. Architecture (plan §3, M2 slice)

```text
crates/
  lilac-hir/      # M2: real (HirProgram, typed + desugared control flow)
  lilac-typeck/   # M2: check_bodies becomes check-and-build (one walk) + W020
  lilac-resolve/  # M2: collect union members (M1 skipped them)
  lilac-ir/       # M2: real (LirModule + printer + verify -> E070)
  lilac-lower/    # M2: real (HIR -> LIR, drops, alloca : zeroed)
  lilac-const/    # M2: real but small (peephole + if-fold + dead-compile_error)
  lilac-driver/   # M2: lex -> parse -> collect -> sigs -> check+build -> lower
                  #       -> fold -> verify -> print
```

- `check_bodies` emits `HirProgram` in the same walk that checks: M1's
  expected-type threading already visits every node, so a second pass
  would duplicate traversal. New `typeck::hir_build` module shares
  `check.rs`'s `Ctx`.
- `lilac-const` in M2 is a peephole only. Full call-eval with fuel stays
  M3 — M2 must not grow a second evaluator by accident.
- `check` runs the whole pipeline through verify but prints diagnostics
  only; `emit-ir` prints LIR text (replacing the M1 header-only stub).
- `W020` rides in `collect_sigs`, gated by
  `Session.warn_on_operator_type_change` (default on) with CLI
  `--allow operator-type-change` to silence (plan §6).

## 3. Components

### 3.1 HIR + checking (retires most M1 E030s)

- HIR mirrors the AST with `TypeId` on every expression, `DefId` on every
  binding, `Span` on every node (verifier errors and the M1-deferred
  mismatch declaration spans both point at source).
- Method calls (`obj.get()`): callee `Member` resolves against the
  receiver's object member table; `self` binds as `ref Parent` per the M1
  rewrite; overloads reuse exact-first selection.
- `if` (statement and value): condition must be `@bit8` else `E010` with
  both spans; branches check against the expected type; HIR desugars to
  branch-join over a temp place. `if f -> cap` binds the payload in the
  then-branch only.
- `loop`: `break`/`continue` validated against the enclosing loop (stray
  keyword → `E022` at the keyword span); HIR keeps an explicit labeled
  block. No unrolling in M2.
- `match`: arms accept `.Tag` and byte patterns (gap C fixed here —
  parser change rides along); SPEC allows non-exhaustive + `else`; HIR
  lowers to tag-test chains ending in the explicit `else` or an
  `unreachable` block, so the verifier never sees fallthrough ambiguity.
- Multi-return (`a, b = f()`): LHS arity must equal callee out-arity else
  `E022` at the call span; each element checks against its slot type.
  Bare `return` keeps M1's flow-insensitive all-slots-assigned `E027`
  rule — control flow does not refine it in M2.
- `enum`/`union`: leading-dot tags keep M0-gap-D parsing with type from
  context; union field access checks the tag set; layout (`tag + data`)
  is `lower`'s concern, not typeck's.
- `TypeKind::Func(params, returns)` added to `lilac-ty`; parser accepts
  `function (…) …` in type position; values are function references via
  `addr.func` (gap F, slated for the M1 port but never landed).
- E030 retires for: member calls, `if`/`loop`/`match`, `if`-values,
  `@compile_error` (liveness decided by folding, §3.3). E030 stays for:
  f-strings (M3), generic type args (M4), `using` beyond single-file.

### 3.2 LIR v2 printer + verifier (plan §5.3 verbatim)

- Op set replacing the 5-way overloaded `copy`: `copy` (place→place,
  value→place), `const LIT:#type -> &reg`, `load ref %x -> &reg`,
  `addr %x -> &reg`, `addr.func @f -> &reg`; `call &f (in …) (out …)`
  with arity at site; `field %obj .name -> &reg` (name required,
  index+type as trailing comment); `alloca #t %x : zeroed`; explicit
  `drop %x` where RAII desugar requires. Terminators only:
  `return | branch $l | branch.if &c, $t [, $e] | panic !c | unreachable`.
  `%vars` are mutable places; `&regs` are SSA values defined once per block.
  `data` consts are typed (`!c0: str`, `!c1: numlit`). `compile.error` is
  banned post-fold — the verifier treats it as `bug!()` (reaching it
  means folding failed to drop dead code).
- Verifier rules, all as spanned diagnostics back to HIR spans (never
  panics): header present + version supported; every sigil name resolves;
  regs defined-once-before-use per block; exactly one terminator per
  block; all branch targets exist; `call` arity/types match callee sig;
  `field` index within layout; arithmetic width matches (`i.*`/`f.*` only
  on `#bit8/16/32/64/Native`); `return` only when all `out` places
  definitely assigned. Failures are new code `E070` (`docs/errors/E070.md`).
- Printer is canonical via `emit-ir`; `--check` byte-compare stays the CI
  mechanism; the 4 M1 header-only `.lir` snapshots are regenerated with
  bodies.

### 3.3 Folding + `@compile_error` liveness + W020

- Peephole rules on LIR, single pass, no fuel (nothing recurses):
  constant arithmetic on `const` regs, `branch.if &c` with const `&c` →
  `branch $live` (dead successor deleted if unreferenced),
  `match`-on-const-tag → winning arm. Runs after lower, before verify.
- `@compile_error(msg)`: typeck records it in HIR with its span but emits
  nothing; the fold pass drops it with dead branches and emits new code
  `E031` (`docs/errors/E031.md`) only if still live after folding
  (Zig-`comptime` / Rust-`const-panic` semantics).
- `W020 operator-type-change` (`docs/errors/W020.md`): fires when a user
  `@add`-family hook changes the operator's shape — silent iff all
  involved types are `@bit` variants, or a comparison keeps
  `(T, T) -> @bit8`. Warning only; `check` still exits 0.

## 4. Grammar riders (spec decisions, all approved)

- **Gap B (struct literals):** canonical form is `= Object { member = 0x1 }`
  (with `=`), matching `name: Type = value`. The `:`-without-`=` spelling
  becomes a parse error at the `:` with a `use '= Object { … }'` help.
  SPEC patch + version bump per the §11 freeze rule, with a paired UI test.
- **Gap E (`and`/`or`):** lexed as keywords, parsed as `BinOp`, but HIR
  builds short-circuit branch-joins over `@bit8` — not plain `@and`/`@or`
  hook calls, which would evaluate both sides. No user overloading in M2.
  SPEC patch documents this.
- SPEC patches ride with their implementation tasks (paired UI tests);
  `SPEC.md` `0.2-draft` bumps when the first normative wording lands.

## 5. Data flow / error handling / testing

- Flow: `read file -> Session -> lex -> parse -> collect -> sigs (+W020)
  -> check+build HIR -> lower -> fold -> verify -> check|print`.
- Errors: user failures → `Result<_, ErrorGuaranteed>`; poison propagates
  without new noise; new codes `E031` (live `@compile_error`), `E070`
  (LIR verification), warning `W020` — each frozen by its `docs/errors/`
  doc with good/bad examples before the owning task commits.
- Tests: `tests/ui/*.lilac` + `*.lir` generated snapshots for every
  behavior (control-flow shapes, multi-return arity pass/fail, method
  calls, enum/union, struct-literal good/bad, `and`/`or` short-circuit,
  `cfold_dead_compile_error` + live-E031, `op_type_change_warn` +
  `op_bit_mixed_ok`); verifier unit tests for each rule in §3.2
  (old-style `copy #t, ref %x` and `branch.if`-on-const rejected with
  spanned E070).

## 6. Ports + exit criteria

- Regenerate `tests/ui/*.lir` from sources for the **non-generic** legacy
  suite only (21 `tests/*.lilac-ir` files triaged; generic ones such as
  `generic_function` wait for M4 — porting them now would freeze wrong
  output, and at least one is already stale). Stale hand-maintained IR is
  replaced, never copied.
- Exit: (1) verifier rejects old `copy #t, ref %x` / `branch.if`-on-const
  with spanned errors, pinned by tests; (2) `op_type_change_warn` warns
  W020, `op_bit_mixed_ok` silent; (3) `cfold_dead_compile_error` silent,
  live case errors E031; (4) all ported snapshots byte-stable via
  `emit-ir --check`; (5) fmt/clippy/test green.

## 7. Self-review

- Placeholders: none — every type, pass, code, and doc above names its
  owner (HIR → hir_build task; LIR → ir task; fold → const task).
- Consistency: one-walk check+build matches §2 pipeline; M2-small const
  matches §10.3 mapping (peephole + if-fold + dead-`@compile_error`);
  E030 retire/stay list matches M1 `E030.md` triggers minus M2's scope.
- Scope: strictly M2; full call-eval/fuel, generics, strings, LLVM all
  deferred to their milestones. Generic `.lilac-ir` exclusion is explicit.
- Ambiguity: `and`/`or` non-overloadability and `=`-form struct literals
  are decided in §4, not left to the implementer.
