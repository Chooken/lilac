#!/usr/bin/env bash
# Assembles a release install folder, which needs nothing else installed:
#
#   <out>/bin/lilac[.exe]
#   <out>/lib/lilac/bin/lld[.exe]        the linker (from the LLVM tree)
#   <out>/lib/lilac/<triple>/...         system import stubs (toolchain/)
#   <out>/lib/Standard/...               the standard library
#
# `lilac` finds `lib/lilac` relative to its own executable.
#
# Usage: scripts/package.sh <out-dir> <llvm-dir>
set -euo pipefail

out=${1:?usage: scripts/package.sh <out-dir> <llvm-dir>}
llvm=${2:?usage: scripts/package.sh <out-dir> <llvm-dir>}
repo=$(cd "$(dirname "$0")/.." && pwd)

cargo build --release --manifest-path "$repo/Cargo.toml" -p lilac-cli

exe=""
if [ -f "$repo/target/release/lilac.exe" ]; then
    exe=".exe"
fi

mkdir -p "$out/bin" "$out/lib/lilac/bin"
cp "$repo/target/release/lilac$exe" "$out/bin/"
cp "$llvm/bin/lld$exe" "$out/lib/lilac/bin/"
for triple in "$repo"/toolchain/*-*/; do
    cp -r "$triple" "$out/lib/lilac/"
done
rm -rf "$out/lib/Standard"
cp -r "$repo/lib/Standard" "$out/lib/"
echo "packaged into $out"
