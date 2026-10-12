#!/usr/bin/env bash
# Packages the VS Code extension (editors/vscode) as
# `package/lilac-<version>.vsix`. It needs Node.js and npm. Install it with
#
#   code --install-extension package/lilac-<version>.vsix
#
# The extension starts `lilac-lsp` from PATH (the `lilac.serverPath`
# setting); `scripts/package.sh` puts it in `package/bin`.
#
# Usage: scripts/package-vscode.sh
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)

cd "$repo/editors/vscode"
npm ci
# vsce packages the license from beside the manifest.
cp "$repo/LICENSE" LICENSE
mkdir -p ../../package
npm run package -- --out ../../package
