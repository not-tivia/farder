#!/bin/bash
# Copy the farder-server binary into the sidecar directory with the correct target triple name.
# Run from the repo root after building the server: cargo build -p farder-server
TARGET_TRIPLE=$(rustc -vV | grep host | cut -d' ' -f2)
cp target/debug/farder-server "client/src-tauri/binaries/farder-server-${TARGET_TRIPLE}"
echo "Copied sidecar binary for ${TARGET_TRIPLE}"
