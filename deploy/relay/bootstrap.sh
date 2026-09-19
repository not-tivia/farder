#!/usr/bin/env bash
# One-shot Farder relay bring-up for a fresh Debian/Ubuntu VPS.
#
#   curl -fsSL https://raw.githubusercontent.com/not-tivia/farder/main/deploy/relay/bootstrap.sh | bash
#
# or, if the repo is already cloned:  bash deploy/relay/bootstrap.sh
#
# It installs Docker if missing, opens UDP 4433 + TCP 8080, builds and starts
# the relay, backs up the relay's identity (its cert), and prints the exact
# default_relay.rs snippet the client needs. Safe to re-run.
#
# Overrides: FARDER_DIR (clone path), FARDER_REPO (git url), FARDER_PUBLIC_IP,
# FARDER_RELAY_BACKUP (path to a relay-data tarball to restore, so the new relay
# keeps its old identity/fingerprint and no client rebuild is needed),
# FARDER_SITE_DOMAIN (also serve the invite website for that domain over HTTPS;
# DNS for it must already point at this box, or the certificate cannot be
# issued).
set -euo pipefail

FARDER_REPO="${FARDER_REPO:-https://github.com/not-tivia/farder.git}"
FARDER_DIR="${FARDER_DIR:-$HOME/farder}"
COMPOSE=(docker compose -f deploy/relay/docker-compose.yml)

say() { printf '\n== %s\n' "$*"; }
die() { printf '\nERROR: %s\n' "$*" >&2; exit 1; }

if [ "$(id -u)" -eq 0 ]; then SUDO=""; else
  command -v sudo >/dev/null 2>&1 || die "run as root, or install sudo"
  SUDO="sudo"
fi

say "Installing prerequisites"
if command -v apt-get >/dev/null 2>&1; then
  $SUDO apt-get update -qq
  $SUDO apt-get install -y -qq git curl ca-certificates
else
  # Non-Debian host: just require the handful of tools we actually use.
  for t in git curl; do command -v "$t" >/dev/null 2>&1 || die "install $t first"; done
fi

if ! command -v docker >/dev/null 2>&1; then
  say "Installing Docker"
  curl -fsSL https://get.docker.com | $SUDO sh
  [ -n "$SUDO" ] && $SUDO usermod -aG docker "$USER" || true
fi
$SUDO docker compose version >/dev/null 2>&1 || die "docker compose plugin missing (install docker-compose-plugin)"

say "Opening the firewall (UDP 4433 = QUIC, TCP 8080 = incoming webhooks)"
if [ -n "${FARDER_SITE_DOMAIN:-}" ]; then
  echo "Also opening TCP 80 + 443 for $FARDER_SITE_DOMAIN (certificate issuance needs 80)."
fi
if command -v ufw >/dev/null 2>&1 && $SUDO ufw status 2>/dev/null | grep -q "Status: active"; then
  $SUDO ufw allow 4433/udp
  $SUDO ufw allow 8080/tcp
  if [ -n "${FARDER_SITE_DOMAIN:-}" ]; then
    $SUDO ufw allow 80/tcp
    $SUDO ufw allow 443/tcp
  fi
elif command -v firewall-cmd >/dev/null 2>&1; then
  $SUDO firewall-cmd --permanent --add-port=4433/udp
  $SUDO firewall-cmd --permanent --add-port=8080/tcp
  if [ -n "${FARDER_SITE_DOMAIN:-}" ]; then
    $SUDO firewall-cmd --permanent --add-port=80/tcp
    $SUDO firewall-cmd --permanent --add-port=443/tcp
  fi
  $SUDO firewall-cmd --reload
else
  echo "No active host firewall found - nothing to open locally."
fi
echo "REMINDER: your provider's own firewall / security group must allow the same two ports."

say "Fetching the relay source"
if [ -d "$FARDER_DIR/.git" ]; then
  git -C "$FARDER_DIR" pull --ff-only
else
  git clone --depth 1 "$FARDER_REPO" "$FARDER_DIR"
fi
cd "$FARDER_DIR"

say "Building and starting the relay (first build takes a few minutes)"
if [ -n "${FARDER_SITE_DOMAIN:-}" ]; then
  # The `web` profile adds Caddy serving the invite site. Exported, not passed
  # inline, because compose reads it for the container's environment too.
  export FARDER_SITE_DOMAIN
  $SUDO -E "${COMPOSE[@]}" --profile web up -d --build
else
  $SUDO "${COMPOSE[@]}" up -d --build
fi

# Compose names the volume <project>_relay-data, so ask the container rather
# than guessing: a wrong name here silently backs up an empty new volume.
CID="$($SUDO "${COMPOSE[@]}" ps -q relay)"
[ -n "$CID" ] || die "the relay container did not start - check: ${COMPOSE[*]} logs"
VOL="$($SUDO docker inspect -f '{{range .Mounts}}{{if eq .Destination "/data"}}{{.Name}}{{end}}{{end}}' "$CID")"
[ -n "$VOL" ] || die "could not find the relay's /data volume"

if [ -n "${FARDER_RELAY_BACKUP:-}" ]; then
  say "Restoring the relay identity from $FARDER_RELAY_BACKUP"
  [ -f "$FARDER_RELAY_BACKUP" ] || die "no such backup: $FARDER_RELAY_BACKUP"
  BDIR="$(cd "$(dirname "$FARDER_RELAY_BACKUP")" && pwd)"
  $SUDO "${COMPOSE[@]}" stop relay >/dev/null
  $SUDO docker run --rm -v "$VOL:/data" -v "$BDIR:/restore:ro" debian:stable-slim \
    tar xzf "/restore/$(basename "$FARDER_RELAY_BACKUP")" -C /data
  $SUDO "${COMPOSE[@]}" start relay >/dev/null
  echo "Restored - this relay keeps its previous fingerprint, so clients need no rebuild."
fi

say "Waiting for the relay to come up"
for _ in $(seq 1 30); do
  if $SUDO "${COMPOSE[@]}" exec -T relay test -f /data/relay_cert.der 2>/dev/null; then break; fi
  sleep 2
done
$SUDO "${COMPOSE[@]}" exec -T relay test -f /data/relay_cert.der 2>/dev/null \
  || die "the relay never wrote its cert - check: ${COMPOSE[*]} logs"

FP="$($SUDO "${COMPOSE[@]}" exec -T relay sha256sum /data/relay_cert.der | awk '{print $1}')"
[ "${#FP}" -eq 64 ] || die "unexpected fingerprint: $FP"

say "Backing up the relay identity"
# The cert IS the relay's identity: lose it and every client pinned to this
# fingerprint has to be rebuilt. Keep this tarball OFF the VPS.
BACKUP="$HOME/relay-data-backup-$(date +%Y%m%d).tar.gz"
$SUDO docker run --rm -v "$VOL:/data" -v "$HOME:/backup" debian:stable-slim \
  tar czf "/backup/$(basename "$BACKUP")" -C /data . >/dev/null
$SUDO chown "$(id -u):$(id -g)" "$BACKUP" 2>/dev/null || true
echo "Wrote $BACKUP"

IP="${FARDER_PUBLIC_IP:-}"
if [ -z "$IP" ]; then
  IP="$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="src") print $(i+1)}')"
  case "$IP" in
    10.*|192.168.*|172.1[6-9].*|172.2[0-9].*|172.3[01].*|"")
      IP="$(curl -fsS --max-time 5 https://api.ipify.org || echo "")" ;;
  esac
fi
[ -n "$IP" ] || IP="<this-vps-public-ip>"

cat <<SNIP

== The relay is up.

Address:      $IP:4433  (QUIC)
Webhooks:     http://$IP:8080/webhook/<server_id>/<token>
Fingerprint:  $FP
Backup:       $BACKUP   <- copy this off the VPS

Put this in client/src-tauri/src/default_relay.rs, then rebuild the client:

pub const DEFAULT_RELAY: Option<DefaultRelay> = Some(DefaultRelay {
    addr: "$IP:4433",
    cert_fp_hex: "$FP",
});

Logs:  ${COMPOSE[*]} logs -f
SNIP

if [ -n "${FARDER_SITE_DOMAIN:-}" ]; then
  cat <<SITE
Website:      https://$FARDER_SITE_DOMAIN  (invite links resolve at /join/<code>)

If the certificate did not issue, DNS for $FARDER_SITE_DOMAIN is not pointing
here yet, or TCP 80 is blocked upstream. Caddy retries by itself once either is
fixed: ${COMPOSE[*]} logs web
SITE
fi
