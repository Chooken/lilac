# Function Values Plan

> Status: phases 1, 2 and 3 implemented, 2026-10-14. The decisions marked "agreed" were settled
> in discussion; the rest are proposed.
>
> Goal: a function value is a `ref function`. It can live anywhere a value
> lives (a field, a global, a constant, an array, an enum payload, a
> generic argument), and any expression of that type can be called,
> including `object.field(args)`, which passes no `self`. Static tables of
> functions (vtables) can be globals. Inside a type's own bodies, its
> constants and static functions are in scope by their bare names.
>
> Decisions:
> - **A function value is `ref function (…)`** (agreed). A bare
>   `function (…)` type, like `unknown`, exists only behind `ref`, or as
>   the target of an `alias`. That makes `ref` mean the same thing
>   everywhere: the value is an address, it copies bitwise, and it never
>   auto-dereferences. It also removes the ambiguity at item level, where
>   `name: function` declares a function and `name: ref function` declares
>   a field, global or constant.
> - **`ref add_one` takes a function's address** (agreed). A bare function
>   name used as a value is an error (`E077`) whose fix inserts `ref `.
> - **`ref function` ↔ `ref unknown` casts are allowed both ways**
>   (agreed). Any function reference converts to `ref unknown` and back
>   with `as`, Lilac and C ABI alike. Converting through `ref unknown` to
>   a different signature is the caller's responsibility, as it is for
>   every `ref unknown` cast. A direct cast between two different function
>   types stays `E010`, so a mismatch always takes two visible casts.
> - **A type's constants and static functions are in scope inside its own
>   bodies** (agreed, option "b"). This covers methods, hooks and member
>   constant initializers. They resolve after locals and before the
>   file's items. Fields and methods still go through `self`.
> - **Generic instances are values with explicit arguments** (agreed if
>   robust): `ref id[@bit32]`, `ref Box[@bit32].make`. Nothing is
>   inferred, as at a call.
> - **Static vtables are globals** whose initializer holds function
>   addresses, and addresses of other globals. The LIR global gains a
>   list of address slots beside its byte runs.
> - **No runtime null check on calls.** A zeroed `ref function` is null,
>   like any zeroed `ref`, and calling it faults. Struct literals already
>   require every field, so a null one only comes from explicit zeroing.
>   `Optional[ref function (…)]` is the nullable form.

## Today

- A function value's type is `function (…)`, a pointer in its layout.
  At item level, `name: function` always parses as a declaration, so a
  field, global or constant of function type can only be written through
  an alias (`Formatter.sink: FormatSink`).
- Only a local can be called (`HirCallee::Local`). `object.sink(…)` is
  `E022` ("`sink` is not callable"), so `Formatter.write_bytes` copies the
  field into a local first. `Standard.Os.thread_start` reads a function
  out of memory through `ref function` and a copy.
- **Miscompile:** `r: ref Step = ref f; r(0x1)` is accepted and
  segfaults. `lir_ty` maps both `function` and `ref function` to one
  opaque pointer, so the call jumps to the address of `f`'s slot. The
  generated code is `&4 = copy %r; call &4`. Phase 1 makes this
  unrepresentable, because `ref` of a function reference is `ref` of a
  `ref` (`E010`).
- A function address cannot appear in a global initializer: `FuncRef` is
  not a constant (`lilac-lower/src/constant.rs`), so it is `E038`.
- Inside `ArenaAllocator`, its own constant must be written
  `ArenaAllocator.LIMIT`, and its own static function must be written
  `ArenaAllocator.allocate_block(…)`.
- `ref id[@bit32]` already parses (as `Expr::TypeApp`), but typeck
  rejects it ("cannot take `ref` of this expression").
  `ref Box[@bit32].make` is `E021`. `ref Box[@bit32].m[@bit8]` does not
  parse.

## Phase 1: `ref function` is the function value

The core change. It fixes the miscompile, and every later phase builds
on it.

### Language rules (SPEC)

- **Types.**
  - `ref function (params) outs` and `ref export function (params) outs`
    are the function value types.
  - A `function (…)` or `export function (…)` type anywhere else is
    `E077`. That includes a local, parameter, out, field, global,
    constant, generic argument, array element, enum payload, `as` target
    and `@size_of`.
  - The one exception is an alias target: `FormatSink: alias function
    (…) nothing` stays bare, and every use writes `ref FormatSink`, as
    `unknown` is written `ref unknown`. An alias to a function type used
    without `ref` is `E077` at the use.
  - `ref` cannot be added a second time, as for any `ref`.
- **Values.**
  - `ref f` is `f`'s address, typed `ref function (…)`. A C function
    (`extern`/`export`) gives `ref export function (…)`.
  - `f` can be written as a qualified path (`ref A.B.f`) or a type's
    member (`ref Type.make`).
  - `ref Type.method` takes a method's address. Its receiver becomes an
    explicit first parameter: `ref Box` for bare `self`, `Box` for
    `self: Box`. There are no bound methods: `ref value.method` is
    `E010`, with a help line naming `ref Type.method`.
  - Hooks and intrinsics are not values (`E021`, as today). A function
    that can fail is not a value (`E057`). A variadic extern is not a
    value (`E028`). A generic function needs arguments (`E044` until
    Phase 4).
  - A local shadows an item: `ref f`, where `f` is a local, is the
    local's address. When that local is a function reference, the result
    is `ref` of a `ref` (`E010`), with the help line "`f` is already a
    function reference: use `f`".
- **Calls.** Any expression of function reference type can be called:
  - a local or parameter: `f(x)`
  - a field: `obj.f(x)`, where `obj` is a value or a `ref`, auto-deref as
    usual
  - a global or constant: `HANDLER(x)`, `A.B.HANDLER(x)`
  - a call result: `make()(x)`
  - any other expression: `(cond_pick())(x)`

  A call through a value never passes `self`. Members of one type share
  one namespace, so `obj.f(…)` is a method call when `f` is a method and
  a value call when `f` is a field. In a template, `value.f(x)` is
  decided per instance (duck typing).

  **Evaluation order:** the callee first, then the arguments left to
  right. A field callee is read before an argument can move or reassign
  its object.
- **Never dereferenced.** A function reference points to code.
  - Auto-deref skips it, as it skips `ref unknown`.
  - `==`/`!=` compare addresses, and both sides need the same function
    type (`E010`).
  - Assigning a function reference rebinds the target.
  - `@read`, `@write` and `@drop` of one are `E010`.
  - It has no fields (`E021`).
  - It copies bitwise and needs no drop.
- **Identity.** Two references are equal exactly when they name the same
  function, or the same generic instance. The backend never marks a
  defined function `unnamed_addr`, and the linker runs no identical-code
  folding. A codegen test pins the first.
- **Casts.**
  - `ref function (…) as ref unknown` and `ref unknown as ref function
    (…)` are allowed, and so are their `ref export function` forms.
    `is_c_function`'s special case merges into the general `ref` ↔ `ref
    unknown` rule.
  - Any other cast between two function types is `E010`, including
    Lilac ↔ C.
  - **The ABI note SPEC gains:** a cast through `ref unknown` is sound
    when every slot has the same representation. A `ref X` slot and a
    `ref unknown` slot always do, so `ref function (self: ref Arena, …)`
    → `ref function (context: ref unknown, …)` is sound. Changing a
    slot's width or ABI (Lilac ↔ C) is not.

### Parser (`lilac-parse`)

- `types.rs::parse_type_starting`: accept `ref export function (…)`.
  Today `ref` is followed only by `function` or a name.
- No other syntax changes:
  - `name: ref function (…) outs` already parses as a field, a global
    (with `=` or nothing) and a constant (`=>`). See the probe
    `t1_ref_field.lilac`.
  - `ref A.B.f`, `ref Type.f` and `ref id[@bit32]` already parse.
- Parse tests: `ref export function` in a field, a parameter and a
  global; `ref ref function` is still the double-reference error.

### Typeck (`lilac-typeck`)

- **Types** (`types.rs`):
  - `resolve_type`'s `Function` arm reports `E077` when `!ty.is_ref`,
    then recovers as the `ref` type, so later uses check normally (one
    error, no cascade).
  - `resolve_named`'s alias arm does the same for an alias whose target
    is a function type and which is used without `ref`.
  - `aliases.rs::alias_type` resolves the target through a path that
    allows a bare function type.
  - The fix replaces the `function` keyword's span (or the alias name's
    span) with `ref function` (or `ref Name`).
- **One predicate** for "a reference that is never dereferenced":
  `Types::is_opaque_ref(TypeRef)`, true for `ref unknown` and for `ref`
  of any `Func`. These sites use it instead of their own checks:
  - `auto_deref`
  - the write-through test in `assign_checked`
  - `check_equality`
  - the memory builtins (`memory.rs`)
  - `field_access`
  - `check_cast`
- **Function values** (`expr.rs`):
  - `func_value` returns `TypeRef::reference(func)`. It is reached only
    from `ref`:
    - `check_ref_of` first tries the operand as a name, after locals: an
      `Ident`, a path (`qualified_name`), or a static member (`Type.f`
      through `static_receiver`). When the name resolves to a function,
      the result is a `FuncRef`.
    - Anything else is a place, as today.
  - `check_ident` on a function, and `static_member` on a function
    member, report `E077` with the fix, then recover as the `FuncRef`.
  - `record_use` stays on the name's tail span, so references and rename
    see it.
  - Method values: the receiver slot comes from the same code that types
    bare `self` in signatures (`sigs`).
- **Calls** (`calls.rs`):
  - `HirCallee::Local(LocalId)` becomes `HirCallee::Value(Box<HirExpr>)`.
    One helper, `value_call(callee: HirExpr, args, …)`, checks the
    arguments against the function type and builds the call. Every path
    below goes through it.
  - `check_named_call`: a local whose type is a function reference
    (instead of a bare function type), and a `Global` or `Const` of
    function reference type, call through `value_call`. Other non-function
    items stay `E022`.
  - `method_call`: a `DefKind::Field` member whose type is a function
    reference calls through `value_call`. `private_denied` and
    `record_use` run first, as for a field read.
    - A field of any other type stays `E022`. The message names the type
      ("`count` is a field of type `@bit32`, not a function").
    - In a template, the `Requirement::Missing { method: true }` path is
      unchanged.
  - `check_call`'s fallback arm (`_ =>`) checks the expression. A
    function reference calls through `value_call`, anything else is
    `E022` as today.
  - Callees can never fail (`E057` keeps failing functions out of
    values), so `FnCx::handled` is untouched.
- **Passes over the callee:** `places.rs` (ownership), `failure.rs`,
  `indices.rs` (`E037`) and the `FuncRef` arms (`indices.rs:145`,
  `places.rs:917`) walk `HirCallee::Value`'s expression before the
  arguments. The order must match lowering's.
- **Display** (`lilac-ty`): `Types::display` prints a function type as
  Lilac spells it, minus the parameter names it does not keep:
  `ref function (@bit32) @bit32`, not `function (@bit32) -> @bit32`.
  `tests/ui` expectations change with it.

### HIR, lowering, LIR, backend

- `lilac-hir`: `HirCallee::Value(Box<HirExpr>)`.
  `HirExprKind::FuncRef(FuncKey)` keeps its shape; only its type changes.
- `lilac-lower`:
  - `lir_ty` maps `ref Func` to `LirTy::opaque_ref()`. A bare `Func` is
    `bug!`, the backstop: any dereference typeck missed becomes an ICE,
    never a jump through data.
  - `calls.rs::emit_call` lowers `Value(expr)` like this:
    1. Lower the callee expression first, into a temporary place, since
       a register lives in its block and argument lowering may branch (a
       division check, an `if` argument).
    2. Lower the arguments.
    3. Load the temporary into a register in the call's block.
    4. Call `Reg`, or `CReg` when the type is `c_abi`, through
       `emit_c_call` as today.
  - `callee_reg` becomes that load.
  - `control.rs` (`HirCallee::Local`) follows.
- LIR, the verifier and the backend are unchanged: `call &r` and
  `call c &r` already exist, and `addr.func` of an export is its C
  entry.
- DCE needs nothing new (`ref f` is `addr.func`, already an edge).
  Stack traces need nothing new: every non-builtin call is instrumented,
  indirect ones included.

### Language server (`lilac-lsp`)

- **Signature help** for a value callee renders the parameter list from
  the function type's syntax in the declaration that gave the callee its
  type, with names: the field, global, constant, parameter or local
  declaration, or the alias's declaration through an alias. The callee
  token's site finds that declaration (`NameSite` for items,
  `LocalSite` for locals).
- **Hover** on a called field shows the field declaration, as on a read.
  A `FuncRef` shows the function's declaration.
- **Completion** after `.` lists function-reference fields as fields
  (truthful kind), with the type as `detail`.
- **Navigation:** references, rename and highlights need nothing new
  beyond `record_use` on every new path.
- **Code actions:** `E077`'s fix rides in `data` like every fix, so the
  quick fix migrates code in the editor.

### Diagnostics

- **New `E077`: a function is a value only behind `ref`.** The page
  (`docs/errors/E077.md`) shows both forms (a bare type, a bare name) and
  the fix. It is registered in `lilac_error::codes`.
- **`E010`** gets the help lines for:
  - `ref` of a function reference
  - `ref value.method`
  - `@read`/`@write`/`@drop` of a function reference

  Its page gains one sentence on function references.
- **`E022`'s** page says what is callable now.
- **`E021`** (hooks, intrinsics), **`E057`**, **`E028`** and **`E044`**
  keep their meaning. Their messages say "function value" where they
  said "value".

### Migration

`E077` is exhaustive, so the compiler finds every site; the editor fix
applies most of them. Two changes are **silent** and are audited by
hand:

- **`x as ref function (…)` from `ref unknown`** used to mean "a pointer
  to a slot holding a function". It now means "the function". The only
  uses are `lib/Standard/Os/thread.lilac:122` and `:135`; both are
  rewritten.
- **Display text** in `tests/ui` expectations.

Known sites:

- `lib/Standard/format.lilac`:
  - `sink: ref FormatSink`
  - `write_bytes` becomes `self.sink(self.target, bytes)`
  - `over(target, sink: ref FormatSink)`
  - callers pass `ref count_characters` and the like
- `lib/Standard/Os/thread.lilac`:
  - `StartBlock { entry: ref function (argument: ref unknown) nothing,
    argument: ref unknown }`
  - `thread_start` becomes `block: ref StartBlock = start as ref
    StartBlock` followed by `block.entry(block.argument)`
  - `spawn`, `start_block`, `clone_thread` and `posix_spawn_thread` take
    `entry: ref function (…)`
  - callers write `Os.spawn(ref worker, …)`
- Externs that take C callbacks: `ref export function (…)` slots.
- Tests:
  - `tests/function_types.lilac`, `tests/ui/function_types.lilac`
  - `tests/run/export_qsort.lilac`, `export_value.lilac`,
    `FAILS_export_function_type.lilac`, `FAILS_variadic_value.lilac`,
    `type_aliases.lilac` (`Callback`)
  - `tests/ui/FAILS_failed_type_cascade.lilac`
  - the parse tests at `lilac-parse/src/tests.rs:750` and `:1689`
  - the `tests/run/stdlib` thread test
  - typeck's `calling_a_field_is_e022`, which keeps a non-function field
- `editors/vscode` samples and the snippets in `docs/`, if any.

### Tests

- **Run tests** (`tests/run/`):
  - `function_ref_field.lilac`: a field call, through a value and
    through a `ref`; a field callee is read before an argument reassigns
    its field.
  - `function_ref_items.lilac`: a call through a global, a constant, a
    qualified global, a call result and a parenthesized expression.
  - `function_ref_table.lilac`: an object of function references built
    at run time, used as an interface (context plus table), with
    `ref Type.method` laundered through `ref unknown`.
  - `function_ref_equality.lilac`: the same function is equal; different
    functions are not; equality holds across a `ref unknown` round
    trip.
  - `function_ref_c.lilac`: a `ref export function` field called with
    the C ABI (qsort's comparator stored in an object).
- **`FAILS_` run tests:**
  - `FAILS_function_ref_ref.lilac`: the old miscompile is now `E010`.
  - `FAILS_bare_function_type.lilac`, `FAILS_bare_function_value.lilac`
    (`E077`, with the fix in the JSON output).
  - `FAILS_function_ref_cast.lilac`: a direct cast between signatures is
    `E010`.
  - `FAILS_call_field.lilac`: a non-function field is `E022`, with the
    type in the message.
- **Typeck unit tests:**
  - every `is_opaque_ref` site refuses a dereference
  - the `E077` recovery reports one error
  - a template's `value.f(x)` is satisfied by a method in one instance
    and a field in another
- **Lowering:** a callee whose arguments branch (division by a
  non-constant divisor) passes the verifier.
- **Codegen:** defined functions carry no `unnamed_addr`.
- **LSP:**
  - signature help on `obj.f(`, `GLOBAL(` and a local
  - hover on a called field
  - rename of a function through a `ref f` use
  - the `E077` quick fix

### Docs

- **SPEC:**
  - §1 types: function references next to `unknown`
  - §4 functions: values, calls through values, evaluation order
  - §5 references: never dereferenced, identity, casts and the ABI note
  - §6 objects: callable fields
  - §9 C function values
- **AGENTS.md:** the "C function values" bullet is rewritten as
  "Function values" (`ref function`, `HirCallee::Value`,
  `is_opaque_ref`, the `lir_ty` backstop).

## Phase 2: a type's own names in its bodies

### Rule

Inside a type's bodies, its member constants and static functions (no
receiver) are in scope by their bare names. The bodies are every method,
hook and member-constant initializer: wherever `Env::owner` is the type.
A bare name resolves in this order, first match wins:

1. locals and parameters
2. the body's generic parameters (a type name, `E021` as a value)
3. **the owner's constants and static functions**
4. `using … as` aliases
5. the file's visible items, then namespaces within reach

Notes on the rule:

- Inner scopes win, as a local already shadows an item.
- An outer item of the same name is still reachable by its namespace
  path. A root item shadowed this way cannot be named inside the type,
  as with a local today; rename one of them.
- Fields and methods are not bare names. A miss that names one says so:
  "`value` is a field: write `self.value`", "`push` is a method: write
  `self.push(…)`". In a body with no `self`, the help is "read it from a
  value of `Box`".
- Variants are not bare names either; `.Variant` tags already cover them.
- **In a generic type**, a bare static function means the current
  instance: `grow(n)` inside `List[T]` is `List[T].grow(n)`, with `T`
  from `Env::args` (the owner's leading arguments, the split
  `static_call` uses).
- In a member-constant initializer of a generic type, which has no
  arguments (one value for every instance), a bare generic static is the
  existing `owner_param_note` error.

### Implementation

- `Checker::own_member(name) -> Option<DefId>`: `Env::owner`'s member
  `name` when it is a `Const`, or a `Func` with no receiver
  (`lilac_ast::Param::is_receiver`). Its callers:
  - `check_ident`, after the local and generic-parameter checks:
    `const_value` for a constant. A function goes to `E077` before
    Phase 1 lands, else to Phase 1's value rule.
  - `check_named_call`: a static function becomes `static_call` with
    `Receiver::Type(own type)`; a constant of function reference type
    becomes Phase 1's `value_call`.
  - `check_ref_of` (Phase 1): `ref grow` is the own static's address.
  - `paths.rs::qualified_name` / `path_prefix` / `lookup_head`: an own
    member at a path's head is a value, and the path stops there.
    Without this, `LIMIT.x` would read a top-level `LIMIT` namespace or
    item instead.
  - `suggest_value` gains the own names, so did-you-mean finds `LIMTI`.
- **The own type:** a helper builds `Env::owner` at the body's arguments,
  the same one that types bare `self`. In a template, the arguments are
  the abstract parameters.
- `record_use` on the bare name's span, so references, rename, hover
  and highlights see own-name uses like qualified ones.
- **Language server:**
  - Bare-name completion (`complete.rs::names`) adds the own constants
    and statics when the cursor is inside a type. That type is the file's
    `Object`/`Enum` def whose span contains the cursor; types are
    top-level only, so there is at most one.
  - Semantic tokens: own-name uses color as their kind (constant,
    function), from the recorded sites.
- **Docs:**
  - SPEC §6 objects: "Inside a type's bodies…"
  - AGENTS.md "Paths" and "Member constants" bullets
  - `E021`'s page: the field/method help

### Tests

- **Run test** `member_scope.lilac` uses a bare constant and a bare
  static:
  - in a method
  - in a hook
  - in another constant's initializer
  - in a generic type (each instance calls its own `grow`)
  - shadowed by a local
  - shadowing a top-level item of the same name
- **`FAILS_`:**
  - a bare field or method name (`E021` with the `self.` help)
  - a bare generic static in a generic type's member constant
- **LSP:** completion inside a method lists own constants; rename of a
  member constant edits both `Type.LIMIT` and the bare `LIMIT`.
- Migrate `Standard`'s own-type qualifications where they read better
  bare (`ArenaAllocator.allocate_block(…)`). This is optional, one file
  at a time, and keeps `tests/run/stdlib` green.

## Phase 3: static vtables (addresses in global initializers)

### Rule

A global's initializer may hold `ref f`, a function's address, and
`ref OTHER`, another global's address, anywhere a value of that type
fits: a field, an array element, an enum payload. Casts between `ref`
types keep the address. Anything that computes with an address is not
constant (`E038`, as today): arithmetic, comparison, branching on it, or
`@ptr_to_address`. `=>` constants are unchanged; they have no storage
and are lowered at each use, so they already work after Phase 1.

```lilac
AllocatorTable: object {
    allocate: ref function (context: ref unknown, size: Int, alignment: Int) memory: ref unknown
    resize: ref function (context: ref unknown, memory: ref unknown, old_size: Int, new_size: Int, alignment: Int) memory: ref unknown
    free: ref function (context: ref unknown, memory: ref unknown, size: Int, alignment: Int) nothing
}

ARENA_TABLE: AllocatorTable = AllocatorTable {
    allocate = ref arena_allocate
    resize = ref arena_resize
    free = ref arena_free
}
```

### Implementation

- **Constant evaluator** (`lilac-lower/src/constant.rs`):
  - `Value::Func(FuncKey)` from `FuncRef`, and `Value::Global(DefId)`
    from `ref` of a global.
  - Neither is truthy.
  - Every operator on them is `Err(NotConstant)`, so a constant
    condition holding one lowers both arms, and a selector holding one is
    `E039`.
  - A `ref` ↔ `ref` cast passes them through.
- **Encoding** (`globals.rs::encode_value`): an address writes no bytes.
  It pushes `LirAddress { offset, target }` onto the new
  `LirGlobal::addresses` (sorted by offset), where `target` is
  `AddressTarget::Func(Symbol)` or `AddressTarget::Global(Symbol)`.
  - The symbol is the function's lowered name. Generic instances are
    already lowered, because typeck requested them.
  - The slot stays a gap in `init`. Runs and addresses never overlap.
- **Printer and IR_SPEC:** `global !g: #T = bytes { 0: "…", 8: addr
  @.root.arena_allocate, 16: addr !other }`. IR_SPEC's global grammar
  and its well-formedness rules gain the address form.
- **Verifier** (`verify.rs`), for each address:
  - the target is a function, or a global, of the module
  - the offset is aligned to the target's pointer alignment
  - offset plus pointer size ≤ the global's size
  - entries are sorted and overlap no run or other address
- **DCE** (`dce.rs`):
  - Reach becomes a walk over functions and globals. A live function's
    `addr.global` reaches the global; a live global reaches its address
    targets.
  - `prune_unreachable` keeps the globals that walk reached (today it
    keeps those live functions name directly).
  - The call graph adds an `EdgeKind::Address` edge from the function
    that names a global to each function its address closure reaches, at
    the `addr.global` span. `E031`/`E035` traces then pass through
    vtables, with no new frame kind.
- **Backend** (`lilac-codegen-llvm/src/lower.rs::global_bytes`):
  - An address is a `ptr` part of the packed struct.
  - A function's part comes from the helper `addr_func_value` uses,
    factored out as `function_address(name)` (an export's address is its
    C entry).
  - A global's part is `global_addr`.
  - Globals are defined (declared) before any initializer names one.
  - Format templates already put `ptr @show` inside constant data on
    every native target (`define_formats`), so the relocation path
    (absolute, rebase, base relocation) is proven. The run test covers
    it on Windows, Linux and macOS anyway.
- `E038`'s page says what an address in an initializer may and may not
  do.

### Tests

- **Run tests:**
  - `static_vtable.lilac`: a global table of three functions called
    through a global interface object (`{ context, table: ref TABLE }`
    in a second global's initializer).
  - A table that names an `export` and an `extern`.
  - A function reached only through a vtable survives DCE.
- **`FAILS_`:** `ref f == ref g` in a global initializer, and
  `@ptr_to_address(ref f)` (`E038`).
- **Unit tests:**
  - the verifier: misaligned, out-of-bounds, overlapping, unknown target
  - DCE: a global chain keeps its functions; an unreferenced vtable
    drops them
  - the printer round trip
  - codegen IR text for a global with a function and a global address
    on a 32-bit and a 64-bit target

## Phase 4: generic instances as values

### Rule

- **Forms:** `ref f[A, …]`, `ref Type[A].f`, `ref Type[A].f[B]`, and
  `ref Type.f[B]` for a generic method of a plain type. Each names
  exactly one instance.
  - Arguments are explicit. `ref id` with a generic `id` stays `E044`,
    with the help line "write `ref id[…]`". Lilac infers no generic
    arguments for calls either.
  - A wrong count is `E044`.
- **Failures:** an instance that fails is `E042` at the `ref` site, with
  its origin recorded, as for a call. A recursion or budget failure is
  `E051`/`E068`.
- **Templates:** `ref id[T]` inside a generic body is checked as a
  template with `T` abstract, and is requested per instance.
- **Unchanged:** hooks and intrinsics are not values, `E057` and `E028`
  still apply, and nothing here creates closures.

### Implementation

- **Parser:** inside `ref`'s operand only, a member chain may end in
  `[args]` with no `(` after it. It parses as a new `Expr::Instance {
  callee, generic_args, span }`. Today this is "expected `(` after
  generic arguments"; it stays that error everywhere else, so the
  bracket reservation holds.
  - `ref id[@bit32]` keeps parsing as `Expr::TypeApp`.
  - The nesting counter counts the new node (`Parser::link`).
- **Typeck:**
  - `check_ref_of` resolves an `Expr::TypeApp` whose path names a
    function, and an `Expr::Instance`, to `FuncKey { def, args }`. For a
    member, the arguments are the owner's then the function's own, the
    order `static_call` builds.
  - `func_value` requests the key and builds the type from the
    instance's signature.
  - `Expr::Instance` anywhere but under `ref` is `E077`, with the fix.
- **Lowering:** nothing new. `FuncRef(FuncKey)` already carries the
  arguments into `addr.func`, and Phase 3's `Value::Func` does the same.
- **Language server:**
  - Hover on `ref Box[@bit32].make` shows the instance's signature.
  - Semantic tokens and references come from `record_use`, as for calls.

### Tests

- **Run test** `generic_function_values.lilac`:
  - `ref id[@bit32]`, `ref Box[@bit8].make`, and `ref Box[@bit8].get`
    (with the receiver as the first parameter)
  - `ref Plain.pick[@bit8]` and `ref Box[@bit8].convert[@bit32]`
  - each in a global vtable (Phase 3)
  - each instantiated from inside a generic body
- **`FAILS_`:**
  - a missing argument list (`E044`)
  - an instance that fails its body (`E042` traced to the `ref`)
  - `Box[@bit8].make[@bit8]` without `ref` (`E077`)

## What this unlocks

`docs/ownership-plan.md` Phase 5.1's `Allocator` becomes an ordinary
library object, with no language interface. It holds a context and a
static table:

```lilac
Allocator: object {
    private {
        context: ref unknown
        table: ref AllocatorTable
    }
    alloc_raw: function (self, size: Int, alignment: Int) memory: ref unknown => self.table.allocate(self.context, size, alignment)
}

ArenaAllocator: object {
    allocator: function (self) result: Allocator {
        result = Allocator { context = self as ref unknown, table = ref ARENA_TABLE }
    }
}
```

`Standard` writes the table's entries as small typed wrappers
(`arena_allocate: function (context: ref unknown, …) => (context as ref
ArenaAllocator).alloc_raw(…)`), so it launders nothing. A user may
instead cast `ref ArenaAllocator.alloc_raw` through `ref unknown`; the ABI
note makes that sound. `Formatter` is the same shape with one function,
and stays as it is apart from Phase 1's migration.

## Out of scope

- **Closures and bound methods.** `ref value.method` is an error; a
  context pointer plus a function is the pattern.
- **Language-level interfaces and traits.** Tables of function
  references cover them.
- **Inferring a generic function value's arguments** from the expected
  type.
- **Hooks and intrinsics as values.**
- **String data addresses in global initializers.** `LirAddress` can
  take an `AddressTarget::Data` later.
- **A runtime null check on indirect calls.** If wanted, it should come
  with null checks on `ref` generally.
- **Formatting a function reference in an f-string.** A `@format` hook
  would take `ref` of a `ref`.

## Order and size

| Phase | Depends on | Size | Notes |
| --- | --- | --- | --- |
| 1. `ref function` values | — | large | Fixes the miscompile; most of the migration |
| 2. own-type names | member constants (uncommitted) | small | Independent of 1, except `ref grow` |
| 3. static vtables | 1 | medium | LIR, verifier, DCE, backend |
| 4. generic instances | 1 (3 for vtables of instances) | small | Parser node plus `check_ref_of` |

Each phase lands alone with the gates (`cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`), its run tests, SPEC/AGENTS.md/IR_SPEC updates
and error pages. If Phase 1 has to wait, a one-line stopgap stops the
miscompile: typeck refuses to call a local whose type is `ref` of a
function (`E022`). It is removed when Phase 1 lands.
