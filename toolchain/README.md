# Toolchain stubs

`lilac build` links natively with `lld` and these stubs, so programs need no
C toolchain or SDK. `scripts/package.sh` copies each `<triple>/` folder into
the install's `lib/lilac/`; the tests assemble a development sysroot from it.

## Windows: `kernel32.lib`

An import library for the `kernel32.dll` functions `Standard.Os` calls, built
from `windows/kernel32.def`. After editing the `.def`, regenerate both with
the LLVM tree's `llvm-lib`:

```
llvm-lib /def:toolchain/windows/kernel32.def /machine:x64 /out:toolchain/x86_64-pc-windows-msvc/kernel32.lib
llvm-lib /def:toolchain/windows/kernel32.def /machine:arm64 /out:toolchain/aarch64-pc-windows-msvc/kernel32.lib
```

`llvm-lib` also writes a `kernel32.exp` next to each; delete it.

A program that calls other `kernel32` functions through `extern` passes its
own import library with `--link-arg=`.

## macOS: `libSystem.tbd`

A hand-written text stub (TAPI v4) for `/usr/lib/libSystem.B.dylib`, one
per architecture, listing only the symbols `Standard.Os` and the backend use:
`write`, `exit`, `calloc`/`malloc`/`free`, `mmap`/`munmap`, `memcpy`/`memmove`/`memset`/`bzero`
(LLVM lowers copies and zeroing to them), `fmod`/`fmodf` (float `%`),
`___chkstk_darwin`, and `dyld_stub_binder`. lld needs nothing else from an
SDK. Keep the two files' symbol lists the same; a symbol missing here is an
`undefined symbol` link error.
