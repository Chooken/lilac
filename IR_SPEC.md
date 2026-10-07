# Lilac IR Specification (0.3)

> Version: `lilac_ir = "0.3"`. Status: normative for the Rust rewrite.
> `SPEC.md` is normative for language semantics; this file is normative for IR text,
> verification, and backend contract. Plan-only material lives in `RUST_REWRITE_PLAN.md`.

# Declaration Syntax
* ! = Constant in data section
* @ = Function
* \# = type
* % = mutable place (allocad variable)
* $ = Label (block)
* & = SSA register value within a block (`&n: #type`, defined once before use)
* `addr` / `addr.func` / `load` = explicit address and dereference operations.
  There is no bare `ref` modifier on operands.

# Sections

Data sections hold constant declarations. String literals and the source bytes
of decimal number literals lower to `data` consts (`!c: str = "..."` /
`!c: numlit = "..."`); a string literal is then read as a data-backed buffer
value with `const !c:#byte_buffer`, never a call to the
`.lilac.byte_buffer_from` runtime function. Hex/bin literals are `const`
registers typed directly, not data. f-strings lower to `@format_args`
construction plus conversions/calls and introduce no new instruction shapes
(see `SPEC.md`).
```lilac-ir
data {
    !c0: str = "String",
    !c1: numlit = "0.54",
}
```

String const bytes are UTF-8. `"` escapes as `\22` and `\` as `\5C`.
`{{` / `}}` from f-string source are already resolved to `{` / `}` before
lowering, so they never appear as escapes here.

Types are defined as like (members carry their field names in layout order;
sections print sorted by type name for deterministic snapshots):

```lilac-ir
type #Number {
    tag: #bit8,
    val: #bit32,
}
```
as produced by `tests/ui/match_enum.lir`.

Builtin aggregates usable in `#` positions are `#byte_buffer` and
`#format_args`. Both are plain values with no hidden behavior, and their field
layouts are fixed by the IR rather than declared in a `type` section:

```lilac-ir
#byte_buffer { len: #bit_native, data: ref #bit8 }
#format_args { count: #bit_native, p0: #byte_buffer, …, p15: #byte_buffer }
```

`#format_args` is a fixed-capacity inline piece table of exactly 16 pieces.
LIR has no array type, so the table is spelled out as sixteen named fields.
The verifier resolves `field`/`field.addr` against these layouts
(`lilac_ir::builtin_layout`); the backend builds matching structs.

Functions can be declared as:
```lilac-ir
lilac_ir = "0.3"
target = "x86_64-unknown-none"
function @func_name
    in #type %param_1,
    in #type %param_2,
    out #type %out_1,
    out #type %out_2
{
    $entry:
      return
}
```
`target` fixes `@bit_native` width. Every block starts with `$label:` and ends
with exactly one terminator (`return | branch | branch.if | panic | unreachable`).

Foreign symbols are declared without a body; calls to them verify like
ordinary calls (arity and `in/out` types from the declaration). Backends emit
native external calls; link libraries come from the package manifest.
```lilac-ir
extern @ext_name
    in #type %param_1,
    out #type %out_1
```

Labels can be defined inside function blocks:
```lilac-ir
{
    $entry:
    $loop:
}
```
Block parameters are not supported; values flow through `%places` and `&regs`.

# Monomorphized names

Verified IR is **concrete-only**: generic declarations and their type
parameters never appear. Each reached `(declaration, type arguments)`
instantiation is monomorphized to ordinary concrete items, and its symbols
carry a deterministic, collision-free, arity-tagged mangle:

`<Name>.<arity>.<arg1>.…`

Each argument is mangled recursively; builtins gain a leading `.`, `ref X`
becomes `ref..X`, and a nested instance is its own mangled form. Examples:
`gen_function.1..bit32`, `GenObject.1..bit32` (with methods such as
`GenObject.1..bit32.get`), and `GenObj.1.GenObj.1..bit32`. The arity tag
keeps `GenObj[GenObj[@bit32]]` distinct from a multi-argument application.
`.` is reserved: Lilac identifiers are `[A-Za-z_][A-Za-z0-9_]*`, so no user
name contains it and every generated segment is unambiguous. Builtin type
mangles gain a leading `.` (`.bit8`, `.byte_buffer`).
Object `type` layouts and RAII `drop` targets are keyed by the same mangled
name; no instruction carries a generic parameter type.

## Reserved names and compiler symbols

Every compiler-generated function or runtime symbol lives in the `.lilac.`
namespace: the LIR runtime (`.lilac.byte_buffer_from`, `.lilac.load_u8`,
`.lilac.format_args_push`, …), the entry point (`.lilac.main`), and the
backend's three leaf intrinsics (`.lilac.ptr_add`, `.lilac.memcpy`,
`.lilac.abort`). LLVM 18 prints `.`-leading and dotted names unquoted. The
verifier rejects two functions with the same name (`E070`), so a residual
mangle collision or a user/prelude clash cannot reach a backend.

A `drop %x` marker records that a place is destroyed; it does not itself run
the destructor. Where the type declares an `@on_drop`, the `call @…@on_drop`
that invokes it precedes the marker, so the hook observes live memory.

# Instructions

## Memory

### allocate 
* Type
* Variable (mutable place, always zero-initialized)

```lilac-ir
alloca #type %name : zeroed
```
Legacy text `allocate #, %` means the same as `alloca`.

### addr / addr.func / load
* `addr %var -> &r` takes a place address; `addr.func @f -> &r` takes a function
  address with pointee signature checked by the verifier; `load ref %x -> &r`
  dereferences a `ref` place.
```lilac-ir
addr %variable -> &1
addr.func @name -> &2
load ref %ptr -> &3
```

### copy / const
* `copy` moves between places and regs. Operands must share a type, except that
  two references are compatible under opaque pointers: `copy %a -> %b` with
  `ref X` -> `ref Y` is a pointer copy. A reference/value mix (`ref X` vs a
  value) and two distinct value types are rejected.
* `const` defines a typed register from a literal.
```lilac-ir
copy %a -> %b
copy &1 -> %b
&1: #bit32 = const 0x1:#bit32
```

### field / field stores / deref stores
* `field` loads `obj.name` (with layout index comment); a field store prints
  as `copy` into `%obj.name`; a store through a `ref` place prints as `copy`
  into `ref %dst` and verifies `dst` is a reference place.
```lilac-ir
&1: #bit32 = field %instance .member  // idx 0
copy &1 -> %tmp.1.member  // idx 0
copy &2 -> ref %tmp.3
```
as produced by `tests/ui/object_instance_function.lir`.

### conversions / field.addr
* `trunc` narrows and `extend.zero` widens bit containers; `field.addr`
  computes a field address like `field` but yields a register holding it.
```lilac-ir
&2: #bit8 = trunc &1
```
as produced by `tests/ui/conversion.lir`. `extend.zero` and `field.addr`
print symmetrically (`&3: #bit32 = extend.zero &2`,
`&4: #bit32 = field.addr %variable .member  // idx 0`); no UI snapshot
covers them yet.

## Control Flow

### call 
* Callee register holding a function address
* `in` then `out` operands in signature order; arity and types are verified
```lilac-ir
call &f (in %a) (out %b)
```

### builtin calls
* Callee is a `@`-builtin **leaf** name (`call @ptr_add (in %base, %off) (out %s)`)
* Only three target-specific leaves remain builtins, verified against the
  signature table below. They lower to the `.lilac.*` backend symbols shown.
  `@size_of` and `@compile_error` are fold-only markers, never builtin calls
  (see `compile.error`)

| builtin | in-args | out | LLVM lowering |
| --- | --- | --- | --- |
| `@ptr_add` | `ref #bit8`, `#bit_native` | `ref #bit8` | `ptrtoint` + `add` + `inttoptr` |
| `@memcpy` | `ref #bit8`, `ref #bit8`, `#bit_native` | — | `llvm.memcpy` (as `.lilac.memcpy`) |
| `@lilac_abort` | — | — | `llvm.trap` + `unreachable` (as `.lilac.abort`) |

The byte-buffer and format-args operations are **not** builtins: they are
ordinary LIR functions in the `.lilac.` namespace, built once by
`lilac_ir::runtime` and emitted through the normal function path.

| runtime function | in-args | out |
| --- | --- | --- |
| `.lilac.byte_buffer_from` | `ref #bit8`, `#bit_native` | `#byte_buffer` |
| `.lilac.byte_buffer_ptr` | `#byte_buffer` | `ref #bit8` |
| `.lilac.byte_buffer_len` | `#byte_buffer` | `#bit_native` |
| `.lilac.load_u8` | `ref #bit8`, `#bit_native` | `#bit8` |
| `.lilac.store_u8` | `ref #bit8`, `#bit_native`, `#bit8` | — |
| `.lilac.format_args_new` | — | `#format_args` |
| `.lilac.format_args_push` | `ref #format_args`, `#byte_buffer` | — |
| `.lilac.format_args_count` | `#format_args` | `#bit_native` |
| `.lilac.format_args_piece` | `#format_args`, `#bit_native` | `#byte_buffer` |

These functions call back into the `@ptr_add` and `@lilac_abort` leaves. The
driver's pass order is `fold` → inject the prelude → `prune_unreachable`, so
the runtime is present during pruning and dead-code elimination removes
whatever a program does not reach.

### branch 
* To: Label or register
```lilac-ir
branch [$, &]
```

### branch.if 
* Condition: variable or register
* True: label or register, 
* Optional Else: label or register
```lilac-ir
branch.if [%, &], [$, &], [$, &]?
```
A compile-time folding pass runs before verification. `branch.if` on a constant
condition is replaced with `branch` to the live target, so verified IR never
contains `branch.if` on a constant. Dead successors left unreachable are removed.
`=>` inline functions are substituted before this pass; remaining `call`s with
all-constant arguments to pure functions with bounded loops may additionally be folded,
otherwise they are kept as runtime calls.

### return
```lilac-ir
return
```
`return` requires every `out` place assigned at least once in its function
(textual rule, flow-insensitive).

### unreachable
```lilac-ir
unreachable
```
Explicit terminator for pruned `match` fallthrough and eliminated dead branches.

### panic
* Message: single `!const` byte string (printed as bytes, no `String` involved)
```lilac-ir
panic !msg
```

### compile.error
* Message: constant or variable
```lilac-ir
@compile_error(live)
```
Lowering emits one `@compile_error(live)` marker per `@compile_error(msg)`
call site. `compile.error` fires only if still live after constant folding and
global dead-code elimination (`E031`): a marker inside a branch eliminated by
folding, or inside a function unreachable from `main` or an `--export` root, is
dropped without diagnostic, so verified IR reaching a backend contains no marker
(`tests/ui/cfold_dead_compile_error.lir` shows the erased shape). A marker
that survives is a live error, diagnosed after reachability pruning — never
during verification (reaching the verifier is an internal error). The
fold-required marker `@size_of` is the same shape: the front-end emits a
pre-fold node, the folder consumes it to a plain `const` (or `E060`), and a
survivor reaching verification is an internal error, never a backend call.
`@size_of` folding is best-effort in M3 (operand-type semantics not yet pinned).

## Casts

### extend.zero 
* Type of From: type
* From: register
* Type of To: type
* To: variable or register
```lilac-ir
extend.zero #, &, #, [%, &]
```

### truncate 
* Type of From: type
* From: registor 
* Type of To: type
* To: variable or register
```lilac-ir
truncate #, &, #, [%, &]
```

## Field Access

### field 
* Parent place, field name (index + type shown as comment for readability)
```lilac-ir
field %obj .name -> &1  // idx 2, #bit32
```

## Comparisons
* ```cmp.eql #type, &a, &b -> &r```
* ```i.cmp.gt &a, &b -> &r``` / ```i.cmp.lt &a, &b -> &r```
* ```i.cmp.gt.u &a, &b -> &r``` / ```i.cmp.lt.u &a, &b -> &r``` (`icmp ugt`/`icmp ult`)
* ```f.cmp.gt &a, &b -> &r``` / ```f.cmp.lt &a, &b -> &r```
Result is `@bit8` (`0x0` false, otherwise true). Widths of inputs must match;
`i.*` vs `f.*` selects two's-complement vs IEEE-754 interpretation. The
unsigned `i.cmp.lt.u` entered with the runtime-in-LIR work; `i.cmp.gt.u`
completes the unsigned comparison set.

## Integer Arithmetic
* ```i.add &a, &b -> &r``` / ```i.sub``` / ```i.mul``` / ```i.div``` / ```i.mod```
* ```i.div.u &a, &b -> &r``` / ```i.mod.u &a, &b -> &r``` (`udiv`/`urem`)
All operands share one `#bit8/16/32/64/Native` width, carried on the result reg.

## Float Arithmetic
* ```f.add &a, &b -> &r``` / ```f.sub``` / ```f.mul``` / ```f.div``` / ```f.mod```
Same width rule as integer arithmetic.

## Bitwise Operations
* ```and &a, &b -> &r``` / ```or``` / ```xor``` / ```not &a -> &r```
* ```shift.l &a, &b -> &r``` / ```shift.r &a, &b -> &r```

## Literals (canonical text)
* Ints: `0x1:#bit32`, `0b101:#bit8`; width suffix is required in canonical text.
* Strings/numbers: `!c: str = "..."` / `!c: numlit = "10.5"` in `data`; a string
  literal is read with `&1: #byte_buffer = const !c` (data-backed buffer value).
* A fold-required marker `@size_of` on a constant operand never appears in
  verified IR — it folds to `const`.

## Verification (`lilac-ir::verify`, runs before any backend)
1. Header present with supported `lilac_ir` version; `target` resolves `@bit_native`.
2. All `!/@/#/%/$/&` names resolve; `&regs` defined once before use in block.
3. Every block ends in exactly one terminator; all branch targets exist.
4. `call` arity and `in/out` types match callee signature.
5. `field .name` exists with index in bounds and matching type comment.
6. Arithmetic/comparison widths match; `i.*`/`f.*` used on allowed widths only.
7. `return` only when all `out` places are definitely assigned.
8. No `branch.if` on a constant, no fold-required marker (`@size_of`) or
   `compile.error` survivor. Violations are user-facing
   `Diagnostic`s with HIR spans, never backend crashes.
9. Types are concrete only: no generic declaration and no type parameter
   survives; every applied declaration is monomorphized to a mangle-named item.