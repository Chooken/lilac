# Loop If Plan

> Status: phases 1 and 2 done, 2026-10-26; phase 3 open.
>
> Goal: `loop if c { … }` ends when `c` is false, without writing
> `else break`. Today `loop` repeats any one statement, and `loop if`
> is just `loop` over an `if`. When the `if` has no `else`, a false
> condition does nothing and the loop tests it again forever:
>
>     probe_count: Int = 0
>     loop if probe_count < capacity {   // no `else break`
>         …
>         probe_count += 1
>     }                                  // spins once probe_count == capacity
>
> That is not a hypothetical. `lib/Standard/Collections/map.lilac`
> has it twice (`Map.remove`'s back-shift loop, line 77, and
> `get_index_of`, line 267): both spin forever if the probe walks the
> whole table without meeting a `break`. Every other `loop if` in the
> repository (42 of them) writes `else break`, so the else-less form
> is never the one anybody means.
>
> Decisions:
> - **A `loop` whose statement is an `if` chain with no final `else`
>   breaks when no branch is taken** (done). `loop if a { … }`
>   and `loop if a { … } else if b { … }` both end once every
>   condition in the chain is false. A capture `if` counts too:
>   `loop if it.next() -> v: T { … }` ends when the call fails.
> - **An explicit `else` keeps today's meaning** (done).
>   `loop if c { … } else { wait() }` still loops until something in
>   it breaks, and `else break` stays legal (it is now what you get
>   anyway). The rule only fills in an `else` that is not there.
> - **Only `if`, not `match`, for now** (done). `loop match x
>   { … }` with no `else` would fit the same rule, but nothing in the
>   repository writes it, and an exhaustive `match` has no path to
>   fill in. It can follow if it comes up.
> - **No warning for a written `else break`** (done). The
>   repository's copies are removed in one mechanical pass. An editor
>   hint (faded, like unreachable code) is optional, see phase 3.
>
> `loop { if c { … } }` is how to write the old behavior on purpose.

## Phase 1: the rule

- `lilac-typeck/src/body.rs`, `Stmt::Loop`: when the body is a
  `Stmt::If`, check it with an implicit `else break` on the last `if`
  of its chain. Do it in typeck, not the parser, so the syntax tree
  (and so the editor's rendering and any later formatter) still says
  what the source says. This matches how compound assignment is
  desugared (`check_compound_assign`).
  - `FnCx` gets `implicit_break: bool`, which `Stmt::Loop` sets when
    its statement is an `if`. `check_if` (`failure.rs`) takes it at
    entry, so an `if` inside the condition or the `then` block never
    sees it, and passes the condition's span to `else_block` only when
    the `else` is another `if` (`else if`) or missing. `else_block`
    (shared with `check_capture_if`) sets the flag again for an
    `else if` and turns a missing `else` into `else break` at that
    span.
  - The implicit `break` goes through exactly the path a written one
    does, so the failure outs of a capture `if` are bound and dropped
    as they are for `else break` today (`bind_rest`), and the
    ownership pass's early-exit drops sit at the condition's span.
- `HirStmt::Break(span)` already carries what lowering needs; nothing
  changes after typeck.

## Phase 2: Standard, tests and spec

- Remove `} else break` after every `loop if` in `lib/`, `tests/`,
  `examples/` and `bench/` (42 sites; the eight `if … else break`
  inside a `loop { }` stay). Keep one test that still writes it
  (`tests/run/loop_if_else_break.lilac`) to show it is still legal.
- `map.lilac`'s two loops are fixed by the rule itself. Their
  whole-table path is unreachable today: the load factor (75%) keeps
  an empty bucket, which ends every probe first. `tests/run/map.lilac`
  walks the longest chains there are (every key hashing alike), and
  `loop_if_implicit_break` tests the loop shape.
- Run test (`tests/run/loop_if_implicit_break.lilac`): a plain `loop if` that
  ends; an `else if` chain that ends only when both are false; a
  capture `loop if` over a failing call whose failure out needs a
  drop; `continue` re-testing the condition; an explicit `else { … }`
  that still loops until it breaks.
- Typeck unit test (`tests.rs`, beside `loop_if_checks`): the HIR of
  `loop if c { }` has a `Break` in its `else`.
- `SPEC.md` ("`loop` repeats any single statement", the capture
  section at `loop if lines.next()`), `docs/ownership-plan.md`'s
  example and `Standard.Sync.Condvar`'s doc comment drop the
  `else break` and state the rule.

## Phase 3 (optional): editor hint

- `lilac-lsp/src/hints.rs`: a written `else break` directly after a
  `loop if` chain is a faded `Unnecessary` hint, with a quick fix that
  deletes it. Editor-only, never a compiler warning.
