# M2 Control + IR Freeze Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Check `if`/`match`/`loop`, multi-return, method calls, and `enum`/`union` bodies into desugared typed HIR, then lower through peephole folding and verification to canonical LIR v2 text.

**Architecture:** Grammar riders first (new AST nodes), then resolve/ty/sigs foundations, then the real `lilac-hir` node set; `check_bodies` becomes check-and-build returning `HirProgram` (core before control flow); `W020` rides `collect_sigs`; string-typed `lilac-ir` (printer + `E070` verifier) feeds mechanical `lower`, peephole `fold` (`E031`), driver/CLI wiring, and finally the 16 remaining non-generic legacy ports plus new UI tests.

**Tech Stack:** Rust, std only (no new crates — all six already scaffolded; no new dependencies).

**Spec:** `docs/superpowers/specs/2026-10-02-lilac-rust-rewrite-m2-design.md`, `RUST_REWRITE_PLAN.md` §5.3/§6/§7-M2/§10.3, `SPEC.md` §4/§6/§8, legacy `tests/*.lilac` + `tests/*.lilac-ir` as behavior ground truth.

## Global Constraints

- Workspace lints stay `deny(all + pedantic)` with the existing three allows; `cargo fmt --check`, `clippy -D warnings`, `cargo test` green after every task.
- No `unwrap`/`expect` on user input; `bug!()` for internal invariants only.
- Frontend crates never import `inkwell`/`llvm-sys` (CI grep gate).
- No new crates and no new external dependencies in M2.
- TDD per task: failing test first, watched fail, minimal code, full suite green.
- One commit per task.
- New codes are frozen once documented in `docs/errors/<CODE>.md` (good/bad example) before the owning task commits (`E031`, `E070`, `W020`).
- `emit-ir --check` byte-stability; snapshots generated via the binary, then eyeballed.
- Normative `SPEC.md` wording changes bump its version header per its own convention and need a paired UI test.

## Review Focus

Uncovered inputs most likely to bite, each pinned to its owning task:

1. `and`/`or` evaluating the right side when the left already decides → HIR must be a short-circuit branch-join, never a call (Task 5 test `and_short_circuits`).
2. `if cond value` without `else` used as a value → stays a parse error `E001` (the `expect(KwElse)` in `parse_if_value`), not a type error (Task 1 test `if_value_without_else_is_E001`).
3. `= match` (match as a value, as in `match_return_value.lilac`) → supported with REQUIRED `else` (parse `expect`, missing → `E001`, pinning `FAILS_match_returns_value`); arms + `else` unify against the expected type (Task 1 + Task 5 tests `match_value_needs_else`, `match_value_unifies_branches`).
4. Multi-`let` arity mismatch (`result: Object2 = get()` with 2 out-slots, or `x: T = f()` with 2 returns) → `E022` naming both counts (Task 4 tests `multilet_arity_mismatch_is_E022`, `single_let_against_multi_return_is_E022`).
5. Unknown member read (`variable.data` on an object without `data`) → `E021` naming the member and owner, NOT `E040` (M4 refines; Task 4 test `unknown_member_is_E021_not_E040`).

---

### Task 1: Grammar riders (and/or, match patterns, multilet, match-value, struct literals, function types)

**Files:**
- Modify: `crates/lilac-lex/src/lib.rs` (`KwAnd`, `KwOr` + keyword-table entries; `and`/`or` stop lexing as `Ident`), `crates/lilac-ast/src/lib.rs` (list below), `crates/lilac-parse/src/lib.rs` (`parse_or` → `parse_and` → `parse_equality` levels (`or` binds loosest); `parse_match_arm`; `parse_let_or_expr_stmt`; `parse_enum_variants`; `parse_primary`; `parse_type`; extend `Visitor` + `walk_*` for every new node and fix all existing `Visitor` impls).
- Test: unit tests in `crates/lilac-parse/src/lib.rs`.

**Interfaces:**
- Consumes: existing `TokenKind`, `Parser::parse_*` fns, `expect`, `join_spans`.
- Produces (used by Tasks 2/4/5): `BinOp::{And, Or}` (no hook names — Task 5 bypasses the M1 hook path, both sides must be `@bit8`); `pub enum MatchPattern { Tag(String), Const(String) }` replacing `MatchArm.tag: String` with `MatchArm.pattern: MatchPattern`; `Stmt::MultiLet { bindings: Vec<LetBinding>, value: Expr, span }` + `pub struct LetBinding { name: String, ty: Type, span }` (plain `Let` unchanged); `Expr::MatchValue { scrutinee: Box<Expr>, arms: Vec<MatchArm>, els: Box<Expr>, span }` (`else` required at parse, mirroring `parse_if_value`); `Expr::StructLit { ty_name: String, fields: Vec<StructField>, span }` + `pub struct StructField { name: String, value: Expr, span }`; `Type.func: Option<FuncType>` + `pub struct FuncType { params: Vec<Type>, returns: Vec<Type> }` (`None` for every existing form); `EnumVariant` gains `payload: Option<Type>` (mirroring `UnionVariant.payload`; `match_union_w_capture.lilac` declares payload enums, which do not parse today).

Rules: `or` binds loosest: `parse_or` calls `parse_and` calls `parse_equality`. `x: T, y: U = expr` parses comma-separated bindings then one `=`; `name: Type {` (brace, no `=`) is `E001` pointing at the `{` with help `use '= Type { … }'`. `match` arms accept `.Tag` or bare number/string patterns. `= match …` without trailing `else` is `E001`. `= Object { … }` parses via `parse_expr`; `function (… ) …` parses in type position into `Type.func`; `parse_enum_variants` accepts `Name` and `Name: Type` (payload). New `And`/`Or` expressions check as `E030` until Task 5 (note it at the use site you touch in Step 4 — no silent hook routing).

- [ ] **Step 1: Write failing tests** `and_or_parse_as_binop` (`0x0 and 0x1` → `BinOp::And`, `and` no longer `Ident`), `match_byte_arms_parse`, `multilet_parses`, `colon_struct_literal_rejected` (`E001` + help text), `structlit_equals_form_parses`, `match_value_without_else_is_E001`, `if_value_without_else_is_E001` (pins current `expect`), `func_type_position_parses`, `enum_payloads_parse` (`Some: @bit32` in enum position) (all in `crates/lilac-parse/src/lib.rs`).
- [ ] **Step 2: Run** `cargo test -p lilac-parse`. Expected: FAIL (no such nodes/levels).
- [ ] **Step 3: Implement** lexer keywords, AST nodes, parser levels/functions, `Visitor` + `walk_*` extensions per the rules above.
- [ ] **Step 4: Run** `cargo test -p lilac-parse`, full `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`. Expected: PASS, clean (M1 `check.rs` matching on `MatchArm.tag` or exhaustive `BinOp`/`Stmt`/`Expr` matches will fail to compile — update those use sites mechanically, no behavior change).
- [ ] **Step 5: Commit** `git add crates/lilac-lex crates/lilac-ast crates/lilac-parse && git commit -m "M2: grammar riders (and/or, match patterns, multilet, match-value, structlit, functypes)"`.

### Task 2: Resolve unions/enums, func types, @compilerError alias

**Files:**
- Modify: `crates/lilac-resolve/src/lib.rs` (`ResKind::Union` gains members; `ResKind::Enum` variants gain payloads; update the M1 `enum_variants_recorded` test for the new shape), `crates/lilac-ty/src/lib.rs` (`TypeKind::Func`, interning, display), `crates/lilac-typeck/src/sigs.rs` (`resolve_type` func form, alias, `sig_key` func rendering).
- Test: unit tests in each modified crate.

**Interfaces:**
- Consumes: Task 1 (`Type.func`, `UnionVariant`/`EnumVariant` AST shapes — read them; they carry optional payload types).
- Produces (used by Tasks 4–6): `ResKind::Union { members: Vec<UnionMember> }` + `pub struct UnionMember { name: String, payload: Option<AstType> }` (mirrors `UnionVariant`); `ResKind::Enum { variants: Vec<EnumMember> }` + `pub struct EnumMember { name: String, payload: Option<AstType> }` (payloads are new from Task 1); `Types::intern_func(params: Vec<TypeRef>, returns: Vec<TypeRef>) -> TypeId`; `Types::display` prints funcs exactly as `function (@bit8) -> @bit32` (params comma-separated in parens, `->`, returns comma-separated, `nothing` when empty); `lookup_intrinsic("@compilerError") == Some(Intrinsic::CompileError)` (legacy-spelling alias, documented in code comment); `resolve_type` recurses into `Type.func` (a `ref function` keeps `is_ref` as written).

- [ ] **Step 1: Write failing tests** `union_members_collected`, `enum_payloads_recorded` (resolve), `func_types_intern_and_display` (ty, assert the exact display string), `compiler_error_alias_maps` (typeck, asserting the `@compilerError` spelling maps).
- [ ] **Step 2: Run** `cargo test -p lilac-resolve -p lilac-ty -p lilac-typeck`. Expected: FAIL.
- [ ] **Step 3: Implement** per the interfaces above.
- [ ] **Step 4: Run** full workspace tests + clippy. Expected: PASS, clean.
- [ ] **Step 5: Commit** `git add crates/lilac-resolve crates/lilac-ty crates/lilac-typeck && git commit -m "M2: resolve unions/enums payloads, func types"`.

### Task 3: Typed HIR nodes

**Files:**
- Modify: `crates/lilac-hir/src/lib.rs` (replace the `HirExpr`/`HirProgram` stub; keep the `lilac_ty::TypeId` + `lilac_span::Span` imports).
- Test: unit tests in `crates/lilac-hir/src/lib.rs`.

**Interfaces:**
- Consumes: `lilac_ty::{TypeId, TypeRef, FuncSig}`, `lilac_resolve::DefId`, Task 2 shapes.
- Produces (used by Tasks 4/5/8): `HirProgram { funcs: Vec<HirFunc> }`; `HirFunc { def: DefId, name: String, params: Vec<HirParam>, returns: Vec<HirParam>, body: HirBlock, span }`; `HirParam { name: String, ty: TypeRef, span }`; `HirBlock { stmts: Vec<HirStmt>, span }`; `HirExpr { ty: TypeId, span: Span, kind: HirExprKind }`; `HirExprKind` = typed mirrors of every `ast::Expr` variant (literals, `Ident`+`DefId`, calls with resolved callee, member reads, `RefOf`, struct literals with checked fields) PLUS `Temp(TempId)` (branch-join use) and `CompileError { msg: String, call_span: Span }` (recorded, never emitted here — Task 9 decides liveness); NO `IfValue`/`MatchValue` in HIR (both desugar to statements with an out-temp, Task 5); `HirStmt::{Let, MultiLet, Assign, ExprStmt, Return, Break, If { cond: HirExpr, then: HirBlock, els: HirBlock, out: Option<TempId> }, Loop { body: HirBlock }, Match { scrutinee: HirExpr, arms: Vec<HirArm>, els: HirBlock, out: Option<TempId> } }`; `HirArm { pattern: HirPattern, capture: Option<HirParam>, body: HirBlock }`; `HirPattern::{Tag { name: String }, Const { bytes: String } }`; `TempId(u32)` newtype.

- [ ] **Step 1: Write failing tests** `hir_nodes_carry_types_and_spans` (hand-build one `HirFunc` with a `Let` + `If` + out-temp; assert fields, `Debug` renders).
- [ ] **Step 2: Run** `cargo test -p lilac-hir`. Expected: FAIL.
- [ ] **Step 3: Implement** the node set above (pure types, no logic).
- [ ] **Step 4: Run** crate tests + clippy. Expected: PASS, clean.
- [ ] **Step 5: Commit** `git add crates/lilac-hir && git commit -m "M2: typed HIR nodes"`.

### Task 4: Check-and-build core (methods, multilet, struct init, func values, => substitution)

**Files:**
- Modify: `crates/lilac-typeck/src/check.rs` (`Checker` emits HIR; `check_bodies` returns `HirProgram`), `crates/lilac-typeck/src/lib.rs` (re-export stays `check_bodies`).
- Test: unit tests in `crates/lilac-typeck/src/check.rs`.

**Interfaces:**
- Consumes: Tasks 1–3 (`Stmt::MultiLet`, `Expr::StructLit`, `TypeKind::Func`, HIR nodes, `Module.hooks` overloads, the M1 `ops::convert` on mismatches).
- Produces (used by Tasks 8/10): `pub fn check_bodies(program: &Program, module: &Module, sigs: &Sigs, types: &mut Types, diags: &mut Diags) -> HirProgram` (partial program on error paths — driver discards it when `has_errors()`; M1 unit tests calling `check_bodies` without using the return keep compiling).

Rules (all errors carry the two-span habit from M1): method call `recv.m(args)`: receiver must be object-typed else `E010`; member must exist else `E021` naming member + owner (Review Focus 5 — never `E040`); a non-function member value called → `E022`; `self` binds the receiver as `ref Parent` in first param position; overloads reuse exact-first selection. `MultiLet`: value must be a call whose out-arity equals binding count else `E022` naming both counts (covers Review Focus 4 both directions); elements check per-slot with conversion search. `StructLit`: type name must resolve to an object else `E023`; unknown field → `E021`; missing field → `E022` naming it; each value checks against the field type; if the object declares an `on_init`-shaped member hook (`@init` with first param `ref Self`), append an explicit HIR `@init(ref tmp)` call (arity/shape mismatch → `E022`). Func-typed locals: an `Ident` resolving to a `Func` item used as a value checks as its `FuncType`; calling through it checks against that type. `=>` substitution: a direct-`Ident` call to a non-recursive `Inline`-body function desugars to `{ tmp_i = arg_i; …; body with params → temps }` (args evaluate once); recursive `=>` stays a call. Control-flow statements/expressions keep emitting `E030` in this task (Task 5 owns them).

- [ ] **Step 1: Write failing tests** `method_call_checks`, `unknown_member_is_E021_not_E040` (assert code is `E021`), `calling_a_field_is_E022`, `multilet_arity_mismatch_is_E022`, `single_let_against_multi_return_is_E022`, `structlit_missing_field_is_E022`, `structlit_unknown_field_is_E021`, `init_hook_called` (HIR contains the `@init` call), `func_value_call_checks`, `inline_substituted_nonrecursive`, `recursive_inline_stays_call`.
- [ ] **Step 2: Run** `cargo test -p lilac-typeck`. Expected: FAIL.
- [ ] **Step 3: Implement** HIR-emitting checker + rules above.
- [ ] **Step 4: Run** full workspace tests + clippy. Expected: PASS, clean.
- [ ] **Step 5: Commit** `git add crates/lilac-typeck && git commit -m "M2: check-and-build methods, multilet, struct init"`.

### Task 5: Control-flow check-and-build (if/loop/match, captures, and/or, @compile_error)

**Files:**
- Modify: `crates/lilac-typeck/src/check.rs` (replace every control-flow `E030` arm).
- Test: unit tests in `crates/lilac-typeck/src/check.rs`.

**Interfaces:**
- Consumes: Tasks 1–4 (`MatchPattern`, `MatchValue`, HIR `If`/`Loop`/`Match` + out-temps, `TempId` freshening inside `Checker`).
- Produces (used by Task 8): fully desugared HIR for all control flow; no control-flow `E030` remains.

Rules: test positions (`if`/`loop-if` cond, `match` scrutinee, `and`/`or` operands) default bare `Number` literals to `@bit8`; other `@bitN` accepted everywhere via non-zero test (M1's cond-`@bit8`-only rule generalizes — update any M1 test that pinned the narrow rule). `if CALL -> name: T`: first out-slot is the condition (must be `@bitN`); `name` must match a call out-slot by name with type `T` else `E022` at the capture span (may name the first slot too); bound as a then-branch-only local. `Break` outside `Loop` → `E022` at the keyword. `loop if cond { } else { }`: both blocks check normally. Match arms: `.Tag` needs an enum/union scrutinee carrying that variant else `E010`; bare patterns need a `@bitN` scrutinee; `-> name: T` capture on a payload variant checks payload-against-`T` (`E010` on mismatch), on a payload-less variant → `E022`. `MatchValue`/`IfValue`: arms/branches check against the expected type (value-position threading, conversions allowed). `and`/`or`: operands `@bit8`, HIR is a short-circuit join over a temp (Review Focus 1 — never a hook call, bypassing `ops::convert`). `@compile_error(msg)` records `HirExprKind::CompileError` and emits nothing (Task 9 decides). M1's flow-insensitive all-slots-assigned `E027` is unchanged by control flow.

- [ ] **Step 1: Write failing tests** `if_any_bit_width_ok`, `capture_binds_by_name`, `unknown_capture_is_E022`, `stray_break_is_E022`, `match_byte_arms_check`, `match_tag_capture_payload`, `match_capture_mismatch_is_E010`, `capture_on_bare_tag_is_E022`, `match_value_unifies_branches`, `match_value_needs_else` (with Task 1's parse test), `and_short_circuits` (HIR is a join, not a call), `and_or_require_bit8`, `compile_error_recorded_silent` (no diagnostic from check alone).
- [ ] **Step 2: Run** `cargo test -p lilac-typeck`. Expected: FAIL.
- [ ] **Step 3: Implement** per the rules above.
- [ ] **Step 4: Run** full workspace tests + clippy. Expected: PASS, clean.
- [ ] **Step 5: Commit** `git add crates/lilac-typeck && git commit -m "M2: control-flow check-and-build"`.

### Task 6: W020 operator-type-change lint

**Files:**
- Modify: `crates/lilac-typeck/src/sigs.rs` (warn during hook collection), `crates/lilac-driver/src/lib.rs` (`Session.warn_on_operator_type_change`, `Session::new` sets it `true`, pass into `collect_sigs`), `crates/lilac-cli/src/main.rs` (`--allow operator-type-change` flips it `false`; usage line update).
- Test: typeck unit tests + driver unit test.
- Create: `docs/errors/W020.md`.

**Interfaces:**
- Consumes: `Sigs.hooks` overloads with resolved `FuncSig`s (M1 collection), `Diagnostic::warning` (exists in `lilac-error`), `Diags::has_errors` (must ignore warnings — verify and pin).
- Produces (used by Task 10): `pub fn collect_sigs(module: &Module, program: &Program, types: &mut Types, diags: &mut Diags, warn_op_change: bool) -> Sigs` (new trailing param — update the driver call site and every M1 unit-test call site mechanically).

Rules (plan §6, normative): for each user hook named `@add/@sub/@mul/@div/@mod/@negate/@less_than/@greater_than/@less_than_or_equal/@greater_than_or_equal`: arithmetic silent iff every param and return type is a `@bit` variant; comparisons silent iff both params share one type `T` and return is `@bit8`; else emit `Diagnostic::warning("W020", …)` naming the hook and the shape change. `check` still exits 0 with warnings only.

- [ ] **Step 1: Write failing tests** `op_change_warns_matrix` (`@add(Matrix, Matrix) -> @bit32` warns), `op_bit_mixed_silent` (all-`@bit` mixed widths, no diagnostic), `cmp_shape_silent` (`(T, T) -> @bit8` quiet), `warnings_do_not_fail_check` (driver test: warn-only source → `Ok` + `W020` in render).
- [ ] **Step 2: Run** `cargo test -p lilac-typeck -p lilac-driver`. Expected: FAIL.
- [ ] **Step 3: Implement** + write `docs/errors/W020.md` (bad = Matrix `@add`, good = `@bit` overload).
- [ ] **Step 4: Run** full workspace tests + clippy. Expected: PASS, clean.
- [ ] **Step 5: Commit** `git add crates/lilac-typeck crates/lilac-driver crates/lilac-cli docs/errors/W020.md && git commit -m "M2: W020 operator-type-change lint"`.

### Task 7: LIR v2 types, printer, verifier (E070)

**Files:**
- Modify: `crates/lilac-ir/src/lib.rs` (replace stub; keep `LILAC_IR_VERSION`, `LirModule::new`, `header_text` behavior byte-identical).
- Test: unit tests in `crates/lilac-ir/src/lib.rs`.
- Create: `docs/errors/E070.md`.

**Interfaces:**
- Consumes: `lilac_span::Span` only (LIR stays string-typed — no `lilac-ty` dependency; types print via the exact `Types::display` spellings from M1/Task 2).
- Produces (used by Tasks 8–10): `LirModule { version: String, target: String, data: Vec<LirData>, funcs: Vec<LirFunc>, layouts: HashMap<String, Vec<(String, String)>> }` (`layouts`: type name → `(field, type)` in order); `LirData { name: String, kind: LirDataKind, span: Span }`, `LirDataKind::{Str(String), NumLit(String)}` (rendered `!c0: str = "…"`, `!c1: numlit = "10.5"`); `LirFunc { name: String, ins: Vec<(String, String)>, outs: Vec<(String, String)>, blocks: Vec<LirBlock> }`; `LirBlock { label: String, insts: Vec<LirInst>, term: LirTerm, span: Span }`; `LirInst::{Const{…}, Copy{…}, Load{…}, Addr{…}, AddrFunc{…}, Call{…arity at site…}, Field{…name required…}, Alloca{…: zeroed}, Drop{…}, Arith{…typed…}}` per spec §3.2 (every inst carries its `Span`); `LirTerm::{Return, Branch{…}, BranchIf{…}, Panic{…}, Unreachable}`; `LirModule::to_text(&self) -> String` (header + bodies, canonical); `pub fn verify(module: &LirModule, diags: &mut Diags) -> Result<(), ErrorGuaranteed>` (replaces the `-> bool` stub; no other callers exist).

Rules: printer emits exactly the §5.3 shapes (`&1: #bit32 = copy %in`, `field %obj .name -> &1  // idx 2, #bit32`, `alloca #t %x : zeroed`, `call &f (in %a) (out %b)`). Verifier checks, each failure a spanned `E070` diagnostic (never panic): header version supported; all sigil names resolve (`%`/`&`/`@`/`#`/`$`/`!`); regs defined-once-before-use per block; exactly one terminator per block; branch targets exist; `call` arity matches callee `ins`/`outs` counts; `field` index < `layouts` len for the owner; arithmetic width matches the `#bitN` name (`i.*`/`f.*`); `return` only when every `out` place is textually assigned ≥ once in the function (deliberately mirrors the flow-insensitive M1 `E027` rule — a stricter flow-sensitive check here would reject programs typeck accepts); `compile.error` present → `bug!()` (unreachable — Task 9 removes it first).

- [ ] **Step 1: Write failing tests** `printer_emits_canonical` (the spec §5.3 `gen_function__bit32` text, byte-equal), `verify_rejects_old_copy` (hand-built `copy #t, ref %x` shape → `E070` with span), `verify_rejects_const_branchif`, `verify_rejects_missing_terminator`, `verify_accepts_clean`.
- [ ] **Step 2: Run** `cargo test -p lilac-ir`. Expected: FAIL.
- [ ] **Step 3: Implement** + write `docs/errors/E070.md`.
- [ ] **Step 4: Run** full workspace tests + clippy. Expected: PASS, clean.
- [ ] **Step 5: Commit** `git add crates/lilac-ir docs/errors/E070.md && git commit -m "M2: LIR v2 types, printer, verifier"`.

### Task 8: HIR → LIR lowering

**Files:**
- Modify: `crates/lilac-lower/src/lib.rs` (real lowering; add `lilac-resolve` + `lilac-ty` + `lilac-driver`? No — `Target` lives in driver; depend on `lilac-driver`? Cycle check: driver will depend on lower (Task 10) — so lower must NOT depend on driver. Take `target_triple: &str` as a plain parameter instead).
- Test: unit tests in `crates/lilac-lower/src/lib.rs`.

**Interfaces:**
- Consumes: Tasks 3–5 (HIR), Task 7 (LIR), `Module` (member lists for layouts + `on_drop` detection), `Types::display` (type spellings), existing `lower(hir: &HirProgram, target: &str)` signature which already returns `Result<LirModule, ErrorGuaranteed>`.
- Produces (used by Task 10): `pub fn lower(hir: &HirProgram, module: &Module, types: &Types, diags: &mut Diags, target_triple: &str) -> Result<LirModule, ErrorGuaranteed>` (extended params — no other callers exist yet).

Rules: each `HirFunc` → one `LirFunc` (`in #t %p` per param, `out #t %s` per return slot); every `%place` gets `alloca #t %x : zeroed` in `$entry`; control-flow statements → labeled blocks with `branch`/`branch.if`; match arms → tag-discriminant const compares (tags) or `cmp.eql` (byte patterns) chained with the explicit `else`/final `unreachable`; calls carry `(in …) (out …)` arity; `field` lowers with owner/index recorded and `layouts` filled from `Module`; `drop %x` emitted for a place iff its object type declares an `on_drop` member hook (look it up in `Module` via the type's `DefId` — no drop otherwise); `addr.func` for function values; literals via `const LIT:#t -> &reg` (M3 owns value folding, so `@as_int` etc. stay calls here).

- [ ] **Step 1: Write failing tests** `lower_if_produces_branches` (assert `branch.if` + two labels in text), `lower_multiret_arity` (`(in …) (out …)` counts), `lower_structlit_alloca_zeroed` (`: zeroed` present), `lower_drop_when_on_drop`, `lower_no_drop_without_hook`.
- [ ] **Step 2: Run** `cargo test -p lilac-lower`. Expected: FAIL.
- [ ] **Step 3: Implement** per the rules above.
- [ ] **Step 4: Run** full workspace tests + clippy. Expected: PASS, clean.
- [ ] **Step 5: Commit** `git add crates/lilac-lower && git commit -m "M2: HIR to LIR lowering"`.

### Task 9: Peephole fold and E031

**Files:**
- Modify: `crates/lilac-const/src/lib.rs` (extend `ConstValue` with `Int(u64)` + `Bool(bool)` only — the full lattice stays M3; add `fold`).
- Test: unit tests in `crates/lilac-const/src/lib.rs`.
- Create: `docs/errors/E031.md`.

**Interfaces:**
- Consumes: Task 7 LIR (insts carry spans; add a `CompileError { msg_span: Span, call_span: Span }` LIR inst shape in Task 7's enum for `@compile_error` sites — Task 7 owns the variant, this task consumes it).
- Produces (used by Task 10): `pub fn fold(module: &mut LirModule, diags: &mut Diags) -> Result<(), ErrorGuaranteed>`.

Rules (single pass, no fuel — nothing recurses): track `Reg -> Const` through `const`/`copy`; fold `i.add/sub`, `cmp.eql`, `extend.zero`/`truncate` on consts in place; `branch.if` on const `&c` → `branch $live`, deleting the dead successor block if unreferenced elsewhere (match chains are `branch.if`s, so const-scrutinee arms fold with no special rule); `CompileError` insts in dead blocks vanish with them; a still-live `CompileError` emits `E031` with both spans (literal + call). A missed fold is never an error.

- [ ] **Step 1: Write failing tests** `folds_const_arith`, `if_fold_prunes_dead`, `dead_compile_error_silent` (mirrors `cfold_dead_compile_error.lilac`), `live_compile_error_is_E031` (both spans in render).
- [ ] **Step 2: Run** `cargo test -p lilac-const`. Expected: FAIL.
- [ ] **Step 3: Implement** + write `docs/errors/E031.md` (bad = live `@compile_error`, good = dead-branch one dropped).
- [ ] **Step 4: Run** full workspace tests + clippy. Expected: PASS, clean.
- [ ] **Step 5: Commit** `git add crates/lilac-const docs/errors/E031.md && git commit -m "M2: peephole fold and E031"`.

### Task 10: Driver pipeline and CLI bodies

**Files:**
- Modify: `crates/lilac-driver/src/lib.rs` (private `run_pipeline`, `compile_lir`; `compile_source` keeps its `Result<Program, ErrorGuaranteed>` signature), `crates/lilac-cli/src/main.rs` (`emit-ir` prints `to_text()`; `--allow` flag), `docs/errors/E030.md` (retire landed triggers: member calls, `if`/`loop`/`match` + values, union members, `@compile_error` liveness; keep f-strings M3, generics M4, `using`, and the `Type(…)` call-form constructor).
- Test: driver unit tests.

**Interfaces:**
- Consumes: Tasks 4–9 entry points.
- Produces (used by Task 11): `pub fn compile_lir(&mut self, name: &str, src: &str) -> Result<LirModule, ErrorGuaranteed>`; `Session::new()` sets `warn_on_operator_type_change = true` (new `pub warn_on_operator_type_change: bool` field on `Session`, per plan §6); `emit-ir` prints the full module text and `--check` compares it (same snapshot path logic, unchanged).

Rules: private `run_pipeline(&mut self, name, src) -> Result<(Program, LirModule), ErrorGuaranteed>` runs lex → parse → `collect` → `collect_sigs(…, self.warn_on_operator_type_change)` → `check_bodies` (keep `HirProgram`) → `lower` → `fold` → `verify`; `compile_source` returns the `Program` (discards the module); `compile_lir` returns the module. `check` therefore runs everything through verify but prints diagnostics only.

- [ ] **Step 1: Write failing tests** `pipeline_produces_lir` (`if 0x0 { }`-shaped source via `compile_lir` → text contains `branch`), `check_runs_through_verify` (clean control-flow source → `Ok`, empty render).
- [ ] **Step 2: Run** `cargo test -p lilac-driver`. Expected: FAIL.
- [ ] **Step 3: Implement** + CLI wiring + `E030.md` refresh.
- [ ] **Step 4: Run** full workspace tests + clippy. Expected: PASS (the 4 M1 `.lir` snapshots now FAIL `--check` — bodies exist; Task 11 regenerates them).
- [ ] **Step 5: Commit** `git add crates/lilac-driver crates/lilac-cli docs/errors/E030.md && git commit -m "M2: driver pipeline and CLI bodies"`.

### Task 11: Ports, snapshots, UI tests, spec patches

**Files:**
- Copy verbatim: the 16 non-generic `tests/*.lilac` with legacy IR that M1 did not port: `enum_decl`, `enum_implicit_member`, `enum_use`, `function_multi_returns`, `function_multi_return_conversion`, `function_types`, `if_and`, `if_condition`, `if_else`, `if_or`, `inline_functions`, `inline_function_w_variable`, `inline_variables`, `main_function`, `references_2`, `union_decl` → `tests/ui/` (same basenames). (`generic_function.lilac-ir` stays unported — M4 owns monomorphization.)
- Regenerate with bodies: `tests/ui/{function_returns,int_addition,references,conversion}.lir` (generate via the binary, then eyeball).
- Create: `tests/ui/*.lir` for each port (generate + eyeball); `tests/ui/cfold_dead_compile_error.lilac` (dead `@compile_error` + `@compilerError` spellings both silent); `tests/ui/op_type_change_warn.lilac` (`@add(Matrix, Matrix) -> @bit32` user); `tests/ui/op_bit_mixed_ok.lilac`; `crates/lilac-cli/tests/m2.rs`.
- Fresh snapshots with no legacy IR (core M2 features, generate + eyeball): copy `match_enum`, `match_union_w_capture`, `object_instance_function`, `union_use` → `tests/ui/` (same basenames) + their `.lir`.
- Port as FAILS UI tests (assert exact codes): `FAILS_function_multi_returns` (`E022`), `FAILS_function_multi_returns_2` (`E027`), `FAILS_function_returns` + `FAILS_function_returns_2` (`E010`), `FAILS_if_as_value` (`E001`), `FAILS_match_returns_value` (`E001`), `FAILS_compiler_error` (`E031` — legacy camelCase spelling, via the Task 2 alias), `FAILS_object_member` (`E021`), `FAILS_object_setter` (`E022`).
- `object_setter_alt.lilac` uses the rejected `:`-without-`=` spelling: copy it verbatim to `tests/ui/` (keep the name — honest history) and pin its rejection (`E001` + the Task 1 help text). It stays OUT of the check-clean loop below.
- Modify: `SPEC.md` patches (B `=`-form canonical, E short-circuit no-overload, if-capture rule, match-value rule, `@compilerError` alias) + version bump per its convention; `IR_SPEC.md` conformance pass (patch on contradiction, paired with the snapshot that proves it).

**Interfaces:**
- Consumes: Task 10 CLI behavior.
- Produces: M2 exit criteria.

`m2.rs` tests: `check_ports_clean` (loop over the 16 ports + 4 fresh snapshots: `check` exits 0, stderr empty), `lir_snapshots_stable` (loop over the same 20 + the 4 regenerated M1 snapshots: `emit-ir --check` exits 0), one `*_is_<CODE>` test per FAILS port above (exit 1, stderr contains `error[<CODE>]`), `object_setter_alt_is_E001` (exit 1, stderr contains `error[E001]` + the help text), `op_warn_is_warning_only` (`check` exits 0, stderr contains `warning[W020]`, no `error[`), `op_mixed_silent` (exit 0, stderr empty), `cfold_dead_silent` (exit 0).

- [ ] **Step 1: Write** `m2.rs` + fixtures (copy the 16 verbatim; craft the new ones; generate `.lir` via `cargo run -q -p lilac-cli -- emit-ir`). NOTE: PowerShell `>` redirect writes UTF-16 — generate snapshots via the binary piped through a UTF-8 writer or `Set-Content -Encoding utf8` (M1 Task 7 ruling).
- [ ] **Step 2: Run** `cargo test -p lilac-cli --test m2`. Expected: PASS (implementation landed in Tasks 1–10; this task only moves test data — if any test fails here, the owning task's code is wrong, fix it there, not here).
- [ ] **Step 3: Apply** `SPEC.md`/`IR_SPEC.md` patches + version bump; regenerate the 4 M1 `.lir` files.
- [ ] **Step 4: Run** `cargo test --workspace`, `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`. Expected: all green, including the pre-existing `m0`/`m1` suites.
- [ ] **Step 5: Commit** `git add tests crates/lilac-cli SPEC.md IR_SPEC.md && git commit -m "M2: ports, snapshots, spec patches"`.

## Self-Review

1. **Spec coverage:** §2 HIR+checking → Tasks 1–5 (grammar → foundations → nodes → core → control flow); W020 (§4) → Task 6; §3.2 printer+verifier → Task 7; lowering → Task 8; §3.3 folding+E031 → Task 9; driver/CLI → Task 10; §5 ports+exit → Task 11. Enum payloads (from `match_union_w_capture`/`match_enum` ground truth) → Task 2 shapes + Task 5 capture rules. `=>` substitution (§10.1 inline-before-fold) → Task 4 rule. No task without a spec parent.
2. **Step scan:** Each step names one artifact or command with its expected output. Cross-task references go through Interfaces blocks only (e.g. Task 7 owns the `CompileError` LIR variant that Task 9 consumes — stated in Task 9's Interfaces, not repeated as code).
3. **Type consistency:** `MatchPattern`/`MultiLet`/`MatchValue`/`StructLit`/`Type.func` (T1) → resolve shapes (T2) → HIR nodes + `TempId` (T3) → `check_bodies -> HirProgram` (T4) → `lower(hir, module, types, diags, triple)` (T8) → `fold(module, diags)` (T9) → `compile_lir` (T10). `collect_sigs` gains its `warn_op_change` param once, in Task 6, with call-site updates called out.
4. **Review Focus:** All five lines have owning tests in Tasks 1/4/5.
5. **Proportion:** Interfaces carry signatures and exact values; bodies belong to the implementer. The plan runs longer than the spec because 11 tasks each restate their error docs and snapshot mechanics — accepted, kept tight by reference (no function bodies transcribed).

Gaps accepted deliberately: f-strings stay `E030` (M3); generic type args stay `E030` (M4); `using` stays single-file; overflow stays unchecked (`E060`, M3); `Type(…)` call-form constructors stay `E030`; `continue` does not exist in v1 grammar (no `break`-complement — `loop if` covers it); full call-eval/fuel stays M3.
