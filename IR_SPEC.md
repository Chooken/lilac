# Lilac IR Specification (0.4)

> Version: `lilac_ir = "0.4"`. Status: normative for the Rust rewrite.
> `SPEC.md` is normative for language semantics; this file is normative for IR text,
> verification, and backend contract. Plan-only material lives in `docs/history/RUST_REWRITE_PLAN.md`.

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

Data sections hold constant declarations: the bytes of string literals
(`!c: str = "..."`). A string literal is read as a data-backed buffer value
with `const !c:#byte_buffer`, never a call to the `.lilac.byte_buffer_from`
runtime function. Number literals are `const` registers typed directly, not
data. f-strings lower to `@format_args` construction plus conversions/calls
and introduce no new instruction shapes (see `SPEC.md`).
```lilac-ir
data {
    !c0: str = "String",
}
```

Globals are mutable static storage, one declaration each, either zeroed or
holding their initial bytes (little-endian, in the type's C layout).
The symbol is the global's qualified name (a root global's under `.root.`);
globals no `addr.global` names are removed by DCE.
```lilac-ir
global !.root.count: #bit32 = bytes "*\x00\x00\x00"
global !.root.pointer: ref #bit32 = zeroed
```

String const bytes are raw bytes: printable ASCII and newlines print as is, `"` and `\` are escaped with a backslash, and any other byte prints as `\xHH` (one byte, as in source).

Types are defined as like (members carry their field names in layout order;
sections print sorted by type name for deterministic snapshots):

```lilac-ir
type #Number {
    tag: #bit8,
    val: #bit32,
}
```
as produced by `tests/ui/match_enum.lir`.

Builtin aggregates usable in `#` positions are `#byte_buffer`,
`#format_args` and `#.lilac.frame`. All are plain values with no hidden
behavior, and their field layouts are fixed by the IR rather than declared in
a `type` section:

```lilac-ir
#byte_buffer { len: #bit_native, data: ref #bit8 }
#format_args { count: #bit_native, p0: #byte_buffer, …, p15: #byte_buffer }
#.lilac.frame { prev: ref #.lilac.frame, site: #byte_buffer }
```

`#.lilac.frame` is one shadow-stack record for panic stack traces (see
*Panics and stack traces* below); the leading `.` keeps it apart from user
types.

`#format_args` is a fixed-capacity inline piece table of exactly 16 pieces.
The table is spelled out as sixteen named fields rather than an array.
The verifier resolves `field`/`field.addr` against these layouts
(`lilac_ir::builtin_layout`); the backend builds matching structs.

The opaque type `#unknown` has no size and is valid only behind a reference:
`ref #unknown` is the erased pointer (C `void*`), lowered to an LLVM `ptr`. A
bare `#unknown` place is unsized and rejected by the verifier.

An array type is spelled `[N x #t]`: `N` elements of `#t` (any sized type,
including another array), laid out like a C array and lowered to LLVM
`[N x T]`. Its elements are addressed with `elem.addr`.

Functions can be declared as:
```lilac-ir
lilac_ir = "0.4"
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
with exactly one terminator (`return | branch | branch.if | unreachable`).

`inline function @f` marks a function lowered from a `=>` body: backends must
inline it at every call site where they can (LLVM `alwaysinline`, honored at
`-O0` too). It is otherwise an ordinary function; a recursive call is simply
left as a call.

Foreign symbols are declared without a body; calls to them verify like
ordinary calls (arity and `in/out` types from the declaration). Backends emit
native external calls; link libraries come from the package manifest.
```lilac-ir
extern @ext_name
    in #type %param_1,
    out #type %out_1
```

An `export` is an ordinary function with an `export` line naming the C
symbol it defines and that symbol's C signature (one C type per `in`, at
most one result). The backend emits that symbol as a C function which
stores each argument in a stack slot, calls the function with the slots,
and returns its out place. `addr.func` of the function is the C symbol's
address.
```lilac-ir
function @.root.compare
  export @compare (ref #unknown, ref #unknown) (#bit32)
  in ref #bit32 %a
  in ref #bit32 %b
  out #bit32 %order
{
  ...
}
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
`GenObject.1..bit32.get`), and `GenObj.1.GenObj.1..bit32`. A function's
own generic arguments follow its name the same way
(`CAllocator.create.1..bit32`), and a hook's symbol always ends in its
signature (`GenBox.1..bit32.@on_drop:ref_GenBox[bit32]`) so overloads stay
distinct. The arity tag
keeps `GenObj[GenObj[@bit32]]` distinct from a multi-argument application.
`.` is reserved: Lilac identifiers are `[A-Za-z_][A-Za-z0-9_]*`, so no user
name contains it and every generated segment is unambiguous. Builtin type
mangles gain a leading `.` (`.bit8`, `.byte_buffer`).
Object `type` layouts and RAII `drop` targets are keyed by the same mangled
name; no instruction carries a generic parameter type.

## Reserved names and compiler symbols

Every compiler-generated function or runtime symbol lives in the `.lilac.`
namespace: the LIR runtime (`.lilac.byte_buffer_from`, `.lilac.load_u8`,
`.lilac.format_args_push`, …), the entry stub
`.lilac.start(#bit_native %count, ref #unknown %arguments, ref #unknown
%environment)` (calls `.lilac.entry` with them, then `.lilac.exit` with 0,
then `@lilac_abort`; the backend's entry wrapper, `main` or
`.lilac.native_entry`, calls it), the
platform hooks (`.lilac.entry`, `.lilac.exit`, `.lilac.compiler_print`: the
program's `@entry`, `@exit` and `@compiler_print`), the renamed Lilac
`main` in LLVM (`.lilac.main`), and the backend's three leaf intrinsics
(`.lilac.ptr_add`, `.lilac.memcpy`, `.lilac.abort`). An `extern` whose name
starts `.lilac.` declares a hook defined elsewhere and keeps the place ABI
(`LirFunc::uses_c_abi`); every other `extern` uses the C ABI. LLVM 18 prints `.`-leading and dotted names unquoted. The
verifier rejects two functions with the same name (`E070`), so a residual
mangle collision or a user/prelude clash cannot reach a backend.

A `drop %x` marker records that a place is destroyed; it does not itself run
the destructor. Where the type needs dropping, the call that does it (the
type's `@on_drop`, or the generated `.lilac.drop.<T>` glue that runs
`@on_drop` and then drops each field) precedes the marker, so the hook
observes live memory. Field-wise copies use `.lilac.copy.<T>` glue the same
way.

# Instructions

## Memory

### allocate 
* Type
* Variable (mutable place, always zero-initialized)

```lilac-ir
alloca #type %name : zeroed
```
Legacy text `allocate #, %` means the same as `alloca`.

`zero %x` re-zeroes a place (a function's out slots at entry, a struct
literal's temporary before its fields are written):
```lilac-ir
zero %out
```

### addr / addr.func / addr.global / load
* `&r: ref #t = addr %var` takes a place address; `addr.func @f` takes a
  function address with pointee signature checked by the verifier;
  `&r: ref #t = addr.global !g` takes a global's address (`#t` is the
  global's type; a `ref` global's address is `ref #unknown`);
  `&r: #t = load %p` reads the value that the pointer in `ref` place `%p`
  points to. `load` always follows the pointer, even when `#t` is itself a
  `ref` (reading an `Optional[ref T]` payload through the slot's address).
  Plain operand reads of a `ref` place (`copy %p`) yield the pointer.
```lilac-ir
&1: ref #bit32 = addr %variable
&2: #bit32 = load %ptr
```

### copy / const
* `copy` moves between places and regs. Operands must share a type, except that
  two references are compatible under opaque pointers: `copy %a -> %b` with
  `ref X` -> `ref Y` is a pointer copy. A reference/value mix (`ref X` vs a
  value) and two distinct value types are rejected.
* `const` defines a typed register from a literal: an integer, a float or a
  data symbol (`LirConst`). The verifier checks it against the register type.
```lilac-ir
copy %a -> %b
copy &1 -> %b
&1: #bit32 = const 0x1
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
* `trunc` narrows, and `extend.zero` and `extend.sign` widen, bit
  containers; `field.addr`
  computes a field address like `field` but yields a register holding it.
```lilac-ir
&2: #bit8 = trunc &1
```
as produced by `tests/ui/conversion.lir`. `extend.zero` and `field.addr`
print symmetrically (`&3: #bit32 = extend.zero &2`,
`&4: #bit32 = field.addr %variable .member  // idx 0`); no UI snapshot
covers them yet.

### atomics
* `atomic.load.<order>`, `atomic.store.<order>`, the read-modify-writes
  `atomic.<op>.<order>` (`swap`, `add`, `sub`, `and`, `or`, `xor`, `max`,
  `min`, `max.u`, `min.u`; each yields the old value),
  `atomic.cmpxchg.<order>` (strong; yields the old value) and
  `fence.<order>`. `<order>` is `relaxed`, `acquire`, `release`, `acq_rel`
  or `seq_cst`. The address is a `ref #t` register, the operands are `#t`
  registers, and `#t` is a bit container; every access is aligned to its
  width. A `cmpxchg`'s failure ordering is implied: the strongest valid one
  (`acquire` for `acq_rel`, `relaxed` for `release`).
```lilac-ir
&3: #bit32 = atomic.load.acquire &2
atomic.store.release #bit32 &5 -> &4
&8: #bit32 = atomic.add.seq_cst &6, &7
&15: #bit32 = atomic.cmpxchg.acq_rel &12, &13, &14
fence.seq_cst
```
as produced by `tests/ui/atomics.lir`.

### elem.addr
* `&d: ref #t = elem.addr &a, &i` yields the address of element `&i` (a
  `#bit_native`) of the array that `&a` (a `ref [N x #t]`) points to. It
  does not check the bounds: lowering emits an unsigned `i.cmp.lt.u`
  against `N` and a branch to a panic first, unless the index is a constant
  below `N`.
```lilac-ir
&2: ref [4 x #bit32] = field.addr %holder .items  // idx 1
&5: ref #bit32 = elem.addr &3, &4
```
as produced by `tests/ui/arrays.lir`.

## Control Flow

### call 
* Callee register holding a function address
* `in` then `out` operands in signature order; arity and types are verified
```lilac-ir
call &f (in %a) (out %b)
```

`call c &f` calls a C function's address (an `export function` value) with
the platform C ABI: each `in` place's type is its C parameter type, and the
`out` place (at most one) receives the C result. Every place must be a C
type.
```lilac-ir
call c &f (in %a) (out %b)
```

### builtin calls
* Callee is a `@`-builtin **leaf** name (`call @ptr_add (in %base, %off) (out %s)`)
* Only the target-specific leaves are builtins, verified against the
  signature table below. They lower to the `.lilac.*` backend symbols shown.
  `@size_of`/`@align_of` are `const`s, `@ptr_add`/`@ptr_to_address`/
  `@address_to_ptr` are `bitcast` (plus an `i.add`), and `@compile_error`
  is a marker, never builtin calls
  (see `compile.error`)

| builtin | in-args | out | LLVM lowering |
| --- | --- | --- | --- |
| `@ptr_add` | `ref #bit8`, `#bit_native` | `ref #bit8` | `ptrtoint` + `add` + `inttoptr` |
| `@memcpy` | `ref #bit8`, `ref #bit8`, `#bit_native` | — | `llvm.memcpy` (as `.lilac.memcpy`) |
| `@memmove` | `ref #bit8`, `ref #bit8`, `#bit_native` | — | `llvm.memmove` (as `.lilac.memmove`) |
| `@memset` | `ref #bit8`, `#bit8`, `#bit_native` | — | `llvm.memset` (as `.lilac.memset`) |
| `@linux_syscall` | `#bit_native` × 7 | `#bit_native` | the Linux system-call instruction, as inline assembly (as `.lilac.linux_syscall`; x86-64 and aarch64 Linux only, `E035` from user code elsewhere) |
| `@lilac_abort` | — | — | `llvm.trap` + `unreachable` (as `.lilac.abort`); `@trap()` lowers to it, then `unreachable` |
| `@trace_head` | — | `ref ref #.lilac.frame` | address of a zeroed internal pointer global (as `.lilac.trace_head`), `thread_local` on Windows, Linux (local-exec) and macOS |
| `@thread_entry` | — | `ref #unknown` | address of `.lilac.thread_entry(ptr start) -> ptr` (C ABI), which calls `start`'s function with its argument (as `.lilac.thread_entry.address`) |
| `@linux_thread_clone` | `#bit_native`, `ref #unknown` × 4, `#bit_native`, `ref #unknown` | `#bit_native` | `.lilac.tls_init` lays out the TLS block from `PT_TLS`, then the `clone` system call in module assembly (`.lilac.clone_raw`), the child calling `.lilac.thread_entry` (as `.lilac.linux_thread_clone`; x86-64 and aarch64 Linux only, `E035`) |

A native Linux executable with thread-locals calls `.lilac.main_tls` first
in `.lilac.native_main`: unless a loader already set the thread pointer, it
builds the main thread's block in a static 4 KiB area. A native Windows one
defines the TLS directory (`_tls_used`, `_tls_index`) a C runtime would.

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
driver's pass order is lower → inject the prelude → `prune_unreachable`, so
the runtime is present during pruning and dead-code elimination removes
whatever a program does not reach. Pruning also drops every `type` layout no
surviving function names (in a signature or an instruction type, directly,
behind a `ref`, or through another surviving layout's fields), so the types
of an unused library leave no trace.

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
Lowering decides constant-expression conditions (SPEC "Constant conditions
and `@compile_error`") and emits only the live arm, so LIR never contains a
`branch.if` on a constant; the verifier rejects one. Other constant
arithmetic and calls are left for the backend to fold.

### return
```lilac-ir
return
```
`return` requires every `out` place assigned at least once in its function
(textual rule, flow-insensitive; `zero %out` counts).

### unreachable
```lilac-ir
unreachable
```
Explicit terminator for pruned `match` fallthrough and eliminated dead branches.

### Panics and stack traces
There is no panic terminator. `@panic(msg)` lowers to a call to the runtime
function `.lilac.panic` (in `msg`, then the panic site as a `!const` line
such as `"  at main (main.lilac:3:5)\n"`) followed by `unreachable`.
`.lilac.panic` (`lilac_ir::panic_runtime`) writes `panic: <msg>`, then the
stack trace, through `.lilac.compiler_print`, then calls `.lilac.exit` with
code 101 and traps (`@lilac_abort`).

The trace is a shadow stack. When stack traces are on, the driver runs
`lilac_ir::trace::instrument` after `prune_unreachable`. It gives every
non-`extern` function outside the `.lilac.` namespace:

* after its entry block's leading `alloca`s, the places `%.lilac.frame`
  (`#.lilac.frame`) and `%.lilac.head` (the `@trace_head` result), and links
  the frame onto the list: `frame.prev ← *head`, `*head ← addr frame`;
* before each non-builtin `call`, `frame.site ← !.lilac.site.N`, that call's
  preformatted `"  at <function> (<file:line:col>)\n"` line;
* before each `return`, `*head ← frame.prev`.

`.lilac.panic` walks the list from `*@trace_head`, writing each `site`
(innermost first, at most 128, then `  ...`). With no frame pushed it writes
the panic site and a hint instead.

### compile.error
* Message: constant or variable
```lilac-ir
@compile_error(live)
```
Lowering emits one `@compile_error(live)` marker per `@compile_error(msg)`
call site it lowers. A call in the dead arm of a constant condition is never
lowered; a marker inside a function unreachable from `main` or an `--export`
root is dropped by global dead-code elimination, without diagnostic. Any other
marker is `E031`, so verified IR reaching a backend contains no marker
(`tests/ui/cfold_dead_compile_error.lir` shows the erased shape). A marker
that survives is a live error, diagnosed after reachability pruning — never
during verification (reaching the verifier is an internal error). `@size_of`
and `@align_of` never reach LIR: lowering computes them from the layouts as
`const`s.

## Casts

### extend.zero 
* Type of From: type
* From: register
* Type of To: type
* To: variable or register
```lilac-ir
extend.zero #, &, #, [%, &]
```

### extend.sign 
As `extend.zero`, filling the new high bits with the sign bit.
```lilac-ir
extend.sign #, &, #, [%, &]
```

### truncate 
* Type of From: type
* From: registor 
* Type of To: type
* To: variable or register
```lilac-ir
truncate #, &, #, [%, &]
```

### bitcast
* `&r: # = bitcast &src`: the same bits under another type. Between an
  integer and a reference it is `ptrtoint`/`inttoptr` (`@ptr_to_address`,
  `@address_to_ptr`); between two references, or same-width integers and
  floats, it is a plain reinterpretation.

## Field Access

### field 
* Parent place, field name (index + type shown as comment for readability)
```lilac-ir
field %obj .name -> &1  // idx 2, #bit32
```

## Comparisons
* ```&r: #bit8 = cmp.eql &a, &b``` — bit equality of any two same-typed
  operands (integers, floats, pointers, and aggregates, which compare their
  zero-padded bytes).
* ```i.cmp.lt``` / ```i.cmp.gt``` / ```i.cmp.le``` / ```i.cmp.ge``` (signed)
* ```i.cmp.lt.u``` / ```i.cmp.gt.u``` / ```i.cmp.le.u``` / ```i.cmp.ge.u``` (unsigned)
* ```f.cmp.lt``` / ```f.cmp.gt``` / ```f.cmp.le``` / ```f.cmp.ge``` (IEEE-754
  ordered: false when either operand is NaN)
Result is `#bit8` (`0x0` false, otherwise true). Input types must match;
`i.*` vs `f.*` selects two's-complement vs IEEE-754 interpretation.

## Integer Arithmetic
* ```i.add &a, &b -> &r``` / ```i.sub``` / ```i.mul``` / ```i.div``` / ```i.mod```
* ```i.div.u &a, &b -> &r``` / ```i.mod.u &a, &b -> &r``` (`udiv`/`urem`)
All operands share one `#bit8/16/32/64/Native` width, carried on the result reg.

## Float Arithmetic
* ```f.add &a, &b -> &r``` / ```f.sub``` / ```f.mul``` / ```f.div``` / ```f.mod```
Same width rule as integer arithmetic.

## Bitwise Operations
* ```and &a, &b -> &r``` / ```or``` / ```xor``` (`not` is `xor` with all ones)
* ```shift.l &a, &b -> &r``` / ```shift.r``` (arithmetic) / ```shift.r.u```
  (logical): the amount `&b` (same width as `&a`) is taken modulo the width.

## Literals (canonical text)
* Ints: `&1: #bit32 = const 0x2a`: lowercase hex, no leading zeros, the
  register type fixes the width. The value must fit it; a `ref` takes only
  `0x0`.
* Floats: `&1: #fbit64 = const 1000.0`: the shortest text that reads back
  as the same `f64`, not the source spelling.
* Strings: `!c: str = "..."` in `data`; a string literal is read with
  `&1: #byte_buffer = const !c` (data-backed buffer value).

## Verification (`lilac-ir::verify`, runs before any backend)
1. Header present with supported `lilac_ir` version; `target` resolves `@bit_native`.
2. All `!/@/#/%/$/&` names resolve; `&regs` defined once before use in block.
3. Every block ends in exactly one terminator; all branch targets exist.
4. `call` arity and `in/out` types match callee signature.
5. `field .name` exists with index in bounds and matching type comment.
6. Arithmetic/comparison operand types match; `i.*` takes bit containers and
   `f.*` float containers (`cmp.eql` takes any matching pair).
7. `return` only when all `out` places are definitely assigned.
8. No `branch.if` on a constant and no `compile.error` survivor. Violations are user-facing
   `Diagnostic`s with HIR spans, never backend crashes.
9. Types are concrete only: no generic declaration and no type parameter
   survives; every applied declaration is monomorphized to a mangle-named item.
10. Atomics: the address register is `ref #t` and the operands `#t` for
    a bit container `#t`; a load is not `release`/`acq_rel`, a store not
    `acquire`/`acq_rel`, and a fence not `relaxed`.
11. `elem.addr`: the array register is `ref [N x #t]`, the index
    `#bit_native`, and the result `ref #t`.
