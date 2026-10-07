# The Lilac Programming Language Specification

> Version: 0.6-draft (M6b extern C ABI: scalars by value, `ref T` → pointer, single return). Status: draft, normative for the Rust rewrite.
> Normative documents: `SPEC.md` (language), `IR_SPEC.md` (IR v2).
> `RUST_REWRITE_PLAN.md` is non-normative (execution plan only).
> Keyword spelling is `object` and `function` everywhere; all `@` hooks,
> intrinsics, and builtin types are snake_case (`@less_than`, `@i_less_than_8`,
> `@size_of`, `@compile_error`, `@bit_native`, `@byte_buffer`,
> `@extend_zero`). Legacy camelCase spellings from earlier drafts and legacy
> `tests/` files are renamed on port; the old Zig pass is not authoritative for
> spelling.

Lilac is an ultra-lean systems programming language designed around compiler minimalism. The core compiler implements only raw bit containers, control flow, functions, generics, and the foundational building blocks required to define custom types. High-level abstractions such as strings, dynamic arrays, high-level floating-point wrappers, optionals, and mutability guards are deliberately excluded from the compiler and implemented within the standard library.

## 1. Core Types & Memory Model

Lilac operates on raw memory containers rather than semantic primitives. The compiler attaches no inherent numeric meaning to types; interpretation is deferred entirely to the functions acting upon them.

### Primitive Bit Containers

* **`@bit8`**, **`@bit16`**, **`@bit32`**, **`@bit64`**: Fixed-width raw memory containers. Whether a container represents a two's complement integer or arbitrary data depends on the intrinsic function invoked (e.g., `@i_add_8`); the matching `@fbit` container is the float-bearing counterpart.

* **`@fbit16`**, **`@fbit32`**, **`@fbit64`**, **`@fbit128`**: Fixed-width raw containers holding an IEEE-754 value of the matching width (binary16/32/64/128), interpreted as such by the `@f_*` intrinsics. Like the integer containers they are plain memory; the `@fbit_to_bits_W`/`@bits_to_fbit_W` reinterpret casts exist only at 16/32/64 because the language has no `@bit128`.

* **`@bit_native`**: A container sized to the target architecture's native word length (equivalent to `usize`/`uintptr_t`).

* **`@byte_buffer`**: A builtin view over bytes: length plus pointer to the first
  byte. It is the type of string literal payloads (`"hi"`) and `data` consts — a
  shared currency for "known bytes". Meaning always comes from use (left-to-right):
  `String` construction reads text. A `@byte_buffer` borrows; it never owns.
* **`@format_args`**: A builtin fixed-capacity table of `@byte_buffer` pieces
  (at most 16; more is a compile error suggesting a split). It is the value of
  an f-string *before* consumption: static chunks reference `data` consts
  directly, holes hold converted bytes. Stack-only, no allocation, no hidden
  behavior — storage and flattening are whoever consumes it.

### Special Keywords

* **`unknown`**: An opaque type of indeterminate size. It can only be instantiated behind a reference (`ref unknown`), serving as Lilac's mechanism for type-erased pointers (analogous to `void*`).

* **`nothing`**: A parse-time keyword used exclusively in a function's return slot to indicate zero return variables. It is not a type and occupies no memory.

* **`self`**: A function-prototype-only type. It always desugars to `self: ref SelfType` — a reference to the enclosing object type, never a by-value copy. `instance.Method()` desugars to `Object.Method(ref instance)`.

### Universal Zero-Initialization

All allocated memory whether on the stack, heap, or within complex objects is automatically **0 initialized** by the compiler. There is no uninitialized memory in Lilac. This eliminates read before write bugs and guarantees deterministic padding bits, ensuring 100% reliability for bit equality checks.

### Copy Semantics

Lilac does not implement move semantics. Value assignment and parameter passing always execute a **fast bitwise copy** by default. Custom copy behavior (such as deep copying or reference counting) is implemented via RAII hooks.

## 2. Literals & Desugaring

String and char literals are compiler-known bytes; number literals are typed
values (below). There is no mirror object to declare.

Number literals are typed values, not `@byte_buffer`s. An integer literal —
decimal (`5`), hex (`0x…`), or binary (`0b…`) — takes the smallest
`@bit8/16/32/64` that holds its unsigned magnitude. An expected bit type, or a
numeric object's single exact-width `@conversion(from: @bitN)`, grows the
literal to that width (never shrinks); a value that does not fit, or one past 64
bits, is `E060`. So `x: @bit8 = 5` and `x: Int16 = 5` both typecheck (the latter
grows to `@bit16` for its conversion), while `x: @bit8 = 0x1FF` and
`x: Int8 = 300` are `E060`.

A float literal is a `.` or exponent form (`3.14`, `1e3`, `1.5e-4`, `2E10`). It
defaults to `@fbit64`; an expected `@fbit16/32/64` (or a numeric object's
`@conversion(from: @fbitN)`) pins it, and the value rounds to nearest-even at
codegen. A value past the target's maximum finite value is `E060`. A float
literal in a bit or integer slot is `E010` (no implicit float→int), and
`@fbit128` literals are out of scope. Unary `-` is an operator, not part of the
literal; reading digits from a runtime `@byte_buffer` is standard-library work,
not a compiler intrinsic.

Arithmetic between two literals of the same kind is folded at compile time: a
binary `+ - * / %` whose **both** operands are integer literals folds at 64-bit,
and whose both operands are float literals folds at `f64`. The result is a
single literal typed by the rules above, so `2 * 3 + 4` is the literal `10` and
nothing is evaluated at run time; an integer result that does not fit the
consumed width is `E060`, and so is a division or modulo by zero (or any
non-finite float result). A mixed integer/float literal pair is `E010`. These
are the only operators folded; comparisons and logical operators are never
folded, and any operand that is not a literal (an identifier, a string) leaves
the operation for ordinary type checking (its `@add`/`@sub`/… hook).

String literals (`"hi"`) have type `@byte_buffer` backed by a `data` const, with
no heap allocation: `data !cN: str = "hi"` plus `const !cN:#byte_buffer`, a
data-backed buffer value. `@byte_buffer_from` stays a builtin for user code,
but the literal path does not use it. `String` itself is purely a standard
library type — the compiler neither names it nor knows any layout for it.
`s: String = "hi"` works through the ordinary
`@conversion(from: @byte_buffer) to: String` search (left-to-right, exact-first
rules in §8); without that binding in scope it is a normal `E043` error.
UTF-8 validation lives in user-land. Values backed by `data` consts are
borrowed, not owned, and the same ref-class lifetime caveat as any `ref` applies
if such a view is stored past its statement.

Char literals (`'A'`) have type `@bit32` holding a Unicode scalar value.
Single bytes are plain `@bit8` values; multi-byte characters are `String`
values of length greater than one.

### Lexical grammar (locked for the rewrite)

* Numbers: `0x…` hex, `0b…` binary, otherwise decimal int or float (`10.5`).
  `_` separators allowed between digits (`0xFF_FF`). No sign is part of the
  literal: unary `-` is always an operator (`@negate` / `@i_sub_W`-family folding).
* Strings: `"…"` with escapes `\"`, `\\`, `\n`, and `\xHH` only. Anything else
  after `\` is a lex error.
* Chars: `'…'` holds exactly one scalar. Empty or multi-scalar char literals are
  lex errors; use a `String` for those.

### Bracket reservation

`[` and `]` are reserved for generics (`GenObj[T]`, `gen_function[@bit32](...)`).
There is no `a[i]` indexing, `a[i:j]` slicing, or `[1, 2, 3]` array literal
syntax. Element access uses methods, exactly like `+` desugars to `@add`:

```lilac
first: @bit8 = s.at(0x0)
view: String = s.slice(0x0, 0x2) // new {Size, Buffer} view, no copy
```

Fixed-size arrays use generic objects plus `@init` constructors
(e.g. `Array(T, len)`), not `T[N]` const-generics.

### Formatted strings

Formatted strings (`$"Hello {name}"`) lower to a `@format_args` value: one
`@byte_buffer` piece per literal chunk and per hole. Holes mirror C# `$"{...}"`
(full expressions, e.g. `{x.toHex()}`); alignment and format clauses do not
exist in v1. `{{` escapes to `{` and `}}` to `}`. Each piece keeps its own span.
At most 16 pieces; more is a compile error suggesting a split.

```lilac
msg: String = $"Hello {name}!"
// lowers to (names simplified):
__f: @format_args = @format_args_new()
@format_args_push(ref __f, !const "Hello ")   // static chunk, borrowed
@format_args_push(ref __f, @convert(name))    // hole: @conversion(T -> @byte_buffer)
@format_args_push(ref __f, !const "!")
msg: String = @convert(__f)                   // @conversion(@format_args -> String)
```

Holes convert through the ordinary `@conversion(from: T) to: @byte_buffer`
search — a missing conversion is the usual `E043` naming `T` and the hole span,
with no method-name coupling of any kind. A `@format_args` is storable
(`f: @format_args = $"..."`) since it is a declared builtin type, but its pieces
borrow (static consts plus conversion temps): storing one past its statement
carries the same caveat as storing a `ref`, while immediate consumption is
always sound.

Consumption is driven by the expected type (left-to-right, no inference, no
special result-type rule):

1. A type with `@conversion(from: @format_args)` (e.g. `String`) converts —
   allocation, if any, happens inside that user-land conversion.
2. A handler type with structural `append_literal(@byte_buffer)` / `append[T](T)`
   methods streams instead: the desugar emits piecewise appends into the handler
   value, formatting each hole straight into the destination with zero temp
   buffers. The handler type comes from context (a parameter, a binding); only
   the structural method shapes are fixed, never a nominal stdlib type.
3. Expected `@format_args` keeps the value itself.
4. No expected type checks the value as `@format_args` and drops it.

The compiler names zero standard library entities anywhere in this path — only
builtins, conversions, and structural shapes. Whether an f-string allocates is
therefore the caller's explicit choice of sink, never hidden sugar behavior.

### Compile-time folding

Constant `if` conditions are folded before code generation: only the live branch
is kept. `@compile_error(msg)` fires only if it is still live after folding and
global dead-code elimination, so a `@compile_error` inside a dead branch, an
uncalled function, or an unused hook is silently dropped. The former camelCase
alias `@compilerError` is no longer accepted.

Folding runs after `=>` (inline) functions are substituted, so a conversion
helper written as `=>` folds exactly like a direct call. A block-body wrapper —
including a hand-written parser — is still
attempted via best-effort call evaluation of pure bodies with bounded loops and
all-constant arguments (`MAX_FOLD_STEPS = 100_000`, `MAX_CALL_DEPTH = 16`); if
that fails the runtime call (and runtime parse) is kept. A missed fold costs
performance only, never correctness. Tiny conversions that must be zero-cost are
written as `=>`.

## 3. Compiler Intrinsics

Intrinsics represent the sole mechanisms for raw arithmetic, float manipulation, and bit-level introspection.

Numeric intrinsic names are width-suffixed: `_8`/`_16`/`_32`/`_64` select the `@bit`/`@fbit` container of that width and `_native` selects `@bit_native`. Below, `W` is any suffix valid for that family (integer widths 8/16/32/64/native; float widths 16/32/64/128) and `@bitW`/`@fbitW` the matching container.

| Intrinsic | Signature | Description |
| --- | --- | --- |
| **`@i_add_W`** | `(@bitW, @bitW): @bitW` | Integer addition using standard two's-complement logic. |
| **`@i_sub_W`** | `(@bitW, @bitW): @bitW` | Integer subtraction. |
| **`@i_mul_W`** | `(@bitW, @bitW): @bitW` | Integer multiplication. |
| **`@i_div_W`** | `(@bitW, @bitW): @bitW` | Signed integer division. |
| **`@i_div_u_W`** | `(@bitW, @bitW): @bitW` | Unsigned integer division. |
| **`@i_mod_W`** | `(@bitW, @bitW): @bitW` | Signed integer modulo. |
| **`@i_mod_u_W`** | `(@bitW, @bitW): @bitW` | Unsigned integer modulo. |
| **`@i_negate_W`** | `(@bitW): @bitW` | Integer two's-complement negation. |
| **`@i_less_than_W`** | `(@bitW, @bitW): @bit8` | Signed integer less-than (two's-complement). Returns `@bit8` (`0x0` false, otherwise true). |
| **`@i_greater_than_W`** | `(@bitW, @bitW): @bit8` | Signed integer greater-than. Returns `@bit8`. |
| **`@i_less_than_u_W`** | `(@bitW, @bitW): @bit8` | Unsigned integer less-than. Returns `@bit8`. |
| **`@i_greater_than_u_W`** | `(@bitW, @bitW): @bit8` | Unsigned integer greater-than. Returns `@bit8`. |
| **`@f_add_W`** | `(@fbitW, @fbitW): @fbitW` | Floating-point addition using IEEE-754 bit interpretation. |
| **`@f_sub_W`** | `(@fbitW, @fbitW): @fbitW` | Floating-point subtraction. |
| **`@f_mul_W`** | `(@fbitW, @fbitW): @fbitW` | Floating-point multiplication. |
| **`@f_div_W`** | `(@fbitW, @fbitW): @fbitW` | Floating-point division. |
| **`@f_mod_W`** | `(@fbitW, @fbitW): @fbitW` | Floating-point modulo. |
| **`@f_negate_W`** | `(@fbitW): @fbitW` | Float negation (IEEE-754 sign flip). |
| **`@f_less_than_W`** | `(@fbitW, @fbitW): @bit8` | Float less-than (IEEE-754). Returns `@bit8`. |
| **`@f_greater_than_W`** | `(@fbitW, @fbitW): @bit8` | Float greater-than (IEEE-754). Returns `@bit8`. |
| **`@fbit_to_bits_W`** | `(@fbitW): @bitW` | Reinterprets a float's bits as the same-width integer (`W` ∈ 16/32/64 only: `@bit128` does not exist). |
| **`@bits_to_fbit_W`** | `(@bitW): @fbitW` | Reinterprets integer bits as the same-width float (`W` ∈ 16/32/64 only). |
| **`@size_of`** | `(value)` | Returns the size of a type or variable in bytes as a `@bit_native`. Fold-required marker; folding is best-effort in M3 (operand-type semantics not yet pinned). |
| **`@truncate`** | `(value)` | Narrows a bit container; target width comes from the expected type (left-to-right). Folded when constant. |
| **`@extend_zero`** | `(value)` | Zero-extends a bit container; target width comes from the expected type (left-to-right). Folded when constant. |
| **`@load_u8`** | `(buffer, offset)` | Loads one byte from `ref @bit8` at `@bit_native` byte offset. Returns `@bit8`. |
| **`@store_u8`** | `(buffer, offset, value)` | Stores one `@bit8` value into `ref @bit8` at `@bit_native` byte offset. |
| **`@memcpy`** | `(dst, src, len)` | Copies `len` bytes (`@bit_native`) from `ref @bit8` to `ref @bit8`. |
| **`@byte_buffer_ptr`** | `(buffer)` | Returns the `ref @bit8` pointer to the first byte of a `@byte_buffer`. |
| **`@byte_buffer_len`** | `(buffer)` | Returns a `@byte_buffer` length as `@bit_native`. |
| **`@byte_buffer_from`** | `(ptr, len)` | Builds a borrowing `@byte_buffer` view over `ref @bit8` plus `@bit_native` length. Same lifetime caveat as any `ref`. |
| **`@format_args_new`** | `()` | Empty `@format_args` table (zero pieces). |
| **`@format_args_push`** | `(args, piece)` | Appends a `@byte_buffer` piece to `ref @format_args`. Panics past 16 pieces. |
| **`@format_args_count`** | `(args)` | Piece count of a `@format_args` as `@bit_native`. |
| **`@format_args_piece`** | `(args, index)` | The `index`-th piece as a borrowing `@byte_buffer` view. Out of bounds panics. |
| **`@compile_error`** | `(message)` | Creates a compile-time error if the compiler hits it logging the message. |

`@size_of` and `@compile_error` are **fold-required markers**, not runtime
builtin calls: the front-end emits a pre-fold node the constant folder must
consume (or diagnose). A survivor reaching IR verification
is an internal error, exactly like a live `@compile_error`. Builtin calls that
do reach IR are checked against the builtin signature table; `@load_u8` takes
two in-args (`ref #bit8`, `#bit_native`) and yields `#bit8`, while `@store_u8`
takes three in-args (`ref #bit8`, `#bit_native`, `#bit8`) and yields nothing.

## 4. Functions & Control Flow

### Function Signatures & Multi-Return

To return values from a function, named return variables **must** be specified in the signature. The `return` keyword takes no arguments; it simply returns the current state of the named return variables.

```lilac
multi_func: function () result_1: @bit8, result_2: @bit16 {
    result_1 = 0x1
    result_2 = 0x2
    return
}

val1: @bit8, val2: @bit16 = multi_func()
```

### Control Flow

Conditional statements evaluate expressions as boolean based on strict non-zero testing against `@bit8` (`0x0` is false; any other value is true). Non-`@bit8` types must be explicitly or implicitly converted to `@bit8` before evaluation.

If expressions take a value to conditionally call code:

```lilac
if condition {

}
```

You can also attach else blocks to ifs:

```lilac
if condition {
    
} else {

}
```

If expressions can also return values (a later milestone; the M2 compiler
rejects value position with `E001`):

```lilac
status: @bit8 = if 0x0 0x0 else 0x1
```

Value capturing from multi-return functions:
A call condition may bind one of the callee's out-slots by name with
`-> name: Type`. The call's first out-slot must be a bit container and is
tested for non-zero; the named slot must match a declared out-slot (`E022`
otherwise) and is scoped to the `then` block:

```lilac
if get_multi() -> result: @bit32 {
    // result is scoped here
}
```

Loops execute infinitely by default unless broken or conditioned:

```lilac
loop {
    // This will loop infinitely.
}
```

To make a standard conditional loop this is a way:

```lilac
val: @bit8 = 0x0
loop if @i_less_than_8(val, 0x1) {
    // do work
} else break
```
The intrinsic form needs no operator binding. Once `@less_than` is defined for
the operand types (see §8), the idiomatic form `loop if val < 0x1` works identically.

### Pattern Matching

The **`match`** statement inspects enum tags and byte patterns. Match statements are **not required to be exhaustive**. If no valid branch matches, execution silently falls through (equivalent to an implicit empty `else {}` branch).

```lilac
match enum_val {
    .Some -> value: @bit16 {
        return
    }
    .None => return
}
```

You can attach else to the end of a match to catch invalid matches:

```lilac
match enum_val {
    .None => return
} else {
    return
}
```

Arm patterns are bare tags (`.Some`); a payload binds with `-> name: Type`,
and a single statement arm shortens to `=>`. Match is statements-only in M2:
using a `match` as a value is rejected with `E001`.

```lilac
match number {
    .Some -> some: @bit32 {
    }
    .None => return
}
```

## 5. Variables, References & Immutability

### Variable Declaration

Variables are declared using `name: Type = value` syntax. Object types can utilize simplified constructor syntax during initialization, or invoke parameterized constructors using `Type(params)` if a matching `@init` hook is defined. Struct construction is always the `=`-form (`variable: Object = Object { member = 0x1 }`); the `:`-without-`=` spelling is rejected with `E001`.

```lilac
counter: @bit32 = 0x0
user: Object {}
custom_user: Object(0x1, "Lilac") // Invokes @init with custom params
```

The base language enforces no concept of mutability. Read-only variables and immutability guards are implemented in user-land using the `@on_override` compiler hook.

### References & Auto-Dereferencing

References are denoted by the **`ref`** keyword. In Lilac, `ref` acts as a type modifier flag rather than a wrapper container. Because a type cannot be flagged as a reference twice, double references (e.g., `ref ref @bit32`) are syntactically invalid unless wrapped inside an object.

```lilac
variable: @bit32 = 0x0
reference: ref @bit32 = ref variable
```

References **auto-dereference by default**. Operators act on the underlying value, not the memory address:

* `ptr + 1` invokes `@add` on the underlying `@bit32`, incrementing the integer value by `1`.

* It does **not** perform pointer arithmetic. To manipulate raw memory addresses, a reference must be explicitly converted to `@bit_native`.

## 6. Composite Types & Namespaces

### Objects

An **`object`** defines both a custom data layout and a namespace. Functions declared inside an object are public members of that namespace by default.

```lilac
Object: object {
    Member: @bit32

    // Static function (no instance parameter)
    // Can be called like Object.StaticFunction()
    StaticFunction: function () nothing {}
    
    // Instance function using 'self' syntactic sugar (self: ref Object)
    // Instance functions have to have can still be called with 
    // Object.InstanceFunction(ref instance).
    // They can also be called via instance.InstanceFunction()
    // This desugars to Object.InstanceFunction(ref instance) anyways.
    InstanceFunction: function (self) nothing {}
}
```

### Namespaces & Visibility

Namespaces mirror folders; no declaration introduces one. Every `*.lilac` file
directly in a folder belongs to that folder's namespace, and the files in one
folder **merge**: they see each other's declarations with no import (a same-named
declaration across them is `E020`). Subfolders are separate namespaces, named by
their dotted path from the root (`<root>/A/B` is `A.B`). A subfolder is loaded
only when a file in an already-loaded namespace `using`s it, directly or
transitively; subfolders are never auto-loaded.

**`using`.** `using A.B` resolves root-relative to `<root>/A/B` and makes that
namespace's public (non-`private`) names visible, unqualified, **in the file that
contains the directive only**. Sibling files do not inherit each other's
`using`s even though they share each other's declarations. Imports are
non-transitive (`B` `using C` does not expose `C` to a file in `A`) and never
pull in a parent folder's imports. A `using` inside a `private { … }` block is
still honored. Importing an ancestor namespace is allowed but unnecessary (see
reach below).

```lilac
using Standard
```

**Ancestor/descendant reach.** The folder tree orders namespaces into ancestor
chains and descendant sets; only namespaces in the compilation unit participate
and the root is always in it. A namespace automatically sees every item of its
ancestor namespaces — public items by *ancestor reach* and private items by
*descendant reach* (a `private` item is visible to its defining namespace and its
descendants). The root is an ancestor of all, so root declarations are usable
everywhere with no import. `using` is only needed for **non-ancestor**
namespaces, and it imports only their public items.

**`private`.** `private { … }` may wrap items at file top level and at
`object`-member position; nested blocks flatten. At top level it marks items
*namespace-private*: they are excluded from the namespace's `using` export, so an
importer sees an unresolved name (`E021`, not a privacy error), while the
defining namespace and its descendants still see them. Inside an `object` it
marks members *object-private*: the member is found but inaccessible from outside
the object, which is `E026`. A private declaration still occupies its name (a
private/public clash is `E020`), and `private` does not gate hook resolution:
hooks are unit-global and scope-independent, so a `private` member hook is still
found for operators and `@conversion`, while `private` still gates direct RAII
access (such as a `@init` constructor call) from outside the object.

**Collisions.** A same-named declaration in one namespace is `E020`, first
binding wins. A local (or ancestor-reached) name shadows an imported one. If two
`using`d namespaces export the same name, referencing it is `E029`; an
unreferenced collision is not an error.

**Qualification.** Namespaces are compile-time scopes only: there are no
qualified paths in expressions (`A.B.foo` is not syntax). For code generation
every top-level item takes a symbol prefixed by its namespace path (`A.B.foo`,
`A.Object.Method`, `A.GenBox.1..bit32.@on_drop`); the root namespace adds no
prefix, so single-file programs stay byte-identical and `main` stays `main`.
User-defined type names and generic instances are qualified the same way, so
same-named types in different namespaces stay distinct, while builtin types are
never qualified. Two exceptions carry no prefix: hooks are unit-global (one
signature-keyed overload table for the whole compilation unit, whether declared
at top level or inside an object) and `extern` symbols keep their unmangled
declared name (§9).

A namespace is loaded from a directory argument on the command line: a directory
selects project mode (that folder is the root namespace). A file argument keeps
single-file mode, so the file's parent folder is **not** treated as a project
root and contributes no sibling files.

### Enums

Enums are the single tagged-data type: a closed set of named variants,
each optionally carrying a payload. They lower to standard objects with
an explicit discriminator tag plus payload slots. The `=>` operator maps
constructor branches to their resulting object state.

```lilac
Enum: enum { One, Two }
```

```lilac
OptionalBit32: enum {
    Some: @bit32
    None
}
```

Values construct from bare tags (`.None`) or tag calls with payloads
(`.Some(0x1)`); `match` arms test tags with `.Tag` patterns and bind
payloads via `-> name: Type`. (The old `union` keyword was removed:
it spelled the same type. Legacy `union { ... }` declarations fail to
parse; rename them to `enum`.)

## 7. Generics & Inline Declarations

### Generics

Generics provide compile-time templating for any declaration. The generic
parameter `T` is a placeholder; the compiler ignores it until the declaration
is instantiated, which is what allows duck-typing inside a generic body.

Type arguments are **explicit**: v1 has no type-argument inference, so every
generic reference spells its arguments (`gen_function[@bit32]`,
`GenObject[@bit32]`). Applying a generic with the wrong number of arguments —
too few, too many, or any argument on a non-generic declaration — is `E044`;
the bogus application is never built. A generic value's type carries its
arguments while its constructor names the bare declaration:
`x: GenObject[@bit32] = GenObject { value = 0x0 }`.

```lilac
GenObj[T]: object {
    value: T
}

GenFunc[T]: function (value: T) nothing {
    // Validated only at instantiation time
    value.call_something() 
}
```

Each concrete `(declaration, type arguments)` pair monomorphizes once to
concrete items, and verified IR is concrete-only (no generic declarations and
no surviving type parameter). A generic object method's `self` is the applied
instance `ref GenObj[T…]`, so `self.value` resolves to the substituted field
type. Duck-typed requirements are proven at instantiation in both receiver
forms: **value-based** `value.method()` (a method with `self`, where
`value: T`) and **static** `T.method()` (an associated function with no
`self`), each resolved against the concrete argument type.

A hook declared inside a generic body (for example an object's `@on_drop`)
monomorphizes with its parent and registers once per concrete instantiation,
so `GenBox[T]` can define `@on_drop` for each `GenBox[@bit32]`-style instance.
A top-level hook may not be generic: a blanket `@add[T](T, T)` stays `E052`.

### Inline Declarations

Inlined functions may omit the return name but are restricted to a single output type. The `=>` operator directly binds the expression output.

```lilac
inlined_const: @bit64 => 0x0
inlined_func: function (val: @bit8) @bit8 => @i_add_8(val, 0x1)
```

## 8. Operators, Conversions & RAII Hooks

Lilac uses designated `@` prefixed function names to bind global behaviors to types. Multiple bindings can be defined across a program provided their type signatures `(from, to)` or `(lhs, rhs)` remain unique.

Hook bindings must be concrete. No conversion, operator, or RAII hook may declare
generic parameters or mention an unsubstituted generic parameter in its signature —
a blanket `@add[T](T, T)` is rejected with `E052`. Compatibility checking is then
always an exact registry lookup over concrete signatures, never
instantiation-time proving. The one allowance is hooks declared inside a generic
body: they monomorphize with the parent and register once per concrete
instantiation, so `GenBox[T]` can still define `@on_drop` for each
`GenBox[@bit32]`-style instance. Hooks written explicitly against concrete
instantiations are ordinary concrete bindings.

### Operators & Strict Equality

Binary and unary operators desugar directly into function calls. For example, `a += b` desugars to `a = @add(a, b)`.

Operators never fall back to intrinsics implicitly: `a < b` requires a matching
`@less_than(lhs, rhs)` binding for the operand types, even when `@i_less_than_8`
would apply. The standard library provides these bindings once per bit width;
without them only the explicit intrinsic form works:

```lilac
@add: function (lhs: @bit8, rhs: @bit8) @bit8 => @i_add_8(lhs, rhs)
@less_than: function (lhs: @bit32, rhs: @bit32) result: @bit8 {
    result = @i_less_than_32(lhs, rhs)
}
```

**Equality (`==`) is strictly bit-equality and cannot be overridden.** Because universal zero-initialization guarantees that unused memory and struct padding are always zeroed, bitwise comparison is 100% deterministic. Custom comparison logic must be implemented as regular named methods to prevent hidden runtime complexity.

List of operators:
| op | function |
| --- | --- |
| **`lhs + rhs`** | `@add(lhs, rhs) output` |
| **`lhs - rhs`** | `@sub(lhs, rhs) output` |
| **`lhs * rhs`** | `@mul(lhs, rhs) output` |
| **`lhs / rhs`** | `@div(lhs, rhs) output` |
| **`lhs % rhs`** | `@mod(lhs, rhs) output` |
| **`lhs < rhs`** | `@less_than(lhs, rhs) output` |
| **`lhs > rhs`** | `@greater_than(lhs, rhs) output` |
| **`lhs <= rhs`** | `@less_than_or_equal(lhs, rhs) output` |
| **`lhs >= rhs`** | `@greater_than_or_equal(lhs, rhs) output` |
| **`-value`** | `@negate(value) output` |

Short-circuit `and` / `or` are builtin control flow, not overloadable hooks:
they evaluate left to right over bit containers and never resolve an operator
binding.

#### Operator hooks

Operator, `@conversion`, and RAII (`@init`/`@on_drop`) hooks are globally
indexed by `(name, signature)`: each declaration is a distinct overload keyed by
its parameter/return shape, so mixed-width bit algebra and custom-type operators
coexist in one table. The index is unit-global and scope-independent — a
top-level hook and an object-member hook share the same table — and the same
name with the same qualified signature anywhere in the unit is `E020`.
`@add(Matrix, Matrix) -> @bit32` simply binds the `Matrix` overload; there is no
operator-shape lint.

### Type Conversions

Conversions are unidirectional `from -> to` bindings, applied left-to-right:
the expected type (lval) is the anchor and the provided value (rhs) converts
toward it. A conversion is attempted only after an exact match fails.

```lilac
@conversion: function (from: CustomType) to: @bit8
```

Resolution order (normative):

1. **Exact match first.** `b: @bit8 = a` with `a: @bit8` copies directly, even if
   a `@conversion(from: @bit32) to: @bit8` exists. `a + b` with `@add(a, b)`
   defined calls it directly. Conversions never steal an exact match.
2. **Single directed conversion in typed contexts.** `b: @bit8 = a` with
   `a: @bit32` tries exactly one `@conversion(from: @bit32) to: @bit8` step.
   Ordinary call arguments behave the same way (parameter type is the anchor).
   No transitive chains in v1 (`u32 -> u16 -> u8` does not chain); chain requests
   are future work with a cycle guard.
3. **Operators and direct hook calls match exactly.** `a + b` requires an exact
   `@add(typeof(a), typeof(b))` overload; no operand conversion is attempted, so
   a miss is `E043` naming the exact pair. A direct `@hook(a)` call likewise
   requires an exact overload: a non-`ref` argument whose type is not the
   parameter type is `E010`. Numeric literals still take the parameter/operand
   type (literal typing, not conversion). Typed contexts (let/assign/return,
   f-string holes) and ordinary function-call arguments still convert as in
   step 2.

`==` never converts: equality is always direct bit-equality.

### Explicit casts with `as`

`expr as T` is a postfix cast, the explicit form of the one-step `@conversion`
path, usable in any expression position (including an operator operand, where
the implicit path does not apply). `as` is a **reserved keyword** and cannot be
an identifier. It is left-associative, binds tighter than binary operators and
looser than `.`/call/index: `a.b as T` is `(a.b) as T`, `f(x) as T` is
`(f(x)) as T`, `a + b as T` is `a + (b as T)`, and `x as A as B` is
`(x as A) as B` (two independent single-step casts).

`as T` supplies `T` as the operand's expected type, so the same one-step,
exact-match rule as the implicit path applies:

- a **literal** operand pins/grows to `T` with no conversion (`0x1 as @bit16`
  is `@bit16`; `5 as Int8` grows then converts; `3.14 as @fbit32` pins);
- a **typed** operand already exactly `T` is an identity no-op;
- otherwise exactly one `@conversion` from the operand's type to `T` is
  resolved (never a chain); a miss is `E043`.

The single builtin exception is the **`ref unknown` ↔ any `ref T`** cast, in
both directions: it is a no-op pointer retype with no `@conversion`.
`as ref unknown` is the only way to make a type-erased pointer from a concrete
`ref`, and `as ref T` is the way to recover one. Any other `ref` mismatch —
including `ref T as T` — is `E010`, consistent with the implicit path.

```lilac
@conversion: function (from: @bit32) to: @bit16 {
    to = @truncate(from)
}

main: function (v: @bit32) nothing {
    a: @bit16 = v as @bit16                          // one @conversion
    b: @bit16 = 0x1 as @bit16                        // literal pins, no conversion
    c: @bit16 = a as @bit16                          // identity (already @bit16)
    p: ref @bit32 = ref v
    q: ref @bit32 = (p as ref unknown) as ref @bit32 // no-op pointer round-trip
}
```

`as` and the implicit path share one conversion mechanism (`ops::convert`);
neither chains, both are exact-match, and a change to conversion rules applies
to both.

### Resource Management (RAII)

Memory lifecycle events trigger specific compiler hooks if defined for a type.

| Hook | Signature | Invocation Trigger & Behavior Rules | Desugar |
| --- | --- | --- | --- |
| **`@init`** | `(...params) Type` | Called immediately after object instantiation. Can be defined with custom parameters and invoked directly as a constructor using the syntax `Type(params)`. | `variable: Type(...params)` -> `variable: Type = @init(...params)` |
| **`@on_override`** | `(ref prev, ref new) nothing` | Called **before** an existing variable's value is overwritten. **Rule:** The `@on_drop` hook is automatically invoked on `prev` immediately afterwards, so you must **not** override or free anything in `prev` during `@on_override`. | `variable = new` -> `@on_override(ref variable, ref new) @on_drop(ref variable) variable = new` |
| **`@on_copy`** | `(ref Type) new_copy` | Called when a value is copied. You are manually responsible for creating and returning the new copied value (`new_copy`). **Rule:** Extreme care must be taken not to recursively trigger the copy mechanism while constructing `new_copy`. | `copied: Type = variable` -> `copied: Type = @on_copy(ref variable)` |
| **`@on_drop`** | `(ref Type) nothing` | Called when a value exits scope or a reference is freed. Used for resource deallocation and cleanup. | `variable: Type()` -> `variable: Type() defer @on_drop(ref variable)` |

Example of defining custom constructors and RAII rules

```lilac
Buffer: object {
    ptr: ref @bit8

    // Custom constructor called via Buffer(size)
    @init: function (self, size: @bit32) nothing {
        // allocation logic
    }

    // Called before override; do not free 'prev' here as @on_drop handles it!
    @on_override: function (prev: ref Buffer, new: ref Buffer) nothing {
        // transfer logic or immutability enforcement
    }

    // Responsible for generating the copy without causing infinite copy recursion
    @on_copy: function (self) new_copy: Buffer {
        // safe copy logic
        return
    }
}
```

## 9. Foreign functions and async

### extern (imports)

`extern` qualifies a declaration: a signature with no body, resolved to a
foreign symbol at link time.

```lilac
malloc: extern function (size: @bit_native) ptr: ref @bit8
```

Rules: an `extern` uses the platform C ABI. Each parameter maps to its C type:
a bit integer (`@bit8`/`@bit16`/`@bit32`/`@bit64`/`@bit_native`) by value, and
any `ref T` to an opaque pointer. A single return slot becomes the C return
value; no return slot is `void`. More than one out, or a parameter/return
outside that set (an aggregate such as `@byte_buffer`, say), is `E028`,
validated before codegen so it reports as an ordinary diagnostic. An `extern`
may not be named `main`: extern symbols are unmangled, so the C symbol `main`
would collide with the entry point the LLVM backend emits, and that is also
`E028`. The symbol is
unmangled and equals the declared name; calls typecheck only against the
declared signature (a wrong declaration is user error and is diagnosed
nowhere — declare carefully); libraries to link (`link = ["c"]`) are declared
in the package manifest, never in code. No overloads, no calling-convention
  annotations in v1.

### export (reserved)

The dual direction — foreign code calling into Lilac (thread entry points,
callbacks) — needs exported symbols. The `export` keyword is reserved for this
and unspecified in v1. Until it lands, `--export=<name>` on the CLI (repeatable)
seeds the reachability roots alongside `main`, so a named function is kept alive
even when nothing calls it; it does not change the ABI or emit a C-callable
symbol.

### Async lives in the standard library

Async needs no compiler support beyond what exists: futures are objects driven
by `loop` + `match`, polling fits multi-return (`ready, value = task.poll()`),
executors accept any type with a structural `poll` via duck-typed generics, and
`@on_drop` covers task cancellation. Suspending a live call stack — the one
thing Lilac cannot express in itself — is sourced through `extern` (platform
context-switch helpers or a tiny C shim via the build script), never through a
compiler intrinsic. `await` syntax is explicitly deferred: if ever added, it
desugars to existing constructs the way f-strings do.

## Lilac to IR (informative; `IR_SPEC.md` is normative)

Lowering follows the IR v2 rules in `IR_SPEC.md`: typed SSA registers (`&n: #type`),
explicit `$label:` blocks each ending in one terminator, `alloca : zeroed`,
verified `call` arity, named `field`, and folding before verification. Examples
below use canonical v2 text.

Binary literal:
```lilac-ir
&1: #bit32 = const 0x1:#bit32
```
String literal (`"hi"` with no heap allocation): a `data` const plus a
data-backed buffer value (`const !cN:#byte_buffer`), never `@byte_buffer_from`:
```lilac-ir
data { !c7: str = "hi" }
&1: #byte_buffer = const !c7
```
`String` construction from there is ordinary user-land conversion (`@conversion`
search); f-strings additionally use `@format_args_*` per §2.
Function call with multi-return (`val1, val2 = multi_func()`):
```lilac-ir
$entry:
  call @multi_func (out %val1, out %val2)
  return
```
Assignment and member access use `copy` / `field .name` as defined in `IR_SPEC.md`.
`@size_of` (a fold-required marker) on constant operands, `branch.if` on
constant conditions, and `compile.error` in dead branches never reach verified
IR (see Compile-time folding above).