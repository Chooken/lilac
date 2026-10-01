# Lilac in Rust — Final Rewrite Plan (LLVM backend, modular, `rustc`-inspired)

> Status: plan, not implementation. Target: simple + robust over clever + fast.
> Gold standards: `rust-lang/rust` for errors and crate boundaries,
> `rustc` MIR + LLVM IR for IR discipline, Lilac IR text for readability.
> Current Zig sources consulted: `SPEC.md`, `IR_SPEC.md`,
> `src/sema.zig` (521 lines, generic instantiation + `ExprToTypeRef`),
> `src/sema/builder.zig` (`Scope` god-object, `Generic{base,cache}`),
> `src/logger.zig` (single-`Log` + `addLine`, no `ErrorGuaranteed`),
> `tests/generic_*.lilac`, `tests/FAILS_generic_duck_typing.lilac`,
> `tests/*.lilac-ir` (e.g. `generic_function.lilac-ir`, `function_returns.lilac-ir`).

---

## 1. Goals and non-goals

### Preserve (from `SPEC.md`)

* Bit containers `@bit8/16/32/64/Native`, `@byte_buffer`, `@format_args`,
  `unknown`, `nothing`, `self`.
* Zero-init everywhere, bitwise copy by default.
* `name: Type = value`, `ref` + auto-deref, no `ref ref` (unless wrapped).
* `function () out1: T, out2: U` multi-return, bare `return`, `if` values,
  `if f -> cap`, `loop`, non-exhaustive `match` + `else`.
* `object/enum/union` (interfaces removed; dynamic dispatch is user-land via
  duck-typed generics and function references).
  `using`, folder-mirrored namespaces (one namespace per folder, `private`
  excluded from imports), non-transitive imports.
* Duck-typed generics: `GenObj[T]`, `GenFunc[T]`, checked at instantiation.
* Inline `=>`, operators desugared to `@add/@sub/…/@negate`,
  `==` is non-overridable bit-equality, `@conversion` search,
  RAII `@init/@on_copy/@on_override/@on_drop` with the exact desugars in §8.

### Goals for the rewrite

1. Same language, Rust workspace, LLVM backend behind a stable trait.
2. `rustc`-grade diagnostics, especially for generics (explicit anti-C++ goal).
3. Fixed, verifiable Lilac IR v2 that keeps current readability.
4. Impossible to land a broken `main`: `cargo fmt/clippy/test` + UI snapshots.

### Non-goals for v1

* No incremental / `salsa` query system, no borrow checker, no async codegen.
* Opt level `-O0`-correct first; no custom LLVM passes.
* No language changes except where `SPEC.md` vs `IR_SPEC.md` contradict
  (listed in §5); those get a spec patch, not silent drift.

---

## 2. Principles stolen from `rustc` (and what we skip to stay simple)

Steal these files-as-patterns from `rust-lang/rust`:

* `compiler/rustc_span`: `Span{lo,hi,file}`, `SourceMap`, `Symbol` interner.
  No `String` in hot paths, no global `sources` list like `src/files.zig` today.
* `compiler/rustc_errors`: `Diagnostic{level,msg,labels,notes,helps,suggestions,code}`,
  `EmitterHuman/EmitterJson`, deduplication, `ErrorGuaranteed` poison.
  Lookup functions never print; they return `Result<T, ErrorGuaranteed>`.
* `compiler/rustc_session`: one `Session{source_map, diags, target, opts}` threaded
  explicitly. Kills the current `Logger` global + `std.debug.print` in passes.
* Crate-per-stage with acyclic deps:
  `rustc_ast -> rustc_hir -> rustc_middle -> rustc_mir_build -> rustc_codegen_llvm -> rustc_driver`.
  Ours mirrors that (§3). A new AST node touches one `Visitor`, not 5 `switch`es
  as in `sema.zig` / `analysis/*` / `generation.zig` today.
* `rustc_codegen_ssa` trait: frontend never imports `inkwell`/`llvm-sys`.

Skip to stay simple/robust:

* No `TyCtxt<'tcx>` lifetime maze. One owned `Middle` arena per compilation
  (`bumpalo` or `la-arena`), passed as `&mut Ctxt`.
* No full trait solver. Lilac needs only *requirement proving* for duck-typing
  (§4), not Rust traits + coherence + specialization.
* No MIR dataflow/borrow. Lower typed-HIR straight to Lilac IR v2, then LLVM.
* Single-threaded driver first. Parallelism only behind the `Backend` trait later.

Robustness rules (enforced by CI):

* `#[deny(warnings)]`, `clippy::all + pedantic -allow-large-enum-variant` at first,
  `cargo fmt --check`.
* No `unwrap/expect/panic!` on user input. Only `bug!()` for internal invariants,
  which prints `ICE + span + backtrace` like `rustc` and returns `ErrorGuaranteed`.
* Exhaustive `match`, newtypes for `DefId/TypeId/FuncId/BlockId/RegId`,
  `Span` on every fallible node.

---

## 3. Workspace layout (backend stays swappable)

```text
lilac/
  Cargo.toml                  # workspace, resolver=2, pinned inkwell + LLVM 18/19
  crates/
    lilac-span/               # FileId, BytePos, Span, SourceMap, Symbol. No deps.
    lilac-error/              # Diagnostic, Emitter, ErrorGuaranteed. dep: span.
    lilac-lex/                # Token, Lexer: Iterator<Result<Token>>. dep: span, error.
    lilac-ast/                # Untyped AST + Visitor. dep: span, lex.
    lilac-parse/              # Parser -> AST with Error recovery. dep: ast, error.
    lilac-resolve/            # DefId, ribs, folder-modules, visibility, usings. dep: ast.
    lilac-ty/                 # Type interner, TypeRef{id,is_ref}, FuncSig. dep: span.
    lilac-hir/                # Typed HIR. dep: ast, resolve, ty.
    lilac-typeck/             # collect_ids -> collect_sigs -> check_bodies + generics. dep: hir.
    lilac-ir/                 # Stable Lilac IR v2 types + text/json printer + verifier. No LLVM dep.
    lilac-const/              # ConstValue, inline(=>) + const-fold/call-eval on LIR. dep: ir only.
    lilac-lower/              # HIR -> LIR (zero-init, RAII, conversions; calls lilac-const). dep: hir, ir, const.
    lilac-codegen/            # trait Backend { codegen(&LirModule) }. dep: ir only.
    lilac-codegen-llvm/       # inkwell impl. ONLY crate linking LLVM.
    lilac-package/            # lilac.zon manifest parsing (name/version/paths/deps). No IO.
    lilac-build/              # Step/Artifact/Target graph + hashing. No process spawning.
    lilac-driver/             # Session{host,target,opts,cache}, orchestration, CLI plumbing.
    lilac-cli/                # binary: check / emit-ir / emit-llvm / build / test / --backend=
  tests/ui/                   # trybuild-style: *.lilac + *.stderr + *.lir snapshots
```

Dependency rule: frontend crates must not `use inkwell`.
`lilac-ir` is the stability boundary (versioned, see §5).
Future `lilac-codegen-cranelift` / `-qbe` / `-interp` implement the same trait;
driver picks with `--backend=llvm`.

Pass order in `lilac-driver::compile`:

```text
lex -> parse (recover) -> resolve ids -> collect sigs -> check bodies
  -> monomorphize generics -> inline (=>) -> const-fold/call-eval (LIR)
  -> lower RAII/drops -> verify LIR -> backend.codegen (only if !has_errors)
```

Each pass: `fn run(ctxt: &mut Ctxt, unit: &Unit) -> Result<(), ErrorGuaranteed>`.

LLVM choice: `inkwell` (safe wrapper) over raw `llvm-sys`. Pin one LLVM
(e.g. `llvm18-1`), document `llvm-dev` requirement. Keep `llvm-sys` escape hatch
closed in v1.

LLVM mapping sketch:

* `@bitN -> iN`, `@bitNative -> i32/i64 by target`, `ref T -> ptr`, `unknown -> ptr`,
  `nothing -> void`.
* Every `alloca` + `llvm.memset 0` (spec zero-init).
* Lilac `in/out` params -> LLVM params + `sret`/out-pointers.
  `a,b = f()` becomes `call f(&a,&b)`.
* `i.add/f.add/cmp.eql/field/...` -> `build_int_add/build_float_add/build_icmp/build_struct_gep`.
* `@size_of/@as_int/@as_float` const-folded in `lower`, never reach LLVM.
* `panic(msg)` -> `call @lilac_panic + unreachable`. `compile.error` diagnosed in
  `lower`, never emitted to LLVM.

---

## 4. Duck-typed generics without C++ template hatred

### 4.1 Why C++ templates are hated (what we must avoid)

1. Error at the *definition* implementation detail, 200 lines deep, not at use site.
2. No recorded *requirement*: compiler shows the failed `operator()` body,
   not "you needed `T::get() -> @bit32`".
3. Note spam + cascading errors from one bad instantiation.
4. No dedup: same `vector<Foo>` failure repeated per TU.
5. No suggestion: "did you mean `get`? missing `using`? wrong arity?".

Rust avoids this with trait bounds + obligation trees + `required by` notes +
short-error-by-default. Lilac has no traits, so we emulate the *effect* without
adding traits to the language.

### 4.2 Design: requirements + obligation tree + instantiation stack

Keep the language duck-typed (`SPEC.md` §7 stays), make the *compiler*
requirement-aware internally.

```rust
// lilac-typeck/generics.rs
pub struct GenericDef {
  params: Vec<Symbol>,          // [T]
  body: NodeId,                 // object/func body, with spans
  def_span: Span,
  cache: HashMap<Vec<TypeId>, TypeId>,
  failed: HashSet<Vec<TypeId>>, // poison: don't re-emit, return ErrorGuaranteed
  active: Vec<Vec<TypeId>>,     // cycle stack
}

pub enum Requirement {
  HasMethod { recv: TypeId, name: Symbol, sig: FuncSig, use_span: Span },
  HasField { owner: TypeId, name: Symbol, ty: TypeRef, use_span: Span },
  Convertible { from: TypeRef, to: TypeRef, use_span: Span },
  HasBinop { lhs: TypeRef, rhs: TypeRef, op: BinOp, use_span: Span },
  Sized { ty: TypeId, use_span: Span }, // e.g. rejects bare `unknown`
}

pub struct Obligation {
  req: Requirement,
  // where the requirement came from inside the generic body:
  def_span: Span,
  // chain: gen_function[T] instantiated at main.lilac:12 -> checking T.get() at gen.lilac:2
  parent: Option<ObligationId>,
  instantiation: InstantiationId,
}

pub struct Instantiation {
  generic: DefId,               // gen_function
  args: Vec<TypeId>,            // [@bit32]
  call_site: Span,              // main.lilac:12 `gen_function[@bit32]()`
  depth: u32,
}
```

Two-phase checking per instantiation (like `rustc` fulfillment, simplified):

1. **Collect** (during generic body check with `T` as opaque placeholder):
   record every duck-use as a `Requirement` with its `def_span`.
   Example `tests/generic_duck_typing.lilac:2` `out = T.get()` records
   `HasMethod{recv:@bit32-after-subst, name:get, sig: ()->T-ish}`.
2. **Prove** (after substituting `T := @bit32`): try each requirement against
   the real type table. Collect *all* failures for that instantiation before emitting,
   so one message lists every missing member, not one-at-a-time whack-a-mole.
   Overload resolution follows `SPEC.md` conversion ranking: exact
   `@add(a, b)` beats `@add(a, a)` + `b -> a`; conversions run only on mismatch,
   single step, binops anchored left (`E043` reports the pair tried plus the one
   conversion attempted).

Depth/cycle guards: `MAX_DEPTH = 5` displayed, full chain with `--verbose`;
recursive `active.contains(args)` -> `E051 cyclic generic` with the loop printed;
memoize success in `cache`, failure in `failed` (poison `TypeId::ERROR`,
propagate `ErrorGuaranteed` without new diagnostics — kills cascades).

### 4.3 Error rendering (rustc-style, short by default)

Primary error always points at the **use site**, not the generic innards:

```text
error[E042]: type `@bit32` does not satisfy `gen_function[T]` requirements
  --> main.lilac:12:28
   |
12 |     variable: @bit32 = gen_function[@bit32]()
   |                            ^^^^^^^^^^^^^^^^^^ instantiated here
   |
note: required by `gen_function[T]` at gen.lilac:1:1
   |
 1 | gen_function[T]: function () out: T {
   | ^^^^^^^^^^^^^^^ generic defined here
   |
note: ... which requires `T.other() -> ?` at gen.lilac:2:12
   |
 2 |     out = T.other()
   |             ^^^^^ missing method `other` on `@bit32`
   |
help: `Object` has `get`, did you mean `get`?
   |   --> obj.lilac:6:5
   |    get: function () result: @bit32
   |    ^^^ available here
```

Rules for `lilac-error`:

* One top-level `Diagnostic` per failed instantiation (deduplicated by
  `(generic DefId, args, call_site file+line bucket)`).
* `labels`: [call_site (primary), def_site, each failing use inside generic].
* `notes`: full instantiation stack, elided after 5 with
  `... 3 more frames elided (use --verbose)`.
* `helps`: ranked suggestions — edit distance on member names, missing `using`,
  arity mismatch ("`get` takes 0 args, you passed 1"), `ref` vs value
  ("try `ref variable`"), `@conversion` found ("`@conversion(from: CustomType) -> @bit8` exists, add `using Conversions`?").
* `--error-format=human|json`, stable `E0xx` codes:
  `E040 unknown member`, `E041 sig mismatch`, `E042 requirements unsatisfied`,
  `E043 conversion missing`, `E051 cyclic/recursive generic`, `E052 generic hook`.
* `FAILED` UI tests assert the short form; `--verbose` snapshot asserts the full tree.

This also fixes *non-generic* type errors ("print more useful errors when types have errors"):

```rust
pub struct TypeMismatch {
  expected: TypeRef, found: TypeRef,
  expected_span: Span, // the `name: Type` annotation
  found_span: Span,    // the value expression
  because: Option<ObligationId>, // e.g. via @conversion search failure
}
```

Render both sides every time (current `logger.zig` often logs one side):

```text
error[E010]: mismatched types
  --> main.lilac:6:17
   |
 6 |     out: @bit32 = gen_function[@bit16](0x1)
   |          ------   ^^^^^^^^^^^^^^^^^^^^^^^^^ expected `@bit32`, found `@bit16`
   |          |
   |          expected here
```

Pretty type printer: `GenObject[@bit32]`, `ref @bit8`, `function (@bit8) -> @bit32`,
never raw indices. Limit long types with `…` + `--verbose` full form.

### 4.4 What to implement first for generics

1. `Requirement` collection for method/field/call/convert/binop only (covers
   `FAILS_generic_duck_typing`, `generic_duck_typing`, `generic_object*`).
2. Instantiation stack + dedup + poison cache.
3. Human renderer + `E04x` codes + suggestion for name-distance only.
4. Later: arity/visibility-aware suggestions, `using`-aware hints, verbosity flag.

Acceptance: `tests/FAILS_generic_duck_typing.lilac` produces the 1-error shape above,
not the current silent/`Missing Field`-at-definition behavior, and
`cargo test` snapshots it.

---

## 5. Lilac IR audit vs Rust MIR/LLVM (keep readability, fix soundness)

### 5.1 What is good and must stay

* Sigils `! @ # % $ &` + keywords `type/function/data/allocate/copy/call/field`
  are terse and grep-friendly — far more readable than raw LLVM for this language.
  Keep them. Keep `function @name in … out … { }`, `type #name { … }`, `data { … }`.
* Flat, function-at-a-time layout matches `tests/*.lilac-ir` expectations.
  Don't adopt full LLVM verbosity.

### 5.2 Issues found in `IR_SPEC.md` + `tests/*.lilac-ir` (measured against MIR/LLVM)

1. **No version/target header.** MIR has crate disambiguators; LLVM has
   `target triple/datalayout`. Lilac IR has neither -> backend drift for `@bitNative`.
2. **No block/terminator discipline.** `{$LABEL: …}` is optional, `branch.if` else
   optional, fallthrough of `match` (spec: silent fallthrough) has no explicit
   `unreachable`/`fallthrough` terminator. MIR requires every `BasicBlock` ends in
   a terminator; LLVM requires terminators. Current IR can't be verified.
3. **Registers untyped and non-SSA.** `&`, `&1` reused across lines
   (`function_returns.lilac-ir:4-6`), no def-before-use rule, type only on `copy`.
   Arithmetic `i.add &, &, [%,&]` has no result type/width. MIR `Local`s are typed;
   LLVM `%r = add i32 %a,%b` is typed.
4. **`copy` overloaded 5 ways.** Literal, move, deref-load, overload for `ref`
   (`copy #t, ref %x, &` vs `copy #t, %x, &`), plus multi-return element copy.
   One opcode doing 5 things cannot be verified or cleanly lowered.
5. **`address` conflates things.** `address [%, @], &` merges var-address,
   func-address, and (implicitly) method-address. No pointee type.
6. **`call` unchecked.** `call [@,&], in %, out %` has no callee signature at site,
   `in/out` ordering by comment only. Multi-return `out %tmp:1` then
   `copy %tmp:1 -> %result` works by convention (see `function_returns.lilac-ir:6-7`).
7. **`field` by raw index.** `field #, %, #, 0, …` loses names, no bounds rule,
   layout (`tag + data` for unions, `value` for enums) implicit.
8. **Memory/lifetime invisible.** `allocate #, %` doesn't say stack/place vs temp,
   no `drop/defer` after RAII desugar, no `memset 0` marker despite zero-init being
   load-bearing for `==` bit-equality.
9. **`panic` / `compile.error` magic struct.** "expects bit64 len + ptr" is prose,
   not a type. `compile.error` should not exist post-`lower` at all.
10. **Int vs float chosen at use, not type.** Lilac types are uninterpreted bits
    (`SPEC.md` §1), but IR bakes `i.add` vs `f.add`. That's correct, but the rule
    "which interpretation is legal for which width" is nowhere stated.
11. **Literals untyped in text.** `0x1`, `"String"`, `"0.54"` appear bare;
    `SPEC.md` "Lilac to IR" says `copy #type, 0x0, &` but tests emit
    `copy #bit32, 0x1, &1`. No canonical literal grammar.

### 5.3 Lilac IR v2 (minimal fixes, same look)

Header (new, 2 lines, keeps diffs small):

```lilac-ir
lilac_ir = "0.2"
target = "x86_64-unknown-none"   // fixes @bitNative width
```

Functions/blocks (terminator-required, typed regs, explicit labels):

```lilac-ir
function @gen_function__bit32
  in #bit32 %in
  out #bit32 %out
{
  $entry:
    &1: #bit32 = copy %in
    copy &1 -> %out
    return
}
```

Rules:

* Every function body is ` $label: + insts + terminator`.
  Terminators only: `return | branch $l | branch.if &c, $t [, $e] | panic … | unreachable`.
  No fallthrough ambiguity: `match` without `else` lowers to explicit `else -> unreachable-or-empty-block`.
* Registers are SSA within a block: `&n: #type = …` defined once, used after def.
  `%vars` are mutable places (`alloca`d); `&regs` are values. This split mirrors
  MIR `Place` vs `Rvalue` while keeping one-character sigils.
* Replace overloaded `copy` with 5 readable ops (same width, clearer):
  `copy %a -> %b | &a -> %b`, `const 0x1:#bit32 -> &1`, `load ref %x -> &1`,
  `addr %x -> &1`, `addr.func @f -> &1`. Keep `copy` as the mnemonic, but with
  distinct, typed forms — no bare `ref` modifier on operands.
* `call &f (in %a) (out %b)` carries arity; verifier checks against callee sig.
* `field %obj .name -> &1  // idx 2, #bit32` — name required, index+type as comment
  (keeps readability, adds checkability).
* `alloca #t %x : zeroed` always; explicit `drop %x` where `@on_drop` was desugared.
  Zero-init is now visible to the backend instead of tribal knowledge.
* `data { !s "hello" }` string/number literals become typed consts
  `!c0: str = "…"`, numbers `!c1: numlit = "10.5"`; `@as_int/@as_float/@size_of`
  folded before IR except documented runtime remnants.
* `compile.error` banned in verified IR (diagnosed in `lower`).
  `panic !msg` takes a single `!const` of known `String` shape.

Verifier (`lilac-ir::verify`, run before any backend — like `mir::verify`):

* header present + version supported; `@bitNative` resolved by target.
* all `%/%&/@/#/$/!` names resolve; regs defined once before use in block.
* every block ends in exactly one terminator; all branch targets exist.
* `call` arity/types match; `field` index < layout len; arithmetic width matches
  (`i.add` only on `#bit8/16/32/64/Native`, `f.*` same widths, documented).
* `return` only when all `out` places definitely assigned (catches missing
  `result_1 = …` before `return`).
* Verifier errors are `bug!()`-free `Diagnostic`s with spans back to HIR.

Text stays canonical via `cargo run -p lilac-cli -- emit-ir`: reformat +
`--check` in CI, so `tests/ui/*.lir` snapshots don't drift like today's
hand-maintained `.lilac-ir` (note `generic_function.lilac-ir:8` calls `@get`
where the source defines `gen_function` — stale snapshot).

---

## 6. Error infrastructure (shared by §4 and §5)

Port `src/logger.zig` to `lilac-error`:

```rust
pub struct Diagnostic {
  pub level: Level, pub code: &'static str, pub msg: String,
  pub labels: Vec<Label>, pub notes: Vec<String>, pub helps: Vec<String>,
}
pub struct Label { pub span: Span, pub primary: bool, pub msg: String }

pub trait Emitter { fn emit(&mut self, d: &Diagnostic, sm: &SourceMap); }
pub struct EmitterHuman { color: bool, verbose: bool }
pub struct EmitterJson;

pub struct Diags { all: Vec<Diagnostic>, dedup: HashSet<u64>, emitter: Box<dyn Emitter> }
impl Diags {
  pub fn emit(&mut self, d: Diagnostic) -> ErrorGuaranteed { /* dedup, cap at N, return poison */ }
  pub fn has_errors(&self) -> bool { ... }
}
#[derive(Clone, Copy)] pub struct ErrorGuaranteed(());
```

Conventions:

* Functions that can fail on user code return `Result<_, ErrorGuaranteed>`.
* `TypeId::ERROR` / `DefId::ERROR` poison propagates without new noise.
* Cap: max 10 primary errors + `--verbose` for full obligation trees.
* Every `E0xx` documented in `docs/errors/E04x.md` with good/bad example
  (copy `rustc` error-index habit at tiny scale). Lints use `W0xx` codes under the
  same scheme; `W020 operator-type-change` fires during `collect_sigs` per `SPEC.md`
  §8 (silent iff all involved types are `@bit` variants, or a comparison keeps its
  `(T, T) -> @bit8` shape), gated by `Session.warn_on_operator_type_change`
  (default on, `--allow operator-type-change` silences).

---

## 7. Milestones and acceptance (manageable chunks)

Each milestone is independently shippable with its own UI snapshots. No milestone
lands with `cargo fmt --check`, `clippy -D warnings`, or `cargo test` red.

* **M0 skeleton (workspace + errors + lex/parse):** `span/error/lex/ast/parse/driver`
  + `Session{host,target}` stub + `emit-ir --check` stub + `tests/ui/hello.lilac`.
  Exit: `lilac check hello.lilac` renders one diagnostic correctly in human+JSON.
* **M1 types (non-generic):** `resolve+ty+typeck` for objects/functions/literals/calls;
  port `function_returns`, `int_addition`, `references`, `conversion` to `tests/ui`.
  Legacy `interface_*` tests are deleted with the feature; legacy `op_*`/`as_*`
  tests are renamed to snake_case hooks on port (`@asInt8(5)` becomes
  `x: @bit8 = @as_int(5)`).
  Exit: 4 `.lir` snapshots byte-stable through `emit-ir --check`, plus
  `FAILS_hook_generic.lilac` asserting `E052` on a blanket-generic hook.
* **M2 control + IR v2 freeze:** `if/match/loop`, multi-return, `enum/union` desugar,
  `lilac_ir = "0.2"` header, verifier + printer; port all legacy `*.lilac-ir` as
  generated snapshots; add `cfold_dead_compile_error` test. Exit: verifier rejects
  old `copy #t, ref %x` / `branch.if`-on-const with spanned errors, and
  `tests/ui/op_type_change_warn.lilac` (custom `@add(Matrix, Matrix) -> @bit32`
  warns `W020`) plus `op_bit_mixed_ok.lilac` (all-`@bit` mixed widths silent) are green.
* **M3 strings + const-fold:** §9 `@format_args` lowering + `data` escaping;
  §10 `lilac-const` (inline `=>`, peephole fold, bounded-loop call-eval with
  `MAX_FOLD_STEPS = 100_000`, `MAX_CALL_DEPTH = 16`); `string`, `fstring`,
  `cfold_*`, `@as_int` overflow tests. Exit: no new IR ops; `E060` snapshots green.
* **M4 generics + errors:** §4 requirements/obligations, dedup + poison cache,
  `E04x` renderer + `did-you-mean`; `FAILS_generic_duck_typing` short + `--verbose`
  snapshots; no-cascade regression test; in-body hook registration
  (`GenBox[T]` with `@on_drop`, consumed at one instantiation).
  Exit: error-tone review sign-off.
* **M5 LLVM + second backend spike:** `llvm -O0` correct (zero-init + drops observed
  in `emit-llvm`), then opts; minimal interpreter/C backend proving `Backend` is not
  LLVM-shaped; `Session{host,target}` + serializable LIR. Exit: `tests/ui` green on host.
* **M6 polish + build stubs:** `--error-format=json`, `docs/errors/E0xx.md`,
  `lilac-package`/`lilac-build` types + `lilac build` stub resolving package root,
  `SPEC.md`/`IR_SPEC.md` freeze check in CI. Exit: plan §11 DoD met.

Global definition of done: spec freeze (`SPEC.md`/`IR_SPEC.md` version bump for any
normative change), UI snapshots for every behavior change, error docs for every new
`E0xx`, no `unwrap` on user input, no frontend `inkwell` import (checked by
`cargo deny`-style import test or `grep` gate).

---

## 8. Open decisions (only these remain; none block M0)

1. `inkwell` pinned LLVM 18 vs 19 — pick whatever host `llvm-dev` supports; hidden
   behind `Backend` either way. Decide at M5 start.
2. ~~Text vs JSON canonical snapshot~~ — decided: text canonical, JSON for tooling.
3. Whether inferred `Requirement` lists become opt-in surface syntax later
   (`GenFunc[T] where T: has get()` a la Concepts) — v1 keeps them internal only;
   revisit after M4 error-tone review.

---

## 9. Strings and f-strings (locked before rewrite)

Design goal: no new builtin string type, no slice/array syntax, no IR changes.
`String` stays a stdlib fat-pointer; f-strings lower to ordinary calls and reuse
the duck-typing `Requirement` machinery from §4.

### 9.1 `String` / `Char` representation (stdlib, not builtin)

```lilac
String: object {
    Size: @bit32
    Buffer: ref @bit8   // UTF-8 bytes, not codepoints
}
```

* Compiler treats `String` bytes as opaque. No UTF-8 validation in the compiler;
  that lives in stdlib.
* `Char` literal `'A'` has type `@bit32` (Unicode scalar value), not `@bit8`.
  Single bytes are just `@bit8` values; multi-byte characters are `String`s of
  length > 1. This avoids a second string-like builtin.
* `"hi"` lowers in `lilac-lower` (not typeck) to a static const plus a borrowed
  view: `data !str_N "hi"` + `@byte_buffer_from(addr !str_N, 0x2)`. No heap
  allocation for literals. Same path as number tokens (both are `@byte_buffer`).

### 9.2 No `[i]` indexing — `[]` stays generics-only

`[` / `]` are reserved for generics (`GenObj[T]`, `f[@bit32](...)`). There is no
`a[i]`, `a[i:j]`, or `[1,2,3]` literal in v1, by design.

* Element access desugars to methods like operators do (`+` -> `@add`):
  `s.at(i)` and `s.slice(lo, hi) -> String` (new fat-pointer view, no copy).
* Raw access stays `ref + offset` via three new byte intrinsics (see §9.4).
  Pointer arithmetic itself remains `ref -> @bitNative + @iadd` per `SPEC.md` §5.
* Fixed arrays later: `Array[T]` generic object + `Array(T, len)` constructor via
  `@init`. No `T[N]` const-generics in v1.

### 9.3 F-strings lower to `@format_args`, consumed by expected type

`$"Hello {name}, you are {age}!"` lowers in `lilac-parse` (spans preserved per
piece) to `@format_args` construction — one `@byte_buffer` piece per chunk/hole,
past the 16-piece cap is a split-the-string error:

```lilac
__f: @format_args = @format_args_new()
@format_args_push(ref __f, !const "Hello ")  // borrowed static chunk
@format_args_push(ref __f, @convert(name))   // hole via @conversion(T -> @byte_buffer)
@format_args_push(ref __f, !const ", you are ")
@format_args_push(ref __f, @convert(age))
```

Holes convert through the ordinary `@conversion` search (missing conversion =
`E043` naming `T` and the hole span). Consumption is expected-type dispatch,
no inference, no special result rule:

1. Type with `@conversion(from: @format_args)` (e.g. `String`) converts —
   allocation lives in that user-land conversion.
2. Handler type with structural `append_literal(@byte_buffer)` / `append[T](T)`
   streams: piecewise appends, zero temp buffers, handler type from context.
3. Expected `@format_args` keeps the value (storable, declared builtin type).
4. No expected type checks it as `@format_args` and drops it.

Pieces borrow (statics plus conversion temps): storing past the statement is the
same caveat class as storing a `ref`; immediate consumption is always sound.
Holes mirror C# `$"{...}"`; no alignment/format clauses in v1.

Failure mode reuses §4 directly: holes record `Convertible{T -> @byte_buffer}`
obligations, proven at instantiation like any generic requirement.

Syntax rules (v1, mirroring C# `$"{...}"` holes):

* Interpolation is `{expr}` only, where `expr` is a full expression
  (identifier / member / call). No alignment/format clauses (`{x:.2}`);
  write `{x.toHex()}` instead.
* Escapes: `{{` -> `{`, `}}` -> `}`.
* Lexer emits `FStrStart / FStrMid / FStrEnd` and reuses the expression lexer
  inside `{...}` with brace-depth tracking, so `{foo.bar()}` errors point inside
  the braces, not at the whole literal.

Failure mode reuses §4 directly: holes record `Convertible{T -> @byte_buffer}`
obligations, proven at instantiation like generic requirements; a type without
a conversion gets the `E043` use-site error (pair tried plus conversion
attempted), not a dump of formatting internals.

### 9.4 Intrinsics needed (no new syntax)

The `@byte_buffer_*` / `@format_args_*` rows already in the `SPEC.md` §3 table
are the complete set: buffer ptr/len/from plus args new/push/count/piece. Byte
motion itself reuses `@load_u8` / `@store_u8` / `@memcpy`. Lexer work is
`FStrStart / FStrMid / FStrEnd` with brace-depth tracking so `{foo.bar()}`
errors point inside the braces.

### 9.5 IR impact: two builtin aggregates, no new op shapes

`lilac-ir` gains `#byte_buffer` (`{len, ptr}` view) and `#format_args`
(fixed-cap inline piece table) as builtin value types with the verifier rules in
`IR_SPEC.md`. String and f-string sugar introduces no other IR: `lilac-ir` sees
only `data` consts, `@byte_buffer_from`, `@format_args_*` calls, and ordinary
conversions/calls.

Acceptance: `tests/ui/string.lilac` (literal + `at`/`slice` via conversions),
`tests/ui/fstring.lilac` (conversion path), `tests/ui/fstring_handler.lilac`
(streaming append path), `FAILS_fstring_missing_conversion.lilac` (asserting the
`E043` shape), and a >16-piece cap test — all generated `*.lir` snapshots.

---

## 10. Compile-time evaluation: `@byte_buffer` literals, `if`-folding, and build-tool hooks

Yes to all three, with one shared foundation. The slow-runtime-parse worry
disappears: a digit buffer is *logically* a runtime-parseable string but
*physically* always folded away, because its operand is always a compile-time
constant. Same machinery gives `if`-folding for free. The Zig-style build tool
reuses the pipeline but needs one extra execution path (see §10.3).

### 10.1 Where folding lives in the pipeline

```text
check bodies -> monomorphize generics -> inline (=>) -> const-fold (LIR)
  -> lower RAII/drops -> verify LIR -> backend
```

* New crate `lilac-const` (`ConstValue`, evaluator, folding pass). Deps: `ir` only.
  Frontend crates never import it except `lilac-driver`.
* Fold on **LIR, not HIR**: LIR already has typed regs, explicit blocks and
  terminators (§5.3), so `branch.if &c, $t, $e` with const `&c` is trivial to
  replace with `branch`. HIR still has `if`-as-value sugar and is the wrong place.
* Inline runs **before** folding. All `=>` (inline) functions are substituted at
  call sites first, so a wrapped `@as_int` inside a `=>` helper is exposed as a
  direct intrinsic call before the folder sees it. Block-body functions are never
  implicitly inlined in v1.
* Beyond peephole rules, the folder does best-effort **call-eval**: it tracks
  `Reg -> Const` through `copy`/`addr`, and when it sees `call @f` with all-const
  `in` args to a pure function (no IO, no stores to non-locals, bounded loops
  under fuel, recursion-depth-capped), it interprets the body and folds the `out`.
  Custom parsers are attempted, not rejected by kind: a hand-written
  `parseU32(s: String)` looping over `@load_u8` with const bytes folds if it stays
  const and terminates within fuel. Success gives the same zero-cost as
  inlining; failure leaves the runtime call. A missed fold is never a hard error,
  only a missed optimization.
* `verify` runs *after* folding, so verified IR never contains `branch.if` on a
  constant condition, nor `@as_int/@as_float` on a constant literal. The verifier
  rejects them; this keeps backends dumb (no per-backend folding).
* Bounded fuel (`MAX_FOLD_STEPS = 100_000` per call-eval, `MAX_CALL_DEPTH = 16`).
  Anything not provably const stays as-is.

### 10.2 `@byte_buffer` digits as foldable bytes (zero-cost by construction)

Semantics (also patched into `SPEC.md` §1/§3):

* `0x1`, `10.5`, `0b101` lex as `Number` with type `@byte_buffer`, whose value
  is the source bytes (`data !num_N "10.5"`). The *logical model* is
  "you could parse these bytes at runtime".
* `@as_int(lit)`, `@as_float(lit)`, `@size_of(v)` are `foldable` intrinsics:
  if all operands are `Const`, `lilac-const` evaluates them during the fold pass
  and replaces the call with `const WIDTH:#type -> &reg`. The runtime-parse path
  is never emitted for constant inputs, so the slowness never materializes.
* Because a `Number` token is definitionally const, `@as_int(0xFF)` *always*
  folds (unless it errors -> `E060` diagnostic with both the literal span and the
  call span). The only non-folded case is a digit buffer smuggled through a
  generic that failed to monomorphize — which is already poison (`ErrorGuaranteed`).
* Wrapped conversions are the one exception to "always folds". Direct intrinsic
  calls fold regardless of inlining, but a helper such as
  `parseNum: function(n: @byte_buffer) r: @bit32 { r = @as_int(n) }` called as
  `parseNum(0xFF)` looks to LIR like `const "0xFF" -> %tmp; call @parseNum(%tmp)`.
  If that helper is a `=>` inline function it is substituted before folding, so no
  penalty. If it is a block-body function, folding relies on best-effort call-eval
  (§10.1): pure bodies with bounded loops and all-const args still fold, including
  hand-written parsers over const bytes; anything else
  keeps the runtime call plus the runtime parse. Rule of thumb: tiny conversions
  that must be zero-cost are written as `=>`; a missed fold is performance only,
  never a correctness error.
* Overflow / bad digits are compile errors at the call, not silent wraps:
  `@as_int("0x1FF") -> @bit8` emits `E060 value does not fit`, with
  `expected @bit8 (max 0xFF), found 9 bits`.

`ConstValue` lattice (v1, deliberately tiny):

```rust
pub enum ConstValue { Int(u64, Width), Float(f64, Width), Bytes(Vec<u8>), Bool(bool), Unknown }
pub fn eval_intrinsic(name: Symbol, args: &[ConstValue], target: &Target) -> Result<ConstValue, FoldError>
```

Foldable set v1: `@as_int`, `@as_float`, `@size_of`, `i.add/sub/mul/div/mod`,
`f.add/sub/mul/div/mod`, `@i_less_than/@i_greater_than`, `@f_less_than/@f_greater_than`,
`@i_negate/@f_negate` (as `0 - x`), `cmp.eql`, `i.cmp.lt/gt`, `and/or/xor/not/shift`,
`extend.zero/truncate` on consts. Everything else (including `@load_u8` on
non-const pointers) is left alone.

### 10.3 `if`-folding (and `match`/`loop` degens)

Same pass, same lattice:

* `branch.if &c` where `&c` is `Const Bool/@bit8`: replace with `branch $live`,
  delete dead successor if now unreachable. `if 0x0 {A} else {B}` keeps only `B`.
* `match` on a const tag with a matching literal arm folds to that arm; no match
  folds nothing (non-exhaustive fallthrough preserved).
* `loop { break }` (immediate break) folds to empty block. No loop unrolling in v1.
* Crucial interaction with `@compile_error(msg)`: it fires **only if still live
  after folding**, like Zig `comptime` + Rust `const panic`. Dead-branch
  `@compile_error` is silently dropped. Document this; add
  `tests/ui/cfold_dead_compile_error.lilac` asserting no error.

### 10.4 Build tool in Lilac (Zig-style), infrastructure reserved now

Zig's model (`build.zig` + `build.zig.zon` + build runner + cache) maps to:

```text
crates/
  lilac-package/  # manifest parsing: lilac.zon (name, version, deps, paths). No IO.
  lilac-build/    # Step graph types: Step{Run,Compile,Test}, Artifact, Target, Optimize.
                  # Pure data + hashing for cache keys. No process spawning.
  lilac-driver    # `lilac build [--target ..] [--release]` subcommand (stub in v1).
  lilac-cli       # `check / emit-ir / emit-llvm / build / test`
```

Two execution paths, kept separate on purpose:

1. **Pure const-eval** (`lilac-const`): no IO, no allocation beyond consts, fuel-capped.
   Used for digit buffers + `if`-folding. Safe to run per-function during compile.
2. **Build-script execution** (later): `build.lilac` is a *normal* Lilac program with
   `build: function(b: ref Build) nothing`, compiled for the **host** target via the
   same `Backend` trait (`build_exe_and_run` method), exposing `lilac-build` host
   functions over FFI. It then drives `lilac-driver` as a library to compile the real
   program for the **target**. Needs: host-vs-target `Session`, package-root discovery
   (walk up to `build.lilac` / `lilac.zon`), content-hashed cache dir (`.lilac-cache/`),
   `LirModule` serialization for cache hits.

v1 reservation (so we don't paint ourselves shut):

* `Session { host: Target, target: Target, optimize: Opt, cache_dir: Path }`
  from day one — never assume host == target (matters for `@bitNative` + folding).
* `lilac-ir` serializable (serde) so future cache keys can hash it.
* `Backend` trait gets `fn build_exe_and_run(&self, module: &LirModule) -> Result<i32>`
  as a *default-unimplemented* method; LLVM implements it later via JIT/AOT+spawn.
* `lilac-package` manifest format frozen early (`lilac.zon`:
  name/version/paths/deps/link, where `link = ["c"]` declares native libraries),
  mirroring `build.zig.zon` paths semantics, even though `lilac build` initially just
  resolves the package root and shells out to `emit-llvm`.
* No IO in `lilac-const`; no comptime mutation of global state. Build scripts get IO
  only through `lilac-build` host calls, never through const-eval.

Milestone mapping: M2 gets peephole folding + `if`-folding + dead-`@compile_error`
tests. M4 gets `Session{host,target}` + serializable LIR. Full `lilac build`
execution is post-v1, but the crates and trait methods exist as stubs so the
cutover is additive.

---

## 11. Execution contract (how to follow this plan)

1. **Work in milestone order; one milestone at a time.** Each bullet in §7 is
   3–8 issues max (e.g. M0: workspace, span, error+emitters, lex, parse, driver
   stub, `hello` UI). Open one issue per crate or per UI snapshot, link it to the
   milestone, close only when its snapshot is green.
2. **Spec freeze rule.** `SPEC.md` / `IR_SPEC.md` carry the version header. Any
   normative wording change bumps `0.2-draft` and needs a paired UI test. Plan-only
   changes stay in this file and never alter compiler behavior silently.
3. **Gates on every PR:** `cargo fmt --check`, `clippy -D warnings`,
   `cargo test` (unit + `tests/ui`), `emit-ir --check` byte-stability, and
   `grep -r inkwell crates/lilac-{span,error,lex,ast,parse,resolve,ty,hir,typeck,ir,const,lower}`
   must be empty (frontend stays LLVM-free).
4. **Traceability.** Test names encode spec sections: `tests/ui/{string,fstring,cfold,generics}_*.lilac`
   map to `SPEC.md` §2 / plan §9–§10; `E0xx` snapshots map to `docs/errors/`.
5. **Risks, accepted:** generic error tone needs human review (mitigation: M4
   sign-off gate); LLVM version drift (mitigation: pin + single `inkwell` crate);
   const-eval scope creep (mitigation: fuel/depth caps + miss-is-ok rule).
   Out of scope until post-v1: incremental compilation, borrow checking,
   `comptime` keyword, full `lilac build` execution.

---

## 12. Foreign interface & async (locked, needs no new crates)

`extern` declarations lower to LIR external symbols (`extern @name in … out …`,
no body); backends emit native external calls, so FFI stays backend-modular.
`export` is a reserved keyword with no semantics in v1. Async is stdlib over
`extern`-sourced context switching per `SPEC.md` §9: no compiler runtime, no
intrinsics, `await` deferred as optional desugar-only syntax. UI coverage lands
with M5 LLVM work: an `emit-ir` snapshot of an extern declaration plus a call
(real libc linking exercised after that, not as part of it).
