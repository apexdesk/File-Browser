#!/usr/bin/env bash
# Compila la propia aplicación a WebAssembly y la deja en server/webui/ para que el servidor la embeba.
# Requiere: rustup target add wasm32-unknown-unknown y wasm-bindgen-cli 0.2.128 (misma versión que la biblioteca).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -p filebrowser --lib --target wasm32-unknown-unknown --profile "${PROFILE:-web}"
wasm-bindgen --target web --no-typescript --out-dir server/webui --out-name filebrowser_web target/wasm32-unknown-unknown/${PROFILE:-web}/filebrowser.wasm
if command -v wasm-opt >/dev/null; then wasm-opt -Oz -o server/webui/filebrowser_web_bg.wasm server/webui/filebrowser_web_bg.wasm; fi
ls -la server/webui
echo "Ahora: cargo build --release -p filebrowser-server"
