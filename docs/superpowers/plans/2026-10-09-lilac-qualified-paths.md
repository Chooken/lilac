# Lilac Qualified Paths and Top-Level Types Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** namespaces can be named by a dotted path in type positions,
struct literals and expressions, and a type declared inside another type is
an error:

```lilac
main: function () nothing {
    sq: Geo.Shapes.Square = Geo.Shapes.Square { side = 0x4 }
    n: @bit32 = Geo.Shapes.area(sq)
    Standard.Io.print("hi")
}
```

**Why:** a nested `object`/`enum` was accepted and then unusable. The
resolver recorded it as a member, but type lookup never found it. Rather
than make nesting work, types are top-level only (decided 2026-10-09:
cleaner code; folders group related types). Separately, SPEC §6 said
qualified paths were not syntax, so a library item could only be named
after a `using`, and two imports of one name could not be told apart.

**Tech stack:** Rust workspace, no new dependencies.

## Design decisions

### Top-level types

- An `object` or `enum` in a type's body (object members, enum members,
  inside `private { }`) is `E036`, reported by the resolver, which defines
  nothing for it (its uses are `E023`).
- The parser keeps accepting the syntax, also in enum bodies, so the user
  sees `E036` and not a parse error.

### Path syntax

- A type name is a dotted path: `A.B.Type`, `A.Box[@bit8]`. Generic
  arguments go on the last segment. A `.` on a new line ends the type.
- `Enum.Tag` patterns: the qualifier is parsed as a type, so a dotted
  qualifier without generic arguments gives up its last segment as the tag
  (`A.Enum.Tag`, `@builtin.Os.Linux`).
- Expressions keep their member-access shape (`A.B.foo` parses as
  `Member(Member(A, B), foo)`). A chain of plain names followed by
  `{ … }` is a struct literal of that path, and followed by `[args]` and
  no call it is a type application (`A.Box[@bit8].make()`), as the single
  name forms already are.

### Resolving a path

The first segment is looked up innermost first:

1. a local or parameter (then the chain is a value, as today);
2. a generic parameter or builtin type;
3. a name the file sees (its namespace, ancestors, `using`s), with the
   existing `E029` for an ambiguous import, and `@entry`'s root fallback;
4. a namespace: a child of the file's namespace, else of its nearest
   ancestor that has one. The root is everyone's ancestor, so `A.B` written
   anywhere reaches the top-level `A.B` unless a closer `A` exists, and a
   library reaches its own subfolders without knowing its mount name
   (`Os.write` inside `Standard` is `Standard.Os.write`).

Each later segment steps into the namespace the previous one named: an
item first, else a child namespace. A `private` item is found only from
that namespace or a descendant (the reach rule), so an outsider gets the
same "unresolved" error a `using` gives. An item ends the path:
`A.Type.method` and `A.Enum.Variant` are static member accesses on the type
`A.Type`.

A namespace must be loaded to be named. Mounted libraries always are; a
project subfolder still needs a `using` somewhere in the unit (an unloaded
one is an unresolved name).

`@builtin.Target.os` stops being a special case: `@builtin` is a top-level
namespace found by rule 4.

### Editor

A path's definition use is recorded on its last segment, so hover and
go-to-definition work per name; namespace segments get no semantic token.

### Known limit (unchanged)

A root type `A` with a method `f` and a namespace `A` with a free function
`f` both take the symbol `A.f`; the IR verifier rejects the duplicate. This
predates the plan. Lookup is unambiguous: the type wins (rule 3 before 4).

## Tasks

### Part A: parser

- [x] Dotted type names for every name, not only `@builtin` (stop at a `.`
      on a new line).
- [x] Split `A.Enum.Tag` patterns.
- [x] Struct literal and type application after a plain-name member chain.
- [x] `Name: object`/`Name: enum` members parse in enum bodies (for
      `E036`).
- [x] Parser tests for each.

### Part B: resolver

- [x] `E036` for a type declared in a type's body (`docs/errors/E036.md`).
- [x] `Resolved::namespace_named` (rule 4), `ns_member` (namespace step
      with reach), `reaches`, a path index of namespaces.

### Part C: type checker

- [x] One head lookup (rules 3–4) and one path walk, used by
      `lookup_item` for every name.
- [x] `qualified_name` flattens any member chain through namespaces (it
      needs the function's locals), replacing the `@builtin` special case.
- [x] Uses recorded on the last segment.
- [x] Tests: `E036` in an object, an enum and a `private` block; uses of a
      rejected type are `E023`.

### Part D: tests and docs

- [x] Run tests `tests/run/paths` (a subfolder reached by path, relative
      and absolute; `Standard` by path) and `FAILS_nested_type.lilac`.
- [x] SPEC §6 (Objects, Paths), AGENTS.md. The VS Code grammar has no
      type-position rule, so it needs no change.

## Status

Done (2026-10-09).
