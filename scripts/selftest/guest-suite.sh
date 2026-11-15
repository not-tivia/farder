#!/bin/bash
# The browser guest page in a real Edge browser against a real server through the gateway.
# Run from WSL after scripts/build_guest_web.sh and `cargo build -p farder-server`, with the Windows
# copy updated (it needs scripts/selftest/node_modules/playwright-core and Edge).
#   bash scripts/selftest/guest-suite.sh
# Uses ports 8480 (gateway) and 8099 (static page server); stops what it starts.
set -u
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WIN_COPY="${WIN_COPY:-/mnt/c/Users/Deez/farder-selftest}"
WIN_PATH="${WIN_PATH:-C:\\Users\\Deez\\farder-selftest}"
DATA="${WIN_DATA:-C:\\Users\\Deez\\farder-selftest-data}"
TMP="${SELFTEST_TMP:-/tmp/farder-selftest-ubuntu}"; mkdir -p "$TMP"
cd "$ROOT"
cargo build -q -p farder-server -p farder-gateway --example guest_fixture || exit 1
cp -r website/guest "$WIN_COPY/website/" 2>/dev/null
cp scripts/selftest/guest-browser.mjs "$WIN_COPY/scripts/selftest/"
pkill -x guest_fixture; pkill -x farder-server
( cd website && exec setsid python3 -m http.server 8099 --bind 127.0.0.1 > "$TMP/static.log" 2>&1 ) &
STATIC_PID=$!
setsid nohup target/debug/examples/guest_fixture > "$TMP/fixture.log" 2>&1 &
for _ in $(seq 1 60); do grep -q GUEST_READY "$TMP/fixture.log" 2>/dev/null && break; sleep 1; done
CODE=$(grep -o "code=[A-Za-z0-9]*" "$TMP/fixture.log" | cut -d= -f2)
[ -n "$CODE" ] || { echo "fixture did not start"; cat "$TMP/fixture.log"; exit 1; }
powershell.exe -NoProfile -Command "Set-Location $WIN_PATH; \$env:SHOTS='$DATA\\shots-guest'; node scripts\\selftest\\guest-browser.mjs $CODE 2>&1" | tr -d '\r' | cut -c1-260
python3 - <<P
import sqlite3
c=sqlite3.connect("/tmp/farder-guest-fixture/s.db")
print("server: guests marked:", c.execute("select count(*) from members where guest_expires_at is not null").fetchone()[0],
      "| messages:", [r[0] for r in c.execute("select content from messages order by id")])
P
pkill -x guest_fixture; pkill -x farder-server; kill $STATIC_PID 2>/dev/null
echo FINISHED
