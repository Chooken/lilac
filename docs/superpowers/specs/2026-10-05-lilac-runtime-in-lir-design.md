# Runtime in the IR: one portable runtime for all backends

**Status:** draft for review
**Date:** 2026-10-05
**Supersedes:** the in-module LLVM runtime section of
`2026-10-04-lilac-rust-rewrite-m5b-design.md` (§3.3) and its fixed
runtime-symbol contract.

## 1. Motivation

M5b put the builtin runtime (`@byte_buffer_*`, `@format_args_*`, `@load_u8`,
`@store_u8`, `@memcpy`, `@lilac_abort`) into each module as hand-written
inkwell functions in `lilac-codegen-llvm`. That has two costs:

1. **No sharing across backends.** M5c's second backend must re-implement all
   eleven functions.
2. **Dead runtime everywhere.** `Lower::run` emits every runtime symbol into
   every module, even when the program calls none of them.

This design moves the runtime into **ordinary LIR functions** so it is written
once, shared by every backend, and culled for free by the existing LIR
reachability pass (`lilac_ir::dce::prune_unreachable`). Only a few
genuinely target-specific **leaf intrinsics** stay backend-defined.

The change also closes a latent naming hazard: generated names currently use
`_`/`__`, but Lilac identifiers may contain `_`, so mangled generic instances
are not collision-proof. We switch the compiler's generated-name grammar to a
reserved character (`.`) that identifiers cannot contain, and add a verifier
guard.

## 2. Goals and non-goals

**Goals**

- The runtime logic lives in LIR, once, for all backends.
- Unused runtime functions are removed by DCE; used ones are emitted.
- Backends define only leaf intrinsics, not the runtime's control flow.
- Generated function/type names are collision-proof against user identifiers.
- The IR verifier rejects duplicate function names.

**Non-goals**

- Adding a general pointer-arithmetic or dynamic-GEP *instruction* to LIR.
  Dynamic access is expressed with the existing `Load`/`Store` through a `ref`
  place plus one leaf (`@ptr_add`).
- A LIR text parser. The runtime prelude is built programmatically.
- Changing user-visible Lilac syntax or diagnostics beyond the reserved-name
  namespace (see §6).

## 3. Runtime as LIR

### 3.1 LIR runtime functions

Seven functions, written as ordinary `LirFunc`s using the standard pointer
place ABI (every place by address; `define void @f(ptr …)`):

| function | signature (pointer ABI) |
|---|---|
| `.lilac.byte_buffer_from` | `in ref #bit8 %data`, `in #bitNative %len`, `out #byte_buffer %out` |
| `.lilac.byte_buffer_ptr` | `in #byte_buffer %buffer`, `out ref #bit8 %out` |
| `.lilac.byte_buffer_len` | `in #byte_buffer %buffer`, `out #bitNative %out` |
| `.lilac.format_args_new` | `out #format_args %out` |
| `.lilac.format_args_push` | `in ref #format_args %table`, `in #byte_buffer %piece` |
| `.lilac.format_args_count` | `in #format_args %table`, `out #bitNative %out` |
| `.lilac.format_args_piece` | `in #format_args %table`, `in #bitNative %index`, `out #byte_buffer %out` |

They are built by a new `lilac_ir::runtime` module (a builder, not text) and
appended to the module by the driver (see §7). Bodies use `alloca`, `field`,
`store`, `copy`, `arith`, `branch`, and the leaves below.

### 3.2 Leaf intrinsics

Only three operations stay backend-defined, as `LirCallee::Builtin`:

| builtin | signature | LLVM lowering |
|---|---|---|
| `@ptr_add` | `(ref #bit8, #bitNative) -> ref #bit8` | `ptrtoint` + `add` + `inttoptr` (safe; no GEP) |
| `@memcpy` | `(ref #bit8, ref #bit8, #bitNative)` | `llvm.memcpy` |
| `@lilac_abort` | `() -> ()` | `llvm.trap` + `unreachable` |

`@ptr_add` is the one new primitive. `@load_u8`/`@store_u8` become LIR (a
`@ptr_add` plus a `ref #bit8` load/store), so they are no longer leaves.

### 3.3 Call sites

`lilac-lower` emits `LirCallee::Func(".lilac.<name>")` for the seven runtime
functions (and for the former `@load_u8`/`@store_u8`). The three leaves stay
`LirCallee::Builtin`. `builtin_sig` shrinks to `@ptr_add`, `@memcpy`, and
`@lilac_abort` (the last is called only by the prelude, but still needs a
signature for the verifier).

## 4. Aggregate representation

`#byte_buffer` and `#format_args` remain **builtin types** (not entries in
`module.layouts`), but the verifier learns their field layouts so LIR can
`field`/`field.addr`/`store` them. Keeping them out of `module.layouts` avoids
printing two type sections into every `.lir` snapshot.

```
#byte_buffer { len: #bitNative, data: ref #bit8 }
#format_args { count: #bitNative, p0: #byte_buffer, …, p15: #byte_buffer }
```

LIR has no array type, so the fixed 16-piece table is sixteen named fields.
`format_args_piece`/`format_args_push` compute a dynamic slot as
`field.addr .p0 + index * stride`, where `stride` is `#byte_buffer`'s size
(`2 × pointer width`). The prelude builder takes the target pointer width and
embeds that constant (`16` on 64-bit targets, `8` on 32-bit).

A `builtin_layout(ty) -> Option<&[(name, ty)]>` helper in `lilac-ir` backs both
the verifier's `check_field_layout` and the backend's type construction. Codegen
builds the same structs (array → sixteen fields for `#format_args`).

## 5. Dynamic typed access

To read/write `pieces[index]` (and a byte at `base + offset`), the prelude:

1. computes the slot address with `@ptr_add` (a `ref #bit8` register),
2. `copy`s that register into a `ref #T` place, then
3. uses the existing `Load`/`Store`, which dereference a `ref` place.

Step 2 requires the verifier's `CopyPlace` type rule to treat references as
compatible: **allow `ref X` ↔ `ref Y`** (LLVM pointers are opaque); `ref` ↔
value mismatches stay rejected, preserving the C1 guard.

## 6. Naming

### 6.1 Reserved namespace

`.` is the compiler's reserved character: identifiers are
`[A-Za-z_][A-Za-z0-9_]*`, so no user name contains `.`. Every
compiler-generated symbol/segment is therefore unambiguous:

- Runtime functions and other compiler symbols: `.lilac.<name>`
  (`.lilac.byte_buffer_from`, `.lilac.main`, `.lilac.memcpy`,
  `.lilac.abort`, `.lilac.ptr_add`).
- Builtin type mangles: leading `.` (`.bit8`, `.byte_buffer`).

LLVM 18 prints `.`-leading and dotted names unquoted (verified with
`llvm-as`/`llvm-dis`).

### 6.2 Mangling overhaul

`lilac_ty::mangle` and the typeck function-instance naming replace `_`/`__`
with `.`:

| item | old | new |
|---|---|---|
| builtin type | `bit32` | `.bit32` |
| reference | `ref_bit32` | `ref..bit32` |
| instance | `GenObj__1__bit32` | `GenObj.1..bit32` |
| func type | `function__2__bit32__bit8__returns__0` | `function.2..bit32..bit8.returns.0` |
| param | `param0` | `param.0` |
| method | `GenObject__1__bit32.get__1__bit64` | `GenObject.1..bit32.get.1..bit64` |
| function instance | `gen__1__Object` | `gen.1.Object` |

Builtin types are dot-marked so a user object literally named `bit32` cannot
collide with `@bit32`. `owner_members` (`check/generics.rs`) splits on `.`
instead of `__`.

### 6.3 Verifier guard

`Verifier::run` gains a duplicate-function-name check: two `LirFunc`s with the
same name are `E070`. This is defense-in-depth against any residual mangle
collision and against user/prelude name clashes.

## 7. Injection and DCE

`lilac-driver::run_pipeline` appends the prelude (built for the session target's
pointer width) **between `fold` and `prune_unreachable`**. `prune_unreachable`
already follows `LirCallee::Func`/`addr.func` edges, so unused runtime functions
are dropped and used ones survive, in deterministic order (user functions, then
the surviving runtime functions in prelude order). A module with no `main` is
unchanged by DCE, so the prelude survives whole — no regression from today.

Injecting after `fold` means the const interpreter runs while the runtime
bodies are still absent; it therefore matches the runtime names *before* any
body lookup (§8), and a folded-away call is a non-issue.

## 8. Const interpreter

`lilac-const` keeps name-keyed shortcuts so `@compile_error` const evaluation is
unchanged: when a `LirCallee::Func` call names one of the foldable runtime
functions (`.lilac.byte_buffer_len`, `.lilac.byte_buffer_from`,
`.lilac.byte_buffer_ptr`, `.lilac.load_u8`, `.lilac.store_u8`), or a
`LirCallee::Builtin("@memcpy")` call, the interpreter evaluates it over its
const-bytes memory model **before** looking up a body. It does not interpret the
new LIR bodies.

## 9. Backend

- `runtime.rs` shrinks to the three leaves (`@ptr_add`, `@memcpy` →
  `llvm.memcpy`, `@lilac_abort` → `llvm.trap`).
- The seven runtime functions lower through the normal function path (pointer
  ABI); no value-ABI builtin special case remains for them.
- `lower_builtin_call` handles only the leaves.
- `#byte_buffer`/`#format_args` LLVM types are built from `builtin_layout`.

## 10. Versioning, snapshots, tests

- Bump `LILAC_IR_VERSION` `0.2 → 0.3`; update `IR_SPEC.md` (builtins, `copy`
  ref-compatibility, reserved names).
- Regenerate every `.lir` and `.ll` snapshot. Modules that use no runtime lose
  all runtime text (the main visible win); `string`/`fstring` gain the LIR
  runtime bodies; generic/drop-hook snapshots show `.`-mangled names.
- New tests: unused runtime absent / used runtime present; duplicate function
  name rejected (`E070`); `ref X` ↔ `ref Y` copy accepted, `ref` ↔ value still
  rejected; prelude builder unit tests; the existing M5b host compile+run
  harness stays green.
- New error doc `docs/errors/E053.md` **only if** we reserve builtin type names
  (see open questions); otherwise no new user-facing code.

## 11. Open questions

1. **Builtin type names.** This spec marks builtins in the mangle (`.bit32`)
   rather than reserving the names. Alternative: reject user objects named
   `bit8`/`byte_buffer`/… with a reserved-name diagnostic. Dot-marking is
   internal-only and needs no language change; confirm.
2. **Prelude authoring.** Programmatic builder in `lilac-ir` (chosen). A LIR
   text parser would be more pleasant to author but is a separate feature.
3. **`#format_args` shape.** Sixteen named fields (chosen) versus a
   pointer-to-array representation; the latter complicates by-value copies.

## 12. Task sketch (for the implementation plan)

1. `.`-mangling overhaul + `owner_members` split + mangle tests.
2. Verifier: duplicate-function check; ref↔ref `CopyPlace`; `builtin_layout`
   for `#byte_buffer`/`#format_args`.
3. `@ptr_add` builtin sig + LLVM leaf; shrink `builtin_sig`.
4. `lilac_ir::runtime` prelude builder (seven functions) + unit tests.
5. Driver injection before `prune_unreachable`; lower emits `.lilac.*` calls.
6. Const interpreter shortcuts for the runtime names.
7. Backend: delete moved runtime bodies; build aggregate types from
   `builtin_layout`.
8. Version bump, `IR_SPEC.md`, snapshot regeneration, integration tests.
