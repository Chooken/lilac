# I/O implementation guide

How to build `docs/io-plan.md`, step by step. The plan says *what*; this
says *how*: the exact layouts, what each function does, and the algorithms
for the hard ones. Read the plan first for the reasons.

The string guide (`docs/string-implementation.md`) is the other half. Its
"Build order across both plans" table says when each I/O phase fits, and
its "Things that will bite" list applies here too. Most important:
**string phase 1 (UTF-8 validation) lands before I/O phase 1**, so
nothing here is written twice.

The code blocks are sketches of the shape and the steps. Check the exact
syntax (guards, `match` arms, casts) against existing `Standard` code as
you write it.

## Corrections to the plan

The plan's code has these wrong; this guide uses the fixed forms.

1. **Function values cannot fail** (`E057`: "a function value cannot
   fail"). So `WriterTable`/`ReaderTable` entries return
   `Result`/`Optional`, as `Os.write`/`Os.read` do, and only `Writer`'s
   and `Reader`'s public methods have `else` outs.
2. **`Os.Error` has no `@format`**, and it needs one for panic messages
   and `print_line($"{e}")`. It also needs `InvalidData` (bad UTF-8) and
   `WriteZero` (a writer that accepts nothing).
3. **The standard-output lock must be reentrant.** A `@format` hook that
   prints, or a panic inside `print_line` whose message goes through
   standard output, would otherwise deadlock (or panic "deadlock" in
   Debug) on the thread's own lock. It is `Sync.ReentrantMutex[T]`
   (`lib/Standard/Sync/reentrant_mutex.lilac`, done).
4. **A Windows console needs UTF-16.** `WriteFile` of UTF-8 to a console
   shows mojibake unless the code page is 65001. A `File` writes through
   `WriteConsoleW` when its handle is a console, so the standard streams
   and any other console handle get it. Both it and `ReadConsoleW` are
   already declared in `Os/Windows/files.lilac` and stubbed.
5. **`run.rs` has no stdin directive.** Phase 4 adds one.
6. **`read_line` validates** (`.InvalidData`); the plan left that open.
   The beginner `read_line()` repairs instead of failing.

## Phase 1: Writer

### `Io/error.lilac` and `Os/error.lilac`

- In `Os.Error`, add `InvalidData` and `WriteZero` after `BadHandle`, and
  document both. No `errno` maps to them: only `Io` produces them.
- **Fix `Os.write_all` first:** today, a `write` that returns 0 loops
  forever. Make 0 written fail with `.WriteZero`.
- In `Io/error.lilac`:

```lilac
using Standard.Os as Os

/// Why an I/O call failed.
Error: alias Os.Error

/// `not found`, `permission denied`, ..., `OS error 1234`.
@format: function (error: ref Os.Error, out: ref Formatter) nothing {
    match error {
        .Other -> code: @bit32 => out.write($"OS error {code as Uint32}")
        .NotFound => out.write("not found")
        ...
    }
}
```

The hook lives in `Io`, not `Os`, so `Os` keeps depending on nothing
above it. Top-level hooks are unit-global, so it applies everywhere
`Io` is loaded, and `Runtime` always loads it.

### `Io/writer.lilac`

```lilac
/// The functions behind a `Writer` over a `T`. Each type keeps one as a
/// constant global and names it `WRITER_TABLE`.
WriterTable[T]: structure {
    /// Writes some of `bytes`, at least one byte unless `bytes` is empty;
    /// how many.
    write: ref function (target: ref T, bytes: @byte_buffer) Result[Int, Error]
    /// Pushes anything held to its destination.
    flush: ref function (target: ref T) Optional[Error]
}

private {
    /// `WriterTable[T]` with `T` erased; the same layout for every `T`.
    WriterFunctions: structure {
        write: ref function (target: ref unknown, bytes: @byte_buffer) Result[Int, Error]
        flush: ref function (target: ref unknown) Optional[Error]
    }
}

/// Anything bytes can be written to, borrowed: a file, a string, a buffer,
/// standard output. It never owns its target.
Writer: structure {
    private { target: ref unknown, functions: ref WriterFunctions }
}
```

Copy `Heap.Allocator`'s erasure exactly (`allocator.lilac`): `from[T]`
reads `T.WRITER_TABLE` and calls `over[T]`, and `over[T]` casts the table
`ref` through `ref unknown` to `ref WriterFunctions`. Every slot is a
`ref`, so the layouts match for every `T`.

**Methods:**

| Method | Algorithm |
|---|---|
| `write_bytes(self, bytes) nothing else error: Error` | `done = 0`; loop while `done < n`: call `functions.write(target, bytes[done ..])`. `.Ok(0)` fails `.WriteZero`, `.Ok(k)` does `done += k`, `.Error(e)` fails `e`. `.Interrupted` retries (Rust does too). |
| `write(self, text: Format) nothing else error: Error` | The `Catch` adapter below. |
| `write_line(self, text)` | `write(text)`, then `write_bytes("\n")`. With a buffered writer under it, that is still one system call. |
| `flush(self) nothing else error: Error` | `functions.flush(target)`; `.Some(e)` fails `e`. |

**The formatter adapter:**

```lilac
private {
    Catch: structure {
        writer: Writer
        error: Optional[Error]
        /// The sink: writes `bytes` unless a write already failed.
        push: function (self, bytes: @byte_buffer) nothing {
            if self.error is .None {
                self.writer.write_bytes(bytes) else -> e: Error { self.error = .Some(e) }
            }
        }
    }
}

write: function (self, text: Format) nothing else error: Error {
    catch: Catch = Catch { writer = self, error = .None }     // .None explicitly
    formatter: Formatter = Formatter.over[Catch](ref catch, ref Catch.push)
    formatter.write(text)
    if catch.error -> e: Error { fail e }
}
```

- `formatter` points at `catch`, so both stay locals of one function. Never
  return the pair (trap 1).
- After the first error, nothing more is written, and the hooks still run
  to the end. A format can't be stopped part-way, since `@format` is
  infallible.

The plan's `formatting(self) WriterFormatter` has the same problem: a
returned `WriterFormatter` would hold a formatter over its own `Catch`.
Make it a structure the caller keeps in place:

```lilac
/// A formatter over a writer, for code that only formats. Make one, call
/// `formatter()` where it lies, then `finish()`.
WriterFormatter: structure {
    private { catch: Catch }
    over: function (writer: Writer) WriterFormatter
    formatter: function (self) Formatter => Formatter.over[Catch](ref self.catch, ref Catch.push)
    finish: function (self) nothing else error: Error
}
```

### `Io/memory.lilac`

**`List[Uint8]`** (bytes, no validation): it needs a raw writer, but
`List` is in `Collections` and must not depend on `Io`. So put the table
in `Io` and use `over[T]`, not `from[T]`:

```lilac
/// A writer that appends to `list`.
bytes_writer: function (list: ref List[Uint8]) Writer => Writer.over[List[Uint8]](list, ref BYTES_TABLE)
```

`write` appends with `add_slice` and returns `.Ok(n)`; `flush` returns
`.None`.

**`String`** (must stay valid UTF-8): a writer may be handed half a scalar
(`write_bytes("\xC3")`, then `"\xA9"`). Appending each piece through
`add_bytes_lossy` would give two U+FFFDs. So the writer is an object that
holds the incomplete tail:

```lilac
/// Appends to a string, keeping it valid: a scalar split across writes is
/// joined, and anything still incomplete at `flush` or drop becomes U+FFFD.
StringWriter: structure {
    private { string: ref String, pending: @array(@bit8, 4), held: Int }
    over: function (string: ref String) StringWriter
    writer: function (self) Writer
    flush ... ; @on_drop: flushes
}
```

`write(bytes)`:

1. If `held > 0`, top up `pending` from `bytes` until it holds a whole
   sequence (its length comes from the lead byte; if a following byte is
   not a continuation, stop early). Then `add_bytes_lossy` the pending
   bytes and set `held = 0`.
2. Find the split: step back at most 3 bytes from the end of the rest. If
   a lead byte there needs more bytes than remain, hold `[lead ..]`.
3. `add_bytes_lossy` the part before it. This validates, so it is still
   valid even if the caller's bytes are garbage.
4. Return `.Ok(n)`, all of it.

`flush`: `add_bytes_lossy` whatever is held, then `held = 0`.

`StringWriter` is move-only (an `@on_drop` and no `@on_copy`) because it
holds a `ref`. A caller writes:

```lilac
out: StringWriter = StringWriter.over(ref text)
render(out.writer()) else -> e: Error { ... }
```

**`ByteReader`** comes in phase 4.

### Tests (`tests/standard/src/Io/writer/`)

- A `Writer` over a `List[Uint8]`; write formats, lines and raw bytes, and
  compare the bytes.
- A test-only `FailAfter { limit: Int, written: Int }` writer that fails
  with `.NoSpace` past `limit`. `write` reports exactly that error once.
  Count the calls: none happen after the failure.
- A writer that accepts 1 byte per call: `write_bytes` still writes it all.
- A writer that returns `.Ok(0)`: fails `.WriteZero`, no hang.
- `StringWriter`:
  - `"é"` written as `"\xC3"` then `"\xA9"` is `é`;
  - `"\xC3"` then a flush is U+FFFD;
  - `"\xC3A"` is U+FFFD then `A`.
- `$"{e}"` for a few `Error`s.

## Phase 2: buffered standard output

### `Os.is_terminal` (`Os/terminal.lilac`)

```lilac
/// Whether `handle` is an interactive terminal (a console on Windows).
is_terminal: function (handle: Handle) Bool
```

| OS | How |
|---|---|
| Windows | `mode: @bit32`; `Windows.GetConsoleMode(handle_ptr(handle), ref mode) != 0` |
| Linux | `termios: @array(@bit8, 64) {}`; `Linux.ioctl(fd, 0x5401 /* TCGETS */, address of termios) == 0`. Both architectures use `0x5401`; the 64 bytes cover the kernel's 36-byte `struct termios` |
| macOS | `MacOs.isatty(fd) == 1` |
| else | `false` |

Linux needs no `isatty`, which is the same `ioctl` in libc. Add the
new function to the cross-link program.

### `Io/buffered.lilac`: `BufferedWriter`

```lilac
BUFFER_BYTES: Int => 0x2000

/// Collects bytes and writes them in large pieces. Line mode also writes
/// out through each newline (for terminals).
BufferedWriter: structure {
    private {
        inner: Writer
        bytes: @array(@bit8, 0x2000)
        used: Int
        line: Bool
    }
    over: function (inner: Writer) BufferedWriter =>
        BufferedWriter { inner = inner, bytes = @array(@bit8, 0x2000) {}, used = 0, line = false }
    over_lines: ... line = true
    writer: function (self) Writer => Writer.from[BufferedWriter](self)
    flush: function (self) nothing else error: Error
    @on_drop: flushes, the error dropped
}
```

`over` is a `=>` so `BufferedWriter.over(...)` can initialize a constant
global.

**`write(self, bytes) Result[Int, Error]`** (the table entry):

```text
n = length of bytes
if line:
    k = index after the last '\n' in bytes, else 0
    if k > 0:
        write_through(bytes[..k]) else -> e: return .Error(e)
        // the rest (no newline) falls through as ordinary bytes
        bytes = bytes[k..]; (return value still counts all n)
if used + len(bytes) <= CAP: copy into bytes[used..]; used += len; return .Ok(n)
flush_buffer() else -> e: return .Error(e)
if len(bytes) >= CAP: inner.write_bytes(bytes) else -> e: return .Error(e); return .Ok(n)
copy into the buffer; used = len; return .Ok(n)

write_through(text):            // text ends in '\n'
    if used + len(text) <= CAP: copy in, then flush_buffer()     // one system call
    else: flush_buffer(); inner.write_bytes(text)
```

`flush_buffer`:

```text
if used == 0: return
inner.write_bytes(bytes[..used]) else -> e: used = 0; fail e
used = 0
```

- **On an error, the buffer is dropped** (`used = 0`). If it were kept,
  every later write would retry it, and a closed pipe would fail
  forever. Rust's `BufWriter` keeps it; for standard output, dropping is
  what `program | head` wants.
- The table's `flush` is `flush_buffer`, then `inner.flush()`.

### `Io/file.lilac`: `File` as a writer

Every writer over a system handle is a `File`: an open file (phase 3) or
a standard stream. It is a handle and whether it owns it, nothing more:

```lilac
File: structure {
    private {
        handle: Os.Handle
        owned: Bool            // dropping it closes `handle`
    }
    WRITER_TABLE: ref WriterTable[File] => ref file_table
    /// Not owned. Each call looks the handle up; keep the `File` to look it up once.
    standard_output: function () File => File { handle = Os.standard_output(), owned = false }
    standard_error: function () File => File { handle = Os.standard_error(), owned = false }
    writer: function (self) Writer => Writer.from[File](self)
    @on_drop: function (self) nothing   // Os.close when owned, the error dropped
    private {
        write_raw: function (self, bytes: Slice[Byte]) Result[Int, Error]   // the table's write
        flush_raw: function (self) Result[Empty, Error] => .Ok(Empty {})     // a file holds nothing back
    }
}
```

- `write_raw`: on Windows, when `Os.is_terminal(handle)`, the console
  path (below), returning all of `bytes`. Otherwise `Os.write`,
  returning the count the system took; `Writer.write_bytes` loops on it.
- The table's functions are private `write_raw`/`flush_raw`, so phase
  3's `write(text: Format)` keeps the plain name.
- It keeps no state. So the standard streams need no writer of their
  own, and standard error needs no lock. Its cost on Windows is one
  `GetConsoleMode` per write (behind a `BufferedWriter`, per flush). If
  that is ever measured, the standard-output state below can keep the
  answer.
- The standard streams are never closed: their `File`s are not owned.

### `Io/standard.lilac`: standard output (done)

**Printing flushes.** `print` and `print_line` write their text out
before they return (Go's model), so a beginner's output always shows,
and nothing is lost to a crash. The buffer is still used: a print's
text gathers there and goes out in one write at the end of the call, so
two threads never mix one print and a format with many holes is one
system call, not one per piece.

**The state** is one constant global behind a `Sync.ReentrantMutex`,
`None` until first use (`new` is a `=>`, so the global is constant, and
nothing is zeroed):

```lilac
private {
    StandardOutput: structure {
        file: File
        buffer: BufferedWriter     // over file.writer()
    }
    standard_out: Sync.ReentrantMutex[Optional[StandardOutput]] = Sync.ReentrantMutex[Optional[StandardOutput]].new(.None)
}
```

**First use** (`set_up`, under the lock): on `None`, take
`File.standard_output()` (its handle is a call on Windows) and
`is_terminal`, store `.Some(StandardOutput { … })`, then make the
buffer again over `out.file.writer()`, since the writer holds the file's
address and the file has moved into the global. It returns a `ref
StandardOutput` through `get_ref_unchecked`.

**Why reentrant.** A thread that already holds `standard_out` locks it
again without waiting: a format hook that prints during `print_line`, or
the exit flush from inside a print that panicked (below).
`Sync.ReentrantMutex` is Rust's `ReentrantLock`, which its `stdout` uses
for the same reason.

- A reentrant print writes into the same buffer in the middle of the
  outer one, and its flush writes out what the outer print had so far.
  That is the cost: the alternative is a deadlock.
- Its guards on one thread reach one `StandardOutput`, so a write into
  the buffer must not be cut by a callback. `BufferedWriter` is safe: a
  format hook runs between its writes, never inside one.
- A lock costs one `current_thread_id` (a system call on Linux), next to
  the write's own.

**The surface:**

| Function | Algorithm |
|---|---|
| `print(text)`, `print_line(text)` | `print_standard`: lock, `set_up`, write (and `\n`) into the buffer, flush, unlock. Errors are dropped, as C's `printf` does: a closed pipe ends the output, not the program. |
| `print_byte(b)` | `print_standard_bytes` of a one-byte stack array, raw: standard output is not a string, so no validation. |
| `print_char(c: @bit32)` | `Scalar.from(c)` and `print` it; a code point that is no scalar prints `Scalar.REPLACEMENT`. |
| `standard_output() Writer` | `Writer.over` the mutex with a private table: each `write`/`flush` locks, `set_up`s and writes into the buffer. The text waits until the buffer fills, a line ends on a terminal (line mode), the writer is flushed, or the program exits. |
| `standard_error() Writer` | Unbuffered and lock-free: each write is a `File.standard_error()` write (looked up per call, it is not constant on Windows). |
| `flush_standard() nothing` | For the exits, below. |

A lock held over a run of writes (`lock_standard_output`) is left until
something needs it: a print already is one unit.

**Standard error.** `print_error` and `print_error_line` gather the text
in a `BufferedWriter` on their own stack (8 KiB, inside the 128 KiB
minimum thread stack) over `standard_error()` and flush it, so a message
up to that size is one system call, and nothing is shared or locked.
The panic handler prints through `@compiler_print`, the same path, so a
panic never waits on a lock. `Io`'s old `Line` buffer is gone.

**Exit flushing.** `Runtime`'s `@exit` and `Process.exit` call
`Io.flush_standard()` before `Os.exit`:

```text
flush_standard:
    guard = standard_out.try_lock() else return   // reentrant: works on the holding thread
    if guard.value() is Some(out): out.buffer.flush(), the error dropped
    // another thread holding it: skip, its text is lost (C does the same)
```

- The panic path is `@compiler_print` (stderr), then `@exit(101)`, so a
  panic inside `print_line` (a format hook) still writes out what that
  print had buffered: the thread holds the lock, so `try_lock`
  succeeds.

### Windows console output (UTF-8 to UTF-16)

When a `File`'s handle is a console, `write_raw` converts (a private
`ConsoleUnits` on the stack):

```text
units: @array(@bit16, 512)
1. decode the bytes (they are valid UTF-8 unless a raw-byte writer fed them:
   decode, and turn each bad subpart into U+FFFD, as add_bytes_lossy does)
2. scalar < 0x10000 -> one unit; else the surrogate pair
3. when a pair no longer fits, WriteConsoleW the units, looping on the count written
4. WriteConsoleW what is left
return .Ok(all of bytes)
```

- `Os.write_console(handle, units: ref @bit16, count)` is the wrapper,
  Windows only (`.Unsupported` elsewhere). The conversion stays in `Io`.
- **A scalar split across two writes shows as U+FFFD,** since a `File`
  holds nothing back. Behind a `BufferedWriter` that is rare: it passes
  on whole writes, and a format pushes whole pieces of text. An
  unbuffered `File` written a byte at a time would show each split
  scalar as U+FFFDs.
- When the handle is not a console (a pipe or a file), `Os.write` the
  bytes unchanged: a file gets UTF-8.

### Tests

| Test | Where | Checks |
|---|---|---|
| existing `standard_print`, `standard_io` | `tests/run` | byte-for-byte unchanged |
| `stdout_then_panic` | `tests/run` | `print_line`, then `@panic`: stdout has the line, stderr the panic, exit 101 |
| `stdout_writer_exit_early` | `tests/run` | `standard_output()` writes left in the buffer, then `Process.exit(3)`: the text is out |
| `stdout_writer_flushed_at_exit` | `tests/run` | a `standard_output()` write left in the buffer when `main` returns is out, after an earlier `print_line` |
| `print_in_format_hook` | `tests/run` | a `@format` that calls `print`, used in `print_line`: no hang, both texts out |
| `panic_in_format_hook` | `tests/run` | a `@format` that panics during `print_line`: exit 101, the print's text so far flushed |
| `print_from_threads` | `tests/run` | 4 threads × 5 `print_line`s, each of 8 holes whose hook yields first: every line is whole |
| `standard_stream_writers` | `tests/run` | `File.standard_output()`/`standard_error()`, `standard_output()` and `standard_error()` writers, `print_byte`, `print_char` (U+FFFD for a surrogate) |
| `buffered_writer` | `tests/standard/src/Io` | over a recording writer: small writes give one inner write on flush; a write ≥ 8 KiB passes straight through; line mode writes at `\n`; an error drops the buffer |
| Windows console | by hand | `print_line("héllo ✓ 🌍")` and `print_error_line` of the same text in a console show correctly; there is no automated console |

Interleaving with stderr: `run.rs` captures each stream on its own, so
tests never relied on their order.

## Phase 3: File (done)

### `Io/file.lilac`

`File` and its writer come from phase 2. This phase adds opening,
reading, seeking and closing. A file it opens is owned, so dropping it
closes it, and `close()` closes it and reports the error.

It is move-only because it has `@on_drop` and no `@on_copy`, like `Box`.
That holds for the standard streams' `File`s too.

`OpenMode` and `SeekFrom` are aliases of `Os`'s, so `Io` code names
neither `Os` nor its modes' module.

**Paths** are `Format`s, formatted on the stack, so opening allocates
nothing:

```lilac
/// The longest path `File` opens, in bytes.
MAX_PATH_BYTES: Int => 0x1000

private {
    PathBuffer: structure {
        bytes: @array(Byte, 0x1000)
        used: Int
        overflowed: Bool
        empty: function () PathBuffer => ...         // every field set
        /// `.InvalidInput` when too long, or holding a zero byte
        /// (C would end the path there).
        format: function (self, path: Format) nothing else error: Error
        text: function (self) Slice[Byte]
        push: function (self, bytes: Slice[Byte]) nothing   // the sink: past the end sets `overflowed`
    }
}
```

`Os.open` and `Os.remove_file` convert the bytes (UTF-16 for
`CreateFileW`/`DeleteFileW` on Windows, NUL-terminated in pages on Linux
and macOS), so `Io` passes bytes. The path buffer is 4 KiB of stack; a
thread's default stack is large enough.

**Methods:**

| Method | Algorithm |
|---|---|
| `open(path)` | `open_with(path, .Read)` |
| `create(path)` | `.Write` (create + truncate, as `Os.open` defines it) |
| `open_with(path, mode)` | `PathBuffer.format`, then `Os.open` → `File { handle, owned = true }` |
| `read(self, into: Slice[Byte]) count: Int else error` | `Os.read` into `into`; retries `.Interrupted` |
| `write_bytes`, `write`, `write_line` | through `self.writer()`, so a console gets the console path |
| `seek(self, offset: Int64, from) position: Uint64`, `size(self) size: Uint64` | forward to `Os` |
| `writer(self) Writer` | phase 2's, unbuffered, with the console path |
| `reader(self) Reader` | phase 4 |
| `close(self: File) nothing else error: Error` | copy out `handle` and `owned`, `@discard(self)`; when `owned`, `Os.close(handle)` and fail with its error |

`close` takes `self` by value and discards it, so `@on_drop` doesn't run
and the handle isn't closed twice. Closing a `File` that doesn't own its
handle (a standard stream) only gives it up, as dropping it does.

A guard binds only a declaration, so a function that opens into its out
binds a local first (`opened: File = open_with(...) else ...`, then
`file = opened`).

### The file functions (`io.lilac`)

**`read_file(path) text: String else error: Error`:**

```text
file = File.open(path) else fail
bytes: List[Byte] = List.new()
file.size() -> size: bytes.reserve(size)      // a hint: the file can change size
chunk: @array(Byte, 0x2000) {}
loop:
    n = file.read(chunk as a Slice) else fail
    if n == 0: break
    bytes.add_slice(chunk[..n])
text = String.from_bytes(bytes) else fail .InvalidData   // validated once, whole
```

- Reading through an 8 KiB chunk costs one extra copy. `List` has no
  "spare capacity" API to read straight into. If profiling asks, add
  `List.reserve_slice(n) Slice[T]` plus `List.commit(n)`. That is a
  `Collections` change, so give it its own small review.
- `from_bytes` copies again into the `String`. Once string phase 2 lands,
  a private `String.adopt(List[Uint8])` could take the block, but a
  `String` block is not a `List` block. Leave it unless it is measured.

**`write_file(path, text: Format)`** and **`append_file`** (`.Append`)
open the file and hand it to the private `write_and_close`:

- `write_buffered(ref file, text)` writes through a `BufferedWriter`
  over `file.writer()` and flushes, so a format with many holes is few
  system calls. It is its own function so the buffered writer's borrow
  of `file` ends before the close; in one body `file` would stay
  borrowed for the rest of its scope.
- Then `@move(file).close()`, reporting the close error: it can be the
  real write error (NFS, quota).

**`remove_file(path)`** formats the path the same way and calls
`Os.remove_file` (`DeleteFileW`, `unlinkat`, `unlink`). `Os` opens every
file with `FILE_SHARE_DELETE` on Windows, so an open file can be
removed there too.

### Tests

| Test | Where | Checks |
|---|---|---|
| `files` | `tests/standard/src/Io` | `write_file` then `read_file` at 0, 1, 4095, 4096, 4097, 8192, 8193 and 100,000 bytes; a scalar split between two reads; `write_file` replaces; `append_file` appends and creates; a missing file is `.NotFound` (`read_file`, `File.open`, `remove_file`); bad UTF-8 is `.InvalidData`; a path of `MAX_PATH_BYTES + 1` bytes and one with a zero byte are `.InvalidInput`; 10,000 opens and drops; `seek`/`read`/`size`; `.ReadWrite` writes in place; `close` keeps what was written and `File.write` writes a format; closing a standard stream's `File` leaves the stream open |
| `io_link` | `crates/lilac-cli/tests` | a program using every `File` function and file function and `standard_output()`, linked for the six native targets at `debug` and `release` |

The tests name their files under the system's temporary folder (`TEMP`
on Windows, else `TMPDIR`, else `/tmp`) with a random part from
`Os.fill_random`, since tests, and the Debug and Release runs, run at
once, and remove them when done. That helper is the test file's own; a
`Testing.temporary_path` can follow when a second suite needs it.

## Phase 4: reading (done)

Built as below, with these differences: `scratch` is an
`Optional[List[Uint8]]` (`List.new` takes the default allocator, so it is
not constant), standard input lives in `Io/input.lilac` (`StandardInput`,
`standard_input()`, the private `InputSource` that converts a Windows
console's UTF-16), `File` is a reader too (`File.reader()`,
`File.standard_input()`), and `read_file` reads through `read_all`.

### `Io/reader.lilac`

```lilac
ReaderTable[T]: structure {
    /// Reads up to `into`'s count: how many bytes, 0 at the end.
    read: ref function (target: ref T, into: Slice[Uint8]) Result[Int, Error]
}
Reader: structure {
    private { target: ref unknown, functions: ref ReaderFunctions }
    from[T], over[T]                                    // as Writer
    read: function (self, into: Slice[Uint8]) count: Int else error: Error   // retries .Interrupted
    read_all: function (self, into: ref List[Uint8]) nothing else error: Error   // the read_file loop
    read_all_text: function (self, into: ref String) nothing else error: Error  // read_all into a List, then add_bytes; .InvalidData on failure, `into` unchanged
}
```

`ByteReader` (`memory.lilac`): `{ bytes: @byte_buffer, at: Int }`, which
copies from `at` and moves it on. Tests use it to feed exact input.

### `BufferedReader`

```lilac
BufferedReader: structure {
    private {
        inner: Reader
        bytes: @array(@bit8, 0x2000)
        start: Int          // first unread byte
        end: Int            // one past the last byte read in
        scratch: List[Uint8]  // a line that spans refills; kept for reuse
    }
}
```

`scratch` holds a `List`, so a constant `BufferedReader` needs `List.new`
to be constant (an empty list, with no allocation, should be). If it
isn't, use `Optional[List[Uint8]]` and make the list on first need.

**`fill(self) Int else error`**: if `start == end`, set both to 0 and read
into `bytes[0 ..]`, returning the count. With the buffer partly full, it
does nothing; this reader never compacts, it only refills when empty.

**`read_line(self, into: ref String) nothing else error: Optional[Error]`:**

```text
scratch.clear(); spanned = false; got_any = false
loop:
    if start == end:
        n = fill() else -> e: fail .Some(e)
        if n == 0:                                  // end of input
            if not got_any: fail .None
            break                                   // a last line without a newline
    got_any = true
    i = find_byte(bytes[start ..< end], '\n')
    if found at i:
        piece = bytes[start ..< i]; start = i + 1
        if not spanned: finish(piece); return       // the common case: no copy into scratch
        scratch.add_slice(piece); break
    scratch.add_slice(bytes[start ..< end]); start = end; spanned = true
finish(scratch)

finish(line):
    if line ends with '\r': drop it                 // \r\n
    into.add_bytes(line) else -> _: fail .Some(.InvalidData)   // validates; `into` unchanged on failure
```

- A line is validated once, whole. Validating per refill would split
  scalars across the 8 KiB boundary.
- The bytes of a bad line are consumed, so the next `read_line` goes on
  with the next line rather than failing forever.
- `fail .None` only when nothing at all was read since the last line. So
  `"a\n"` gives one line, `"a"` gives one line, and `""` gives none.

`read` on a `BufferedReader` (its `ReaderTable`) hands out buffered bytes
first. If the buffer is empty and the request is ≥ 8 KiB, it reads
straight into the caller's slice.

### Standard input

- `input: Sync.Mutex[StandardInput]` is a constant global holding a
  `BufferedReader` over a stateless `ConsoleIn` (`Os.read(Os.standard_input())`).
- `standard_input() StandardInput` hands out a shim with `read_line(ref
  String)` and `reader()`. Each call:
  1. calls `flush_standard()` first, so a prompt shows;
  2. locks;
  3. reads;
  4. unlocks.
- A plain `Mutex` is fine here: no `@format` hook reads input.
- On Windows, when standard input is a console, `ConsoleIn` reads with
  `ReadConsoleW` into a 512-unit stack array, then converts the units to
  UTF-8 into the reader's buffer:
  - a high surrogate at the end of the array is held for the next read;
  - a lone surrogate becomes U+FFFD;
  - `\r\n` arrives as typed and `read_line` strips it;
  - Ctrl+Z on an empty line arrives as `0x1A`: treat a line that is
    exactly `0x1A` as the end of input, as C does.

**The beginner functions:**

```lilac
/// One line from standard input, without its newline; fails at the end.
/// Bytes that are not UTF-8 become U+FFFD.
read_line: function () line: String else nothing {
    line = String.new("")
    standard_input().read_line_lossy(ref line) else -> e: Optional[Error] {
        if e -> real: Error { @panic($"reading standard input failed: {real}") }
        fail
    }
}
ask: function (text: Format) line: String else nothing {
    print(text)          // read_line flushes it
    line = read_line() else fail
}
```

`read_line_lossy` is `read_line` with `add_bytes_lossy` in `finish`. Add
it to `BufferedReader` (a `lossy: Bool` parameter on a private shared
function).

### The `// stdin:` directive (`crates/lilac-cli/tests/run.rs`)

- `// stdin: text` adds `text` and `\n` to the program's standard input;
  more lines append. `// stdin-raw: a\r\nb` takes Rust escapes (`\n`,
  `\r`, `\t`, `\\`, `\xHH`) and adds no newline, for exact bytes and a
  last line without a newline.
- Without either, stdin stays empty: the harness runs programs with
  `Command::output()`, whose child reads end of input at once.
- With one, spawn with `Stdio::piped()`, write the bytes from a thread
  (a write larger than the pipe buffer would block before the child
  reads), then close it.
- Document both in the header comment with the other directives.

### Tests

Most go in `tests/standard/src/Io/reading/` over `ByteReader`, so they
need no stdin:

- `"a\nb\n"`, `"a\nb"` and `"a\r\nb\r\n"` give `a`, `b`.
- `""` gives no lines; `"\n"` gives one empty line; `"\n\n"` gives two.
- A 20,000-byte line spans refills.
- A line of exactly 8,192 bytes plus `\n`.
- `\r` only at a refill boundary (`8191 bytes + "\r"`, then `"\n"`).
- Invalid UTF-8 in line 2: `.InvalidData`, then line 3 reads fine.
- A scalar split across a refill boundary stays whole.
- A `ByteReader` that returns 1 byte per read.

Run tests (they need real stdin):

- `read_lines_stdin`: echo each line numbered;
- `ask_prompt`: the prompt comes out before the answer is read;
- `read_line_eof`: empty stdin ends the loop at once.

## Phase 5: cleanup (done)

1. Once `Scalar` has a `@format` (string phase 1), migrate
   `print_char(c)` to `print($"{c as Scalar}")`, or to `print("…")` where
   it was a literal. `print_byte` uses become `print` of the literal.
   Then delete both. Use a script for the 187 test uses, and check the
   output stays byte-identical.
2. Done in phase 2: `Io`'s private `Line` is gone, and `print_error`
   gathers a message in a `BufferedWriter` on its stack.
3. `STDLIB.md`: the `Io` section.
4. `AGENTS.md`: a `Standard.Io` entry beside `Standard.Heap`. Cover:
   - the handle tables and why they return `Result`;
   - `Catch`;
   - buffering and exit flushing;
   - standard output's `Sync.ReentrantMutex`;
   - `File` (a handle and whether it owns it) and its Windows console
     conversion;
   - `read_line`'s validation;
   - the `// stdin:` directive.
