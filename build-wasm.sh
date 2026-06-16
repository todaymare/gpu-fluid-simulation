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
cp pkg/app_bg.wasm     site/pkg/app.wasm
cp pkg/app_bg.wasm.d.ts site/pkg/app.wasm.d.ts
cp pkg/package.json    site/pkg/
# Keep the `files` list in package.json consistent with the renamed wasm.
sed -i '' 's/app_bg.wasm/app.wasm/g' site/pkg/package.json

echo "==> Done. Serve the site/ directory:"
echo "       python3 -m http.server --directory site 8000"
