#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

echo "==> Building app.wasm via wasm-pack"
wasm-pack build --target web --release --out-name app

echo "==> Syncing pkg/ -> site/pkg/"
rm -rf site/pkg
mkdir -p site/pkg
cp pkg/app.js          site/pkg/
cp pkg/app.d.ts        site/pkg/
cp pkg/app_bg.wasm     site/pkg/
cp pkg/app_bg.wasm.d.ts site/pkg/
cp pkg/package.json    site/pkg/

echo "==> Done. Serve the site/ directory:"
echo "       python3 -m http.server --directory site 8000"
