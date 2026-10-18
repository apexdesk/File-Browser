#!/usr/bin/env bash
# Compila el cliente web a WebAssembly y lo deja en server/webui/ para que el servidor lo embeba.
# Requiere: rustup target add wasm32-unknown-unknown y wasm-bindgen-cli 0.2.128 (misma versión que la biblioteca).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -p filebrowser-web --target wasm32-unknown-unknown --release
wasm-bindgen --target web --no-typescript --out-dir server/webui --out-name filebrowser_web target/wasm32-unknown-unknown/release/filebrowser_web.wasm
if command -v wasm-opt >/dev/null; then wasm-opt -Oz -o server/webui/filebrowser_web_bg.wasm server/webui/filebrowser_web_bg.wasm; fi
ls -la server/webui
echo "Ahora: cargo build --release -p filebrowser-server"
