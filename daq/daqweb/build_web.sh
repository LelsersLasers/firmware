#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DAQ_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
DIST_DIR="${SCRIPT_DIR}/dist"

cd "${DAQ_DIR}"

echo "==> Compiling daqweb for wasm32-unknown-unknown (release)..."
cargo build --release -p daqweb --target wasm32-unknown-unknown --locked

mkdir -p "${DIST_DIR}/assets"
cp "${SCRIPT_DIR}/index.html" "${DIST_DIR}/index.html"

echo "==> Running wasm-bindgen into ${DIST_DIR}/assets..."
wasm-bindgen \
    --out-dir "${DIST_DIR}/assets" \
    --out-name "daqweb" \
    --target web \
    --no-typescript \
    "${DAQ_DIR}/target/wasm32-unknown-unknown/release/daqweb.wasm"

echo "==> Done! Web bundle ready at ${DIST_DIR}"
