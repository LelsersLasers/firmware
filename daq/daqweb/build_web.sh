#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DAQ_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
DIST_DIR="${SCRIPT_DIR}/dist"

cd "${DAQ_DIR}"

if ! command -v wasm-bindgen >/dev/null 2>&1; then
    echo "error: wasm-bindgen CLI not found in PATH (enter 'nix develop ./nix_flake' or run 'cargo install wasm-bindgen-cli')." >&2
    exit 1
fi

CLI_VERSION="$(wasm-bindgen --version | awk '{print $2}')"
LOCK_VERSION="$(awk '/^name = "wasm-bindgen"$/{getline; gsub(/"/, "", $3); print $3; exit}' Cargo.lock)"

if [[ -n "${CLI_VERSION}" && -n "${LOCK_VERSION}" && "${CLI_VERSION}" != "${LOCK_VERSION}" ]]; then
    echo "==> Aligning Cargo.lock wasm-bindgen family (${LOCK_VERSION}) with installed wasm-bindgen CLI (${CLI_VERSION})..."
    cargo update \
        -p wasm-bindgen \
        -p wasm-bindgen-macro \
        -p wasm-bindgen-macro-support \
        -p wasm-bindgen-shared \
        -p js-sys \
        -p web-sys \
        -p wasm-bindgen-futures
fi

echo "==> Compiling daqweb for wasm32-unknown-unknown (release)..."
cargo build --release -p daqweb --target wasm32-unknown-unknown --locked

mkdir -p "${DIST_DIR}/assets"
cp "${SCRIPT_DIR}/index.html" "${DIST_DIR}/index.html"
cp "${DAQ_DIR}/daqviewer/assets/per_logo.png" "${DIST_DIR}/assets/per_logo.png"

echo "==> Running wasm-bindgen into ${DIST_DIR}/assets..."
wasm-bindgen \
    --out-dir "${DIST_DIR}/assets" \
    --out-name "daqweb" \
    --target web \
    --no-typescript \
    "${DAQ_DIR}/target/wasm32-unknown-unknown/release/daqweb.wasm"

echo "==> Done! Web bundle ready at ${DIST_DIR}"
