# Lilac VSCode Syntax Highlighter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a self-contained VSCode extension under `editors/vscode/` that syntax-highlights Lilac source (`.lilac`) and Lilac IR (`.lir`) with hand-written TextMate grammars, tested and packaged.

**Architecture:** Declarative-only extension: two `*.tmLanguage.json` grammars + one shared `language-configuration.json`, wired through `package.json`'s `contributes.languages`/`contributes.grammars`. No extension-host runtime code, no `vscode` API dependency. Grammar behavior is pinned by `vscode-tmgrammar-test` assertion files; `@vscode/vsce` produces the `.vsix`.

**Tech Stack:** JSON TextMate grammars; Node devDependencies `vscode-tmgrammar-test` (0.1.3) and `@vscode/vsce` (4.0.0); npm scripts.

**Spec:** `docs/superpowers/specs/2026-10-06-lilac-vscode-highlighter-design.md` — the plan argues from it; read both.

## Global Constraints

- Everything lives under `editors/vscode/`; **no file under `crates/` or any `Cargo.toml` is modified**.
- Highlighting is **declarative only**: no `main`/extension-host code, no `vscode` runtime dependency, no `activationEvents` (VS Code ≥1.75 derives them from contributions).
- Use the **exact TextMate scope names** in the spec's §4/§6 tables.
- Language ids: `lilac` (`.lilac`) and `lilac-ir` (`.lir`). Scope names: `source.lilac`, `source.lilac-ir`.
- Keyword/builtin/instruction lists must match `crates/lilac-lex/src/lib.rs`, `IR_SPEC.md`, and `crates/lilac-ir/src/lib.rs` **as read at implementation time**.
- `node_modules/` and `*.vsix` are gitignored; `package-lock.json` is committed.
- Test runner invocation: `vscode-tmgrammar-test "test/*.test"` (reads grammars from `package.json`; do not pass `-g`).

## Review Focus

Failure modes the spec implies but no task's happy-path test exercises; each gets the test named beside it.

1. `//` inside a string (`"http://x"`) must stay a string, not start a comment — comment pattern is first. → Task 1, `lilac.test` "comment vs string".
2. A f-string hole containing a nested string/braces (`$"a {f("x")} b"`) must re-tokenize the hole as expressions without swallowing the closing quote. → Task 1, `lilac.test` "f-string holes".
3. `@bit_x` (a hook whose name starts with a builtin prefix) must be a hook, not a builtin type. → Task 1, `lilac.test` "builtin vs hook".
4. An unterminated string/f-string at end of line must not run away and recolor the rest of the file. → Task 1, `lilac.test` "unterminated string".
5. `//` inside a `.lir` `!c0: str = "…"` literal must stay a string, not a comment. → Task 2, `lilac-ir.test` "comment vs string".
6. A `.lir` `#bit32` sigil must be a type, never a comment (Lilac IR has no `#` comments). → Task 2, `lilac-ir.test` "sigils".
7. An empty or whitespace-only file must not error. → Task 1 and Task 2, "empty input".

---

### Task 1: Extension scaffold + `.lilac` grammar

**Files:**
- Create: `editors/vscode/package.json`
- Create: `editors/vscode/language-configuration.json`
- Create: `editors/vscode/syntaxes/lilac.tmLanguage.json`
- Create: `editors/vscode/test/lilac.test`
- Create: `editors/vscode/.gitignore`
- Create: `editors/vscode/.vscodeignore`
- Create: `editors/vscode/README.md`
- Create: `editors/vscode/CHANGELOG.md`

**Interfaces:**
- Consumes: nothing.
- Produces: language id `lilac`, scope `source.lilac`; `package.json` `contributes` shape that Task 2 extends with `lilac-ir`; npm scripts `test`/`package` that Task 3 uses.

- [ ] **Step 1: Write `package.json`**

```json
{
  "name": "lilac",
  "displayName": "Lilac",
  "description": "Syntax highlighting for the Lilac language and Lilac IR",
  "version": "0.1.0",
  "publisher": "lilac-lang",
  "engines": { "vscode": "^1.75.0" },
  "categories": ["Programming Languages"],
  "contributes": {
    "languages": [
      {
        "id": "lilac",
        "aliases": ["Lilac", "lilac"],
        "extensions": [".lilac"],
        "configuration": "./language-configuration.json"
      }
    ],
    "grammars": [
      {
        "language": "lilac",
        "scopeName": "source.lilac",
        "path": "./syntaxes/lilac.tmLanguage.json"
      }
    ]
  },
  "scripts": {
    "test": "vscode-tmgrammar-test \"test/*.test\"",
    "package": "vsce package"
  },
  "devDependencies": {
    "vscode-tmgrammar-test": "^0.1.3",
    "@vscode/vsce": "^4.0.0"
  }
}
```

- [ ] **Step 2: Write `language-configuration.json`**

```json
{
  "comments": { "lineComment": "//" },
  "brackets": [["{", "}"], ["(", ")"], ["[", "]"]],
  "autoClosingPairs": [
    { "open": "{", "close": "}" },
    { "open": "(", "close": ")" },
    { "open": "[", "close": "]" },
    { "open": "\"", "close": "\"", "notIn": ["string"] },
    { "open": "'", "close": "'", "notIn": ["string"] }
  ],
  "surroundingPairs": [["{", "}"], ["(", ")"], ["[", "]"], ["\"", "\""], ["'", "'"]],
  "wordPattern": "[@#%!$&]?[A-Za-z_][A-Za-z0-9_]*"
}
```

- [ ] **Step 3: Write `syntaxes/lilac.tmLanguage.json`**

Top level: `scopeName: "source.lilac"`, `fileTypes: ["lilac"]`, and `patterns` in **this order** (order is the disambiguation; the spec's §4 table gives each scope). Implement each as a `match`/`begin`-`end` using these regexes (JSON-escaped):

| # | Pattern key | Regex (match) | Scope |
|---|---|---|---|
| 1 | comments | `//.*$` | `comment.line.double-slash.lilac` |
| 2 | decl-names (fn) | `^\s*(@?[A-Za-z_]\w*)(\s*:)(\s*)(extern\s+)?(function)\b` | cap1 `entity.name.function.lilac`; cap4 & cap5 `storage.type.lilac` |
| 3 | decl-names (type) | `^\s*([A-Za-z_]\w*)(\s*:)(\s*)(object|enum)\b` | cap1 `entity.name.type.lilac`; cap4 `storage.type.lilac` |
| 4 | builtin-types | `@(bit8|bit16|bit32|bit64|bit_native|byte_buffer|format_args)\b` | `support.type.builtin.lilac` |
| 5 | hooks | `@[A-Za-z_]\w*` | `support.function.builtin.lilac` |
| 6 | keywords | `\b(object|function|enum|extern)\b` | `storage.type.lilac` |
| 7 | keywords | `\b(using)\b` | `keyword.control.import.lilac` |
| 8 | keywords | `\b(private)\b` | `storage.modifier.lilac` |
| 9 | keywords | `\b(ref|unknown|nothing|self)\b` | `storage.type.lilac` |
| 10 | keywords | `\b(if|else|loop|match|break|return)\b` | `keyword.control.lilac` |
| 11 | keywords | `\b(and|or)\b` | `keyword.operator.lilac` |
| 12 | numbers | `\b0[xX][0-9A-Fa-f_]*\b` | `constant.numeric.hex.lilac` |
| 13 | numbers | `\b0[bB][01_]*\b` | `constant.numeric.binary.lilac` |
| 14 | numbers | `\b[0-9][0-9_]*\.[0-9][0-9_]*\b` | `constant.numeric.float.lilac` |
| 15 | numbers | `\b[0-9][0-9_]*\b` | `constant.numeric.integer.lilac` |
| 16 | fstrings | `begin: \$"`, `end: "` (see below) | `string.interpolated.lilac` |
| 17 | strings | `begin: "`, `end: "`, escapes `\\(?:["\\n]|x[0-9A-Fa-f]{2})` | `string.quoted.double.lilac` + `constant.character.escape.lilac` |
| 18 | chars | `'(\\.|[^'\\])'` | `string.quoted.single.lilac` |
| 19 | brackets | `[\[\]]` | `punctuation.section.brackets.lilac` |
| 20 | operators | `(==|=>|->|<=|>=|[.,:=+\-*/%<>!])` | `keyword.operator.lilac` |

F-string pattern (#16) must be `begin: "\$"`, `end: "\""`, with `patterns`:
- `{match: "\\{\\{|\\}\\}", name: "constant.character.escape.lilac"}` (escaped braces)
- interpolation: `begin: "\\{", end: "\\}"`, `name: "punctuation.section.interpolation.lilac"`, whose `patterns` re-include `#builtin-types`, `#hooks`, `#keywords`, `#numbers`, `#strings`, `#operators`, and a nested `#fstrings` (so `{f("x")}` tokenizes). Put this interpolation pattern before the escape pattern's siblings so `{` opens a hole; the `{{` alternative is matched by the escape pattern first.
- `{match: "\\\\[\\\\\"]", name: "constant.character.escape.lilac"}`

Decl-name patterns (#2, #3) must precede the keyword patterns so the declaration name is captured; the `function`/`object`/`enum` word on that same line is colored by the capture, and the generic keyword patterns (#6) color it on all other lines.

- [ ] **Step 4: Write `test/lilac.test`**

Use one `// SYNTAX TEST "source.lilac" "<label>"` header per case. Marker line = `//` + space + leading spaces + `^`/`<-` markers (source column = marker index after `// `). Assertions must match the §4 table. Cover every table row plus Review Focus 1–4 and 7:

```
// SYNTAX TEST "source.lilac" "declaration names and keywords"

main: function () nothing
// ^^^^ entity.name.function.lilac
//       ^^^^^^^^ storage.type.lilac
//                ^^^^^^^ storage.type.lilac

Shape: object
// ^^^^^ entity.name.type.lilac
//        ^^^^^^ storage.type.lilac

Color: enum
// ^^^^^ entity.name.type.lilac
//        ^^^^ storage.type.lilac

write: extern function () nothing
// ^^^^^ entity.name.function.lilac
//        ^^^^^^ storage.type.lilac
//               ^^^^^^^^ storage.type.lilac

// SYNTAX TEST "source.lilac" "control, modifiers, using, private"

g: function (p: ref unknown) nothing {
// ^ entity.name.function.lilac
//  ^ keyword.operator.lilac
//    ^^^^^^^^ storage.type.lilac
//             ^^^ storage.type.lilac
//                 ^^^^^^^ storage.type.lilac
    using A.B
//  ^^^^^ keyword.control.import.lilac
    private { if p and q or r { return p } else { loop { break } } }
//  ^^^^^^^ storage.modifier.lilac
//            ^^ keyword.control.lilac
//               ^^^ keyword.operator.lilac
//                     ^^ keyword.operator.lilac
//                          ^^^^^^ keyword.control.lilac
//                                        ^^^^^ keyword.control.lilac
//                                              ^^^^^ keyword.control.lilac
}

// SYNTAX TEST "source.lilac" "builtin vs hook"

a: @bit32 = @add(x, y)
//  ^^^^^^ support.type.builtin.lilac
//            ^^^^ support.function.builtin.lilac
b: @bit_x = @byte_buffer
//  ^^^^^^ support.function.builtin.lilac
//            ^^^^^^^^^^^^ support.type.builtin.lilac

// SYNTAX TEST "source.lilac" "numbers"

n: @bit32 = 0xFF_FF
//          ^^^^^^^ constant.numeric.hex.lilac
m: @bit32 = 0b10_10
//          ^^^^^^^ constant.numeric.binary.lilac
f: @bit32 = 10.5
//          ^^^^ constant.numeric.float.lilac
i: @bit32 = 10_000
//          ^^^^^^ constant.numeric.integer.lilac

// SYNTAX TEST "source.lilac" "strings, chars, escapes, comment vs string"

s: @byte_buffer = "hi\n"
//                 ^^^^^^ string.quoted.double.lilac
//                    ^^ constant.character.escape.lilac
u: @byte_buffer = "http://x"
//                 ^^^^^^^^^^ string.quoted.double.lilac
c: @bit32 = 'A'
//          ^^^ string.quoted.single.lilac

// SYNTAX TEST "source.lilac" "f-string holes"

msg: @format_args = $"hi {name}!"
//                   ^^ string.interpolated.lilac
//                        ^ punctuation.section.interpolation.lilac
//                         ^^^^ source.lilac
//                             ^ punctuation.section.interpolation.lilac

// SYNTAX TEST "source.lilac" "unterminated string"

s: @byte_buffer = "no close
t: @bit32 = 1
//          ^ constant.numeric.integer.lilac

// SYNTAX TEST "source.lilac" "empty input"

```

The exact caret columns are mechanical: run the tool (Step 5), read the reported actual scopes, and correct any misaligned marker. Do **not** change a scope to make a test pass — the §4 table is authoritative; fix the grammar instead.

- [ ] **Step 5: Install devDependencies and run the tests**

Run: `npm install` (in `editors/vscode/`), then `npm test`
Expected: the `lilac.test` cases pass; if a marker is misaligned the tool prints the actual scopes for that line — realign and re-run.

- [ ] **Step 6: Write `.gitignore`, `.vscodeignore`, `README.md`, `CHANGELOG.md`**

`.gitignore`: `node_modules/` and `*.vsix`.
`.vscodeignore`:
```
.vscode/**
test/**
node_modules/**
*.vsix
.gitignore
package-lock.json
```
`README.md`: what the extension is, the two language ids/extensions, how to install the VSIX, and the `npm test` / `npm run package` scripts.
`CHANGELOG.md`: a `## 0.1.0` entry ("Initial release: `.lilac` syntax highlighting").

- [ ] **Step 7: Commit**

```bash
git add editors/vscode
git commit -m "editors: add VSCode extension scaffold and .lilac grammar"
```

---

### Task 2: `.lir` grammar

**Files:**
- Modify: `editors/vscode/package.json` (add the `lilac-ir` language + grammar)
- Create: `editors/vscode/syntaxes/lilac-ir.tmLanguage.json`
- Create: `editors/vscode/test/lilac-ir.test`

**Interfaces:**
- Consumes: Task 1's `package.json` `contributes` shape and the `test` script.
- Produces: language id `lilac-ir`, scope `source.lilac-ir`.

- [ ] **Step 1: Extend `package.json`**

Add to `contributes.languages`:
```json
{
  "id": "lilac-ir",
  "aliases": ["Lilac IR", "lilac-ir"],
  "extensions": [".lir"],
  "configuration": "./language-configuration.json"
}
```
Add to `contributes.grammars`:
```json
{
  "language": "lilac-ir",
  "scopeName": "source.lilac-ir",
  "path": "./syntaxes/lilac-ir.tmLanguage.json"
}
```

- [ ] **Step 2: Write `syntaxes/lilac-ir.tmLanguage.json`**

`scopeName: "source.lilac-ir"`, `fileTypes: ["lir"]`, `patterns` in this order (spec §6 gives the scopes):

| # | Pattern key | Regex (match) | Scope |
|---|---|---|---|
| 1 | comments | `//.*$` | `comment.line.double-slash.lilac-ir` |
| 2 | header | `\b(lilac_ir|target)\b(?=\s*=)` | `keyword.other.lilac-ir` |
| 3 | sigil-const | `![\w.]+` | `variable.other.constant.lilac-ir` |
| 4 | sigil-func | `@[\w.]+` | `entity.name.function.lilac-ir` |
| 5 | sigil-type | `#[\w.]+` | `entity.name.type.lilac-ir` |
| 6 | sigil-place | `%[\w.]+` | `variable.other.lilac-ir` |
| 7 | sigil-label | `\$[\w.]+` | `entity.name.label.lilac-ir` |
| 8 | sigil-reg | `&[\w.]+` | `variable.other.ssa.lilac-ir` |
| 9 | keywords | `\b(function|extern|type)\b` | `storage.type.lilac-ir` |
| 10 | keywords | `\b(in|out)\b` | `storage.modifier.lilac-ir` |
| 11 | keywords | `\b(str|numlit)\b` | `storage.type.lilac-ir` |
| 12 | keywords | `\bzeroed\b` | `storage.modifier.lilac-ir` |
| 13 | instructions | `\b(call|copy|const|load|addr\.func|addr|field\.addr|field|drop|alloca|branch\.if|branch|return|panic|unreachable)\b` | `keyword.other.instruction.lilac-ir` |
| 14 | operators | `\b(i\.div\.u|i\.mod\.u|i\.add|i\.sub|i\.mul|i\.div|i\.mod|f\.add|f\.sub|f\.mul|f\.div|f\.mod|cmp\.eql|i\.cmp\.lt\.u|i\.cmp\.lt|i\.cmp\.gt\.u|i\.cmp\.gt|f\.cmp\.lt|f\.cmp\.gt|extend\.zero|trunc)\b` | `keyword.operator.lilac-ir` |
| 15 | numbers | `\b0[xX][0-9A-Fa-f_]*\b`, `\b0[bB][01_]*\b`, `\b[0-9][0-9_]*\.[0-9][0-9_]*\b`, `\b[0-9][0-9_]*\b` | `constant.numeric.lilac-ir` |
| 16 | strings | `begin: "`, `end: "`, escapes `\\.` | `string.quoted.double.lilac-ir` |

In #13 and #14 the longer alternatives must come first (`addr.func` before `addr`, `branch.if` before `branch`, `i.div.u` before `i.div`), or the shorter prefix wins. #2 must precede #13 so `target`/`lilac_ir` are header keys, not instructions.

- [ ] **Step 3: Write `test/lilac-ir.test`**

Cover every §6 row plus Review Focus 5–7:

```
// SYNTAX TEST "source.lilac-ir" "header and sigils"

lilac_ir = "0.3"
// ^^^^^^^ keyword.other.lilac-ir
//           ^^^^^ string.quoted.double.lilac-ir
target = "x86_64-unknown-none"
// ^^^^ keyword.other.lilac-ir

// SYNTAX TEST "source.lilac-ir" "data, types, functions, places, labels, regs"

!c0: str = "hi"
// ^ variable.other.constant.lilac-ir
//   ^^^ storage.type.lilac-ir
type #Number {
//   ^^^^^^^ entity.name.type.lilac-ir
function @main
// ^^^^^^^^ storage.type.lilac-ir
//          ^^^^^ entity.name.function.lilac-ir
$entry:
// ^^^^^ entity.name.label.lilac-ir
alloca #bit_native %n : zeroed
// ^^^^ keyword.other.instruction.lilac-ir
//      ^^^^^^^^^^^ entity.name.type.lilac-ir
//                  ^^ variable.other.lilac-ir
//                       ^^^^^^ storage.modifier.lilac-ir
&1: #bit32 = const 0x1
// ^ variable.other.ssa.lilac-ir
//    ^^^^^^ entity.name.type.lilac-ir
//           ^^^^^ keyword.other.instruction.lilac-ir
//                 ^^^ constant.numeric.lilac-ir
copy &1 -> %tmp.2
// ^^ keyword.other.instruction.lilac-ir
call @write (in %tmp.2) (out %tmp.1)
// ^^ keyword.other.instruction.lilac-ir
//            ^^ storage.modifier.lilac-ir
//                    ^^^ storage.modifier.lilac-ir
&3: #bit_native = i.add &1, &2
//                ^^^^^ keyword.operator.lilac-ir

// SYNTAX TEST "source.lilac-ir" "comment vs string"

!c1: str = "http://x"
//          ^^^^^^^^^^ string.quoted.double.lilac-ir
// idx 1
// ^^^^^ comment.line.double-slash.lilac-ir

// SYNTAX TEST "source.lilac-ir" "empty input"

```

Run the tool and realign any marker (same rule: the §6 table is authoritative, not the tool's output).

- [ ] **Step 4: Run the tests**

Run: `npm test` (in `editors/vscode/`)
Expected: both `lilac.test` and `lilac-ir.test` pass.

- [ ] **Step 5: Commit**

```bash
git add editors/vscode
git commit -m "editors: add .lir grammar and manifest wiring"
```

---

### Task 3: Packaging and docs

**Files:**
- Modify: `editors/vscode/README.md`, `editors/vscode/CHANGELOG.md`
- Verify: `editors/vscode/.vscodeignore`

**Interfaces:**
- Consumes: Task 1+2 `package.json` (`package` script, contributions).
- Produces: `editors/vscode/lilac-0.1.0.vsix`.

- [ ] **Step 1: Finalize docs**

`README.md` states: features (both languages), the two language ids/extensions, install-from-VSIX steps, and `npm test` / `npm run package`. `CHANGELOG.md` `## 0.1.0` lists `.lilac` and `.lir` highlighting.

- [ ] **Step 2: Package**

Run: `npm run package` (in `editors/vscode/`)
Expected: `lilac-0.1.0.vsix` is created and `vsce` reports success. If it errors on a missing field (publisher/license), fix `package.json` — do not add runtime dependencies.

- [ ] **Step 3: Verify the package contents**

Run: `npx vsce ls`
Expected: output includes `package.json`, `language-configuration.json`, `syntaxes/lilac.tmLanguage.json`, `syntaxes/lilac-ir.tmLanguage.json`, `README.md`, `CHANGELOG.md`; it does **not** include anything under `test/`, `node_modules/`, or the `.vsix` itself.

- [ ] **Step 4: Commit**

```bash
git add editors/vscode
git commit -m "editors: package the Lilac extension and finish docs"
```

---

## Self-Review

- **Spec coverage:** §2 structure → Tasks 1–2 files; §3/§4 `.lilac` → Task 1; §5/§6 `.lir` → Task 2; §7 language config → Task 1 Step 2; §8 tests → Task 1 Step 4/5, Task 2 Step 3/4; §9 packaging → Task 3; §10 maintenance → noted in Global Constraints + README. No gaps.
- **Step scan:** each step writes one file or runs one command; no undecided steps.
- **Type consistency:** language ids `lilac`/`lilac-ir`, scopes `source.lilac`/`source.lilac-ir`, and the `test`/`package` script names are identical across tasks.
- **Review Focus:** all seven items are pinned to a named test in Tasks 1–2.
- **Proportion:** shorter than the spec; grammar JSON bodies are left to the implementer from the regex+scope tables.
