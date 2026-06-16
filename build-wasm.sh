#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

echo "==> Building molasses.wasm via wasm-pack"
wasm-pack build --target web --release

echo "==> Syncing pkg/ -> site/pkg/"
rm -rf site/pkg
mkdir -p site/pkg
cp pkg/molasses.js          site/pkg/
cp pkg/molasses.d.ts        site/pkg/
cp pkg/molasses_bg.wasm     site/pkg/
cp pkg/molasses_bg.wasm.d.ts site/pkg/
cp pkg/package.json         site/pkg/

echo "==> Done. Serve the site/ directory:"
echo "       python3 -m http.server --directory site 8000"
