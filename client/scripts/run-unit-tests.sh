#!/usr/bin/env bash
# Pure-logic unit tests for the frontend (there is no JS test framework; these use
# node's built-in runner). Run from anywhere:  bash client/scripts/run-unit-tests.sh
set -euo pipefail
cd "$(dirname "$0")/.."
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
npx esbuild src/lib/garble.ts --bundle --platform=node --format=esm --outfile="$out/garble.mjs" --log-level=error
GARBLE_JS="$out/garble.mjs" node --test tests/*.test.mjs
