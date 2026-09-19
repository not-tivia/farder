# Deploying a Farder Relay

A Farder relay is a lightweight hop server that sits between two Farder clients. When two users communicate through a relay, neither side learns the other's real IP address — the relay sees only its own peers, not the parties behind them. Running a relay is genuinely useful, but it comes with real responsibilities: you are carrying other people's (encrypted) traffic, you pay the hosting bill (~$5/month for a minimal VPS), and you are responsible for keeping the host secure and the process running. Go in with eyes open.

---

## What you need

- A small VPS: 1 vCPU and 512 MB–1 GB RAM is more than enough. Any cloud provider (Hetzner, DigitalOcean, Vultr, Linode, etc.) works.
- The ability to open a **UDP** port (4433) — check that your provider allows UDP in its firewall/security-group rules; some restrict it.
- A **TCP** port (8080) if you want incoming webhooks to work. Third parties POST to `http://<relay>:8080/webhook/<server_id>/<token>`, and the relay forwards the body to the registered server over its existing QUIC tunnel. Skip it and everything else still works; webhooks just never arrive.
- Optionally: a domain name. Note it is only cosmetic today — see step 3.

---

## 0. The fast path

On a fresh Debian/Ubuntu VPS, `deploy/relay/bootstrap.sh` does steps 1, 2 and 4 in one go — installs Docker, opens both ports, builds and starts the relay, backs up its identity, and prints the `default_relay.rs` snippet from step 5:

```bash
curl -fsSL https://raw.githubusercontent.com/not-tivia/farder/main/deploy/relay/bootstrap.sh | bash
```

It is safe to re-run (it pulls and rebuilds). The rest of this document is the same thing by hand, plus the operating notes.

---

## 1. Open the firewall

Each port has to be opened in **two** places — your provider's firewall and the host's own:

**1a. VPS provider security group / firewall**
Log into your provider's dashboard and add two inbound rules: protocol UDP port 4433 (the relay itself), and protocol TCP port 8080 (webhook ingress — skip it if you do not want webhooks), both with source 0.0.0.0/0 (any). Exact steps vary by provider; look for "Firewall", "Security Groups", or "Network" in the dashboard.

**1b. Host firewall (ufw)**
If your VPS is running Ubuntu/Debian with ufw enabled:

```bash
sudo ufw allow 4433/udp
sudo ufw allow 8080/tcp   # webhook ingress; skip if you do not want webhooks
sudo ufw status           # confirm the rules appear
```

If you use firewalld (CentOS/Rocky):

```bash
sudo firewall-cmd --permanent --add-port=4433/udp
sudo firewall-cmd --permanent --add-port=8080/tcp
sudo firewall-cmd --reload
```

If ufw/firewalld is not installed or not active, skip this step — but verify there is no other host-level iptables rule blocking UDP 4433 or TCP 8080.

---

## 2. Run the relay — Docker (recommended)

**Install Docker** (if not already installed):

```bash
curl -fsSL https://get.docker.com | sh
sudo usermod -aG docker $USER   # then log out and back in
```

**Clone the Farder repo on the host:**

```bash
git clone https://github.com/not-tivia/farder.git
cd farder
```

**Start the relay:**

```bash
docker compose -f deploy/relay/docker-compose.yml up -d --build
```

This builds the relay binary inside Docker (takes a few minutes the first time) and starts it with `restart: unless-stopped` so it comes back after reboots.

**Verify it is running:**

```bash
docker compose -f deploy/relay/docker-compose.yml logs
```

You should see a line like:

```
relay  | INFO farder_relay: Relay listening on 0.0.0.0:4433
relay  | INFO farder_relay: webhook HTTP listening on 0.0.0.0:8080
```

Both lines should appear. If the container exited, the logs will show the error. The most common cause is the port already being in use or a permission problem with the data volume.

---

## 2b. Run the relay — systemd (alternative)

Use this if you prefer not to install Docker on the VPS. You will need Rust installed to build the binary.

**Install Rust (if not already installed):**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

**Build the relay binary (from the repo root):**

```bash
cargo build --release -p farder-relay
```

**Install it:**

```bash
sudo cp target/release/farder-relay /usr/local/bin/
sudo useradd -r -s /usr/sbin/nologin relay
sudo mkdir -p /var/lib/farder-relay
sudo chown relay /var/lib/farder-relay
sudo cp deploy/relay/farder-relay.service /etc/systemd/system/
```

**Enable and start:**

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now farder-relay
```

**Verify:**

```bash
systemctl status farder-relay
journalctl -u farder-relay -f
```

The status should show `active (running)` and the journal should contain:

```
INFO farder_relay: Relay listening on 0.0.0.0:4433
INFO farder_relay: webhook HTTP listening on 0.0.0.0:8080
```

---

## 3. (Optional) Point a domain at it

If you want users (and invite links) to reference a hostname instead of a raw IP:

1. In your DNS provider's dashboard, add an **A record** (IPv4) or **AAAA record** (IPv6) pointing your chosen subdomain to the VPS's public IP. For example:

   | Type | Name             | Value          |
   |------|------------------|----------------|
   | A    | relay.farder.xyz  | 203.0.113.42   |

2. DNS propagation typically takes a few minutes to an hour. You can check it with:

   ```bash
   dig +short relay.farder.xyz
   ```

3. Your relay address is now `relay.farder.xyz:4433` instead of `203.0.113.42:4433`.

**Caveat — the hostname is cosmetic today.** The client parses `DEFAULT_RELAY.addr` (and the address inside every relay invite link) as a literal `IP:port`; a hostname there fails to parse and silently leaves the build with *no* default relay. So a DNS record is useful for documentation and for humans reading a link, but step 5 still takes the raw IP. Teaching the client to resolve hostnames is a small change we have not made yet — without it, moving the relay to a new IP means rebuilding the client even if the DNS name stays the same.

Note: Farder relays use QUIC, not TLS over TCP, so you do **not** need a certificate from Let's Encrypt or any CA. The relay generates its own self-signed cert and the client pins it by fingerprint (see step 4).

---

## 4. Read the cert fingerprint

When the relay first starts it generates a self-signed TLS certificate and writes it to `relay_cert.der` in its data directory. The Farder client does not trust any CA for relay connections — it pins this specific cert by the SHA-256 hash of the raw DER bytes. You need to read that hash.

**Docker:**

```bash
docker compose -f deploy/relay/docker-compose.yml exec relay sha256sum /data/relay_cert.der
```

**systemd:**

```bash
sha256sum /var/lib/farder-relay/relay_cert.der
```

Both commands print something like:

```
a3f1b2c4d5e6...0987654321ab  /data/relay_cert.der
```

Copy the first field — the 64 hex characters before the two spaces. That is your cert fingerprint.

**Back it up now, not later.** That cert file is the relay's identity: if you lose it, the fingerprint changes, every client built with the old one refuses to connect, and every invite link carrying the old fingerprint is dead. Run the backup from [Back up the data directory](#back-up-the-data-directory) before you do anything else, and copy the tarball off the VPS.

---

## 5. Tell the client about the default relay

Edit `client/src-tauri/src/default_relay.rs` and fill in your relay's address and the fingerprint you copied in step 4:

```rust
pub const DEFAULT_RELAY: Option<DefaultRelay> = Some(DefaultRelay {
    addr: "203.0.113.42:4433",          // raw IP:port - NOT a hostname (see step 3)
    cert_fp_hex: "<the 64 hex chars>",  // from step 4
});
```

Then rebuild the client:

```bash
cd client && npm run tauri build
```

This one constant is the single source of truth for the relay in the client: relayed server creation, compact `farder://relayd/...` invite links, invite previews, link embeds, proxied media, and the webhook ingest URL shown in Channel Settings all read from it. Nothing else needs editing when the relay moves — but everyone does need a rebuilt client, because the pinned fingerprint lives in the binary.

---

## Operating it

### Updating the relay

> **Important (invite previews):** the relay now serves as a privacy fetch
> proxy for invite previews. Older relay binaries (pre-invite-preview release)
> do not speak the `ProxyInvitePreview` protocol. Clients connected to an
> old relay will see "Preview unavailable" on invite links instead of the
> server name and member count. Redeploy with `git pull && compose up --build`
> to enable previews.

**Docker:**

```bash
cd farder
git pull
docker compose -f deploy/relay/docker-compose.yml up -d --build
```

The compose file uses `restart: unless-stopped`, so the container restarts automatically on reboot without any extra steps.

**systemd:**

```bash
cd farder
git pull
cargo build --release -p farder-relay
sudo cp target/release/farder-relay /usr/local/bin/farder-relay
sudo systemctl restart farder-relay
```

### Viewing logs

```bash
# Docker
docker compose -f deploy/relay/docker-compose.yml logs -f

# systemd
journalctl -u farder-relay -f
```

### Abuse controls

The relay has two built-in abuse controls:

1. **Global concurrent-connection cap** — set with `--max-connections <N>` (default: 1024). To lower it, edit the `ENTRYPOINT` line in `deploy/relay/Dockerfile` (Docker) or the `ExecStart` line in `deploy/relay/farder-relay.service` (systemd), then redeploy/restart.

2. **Per-IP new-connection rate limit** — hardcoded to 300 new connections per IP per 60-second window (one IP legitimately carries a lot: a NAT shares one address across many users, and a host runs both its client and its local server from the same IP). Webhook POSTs use a separate 60-per-IP-per-minute budget. If you need to change these values, edit the constants in `crates/farder-relay/src/main.rs` (the two arguments after `config.max_connections as usize` in the `ConnectionLimiter::new(...)` call) and rebuild.

### Back up the data directory

**This is important, and it is the step we got wrong once already.** `relay_cert.der` plus `relay_key.der` are the relay's identity. If they are deleted or regenerated (e.g. the volume was wiped, or the VPS went away), the fingerprint changes: every client binary built with the old fingerprint refuses to connect, every invite link carrying the old fingerprint is dead, and the only way back is to rebuild and redistribute the client.

Back it up the moment the relay first starts, and again before anything destructive to the host.

```bash
# Docker -- ask the container for its volume name first.
# Compose names the volume <project>_relay-data (e.g. farder_relay-data), NOT
# "relay-data": passing the bare name just creates a new EMPTY volume and your
# backup silently contains nothing.
CID=$(docker compose -f deploy/relay/docker-compose.yml ps -q relay)
VOL=$(docker inspect -f '{{range .Mounts}}{{if eq .Destination "/data"}}{{.Name}}{{end}}{{end}}' "$CID")
docker run --rm -v "$VOL:/data" -v "$PWD:/backup" debian:stable-slim \
  tar czf /backup/relay-data-backup.tar.gz -C /data .

# systemd -- plain copy
sudo tar czf relay-data-backup.tar.gz /var/lib/farder-relay
```

Verify the tarball is not empty (`tar tzf relay-data-backup.tar.gz` should list `./relay_cert.der` and `./relay_key.der`) and store it somewhere off the VPS.

### Restoring onto a new VPS

A restored relay keeps its old fingerprint, so **no client rebuild is needed** — this is the whole point of keeping the backup.

With the bootstrap script, copy the tarball to the new box and point the script at it:

```bash
FARDER_RELAY_BACKUP=~/relay-data-backup.tar.gz bash deploy/relay/bootstrap.sh
```

By hand (Docker), after `compose up -d --build` has run once:

```bash
CID=$(docker compose -f deploy/relay/docker-compose.yml ps -q relay)
VOL=$(docker inspect -f '{{range .Mounts}}{{if eq .Destination "/data"}}{{.Name}}{{end}}{{end}}' "$CID")
docker compose -f deploy/relay/docker-compose.yml stop relay
docker run --rm -v "$VOL:/data" -v "$PWD:/restore:ro" debian:stable-slim \
  tar xzf /restore/relay-data-backup.tar.gz -C /data
docker compose -f deploy/relay/docker-compose.yml start relay
```

Then confirm the fingerprint matches the one your clients are built with (step 4). If the IP changed but the fingerprint did not, existing compact `farder://relayd/...` invite links keep working once clients get a build carrying the new address — the long-form links, which embed the old address, do not.
