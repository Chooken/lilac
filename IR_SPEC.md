# Lilac IR Specification (0.5)

> Version: `lilac_ir = "0.5"`. Status: normative for the Rust rewrite.
> `SPEC.md` is normative for language semantics; this file is normative for IR text,
> verification, and backend contract. Plan-only material lives in `docs/history/RUST_REWRITE_PLAN.md`.

# Declaration Syntax
* ! = Constant in data section
* @ = Function
* \# = type
* % = mutable place (allocad variable)
* $ = Label (block)
* & = SSA register value within a block (`&n: #type`, defined once before use).
  A register holds a scalar only: a bit or float container, `#bit_native` or
  a `ref`. Aggregates (`#Name`, arrays, the builtin layouts) live in places
  and move between them with `copy`; an aggregate register is a
  verification error.
* `addr` / `addr.func` / `load` = explicit address and dereference operations.
  There is no bare `ref` modifier on operands.

# Sections

Data sections hold constant declarations: the bytes of string literals
(`!c: str = "..."`). A data `const` is the address of those bytes
(`&1: ref #bit8 = const !c`); a string literal is a `#byte_buffer` place
filled field by field with it and its length, never a call to the
`.lilac.byte_buffer_from` runtime function. Number literals are `const`
registers typed directly, not data. An f-string is a template (below) and a
`format` instruction.
```lilac-ir
data {
    !c0: str = "String",
}
```

A template is the static half of an f-string: its literal pieces and, for
each hole, the function that formats it, printed as the f-string reads
(`LirModule::formats`). Piece `i` comes before hole `i`, so there is one
more piece than holes, and any piece may be empty:
```lilac-ir
!fmt0: format "a" {@@format(ReR9FormatterE)} "b"
```
A formatter is an ordinary place-ABI function with ins `ref #T, ref #W`
(its hole's type, and the one type every formatter of the template writes
into) and no outs, so a consumer calls it with the hole's address as is.
In memory a template is the C struct
```c
struct template {
    uintptr_t holes;
    struct {
        struct { uintptr_t len; const uint8_t *data; } text;  /* #byte_buffer */
        void (*format)(void);   /* null after the last piece */
    } parts[holes + 1];          /* #.lilac.format_part */
};
```
(`lilac_ir::format_template_layout`, from `lilac-layout` like every other
layout). Each piece points at its own read-only bytes, an empty one too,
so no piece's `data` is null. DCE keeps a template while a live `format`
names it, and a template keeps its formatters.

Globals are mutable static storage, one declaration each, either zeroed or
holding their initial bytes (little-endian, in the type's C layout) as runs:
`offset: "bytes"` pairs over an otherwise zero object, in offset order,
non-empty, not overlapping and within the type's size (the verifier checks
all four). Lowering writes one run per stretch of nonzero scalars, joining
runs less than 16 zero bytes apart, and never materializes the zeros; the
backend emits a packed constant with `zeroinitializer` gaps.
An initial value may also hold addresses, `offset: addr @f` (a function's;
an `export`'s is its C entry) or `offset: addr !g` (a global's), printed in
offset order among the runs. Each is a pointer-sized slot aligned to a
pointer, inside the type, overlapping no run or other address, and names a
function or global of the module; the backend puts a `ptr` there. Runs
never join across an address.
The symbol is the global's qualified name (a root global's under `.root.`).
DCE keeps a global that a live `addr.global` names, or that a kept global's
address points at; an `addr.global` reaches every function the global's
addresses reach, through other globals too, so a function only a table
points at stays alive.
```lilac-ir
global !.root.count: #bit32 = bytes { 0: "*\x00\x00\x00" }
global !.root.origin: #Point = bytes { 0: "\x03\x00\x00\x00\x04\x00\x00\x00" }
global !.root.pointer: ref #bit32 = zeroed
global !.root.TABLE: #Table = bytes { 0: addr @.root.add, 8: addr @.root.total }
global !.root.sink: #Sink = bytes { 0: addr !.root.count, 8: addr !.root.TABLE }
```

String const bytes are raw bytes: printable ASCII and newlines print as is, `"` and `\` are escaped with a backslash, and any other byte prints as `\xHH` (one byte, as in source).

Types are defined with their layout on the module's target: the size and
alignment in bytes after the name, and the members with their field names
in layout order (sections print sorted by type name for deterministic
snapshots):

```lilac-ir
type #Pair size 8 align 4 {
    tag: #bit8,
    val: #bit32,
}
type #Shape tagged size 16 align 8 {
    tag: #bit8,
    Byte: #bit8,
    Word: #bit64,
}
```

A layout is the C layout of its fields on the target (`lilac-layout`): a C
struct, each field at the next multiple of its alignment and the size
padded to the largest alignment, with no reordering; no fields is size 0,
align 1. Scalars are laid out as `uint8_t`…`uint64_t`, `_Float16`,
`float`, `double`, the 128-bit float, and `uintptr_t`/`T *` for
`#bit_native`/`ref T` (`Target::scalar_layout`). A `type #Name tagged size
S align A` section is a tagged union (the C `struct { tag; union { … }
payload; }`): its first member is the integer tag, at offset 0, and every
other member a payload at one shared offset, the tag's size rounded up to
the strictest payload's alignment; the size is that offset plus the largest
payload, rounded up to the alignment (the tag's or the strictest payload's,
whichever is larger), and with no payload members it is just the tag.
Lowering lays out every enum this way: the tag (named `tag`), then one
member per payload variant, named after it, in declaration order, so a
variant's payload is the member after the tag counting only variants with
a payload. A variant is built in a zeroed place, so the payload bytes past
the live member stay zero, which `@memcmp` equality relies on. The
verifier recomputes every section for the module's `target` and rejects one
whose offsets, size or alignment differ; nothing after lowering computes a
layout. The backend gives an aggregate the LLVM type `[size x i8]` aligned
to the layout's alignment and addresses a field as the base plus its
offset, so the layout here is the only one. No layout, and no array, may
be larger than the target's `isize::MAX` bytes (typeck reports one as `E067`).

Builtin aggregates usable in `#` positions are `#byte_buffer`,
`#format_args`, `#.lilac.format_part`, `#.lilac.format_line` and
`#.lilac.frame`. All are plain values with no hidden behavior, and their
field layouts are fixed by the IR rather than declared in a `type` section:

```lilac-ir
#byte_buffer { len: #bit_native, data: ref #bit8 }
#format_args { template: ref #unknown, holes: ref #unknown }
#.lilac.format_part { text: #byte_buffer, format: ref #unknown }
#.lilac.format_line { holes: #bit_native, len: #bit_native, data: ref #bit8, format: ref #unknown }
#.lilac.frame { prev: ref #.lilac.frame, site: #byte_buffer }
```

`#format_args` is an f-string built by `format`: its template and its array
of hole addresses (null with no holes). `#.lilac.format_part` is one part
of a template. `#.lilac.format_line` is a template with no holes and one
piece, its fields flattened, laid out as such a template is: the panic
handler builds one on its stack to write a trace line as a format.

`#.lilac.frame` is one shadow-stack record for panic stack traces (see
*Panics and stack traces* below); the leading `.` keeps it apart from user
types.

The verifier resolves `field`/`field.addr` against these layouts
(`lilac_ir::builtin_fields`), laid out as C structs like any other
(`lilac_ir::builtin_layout`); the backend sizes them from that layout.

The opaque type `#unknown` has no size and is valid only behind a reference:
`ref #unknown` is the erased pointer (C `void*`), lowered to an LLVM `ptr`. A
bare `#unknown` place is unsized and rejected by the verifier.

An array type is spelled `[N x #t]`: `N` elements of `#t` (any sized type,
including another array), laid out like a C array and lowered, like every
aggregate, to LLVM bytes. Its elements are addressed with `elem.addr`
(the base plus the index times the element's size, an `inbounds` byte
offset).

Functions can be declared as:
```lilac-ir
lilac_ir = "0.5"
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

# Symbol names

Verified IR is **concrete-only**: generic declarations and their type
parameters never appear. Each reached `(declaration, type arguments)`
instantiation is monomorphized to ordinary concrete items. Their symbols
are mangled after Rust's v0 scheme (the `rustc` book, "v0 Symbol
Format"): a symbol keeps its readable path, and the types in it are
written in *regions*, compactly, with every repeat a backreference.

**The readable part** is a function's namespace path or owning type, then
its name: `A.B.foo`, `.root.foo` (a root free function), `Object.Method`,
`Box.@on_drop`. An object or enum type's name is its qualified name
(`A.Holder`). Readable parts use `[A-Za-z0-9_.@]` and never a bracket.

**Regions** hold the types:

- `[<types>]` after a generic type's or function's name holds its generic
  arguments: `GenObject[m]` is `GenObject[@bit32]`, with methods such as
  `GenObject[m].get` and `GenObject[m].get[y]` (a method's own arguments),
  and `.root.id[m]` is `id[@bit32]`.
- `(<params>E<returns>)` after a hook's name holds its signature, so
  overloads stay distinct: `GenBox[m].@on_drop(RI6GenBoxmEE)` (`(ref
  GenBox[@bit32])`), `@conversion(mEh)` (`(@bit32) -> @bit8`).
- A type that is not an object, enum or instance is named by a region of
  its own (`[Am3_]` is `@array(@bit32, 4)`).

A type's name keys its `type` layout (`#GenObject[m]`) and its glue
(`.lilac.drop.GenBox[m]`, `.lilac.copy.GenBox[m]`); no instruction carries
a generic parameter type.

Inside a region only `[A-Za-z0-9_]` appears:

```
type    = basic | path | "R" type | "I" path {type} "E"
        | "A" type number | "w" type | "F" ["KC"] {type} "E" {type} "E"
        | "p" number ident | backref
path    = ident | "N" path ident | backref
ident   = decimal ["_"] bytes | "u" decimal ["_"] escaped
number  = "_" | base62-digits "_"
backref = "B" number
```

| Basic | Type | Basic | Type | Basic | Type |
|---|---|---|---|---|---|
| `h` | `@bit8` | `k` | `@fbit16` | `e` | `@byte_buffer` |
| `t` | `@bit16` | `f` | `@fbit32` | | |
| `m` | `@bit32` | `d` | `@fbit64` | `g` | `unknown` |
| `y` | `@bit64` | `q` | `@fbit128` | `r` | error (never in verified IR) |
| `j` | `@bit_native` | | | | |

- An object or enum is its path: `N <parent> <name>` from the root, so
  `A.B.Holder` is `NN1A1B6Holder`. `R` is `ref`; `I` an instance (its
  generic type, its arguments, `E`); `A` an array (its element, its
  length); `w` an `@format_args` (its writer); `F` a function type
  (parameters, `E`, results, `E`), `FKC` an
  `export function`; `p` a generic parameter (index and name: templates
  only, never in verified IR).
- An identifier is its length in decimal, a `_` when its bytes start with
  a digit or `_`, then its bytes (`6Holder`, `4__Gen`). One with a byte
  outside `[A-Za-z0-9_]` (the `@` of `@builtin`) is `u`, then the same of
  its escaped form, where each such byte, and `_`, is `_` and two
  lowercase hex digits: `@builtin` is `u10__40builtin`.
- Numbers are v0's base 62: `_` is 0, and `n` is `n - 1` in the digits
  `0-9a-zA-Z` followed by `_` (`3_` is 4, `Z_` is 62, `10_` is 63).
- A backreference `B<number>` stands for the type or path that starts at
  that byte offset of its region (counted from the byte after the
  opening bracket), which must be written in full and end before the
  backreference. The encoder memoizes each type and path it writes and
  uses a backreference wherever it is shorter, so a backreference never
  points at another one. `Q[Holder[@bit32], Holder[@bit32]]`, with
  `Holder` in `A`, is `Q[IN1A6HoldermEB_]`; four repeats of four repeats,
  `f3[P[P[Empty, …], P[Empty, …], …]]`, is
  `.root.f3[I1PI1P5EmptyB5_B5_B5_EB2_B2_B2_E]`.

**Size.** A region's length is linear in the number of distinct types it
names, however long their written form: `P[T, T]` applied `n` times is a
tree of `2^n` leaves, and its region is `O(n)` bytes. Types may be
arbitrarily large; there is no type length limit (`SPEC.md` §3).

**No two symbols collide.**

- A readable part has no bracket, and a region ends at its matching
  bracket, so a symbol splits into readable parts and regions one way.
- A region decodes to exactly one list of types: each element starts with
  its tag and has a fixed shape, an identifier carries its length, and a
  backreference names a whole earlier element. So two instances share a
  symbol only if they share a definition (whose qualified name is
  unique) and every type argument: then they are the same instance.
- Every Lilac symbol has a `.`, an `@` or a bracket: a root free function
  is `.root.`-prefixed, a namespaced item or method has its `.`, a hook
  its `@`. Root `main` is the exception, and LLVM renames it `.lilac.main`.
  A C identifier has none of them, so no Lilac symbol is a C symbol, and
  `extern` and `export` symbols, which are C identifiers, never collide
  with one (two externs that share a link name are one declaration).
- Compiler-generated names are in `.lilac.` (below), which no user name
  can start; glue is named by its type (`.lilac.drop.<type name>`).

**Demangling.** `lilac_ty::demangle` writes a symbol's regions as Lilac
types (`GenObject[@bit32].get[@bit64]`, `GenBox[@bit32].@on_drop(ref
GenBox[@bit32])`, `@conversion(@bit32) -> @bit8`), cut past 4,096 bytes
with `...`. It checks a whole symbol before printing any of it and gives
nothing for a malformed one (an unknown tag, a number that overflows, a
backreference that does not point back at a whole element), which then
reads raw. It keeps its own stacks rather than recursing, never panics,
and takes time linear in the symbol plus the output. `lilac demangle
[<symbol>...]` prints each symbol demangled; with none it filters standard
input like `rustfilt`, rewriting each region that follows a symbol
character, so `lilac emit-ir x.lilac | lilac demangle` reads like source.

## Reserved names and compiler symbols

Every compiler-generated function or runtime symbol lives in the `.lilac.`
namespace: the LIR runtime (`.lilac.byte_buffer_from`, `.lilac.load_u8`,
`.lilac.format_args_piece`, …), the entry stub
`.lilac.start(#bit_native %count, ref #unknown %arguments, ref #unknown
%environment)` (calls `.lilac.entry` with them, then `.lilac.exit` with 0,
then `@lilac_abort`; the backend's entry wrapper, `main` or
`.lilac.native_entry`, calls it), the
platform hooks (`.lilac.entry`, `.lilac.exit`, `.lilac.compiler_print`: the
program's `@entry`, `@exit` and `@compiler_print`), the renamed Lilac
`main` in LLVM (`.lilac.main`), and the backend's three leaf intrinsics
(`.lilac.ptr_add`, `.lilac.memcpy`, `.lilac.abort`). An `extern` whose name
starts `.lilac.` declares a hook defined elsewhere and keeps the place ABI
(`LirFunc::uses_c_abi`); every other `extern` uses the C ABI. LLVM 18 prints `.`-leading and dotted names unquoted,
and quotes one with a bracket, a parenthesis or an `@`
(`@"GenObject[m].get"`). The
verifier rejects two functions with the same name (`E070`), so a
user/prelude clash cannot reach a backend; mangled names never collide
(see "Symbol names").

A function that can fail (`SPEC.md` §4) has no LIR form of its own: its
out slots are its success outs, then its failure outs, then a hidden
`#bit8` flag, all in the place ABI. The body sets the flag to 1 at entry;
`fail` stores 0 and returns. A caller binds every out and branches on the
flag. Such functions are never `extern`, `export` or function values.

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

`zero %x` re-zeroes a place (a function's out slots and drop flags at entry,
a struct literal's temporary before its fields are written); an aggregate
place is a `memset` of its size at its alignment:
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
  `#t` is a scalar (registers hold nothing else); an aggregate behind a
  `ref` is read with `copy ref %p -> %dst`, which copies the `size` bytes
  `%p` points to into the place `%dst` (a `memcpy`). Every scalar load
  and store carries the scalar's alignment.
  Plain operand reads of a `ref` place (`copy %p`) yield the pointer.
```lilac-ir
&1: ref #bit32 = addr %variable
&2: #bit32 = load %ptr
copy ref %self -> %tmp.1
```

### copy / const
* `copy` moves between places and regs. Operands must share a type, except that
  two references are compatible under opaque pointers: `copy %a -> %b` with
  `ref X` -> `ref Y` is a pointer copy. A reference/value mix (`ref X` vs a
  value) and two distinct value types are rejected. A `copy` of an aggregate
  is place to place, a `memcpy` of the layout's size at its alignment, so
  it carries padding bytes too and costs LLVM nothing however large.
* `const` defines a typed register from a literal: an integer, a float or a
  data symbol (`LirConst`; a data symbol's register is `ref #bit8`). The
  verifier checks it against the register type. A float is its exact IEEE
  bit pattern (`lilac_float::FloatBits`), of its type's width: `#fbit32`
  and `#fbit64` print as the shortest decimal that reads back to the same
  bits (`0.1`, `-0.0`), `#fbit16` and `#fbit128` as the pattern in hex after
  `0xH` and `0xQ` (LLVM's spelling). The backend builds it from those bits,
  so no float constant passes through another width.
```lilac-ir
copy %a -> %b
copy &1 -> %b
&1: #bit32 = const 0x1
&2: #fbit64 = const 0.1
&3: #fbit16 = const 0xH2E66
&4: #fbit128 = const 0xQ3FFB999999999999999999999999999A
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

### format
* `format !t (&h0, …) -> %d` builds an f-string from the template `!t`:
  register `i` is the address of hole `i`, of the type its formatter takes
  first, and `%d` is a `#format_args` place. It stores the hole
  addresses in an array that lives as long as the function's frame (one
  per instruction, so a `format` in a loop reuses it), then sets
  `%d.template` to the template and `%d.holes` to the array (null when
  there are no holes). It calls nothing, so stack traces add no site.
```lilac-ir
&3: ref #byte_buffer = addr %part
format !fmt0 (&3) -> %msg
```
The LLVM backend emits the template as a constant in its layout, with
relocations to the piece bytes and the formatters, and the instruction as
an entry-block `alloca` of the array, one store per hole and two field
stores.

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
  `@size_of`/`@align_of` are `const`s, `@ptr_to_address`/
  `@address_to_ptr` are `bitcast`s, and `@compile_error`
  is a marker, never builtin calls
  (see `compile.error`)

| builtin | in-args | out | LLVM lowering |
| --- | --- | --- | --- |
| `@ptr_add` | `ref #bit8`, `#bit_native` | `ref #bit8` | `getelementptr i8` (wrapping: no `inbounds`) in an always-inline IR helper (as `.lilac.ptr_add`) |
| `@memcpy` | `ref #bit8`, `ref #bit8`, `#bit_native` | — | `llvm.memcpy` (as `.lilac.memcpy`) |
| `@memmove` | `ref #bit8`, `ref #bit8`, `#bit_native` | — | `llvm.memmove` (as `.lilac.memmove`) |
| `@memset` | `ref #bit8`, `#bit8`, `#bit_native` | — | `llvm.memset` (as `.lilac.memset`) |
| `@memcmp` | `ref #bit8`, `ref #bit8`, `#bit_native` | `#bit32` | the C `memcmp` (aggregate `==`; a native executable defines it, `no-builtins`) |
| `@linux_syscall` | `#bit_native` × 7 | `#bit_native` | the Linux system-call instruction, as inline assembly (as `.lilac.linux_syscall`; x86-64 and aarch64 Linux only, `E035` from user code elsewhere) |
| `@lilac_abort` | — | — | `llvm.trap` + `unreachable` (as `.lilac.abort`); `@trap()` lowers to it, then `unreachable` |
| `@trace_head` | — | `ref ref #.lilac.frame` | address of a zeroed internal pointer global (as `.lilac.trace_head`), `thread_local` on Windows, Linux (local-exec) and macOS |
| `@panic_depth` | — | `ref #bit_native` | address of the thread's panic depth, a zeroed internal integer global (as `.lilac.panic_depth`), thread-local like `@trace_head`'s |
| `@linux_thread_clone` | `#bit_native`, `ref #unknown` × 4, `#bit_native`, `ref #unknown` × 2 | `#bit_native` | `.lilac.tls_init` lays out the TLS block from `PT_TLS`, then the `clone` system call in module assembly (`.lilac.clone_raw`), the child calling the C function `entry(argument)` (as `.lilac.linux_thread_clone`; x86-64 and aarch64 Linux only, `E035`) |

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
| `.lilac.format_args_holes` | `#format_args` | `#bit_native` |
| `.lilac.format_args_piece` | `#format_args`, `#bit_native` | `#byte_buffer` |
| `.lilac.format_args_hole` | `#format_args`, `#bit_native`, `ref #unknown` | — |

The three `format_args` readers are always-inline and read the template
at the target's layout (the part size and offsets of `format_template_layout`);
lowering checks the index against the hole count first, so they never panic.

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

`msg` has the type `.lilac.compiler_print` takes, `M`: `#byte_buffer`, or
`#format_args` (the hook takes an `@format_args(W)`; a missing hook is
declared with `#byte_buffer`). The handler writes each line as an `M`: its
own text as `!const` data or as hole-free templates
(`!.lilac.panic.head: format "panic: "`), and each trace site through a
`#.lilac.format_line` on its stack. The messages lowering generates
(`division by zero`, `index out of bounds`, the format bounds checks) are
built the same way: a `#byte_buffer` over a `!const`, or `format !fmtN ()`
of a template with one piece.

A panic while the message is written must not recurse forever. The
handler first adds one to the thread's panic depth (`@panic_depth`): a
handler entered at depth 1 writes `panic while writing a panic message`
and exits with 101, and one entered deeper (the hook itself panics) traps
at once.

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
* ```&r: #bit8 = cmp.eql &a, &b``` — bit equality of two same-typed
  scalars (integers, floats, pointers). Aggregate `==` is a `@memcmp` of
  the two places' addresses over the layout's size, compared with zero:
  every byte, padding included.
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

Precondition: the divisor of `i.div`, `i.div.u`, `i.mod` and `i.mod.u` is
never zero, and the operands of `i.div` and `i.mod` are never the minimum
signed value with `-1`. The producer guarantees it: lowering tests each
division first (`cmp.eql` with `0`, and for the signed forms with `MIN` and
`-1`) and branches to a `@.lilac.panic` call with "division by zero" or
"division overflow" and the intrinsic call's site, leaving out a test that
constant operands rule out. A backend may therefore lower them to
`sdiv`/`udiv`/`srem`/`urem` directly; executing one that breaks the
precondition is undefined.

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
* Strings: `!c: str = "..."` in `data`; `&1: ref #bit8 = const !c` is its
  address, stored with the length into a `#byte_buffer` place.

## Verification (`lilac-ir::verify`, runs before any backend)
1. Header present with supported `lilac_ir` version; `target` resolves `@bit_native`.
2. All `!/@/#/%/$/&` names resolve; `&regs` defined once before use in block,
   each of a scalar type (an aggregate register is rejected).
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
12. `copy ref %src -> %dst`: `%src` is a `ref` place and `%dst` a place of
    its pointee's type. A global's initializer runs are ordered, non-empty,
    disjoint and inside its size; its addresses are ordered,
    pointer-aligned, inside its size, overlap no run or each other, and
    name a function or global of the module.
13. Every `!` symbol (data, template or global) is defined once.
14. A template has one more piece than holes, and each formatter is a
    place-ABI function (not a C `extern`, not variadic) with ins `ref #T,
    ref #W` and no outs, with one `#W` for the whole template. A `format`
    names a declared template, passes one register per hole, register `i`
    of exactly the type hole `i`'s formatter takes first, and writes a
    `#format_args` place.
