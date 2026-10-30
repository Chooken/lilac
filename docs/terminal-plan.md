# Terminal plan

A `Standard.Terminal` for programs that talk to a person at a terminal:
colored and styled text, moving the cursor and redrawing the screen, the
terminal's size, and reading keys one at a time. Beginners get one-call
helpers (`print_line($"{Style.red("error")}: ...")`); everything is
built from small pieces a library can reuse for its own TUI.

**When:** after the I/O and String plans are finished. It needs I/O
phase 2 (the buffered, locked standard output and `Os.is_terminal`),
I/O phase 4 (reading standard input) for keys, and the `Character`
(grapheme cluster) work for display widths.

## Where we are (once I/O and String land)

- `Os.is_terminal(handle)` says whether a handle is a terminal, and
  `Os.write_console` writes UTF-16 to a Windows console.
- Standard output is one buffered writer under a reentrant lock, line
  buffered on a terminal, fully buffered otherwise; standard error is
  unbuffered. `Io.flush_standard()` runs from `@exit`, so the panic path
  flushes too.
- `Formatter` is a target and a sink: it knows nothing about where its
  bytes go.
- Nothing knows the terminal's size, changes its modes, or reads a key
  without waiting for Enter.

## Principles

1. **ANSI/VT escape sequences only.** Every current terminal understands
   them: Linux and macOS terminals, Windows Terminal, and the Windows 10+
   console once `ENABLE_VIRTUAL_TERMINAL_PROCESSING` is set. No terminfo
   database and no legacy Windows console API (`SetConsoleTextAttribute`).
   Where virtual terminal mode can't be turned on, colors and cursor
   control are simply off.
2. **Two layers, like the rest of `Os`.** `Os` gets the raw platform
   calls (modes, size, raw input). `Standard.Terminal` is portable and
   is the only thing that writes escape sequences.
3. **Escape sequences only where they mean something.** Styled text
   written into a `String`, a file or a pipe comes out plain. Whether a
   destination takes colors and cursor control travels with the
   `Formatter` (below), so one `@format` hook works everywhere.
4. **The user's terminal is always given back.** Raw mode, the alternate
   screen and a hidden cursor are undone on every exit, panics included.
   A panic runs no drops, so a guard's `@on_drop` is not enough on its
   own.
5. **Building blocks, not a framework.** Styles, cursor and screen
   control, size, keys, widths. Layouts, widgets and event loops are a
   library on top, not `Standard`.

## The capability on `Formatter`

A `@format` hook can't know where its bytes go, so the destination says
what it takes:

```lilac
/// What a destination does with terminal escape sequences.
TerminalSupport: structure {
    /// How many colors it shows; `.None` writes no color or style.
    colors: ColorDepth
    /// Whether it takes cursor and screen control.
    control: Bool

    PLAIN: TerminalSupport => TerminalSupport { colors = .None, control = false }
}

ColorDepth: either { None, Basic, Palette, Rgb }    // 16, 256, 24-bit
```

- `Formatter` gets a private `support: TerminalSupport` field, `PLAIN`
  unless it is made with `Formatter.over_terminal[T](target, sink,
  support)`, and a public `terminal_support(self)`.
- `WriterTable[T]` gets an optional slot, `support: ref function
  (target: ref T) TerminalSupport`; a null slot means `PLAIN`.
  `WriterFormatter` copies the writer's support into its formatter.
- Standard output and standard error report what detection decided
  (below). `String`, `List[Byte]`, files and every other writer are
  `PLAIN`, so formatting into them strips styles for free.
- Nothing else changes: a hook that ignores `terminal_support()` works
  exactly as before.

This lives in `format.lilac` and `Io`, not `Terminal`, since `Formatter`
and `Writer` are below it. Only the two types (`TerminalSupport`,
`ColorDepth`) and the field are new there.

## Detection

Done once per stream, the first time it is written, under the stream's
lock (next to I/O phase 2's line-mode decision). A pure function, so it
can be tested without a terminal:

```lilac
/// What a stream supports, from facts the caller looks up.
detect: function (facts: DetectFacts) TerminalSupport
```

`DetectFacts` holds whether the handle is a terminal, whether VT mode
could be enabled (Windows), and the environment variables below.

| Rule, in order | Result |
|---|---|
| `CLICOLOR_FORCE` or `FORCE_COLOR` set and not `0` | colors on even when not a terminal (at the depth below); control follows the terminal check |
| not a terminal | `PLAIN` |
| Windows and `SetConsoleMode` with VT processing failed | `PLAIN` |
| `TERM=dumb` | `PLAIN` |
| `NO_COLOR` set and not empty | colors `.None`, control on |
| `COLORTERM` is `truecolor` or `24bit` | `.Rgb` |
| `TERM` contains `256color` | `.Palette` |
| Windows (VT mode on) | `.Rgb` (Windows 10+ consoles show 24-bit) |
| otherwise | `.Basic` |

- `Terminal.set_colors(stream, depth)` overrides detection, for a
  program's own `--color=always|never|auto` flag.
- A style asked for at a depth the stream lacks is downgraded: RGB to
  the nearest of the 256 palette, either to the nearest of the 16 basic
  colors. The nearest color is the smallest squared distance over the
  palette's published RGB values (a table in `Terminal/palette.lilac`).

### Turning on VT mode (Windows)

On first use of a console stream: `GetConsoleMode`, then
`SetConsoleMode(mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING (0x4))`. Keep
the old mode for the restore (below). `DISABLE_NEWLINE_AUTO_RETURN` is
left alone, so `\n` still returns the cursor to column 0.

## Phase 1: styles and colors

`lib/Standard/Terminal/style.lilac`:

```lilac
Color: either {
    Default
    Black
    Red
    Green
    Yellow
    Blue
    Magenta
    Cyan
    White
    BrightBlack
    // ... the bright eight
    BrightWhite
    /// One of the 256-color palette.
    Palette: Uint8
    /// 24-bit color.
    Rgb: Rgb
}

Rgb: structure { red: Uint8, green: Uint8, blue: Uint8 }

/// How text looks: colors and attributes. Built by value, so styles can
/// be constants: `ERROR: Style => Style.new().red().bold()`.
Style: structure {
    foreground: Color
    background: Color
    bold: Bool
    dim: Bool
    italic: Bool
    underline: Bool
    inverse: Bool
    strikethrough: Bool

    new: function () Style => ...           // all off, both colors .Default
    red: function (self: Style) Style => ...  // and one per named color
    on: function (self: Style, background: Color) Style => ...
    bold: function (self: Style) Style => ...  // and one per attribute

    /// `text` in this style.
    paint: function (self: Style, text: Format) Styled
}

/// Text in a style; writes the style, the text, then a reset.
Styled: structure {
    style: Style
    text: Format
    @format: function (self, out: ref Formatter) nothing
}
```

Shortcuts for the common case live on `Style` as static functions that
paint directly: `Style.red("error")` is `Style.new().red().paint("error")`.

**SGR sequences** (`ESC [ … m`), written only when
`out.terminal_support().colors != .None`:

| What | Parameters |
|---|---|
| reset | `0` |
| bold, dim, italic, underline, inverse, strikethrough | `1`, `2`, `3`, `4`, `7`, `9` |
| foreground: basic 8, bright 8 | `30`–`37`, `90`–`97` |
| background: basic 8, bright 8 | `40`–`47`, `100`–`107` |
| foreground palette, RGB | `38;5;n`, `38;2;r;g;b` |
| background palette, RGB | `48;5;n`, `48;2;r;g;b` |
| default foreground, background | `39`, `49` |

- One sequence per `Styled`: `ESC [ 1;31m` for bold red, not one per
  attribute.
- The reset after the text is a full `ESC [ 0m`. **Nesting:** a
  `Styled` inside another's text would reset the outer style, so
  `Styled.@format` writes the outer style again after an inner reset.
  The formatter carries the active style in a private field that
  `Styled` saves, sets and restores around its text. A style written
  over plain text never touches it.
- `Style.new()` writes nothing at all (no sequence, no reset).

**Thread safety:** a print is one locked write (I/O phase 2), so a style
and its reset never split across threads.

### Tests (`tests/standard/src/Terminal/style/`)

- Format `Styled` values into a writer whose support is forced to each
  depth, and compare the bytes: every color form, every attribute,
  combinations in one sequence, nesting restoring the outer style.
- The same values into a `String`: the plain text, no escape bytes.
- Downgrading: a few RGB values to their nearest palette and basic
  colors.
- `detect` over a table of facts, one row per rule above.

## Phase 2: cursor and screen

`lib/Standard/Terminal/control.lilac`. Each operation is a value with a
`@format`, written only when `out.terminal_support().control`, so it
composes with text in one print:

```lilac
print($"{Cursor.to(1, 1)}{Screen.CLEAR}{title}")
```

| Value | Sequence |
|---|---|
| `Cursor.to(row, column)` (1-based) | `ESC [ row;column H` |
| `Cursor.up(n)`, `down`, `right`, `left` | `ESC [ n A`, `B`, `C`, `D` |
| `Cursor.to_column(column)` | `ESC [ column G` |
| `Cursor.SAVE`, `Cursor.RESTORE` | `ESC 7`, `ESC 8` (DEC; better supported than `ESC [ s`) |
| `Cursor.HIDE`, `Cursor.SHOW` | `ESC [ ?25l`, `ESC [ ?25h` |
| `Screen.CLEAR` | `ESC [ 2J` |
| `Screen.CLEAR_BELOW` | `ESC [ 0J` |
| `Line.CLEAR`, `Line.CLEAR_RIGHT` | `ESC [ 2K`, `ESC [ 0K` |
| `Screen.ENTER_ALTERNATE`, `LEAVE_ALTERNATE` | `ESC [ ?1049h`, `ESC [ ?1049l` |

- `n == 0` writes nothing (`ESC [ 0A` moves one line on some
  terminals).
- Beginner functions wrap them: `Terminal.clear_screen()`,
  `Terminal.move_to(row, column)`, `Terminal.hide_cursor()`, ... Each
  prints the value and **flushes**, since a terminal's standard output
  is line buffered and a cursor move has no newline.
- `Terminal.flush()` is `Io.flush_standard()`, for a program that draws
  a frame of many pieces and flushes once.
- Hiding the cursor and entering the alternate screen are recorded in
  the terminal state (below), so an exit undoes them.

**Size:** `Terminal.size() TerminalSize else nothing`
(`TerminalSize { rows: Int, columns: Int }`), failing when standard
output is not a terminal. Through `Os.terminal_size(handle)`:

| OS | How |
|---|---|
| Windows | `GetConsoleScreenBufferInfo`; `columns = srWindow.Right - srWindow.Left + 1`, rows likewise from `Top`/`Bottom` (the window, not the scrollback buffer) |
| Linux | `ioctl(fd, TIOCGWINSZ (0x5413), &winsize)`; `struct winsize` is four `u16`s: rows, columns, then pixels |
| macOS | `ioctl(fd, TIOCGWINSZ (0x40087468), &winsize)`, the same layout. `ioctl` is variadic in libSystem: declare it `extern ... ...` |

A size of 0 (some CI pseudo-terminals) fails like a non-terminal.

### Tests

- Each value's bytes at `control = true`, nothing at `false`.
- `Terminal.size()` fails under `lilac test` (a pipe).
- A cross-link program (see "Cross-linking") calls `Os.terminal_size` on
  all six targets.
- `examples/terminal/`: a hand-run demo (a color chart, a box that moves
  with the size, a clock redrawn in place) for the by-hand checklist.

## Phase 3: raw mode and keys

### `Os` additions

| Function | Windows | Linux | macOS |
|---|---|---|---|
| `terminal_mode(handle) TerminalMode else Error` | `GetConsoleMode` | `ioctl(TCGETS)` into the kernel's 36-byte `termios` | `tcgetattr` into Darwin's `termios` (72 bytes: `tcflag_t` is 8 bytes there, and `c_cc` has 20 slots) |
| `set_terminal_mode(handle, mode) nothing else Error` | `SetConsoleMode` | `ioctl(TCSETSW (0x5403))`, which waits for output to drain | `tcsetattr(fd, TCSADRAIN, …)` |
| `raw_terminal_mode(mode) TerminalMode` | clear `ENABLE_LINE_INPUT`, `ENABLE_ECHO_INPUT`, `ENABLE_PROCESSED_INPUT`; set `ENABLE_VIRTUAL_TERMINAL_INPUT (0x200)` | `cfmakeraw`'s flags (below), `VMIN = 1`, `VTIME = 0` | the same flags at Darwin's values and offsets |
| `wait_readable(handle, millis) Bool` | `WaitForSingleObject` on the input handle | `poll` | `poll` |

`TerminalMode` is an opaque structure big enough for either `termios`
(`@array(@bit8, 72)`) or a Windows mode. `cfmakeraw`: clear `IGNBRK
BRKINT PARMRK ISTRIP INLCR IGNCR ICRNL IXON` in `c_iflag`, `OPOST` in
`c_oflag`, `ECHO ECHONL ICANON ISIG IEXTEN` in `c_lflag`, `CSIZE PARENB`
in `c_cflag`, then set `CS8`. Every new constant goes in the raw
namespaces by its header name (`TIOCGWINSZ`, `TCSETSW`, `ICANON`, ...),
and every new Windows or macOS function in the `toolchain/` stubs
(`tests/toolchain.rs` checks).

**Leave `OPOST` alone?** Clearing it means `\n` no longer returns to
column 0, so a raw-mode program printing lines staircases. Clear it, as
`cfmakeraw` does, but make `print_line` in raw mode write `\r\n`: the
terminal state knows raw mode is on.

### `Terminal.RawMode`

```lilac
/// Keys arrive one at a time, unechoed, with no line editing. Leaves raw
/// mode when it drops. Move-only.
RawMode: structure {
    enter: function () mode: RawMode else error: Io.Error
    read_key: function (self) event: KeyEvent else error: Io.Error
    /// `.None` when nothing arrives within `millis`.
    read_key_timeout: function (self, millis: Int) event: Optional[KeyEvent] else error: Io.Error
    read_event: function (self) event: Event else error: Io.Error
    @on_drop: function (self) nothing    // restores the saved mode
}
```

- `enter` fails when standard input is not a terminal, and when raw mode
  is already on (one at a time; the state is process-wide).
- **Ctrl+C** no longer interrupts (`ISIG` and `ENABLE_PROCESSED_INPUT`
  are off): it arrives as a key (`KeyEvent { key = .Character('c'),
  control = true }`) and the program decides. Leaving `ISIG` on would
  let `SIGINT` kill the process without running `@exit`, leaving the
  terminal raw. Document this in `RawMode`'s doc comment, and note that
  real signal handling is a later `Os` feature.
- Reading goes through standard input's reader (I/O phase 4) with its
  buffering bypassed while raw mode is on, so a key pressed before
  `enter` isn't stuck in a line buffer.

### Keys

```lilac
Key: either {
    Character: Scalar      // a printable key
    Enter
    Tab
    Backspace
    Escape
    Up
    Down
    Left
    Right
    Home
    End
    PageUp
    PageDown
    Insert
    Delete
    Function: Uint8        // F1 to F12
}

KeyEvent: structure { key: Key, control: Bool, alt: Bool, shift: Bool }

Event: either {
    Key: KeyEvent
    Resize: TerminalSize
    /// Bracketed paste: the pasted text, not a stream of keys.
    Paste: String
}
```

**One parser for every OS.** With `ENABLE_VIRTUAL_TERMINAL_INPUT`, the
Windows console sends the same VT sequences Unix terminals do, so all
three feed bytes (Windows: `ReadConsoleW`'s UTF-16 converted to UTF-8)
into one state machine, `Terminal/keys.lilac`. It is pure (bytes in,
events out), so it is tested without a terminal.

| Bytes | Key |
|---|---|
| `0x0D` | `Enter` (raw mode sends CR) |
| `0x09` | `Tab` |
| `0x7F`, `0x08` | `Backspace` |
| `0x01`–`0x1A` (not the above) | `Character('a' + n - 1)` with `control` |
| `ESC [ A` / `B` / `C` / `D` | `Up` / `Down` / `Right` / `Left` |
| `ESC [ H`, `ESC O H` / `ESC [ F`, `ESC O F` | `Home` / `End` |
| `ESC [ 2~` `3~` `5~` `6~` | `Insert` `Delete` `PageUp` `PageDown` |
| `ESC O P`–`S` | `F1`–`F4` |
| `ESC [ 15~` `17~` `18~` `19~` `20~` `21~` `23~` `24~` | `F5`–`F12` |
| `ESC [ 1 ; m X` | key `X` with modifiers `m - 1`: bit 0 shift, 1 alt, 2 control |
| `ESC [ Z` | `Tab` with `shift` |
| `ESC` then a byte `b` | `b`'s key with `alt` |
| `ESC [ 200~ … ESC [ 201~` | `Paste` of the text between (bracketed paste) |
| any other UTF-8 | `Character(scalar)` |

- **A lone Escape:** after `ESC`, wait up to 50 ms (`wait_readable`) for
  the next byte. Nothing means `Escape`; a byte continues the sequence.
- An unknown sequence is dropped whole (up to its final byte, `0x40`–
  `0x7E`), never delivered as stray characters.
- `enter` turns on bracketed paste (`ESC [ ?2004h`) and `@on_drop` turns
  it off.
- **Resizes:** no signal handling yet, so `read_event` polls
  `Os.terminal_size` each time it waits (every 100 ms while idle) and
  reports `Resize` when it changed. When `Os` gets signals, `SIGWINCH`
  replaces the poll on Unix.
- A key gives a `Scalar`, not a `Character`: a key press is one scalar.
  Clustered input (an IME, a pasted emoji) arrives as a `Paste`, or as
  consecutive `Character` keys without bracketed paste.

### Tests

- The parser over a table of byte strings, one row per line above, plus
  sequences split across reads, an unknown sequence followed by a key,
  and a lone `ESC` with the timeout faked.
- `RawMode.enter` fails under `lilac test` (input is not a terminal).
- Cross-linking every new `Os` function on six targets.
- By hand: `examples/terminal/keys` prints each event; check arrows,
  modifiers, F-keys, paste and resize in Windows Terminal, the Windows
  console, a Linux terminal and macOS Terminal.

## Restoring the terminal

`Terminal/state.lilac` keeps one process-wide record, under its own
`Sync.RawMutex`:

```lilac
TerminalState: structure {
    saved_input: Optional[Os.TerminalMode]    // raw mode on: the mode to restore
    saved_output: Optional[@bit32]            // Windows: the console mode before VT
    alternate: Bool
    cursor_hidden: Bool
    bracketed_paste: Bool
}
```

`restore()` undoes whatever is set, in reverse: leave bracketed paste,
show the cursor, leave the alternate screen, reset the style (`ESC [
0m`), restore the input mode, restore the output mode.

**Exit hooks.** `Runtime`'s `@exit` must run it before
`Io.flush_standard()`, but `Runtime` shouldn't name `Terminal`, and a
program that never uses `Terminal` shouldn't link it. So `Io` gets a
small exit list:

```lilac
/// Runs `hook` once when the process exits, before standard output is
/// flushed. At most 8 hooks; panics past that.
at_exit: function (hook: ref function () nothing) nothing
```

- `Io.flush_standard()` runs the hooks first, newest first, then
  flushes. The array is fixed (8 slots) and the count is atomic, so it
  needs no heap and is safe from any thread.
- `Terminal` registers `restore` the first time it changes any state.
- The panic handler calls `@exit(101)` after printing, so a panic in raw
  mode or on the alternate screen still gives the terminal back. **The
  order matters, though:** the handler writes the message first, so it
  would land on the alternate screen, and `restore` would then leave
  that screen and wipe it. So the hooks must also run *before* the
  message. `Standard.Runtime`'s `@compiler_print` is the panic handler's
  only way out, so it runs the exit hooks on its first call
  (`Io.run_exit_hooks()`, which marks them run, so `@exit` doesn't run
  them twice). The compiler's panic handler stays as it is.
- `Process.exit` already goes through `Io.flush_standard()` (I/O
  phase 2), so it restores too.

**Not covered:** a process killed by a signal (`kill -9`, or `SIGINT`
with `ISIG` on outside raw mode) runs nothing. The shell's `reset`
fixes the terminal. Signal handling is a separate `Os` plan.

## Phase 4: display width

How many columns text takes, for aligning columns, padding and boxes:

```lilac
/// Columns `text` takes in a terminal.
Terminal.width(text: Format) Int
Character.width(self) Int                // 0, 1 or 2
```

- **Rules** (after `wcwidth` and Unicode's `EastAsianWidth`), per
  `Character` (grapheme cluster), taking its first scalar:
  - East Asian Wide (`W`) or Fullwidth (`F`): 2;
  - `Emoji_Presentation`, or a cluster with `U+FE0F`: 2;
  - combining marks (`Mn`, `Me`), zero-width format characters (`Cf`,
    such as `U+200B` and `U+200D`), and controls: 0;
  - everything else, `A` (Ambiguous) included: 1.
- The table comes from `EastAsianWidth.txt`, `emoji-data.txt` and
  `DerivedGeneralCategory.txt`, generated by the same script that builds
  the grapheme tables (the `Character` work), into
  `lib/Standard/Unicode/width_tables.lilac`. Change the script and
  rerun it; never edit the table.
- Helpers: `Terminal.pad(text, width)` (pads with spaces to `width`
  columns) and `Terminal.truncate(text, width)` (cuts at a cluster
  boundary and adds `…` when it cut).
- Escape sequences inside `text` are 0 wide: `width` skips `ESC [ …
  final` and `ESC` plus one byte, so styled text measures as its plain
  text.

### Tests

- Widths over a table: ASCII, CJK, Hangul, combining sequences, emoji
  with and without `U+FE0F`, ZWJ families, flags, `U+200B`.
- `width` of styled text equals the plain text's.
- `pad` and `truncate` at cluster boundaries, including a wide character
  that doesn't fit the last column.

## Cross-linking

`crates/lilac-cli/tests/terminal_link.rs`, like `sync_link.rs`: one
program that calls every new `Os` function (`terminal_size`,
`terminal_mode`, `set_terminal_mode`, `wait_readable`) and builds for
the six native targets in Debug and Release, so a missing stub or a
wrong declaration fails the link.

## Order

| Phase | Needs | Adds |
|---|---|---|
| 1. Styles and colors | I/O phase 2 | `TerminalSupport` on `Formatter` and `Writer`, detection, VT mode on Windows, `Style`/`Styled` |
| 2. Cursor and screen | phase 1 | control values, `Os.terminal_size`, the terminal state and `Io.at_exit` |
| 3. Raw mode and keys | phase 2, I/O phase 4 | `Os` mode functions, `RawMode`, the key parser, events |
| 4. Display width | phase 1, `Character` | width tables, `width`, `pad`, `truncate` |

Phases 1 and 2 are useful alone (colored output, progress bars, redrawn
status lines); 3 makes interactive programs possible; 4 makes their
layout right for text that isn't ASCII.

## Not planned

- Mouse input (`ESC [ ?1006h` SGR mouse): easy to add to the parser
  later, as more `Event` variants.
- Querying the terminal (`ESC [ 6n` cursor position, color queries):
  needs raw mode and a timeout for terminals that never answer. Later,
  if a use appears.
- terminfo, the legacy Windows console API, Windows before 10.
- Widgets, layouts, an event loop: a library over these pieces.
