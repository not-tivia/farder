#!/usr/bin/env bash
# Does the shared (non-desktop) Rust code still compile for Android and iOS?
#   bash scripts/portability_check.sh
# Pure-Rust crates must pass. Crates that bundle C code (SQLite) need the platform
# toolchain (Android NDK / Xcode); without it they fail in the C build step, which
# is reported as "needs toolchain", not as a code failure.
set -u
cd "$(dirname "$0")/.."
rustup target add aarch64-linux-android aarch64-apple-ios >/dev/null 2>&1
bad=0
for t in aarch64-linux-android aarch64-apple-ios; do
  for c in farder-protocol farder-crypto farder-history farder-e2ee-client farder-mls; do
    out=$(cargo check -p "$c" --target "$t" 2>&1)
    if echo "$out" | grep -q "^error: failed to run custom build command"; then
      echo "NEEDS-TOOLCHAIN  $t  $c  (C build step; install the NDK/Xcode to check)"
    elif echo "$out" | grep -q "^error"; then
      echo "FAIL             $t  $c"; echo "$out" | grep -E "^error" | head -3; bad=1
    else
      echo "ok               $t  $c"
    fi
  done
done
exit $bad
