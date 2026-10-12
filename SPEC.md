# The Lilac Programming Language Specification

> Version: 0.7-draft (ownership and drop order, `@bit8` conditions, library mounting, `=>` as always-inline functions). Status: draft, normative for the Rust rewrite.
> Normative documents: `SPEC.md` (language), `IR_SPEC.md` (IR v2).
> `docs/history/RUST_REWRITE_PLAN.md` is non-normative (execution plan only).
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
* **`@format_args(W)`**: The value of an f-string (§2): two pointers, one
  to a static template (the literal pieces and, per hole, the `@format`
  hook that writes it into a `W`) and one to the holes' addresses on the
  stack. Nothing is formatted until a consumer walks it. `W` is any type;
  the compiler never looks inside it. A format borrows, so only locals and
  parameters hold one (`E072`).

### Special Keywords

* **`unknown`**: An opaque type of indeterminate size. It can only be instantiated behind a reference (`ref unknown`), serving as Lilac's mechanism for type-erased pointers (analogous to `void*`).

* **`nothing`**: A parse-time keyword used exclusively in a function's return slot to indicate zero return variables (and in `else nothing`: a function that can fail with no failure outs, §4). It is not a type and occupies no memory.

* **`self`**: A function-prototype-only type. It always desugars to `self: ref SelfType` — a reference to the enclosing object type, never a by-value copy. `instance.Method()` desugars to `Object.Method(ref instance)`.

### Fixed-size arrays

* **`@array(T, N)`**: `N` values of `T` in a row, laid out like a C array
  (size `N` times the size of `T`, aligned to `T`). `N` is a number literal
  from 1 to `0xffffffff`. `T` is held by value: a `ref` element is `E010`
  (wrap it in an object), and so is an element with an RAII hook, itself or
  in a field, because an array copies and drops as plain bytes.
* An array value is written `@array(T, N) {}` and starts zeroed, like all
  memory; `name: @array(T, N) {}` declares one. Elements are reached through
  `@array_at` and the length through `@array_len` (see §3); there is no
  bracket syntax.

```lilac
Buffer: object {
    used: @bit_native
    bytes: @array(@bit8, 64)
}

buffer: Buffer { used = 0x0, bytes = @array(@bit8, 64) {} }
first: ref @bit8 = @array_at(ref buffer.bytes, 0x0)
first = 0x41
```

### Universal Zero-Initialization

All allocated memory whether on the stack, heap, or within complex objects is automatically **0 initialized** by the compiler. There is no uninitialized memory in Lilac: padding bits are deterministic, ensuring 100% reliability for bit equality checks. Zeroed storage is never a value, though: a place nothing has set yet is *empty* (see below), and reading it is an error rather than a zero.

### Ownership: copies, moves and drops

Every value has exactly one owner (a local, a by-value parameter, an out,
a field of one of those, or a statement temporary). A `ref` never owns
what it points at and never drops it. The compiler tracks every place it
owns as **full** (it holds a live value) or **empty**, along every path
through the function:

* **Copy or move, by type.** Reading a place into a new owner (`b: T = a`,
  a by-value argument or receiver, a field initializer, an assignment)
  copies it when its type copies, and moves it otherwise. A type with its
  own `@on_copy` copies through the hook; a type with its own `@on_drop`
  and no `@on_copy` moves; any other type copies field by field when every
  field (or payload) copies, and moves when one does not. Builtins, `ref`s,
  function values and arrays copy bitwise. A fresh value (a call result, a
  literal) moves into its owner.
* **`@move(a)`** moves any place, copyable or not, without its copy hook
  (hand an `Rc` over without a bump). `a` is empty after.
* **Empty places.** A place is empty after it moves, and an out is empty
  until it is set. Reading an empty place, or one that is empty on some
  path (moved in one branch, or in an earlier iteration of a loop), is
  `E063`, and the error points at the move. Setting a place again fills
  it.
* **What can move.** Only owned places move: a local, a by-value
  parameter, or a field of one, never through a `ref`, out of a global, or
  out of a field of a temporary (`E064`). A field moves out only of a type
  with no `@on_drop` of its own; the object can then not be used whole,
  its other fields can, and those still full drop with it. Setting the
  field again makes the object whole.
* **Drops:** at the end of its block (or at the `return`, `fail`, `break`
  or `continue` leaving it) each local that is still full is dropped, in
  reverse declaration order; an empty one is skipped, and one that is full
  on some paths only is dropped when a hidden flag says so. Statement
  temporaries are dropped at the end of their statement. Dropping an
  object runs its `@on_drop` first, then drops its fields in declaration
  order; dropping an enum drops its live payload. A type therefore only
  manages what it allocated itself.
* **Assignment** (`place = value`) evaluates `value` first. A full place
  then runs `@on_override(ref place, ref value)` if defined and drops the
  old value; an empty place (an out's first assignment) is just filled.
  Then the new value moves in. Memory behind a `ref` is always full.
* **Outs** are set on every path that returns (`E065`): success outs at
  each `return` and the end of the body, failure outs at each `fail`. An
  out of the other side that is full there is dropped. Paths ending in
  `@panic`, `@trap` or `@compile_error` are exempt.
* **Ending a value without its `@on_drop`.** Inside a function declared
  in a type `T`, `@discard(x)` (`x` an owned `T`) ends `x`'s life without
  running `T`'s `@on_drop`: its fields become places of their own, so the
  function can move out what it needs, and the fields still full drop at
  the end of the scope as usual. Using `x` whole after is `E063`; anywhere
  else, or on some paths only, it is `E066`. So for every other caller
  `@on_drop` is a guarantee.

A value with no hooks anywhere inside it is copied and dropped bitwise, at no
cost.

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
defaults to `@fbit64`; an expected `@fbit16/32/64/128` (or a numeric object's
`@conversion(from: @fbitN)`) pins it. Its exact decimal value is rounded
once, to nearest with ties to even, to that type (IEEE 754 binary16, 32, 64
or 128), so `0.1` as an `@fbit128` has all 113 bits of `0.1`; a value too
small for the smallest subnormal is zero. A value that rounds past the type's
largest finite value is `E060`. A float literal in a bit or integer slot is
`E010` (no implicit float→int). Unary `-` is an operator, not part of the
literal; reading digits from a runtime `@byte_buffer` is standard-library work,
not a compiler intrinsic.

Arithmetic between two literals of the same kind is folded at compile time: a
binary `+ - * / %` whose **both** operands are integer literals folds at 64-bit,
and whose both operands are float literals folds at the float type the
expression takes (by the rules above), as IEEE 754 computes it at run time:
each literal is rounded to that type, each result is the exact one rounded
once, and `%` is C's `fmod`. So `x: @fbit32 = 0.1 + 0.2` has the bits the
same `@fbit32` addition would give. The result is a
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
  after `\` is a lex error. A string is a byte sequence: `\xHH` is exactly one
  raw byte (`"\xFF"` has length 1), and other characters contribute their
  UTF-8 bytes.
* Chars: `'…'` holds exactly one scalar. Empty or multi-scalar char literals are
  lex errors; use a `String` for those.
* A string, char or f-string literal ends on its line: one its line ends
  inside is a lex error. (An f-string hole is code, and may span lines.)
* A UTF-8 byte-order mark (U+FEFF) at the start of a file is whitespace;
  anywhere else it is an unexpected character.

### Bracket reservation

`[` and `]` are reserved for generics (`GenObj[T]`, `gen_function[@bit32](...)`).
There is no `a[i]` indexing, `a[i:j]` slicing, or `[1, 2, 3]` array literal
syntax. Element access uses methods, exactly like `+` desugars to `@add`:

```lilac
first: @bit8 = s.at(0x0)
view: String = s.slice(0x0, 0x2) // new {Size, Buffer} view, no copy
```

Fixed-size storage is the builtin `@array(T, N)` (§1), reached through
`@array_at`/`@array_len`, not `T[N]` syntax.

### Formatted strings

An f-string (`$"Hello {name}!"`) is data, not text: an `@format_args(W)`,
two pointers.

- **The template** (static, read-only, one per distinct f-string): the
  literal pieces and, for each hole, its formatter, the `@format` hook of
  the hole's type for `W` (§8), found at compile time.
- **The holes** (on the stack): one pointer per hole, to the value it
  borrows.

Nothing is formatted when the f-string is evaluated: its holes are
evaluated, once each and left to right, and their addresses stored. A
consumer formats it, as often as it likes (zero times when a log level
is off). Holes are full expressions (`{count.hex()}`), checked with no
expected type; `{{` escapes to `{` and `}}` to `}`. There is no spec
syntax: a hole's top-level `:` or `,` is `E001`, and a variation is a
value with its own `@format` (`{count.hex()}`, or a function over a
nested format: `{pad_left($"{name}", 10)}`).

`W` is the type every formatter in the template writes into, and it comes
from where the f-string goes, the way a number literal's width does:

1. An expected `@format_args(W)` (or an alias of one) gives `W`.
2. Any other expected type takes the `W` of its one `@conversion` from an
   `@format_args(W)`, and the f-string converts through it (`text: String
   = $"..."`). With none it is `E043`; with two (from two writer types)
   it is `E073`.
3. With nothing expected (a statement, an operand) it is `E073`.

A hole of type `T` (or `ref T`) uses the `@format(ref T, ref W)` hook,
found by its exact signature like every hook; a type with none is `E071`
at the hole. The compiler formats nothing itself: a `@byte_buffer` or
nested-format hole works because a library defines that hook.

In an `@format_args(W)` slot, and only there (never in a type that merely
converts from a format), two more forms are formats: a string literal is
a format with no holes, and any other value is the one-hole format
`$"{value}"`. So `out.write("hi")`, `out.write(count)` and
`out.write($"{count} items")` all work. The slot rules apply where
conversions do (typed contexts and call arguments), never to an
operator's or a direct hook call's arguments.

Three intrinsics read a format (§3); a library walks it with them:

```lilac
Out: object {
    write: function (self, text: @format_args(Out)) nothing {
        last: @bit_native = @format_args_holes(text)
        index: @bit_native = 0x0
        loop {
            write_bytes(@format_args_piece(text, index))
            if index == last { break }
            @format_args_hole(text, index, self)
            index = @i_add_native(index, 0x1)
        }
    }
}
```

A format borrows twice (its holes point at values, and its hole pointers
sit in the frame that built it), so it stays in that frame, with no
borrow checker (`E072`):

- Only locals and parameters hold one, never a field, enum payload, array
  element, out, global or constant. A `ref` to one may be anywhere a
  `ref` may.
- It is never stored through a `ref` or with `@write`.
- Holes are borrowed, never moved or copied: `$"{s}"` leaves `s` full. A
  place hole must not be outlived by the format, as for any `ref`.
- A temporary hole (a call result, a literal) lives until the end of its
  statement, except in the initializer of a new local: each hole is then
  evaluated into a hidden local declared just before it, so its
  temporaries live, and drop, with the local's block. A format with a
  temporary hole assigned to an existing local, or as an arm of a value
  `if` or `match`, is `E072`; declare a local for it.

The compiler names no library entity anywhere in this: `W`, its hooks and
every consumer are library code, so whether an f-string allocates is the
consumer's choice, never hidden sugar.

### Constant conditions and `@compile_error`

Two rules decide whether a `@compile_error(msg)` is an error:

1. **Reachability.** A `@compile_error` is `E031` exactly when the function
   containing it is reachable from the program's roots (`main` and every
   `export`) after generic instantiation. One in an uncalled function or an
   unused hook is silently dropped. The `E031` diagnostic notes one shortest
   call chain from a root, innermost first, in stack-trace form
   (`at Holder[@bit32].make (main.lilac:5:9)`); compiler glue frames are
   hidden, and a frame that only takes the next function's address is marked
   `[address taken]`.
2. **Constant conditions.** An `if` (statement or value), an `and`/`or`
   operand, or a `match` whose condition or scrutinee is a *constant
   expression* has only its live arm compiled. The dead arm's calls never
   happen, so they make nothing reachable, and its `@compile_error`s never
   exist.

An expression is constant when it is one of:

| Form | Value |
| --- | --- |
| an integer literal | its bits at the expression's width |
| `@size_of(T)` | the size of `T` (per generic instance) |
| `@align_of(T)` | the alignment of `T` (per generic instance) |
| an inline constant (`name: T => expr`) | its initializer, which must be constant |
| a `@builtin` constant (`@builtin.Target.os`) | the target or build fact it states |
| an object or enum-variant literal | if every field / the payload is constant |
| `value.field` | the field of a constant object |
| `a == b` | bit equality of two constant values of one type (integers, objects, enum variants with their payloads) |
| `if c then x else y` | the arm a constant `c` selects (the other is not evaluated) |
| `a and b`, `a or b` | short-circuit over a constant `a` (`b` only when it decides) |
| a value `match` | the arm a constant scrutinee selects (a variant pattern binds its capture), else the `else` value |
| an integer intrinsic | `@i_*` arithmetic and comparisons, `@truncate`, `@extend_zero`, `@extend_sign`, over constant arguments |
| a call to a `=>` function | with constant arguments: its body, with the parameters bound |

and nothing else. In particular:

- **A local is never constant**, even one initialized from a constant and
  never assigned: `t: Bool = false; if t { … }` keeps both arms.
- **Every value involved must be hook-free**: a value of a type with an
  `@on_drop` or `@on_copy` (directly or through a field) is not constant,
  since skipping it would skip the hook.
- **A recursive `=>` call** (to a `=>` function already being expanded) is not
  constant, so evaluation always terminates. There is no step limit.
- Integer arithmetic wraps like the runtime operation; a division or
  remainder that panics (see [Integer division](#integer-division)) is not
  constant. Float arithmetic is never constant.

Not being constant is never an error: the condition is simply a runtime
branch. The one exception is a division that would panic, which is `E060`
wherever it is compiled ([Integer division](#integer-division)).
Optimizing constant arithmetic and calls elsewhere is the backend's job
(LLVM folds them at `-O1` and above) and never changes which programs
compile.

`true` and `false` in `Standard` are inline constants built from `=>`
functions over a hook-free `Bool`, so `if true { … } else { … }` compiles one
arm. The former camelCase alias `@compilerError` is no longer accepted.

### The `@builtin` namespace

The compiler supplies one namespace of facts about the target and the build.
It needs no `using`, and since `@` names are reserved it cannot collide with
user code. Its names are always written qualified:

```lilac
@builtin.Os: enum { Windows, Linux, MacOs, None }
@builtin.Arch: enum { X86, X86_64, Arm, Aarch64, Riscv32, Riscv64, Wasm32, Wasm64 }
@builtin.Mode: enum { Debug, Release, ReleaseSmall }

@builtin.Target.os: Os                    // None for bare and unknown targets
@builtin.Target.arch: Arch
@builtin.Target.pointer_size: @bit_native // in bytes
@builtin.Target.page_size: @bit_native    // 16 KiB on Apple arm64, 64 KiB on wasm, else 4 KiB
@builtin.Target.libc: @bit8               // 0x1 when the C library is linked

@builtin.Build.mode: Mode
```

Every `Target` and `Build` member is an inline constant, so a `match` or
`if` on one compiles only the live arm (see Constant conditions above). A
`@compile_error` in another platform's arm is never an error:

```lilac
write: function (fd: @bit32, text: @byte_buffer) nothing {
    match @builtin.Target.os {
        .Windows => write_windows(fd, text)
        .Linux => write_linux(fd, text)
    } else {
        @compile_error("write: unsupported OS")
    }
}
```

`libc` is set for `--libc` builds, on macOS (which links `libSystem`), and for
`check`/`emit-*`, which compile with a C entry. The namespace's source is
generated per compile, and diagnostics name its file `<builtin>`. A library
cannot be mounted under an `@` name.

## 3. Compiler Intrinsics

Intrinsics represent the sole mechanisms for raw arithmetic, float manipulation, and bit-level introspection.

Numeric intrinsic names are width-suffixed: `_8`/`_16`/`_32`/`_64` select the `@bit`/`@fbit` container of that width and `_native` selects `@bit_native`. Below, `W` is any suffix valid for that family (integer widths 8/16/32/64/native; float widths 16/32/64/128) and `@bitW`/`@fbitW` the matching container.

| Intrinsic | Signature | Description |
| --- | --- | --- |
| **`@i_add_W`** | `(@bitW, @bitW): @bitW` | Integer addition using standard two's-complement logic. |
| **`@i_sub_W`** | `(@bitW, @bitW): @bitW` | Integer subtraction. |
| **`@i_mul_W`** | `(@bitW, @bitW): @bitW` | Integer multiplication. |
| **`@i_div_W`** | `(@bitW, @bitW): @bitW` | Signed integer division, truncating toward zero. Panics on a zero divisor or `MIN / -1` ([Integer division](#integer-division)). |
| **`@i_div_u_W`** | `(@bitW, @bitW): @bitW` | Unsigned integer division. Panics on a zero divisor. |
| **`@i_mod_W`** | `(@bitW, @bitW): @bitW` | Signed integer remainder, with the dividend's sign. Panics on a zero divisor or `MIN % -1`. |
| **`@i_mod_u_W`** | `(@bitW, @bitW): @bitW` | Unsigned integer remainder. Panics on a zero divisor. |
| **`@i_negate_W`** | `(@bitW): @bitW` | Integer two's-complement negation. |
| **`@i_less_than_W`** | `(@bitW, @bitW): @bit8` | Signed integer less-than (two's-complement). Returns `@bit8` (`0x0` false, otherwise true). |
| **`@i_greater_than_W`** | `(@bitW, @bitW): @bit8` | Signed integer greater-than. Returns `@bit8`. |
| **`@i_less_than_u_W`** | `(@bitW, @bitW): @bit8` | Unsigned integer less-than. Returns `@bit8`. |
| **`@i_greater_than_u_W`** | `(@bitW, @bitW): @bit8` | Unsigned integer greater-than. Returns `@bit8`. |
| **`@i_less_than_or_equal_W`** | `(@bitW, @bitW): @bit8` | Signed integer less-or-equal. Returns `@bit8`. |
| **`@i_greater_than_or_equal_W`** | `(@bitW, @bitW): @bit8` | Signed integer greater-or-equal. Returns `@bit8`. |
| **`@i_less_than_or_equal_u_W`** | `(@bitW, @bitW): @bit8` | Unsigned integer less-or-equal. Returns `@bit8`. |
| **`@i_greater_than_or_equal_u_W`** | `(@bitW, @bitW): @bit8` | Unsigned integer greater-or-equal. Returns `@bit8`. |
| **`@bit_and_W`**, **`@bit_or_W`**, **`@bit_xor_W`** | `(@bitW, @bitW): @bitW` | Bitwise and, or, exclusive or. |
| **`@bit_not_W`** | `(@bitW): @bitW` | Bitwise complement. |
| **`@bit_shift_left_W`** | `(@bitW, amount: @bitW): @bitW` | Shifts left by `amount` modulo the width (so a shift is never undefined). |
| **`@bit_shift_right_W`** | `(@bitW, amount: @bitW): @bitW` | Arithmetic shift right (copies the sign bit), by `amount` modulo the width. |
| **`@bit_shift_right_u_W`** | `(@bitW, amount: @bitW): @bitW` | Logical shift right (shifts in zeros), by `amount` modulo the width. |
| **`@f_add_W`** | `(@fbitW, @fbitW): @fbitW` | Floating-point addition using IEEE-754 bit interpretation. |
| **`@f_sub_W`** | `(@fbitW, @fbitW): @fbitW` | Floating-point subtraction. |
| **`@f_mul_W`** | `(@fbitW, @fbitW): @fbitW` | Floating-point multiplication. |
| **`@f_div_W`** | `(@fbitW, @fbitW): @fbitW` | Floating-point division. |
| **`@f_mod_W`** | `(@fbitW, @fbitW): @fbitW` | Floating-point modulo. |
| **`@f_negate_W`** | `(@fbitW): @fbitW` | Float negation (IEEE-754 sign flip). |
| **`@f_less_than_W`** | `(@fbitW, @fbitW): @bit8` | Float less-than (IEEE-754). Returns `@bit8`. |
| **`@f_greater_than_W`** | `(@fbitW, @fbitW): @bit8` | Float greater-than (IEEE-754). Returns `@bit8`. |
| **`@f_less_than_or_equal_W`** | `(@fbitW, @fbitW): @bit8` | Float less-or-equal (IEEE-754; false when either is NaN). Returns `@bit8`. |
| **`@f_greater_than_or_equal_W`** | `(@fbitW, @fbitW): @bit8` | Float greater-or-equal (IEEE-754). Returns `@bit8`. |
| **`@fbit_to_bits_W`** | `(@fbitW): @bitW` | Reinterprets a float's bits as the same-width integer (`W` ∈ 16/32/64 only: `@bit128` does not exist). |
| **`@bits_to_fbit_W`** | `(@bitW): @fbitW` | Reinterprets integer bits as the same-width float (`W` ∈ 16/32/64 only). |
| **`@size_of`** | `(T)` or `(value)` | The size in bytes of a type, or of a value's type, as a `@bit_native` constant (target C layout: fields in order, each aligned, padded to the largest alignment). Inside a generic body it is the size of the instantiated type. |
| **`@align_of`** | `(T)` or `(value)` | The alignment in bytes of a type, or of a value's type, as a `@bit_native` constant (the same target C layout as `@size_of`). |
| **`@truncate`** | `(value)` | Narrows a bit container; target width comes from the expected type (left-to-right). |
| **`@extend_zero`** | `(value)` | Zero-extends a bit container; target width comes from the expected type (left-to-right). |
| **`@extend_sign`** | `(value)` | Sign-extends a bit container (copies its top bit into the new bits); target width as `@extend_zero`. |
| **`@load_u8`**
 | `(buffer, offset)` | Loads one byte from `ref @bit8` at `@bit_native` byte offset. Returns `@bit8`. |
| **`@store_u8`** | `(buffer, offset, value)` | Stores one `@bit8` value into `ref @bit8` at `@bit_native` byte offset. |
| **`@memcpy`** | `(dst, src, len)` | Copies `len` bytes (`@bit_native`) from `ref @bit8` to `ref @bit8`. |
| **`@memmove`** | `(dst, src, len)` | `@memcpy` for ranges that may overlap. |
| **`@memset`** | `(dst, byte, len)` | Fills `len` bytes (`@bit_native`) at `ref @bit8` with `byte` (`@bit8`). |
| **`@ptr_add`** | `(ptr, offset)` | The `ref unknown` that is `offset` bytes (`@bit_native`) past `ptr` (`ref unknown`). |
| **`@ptr_to_address`** | `(ptr)` | The address of `ptr` (`ref unknown`) as a `@bit_native`. |
| **`@address_to_ptr`** | `(address)` | The `ref unknown` at `address` (`@bit_native`); `@address_to_ptr(0x0)` is null. |
| **`@linux_syscall`** | `(number, a0, a1, a2, a3, a4, a5)` | The Linux system-call instruction: system call `number` with six `@bit_native` arguments (pass `0x0` for unused ones). Returns the result, or `-errno`, as `@bit_native`. Only on Linux for x86-64 and aarch64; a reachable call on any other target is `E035`. |
| **`@linux_thread_clone`** | `(flags, stack, parent_tid, child_tid, tls_area, tls_size, entry, argument)` | A Linux thread from the `clone` system call: `flags` (`@bit_native`) as `clone`'s, the child on `stack` (its top, `ref unknown`), `parent_tid`/`child_tid` as `clone`'s, and its thread-local block laid out in `tls_size` zeroed bytes at `tls_area` (the thread pointer it gets with `CLONE_SETTLS`). The child calls `entry` (a C function's address, `ref unknown`: an `export` converted with `as`) with `argument`, then exits. Returns the child's id or `-errno` (`-ENOMEM` when the area is too small). Linux x86-64 and aarch64 only, like `@linux_syscall` (`E035`). |
| **`@trap()`** | `()` |
 Stops the program at once with the target's trap instruction (an abnormal end, not an exit code). Does not return. |
| **`@atomic_load_W`** | `(ref @bitW): @bitW` | Atomic load. |
| **`@atomic_store_W`** | `(ref @bitW, @bitW)` | Atomic store. |
| **`@atomic_swap_W`** | `(ref @bitW, @bitW): @bitW` | Atomically replaces the value; returns the old one. |
| **`@atomic_compare_swap_W`** | `(ref @bitW, expected: @bitW, desired: @bitW): @bitW` | Atomically replaces the value with `desired` if it equals `expected`; returns the old value (it succeeded iff that equals `expected`). Strong: it never fails spuriously. |
| **`@atomic_add_W`**, **`_sub_`**, **`_and_`**, **`_or_`**, **`_xor_`**, **`_max_`**, **`_min_`**, **`_max_u_`**, **`_min_u_`** | `(ref @bitW, @bitW): @bitW` | Atomic read-modify-write (`max`/`min` signed, `max_u`/`min_u` unsigned); returns the old value. |
| **`@atomic_fence`** | `()` | A memory fence. |
| **`@array_at`** | `(ref @array(T, N), index: @bit_native): ref T` | A reference to element `index`. An index of `N` or more panics with `index out of bounds`; a constant index in bounds is not checked at run time, and a constant index out of bounds is `E037`, in every function, called or not (an arm a constant condition rules out is not checked). |
| **`@array_len`** | `(T)` or `(value)` | The length `N` of an array type, or of a value's array type, as a `@bit_native` constant. |
| **`@byte_buffer_ptr`** | `(buffer)` | Returns the `ref @bit8` pointer to the first byte of a `@byte_buffer`. |
| **`@byte_buffer_len`** | `(buffer)` | Returns a `@byte_buffer` length as `@bit_native`. |
| **`@byte_buffer_from`** | `(ptr, len)` | Builds a borrowing `@byte_buffer` view over `ref @bit8` plus `@bit_native` length. Same lifetime caveat as any `ref`. |
| **`@format_args_holes`** | `(args)` | The hole count of an `@format_args(W)` as `@bit_native`; there is one more piece. |
| **`@format_args_piece`** | `(args, index)` | The `index`-th literal piece (`0..=holes`) as a borrowing `@byte_buffer`. Out of bounds panics. |
| **`@format_args_hole`** | `(args, index, out)` | Writes hole `index` (`0..holes`) into `out: ref W` through its `@format` hook. Out of bounds panics. |
| **`@compile_error`** | `(message)` | Creates a compile-time error if the compiler hits it logging the message. |
| **`@move`** | `(place)` | Moves an owned place out, whatever its type, without its copy hook (§1). |
| **`@write`** | `(ref T, value: T)` | Moves `value` into the slot at the address, which must be empty: nothing there is dropped. |
| **`@read`** | `(ref T): T` | Moves the `T` at the address out, bit for bit (no copy hook), leaving the slot empty. |
| **`@drop`** | `(ref T)` | Drops the `T` at the address, leaving the slot empty. |
| **`@needs_drop`** | `(T)` or `(value)` | Whether dropping a `T` does anything (its own `@on_drop`, or a field or payload that needs a drop), as a `@bit8` constant. |
| **`@discard`** | `(place)` | Ends an owned value's life without its `@on_drop`, inside a function of its type (§1, `E066`). |

Atomics are sequentially consistent unless the name carries an ordering
before the width: `@atomic_<op>_<order>_W` and `@atomic_fence_<order>`, with
`relaxed`, `acquire`, `release` or `acq_rel`. A load cannot be `release` or
`acq_rel`, a store cannot be `acquire` or `acq_rel`, and a fence cannot be
`relaxed`; such a name is not an intrinsic (`E021`). Operands must be exactly
`@bitW`, the address is a `ref @bitW` (`ref place`), and every access is
aligned to its width. A compare-swap's failure ordering is the strongest one
valid for its ordering (`acquire` for `acq_rel`, `relaxed` for `release`).

### Integer division

Integer arithmetic wraps in two's complement, with one exception: division
and remainder have no wrapped answer, so they panic instead, in every build
mode and on every target:

- `@i_div_W`, `@i_div_u_W`, `@i_mod_W` and `@i_mod_u_W` with a zero divisor
  panic with `division by zero`;
- `@i_div_W` and `@i_mod_W` of the minimum signed value (`0x80…0`) by `-1`
  (all ones) panic with `division overflow` (the quotient does not fit, and
  the remainder, though mathematically `0`, is computed by the same
  division).

Otherwise signed division truncates toward zero and the signed remainder
takes the dividend's sign (`-7 / 2` is `-3`, `-7 % 2` is `-1`). The panic is
an ordinary `@panic` whose site is the intrinsic call, so it prints the
message and stack trace and exits with `101`. `Standard`'s `/` and `%` on its
integer types call these intrinsics, so they panic the same way. The check
costs nothing when the divisor is a constant that rules it out (any nonzero
constant for an unsigned division, any but `0` and `-1` for a signed one): it
is not emitted.

A division that panics whatever runs before it is a compile-time error,
`E060`, wherever it is compiled (in every function, called or not, but never
in an arm a constant condition rules out):

- a constant zero divisor, whatever the dividend;
- a constant minimum signed value divided by a constant `-1`;
- a call to a `=>` function whose body reaches one of these with the call's
  constant arguments (`value / 0x0 as Int32` through `Standard`'s `@div`);
- a global's initializer that divides that way.

Literal arithmetic (`1 / 0`, §2) is folded before any type is known and is
the same `E060`.

The raw memory builtins (`@write`, `@read`, `@drop`) are for buffer and
allocator authors: they move values in and out of memory the compiler does
not track (behind a `ref`), so whether a slot is full is up to the caller.
Assignment through a `ref` (`p = value`) is for a full slot (it drops the
old value); `@write` is for an empty one. They need a `ref` to a known type
(`E010` for `ref unknown`: there is no type to move or drop). `@needs_drop`
is a constant, so `if @needs_drop(T)` lowers only the live arm.

`@size_of` and `@align_of` are computed during lowering and are always
constants. Pointer math goes through `ref unknown`: cast a typed reference
to it (`ref value as ref unknown`), move it with `@ptr_add`, and cast it back
(`as ref T`), which auto-dereferences like any reference.
The compiler has no operating-system intrinsics. OS access is library code:
`extern`s and `@linux_syscall`s chosen by `@builtin.Target` (`Standard.Os` uses
`kernel32` on Windows, system calls on Linux, and the C library on macOS,
where Apple supports no other), so programs do not depend on a separate C
library. What the compiler itself needs from the platform, it asks for
through the platform hooks (§8).
`@compile_error` is a **compile-time marker**, not a runtime builtin call: it
is never compiled in the dead arm of a constant condition, it is removed with
its function when that function is unreachable, and otherwise it is reported
as `E031` (see Constant conditions and `@compile_error`); a survivor reaching
IR verification is an internal error. Builtin calls that
do reach IR are checked against the builtin signature table; `@load_u8` takes
two in-args (`ref #bit8`, `#bit_native`) and yields `#bit8`, while `@store_u8`
takes three in-args (`ref #bit8`, `#bit_native`, `#bit8`) and yields nothing.

## 4. Functions & Control Flow

### Function Signatures & Multi-Return

To return values from a function, named return variables **must** be specified in the signature. The `return` keyword takes no arguments; it simply returns the current state of the named return variables. Each starts empty and must be set on every path that returns (§1, `E065`).

```lilac
multi_func: function () result_1: @bit8, result_2: @bit16 {
    result_1 = 0x1
    result_2 = 0x2
    return
}

val1: @bit8, val2: @bit16 = multi_func()
```

A caller need not bind every result. The bindings take the results left to
right, and the results past them are dropped at the end of the statement
(`@on_drop` runs, so nothing leaks). A call used as a value is its first
result, and a call statement drops all of them:

```lilac
val1: @bit8 = multi_func()   // result_2 is dropped
multi_func()                 // both are dropped
```

Binding more values than the call returns, or using a call that returns
`nothing` as a value, is `E022`.

### Functions that can fail

A function that can fail declares its failure outs after `else`, following
its success outs (`else nothing` when a failure carries no value):

```lilac
get: function (self) value: T else nothing
map: function (size: @bit_native) memory: ref unknown, length: @bit_native else error: Error
```

`return` (or reaching the end of the body) succeeds; `fail` fails. Like
`return`, `fail` takes no arguments: set the failure outs first. `fail` in
a function that cannot fail is `E058`.

```lilac
halve: function (n: @bit32) half: @bit32 else odd: @bit32 {
    if @i_less_than_u_32(@i_mod_u_32(n, 0x2), 0x1) {
        half = @i_div_u_32(n, 0x2)
        return
    }
    odd = n
    fail
}
```

A call that can fail must be handled where it happens, in one of three
forms; anywhere else it is `E057` (there is no automatic propagation: to
pass a failure on, handle it and `fail`). Each binds outs **by position**,
under any names; binding fewer than there are drops the rest, and more is
`E062`:

```lilac
// Guard: the success outs land in the current scope. The `else` block
// (with optional `-> captures` of the failure outs) must leave: it ends in
// `return`, `fail`, `break`, `continue`, `@panic` or `@trap` (E059).
half: @bit32 = halve(n) else -> odd: @bit32 {
    return
}
halve(n) else { return }                // the statement form binds nothing

// If: success outs in the `then` block, failure outs in the `else` block.
if halve(n) -> half: @bit32 {
    ...
} else -> odd: @bit32 {
    ...
}

// Loop: repeat until the call fails.
loop if lines.next() -> line: Text {
    ...
} else break
```

Captures on a condition that cannot fail, and a guard on a call that
cannot fail, are `E062`. The declaration shorthand may fail too:
`arena: Arena.create(size) else -> why: Os.Error { return }`.

A function that can fail cannot be a value, a function type cannot fail
(`E057`), and neither can an `extern`, an `export` (`E028`) or a hook
(`E022`).

### Control Flow

A condition (`if`, value `if`, `and`/`or` operands) is a `@bit8` tested for
non-zero (`0x0` is false; any other value is true). A condition of any other
type must have exactly one `@conversion` to `@bit8` (so a library `Bool`
works); otherwise it is `E010`. Wider containers are never tested implicitly:
compare explicitly (`@i_less_than_32(i, n)`, `x == 0x0`).

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

`else` takes any statement, so `else if`, `else break` and `else return`
need no braces:

```lilac
if a {
} else if b {
} else return
```

An `if` used as a value is written `if c then x else y` (`then` is a keyword;
the `else` branch is required):

```lilac
status: @bit8 = if 0x0 then 0x0 else 0x1
```

A call that can fail is a condition too: the `if` takes its `then` block
when the call succeeds, and binds its outs with `-> captures` (see
*Functions that can fail* above). Any other call is its first out, tested
as above.

`loop` repeats any single statement until a `break`:

```lilac
loop {
    // This will loop infinitely.
}
```

A conditional loop is just `loop` over an `if`:

```lilac
val: @bit8 = 0x0
loop if @i_less_than_8(val, 0x1) {
    // do work
} else break
```
The intrinsic form needs no operator binding. Once `@less_than` is defined for
the operand types (see §8), the idiomatic form `loop if val < 0x1` works identically.

`continue` skips the rest of the innermost loop's body and starts the next
iteration, so a `loop if` re-tests its condition:

```lilac
i: @bit32 = 0x0
loop if @i_less_than_32(i, 0xa) {
    i = @i_add_32(i, 0x1)
    if @i_less_than_u_32(@i_mod_u_32(i, 0x2), 0x1) {
        continue // skip even numbers
    }
    // odd numbers only
} else break
```

`break` and `continue` outside a `loop` are `E022`.

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

A pattern may name the enum (`Optional.Some`, `Optional[T].Some`); it must
be the enum being matched, and lowers exactly like `.Some`.

You can attach else to the end of a match to catch invalid matches:

```lilac
match enum_val {
    .None => return
} else {
    return
}
```

A capture follows the field rule of §1: a payload that copies is copied,
and one that does not moves out of the matched value, which is then used
up (it does not drop again). That needs an owned value (a local, a
by-value parameter, a field of one, or a temporary such as
`match make() { … }`) whose enum has no `@on_drop` of its own; matching
through a `ref` cannot move a payload out (`E064`). A `match` that names
every variant has no fall-through path, so an out set in every arm is set.

Arm patterns are tags (`.Some` or `Enum.Some`); a payload binds with
`-> name: Type`, and a single statement arm shortens to `=>`. A match on a
bit container (`@bitN`) takes literal patterns instead: integers (`0x1`,
`42`) and characters (`'a'`, its Unicode scalar value), each compared with
the scrutinee's bits. A literal that needs more bits than the scrutinee
holds is `E060`, and a float or string literal is not a pattern (`E010`). Match is a
statement: using a `match` as a value is rejected with `E001`. Because an arm
starts with `.`, a `.` at the start of a line never continues the previous
line's expression as a member access. The name after a `.` (a member's, a
path segment's, a tag's) is on the `.`'s line: a `.` ending a line is an
error, not an access of the next line's first name.

```lilac
match number {
    .Some -> some: @bit32 {
    }
    .None => return
}
```

## 5. Variables, References & Immutability

### Variable Declaration

Variables are declared using `name: Type = value` syntax. Two shorthands
avoid writing the type twice: `name: Type { fields }` means
`name: Type = Type { fields }`, and `name: Type.function(args)` means
`name: Type = Type.function(args)`. Both work for generic types
(`b: Box[@bit32] { value = 0x1 }`, `b: Box[@bit32].create(0x1)`).

In the second, `function` is a static function of `Type` (declared in its
body, without `self`) that returns exactly one `Type`; anything else is
`E056`. The segment before `(` is always the function and the rest is the
type, so `name: A.Pair.make[@bit8](x)` calls `make[@bit8]` on `A.Pair`, and
a namespace there is an error. A type is not callable: there is no
`Type(args)` and no `@init` hook. A constructor is an ordinary static
function with a name that says what it does (`create`, `with_capacity`);
plain functions do not overload, so each constructor gets its own name.

```lilac
counter: @bit32 = 0x0
user: Object {}
custom_user: Object.create(0x1, "Lilac") // `Object = Object.create(0x1, "Lilac")`
```

Redeclaring a name in the same block is `E020`; a nested block may shadow
an outer local (or a global) until it ends.

The base language enforces no concept of mutability. Read-only variables and immutability guards are implemented in user-land using the `@on_override` compiler hook.

### Global variables

A top-level `name: Type = value` declares a **global**: one mutable,
statically allocated value for the whole program, read and assigned like a
local (`count = @i_add_32(count, 0x1)`, `origin.x = 0x9`, `ref count`).
`name: Type` with no initializer is a zeroed global. A top-level
`name: Type => value` is an inline constant instead (§7), with no storage.

The initializer is evaluated at compile time and must be a constant
expression (the forms in §2's constant-condition table, plus object
literals and enum variants of constants); anything else is `E038`. No code
runs before `@entry`, so a global needing a computed value is declared
zeroed and assigned at run time. A global is never dropped: its `@on_drop`
hook, if any, does not run at exit.

```lilac
count: @bit32 = 0x2A
origin: Point = Point { x = 0x3, y = 0x4 }
scratch: @array(@bit8, 64)   // zeroed
```

### Parameters and `self`

A by-value parameter (`x: T`) takes ownership: the caller's value moves
in (or is copied, for a type that copies) and the callee drops it unless
it moves it on. A `ref T` parameter borrows: the caller keeps ownership,
and nothing behind the reference moves out (`E064`). Both are passed by
address; only ownership differs.

The bare `self` receiver is `self: ref T`. A method that consumes its
receiver writes `self: T` (`get: function (self: Optional[T]) value: T
else nothing`), so `found.get()` moves `found` at the call site.
`@on_drop`, `@on_copy` and the other hooks keep the bare `self`.

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

Types are declared only at the top level of a file: an `object` or `enum`
declared inside another type's body is `E036`. Group related types with a
folder namespace instead.

**An object's layout is its C layout.** An `object` is laid out exactly as
the C struct with the same fields in the same order on the target:
`sizeof`, `_Alignof` and every `offsetof` are the target C compiler's. Each
field sits at the next multiple of its alignment and the size is padded to
a multiple of the largest alignment; fields are never reordered. Field
types map to C as `@bit8`…`@bit64` → `uint8_t`…`uint64_t`, `@bit_native`
→ `uintptr_t`, `@fbit16`/`@fbit32`/`@fbit64` → `_Float16`/`float`/`double`,
`@fbit128` → the target's 128-bit float (`__float128`, or a binary128 `long
double`), `ref T` → `T *`, and `@array(T, N)` → `T[N]`. So a `ref Object`
crosses an `extern` as a `struct Object *` with no conversion, and a C
header translates field by field. An object with no fields has size 0 and
alignment 1, as GCC and Clang lay out an empty struct. Alignment follows
the target's C ABI, not the size: on 32-bit x86 off Windows, for one,
`@bit64` and `@fbit64` align to 4, so `{ a: @bit8, b: @bit64 }` is 12
bytes there and 16 on x86-64. No type may be larger than the target's
`isize::MAX` bytes (2^31 − 1 or 2^63 − 1): a larger one is `E067`. Enums
have a C layout too: a tagged union (see Enums).

### Namespaces & Visibility

Namespaces mirror folders; no declaration introduces one. Every `*.lilac` file
directly in a folder belongs to that folder's namespace, and the files in one
folder **merge**: they see each other's declarations with no import (a same-named
declaration across them is `E020`). Subfolders are separate namespaces, named by
their dotted path from the root (`<root>/A/B` is `A.B`). A project is
loaded and checked whole, like a mounted library (below): every folder under
the root whose name is an identifier is a namespace whether or not anything
`using`s it, so `using` only decides visibility. Skipped are `build` (the
build folder), a folder holding its own `lilac.json` (another project) and a
folder named like a mounted library. A root with no `*.lilac` file of its own
(a folder of projects) loads only what `using`s reach.

**`using`.** `using A.B` resolves root-relative to `<root>/A/B` and makes that
namespace's public (non-`private`) names visible, unqualified, **in the file that
contains the directive only**. `using *.A.B` resolves relative to the importing
file's own namespace (`*` is that namespace), so a library can name its own
subfolders without knowing where it is mounted. Sibling files do not inherit each other's
`using`s even though they share each other's declarations. Imports are
non-transitive (`B` `using C` does not expose `C` to a file in `A`) and never
pull in a parent folder's imports. A `using` inside a `private { … }` block is
still honored. Importing an ancestor namespace is allowed but unnecessary (see
reach below).

```lilac
using Standard
```

**`using … as`.** `using A.B as Name` (or `using *.A.B as Name`) loads `A.B`
like any `using` but imports none of its names: it binds the namespace to
`Name` in the file that contains the directive, so `Name.pow()` is
`A.B.pow()` and `x: Name.Type` is `x: A.B.Type`. The alias may shadow a
namespace and a name reached from an ancestor or imported by another
`using`, but not a declaration: an alias named like an item of the file's
own namespace (any of its files), or like another alias in the file, is
`E054`. A local or parameter shadows an alias in its block.

```lilac
using Standard.Io as Io
```

**Ancestor/descendant reach.** The folder tree orders namespaces into ancestor
chains and descendant sets; only namespaces in the compilation unit participate
and the root is always in it. Loading a namespace also loads its ancestors
(`using Standard.C` loads `Standard` too), so ancestor reach never depends on
another import. A namespace automatically sees every item of its
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
found for operators and `@conversion`.

**Collisions.** A same-named declaration in one namespace is `E020`, first
binding wins. A local (or ancestor-reached) name shadows an imported one. If two
`using`d namespaces export the same name, referencing it is `E029`; an
unreferenced collision is not an error. A `using … as` alias named like a
declaration of the file's own namespace, or like another alias, is `E054`.


**Paths.** A dotted path names an item through its namespace, in type
positions (`x: A.B.Type`, `A.Box[@bit8]`, with generic arguments on the
last segment) and in expressions (`A.B.foo()`, `A.B.Type { … }`,
`A.B.Type.method()`, `A.Enum.Variant`, `match` patterns `A.Enum.Tag`). The
first segment is looked up innermost first:

1. a local or parameter (the chain is then a field access);
2. a generic parameter or builtin type;
3. a `using … as` alias of the file;
4. a name the file sees (its namespace, its ancestors, its `using`s;
   `E029` when ambiguous);
5. a namespace: a child of the file's namespace, else of its nearest
   ancestor that has one. The root is every namespace's ancestor, so a
   top-level namespace (`Standard.Io`) can be named anywhere unless a closer
   one of that name shadows it, and a library names its own subfolders
   relative to itself (`Os.write` inside `Standard` is `Standard.Os.write`).

Each later segment steps into the namespace the previous one named: its
item, else its child namespace. A path ends at an item; a `.member` after
a type is the usual static member access. A path reaches a
namespace's `private` items only from that namespace or a descendant, as
`using` does (others see an unresolved name). A path needs no `using`, so it
also tells two colliding imports apart (`A.shared()` where `shared()` is
`E029`), but it names only loaded namespaces: a mounted library always is,
while a folder of the project's own tree still needs a `using` somewhere in
the program. `@builtin.Target.os` is such a path: `@builtin` is a top-level
namespace.

**Qualification.** Namespaces are compile-time scopes only. For code generation
every top-level item takes a symbol prefixed by its namespace path (`A.B.foo`,
`A.Object.Method`, `A.GenBox[m].get`): generic arguments and hook
signatures are written in a compact encoding of their types
(`GenBox[m]` is `GenBox[@bit32]`; `IR_SPEC.md`, "Symbol names"), which
`lilac demangle` reads back. A root-namespace free
function is prefixed `.root` (`.root.foo`), which no user name or C symbol
can spell, so it never takes a symbol an `extern` links against; root `main`
stays `main` (`Standard`'s `@entry` calls it). Root types and their methods take no prefix
(`Object.Method`): they already contain a `.`.
User-defined type names and generic instances are qualified the same way, so
same-named types in different namespaces stay distinct, while builtin types are
never qualified. Two exceptions carry no prefix: hooks are unit-global (one
signature-keyed overload table for the whole compilation unit, whether declared
at top level or inside an object) and `extern` symbols keep their unmangled
declared name (§9). So a root `free` and `Standard.C`'s `extern` `free`
coexist: one is `.root.free`, the other `free`.

A namespace is loaded from a directory argument on the command line: a directory
selects project mode (that folder is the root namespace). A file argument keeps
single-file mode, so the file's parent folder is **not** treated as a project
root and contributes no sibling files.

**Libraries.** `--lib=Name=dir` (repeatable; the editor setting is
`lilac.libraries`) mounts `dir` as the top-level namespace `Name`:
`using Name` loads `dir`, and `using Name.A` loads `dir/A`. A library is an
ordinary folder tree; its own files reach its subfolders with `using *.A` (or
`using Name.A`), so it works wherever it is mounted. Every mounted library is
loaded and checked whole: every folder under `dir` whose name is an
identifier is a namespace whether or not anything `using`s it, so an error
in unused library code is still reported, and `using` only decides which
names a file sees. Dead-code elimination keeps unused library code out of
the output, so mounting a library a program does not use changes nothing it
emits. A missing library folder is `E025`.

### Conditional items

At the top level (and inside a top-level `private { }`), an `if` chain or a
`match` can hold items instead of statements. Only the arm its condition
selects exists: its items are declared in place of the conditional, as if
written there, and the other arms are never checked or compiled.

```lilac
if @builtin.Target.pointer_size == 0x8 {
    Word: object { value: @bit64 }
} else {
    Word: object { value: @bit32 }
}

match @builtin.Target.os {
    .Windows {
        using Standard.Os.Windows as Platform
    }
    .Linux => using Standard.Os.Linux as Platform
} else {
    using Standard.Os.MacOs as Platform
}
```

A `match` arm is `{ items }` or `=> item`; a `match` with no matching arm
and no `else` declares nothing. The condition (or scrutinee) must be a
constant expression (see Constant conditions), else `E039`. It is checked in
its file's scope and may name items other conditionals declare: those are
decided first, and an arm can hold further conditionals. A `using` inside an
arm takes effect only in the selected arm, but every arm's `using`s are
loaded, so each must name a folder that exists.

### Enums

Enums are the single tagged-data type: a closed set of named variants,
each optionally carrying a payload. In memory an enum is a tagged union:
an explicit discriminator tag, then one area every payload shares. The
`=>` operator maps constructor branches to their resulting object state.

```lilac
Enum: enum { One, Two }
```

```lilac
OptionalBit32: enum {
    Some: @bit32
    None
}
```

Values construct as `Enum.Variant` / `Enum.Variant(payload)`, with the
arguments of a generic enum spelled on the enum (`Optional[@bit32].Some(0x1)`),
or as a bare `.Variant` / `.Variant(payload)` when the expected type names the
enum. `match` arms test tags with `.Tag` patterns and bind payloads via
`-> name: Type`. A bare tag's name is on its `.`'s line (a `.` ending a
line is an error).

The tag is the smallest `@bitN` that holds the variant count (`@bit8` up to
256 variants), and the tag of a variant is its index in declaration order.
**An enum's layout is its C layout**, that of the C struct

```c
struct { uintN_t tag; union { P0 p0; P1 p1; /* … */ } payload; }
```

where `uintN_t` is the tag's container and the union has one member per
variant with a payload, in declaration order, each mapped to C as an
object's fields are (see Objects). Variants without a payload add no
member, and an enum with no payloads at all is just its tag. So the tag is
at offset 0, every payload starts at the tag's size rounded up to the
strictest payload alignment, and the size is that offset plus the largest
payload, padded to the enum's alignment (the larger of the tag's and the
payloads'). For example `{ A: @array(@bit8, 0x100), B: @array(@bit8,
0x100), C: @array(@bit8, 0x100) }` is 257 bytes, aligned 1, and
`Result[@bit32, @bit8]` is 8 bytes, aligned 4. There are no niche layouts:
`Optional[ref T]` keeps its tag (16 bytes on a 64-bit target). An enum
crosses an `extern` behind a `ref` as a pointer to that C struct.

Like every value, an enum starts zeroed, which is its **first** variant
(with a zero payload). A variant is always built whole in a zeroed object,
and every write of an enum (an assignment, an out, `@write`, a global's
initializer, a copy) writes the whole value: the bytes of the payload area
past the live payload, and the padding, are zero. `==` compares an enum's
every byte, inactive payload bytes included, so two values of the same
variant with equal payloads are equal whatever the place held before. A
type
that contains itself by value, directly or through other fields and
payloads, is `E053`; store the recursive part behind a `ref`.

An enum body may also declare members exactly like an object: methods
(`name: function (self) ... { }` or `=> expr`), hooks (`@name: ...`) and
`private { }` blocks. A `name: function (...) ...` with a body is a method; a
`name: Type` without one is a payload variant. (The old `union` keyword was removed:
it spelled the same type. Legacy `union { ... }` declarations fail to
parse; rename them to `enum`.)

### Type aliases

`Name: alias Type` gives a type a second name:

```lilac
Byte: alias @bit8
Bytes: alias List[@bit8]
Callback: alias function (Event) nothing
```

* **An alias is its type**, not a wrapper: `Bytes` and `List[@bit8]` are
  one type, so values pass between them with no conversion, and they
  have the same hooks, layout and symbols. A hook declared over one
  spelling is the hook of the other; two that differ only in spelling
  are a duplicate (`E020`).
* An alias may be written wherever a type can: a parameter, out, local,
  field, payload, generic argument, `as`, `ref Bytes`. An alias of an
  object or enum also names its statics and builds its literals
  (`Spot.origin()`, `Spot { x = 0x1 }`); an alias of a generic instance
  has its arguments already, so `Bytes { ... }` infers nothing.
* It is an item like an object: declared at the top level of a file only
  (inside a type body it is `E036`), namespaced and visible like any item
  (`private { }`, `using`), and its name clashes like one (`E020`). It is
  not a value (`E021`).
* It is **not generic**: `Name[T]: alias …` is a syntax error (`E001`),
  but it may name an instance. Generic arguments on an alias where it is
  used, and a generic type named without its arguments, are `E044`.
* It names a type, never a reference: `Name: alias ref T` is `E001`;
  write `ref Name` where it is used. `unknown` lives only behind a `ref`,
  so `alias unknown` is `E023`.
* An alias that needs itself to be known, directly or through other
  aliases (`Pair: alias Both[Pair]`), is `E074`, reported once at the use
  that closes the loop.
* **Diagnostics** show a type written with arguments (an instance, an
  array or a function type) by its alias when exactly one alias names it:
  `no conversion from Bytes to @bit32`. A plain name (`@bit8`, `Point`)
  is always shown as itself, so `Byte` never renames `@bit8` elsewhere.

## 7. Generics & Inline Declarations

### Generics

Generics provide compile-time templating for any declaration. The generic
parameter `T` is a placeholder whose requirements are proven only when the
declaration is instantiated, which is what allows duck-typing inside a
generic body.

A generic function nothing instantiates is still checked, with `T` left
abstract: an error about a type that contains a parameter (`T`,
`GenObj[T]`, a member looked up on `T`, a value converted to or from `T`) is
dropped, since some argument might satisfy it; every other error (an
unresolved name, a mismatch between concrete types, an unknown member of
`GenObj[T]`, a literal index past an array's length, `E037`) is reported as
usual. An `@array_at` under a condition that reads no local is not checked
there, since such a condition may be constant once `T` is known. A generic
that is instantiated is checked only through its instances.

Type arguments are **explicit**: v1 has no type-argument inference, so every
generic reference spells its arguments (`gen_function[@bit32]`,
`GenObject[@bit32]`). Applying a generic with the wrong number of arguments —
too few, too many, or any argument on a non-generic declaration — is `E044`;
the bogus application is never built. In expression position `Name[args]`
may be followed by `(` (a generic call), `.` (a static member,
`GenObject[@bit32].make()`), or `{` (a struct literal,
`GenObject[@bit32] { value = 0x0 }`). A struct literal may omit the
arguments when the expected type supplies them:
`x: GenObject[@bit32] = GenObject { value = 0x0 }`.

```lilac
GenObj[T]: object {
    value: T
}

GenFunc[T]: function (value: T) nothing {
    // Validated at instantiation time
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

A generic definition may appear at most 64 times in a chain of instances
that request one another (`E051`), and a compile builds at most 50,000
generic instances (`E068`). A type's size has no limit: `P[T, T]` applied
to itself 20 times is a tree of a million leaves, and it compiles in time,
and to symbols, linear in the number of distinct types it names.

A hook declared inside a generic body (for example an object's `@on_drop`)
monomorphizes with its parent and registers once per concrete instantiation,
so `GenBox[T]` can define `@on_drop` for each `GenBox[@bit32]`-style instance.
Its signature registers with the instance, so lookups find it, but its
body is checked for an instance only when something uses it with those
arguments (an operator, a conversion, a direct `@hook(...)` call): a
`Pair[T]` whose `@add` adds two `T`s is fine as `Pair[@bit8]` until two
`Pair[@bit8]`s are added, and then the failure is `E042` at that `+`.
`@on_drop`, `@on_copy` and `@on_override` are checked with the instance,
since every drop or copy of the type uses them. A hook no instance uses is
checked like an unused generic, with `T` left abstract.
A top-level hook may not be generic: a blanket `@add[T](T, T)` stays `E052`.

### Inline Declarations

Inlined functions may omit the return name but are restricted to a single output type. The `=>` operator directly binds the expression output.

A `=> expr` body is exactly `{ out = expr }` on an ordinary function marked
always-inline: arguments are evaluated once, recursion is allowed (the
recursive call is simply not inlined), and a call with constant arguments is
a constant expression (see Constant conditions and `@compile_error`).

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

**Equality (`==`) is strictly bit-equality and cannot be overridden.** Because universal zero-initialization guarantees that unused memory and struct padding are always zeroed (and an enum's payload area past its live payload, since a variant is built whole in a zeroed object), bitwise comparison is 100% deterministic. Custom comparison logic must be implemented as regular named methods to prevent hidden runtime complexity.

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

Operator, `@conversion`, and RAII (`@on_copy`/`@on_drop`) hooks are globally
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
   parameter type is `E010`. Numeric literals and tags (`.None`, `.Some(x)`)
   still take the parameter/operand type (literal typing, not conversion), so
   `os == .Windows` types the tag from `os`. Typed contexts
   (let/assign/return) and ordinary function-call arguments
   still convert as in step 2.

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

`as` and the implicit path share one conversion mechanism; neither chains, both are exact-match, and a change to conversion rules applies
to both.

### The `@format` hook

`@format(value: ref T, out: ref W) nothing` says how a `T` is written into a
writer `W` (§2, "Formatted strings"). Both parameters are `ref`s and it has
no outs; any other shape is `E022`. It is indexed by its exact signature
like every hook, so one type may have a hook per writer, and a writer is
any type: the compiler never looks inside `W`. A top-level hook may name a
type the library does not own (`@format(value: ref @bit32, out: ref
String)`). A generic type's hook is checked for an instance when an
f-string first uses it, so `@format` of a `Box(T)` that fails for some `T`
is `E042` only where it is used with that `T`.

### Resource Management (RAII)

Memory lifecycle events trigger specific compiler hooks if defined for a type.

| Hook | Signature | Invocation Trigger & Behavior Rules | Desugar |
| --- | --- | --- | --- |
| **`@on_override`** | `(ref prev, ref new) nothing` | Called **before** a full place's value is overwritten (not when an empty place, such as an out, is first set). **Rule:** The value in `prev` is dropped immediately afterwards, so you must **not** override or free anything in `prev` during `@on_override`. | `variable = new` -> `@on_override(ref variable, ref new) drop(variable) variable = new` |
| **`@on_copy`** | `(ref Type) new_copy` | Called when a place is read into a new owner (see §1); `@move` skips it. A type with an `@on_drop` and no `@on_copy` moves instead of copying. You are manually responsible for creating and returning the new copied value (`new_copy`). **Rule:** Extreme care must be taken not to recursively trigger the copy mechanism while constructing `new_copy`. | `copied: Type = variable` -> `copied: Type = @on_copy(ref variable)` |
| **`@on_drop`** | `(ref Type) nothing` | Called when a full owner of a value goes away (§1), before the value's fields are dropped; a moved-out place is empty and drops nothing. Its own `@on_drop` makes a type move rather than copy, and keeps fields from moving out of it. Used for resource deallocation and cleanup of what this type allocated. | `variable: Type = value` -> `variable: Type = value defer drop(variable)` |

There is no construction hook: a constructor is a static function, and
`name: Type.function(args)` declares a variable with one (§5).

Example of defining a constructor and RAII rules

```lilac
Buffer: object {
    ptr: ref @bit8

    // A constructor: `buffer: Buffer.create(size)`
    create: function (size: @bit32) buffer: Buffer {
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

### Platform hooks

Three top-level hooks connect a program to its platform. The compiler calls
them; a runtime library (normally `Standard`, through `Standard.Runtime`)
defines them:

| Hook | Signature | Called |
| --- | --- | --- |
| **`@entry`** | `function (count: @bit_native, arguments: ref unknown, environment: ref unknown) nothing` | By the process entry stub, with the process's argument count and argument and environment vectors (C's `argc`, `argv` and `envp`; zero and null on native Windows, whose command line comes from `GetCommandLineW`). It runs the program. |
| **`@exit`** | `function (code: @bit32) nothing` | After `@entry` returns (code 0), and by the panic handler (code 101). It must end the process. |
| **`@compiler_print`** | `function (text: @byte_buffer) nothing` | By the panic handler, for the message and each stack-trace line. |

- **The entry stub** (`.lilac.start`, called from the C `main` under
  `--libc` and from the native entry otherwise) makes no OS call: it calls
  `@entry` with the arguments, then `@exit(0x0)`,
  then traps. The panic handler writes through `@compiler_print`, calls
  `@exit(0x65)`, then traps. So an `@exit` that returns stops at a trap
  rather than running on.
- **`@entry` calls `main` itself.** Inside an `@entry` body, a name not
  found in the hook's own scope is looked up in the program's root
  namespace, so `Standard`'s `@entry` calls the program's `main`. Another
  runtime library may enter the program under any name.
- **Each hook is defined at most once** in the program (`E034`), at the top
  level, with exactly its signature (`E022`). Hooks are unit-global and carry
  no namespace prefix; they are emitted as `.lilac.entry`, `.lilac.exit` and
  `.lilac.compiler_print`, and user code may call them (`@exit(0x1)`).
- **An executable** (`lilac build`) needs all three: a missing one is
  `E033`, and an `@entry` that names a missing `main` is `E032`. The stub
  roots reachability.
- **Anything else** (`check`, `emit-*`, and static libraries later) needs
  none. `@entry` is not a root, and an `@entry` that names a missing `main`
  is left out silently. A missing `@exit` or `@compiler_print` is declared as
  an external symbol under its fixed name, so the panics of code linked into
  another program resolve against that program's hooks.

## 9. Foreign functions and async

### extern (imports)

`extern` qualifies a declaration: a signature with no body, resolved to a
foreign symbol at link time.

```lilac
malloc: extern function (size: @bit_native) ptr: ref @bit8
```

Rules: an `extern` uses the platform C ABI. Each parameter maps to its C type:
a bit integer (`@bit8`/`@bit16`/`@bit32`/`@bit64`/`@bit_native`) or float
(`@fbitN`) by value, any `ref T` to an opaque pointer, and an object with a
single field of one of those types (a transparent wrapper such as a library
`CInt` or `FILE`) as that field. A single return slot becomes the C return
value; no return slot is `void`. More than one out, or a parameter/return
outside that set (an aggregate such as `@byte_buffer`, say), is `E028`,
validated before codegen so it reports as an ordinary diagnostic. An `extern`
may not be named `main`: extern symbols are unmangled, so the C symbol `main`
would collide with the entry point the LLVM backend emits, and that is also
`E028`. Calls typecheck only against the declared signature (a wrong
declaration is user error and is diagnosed nowhere — declare carefully);
libraries to link (`link = ["c"]`) are declared in the package manifest,
never in code. No overloads, no calling-convention annotations in v1.

**Aggregates cross as their C layout.** An object or enum is not passed by
value, but behind a `ref` C sees exactly the C layout of the Lilac type: an
`object` is the C struct of its fields in order, and an `enum` is
`struct { uintN_t tag; union { … } payload; }` (see Objects and Enums), on
every target, with the same `sizeof`, `_Alignof` and `offsetof`s. So a
`ref Object` or `ref Enum` is a `struct … *` to C with no conversion, and a
C header translates field by field. (An enum written by C must keep the
zero bytes past its live payload for `==` to hold.)

**Link names.** The symbol an `extern` links is its declared name, unmangled
(no namespace prefix), unless a string after `extern` names it:

```lilac
os_write: extern "write" function (fd: @bit32, data: ref @bit8, len: @bit_native) written: @bit_native
```

The Lilac name (`os_write`) is an ordinary, namespaced item; only the linker
sees `write`. Externs that link the same symbol, in any namespaces, are one
declaration when their C signatures match (parameter and return types, and
`...`), and `E028` when they differ. C sees only a pointer, so `ref T`s
match whatever their pointee (`ref @bit8` and `ref Text` declare the same
`puts`). The link name `main` is `E028`, like an `extern` named `main`.

**Variadic externs.** A trailing `...` lets an `extern` take C varargs:

```lilac
format_into: extern "snprintf" function (out: ref @bit8, size: @bit_native, format: ref @bit8, ...) written: @bit32
```

`...` is allowed only as the last parameter of an `extern`; Lilac functions
are never variadic. Arguments past the fixed parameters have no parameter type
to check against, so each must carry its own:

- an untyped literal is `E028` (write `0x1b6 as @bit32`);
- only `@bit32`, `@bit64`, `@bit_native`, `@fbit64` and `ref T` may be
  passed. C promotes narrower integers and `float`, and Lilac integers carry
  no sign to promote with, so `@bit8`, `@bit16` and `@fbit32` are `E028`
  (widen them with `as`).

A variadic `extern` cannot be used as a function value (`E028`): function
types have no `...`. A non-variadic one named as a value is a C function
(see `export`). The backend declares it with a C variadic prototype,
so each target's varargs convention applies (on Apple arm64, varargs go on the
stack, which a fixed prototype gets wrong).

### export (C-callable definitions)

`export` is the other direction: a function with a body that C calls, by
an unmangled symbol, with the platform C ABI. Like `extern`, a string after
it names the symbol; otherwise it is the declared name.

```lilac
compare: export function (a: ref @bit32, b: ref @bit32) order: @bit32 {
    order = 0x0
}

lilac_scale: export "scale" function (x: @bit32) y: @bit32 => @i_mul_32(x, 0x2)
```

Rules (`E055` unless noted):

- An `export` is a top-level function: not a method, a hook or generic.
- Its symbol is not `main`, and no other `export` or `extern` names it (an
  `export` defines the symbol; a second definition or a declaration of it
  would clash).
- Its parameters and result follow the `extern` rules: C types, at most one
  result (`E028`).
- Every `export` is kept: it is a reachability root beside `main`, in the
  project and in mounted libraries alike, so an unused one still emits.

Lilac calls an `export` by name like any function (the Lilac ABI, no C
conversion). Named as a value, it is a C function: its type is
`export function (params) outs`, and its value is the C symbol's address.

**C function values.** `export function (params) outs` is the type of a C
function's address. It is a C type, so it passes to and from `extern`s
(`qsort`'s comparator, a thread start routine), and calling a value of it
uses the C ABI. Its slots follow the `extern` rules (`E028`). An `export`
or a non-variadic `extern` named as a value has this type. It converts to
and from `ref unknown` with `as`; a Lilac `function` value never converts
to it, since the two ABIs differ.

```lilac
qsort: extern function (base: ref unknown, count: @bit_native, size: @bit_native, compare: export function (a: ref @bit32, b: ref @bit32) order: @bit32) nothing

sort: function (values: ref unknown, count: @bit_native) nothing {
    qsort(values, count, 0x4, compare)
}
```

### Async lives in the standard library

Async needs no compiler support beyond what exists: futures are objects driven
by `loop` + `match`, polling fits multi-return (`ready, value = task.poll()`),
executors accept any type with a structural `poll` via duck-typed generics, and
`@on_drop` covers task cancellation. Suspending a live call stack — the one
thing Lilac cannot express in itself — is sourced through `extern` (platform
context-switch helpers or a tiny C shim via the build script), never through a
compiler intrinsic. `await` syntax is explicitly deferred: if ever added, it
desugars to existing constructs.

## Lilac to IR (informative; `IR_SPEC.md` is normative)

Lowering follows the IR v2 rules in `IR_SPEC.md`: typed SSA registers (`&n: #type`),
explicit `$label:` blocks each ending in one terminator, `alloca : zeroed`,
verified `call` arity, and named `field`. Examples
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
search).
F-string (`msg: @format_args(Out) = $"a{part}b"`): a static template naming
each hole's `@format` hook, and a `format` instruction that fills the
`#format_args` from the holes' addresses:
```lilac-ir
!fmt0: format "a" {@@format(ReR3OutE)} "b"
...
&3: ref #byte_buffer = addr %part
format !fmt0 (&3) -> %tmp.2
copy %tmp.2 -> %msg
```
Function call with multi-return (`val1, val2 = multi_func()`):
```lilac-ir
$entry:
  call @multi_func (out %val1, out %val2)
  return
```
Assignment and member access use `copy` / `field .name` as defined in `IR_SPEC.md`.
`branch.if` on a constant-expression condition and `compile.error` in the
dead arm of one never reach IR (see Constant conditions and `@compile_error`
above).