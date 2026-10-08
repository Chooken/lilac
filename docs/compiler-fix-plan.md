# Compiler Fix & Simplification Plan

> Status: phases 0–7 done (2026-10-08) except 6.6 (typed LIR, optional) and
> archiving `docs/superpowers` (left for the owner). Source: the compiler
> review of the same date.
>
> Decisions made while implementing (all in `SPEC.md`):
> - Assigning a function's own out slot skips `@on_override` (it is
>   initialization; the old value is still dropped), so a guarded type such as
>   `Const[T]` can be returned.
> - A `.` at the start of a line is never a member access (it starts the next
>   `.Tag` match arm).
> - Recursive `=>` functions are legal (always-inline is a hint LLVM drops for
>   recursion); the old recursion `E022` is gone.
> - Legacy `tests/*.lilac` that no longer parse were deleted; the rest are
>   checked by `legacy_programs_check_clean`. `lilac-build`/`lilac-package`
>   stubs and the unemitted `E030`/`E040`/`E041` docs were removed.
> Goal: make `lib/Standard` compile and run, fix the confirmed bugs, and cut
> the compiler down to something one person can hold in their head.
> `SPEC.md` stays normative; every phase lists the spec edits it needs.

## Decisions recorded

These came out of the review discussion and are fixed for this plan.

| Topic | Decision |
|---|---|
| `if f() -> name: T` captures | **Kept.** Multi-return is first class; captures stay. (Implementation shares code with `match` captures, see 3.3.) |
| `self` | Stays the receiver keyword. The internal `self` → `instance` rename is removed (2.4). |
| f-string handler protocol (SPEC §2 rule 2) | **Removed from the spec.** `@format_args` + `@conversion` is the only path; the compiler knows nothing about strings. |
| Enum construction | `Enum.Variant`, `Enum.Variant(payload)`, `Enum[T].Variant(payload)`; `.Variant` / `.Variant(payload)` when the enum is inferable from the expected type. |
| Generic struct literals | `Name[T] { ... }` is the default form; `Name { ... }` infers arguments from the expected type when possible (optional sugar). |
| `loop` | `loop <statement>`. `loop { }`, `loop if c { } else break`, `loop match ...` all fall out. |
| `else` | `else <statement>` (so `else break`, `else return`, `else if ...` work). |
| Declaration shorthand | `x: T { fields }` ≡ `x: T = T { fields }`; `x: T(args)` ≡ `x: T = T(args)` (`@init`). Works for generic `T`. |
| Value `if` | `if c then x else y`. `then` becomes a keyword. |
| By-value parameters | Caller copies (`@on_copy` when the argument is a place, move when it is a temporary); callee owns and drops its parameters on exit. |
| Moves | No move intrinsic. Fresh values (struct/variant literals, call results) move; only reads *out of a place* copy. `to = Box[T] { value = self.value }` is the idiom. |
| E027 | **Dropped.** Return slots are zero-initialized like everything else. `@zeroed` is not needed. |
| Library mounting | `--lib <Name>=<dir>` (repeatable) on the CLI and a `lilac.libraries` LSP setting: a mounted library is a top-level namespace `<Name>`. Plus a relative form: `using *.C` imports the `C` subfolder of the importing file's own namespace, so a library can name its own folders without knowing where it is mounted. |
| Field drops | Objects **always** drop their fields. A parent's `@on_drop` runs first, then each field's drop (recursively). Enums drop their live payload. A type only manages its own allocations. |
| Field copies | Derived from the copy rule: a type with `@on_copy` builds its copy by hand (reading `self.field` is a place read, so field hooks fire automatically); a type without `@on_copy` copies field by field, calling each field's `@on_copy`. |
| Conditions | Exactly `@bit8`. Anything else goes through one `@conversion` to `@bit8` (so `Bool` works). |
| Match patterns | Arms accept `.Variant` and `Enum.Variant` (and `Enum[T].Variant`); a qualified pattern must name the scrutinee's enum. |
| Enum tags | The tag is the smallest `@bitN` that holds the variant count (`@bit8` up to 256 variants, then `@bit16`, …). |


## Ground rules

- Every phase ends with the `AGENTS.md` gates green
  (`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`).
- Every bug fix lands with a regression test that **uses** the feature, not
  just declares it (the root cause of most missed bugs: e.g.
  `hook_index_int64_checks_clean` only declares hooks with an empty `main`).
- `.lir`/`.ll` snapshot changes are reviewed, not blindly re-blessed; each
  phase notes which snapshots it is expected to change.
- Phases 1 and 6 are mostly independent; 2 → 3 → 4 → 5 → 7 are ordered.

---

## Phase 0 — Test harness that runs programs

**Why:** 635 tests pass while operators, conversions, `=>` methods and RAII
are broken. We need tests that execute.

- **0.1 Run tests.** New `crates/lilac-cli/tests/run.rs`: for each
  `tests/run/*.lilac`, `lilac emit-llvm` → `clang` (from
  `LLVM_SYS_181_PREFIX/bin`) → execute → compare stdout and exit code against
  `// expect-stdout:` / `// expect-exit:` header comments. Skip with a
  message if clang is absent. Output via `extern putchar`/`exit`.
- **0.2 Repro corpus.** Add every repro from the review as a test, marked
  `#[ignore = "phase N"]` until its phase lands (un-ignore in that phase):
  `if x { y = 1 }`, `=>` operator/conversion/method/static, member `@add`,
  member `@negate`, `Bool` condition, generic enum, `ref` param into an
  intrinsic, `ref self.field`, nested field store, `instance` in a static
  method, `"\xFF"` length, double evaluation through `=>`, loop-scoped drop,
  `@size_of(@bit32)`, `loop if … else break`, value-`if` with `-x`/`.A`.
- **0.3 Stdlib test.** Mark `standard_library_checks_and_emits` ignored until
  Phase 7 (it cannot pass while `lib/Standard` is the root, see 4.12).

**Exit:** harness runs `tests/run/hello.lilac`; the repro corpus exists.

---

## Phase 1 — Lexer & parser

Small, independent, unblocks everything else.

- **1.1 Struct-literal restriction bug.** Delete the "struct literal not
  allowed here" branch in `parse_postfix` (`crates/lilac-parse/src/lib.rs:1447`).
  With `allow_struct_lit == false`, `{` always ends the expression. Test:
  `if x { y = 0x1 }`, `loop if x { x = 0x0 } else break`, `match x { ... }`.
- **1.2 `else <stmt>` and `loop <stmt>`.**
  - AST: `Stmt::If { els: Option<Box<Stmt>> }`; `Stmt::Loop { body: Box<Stmt> }`
    (drop `Loop.cond`/`Loop.els`); new `Stmt::Block(Block)` so `{ ... }` is a
    statement.
  - Parser: `if <cond> <block> [else <stmt>]`; `loop <stmt>`.
  - Typeck: `check_loop` shrinks to "check body with `loop_depth + 1`" (the
    conditional-loop desugar disappears; `loop if` is just a loop whose body
    is an `if`).
- **1.3 Value `if c then x else y`.** Lexer: `then` keyword. `parse_if_value`:
  condition with struct literals disabled, expect `then`, then-expr, `else`,
  else-expr. Update fixtures using the old form (e.g. the driver test
  `generic_inline_control_flow_inlines`).
- **1.4 Generic arguments in expression position.** `Name[args]` may be
  followed by `(` (generic call), `.` (static member / variant), or `{`
  (struct literal, when allowed). Suggested AST:
  `Expr::StructLit { ty: Type, fields }` (carries generic args) and a new
  `Expr::TypeApp { name, args, span }` for `Name[args]` receivers;
  `Call.generic_args` stays for `f[T](...)` / `obj.f[T](...)`.
  Tests: `Box[@bit32] { ... }`, `G[@bit32].make(0x1)`, `CAllocator.create[T]()`.
- **1.5 Declaration shorthands.** In `parse_let_or_expr_stmt`, after
  `name: Type`: `{` → `Let { value: StructLit { ty: Type, .. } }`;
  `(` → `Let { value: Call { callee: Type as expr, args } }`. Same in
  `parse_item_tail` for top-level constants. Remove the "expected `=` after
  variable type" special error.
- **1.6 Enum bodies with members.** In `parse_enum_variants`: a member whose
  type is `function` **and** has a body (`{` or `=>`), or whose name starts
  with `@`, is an item (method/hook); anything else is a variant. AST:
  `ItemKind::Enum { variants, members: Vec<Item>, generic_params }`.
- **1.7 Qualified patterns.** Match arms accept `.Variant`, `Enum.Variant`,
  `Enum[T].Variant` (`MatchPattern::Tag { qualifier: Option<Type>, name }`).
  Typeck: the qualifier must be the scrutinee's enum (`E010` otherwise); both
  forms lower identically.
- **1.7b Relative `using`.** `using *.C` (and `using *.A.B`): `*` is the
  importing file's own namespace. The parser keeps the `*` marker; the module
  loader resolves it against the file's namespace path.
- **1.8 String literals are bytes.** `TokenKind::StringLit` and
  `FStrMid` carry `Vec<u8>`; `\xHH` is one raw byte. `Expr::Str(Vec<u8>)`,
  `HirExprKind::Str { bytes }`, `LirDataKind::Str(Vec<u8>)`; the LIR printer
  escapes non-printable bytes as `\xHH`. Char literals keep `char`.
  Test: `@byte_buffer_len("\xFF") == 1`.
- **1.9 Tidy.** Merge `starts_expr`/`starts_expression`. Replace the
  `Type.name == "function"` / empty-name sentinels with
  `enum TypeExpr { Named { name, args, .. }, Func(FuncType), Error }`.

**Spec:** §4 (`else <stmt>`, `loop <stmt>`, value `if … then … else`), §5
(shorthands; delete the contradictory "`:`-without-`=` is E001" sentence),
§6 (enum members), §2 lexical grammar (`\xHH` is a byte in strings).

---

## Phase 2 — One pipeline, one definition table

Foundation for Phase 3. Pure refactor: no snapshot changes expected.

- **2.1 Single pipeline.** Single-file mode becomes a one-file project via an
  in-memory `Fs` (the file is the only entry in the root). Delete
  `Session::run_pipeline`; `compile_source`/`compile_lir*` call the project
  path. In `lilac-resolve` delete `collect`, `Module`, `dedup_single_hooks`,
  `single_hook_key`, `Collector::same_sig`, the hook branch of
  `insert_overload`, and `ResolvedProject::single`. Snapshots must stay
  byte-identical (the root namespace adds no prefix).
- **2.2 Definition table.** In `lilac-resolve`:
  ```rust
  pub struct Def {
      pub name: String,
      pub kind: DefKind,          // Func | Hook | Type(Object|Enum) | Field | Const | Variant
      pub parent: Option<DefId>,  // owning object/enum
      pub ns: NsId,
      pub file: FileId,
      pub span: Span,
      pub vis: Visibility,
      pub generic_params: Vec<String>,
      pub ast: AstPath,           // (file index, item index path) back into the Program
  }
  pub struct Defs(Vec<Def>);      // indexed by DefId
  ```
  `FileScope.visible: HashMap<String, Lookup<DefId>>`; `files` indexed by
  `FileId`. Members are `HashMap<String, DefId>` on the type's `Def`. Hooks:
  `HashMap<String, Vec<DefId>>`. Then delete the linear-scan helpers:
  `find_item`, `find_by_def`, `res_item`, `function_name`, `member_name`,
  `object_name_of`, `object_params`, `find_parent_name`, `method_object`,
  `qualify_local`, `flat_member_pairs`, and `ResItem` cloning into scopes.
- **2.3 Hook duplicate check by resolved signature.** Move hook `E020` out of
  `lilac-resolve` (string keys: `ty_key`, `sig_key`, `TypeResolver`,
  `same_sig_resolved`, `add_hook`) into `collect_sigs`, comparing `FuncSig`s
  of `TypeId`s. One check, no string keys.
- **2.4 Remove the `instance` rename.** The receiver is `self` in HIR and LIR
  (or a reserved `.lilac.self`, which cannot collide). Static methods have no
  receiver: delete the implicit `instance` binding in `func_ctx`
  (`check.rs:1660`) and the lazy zeroed `instance` alloca in `lower_ident`
  (`lilac-lower/src/lib.rs:1280`). Tests: `instance` in a static method is
  `E021`; a local named `instance` in a method works.

**Exit:** all existing tests green, snapshots unchanged, `check.rs` and
`generics.rs` no longer scan `all_items()`.

---

## Phase 3 — Calls, symbols, and `=>`

Fixes stdlib blockers 2–4 (operators, conversions, member hooks, `@negate`,
`=>` methods) and the double evaluation.

- **3.1 `=>` is an ordinary function.** Check `=> expr` as `{ out = expr }`
  and emit a normal `HirFunc` with `inline: true`; `LirFunc.inline` → LLVM
  `alwaysinline` (honored at `-O0` too). Arguments are evaluated once.
  Delete: `InlineBody`, `CheckedBody::Inline`, `inline_bodies`,
  `instance_inline`, `check_inline_call`, `expand_inline_body`,
  `subst_ident`/`subst_block`/`subst_stmts`/`subst_stmt`,
  `rename_temps_*`, `collect_*temps`, the `in_progress` recursion guard and
  its `E022`. Constant folding of `=>` calls with constant arguments now
  comes from the existing LIR call evaluator (`lilac-const`), so
  `@compile_error` behind a `=>` predicate still folds.
- **3.2 Callees carry identity, not names.** `HirCallee::Func { def, args:
  Vec<TypeRef> }` (args empty unless generic) | `Local(LocalId)` |
  `Intrinsic(Intrinsic)`. `HirFunc` carries `def` + `args`, not `name`. One
  `fn symbol(defs, types, def, args) -> String` in `lilac-lower` produces
  every LIR name (namespace prefix, owner, hook signature suffix, instance
  args), keeping today's spelling so snapshots barely move. Delete
  `hook_name`, `instance_name`, `already_prefixed`, and the
  `func.name.rsplit_once('.')` member detection. Layout, tag and drop tables
  are keyed by `TypeId`, not display strings (`HirProgram.layouts`,
  `drops`).
- **3.3 One callee-resolution path.** Replace the ~12 parallel paths
  (`check_named_call`, `check_static_call`, `check_static_call_ty`,
  `check_instance_static_call`, `check_bare_static_call`,
  `check_generic_param_static`, `check_member_call`,
  `check_instance_member_call`, `resolve_method`, `check_factory_call`,
  `check_hook_call`, `check_tag_call`) with:
  ```rust
  fn resolve_callee(&mut self, callee: &Expr, generic_args: &[TypeExpr], expected) -> Callee
  struct Callee { def: DefId, args: Vec<TypeRef>, sig: FuncSig, receiver: Option<HirExpr>, kind: Plain|Variant|Init }
  fn check_call_args(&mut self, callee: Callee, args: &[Expr]) -> CheckedCall
  ```
  Rule for `@name(...)`: an intrinsic if the table knows it, otherwise the
  unit-global hook table (scope-independent, as the spec says). Unary `-`
  becomes `Expr::Unary` and resolves `@negate` through the hook table exactly
  like binary operators. `if f() -> x: T` and `match` captures share one
  capture binder.
- **3.4 Split `check.rs`.** `check/{mod, stmts, exprs, calls, literals,
  control, places, fstring, generics}.rs`. Add `TypeRef::ERROR` and
  `Checker::poison(span)` (the poison tuple is repeated ~40 times).

**Tests un-ignored:** `=>` operator/conversion/method/static, member `@add`,
member `@negate`, double evaluation (`twice(g(ref c))` calls `g` once).
**Snapshots:** inline fixtures change (calls instead of substituted bodies).

---

## Phase 4 — Type-system features and spec conformance

- **4.1 Conditions are `@bit8`.** `check_cond` (and `if`-capture/`loop`
  conditions, `and`/`or` operands) expect exactly `@bit8`; any other type
  takes one `@conversion` to `@bit8`. Wider `@bitN` no longer test directly
  (update `if_any_bit_width_ok` and fixtures that branch on `@bit32`). Test:
  `if t` with `t: Bool`; `if a < b` on `Int32`. Stdlib note: a `@bitN → @bit8`
  conversion meant for conditions must test non-zero, not truncate (a
  truncating one makes `0x100` false).
- **4.2 Generic enums.** Resolver keeps enum `generic_params`;
  `resolve_applied` accepts enums; enum instances get per-instance layouts
  like objects (`instantiate_object` generalized to `instantiate_type`).
  Consider merging `ResKind::Object`/`Enum` into one `Type` def with a
  `shape: Object | Enum { variants }` field. Tag width is the smallest
  `@bitN` holding the variant count (one helper used by layout, lowering of
  tags/tests, and `@size_of`).
- **4.3 Enum members.** Methods and hooks on enums behave exactly like object
  members (same resolution path from 3.3). Variant and member with the same
  name is `E020`.
- **4.4 Qualified variants.** `Enum.Variant`, `Enum.Variant(p)`,
  `Enum[T].Variant(p)` resolve through `resolve_callee` (`kind: Variant`).
  `.Variant` infers from the expected type; bare generic `Optional.Some(x)`
  takes its arguments from the expected type, else `E044`.
- **4.5 Generic struct literals.** `Name[T] { }` explicit; `Name { }` infers
  from the expected type, else the existing "cannot infer" `E023`.
- **4.6 Hooks in generic bodies.** Monomorphize **every** member hook per
  instance (today only `@on_drop`: `generics.rs:141`) and register concrete
  operators/conversions into the hook table at instantiation (`Sigs` becomes
  mutable inside the checker).
- **4.7 `@size_of`.** Accepts a type (`@size_of(@bit32)`, `@size_of(T)`,
  `@size_of(Box[T])`) or a value (its type). Sizes/alignments live in
  `lilac-target` (`Target::size_align`), computed with C layout rules that
  match LLVM's datalayout; lowering emits a `const`. Delete
  `LirInst::FoldIntrinsic` and `eval_size_of`. Test: a codegen test that
  compares our size against LLVM `TargetData` for builtins, a mixed object,
  and an enum, on x86_64 and riscv32. Result is `@bit_native` at the target
  width.
- **4.8 Explicit dereference.** Whenever a `ref T` value flows into a `T`
  slot, typeck inserts `HirExprKind::Deref`. Lowering handles `Deref` in one
  place (a `load`) and drops the ad-hoc deref logic in `store_operand`.
  Fixes the `r = @i_add_8(r, 0x1)` ICE. Verifier: integer `Arith` operands
  must match the result type (comparisons: operands equal, result `#bit8`)
  (`lilac-ir/src/lib.rs:1503`).
- **4.9 Field places.** `ref a.b.c`, `a.b.c = v`, `ref self.field`: lower
  member chains as field-address chains (`FieldAddr` on a `ref`, `layout_key`
  everywhere). Removes the two `E030` "later milestone" paths and the ICE in
  `lower_ref_of`.
- **4.10 Transparent extern wrappers.** An object with exactly one field of a
  C ABI type passes as that field in `extern` declarations and calls
  (lowering reads/writes field 0). `CInt`, `CLong`, `Float64`, `FILE` then
  work in `Standard.C`.
- **4.11 Target-correct function values.** Function values lower to
  `#bit_native`/`ptr`, not `#bit64` (`lilac-lower/src/lib.rs:192`).
- **4.12 Library mounting.** `--lib Name=dir`; the module loader resolves
  `using Name.X` to `dir/X` for mounted names before the root, and
  `using *.X` relative to the importing file's namespace (1.7b). LSP gets a
  `lilac.libraries` setting. Update `AGENTS.md` and `SPEC.md` §6.
- **4.13 Drop E027.** Remove the slot sweep in `check_func_body`, the code
  from `docs/errors`, and its tests.
- **4.14 New intrinsics** (via the table in 6.4): float
  `@f_less_than_or_equal_W` / `@f_greater_than_or_equal_W` (NaN-correct;
  `(lhs == rhs) or lt` is wrong for `-0.0` and NaN), and integer
  `le/ge` (+ `_u`) for symmetry.

**Spec:** §1/§3 (`@size_of` semantics, new intrinsics), §4 (conditions are
`@bit8` or convert to it), §6 (generic enums, enum members, qualified
variants and patterns, tag width, library mounting and `using *.X`), §8
(field drop/copy glue), §7 (all hooks monomorphize; generic literal
forms), §9 (transparent wrappers in `extern`), §2 (delete f-string rule 2;
rewrite "Folding runs after `=>` substitution" to "`=>` calls fold like any
call with constant arguments").

---

## Phase 5 — Scopes and RAII

Implements the ownership model decided above.

- **5.1 Block scopes and local ids.** `FuncCtx` holds a scope stack; `Let`
  binds in the innermost scope (same-scope redeclaration `E020`, nested
  shadowing allowed). HIR locals become `LocalId`s with a per-function local
  table (`HirFunc.locals: Vec<{name, ty, span}>`); `HirExprKind::Local(id)`
  replaces name-based `Ident` for locals. Lowering drops `bind_place` /
  `counts` name juggling.
- **5.2 Copy vs move.** Typeck marks every read that copies out of a place
  (local, field, deref) into a new place (let, assign, by-value argument,
  field/payload initializer, return-slot assignment) as
  `HirExprKind::Copy { place, hook }` when the type has `@on_copy` (or copy
  glue, open question 2). Struct/variant literals, call results and temps
  move. Box's `to = Box[T] { value = self.value }` therefore never calls
  `Box.@on_copy`.
- **5.3 Assignment.** `a = v`: evaluate `v` (copy or move) into a temp; call
  `@on_override(ref a, ref tmp)` if present; drop `a` (`@on_drop` / glue);
  bitwise-store `tmp` into `a` (the temp is not dropped).
- **5.4 Drops at scope exit.** Lowering keeps a scope stack of owned places:
  drop in **reverse** declaration order at block exit; `return` drops every
  open scope; `break` drops scopes up to the loop; a `Let` inside a loop is
  dropped every iteration. Temps of dropping types that are not moved
  elsewhere drop at the end of their statement. By-value parameters are
  owned by the callee and dropped on exit; return slots move to the caller.
- **5.5 Glue.** Generated per type, recursive, skipped when nothing in the
  type needs it. Drop glue: the type's own `@on_drop` (if any), then every
  field's drop in declaration order; enums drop the live payload (switch on
  the tag). Copy glue: if the type has no `@on_copy`, copy field by field and
  call each field's `@on_copy`; if it has one, call only that (field hooks
  fire through the place reads inside it).

**Tests (run harness):** a counting `@on_drop`/`@on_copy`/`@on_override`
object across loops, early `return`, `break`, nested blocks, by-value
arguments, returns, field assignment, and the Box move idiom.

---

## Phase 6 — Simplification and cleanup

Can be interleaved; each item is independent.

- **6.1 `@compile_error` liveness.** Keep the LIR dataflow folder and the
  call evaluator (3.1 relies on it). Spec the rule honestly: a
  `@compile_error` is live unless folding proves its block dead. When a
  deciding condition could not be evaluated (fuel/depth), `E031` adds a note
  saying so, so the failure is explicit rather than silent.
- **6.2 Literal folding.** Keep the semantics; dedupe
  `check_float_literal`/`type_folded_value` and the int/negative variants
  into one `type_literal(value, expected)`; move to `check/literals.rs`.
- **6.3 Diagnostics without string parsing.** Delete the message-parsing
  helpers in `generics.rs` (`lookup_miss`, `conversion_pair`, `arity_pair`,
  `missing_member`, `requirement_helps`, the `msg.starts_with` test in
  `is_requirement_failure`). If a help is worth keeping, attach structured
  data to `Diagnostic` (e.g. `Diagnostic.data: Option<DiagData>`). Keep the
  structurally-built required-signature note.
- **6.4 Intrinsic table.** One table (in `lilac-ir`, which both frontend and
  backend already depend on): `name → { params, result: Fixed | Expected,
  lowering: LirOp | RuntimeCall | Const }`. Typeck's `check_intrinsic` and
  lowering's `lower_intrinsic` both read it; `arith_op_for`,
  `split_numeric_intrinsic`, `negate_op_for`, `is_bitcast_intrinsic`,
  `runtime_callee` disappear. Adding an intrinsic = one table row (+ IR op
  and backend only if the op is new). Bitwise/shift intrinsics
  (`STDLIB.md` §5) become a table exercise.
- **6.5 Dead and stale code.** `LirDataKind::NumLit` and the `#byte_buffer`
  branch of `lower_number`; duplicate local lookup in `lower_ident`;
  `lilac-build`/`lilac-package` stubs (delete or fold into the CLI when the
  manifest lands); 55 stale milestone comments ("real checker lands in M1",
  "lands in M3"); every remaining `E030` "later milestone" path becomes
  either implemented or a real error code; 37 unreferenced legacy
  `tests/*.lilac` (some use the removed `union`); archive
  `docs/superpowers` (944 KB) outside the main tree; `Session.analysis`
  cloned only when `retain_partial_analysis` is set.
- **6.6 Typed LIR (optional, last).** A small `LirTy` enum
  (`Bit(u8) | BitNative | FBit(u8) | Ref(Box) | Named(String) | ...`) with
  the same text form. Removes `"ref "` prefix parsing and string compares in
  the verifier, const folder and backend.

---

## Phase 7 — Standard library bring-up

- Mount with `--lib Standard=lib/Standard`; the stdlib test checks a tiny
  root project that `using`s every `Standard` namespace and **uses** each
  type (operators, conversions, comparisons in conditions, `Optional`
  methods, `Box` create/move/drop, `Const`).
- Stdlib-side fixes found in review: `@sizeof` → `@size_of`; `Box` uses
  `self.ptr` (no such field) and `Optional.Some(ptr)` → `.Some(ptr)` or
  `Optional[ref T].Some(ptr)`; `Box.move` → `to = Box[T] { value =
  self.value }` then `self.value = .None`; drop `@zeroed`; float `<=`/`>=`
  via the new intrinsics; extern signatures (`setvbuf` arity, `freopen`
  takes path/mode/stream, `modf` takes `ref Float64`, `fputc` takes `CInt`,
  `fopen` returns `ref FILE`-style pointer).
- Run tests for each numeric type's operators and conversions on x86_64.

**Exit:** `standard_library_checks_and_emits` un-ignored and green; the
stdlib run tests pass.

---

## What gets deleted (rough tally)

| Area | ~Lines |
|---|---|
| HIR `=>` inlining machinery (3.1) | 600 |
| Parallel call paths collapsed (3.3) | 800 |
| Single-file pipeline + string hook keys (2.1, 2.3) | 300 |
| DefId linear-scan helpers (2.2) | 250 |
| Diagnostic message parsing (6.3) | 200 |
| Literal-fold duplication (6.2) | 200 |
| Intrinsic name re-parsing in lowering (6.4) | 120 |
| Dead/stale code (6.5) | 100+ |

Roughly 2.5k lines out of ~22k production lines, while adding block scopes,
RAII copy/override, generic enums, enum members and `@size_of`.
