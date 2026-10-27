#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build -p ff10k-wasm --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir crates/ff10k-wasm/pkg \
  target/wasm32-unknown-unknown/release/ff10k_wasm.wasm

WASM=crates/ff10k-wasm/pkg/ff10k_wasm_bg.wasm
if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -O3 --enable-simd --enable-bulk-memory --enable-nontrapping-float-to-int \
    --enable-sign-ext --enable-mutable-globals -o "$WASM" "$WASM"
else
  echo "wasm-opt not found; skipping (install binaryen for a smaller build)"
fi
echo "wasm size: $(stat -c %s "$WASM") bytes"
