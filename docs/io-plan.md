# Io plan

A real `Standard.Io`: `Writer` and `Reader` handles any byte stream can
stand behind, a `File` that owns an open file, buffered wrappers, and the
standard streams as shared buffered writers. The beginner functions
(`print`, `print_line`, `read_line`, `read_file`, ...) stay one call each
and become thin wrappers over that machinery instead of a parallel path.

## Where we are

- `Io` is `print`/`print_line`/`print_error`/`print_error_line`
  (`Format` in), `print_byte`, `print_char`. Each call builds a private
  `Line` (256 bytes on the stack) over `Os.standard_output()`, formats
  into it through a `Formatter`, and flushes before returning. Write
  errors are dropped.
- So every `print_char` is a system call (187 uses in the tests), and
  output is never buffered across calls.
- `Os` already has the portable layer: `Handle`, `read`, `write`,
  `write_all`, `open(path: @byte_buffer, OpenMode)`, `close`, `seek`,
  `size`, `standard_input/output/error`, and `Os.Error`.
- `Formatter` (`format.lilac`) is an infallible byte sink: a target and a
  `sink(target, bytes)`. Every `@format` hook takes `ref Formatter`.
  `String.formatter()` already makes one over a string.
- Nothing reads: no input, no files, no lines.

## Principles

1. **One backbone.** `print_line(x)` is `standard_output().write_line(x)`
   with the error dropped. No print-specific buffering or formatting code.
2. **Formatting stays infallible.** `@format` hooks keep taking
   `ref Formatter`; a type writes one hook and works with every
   destination. A fallible `Writer` adapts to a `Formatter` by recording
   the first error and skipping the rest (Rust's `fmt::Write` adapter).
3. **Handles, like `Allocator`.** `Writer`/`Reader` are a target address
   plus a `ref` to a static table of functions, built by
   `Writer.from[T]` from `T.WRITER_TABLE`. Code that writes takes a
   `Writer` and works with files, strings, buffers and the console. A
   handle borrows; it never owns. (Consistent with `Allocator`; no new
   compiler feature.)
4. **Owners are move-only.** `File` owns its `Os.Handle` and closes it in
   `@on_drop` (error dropped); `close` is explicit when the error matters.
   Like `Box`, not copyable.
5. **Errors are `Io.Error`**, an alias of `Os.Error`. Fallible calls use
   failure outs (`nothing else error: Error`), so a caller must handle
   them (`E057`). The beginner wrappers are the only place errors are
   swallowed (print) or turned into a panic or `else nothing` (reading).
6. **Paths are `Format`s.** `File.open($"{dir}/notes.txt")`, a literal
   and a `String` all work. The path is formatted into a stack buffer
   (`MAX_PATH_BYTES`, 4096; longer is `.InvalidInput`), then handed to
   `Os.open` as an `@byte_buffer`.
7. **The compiler does not change.** Everything here is library code over
   what exists (failure outs, function values, constant `Mutex` globals,
   `@discard`). If a phase finds it needs a compiler change, it stops
   and the change gets its own plan.
8. **All blocking goes through `Os`.** Every read, write, wait and sleep
   in `Io` calls `Standard.Os` (`read`, `write`, `wait_on_address`,
   `sleep`, ...), and nothing makes a system call around it. Those are
   the points where a later task scheduler can suspend a task instead
   of the thread, so async I/O can come without changing code that uses
   `Io` (`docs/threads-plan.md`, "Async I/O").

## The API

Files under `lib/Standard/Io/`:

| File | Contents |
|---|---|
| `io.lilac` | The beginner functions |
| `error.lilac` | `Error: alias Os.Error` |
| `writer.lilac` | `Writer`, `WriterTable[T]`, the formatter adapter |
| `reader.lilac` | `Reader`, `ReaderTable[T]` |
| `buffered.lilac` | `BufferedWriter`, `BufferedReader` |
| `file.lilac` | `File`, `OpenMode` re-export, `read_file`, `write_file` |
| `standard.lilac` | the standard streams and their globals |
| `memory.lilac` | `String`/`List[Uint8]` writers, `ByteReader` over a slice |

### Writer

```lilac
WriterTable[T]: structure {
    /// Writes some of `bytes`; how many were written (at least one when
    /// `bytes` is not empty).
    write: ref function (target: ref T, bytes: @byte_buffer) written: Int else error: Error
    /// Pushes anything buffered to its destination.
    flush: ref function (target: ref T) nothing else error: Error
}

Writer: structure {
    private { target: ref unknown, functions: ref WriterFunctions }

    from[T]: function (target: ref T) Writer          // T.WRITER_TABLE
    over[T]: function (target: ref T, table: ref WriterTable[T]) Writer

    write_bytes: function (self, bytes: @byte_buffer) nothing else error: Error   // loops to all
    write: function (self, text: Format) nothing else error: Error
    write_line: function (self, text: Format) nothing else error: Error
    flush: function (self) nothing else error: Error

    /// A formatter over this writer, for code that only formats. It
    /// remembers the first error; `finish` reports it.
    formatting: function (self) out: WriterFormatter
}
```

`write(text)` makes a private `Catch { writer, error: Optional[Error] }`
on its stack, `Formatter.over[Catch](ref catch, ref Catch.push)`, writes
the format, then fails with `catch.error` if set. `Catch.push` does
nothing once an error is recorded.

### Reader

```lilac
ReaderTable[T]: structure {
    /// Reads up to the slice's length into it: how many bytes, 0 at the end.
    read: ref function (target: ref T, into: Slice[Uint8]) count: Int else error: Error
}

Reader: structure {
    from[T], over[T]
    read: function (self, into: Slice[Uint8]) count: Int else error: Error
    /// Reads until the end, appending to `into`.
    read_all: function (self, into: ref List[Uint8]) nothing else error: Error
    read_all_text: function (self, into: ref String) nothing else error: Error
}
```

`read_all_text` checks UTF-8 only if `String` starts promising valid
UTF-8; today it appends bytes.

### Buffered

```lilac
/// Bytes collected and written in large pieces. `line` mode also writes
/// at each newline (for terminals).
BufferedWriter: structure {
    private { inner: Writer, bytes: @array(@bit8, 0x2000), used: Int, line: Bool }
    over: function (inner: Writer) BufferedWriter
    over_lines: function (inner: Writer) BufferedWriter
    writer: function (self) Writer          // WRITER_TABLE
    flush: function (self) nothing else error: Error
    @on_drop                                 // flushes, error dropped
}

BufferedReader: structure {
    private { inner: Reader, bytes: @array(@bit8, 0x2000), start: Int, end: Int }
    over: function (inner: Reader) BufferedReader
    reader: function (self) Reader
    /// Appends one line to `into`, without its `\n` or `\r\n`; fails with
    /// nothing at the end of input.
    read_line: function (self, into: ref String) nothing else error: Optional[Error]
}
```

A fixed 8 KiB array keeps the buffers allocation-free, so the standard
streams can be constant globals. Writes bigger than the buffer bypass it.
`read_line`'s failure out is `Optional[Error]`: `.None` is the end of
input, `.Some(e)` a real error, so a `loop if` reads every line.

### File

```lilac
File: structure {
    private { handle: Os.Handle }

    /// Opens an existing file to read.
    open: function (path: Format) file: File else error: Error
    /// Creates a file to write, emptying it if it exists.
    create: function (path: Format) file: File else error: Error
    /// Opens with an explicit mode (`.Read`, `.Write`, `.Append`, `.ReadWrite`).
    open_with: function (path: Format, mode: OpenMode) file: File else error: Error

    read: function (self, into: Slice[Uint8]) count: Int else error: Error
    write_bytes: function (self, bytes: @byte_buffer) nothing else error: Error
    write: function (self, text: Format) nothing else error: Error
    seek: function (self, offset: Int64, from: SeekFrom) position: Uint64 else error: Error
    size: function (self) size: Uint64 else error: Error

    writer: function (self) Writer   // unbuffered
    reader: function (self) Reader
    /// Closes now and reports the error; otherwise dropping closes.
    close: function (self: File) nothing else error: Error   // @discard(self)
    @on_drop                          // Os.close, error dropped
}
```

`File` itself is unbuffered (a system call per write, as Rust's). Wrap it
for speed: `buffered: BufferedWriter = BufferedWriter.over(file.writer())`.

### Standard streams

```lilac
/// The program's standard output, line-buffered on a terminal and
/// fully buffered otherwise (`print` flushes). Shared by every thread.
standard_output: function () Writer
/// Standard error: unbuffered, so a message is out before a crash.
standard_error: function () Writer
/// Standard input, buffered; reading flushes standard output first, so a
/// prompt shows before the program waits.
standard_input: function () StandardInput
```

State: `standard_out: Sync.ReentrantMutex[Optional[StandardOutput]]`
(a constant global, `.None` until first use) holding the standard output
`File` and a `BufferedWriter` over it. The `Writer` it hands out is a
shim whose `write`/`flush` lock the mutex for the call, so concurrent
prints never interleave inside one `write_line`. The lock is reentrant,
so a `@format` hook may print during a print. The same for input.

Line vs full buffering needs `Os.is_terminal(handle) Bool`
(`GetConsoleMode` on Windows, `ioctl(TCGETS)` on Linux, `isatty` on macOS)
decided once, on first use.

Flushing at exit: `Standard.Runtime`'s `@exit` and `Process.exit` call
`Io.flush_standard` before `Os.exit`. It uses `try_lock`, which the
reentrant lock grants the thread holding it, so a panic raised mid-print
(a `@format` hook that panics) still writes out that print's text and
exits instead of deadlocking; while another thread holds the lock, its
text is lost, as in C. `@compiler_print`
keeps going to standard error, which is unbuffered and unlocked for the
panic path (the panic handler already guards re-entry).

### The beginner layer (`io.lilac`)

All of these are one line over the API above:

```lilac
print: function (text: Format) nothing                 // written into the buffer, then flushed; error dropped
print_line: function (text: Format) nothing
print_error: function (text: Format) nothing
print_error_line: function (text: Format) nothing

/// One line from standard input, without its newline; fails at the end.
read_line: function () line: String else nothing
/// Prints `text`, then reads a line: `name: String = ask("Name? ") else { ... }`.
ask: function (text: Format) line: String else nothing

/// The whole file as text / writes text to a file, replacing it.
read_file: function (path: Format) text: String else error: Error
write_file: function (path: Format, text: Format) nothing else error: Error
append_file: function (path: Format, text: Format) nothing else error: Error
```

`read_line` panics on a real read error (with the error formatted) and
fails only at the end of input; a beginner's loop is
`loop if line: String = read_line() { ... }`. Code that cares uses
`standard_input().read_line(ref s)` directly.

Each `print*` call is written out before it returns (Go's model), so a
beginner's output always shows; the buffer makes it one write, not one
per piece. `print_error*` gather the text in a stack buffer, so a
message is one write too.

`print_char`/`print_byte` go: a character formats (`print($"{c}")`)
once a character type has a `@format`. Until then they stay, over
`print` (`print_char` prints the `Scalar`, U+FFFD for a code point that
is no scalar). The tests'
187 `print_char` uses are migrated in the same phase they are removed.

Errors on standard output are dropped, as today and as C's `printf`;
`BrokenPipe` stays silent so `program | head` ends quietly.

## Phases

1. **Writer and formatter adapter.** `error.lilac`, `writer.lilac`,
   `memory.lilac` (`String` and `List[Uint8]` get `WRITER_TABLE`).
   Tests: a `Writer` over a string, over a writer that fails after N
   bytes (first error reported, the rest skipped).
2. **Standard output on the backbone.** `BufferedWriter`, `Os.is_terminal`,
   `standard.lilac`, `@exit` flushing, `print*` rewritten.
   `print_char`/`print_byte` rewritten over `print`. Tests: the existing
   `standard_print`/`standard_io` pass byte-for-byte; `standard_output()`
   text piped to a file (fully buffered) is complete after a normal exit,
   `Process.exit` and `@panic`;
   two threads printing lines never mix a line; a panic inside a
   `@format` during `print_line` exits 101 rather than hanging.
3. **File.** `file.lilac`, `read_file`/`write_file`/`append_file`,
   `File`'s path formatting. Tests: write, reopen, read back; append;
   `.NotFound` on a missing file; a dropped file closes (open many in a
   loop); `seek`/`size`. Cross-link test like `sync_link.rs` for the six
   native targets.
4. **Reading.** `reader.lilac`, `BufferedReader`, `standard_input`,
   `read_line`, `ask`. Tests feed stdin (the harness needs an
   `// stdin:` directive in `run.rs`): lines with `\n` and `\r\n`, a last
   line without a newline, empty input, a line longer than the buffer.
5. **Cleanup.** A character type's `@format` (or keep `print_char` if
   none lands), migrate the tests off `print_char`, update the docs:
   `AGENTS.md` gets an `Standard.Io` entry beside `Standard.Heap`.

Each phase passes the gates on its own.

## Open questions

- **Handle vs generic.** A `Writer` handle costs an indirect call per
  write; a `BufferedWriter` in front makes that rare. If it shows in a
  benchmark, add `BufferedWriter[W]` generic over a type with
  `WRITER_TABLE` later; the handle stays the common currency.
- **`Format` paths allocate nothing** but cap at 4096 bytes. Windows long
  paths (`\\?\`) would need more; revisit if anyone hits it.
- **Text vs bytes.** `String` holds bytes today. If it later promises
  UTF-8, `read_file`/`read_line` validate and fail with `.InvalidData`
  (a new `Error` variant).
- **Directories** (`list_directory`, `remove`, `rename`, metadata) are
  the next plan, not this one.
