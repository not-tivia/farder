#!/bin/bash
# Real-world tests that need an UBUNTU (Linux) Farder server plus the two Windows test clients.
# Run from WSL, after scripts\selftest\build-windows.ps1 has built the Windows copy:
#
#   bash scripts/selftest/ubuntu-suite.sh [restore | relay | recovery | all]
#
#   restore   two clients chat on a Linux server; it is backed up, wiped and restored; they
#             reconnect and find their history and (for A) ownership; then the server is
#             switched off and the app must stay usable (banner, other servers, add-server)
#   relay     a Linux server reached THROUGH THE RELAY (no port forwarding): the owner claims it
#             with its setup link, invites the second client, they chat
#   recovery  the recovery owner: make the key, a member takes the server after the waiting
#             period, the old owner (still connected) is refused. Server started with a SHORT
#             --recovery-wait-secs (30)
#
# Needs: cargo (to build the server), powershell.exe, the Windows copy at $WIN_COPY.
# Uses only its own scratch folder and the processes it starts; stops with `pkill -x farder-server`
# (so do not run it on a machine where you run another farder-server you care about).
set -u
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WIN_COPY="${WIN_COPY:-/mnt/c/Users/Deez/farder-selftest}"
WIN_PATH="${WIN_PATH:-C:\\Users\\Deez\\farder-selftest}"
DATA="${WIN_DATA:-C:\\Users\\Deez\\farder-selftest-data}"
TMP="${SELFTEST_TMP:-/tmp/farder-selftest-ubuntu}"
BIN="$ROOT/target/debug/farder-server"
RELAY="${RELAY_ADDR:-45.61.136.29:4433}"
WSLIP=$(hostname -I | awk '{print $1}')
PS="powershell.exe -NoProfile -ExecutionPolicy Bypass -File"

winrun() { # winrun <scenario>   (UBU_* come from the environment)
  powershell.exe -NoProfile -Command "Set-Location $WIN_PATH; \$env:SHOTS='$DATA\\shots-ubuntu'; \$env:STATE='$DATA\\state.json'; \$env:RESULTS='$DATA\\r.json'; \$env:UBU_ADDR='${UBU_ADDR:-}'; \$env:UBU_TOKEN='${UBU_TOKEN:-}'; \$env:UBU_LINK='${UBU_LINK:-}'; \$env:UBU_NAME='${UBU_NAME:-}'; \$env:GUEST_PHASE='${GUEST_PHASE:-make}'; \$env:GUEST_ROUTE='${GUEST_ROUTE:-ubuntu}'; \$env:GUEST_LINK_OUT='${GUEST_LINK_OUT:-}'; \$env:RESET_PASS='${RESET_PASS:-1}'; node scripts\\selftest\\driver.mjs $1 2>&1" | cut -c1-300
}
fresh_pair() {
  $PS "$WIN_PATH\\scripts\\selftest\\stop-pair.ps1" >/dev/null
  $PS "$WIN_PATH\\scripts\\selftest\\start-pair.ps1" -Fresh >/dev/null 2>&1
  UBU_ADDR= UBU_TOKEN= UBU_LINK= UBU_NAME= winrun core | tail -1
}
wait_for() { for _ in $(seq 1 90); do grep -q "$2" "$1" 2>/dev/null && return 0; sleep 1; done; return 1; }
token_of() { grep -o "token instead: [0-9a-f]*" "$1" | awk '{print $3}'; }

start_direct() { # start_direct <port> <dir> [extra args]
  local port=$1 dir=$2; shift 2
  setsid nohup "$BIN" --bind 0.0.0.0:$port --name "Ubuntu Server" --db "$dir/u.db" --storage-dir "$dir/files" --data-dir "$dir/sd" "$@" > "$dir/server.log" 2>&1 &
  wait_for "$dir/server.log" "Server listening"; sleep 1
}

suite_restore() {
  local D="$TMP/restore" PORT=4571; rm -rf "$D"; mkdir -p "$D"; pkill -x farder-server
  fresh_pair
  start_direct $PORT "$D" --backup-dir "$D/auto"
  export UBU_ADDR="$WSLIP:$PORT" UBU_TOKEN="$(token_of "$D/server.log")" UBU_LINK= UBU_NAME="Ubuntu Server"
  echo "== claim, invite, chat"; winrun ubuntu
  echo "== manual backup while running"
  local BK; BK=$("$BIN" --backup-now "$D/manual" --db "$D/u.db" --storage-dir "$D/files" --data-dir "$D/sd" 2>/dev/null | tail -1); echo "backup: $(basename "$BK")"
  echo "== NUKE: stop, destroy the data, restore"; pkill -x farder-server; sleep 2; rm -rf "$D/u.db" "$D/u.db-wal" "$D/u.db-shm" "$D/files" "$D/sd"
  "$BIN" --restore-from "$BK" --db "$D/u.db" --storage-dir "$D/files" --data-dir "$D/sd" 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | tail -1 | cut -c1-160
  start_direct $PORT "$D" --backup-dir "$D/auto"
  echo "== clients after the restore"; winrun ubuntu-restored
  python3 -c "
import sqlite3; c=sqlite3.connect('$D/u.db')
print('server database: members', c.execute('select count(*) from members').fetchone()[0], 'messages', c.execute('select count(*) from messages').fetchone()[0])"
  echo "== server switched off: the app must stay usable"; pkill -x farder-server; sleep 1
  winrun server-down
  pkill -x farder-server
}

suite_relay() {
  local D="$TMP/relay"; rm -rf "$D"; mkdir -p "$D"; pkill -x farder-server
  fresh_pair
  setsid nohup "$BIN" --relay "$RELAY" --name "Ubuntu Relay" --db "$D/u.db" --storage-dir "$D/files" --data-dir "$D/sd" > "$D/server.log" 2>&1 &
  wait_for "$D/server.log" "registered with relay"; sleep 1
  local SID; SID=$(xxd -p -c 64 "$D/sd/server_id")
  export UBU_ADDR= UBU_TOKEN= UBU_NAME="Ubuntu Relay" UBU_LINK="farder://relayd/$SID/setup:$(token_of "$D/server.log")"
  echo "== claim through the relay, invite, chat"; winrun ubuntu
  pkill -x farder-server
}

suite_recovery() {
  local D="$TMP/recovery" PORT=4572; rm -rf "$D"; mkdir -p "$D"; pkill -x farder-server
  fresh_pair
  start_direct $PORT "$D" --recovery-wait-secs 30
  export UBU_ADDR="$WSLIP:$PORT" UBU_TOKEN="$(token_of "$D/server.log")" UBU_LINK= UBU_NAME="Ubuntu Server"
  echo "== claim, invite, chat"; winrun ubuntu
  echo "== recovery owner"; winrun recovery
  python3 -c "
import sqlite3; c=sqlite3.connect('$D/u.db')
print('audit log:', [r[0] for r in c.execute(\"select action from audit_events where action like 'recovery_%' order by id\")])"
  pkill -x farder-server
}

suite_reset() {
  local D="$TMP/reset" PORT=4573; rm -rf "$D"; mkdir -p "$D"; pkill -x farder-server
  fresh_pair
  start_direct $PORT "$D"
  export UBU_ADDR="$WSLIP:$PORT" UBU_TOKEN="$(token_of "$D/server.log")" UBU_LINK= UBU_NAME="Ubuntu Server"
  echo "== claim, invite, chat"; winrun ubuntu
  pkill -x farder-server; sleep 2
  # B's key, as the host would read it from the members list (B joined second).
  local BKEY; BKEY=$(python3 -c "
import sqlite3; c=sqlite3.connect('$D/u.db')
print('vk_'+c.execute('select hex(public_key) from members order by joined_at desc, rowid desc limit 1').fetchone()[0].lower())")
  echo "== host resets the owner to B ($BKEY)"
  "$BIN" --reset-owner "$BKEY" --db "$D/u.db" --storage-dir "$D/files" --data-dir "$D/sd" 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | cut -c1-200 | tail -4
  start_direct $PORT "$D"
  echo "== after the reset"; RESET_PASS=1 winrun host-reset
  echo "== restart the server: same answer"; pkill -x farder-server; sleep 2; start_direct $PORT "$D"
  RESET_PASS=2 winrun host-reset
  pkill -x farder-server
}

suite_guestlink() {
  local D="$TMP/guestlink" PORT=4574; rm -rf "$D"; mkdir -p "$D"; pkill -x farder-server; pkill -x farder-gateway
  fresh_pair
  start_direct $PORT "$D"
  export UBU_ADDR="$WSLIP:$PORT" UBU_TOKEN="$(token_of "$D/server.log")" UBU_LINK= UBU_NAME="Ubuntu Server"
  echo "== claim, invite, chat"; winrun ubuntu
  setsid nohup "$ROOT/target/debug/farder-gateway" --listen 127.0.0.1:8480 --route ubuntu=127.0.0.1:$PORT > "$D/gateway.log" 2>&1 &
  ( cd "$ROOT/website" && exec setsid python3 -m http.server 8099 --bind 127.0.0.1 > "$D/static.log" 2>&1 ) &
  local STATIC_PID=$!
  cp -r "$ROOT/website/guest" "$WIN_COPY/website/" 2>/dev/null
  cp "$ROOT/scripts/selftest/guest-browser.mjs" "$WIN_COPY/scripts/selftest/"
  sleep 2
  echo "== the owner makes a guest link in the app"
  export GUEST_LINK_OUT="$DATA\\guest-link.txt"; rm -f "$WIN_COPY/../farder-selftest-data/guest-link.txt"
  GUEST_PHASE=make GUEST_ROUTE=ubuntu winrun guest-link
  local LINK; LINK=$(tr -d '\r\n' < /mnt/c/Users/Deez/farder-selftest-data/guest-link.txt)
  echo "link: $LINK"
  echo "== a real browser joins with that link"
  powershell.exe -NoProfile -Command "Set-Location $WIN_PATH; \$env:SHOTS='$DATA\\shots-guest'; \$env:GUEST_SERVER_NAME='Ubuntu Server'; \$env:GUEST_WELCOME='hello from A to ubuntu'; node scripts\\selftest\\guest-browser.mjs '$LINK' 2>&1" | tr -d '\r' | cut -c1-240
  echo "== the app sees the browser guest"
  GUEST_PHASE=seen winrun guest-link
  python3 -c "
import sqlite3; c=sqlite3.connect('$D/u.db')
print('server: guest members:', c.execute('select display_name from members where guest_expires_at is not null').fetchall())"
  pkill -x farder-server; pkill -x farder-gateway; kill $STATIC_PID 2>/dev/null
}

case "${1:-all}" in
  guestlink) suite_guestlink ;;
  reset) suite_reset ;;
  restore) suite_restore ;;
  relay) suite_relay ;;
  recovery) suite_recovery ;;
  all) suite_restore; suite_relay; suite_recovery; suite_reset ;;
  *) echo "usage: $0 [restore|relay|recovery|reset|guestlink|all]"; exit 2 ;;
esac
echo FINISHED
