# Toolchain stubs

`lilac build` links natively with `lld` and these stubs, so programs need no
C toolchain or SDK. `scripts/package.sh` copies each `<triple>/` folder into
the install's `lib/lilac/`; the tests assemble a development sysroot from it.

## Windows: `kernel32.lib`, `bcryptprimitives.lib`, `synchronization.lib`

Import libraries for the functions `Standard.Os.Windows` declares, built
from `windows/<dll>.def` (`ProcessPrng` is in `bcryptprimitives.dll`,
`WaitOnAddress` and `WakeByAddress*` in the API set
`api-ms-win-core-synch-l1-2-0.dll`, which `synchronization.lib` names as
the SDK's does, the rest in `kernel32.dll`). After editing a `.def`, regenerate both of its
libraries with the LLVM tree's `llvm-lib`:

```
llvm-lib /def:toolchain/windows/kernel32.def /machine:x64 /out:toolchain/x86_64-pc-windows-msvc/kernel32.lib
llvm-lib /def:toolchain/windows/kernel32.def /machine:arm64 /out:toolchain/aarch64-pc-windows-msvc/kernel32.lib
```

`llvm-lib` also writes a `.exp` next to each; delete it. A new library
also goes in `Target::link_info`'s `system_libs`.

A program that calls other `kernel32` functions through `extern` passes its
own import library with `--link-arg=`.

## macOS: `libSystem.tbd`

A hand-written text stub (TAPI v4) for `/usr/lib/libSystem.B.dylib`, one
per architecture, listing only the symbols `Standard.Os.MacOs` declares
(with Mach-O's leading `_`) and the backend uses: `memcmp` (aggregate
`==`), `memcpy`/`memmove`/`memset`/`bzero` (LLVM lowers copies and zeroing
to them), `fmod`/`fmodf` (float `%`), `___chkstk_darwin`, and
`dyld_stub_binder`. lld needs nothing else from an SDK. Keep the two
files' symbol lists the same; a symbol missing here is an `undefined
symbol` link error.

`crates/lilac-cli/tests/toolchain.rs` checks that every `extern` of
`Standard.Os.Windows` is in a `.def` (with both `.lib`s present) and every
one of `Standard.Os.MacOs`, and each symbol the backend calls, is in both
`.tbd`s, which match.

