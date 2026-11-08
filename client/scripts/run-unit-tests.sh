#!/usr/bin/env bash
# Pure-logic unit tests for the frontend (there is no JS test framework; these use
# node's built-in runner). Run from anywhere:  bash client/scripts/run-unit-tests.sh
set -euo pipefail
cd "$(dirname "$0")/.."
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
npx esbuild src/lib/garble.ts --bundle --platform=node --format=esm --outfile="$out/garble.mjs" --log-level=error
npx esbuild src/lib/autoplay.ts --bundle --platform=node --format=esm --outfile="$out/autoplay.mjs" --log-level=error
npx esbuild src/context/MediaPlayersContext.tsx --bundle --platform=node --format=esm --outfile="$out/players.mjs" --log-level=error
npx esbuild src/lib/visibleInterval.ts --bundle --platform=node --format=esm --outfile="$out/visible.mjs" --log-level=error
npx esbuild src/lib/notificationText.ts --bundle --platform=node --format=esm --outfile="$out/notiftext.mjs" --log-level=error
npx esbuild src/lib/audioLimiterWorklet.ts --bundle --platform=node --format=esm --outfile="$out/limiter.mjs" --log-level=error
npx esbuild src/lib/quickReply.ts --bundle --platform=node --format=esm --outfile="$out/quickreply.mjs" --log-level=error
npx esbuild src/lib/density.ts --bundle --platform=node --format=esm --outfile="$out/density.mjs" --log-level=error
DENSITY_JS="$out/density.mjs" QUICKREPLY_JS="$out/quickreply.mjs" LIMITER_JS="$out/limiter.mjs" NOTIFTEXT_JS="$out/notiftext.mjs" GARBLE_JS="$out/garble.mjs" VISIBLE_JS="$out/visible.mjs" AUTOPLAY_JS="$out/autoplay.mjs" PLAYERS_JS="$out/players.mjs" node --test tests/*.test.mjs
