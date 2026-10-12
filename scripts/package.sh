#!/usr/bin/env bash
# Builds `lilac` and `lilac-lsp` in release mode and assembles the install
# folder `package/`, which needs nothing else installed:
#
#   package/bin/lilac[.exe]            the compiler
#   package/bin/lilac-lsp[.exe]        the language server
#   package/lib/lilac/bin/lld[.exe]    the linker, from the LLVM tree
#   package/lib/lilac/<triple>/...     system import stubs (toolchain/)
#   package/lib/Standard/...           the standard library
#
# `lilac` finds `lib/lilac` relative to its own executable. The LLVM tree is
# LLVM_SYS_181_PREFIX, from the environment or `.cargo/config.toml`.
#
# Usage: scripts/package.sh
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
out="$repo/package"
target=${CARGO_TARGET_DIR:-$repo/target}

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) exe=.exe ;;
    *) exe= ;;
esac

llvm=${LLVM_SYS_181_PREFIX:-}
if [ -z "$llvm" ] && [ -f "$repo/.cargo/config.toml" ]; then
    llvm=$(sed -n "s/^LLVM_SYS_181_PREFIX *= *[\"']\(.*\)[\"'].*/\1/p" "$repo/.cargo/config.toml")
fi
if [ -z "$llvm" ] || [ ! -f "$llvm/bin/lld$exe" ]; then
    echo "error: no lld; set LLVM_SYS_181_PREFIX to the LLVM 18.1.8 tree" >&2
    exit 1
fi

cargo build --release --manifest-path "$repo/Cargo.toml" -p lilac-cli -p lilac-lsp

rm -rf "$out/bin" "$out/lib"
mkdir -p "$out/bin" "$out/lib/lilac/bin"
cp "$target/release/lilac$exe" "$target/release/lilac-lsp$exe" "$out/bin/"
cp "$llvm/bin/lld$exe" "$out/lib/lilac/bin/"
cp -R "$repo"/toolchain/*-* "$out/lib/lilac/"
cp -R "$repo/lib/Standard" "$out/lib/"
echo "packaged into $out"
