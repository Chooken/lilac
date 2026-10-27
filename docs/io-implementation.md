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
   Debug) on the thread's own lock.
4. **A Windows console needs UTF-16.** `WriteFile` of UTF-8 to a console
   shows mojibake unless the code page is 65001. Write through
   `WriteConsoleW` when the handle is a console. Both it and `ReadConsoleW`
   are already declared in `Os/Windows/files.lilac` and stubbed.
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

### `Io/standard.lilac`: standard output

**The sink.** The buffer's inner writer is a stateless writer over
`Os.standard_output()`. It holds no handle, so a constant global can
build it:

```lilac
private {
    /// Writes to the process's standard output, looking the handle up each call.
    ConsoleOut: structure {
        WRITER_TABLE: ref WriterTable[ConsoleOut] => ref console_out_table
        write: function (self, bytes: @byte_buffer) Result[Int, Error]   // Os.write, or the Windows console path
        flush: function (self) Optional[Error] => .None
    }
    console_out: ConsoleOut
    console_out_table: WriterTable[ConsoleOut] = WriterTable[ConsoleOut] { write = ref ConsoleOut.write, flush = ref ConsoleOut.flush }
}
```

A constant global can hold `ref console_out`, a `ref G` in an
initializer, so the stream's state is one global:

```lilac
private {
    StandardOutput: structure {
        lock: ReentrantLock
        buffer: BufferedWriter     // over Writer.over[ConsoleOut](ref console_out, ref console_out_table)
        ready: Bool                // line mode decided?
        console: Bool              // Windows: write UTF-16 through WriteConsoleW
        console_pending: @array(@bit8, 4), console_held: Int   // a scalar split across writes
    }
    output: StandardOutput = StandardOutput { ... }       // a constant initializer
}
```

If the initializer can't take the `Writer` (an `E038`), give
`BufferedWriter` a private `inner` setter and fill it in on first use
under the lock. Either way there is one global and one lock.

**The reentrant lock** (private to `standard.lilac`):

```lilac
ReentrantLock: structure {
    private { mutex: Sync.RawMutex, owner: @bit64, depth: Int }
    /// Holds the lock; again from the holding thread only counts.
    lock: function (self) nothing {
        me = Os.current_thread_id()
        if @atomic_load_relaxed_64(ref self.owner) == me { self.depth += 1; return }
        self.mutex.lock()
        @atomic_store_relaxed_64(ref self.owner, me)
        self.depth = 1
    }
    unlock: function (self) nothing {
        self.depth -= 1
        if self.depth == 0 { @atomic_store_relaxed_64(ref self.owner, 0x0); self.mutex.unlock() }
    }
    /// Holds the lock unless another thread does. For the exit flush.
    try_lock: function (self) acquired: Bool   // same as lock, `try_lock` on the mutex
}
```

- `owner` is read without the mutex, so it is atomic. Only the holder
  writes it, and a thread sees its own id only if it wrote it.
- Thread id 0 must mean "nobody". Check that `Os.current_thread_id`
  never returns 0 on the three OSes.
- `RawMutex.try_lock` from the holding thread returns `false` and doesn't
  panic, so `try_lock` here is safe.
- This is Rust's `ReentrantMutex` (its `stdout` uses one for the same
  reason).
- A reentrant print writes into the same buffer in the middle of the
  outer line. That is the cost: the alternative is a deadlock.

**First use** (under the lock):

```text
if not ready:
    h = Os.standard_output()
    terminal = Os.is_terminal(h)
    buffer.line = terminal
    console = target is Windows and terminal
    ready = true
```

**The public surface:**

| Function | Algorithm |
|---|---|
| `standard_output() Writer` | `Writer.over[StandardOutputShim](...)`: each `write`/`flush` takes the lock, decides `ready`, writes into `output.buffer`, then unlocks. |
| `lock_standard_output() StandardOutputGuard` | Locks once, so a run of writes is one unit. The guard's `writer()` writes with no further lock, `@on_drop` unlocks, and the guard is move-only. |
| `flush_standard() nothing` | For `@exit`, below. |

`print`, `print_line`, `print_error` and `print_error_line` (`io.lilac`)
are each one locked write. `print_line` locks, writes the text and `\n`,
then unlocks, so two threads never mix one line:

```lilac
print_line: function (text: Format) nothing {
    guard: StandardOutputGuard = lock_standard_output()
    guard.writer().write_line(text) else -> _: Error {}      // dropped, BrokenPipe too
}
```

**Standard error** is unbuffered and lock-free: a `Writer` over a
stateless `ConsoleError` that calls `Os.write_all(Os.standard_error())`
(with the console path on Windows).

- `print_error` keeps today's private `Line` stack buffer, so one message
  is still one system call.
- The panic handler prints through it, so a panic never waits on a lock.

**Exit flushing** (`Runtime/runtime.lilac`):

```lilac
@exit: function (code: @bit32) nothing {
    Io.flush_standard()
    Os.exit(code)
}
```

```text
flush_standard:
    if output.lock.try_lock():        // also true when this thread holds it (reentrant)
        output.buffer.flush() else drop
        output.lock.unlock()
    // else another thread holds it: skip, its text is lost (C does the same)
```

- The panic path is `@compiler_print` (stderr), then `@exit(101)`, so
  output printed before a panic is flushed even when the panic happened
  inside `print_line` (the thread holds the lock, so the reentrant
  `try_lock` succeeds).
- `Standard.Process.exit` calls `Os.exit` directly. Make it call
  `Io.flush_standard()` first, or a program that exits early loses its
  output.

### Windows console output (UTF-8 to UTF-16)

When `console` is set, `ConsoleOut.write` converts:

```text
units: @array(@bit16, 512)          // stack
1. join any held bytes with the start of `bytes` (as StringWriter does)
2. decode the bytes (they are valid UTF-8 unless a `List[Uint8]` writer fed raw bytes:
   decode, and turn a bad subpart into U+FFFD)
3. scalar < 0x10000 -> one unit; else the surrogate pair
4. when units is nearly full (510), WriteConsoleW it, looping on the count written
5. hold a split tail (at most 3 bytes) for the next write
return .Ok(all of bytes)
```

- `Os` gets a `write_console(handle, units: ref @bit16, count)` wrapper
  for Windows only. It is the only new `Os` function, and the conversion
  itself stays in `Io`.
- A held tail at `flush` or exit is written as U+FFFD.
- When the handle is not a console (a pipe or a file), `Os.write` the
  bytes unchanged: a file gets UTF-8.

### `print_char` and `print_byte`

Rewrite both over `print`:

- `print_byte`: one byte, through `print`'s writer as raw bytes. It
  writes to standard output, not a string, so no validation is needed.
- `print_char(c: @bit32)`: `Scalar.encode` into a 4-byte array, then
  `write_bytes`. An invalid scalar prints U+FFFD.

Both now buffer, so the 187 test uses get fast without being touched.

### Tests

| Test | Where | Checks |
|---|---|---|
| existing `standard_print`, `standard_io` | `tests/run` | byte-for-byte unchanged |
| `stdout_then_panic` | `tests/run` | `print("a")`, then `@panic`: stdout has `a`, stderr the panic, exit 101 |
| `stdout_exit_early` | `tests/run` | `print`, then `Process.exit(3)`: the text is out |
| `print_in_format_hook` | `tests/run` | a `@format` that calls `print`, used in `print_line`: no hang, both texts out |
| `panic_in_format_hook` | `tests/run` | a `@format` that panics during `print_line`: exit 101, earlier output flushed |
| `threads_print_lines` | `tests/standard/src/Io` | 4 threads × 1,000 `print_line`s of a distinct 60-byte line through a `Transcript`-like capture, or a run test that checks each output line is whole |
| `buffered_writer` | `tests/standard/src/Io` | over a recording writer: small writes give one inner write on flush; a write ≥ 8 KiB passes straight through; line mode writes at `\n`; an error drops the buffer |
| Windows console | by hand | `print_line("héllo ✓ 🌍")` in a console shows correctly; there is no automated console |

Interleaving with stderr: since stdout is fully buffered when piped, run
tests that compare stdout and stderr together never relied on order (each
stream is captured separately). Check that `run.rs` does compare them
separately; if any test merges them, it needs a flush, or it must expect
the new order.

## Phase 3: File

### `Io/file.lilac`

```lilac
/// An open file. Dropping it closes it; `close()` closes it and reports
/// the error. Not copyable.
File: structure {
    private { handle: Os.Handle }
    @on_drop: function (self) nothing { Os.close(self.handle) }    // error dropped
}
```

It is move-only because it has `@on_drop` and no `@on_copy`, like `Box`.

**Paths:**

```lilac
private {
    MAX_PATH_BYTES: Int => 4096
    /// A path formatted on the stack. `overflowed` once it is too long.
    PathBuffer: structure {
        bytes: @array(@bit8, 4096)
        used: Int
        overflowed: Bool
        push: function (self, text: @byte_buffer) nothing   // copy what fits; past the end set overflowed
        text: function (self) @byte_buffer
    }
    /// Formats `path` and opens it.
    open_path: function (path: Format, mode: Os.OpenMode) handle: Os.Handle else error: Error {
        buffer: PathBuffer = PathBuffer { ... used = 0, overflowed = false }
        formatter: Formatter = Formatter.over[PathBuffer](ref buffer, ref PathBuffer.push)
        formatter.write(path)
        if buffer.overflowed { fail .InvalidInput }
        if the bytes contain a 0 byte { fail .InvalidInput }     // C would cut the path there
        match Os.open(buffer.text(), mode) { .Ok -> h => handle = h, .Error -> e => fail e }
    }
}
```

`Os.open` already converts the path (UTF-16 for `CreateFileW` on
Windows, NUL-terminated in pages on Linux and macOS), so `Io` passes
bytes. The path buffer is 4 KiB of stack; a thread's default stack is
large enough (threads plan phase 1).

**Methods:**

| Method | Algorithm |
|---|---|
| `open(path)` | `open_path(path, .Read)` → `File { handle }` |
| `create(path)` | `.Write` (create + truncate, as `Os.open` defines it) |
| `open_with(path, mode)` | as given |
| `read(self, into: Slice[Uint8]) count: Int else error` | `Os.read(handle, into.base(), into.get_count())`; retry `.Interrupted` |
| `write_bytes(self, bytes)` | `Os.write_all`; `.Some(e)` fails `e` |
| `write(self, text: Format)` | `self.writer().write(text)` |
| `seek`, `size` | forward to `Os` |
| `writer(self) Writer` | `Writer.from[File](self)`, unbuffered |
| `reader(self) Reader` | phase 4 |
| `close(self: File) nothing else error: Error` | `handle = self.handle; @discard(self); Os.close(handle)`, and fail with its error |

`close` takes `self` by value and discards it, so `@on_drop` doesn't run
and the handle isn't closed twice.

### The file functions (`io.lilac`)

**`read_file(path) text: String else error: Error`:**

```text
file = File.open(path) else fail
bytes: List[Uint8] = List.new()
file.size() -> size: bytes.reserve(size + 1)     // a hint: the file can change size; +1 so the end-of-file read needs no grow
chunk: @array(@bit8, 0x1000) {}
loop:
    n = file.read(chunk as a Slice) else fail
    if n == 0: break
    bytes.add_slice(chunk[..n])
text = String.from_bytes(bytes) else -> _: Utf8Error: fail .InvalidData
```

- Reading through a 4 KiB chunk costs one extra copy. `List` has no
  "spare capacity" API to read straight into. If profiling asks, add
  `List.reserve_slice(n) Slice[T]` plus `List.commit(n)`. That is a
  `Collections` change, so give it its own small review.
- `from_bytes` copies again into the `String`. Once string phase 2 lands,
  a private `String.adopt(List[Uint8])` could take the block, but a
  `String` block is not a `List` block. Leave it unless it is measured.

**`write_file(path, text: Format)`:**

```text
file = File.create(path) else fail
file.write(text) else fail      // the Catch adapter; unbuffered, so wrap it:
// better: buffered = BufferedWriter.over(file.writer()); buffered.writer().write(text); buffered.flush()
file.close() else fail          // report the close error: it can be the real write error (NFS, quota)
```

Use the buffered form: a format with many holes would otherwise be one
system call per piece. The `BufferedWriter` borrows `file`, so drop it
(or let its scope end) before `close`. A move of `file` while it is
borrowed is `E079`, so give the buffered writer an inner block.

`append_file` is the same with `.Append`.

### Tests (`tests/standard/src/Io/files/`)

Use a temporary folder: name files under `Os.temporary_directory()` if it
exists; otherwise add a `Testing.temporary_path(name)` helper that
formats the process id and test index into the system temp folder.

- `write_file`, then `read_file`, round trip (empty, 1 byte, 4095, 4096,
  4097, 100,000 bytes).
- `append_file` twice.
- A missing file is `.NotFound`.
- A file with invalid UTF-8 is `.InvalidData`.
- A path of 4,097 bytes is `.InvalidInput`.
- A path with a NUL is `.InvalidInput`.
- Open and drop 10,000 times in a loop: no handle leak (it would run out).
- `seek` then `read`; `size`.
- `close()` reports `.None` normally, and a closed file is not closed
  again: no `.BadHandle` from the drop.
- `crates/lilac-cli/tests/io_link.rs`: copy `sync_link.rs`, with a
  program that uses `File`, `read_file` and `standard_output`, linked for
  the six native targets.

## Phase 4: reading

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

## Phase 5: cleanup

1. Once `Scalar` has a `@format` (string phase 1), migrate
   `print_char(c)` to `print($"{c as Scalar}")`, or to `print("…")` where
   it was a literal. `print_byte` uses become `print` of the literal.
   Then delete both. Use a script for the 187 test uses, and check the
   output stays byte-identical.
2. Remove `Io`'s private `Line`. `print_error` keeps a stack buffer of its
   own (or uses an unbuffered `Writer` with a `Catch`), so a panic
   message stays one system call.
3. `STDLIB.md`: the `Io` section.
4. `AGENTS.md`: a `Standard.Io` entry beside `Standard.Heap`. Cover:
   - the handle tables and why they return `Result`;
   - `Catch`;
   - buffering and exit flushing;
   - the reentrant standard-output lock;
   - the Windows console conversion;
   - `read_line`'s validation;
   - the `// stdin:` directive.
