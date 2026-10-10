# Lilac Rust Rewrite — M0 Design (skeleton + errors + lex/parse)

> Scope: M0 only (plan `RUST_REWRITE_PLAN.md` §7). Normative language/ir docs:
> `SPEC.md` v0.2-draft, `IR_SPEC.md` v0.2. This file is the M0 design record.
> Decisions: root `Cargo.toml` workspace (coexists with Zig `src/` + `build.zig`),
> full crate scaffold per plan §3 with M1–M6 crates as compiling stubs,
> zero external deps in M0 (std only, no `inkwell` until M5).

## 1. Outcome / constraints / success

- Outcome: `lilac check hello.lilac` lexes + parses and renders one diagnostic
  correctly in human and JSON forms; `emit-ir --check` stub present.
- Constraints: (a) acyclic crate deps mirroring `rustc` stages;
  (b) frontend crates must not touch LLVM (`grep inkwell` gate);
  (c) no `unwrap/expect` on user input — `bug!()` only for ICEs;
  (d) `cargo fmt --check`, `clippy -D warnings`, `cargo test` green.
- Success: `cargo test` passes incl. `hello` UI snapshot; Zig `src/` untouched.

## 2. Architecture (plan §3, M0 slice)

```text
crates/
  lilac-span/ lilac-error/ lilac-lex/ lilac-ast/ lilac-parse/   # M0: real
  lilac-resolve/ lilac-ty/ lilac-hir/ lilac-typeck/             # M0: stubs
  lilac-ir/ lilac-const/ lilac-lower/                          # M0: stubs (+IR version const)
  lilac-codegen/ lilac-codegen-llvm/                           # M0: stubs (Backend trait only, no LLVM link)
  lilac-package/ lilac-build/ lilac-driver/ lilac-cli/         # driver real (thin), package/build/cli-stub real
```

Dep rule: `parse -> ast -> lex -> error -> span`; `driver -> parse + error + span`;
`cli -> driver`. Stub crates compile with no deps beyond `span`/`error` where needed.
`lilac-ir` exposes `pub const LILAC_IR_VERSION: &str = "0.2"` as the stability anchor.

## 3. Components (M0 behavior)

- `lilac-span`: `FileId(u32)`, `BytePos(u32)`, `Span{lo,hi,file}`, `SourceMap`
  (add_file/name/src/line_col/span_text), `Symbol(u32)` interner (`intern/resolve`).
- `lilac-error`: `Diagnostic{level,code,msg,labels,notes,helps}`, `Label{span,primary,msg}`,
  `Emitter` trait, `EmitterHuman` (file:line:col + snippet + labels/notes/helps),
  `EmitterJson` (hand-rolled, no serde), `Diags{emit/dedup/cap 10/has_errors}`,
  `ErrorGuaranteed(())`, `bug!()` macro (ICE + span + panic).
- `lilac-lex`: `TokenKind` (ident/number/string/char/keywords per SPEC §2 + symbols +
  `FStr{Start,Mid,End}` reserved), `Token{kind,span,text}`, `Lexer: Iterator<Item=Result<Token,LexError>>`.
  Numbers: `0x/0b`/dec/float with `_` separators; strings: `\" \\ \n \xHH` only;
  chars: exactly one scalar; anything else after `\` is a lex error diagnostic.
- `lilac-ast`: untyped `Program{items}`, `Item::{FuncDecl,ObjectDecl}`, minimal
  `Stmt/Expr` for `hello` (bindings, calls, literals); `Visitor` trait so new nodes
  touch one trait, not N switches.
- `lilac-parse`: recursive-descent `Parser` over `Vec<Token>` with recovery
  (on item error: emit diagnostic, skip to next `}`/newline boundary, continue).
  Every fallible node carries a `Span`.
- `lilac-driver`: `Target{arch}` (stringly for M0; `bit_native_width()` maps
  `x86_64 -> 64`), `Opts{verbose,error_format}`, `Session{source_map,diags,target,opts}`,
  `compile_source(name, src) -> Result<Program, ErrorGuaranteed>` (lex+parse; later
  milestones insert resolve/typeck/lower here).
- `lilac-cli`: `check <file>` (lex+parse, print diagnostics, exit 1 on error),
  `--error-format=human|json`, `emit-ir` (M0: prints `lilac_ir = "0.2"` header stub).

## 4. Data flow / error handling / testing

- Flow: `read file -> Session -> lex (recover) -> parse (recover) -> report`.
  No resolve/typeck in M0; unknown names are not errors yet.
- Errors: all user-input failures -> `Result<_, ErrorGuaranteed>`; poison propagates
  without new noise; `E0xx` codes reserved (`E000` lex, `E001` parse in M0);
  lints `W0xx` reserved.
- Tests: `crates/lilac-cli/tests/m0.rs` — `check` on `tests/ui/hello.lilac` exits 0;
  broken input exits 1 with `error\[E001\]` on stderr; unit tests for span line/col,
  lexer numbers/strings/escapes, parser recovery. `tests/ui/hello.lilac` is the M0 snapshot.

## 5. Self-review

- Placeholders: none — every type above exists in the scaffold.
- Consistency: header `lilac_ir = "0.2"` + `target` matches `IR_SPEC.md` v2 rules;
  `@bitNative` width comes from `Target`, never hardcoded.
- Scope: strictly M0; generics/IR-freeze/const-fold/LLVM explicitly deferred
  (stubs compile so the cutover is additive).
- Ambiguity: `emit-ir` in M0 prints only the header + `unimplemented` note —
  documented, not silent.

## 6. Legacy-suite sweep (2026-10-01, post-verify; M1/M2 backlog, NOT M0 scope)

`lilac check` over all 85 legacy `tests/*.lilac`: **39 parse, 46 fail**
(M0 exit criteria are unaffected (hello + gates green); the failures cluster
into six grammar gaps plus removed features (A and D fixed same-day, moving
the sweep to **46 parse** — flipped: `enum_implicit_member`, `generic_union`,
`match_enum`, `object_declaration`, `raii_on_drop_decl`,
`raii_on_override_decl`, `union_use`)):

- **A. Object fields — DONE 2026-10-01.** `member: @bit8` (no initializer)
  inside `object` (`SPEC.md` §6 shows `Member: @bit32` verbatim) now parses to
  `ItemKind::Field`; bare `name: Type` outside objects is still `E001`.
  Covered by `object_field_parses` + `tests/ui/object_fields.lilac`.
- **B. Struct literals — SPEC gap.** Two legacy spellings exist:
  `= Object { member = 0x1 }` and `: Object { value = 0x2 }` (no `=`).
  `SPEC.md`/`IR_SPEC.md` specify neither; needs a spec decision on the
  canonical form before implementing (M1).
- **C. Match literal arms.** `match 0x0 { 0x0 => {} }` — arms match byte
  patterns too (`SPEC.md` §4: "union tags and byte patterns"), but the parser
  only accepts `.Tag` arms.
- **D. Leading-dot tags — DONE 2026-10-01.** `number: Number = .One`,
  `tag = .Some` (`SPEC.md` §6 desugar) now parse to `Expr::Tag` (type comes
  from context at typeck). Covered by `tag_literal_parses`.
- **E. `and`/`or` infix.** `if 0x0 and 0x1` — lexed as idents today; needs
  keyword tokens + `BinOp` variants (+ spec patch: `SPEC.md` never defines
  them; presumably `@and`/`@or` desugar).
- **F. Function-typed locals.** `add: function (@bit32) @bit32 = addOne`
  needs function types in type position and as `let` types.
- **Removed:** `interface_*` tests exercise a deleted feature; M1 deletes them
  per plan §7. `FAILS_*` type-error tests must eventually fail with their
  documented `E0xx` codes at typeck, not `E001` at parse.

Recommended order: A (+ spec patch for B) with M1 types, C/D/E with M2
control-flow, F with M1 `function_types` port.
