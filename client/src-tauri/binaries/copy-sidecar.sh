#!/bin/bash
# Copy the farder-server binary for Tauri sidecar resolution.
# Run from the repo root after building the server: cargo build -p farder-server
TARGET_TRIPLE=$(rustc -vV | grep host | cut -d' ' -f2)
SIDECAR_NAME="farder-server-${TARGET_TRIPLE}"

# Copy to binaries/ (for tauri build)
cp target/debug/farder-server "client/src-tauri/binaries/${SIDECAR_NAME}"

# Copy next to the dev executable (for tauri dev)
mkdir -p client/src-tauri/target/debug
cp target/debug/farder-server "client/src-tauri/target/debug/${SIDECAR_NAME}"

echo "Copied sidecar binary for ${TARGET_TRIPLE}"
