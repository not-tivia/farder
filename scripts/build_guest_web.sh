#!/bin/bash
# Build the browser guest page's WebAssembly package into website/guest/pkg (not committed: it is
# a build output). One-time setup:
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version 0.2.117 --locked     # must match the wasm-bindgen in Cargo.lock
set -e
cd "$(dirname "$0")/.."
cargo build -p farder-web --target wasm32-unknown-unknown --release
mkdir -p website/guest/pkg
wasm-bindgen --target web --out-dir website/guest/pkg --out-name farder_web target/wasm32-unknown-unknown/release/farder_web.wasm
ls -la website/guest/pkg
