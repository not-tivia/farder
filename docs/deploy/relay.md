# Deploying a Farder Relay

A Farder relay is a lightweight hop server that sits between two Farder clients. When two users communicate through a relay, neither side learns the other's real IP address — the relay sees only its own peers, not the parties behind them. Running a relay is genuinely useful, but it comes with real responsibilities: you are carrying other people's (encrypted) traffic, you pay the hosting bill (~$5/month for a minimal VPS), and you are responsible for keeping the host secure and the process running. Go in with eyes open.

---

## What you need

- A small VPS: 1 vCPU and 512 MB–1 GB RAM is more than enough. Any cloud provider (Hetzner, DigitalOcean, Vultr, Linode, etc.) works.
- The ability to open a **UDP** port (4433) — check that your provider allows UDP in its firewall/security-group rules; some restrict it.
- Optionally: a domain name so you can give users a stable hostname instead of a raw IP.

---

## 1. Open the firewall

You need to open UDP 4433 in **two** places:

**1a. VPS provider security group / firewall**
Log into your provider's dashboard and add an inbound rule: protocol UDP, port 4433, source 0.0.0.0/0 (any). Exact steps vary by provider; look for "Firewall", "Security Groups", or "Network" in the dashboard.

**1b. Host firewall (ufw)**
If your VPS is running Ubuntu/Debian with ufw enabled:

```bash
sudo ufw allow 4433/udp
sudo ufw status   # confirm the rule appears
```

If you use firewalld (CentOS/Rocky):

```bash
sudo firewall-cmd --permanent --add-port=4433/udp
sudo firewall-cmd --reload
```

If ufw/firewalld is not installed or not active, skip this step — but verify there is no other host-level iptables rule blocking UDP 4433.

---

## 2. Run the relay — Docker (recommended)

**Install Docker** (if not already installed):

```bash
curl -fsSL https://get.docker.com | sh
sudo usermod -aG docker $USER   # then log out and back in
```

**Clone the Farder repo on the host:**

```bash
git clone https://github.com/your-org/farder.git
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
```

If the container exited, the logs will show the error. The most common cause is the port already being in use or a permission problem with the data volume.

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
```

---

## 3. (Optional) Point a domain at it

If you want users (and invite links) to reference a hostname instead of a raw IP:

1. In your DNS provider's dashboard, add an **A record** (IPv4) or **AAAA record** (IPv6) pointing your chosen subdomain to the VPS's public IP. For example:

   | Type | Name             | Value          |
   |------|------------------|----------------|
   | A    | relay.farder.gg  | 203.0.113.42   |

2. DNS propagation typically takes a few minutes to an hour. You can check it with:

   ```bash
   dig +short relay.farder.gg
   ```

3. Your relay address is now `relay.farder.gg:4433` instead of `203.0.113.42:4433`.

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

---

## 5. Tell the client about the default relay

Edit `client/src-tauri/src/default_relay.rs` and fill in your relay's address and the fingerprint you copied in step 4:

```rust
pub const DEFAULT_RELAY: Option<DefaultRelay> = Some(DefaultRelay {
    addr: "relay.farder.gg:4433",       // your relay's address (or raw IP:port)
    cert_fp_hex: "<the 64 hex chars>",  // from step 4
});
```

Then rebuild the client:

```bash
cd client && npm run tauri build
```

**Note:** wiring the default relay into server-creation flows and invite links is a later feature. This step just records the value so the client binary carries it — nothing will actively use it until that feature lands.

---

## Operating it

### Updating the relay

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

2. **Per-IP new-connection rate limit** — hardcoded to 30 new connections per IP per 60-second window. If you need to change these values, edit the constants in `crates/farder-relay/src/main.rs` (the two arguments after `config.max_connections as usize` in the `ConnectionLimiter::new(...)` call) and rebuild.

### Back up the data directory

**This is important.** The file `relay_cert.der` is the relay's identity. If it is deleted or the relay regenerates it (e.g. because the volume was wiped), the fingerprint changes. Any client binary built with the old fingerprint will refuse to connect, and any existing invite links that embed the fingerprint become invalid.

Back up your data directory before doing anything destructive to the host:

```bash
# Docker — export the named volume
docker run --rm -v relay-data:/data -v $(pwd):/backup debian \
  tar czf /backup/relay-data-backup.tar.gz -C /data .

# systemd — plain copy
sudo tar czf relay-data-backup.tar.gz /var/lib/farder-relay
```

Store the backup somewhere off the VPS (object storage, your local machine, etc.).
