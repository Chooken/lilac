# Lilac IR Specification (v2)

> Version: `lilac_ir = "0.2"`. Status: normative for the Rust rewrite.
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
value with `const !c:#byte_buffer`, never `@byte_buffer_from` (which stays a
builtin for user code). Hex/bin literals are `const` registers typed directly,
not data. f-strings lower to `@format_args` construction plus conversions/calls
and introduce no new instruction shapes (see `SPEC.md`).
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

Builtin aggregates usable in `#` positions: `#byte_buffer` (`{len: #bitNative,
ptr: #ptr}` view, borrows) and `#format_args` (fixed-capacity inline piece table,
at most 16 pieces). Both are plain values with no hidden behavior.

Functions can be declared as:
```lilac-ir
lilac_ir = "0.2"
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
`target` fixes `@bitNative` width. Every block starts with `$label:` and ends
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
name contains it and every generated segment is unambiguous.
Object `type` layouts and RAII `drop` targets are keyed by the same mangled
name; no instruction carries a generic parameter type.

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
* `copy` moves between places and regs of the same type; `const` defines a typed
  register from a literal.
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
* Callee is a `@`-builtin name (`call @byte_buffer_len (in %buf) (out %len)`)
* Verified against the builtin signature table below; `@as_int`, `@as_float`,
  `@size_of`, and `@compile_error` are fold-only markers, never builtin calls
  (see `compile.error`)

| builtin | in-args | out |
| --- | --- | --- |
| `@byte_buffer_from` | `ref #bit8`, `#bitNative` | `#byte_buffer` |
| `@byte_buffer_ptr` | `#byte_buffer` | `ref #bit8` |
| `@byte_buffer_len` | `#byte_buffer` | `#bitNative` |
| `@format_args_new` | — | `#format_args` |
| `@format_args_push` | `ref #format_args`, `#byte_buffer` | — |
| `@format_args_count` | `#format_args` | `#bitNative` |
| `@format_args_piece` | `#format_args`, `#bitNative` | `#byte_buffer` |
| `@load_u8` | `ref #bit8`, `#bitNative` | `#bit8` |
| `@store_u8` | `ref #bit8`, `#bitNative`, `#bit8` | — |
| `@memcpy` | `ref #bit8`, `ref #bit8`, `#bitNative` | — |

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
call site. `compile.error` fires only if still live after constant folding
(`E031`): a marker inside a branch eliminated by folding is dropped without
diagnostic, so verified IR reaching a backend contains no marker
(`tests/ui/cfold_dead_compile_error.lir` shows the erased shape). A marker
that survives folding is a live error, diagnosed during folding — never during
verification (reaching the verifier is an internal error). The fold-required
markers `@as_int`, `@as_float`, and `@size_of` are the same shape: the front-end
emits a pre-fold node, the folder consumes it to a plain `const` (or `E060`), and
a survivor reaching verification is an internal error, never a backend call.
`@as_float` folds to the IEEE-754 bit pattern of its value at the target width
(16/32/64) as an integer `const`, never a float literal. `@size_of` folding is
best-effort in M3 (operand-type semantics not yet pinned).

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
* ```f.cmp.gt &a, &b -> &r``` / ```f.cmp.lt &a, &b -> &r```
Result is `@bit8` (`0x0` false, otherwise true). Widths of inputs must match;
`i.*` vs `f.*` selects two's-complement vs IEEE-754 interpretation.

## Integer Arithmetic
* ```i.add &a, &b -> &r``` / ```i.sub``` / ```i.mul``` / ```i.div``` / ```i.mod```
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
* Fold-required markers `@as_int`/`@as_float`/`@size_of` on constant operands
  never appear in verified IR — they fold to `const`.

## Verification (`lilac-ir::verify`, runs before any backend)
1. Header present with supported `lilac_ir` version; `target` resolves `@bitNative`.
2. All `!/@/#/%/$/&` names resolve; `&regs` defined once before use in block.
3. Every block ends in exactly one terminator; all branch targets exist.
4. `call` arity and `in/out` types match callee signature.
5. `field .name` exists with index in bounds and matching type comment.
6. Arithmetic/comparison widths match; `i.*`/`f.*` used on allowed widths only.
7. `return` only when all `out` places are definitely assigned.
8. No `branch.if` on a constant, no fold-required marker (`@as_int`/`@as_float`/
   `@size_of`) or `compile.error` survivor. Violations are user-facing
   `Diagnostic`s with HIR spans, never backend crashes.
9. Types are concrete only: no generic declaration and no type parameter
   survives; every applied declaration is monomorphized to a mangle-named item.