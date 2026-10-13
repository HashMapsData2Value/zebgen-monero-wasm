#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

cargo build --target wasm32-unknown-unknown --release
WASM=target/wasm32-unknown-unknown/release/zebgen_monero_wasm.wasm

rm -rf pkg pkg-node
wasm-bindgen --target web --out-dir pkg "$WASM"
wasm-bindgen --target nodejs --out-dir pkg-node "$WASM"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
cat > pkg/package.json <<EOF
{
  "name": "zebgen-monero-wasm",
  "version": "$VERSION",
  "license": "MIT",
  "type": "module",
  "main": "zebgen_monero_wasm.js",
  "types": "zebgen_monero_wasm.d.ts",
  "files": ["zebgen_monero_wasm.js", "zebgen_monero_wasm.d.ts", "zebgen_monero_wasm_bg.wasm", "zebgen_monero_wasm_bg.wasm.d.ts"],
  "sideEffects": false
}
EOF
cp LICENSE pkg/LICENSE

# zebgen's MV3 CSP forbids eval; fail the build if the generated glue ever uses it.
if grep -nE 'new Function|[^.a-zA-Z_]eval\(' pkg/*.js pkg-node/*.js; then
  echo "error: generated JS uses eval/new Function" >&2
  exit 1
fi
