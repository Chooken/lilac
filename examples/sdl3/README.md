# SDL3 from Lilac

A window drawn through [SDL3](https://libsdl.org), built in native mode
(no C library). It shows an animated triangle and a bouncing square for
six seconds (a key or closing the window ends it sooner). First it
round-trips a GUID through `SDL_StringToGUID` and `SDL_GUIDToString`,
which return and take the 16-byte `SDL_GUID` struct by value.

## Linked to SDL3.dll

Windows x86-64, with `SDL3.dll` (3.2 or later) in this folder:

```bash
llvm-dlltool -m i386:x86-64 -d SDL3.def -l SDL3.lib
lilac build
cp SDL3.dll build/debug/
build/debug/sdl3_demo
```

`SDL3.def` lists the functions `src/main.lilac` declares; add a line
there for each new `extern`.

## Linked statically

`static/lilac.toml` builds the same program into one executable that
loads no `SDL3.dll` and no C runtime, only Windows' own DLLs. It needs
SDL3 built as a static library without the C library (`SDL_LIBC=OFF`),
with Clang and the Windows SDK. From an SDL3 source release (3.4 or
later), in a shell that leaves `-GS-` alone (Git Bash: set
`MSYS_NO_PATHCONV=1`):

```bash
cmake -S SDL3-3.4.18 -B sdl-build -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_COMPILER=clang-cl -DCMAKE_CXX_COMPILER=clang-cl -DCMAKE_LINKER=lld-link -DCMAKE_AR=llvm-lib -DCMAKE_RC_COMPILER=llvm-rc -DCMAKE_MT=llvm-mt "-DCMAKE_C_FLAGS=-GS- -Gs1048576" "-DCMAKE_CXX_FLAGS=-GS- -Gs1048576 -EHsc" -DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded -DSDL_STATIC=ON -DSDL_SHARED=OFF -DSDL_LIBC=OFF -DSDL_TESTS=OFF -DSDL_TEST_LIBRARY=OFF -DSDL_EXAMPLES=OFF
```

```bash
cmake --build sdl-build
```

Then copy `sdl-build/SDL3-static.lib` into `static/` and build there:

```bash
cd static
lilac build
build/debug/sdl3_demo
```

`-GS-` keeps the compiler from calling the C runtime's stack-cookie
check. The static library leaves `memcpy`, `memset`, `__chkstk` and
`_fltused` to the program; a native Lilac program defines them (weakly,
so SDL's own `memcpy`, `memmove` and `memset` win). `lld` finds
`user32.lib` and the other system import libraries in the Windows SDK.
