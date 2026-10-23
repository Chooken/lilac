#!/usr/bin/env bash
# Builds a self-contained `lld` (every flavor) from an LLVM 18 tree's static
# libraries, for a macOS sysroot: the tree's own `lld` (Homebrew's) loads
# `@rpath/libLLVM.dylib`, so a copy of it runs nowhere else. The result
# needs only macOS's own libraries.
#
# Usage: scripts/build-lld.sh <llvm-prefix> <output>
set -euo pipefail

if [ $# -ne 2 ]; then
    echo "usage: $0 <llvm-prefix> <output>" >&2
    exit 1
fi
repo=$(cd "$(dirname "$0")/.." && pwd)
llvm=$1
out=$2
config="$llvm/bin/llvm-config"

zstd=$(brew --prefix zstd 2>/dev/null)/lib/libzstd.a
if [ ! -f "$zstd" ]; then
    echo "error: no static libzstd.a; run \`brew install zstd\`" >&2
    exit 1
fi

lld_libs=()
for driver in COFF ELF MinGW MachO Wasm Common; do
    lld_libs+=("$llvm/lib/liblld$driver.a")
done
read -r -a llvm_libs <<< "$("$config" --link-static --libfiles all)"
read -r -a system_libs <<< "$("$config" --link-static --system-libs | sed 's/-lzstd//')"
read -r -a cxxflags <<< "$("$config" --cxxflags)"

mkdir -p "$(dirname "$out")"
clang++ "${cxxflags[@]}" -O2 "$repo/scripts/lld/main.cpp" -o "$out" \
    "${lld_libs[@]}" "${llvm_libs[@]}" "$zstd" "${system_libs[@]}" \
    -Wl,-dead_strip -Wl,-x

if otool -L "$out" | tail -n +2 | grep -v '^[[:space:]]*/usr/lib/' >&2; then
    echo "error: $out loads a library outside macOS (above)" >&2
    exit 1
fi
