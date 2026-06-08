# Default Relay — Deploy-Readiness — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the default relay deploy-ready — add abuse controls + transport hardening to the relay, ship deployment artifacts + a guide, and scaffold the client's default-relay config — without provisioning any infrastructure.

**Architecture:** A `ConnectionLimiter` (global cap + per-IP rate limit, injected-clock testable) gates the relay's accept loop; an explicit idle timeout + coordinated keep-alive reap dead connections without dropping idle-but-live registered servers. A Dockerfile/compose/systemd unit + a deploy guide make the deploy turn-key. An empty `default_relay.rs` is the post-deploy config target.

**Tech Stack:** Rust (quinn 0.11, std), Docker, systemd.

**Spec:** `docs/superpowers/specs/2026-06-07-default-relay-deploy-readiness-design.md`

**Scope boundary:** Actual provisioning (VPS/DNS/running the host) is OUT OF SCOPE — it's the user's guided hands-on step (the deploy guide). Wiring `default_relay` into consumers is a future phase.

---

## File Structure

- `crates/farder-relay/src/limits.rs` *(new)* — `ConnectionLimiter` + `ConnectionGuard` + tests.
- `crates/farder-relay/src/router.rs` — `serve` admits via the limiter; refuses over-limit.
- `crates/farder-relay/src/listener.rs` — explicit idle timeout on the endpoint.
- `crates/farder-relay/src/main.rs` — `mod limits;`, build + pass the limiter.
- `crates/farder-server/src/relay.rs` — keep-alive on the server's relay client endpoint.
- `deploy/relay/Dockerfile`, `deploy/relay/docker-compose.yml`, `deploy/relay/farder-relay.service`, `.dockerignore` *(new)*.
- `docs/deploy/relay.md` *(new)* — the deploy guide.
- `client/src-tauri/src/default_relay.rs` *(new)* + `mod default_relay;` in the client `main.rs`.

---

## Task 1: `ConnectionLimiter` (abuse-control logic)

**Files:** Create `crates/farder-relay/src/limits.rs`.

- [ ] **Step 1: Write the module with failing tests.** Create `crates/farder-relay/src/limits.rs`:

```rust
//! Abuse controls for the relay: a global concurrent-connection cap and a
//! per-IP new-connection rate limit, so the public default relay can sit on the
//! internet without one source flooding it. The clock is injected (`now`) so the
//! logic is deterministically testable.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Held for a connection's lifetime; decrements the active count on drop.
pub struct ConnectionGuard {
    active: Arc<AtomicUsize>,
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct ConnectionLimiter {
    max_connections: usize,
    rate_per_window: usize,
    window: Duration,
    active: Arc<AtomicUsize>,
    per_ip: Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
}

impl ConnectionLimiter {
    pub fn new(max_connections: usize, rate_per_window: usize, window: Duration) -> Self {
        Self {
            max_connections,
            rate_per_window,
            window,
            active: Arc::new(AtomicUsize::new(0)),
            per_ip: Mutex::new(HashMap::new()),
        }
    }

    /// Try to admit a new connection from `ip` at `now`. Returns a guard (held for
    /// the connection's lifetime) if admitted, or `None` if the global cap or the
    /// per-IP rate limit is hit. Checks the cap first so a cap-refused attempt does
    /// not count against the IP's rate.
    pub fn try_admit(&self, ip: IpAddr, now: Instant) -> Option<ConnectionGuard> {
        // Global concurrent cap.
        let prev = self.active.fetch_add(1, Ordering::AcqRel);
        if prev >= self.max_connections {
            self.active.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        // Per-IP sliding-window rate limit.
        let over_rate = {
            let mut map = self.per_ip.lock().unwrap_or_else(|e| e.into_inner());
            let window = self.window;
            // Opportunistic prune: drop IPs whose most recent hit is outside the window.
            map.retain(|_, hits| hits.back().map(|t| now.duration_since(*t) < window).unwrap_or(false));
            let hits = map.entry(ip).or_default();
            while hits.front().map(|t| now.duration_since(*t) >= window).unwrap_or(false) {
                hits.pop_front();
            }
            if hits.len() >= self.rate_per_window {
                true
            } else {
                hits.push_back(now);
                false
            }
        };
        if over_rate {
            self.active.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(ConnectionGuard { active: self.active.clone() })
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, n])
    }

    #[test]
    fn enforces_global_cap_and_guard_frees_a_slot() {
        let lim = ConnectionLimiter::new(2, 1000, Duration::from_secs(60));
        let now = Instant::now();
        let g1 = lim.try_admit(ip(1), now);
        let g2 = lim.try_admit(ip(2), now);
        assert!(g1.is_some() && g2.is_some());
        assert_eq!(lim.active(), 2);
        assert!(lim.try_admit(ip(3), now).is_none(), "over cap is refused");
        assert_eq!(lim.active(), 2, "a refused attempt does not increment active");
        drop(g1);
        assert_eq!(lim.active(), 1);
        assert!(lim.try_admit(ip(3), now).is_some(), "freed slot admits again");
    }

    #[test]
    fn enforces_per_ip_rate_window() {
        let lim = ConnectionLimiter::new(1000, 2, Duration::from_secs(60));
        let t0 = Instant::now();
        let _a = lim.try_admit(ip(1), t0).expect("1st");
        let _b = lim.try_admit(ip(1), t0).expect("2nd");
        assert!(lim.try_admit(ip(1), t0).is_none(), "3rd within window refused");
        // A different IP is independent.
        assert!(lim.try_admit(ip(2), t0).is_some(), "other IP unaffected");
        // After the window, the same IP is admitted again.
        let later = t0 + Duration::from_secs(61);
        assert!(lim.try_admit(ip(1), later).is_some(), "admitted after window");
    }

    #[test]
    fn cap_refusal_does_not_count_against_ip_rate() {
        let lim = ConnectionLimiter::new(1, 5, Duration::from_secs(60));
        let now = Instant::now();
        let _g = lim.try_admit(ip(1), now).expect("1st admitted");
        // Next is refused by the CAP (active=1=max), not the rate.
        assert!(lim.try_admit(ip(1), now).is_none());
        // The IP still has rate budget: free the slot, admit again.
        drop(_g);
        assert!(lim.try_admit(ip(1), now).is_some(), "rate budget not consumed by cap refusal");
    }
}
```

- [ ] **Step 2: Declare the module + run the tests.** In `crates/farder-relay/src/main.rs`, add `mod limits;` to the module list (with `mod config; mod listener; mod router;`). Run: `cd ~/farder && cargo test -p farder-relay limits 2>&1 | tail -12` — expect the 3 tests PASS.

- [ ] **Step 3: Commit:**
```bash
cd ~/farder && git add crates/farder-relay/src/limits.rs crates/farder-relay/src/main.rs && \
git commit -m "relay: ConnectionLimiter (global cap + per-IP rate limit)"
```

---

## Task 2: Wire the limiter into the accept loop

**Files:** `crates/farder-relay/src/router.rs`, `crates/farder-relay/src/main.rs`.

- [ ] **Step 1: `serve` admits via the limiter.** In `crates/farder-relay/src/router.rs`, change `serve` to take a limiter and gate each incoming connection. Replace the current `serve`:

```rust
/// Accept loop: spawn a handler per incoming QUIC connection, gated by the
/// abuse-control limiter (global cap + per-IP rate limit). Over-limit
/// connections are refused before the handshake.
pub async fn serve(
    endpoint: Endpoint,
    connections: ConnectionMap,
    limiter: std::sync::Arc<crate::limits::ConnectionLimiter>,
) -> Result<()> {
    while let Some(incoming) = endpoint.accept().await {
        let ip = incoming.remote_address().ip();
        let guard = match limiter.try_admit(ip, std::time::Instant::now()) {
            Some(g) => g,
            None => {
                warn!("refused connection from {} (over limit)", ip);
                incoming.refuse();
                continue;
            }
        };
        let connections = connections.clone();
        tokio::spawn(async move {
            let _guard = guard; // held for the connection's lifetime
            match incoming.await {
                Ok(conn) => {
                    if let Err(e) = handle_connection(conn, connections).await {
                        warn!("connection error: {}", e);
                    }
                }
                Err(e) => warn!("incoming connection failed: {}", e),
            }
        });
    }
    Ok(())
}
```

(quinn 0.11 `Incoming` has `remote_address(&self)` and `refuse(self)`; awaiting `incoming` still yields `Result<Connection>`.)

- [ ] **Step 2: Build the limiter in `main.rs`.** In `crates/farder-relay/src/main.rs`, build the limiter from config and pass it to `serve`:

```rust
    let limiter = std::sync::Arc::new(limits::ConnectionLimiter::new(
        config.max_connections as usize,
        30,                                  // max new connections per IP per window
        std::time::Duration::from_secs(60),  // the rate window
    ));
    let connections = router::new_connection_map();
    router::serve(endpoint, connections, limiter).await?;
    Ok(())
```

- [ ] **Step 3: Fix the relay tests that call `serve`.** The relay's router tests (`#[cfg(test)] mod tests` in `router.rs`) and `start_relay()` helper call `serve(ep, conns.clone())`. Update those calls to pass a permissive limiter, e.g.:

```rust
        let limiter = std::sync::Arc::new(crate::limits::ConnectionLimiter::new(
            10_000, 10_000, std::time::Duration::from_secs(60),
        ));
        tokio::spawn(serve(ep, conns.clone(), limiter));
```

(Grep `serve(` in `router.rs` tests and update every call. A permissive limiter so existing tests are unaffected.)

- [ ] **Step 4: Add a cap-refusal integration test.** In `router.rs`'s `#[cfg(test)] mod tests`, add a test that a connection beyond a `max_connections=1` cap is refused. Use the existing `start_relay`/`test_client_endpoint` helpers but with a cap-1 limiter — you'll need a variant of `start_relay` that takes a limiter, or inline the relay start. Add:

```rust
    #[tokio::test]
    async fn over_cap_connection_is_refused() {
        ensure_provider();
        let dir = tempfile::tempdir().unwrap();
        let ep = crate::listener::create_endpoint("127.0.0.1:0".parse().unwrap(), dir.path()).unwrap();
        let addr = ep.local_addr().unwrap();
        let conns = new_connection_map();
        let limiter = std::sync::Arc::new(crate::limits::ConnectionLimiter::new(
            1, 10_000, std::time::Duration::from_secs(60),
        ));
        tokio::spawn(serve(ep, conns.clone(), limiter));
        std::mem::forget(dir);

        // First connection: open it and keep it alive (don't drop) so it holds the slot.
        let ep1 = test_client_endpoint();
        let c1 = ep1.connect(addr, "farder-relay").unwrap().await.unwrap();
        // Open a bi-stream so the connection is fully established and held.
        let _s1 = c1.open_bi().await.unwrap();
        std::mem::forget(ep1);

        // Second connection: should be refused by the cap (the relay calls incoming.refuse()).
        let ep2 = test_client_endpoint();
        let attempt = ep2.connect(addr, "farder-relay").unwrap().await;
        assert!(attempt.is_err(), "connection over the cap must be refused");
        std::mem::forget(ep2);
        let _ = c1; // keep c1 alive until here
    }
```

(If `refuse()` results in the client's `connect().await` succeeding-then-closing rather than erroring in your quinn version, adjust the assertion to detect the immediate close — but in quinn 0.11 `Incoming::refuse()` rejects the handshake, so `connect().await` returns `Err`. Confirm and keep the assertion meaningful.)

- [ ] **Step 5: Run the relay tests:** `cd ~/farder && cargo test -p farder-relay 2>&1 | tail -15` — expect all pass (the existing register/bridge tests + `over_cap_connection_is_refused`).

- [ ] **Step 6: Commit:**
```bash
cd ~/farder && git add crates/farder-relay/src/router.rs crates/farder-relay/src/main.rs && \
git commit -m "relay: gate the accept loop with the connection limiter"
```

---

## Task 3: Idle timeout + coordinated keep-alive

**Files:** `crates/farder-relay/src/listener.rs`, `crates/farder-server/src/relay.rs`.

Background: the relay currently sets no explicit idle timeout (quinn's ~30s default), and the SERVER's relay client endpoint sets no keep-alive — so an idle-but-live registered server can be dropped. Set an explicit 60 s idle timeout on the relay and a 15 s keep-alive on the server's relay endpoint (the CLIENT's pinned relay endpoint already keep-alives at 15 s).

- [ ] **Step 1: Idle timeout on the relay endpoint.** In `crates/farder-relay/src/listener.rs` `create_endpoint`, add a transport config with an idle timeout to the server config. Change the `server_config` construction:

```rust
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
    ));
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        std::time::Duration::from_secs(60)
            .try_into()
            .map_err(|e| anyhow::anyhow!("idle timeout: {:?}", e))?,
    ));
    server_config.transport_config(Arc::new(transport));
    let endpoint = Endpoint::server(server_config, bind_addr)?;
```

- [ ] **Step 2: Keep-alive on the server's relay endpoint.** In `crates/farder-server/src/relay.rs`, find `relay_client_endpoint` (it builds a quinn client `Endpoint` with skip-verify). Add a transport config with a keep-alive interval so the registered server's control connection stays alive under the relay's idle timeout. After building `cfg` (the `quinn::ClientConfig`) and before/at `ep.set_default_client_config(cfg)`, set a transport config:

```rust
    let mut cfg = quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?));
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(std::time::Duration::from_secs(15)));
    cfg.transport_config(Arc::new(transport));
    let mut ep = Endpoint::client("0.0.0.0:0".parse()?)?;
    ep.set_default_client_config(cfg);
```

(Adapt to the actual variable names in `relay_client_endpoint`. The key addition is the 15 s keep-alive on its transport config.)

- [ ] **Step 3: Build + run the relay and server tests:** `cd ~/farder && cargo build -p farder-relay -p farder-server 2>&1 | tail -4 && cargo test -p farder-relay 2>&1 | tail -6 && cargo test -p farder-server --test relay_mode 2>&1 | tail -6` — expect builds + all pass (the relay-mode integration tests are fast, well under 60 s, so the idle timeout doesn't trip them).

- [ ] **Step 4: Commit:**
```bash
cd ~/farder && git add crates/farder-relay/src/listener.rs crates/farder-server/src/relay.rs && \
git commit -m "relay: explicit idle timeout + keep-alive on the server relay endpoint"
```

---

## Task 4: Deployment artifacts (Docker + systemd)

**Files:** Create `deploy/relay/Dockerfile`, `deploy/relay/docker-compose.yml`, `deploy/relay/farder-relay.service`, and `.dockerignore` (repo root).

- [ ] **Step 1: `.dockerignore`.** Create `/home/deez/farder/.dockerignore` so the Docker build context excludes build output:

```
target/
**/target/
node_modules/
**/node_modules/
.git/
client/dist/
*.db
*.db-shm
*.db-wal
```

- [ ] **Step 2: Dockerfile.** Create `deploy/relay/Dockerfile`:

```dockerfile
# Build: from the repo root, run
#   docker build -f deploy/relay/Dockerfile -t farder-relay .
FROM rust:1-slim AS builder
WORKDIR /build
COPY . .
RUN cargo build --release -p farder-relay

FROM debian:stable-slim
RUN useradd -r -u 10001 relay && mkdir -p /data && chown relay /data
COPY --from=builder /build/target/release/farder-relay /usr/local/bin/farder-relay
USER relay
VOLUME ["/data"]
EXPOSE 4433/udp
ENTRYPOINT ["/usr/local/bin/farder-relay", "--bind", "0.0.0.0:4433", "--data-dir", "/data"]
```

- [ ] **Step 3: docker-compose.yml.** Create `deploy/relay/docker-compose.yml`:

```yaml
# From the repo root: docker compose -f deploy/relay/docker-compose.yml up -d --build
services:
  relay:
    build:
      context: ../..
      dockerfile: deploy/relay/Dockerfile
    image: farder-relay
    restart: unless-stopped
    ports:
      - "4433:4433/udp"
    volumes:
      - relay-data:/data
volumes:
  relay-data:
```

- [ ] **Step 4: systemd unit.** Create `deploy/relay/farder-relay.service`:

```ini
# Bare-VPS alternative to Docker. Install:
#   sudo cp target/release/farder-relay /usr/local/bin/
#   sudo useradd -r -s /usr/sbin/nologin relay && sudo mkdir -p /var/lib/farder-relay && sudo chown relay /var/lib/farder-relay
#   sudo cp deploy/relay/farder-relay.service /etc/systemd/system/
#   sudo systemctl daemon-reload && sudo systemctl enable --now farder-relay
[Unit]
Description=Farder privacy relay
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/farder-relay --bind 0.0.0.0:4433 --data-dir /var/lib/farder-relay
User=relay
Restart=always
RestartSec=3
Environment=RUST_LOG=farder_relay=info

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 5: Optional Docker build smoke (only if Docker is present).** Run: `cd ~/farder && command -v docker >/dev/null && docker build -f deploy/relay/Dockerfile -t farder-relay-smoke . 2>&1 | tail -5 || echo "docker not available — build verified on host during deploy"`. Either a successful build or the "host-verified" note is acceptable (WSL often lacks Docker).

- [ ] **Step 6: Commit:**
```bash
cd ~/farder && git add deploy/relay/ .dockerignore && \
git commit -m "deploy: relay Dockerfile, compose, and systemd unit"
```

---

## Task 5: Deploy guide

**Files:** Create `docs/deploy/relay.md`.

- [ ] **Step 1: Write the guide.** Create `docs/deploy/relay.md` with these concrete sections (real commands, not prose placeholders):

1. **What you need:** a small VPS (1 vCPU / 512MB–1GB is plenty; ~$5/mo), a domain (optional but recommended), and the ability to open a UDP port. Note the ongoing cost + that you carry users' traffic.
2. **Open the firewall:** allow **inbound UDP 4433** (the relay's QUIC port) in the VPS provider's security group AND the host firewall (`sudo ufw allow 4433/udp`).
3. **Run it — Docker (recommended):**
   - Install Docker. From the repo on the host: `docker compose -f deploy/relay/docker-compose.yml up -d --build`.
   - Verify: `docker compose -f deploy/relay/docker-compose.yml logs` shows `Relay listening on 0.0.0.0:4433` and `server registered`/`new connection` as peers connect.
4. **Run it — systemd (alternative):** the steps from the unit-file header comment (build `cargo build --release -p farder-relay`, copy the binary, create the user + `/var/lib/farder-relay`, install + enable the unit). Verify with `systemctl status farder-relay` and `journalctl -u farder-relay -f`.
5. **DNS (optional):** point an A/AAAA record (e.g. `relay.farder.gg`) at the VPS IP, so the relay address is a name (`relay.farder.gg:4433`) not a raw IP.
6. **Read the cert fingerprint:** the client pins the relay's cert by its SHA-256 DER fingerprint. Read it from the persisted cert:
   - Docker: `docker compose -f deploy/relay/docker-compose.yml exec relay sha256sum /data/relay_cert.der`
   - systemd: `sha256sum /var/lib/farder-relay/relay_cert.der`
   Copy the 64-hex-char value.
7. **Plug it into the client:** edit `client/src-tauri/src/default_relay.rs` and set `DEFAULT_RELAY` to `Some(DefaultRelay { addr: "relay.farder.gg:4433", cert_fp_hex: "<the 64 hex chars>" })`, then rebuild the client. (Consuming it — defaulting in-app relayed-server creation, shorter links — is a later phase; this just records it.)
8. **Operating it:** updating (`git pull` + `up -d --build` / rebuild+restart), logs, the abuse-control knobs (`--max-connections`, and the per-IP rate is a code constant in `limits.rs`), and that the persistent cert lives in the data volume/dir (back it up so the fingerprint is stable across redeploys — regenerating the cert changes the fingerprint and invalidates the bundled value).

- [ ] **Step 2: Commit:**
```bash
cd ~/farder && git add docs/deploy/relay.md && \
git commit -m "docs: relay deployment guide"
```

---

## Task 6: Client default-relay config scaffold

**Files:** Create `client/src-tauri/src/default_relay.rs`; modify `client/src-tauri/src/main.rs`.

- [ ] **Step 1: Create the scaffold.** Create `client/src-tauri/src/default_relay.rs`:

```rust
//! The hosted default Farder relay, if one is configured. Filled in AFTER the
//! default relay is deployed — see docs/deploy/relay.md. `None` means no default
//! relay is configured (users connect via custom/self-hosted relay links only).
//!
//! Not consumed by any code yet: this is the post-deploy config target and the
//! stable anchor that future phases (in-app relayed-server creation, shorter
//! invite links) will read.

#[allow(dead_code)]
pub struct DefaultRelay {
    /// The relay's address, e.g. "relay.farder.gg:4433".
    pub addr: &'static str,
    /// SHA-256 of the relay's certificate DER, hex (64 chars).
    pub cert_fp_hex: &'static str,
}

/// The configured default relay, or `None` until one is deployed and filled in.
#[allow(dead_code)]
pub const DEFAULT_RELAY: Option<DefaultRelay> = None;
```

- [ ] **Step 2: Declare the module.** In `client/src-tauri/src/main.rs`, add `mod default_relay;` to the module list (alphabetical-ish, near the other `mod` lines).

- [ ] **Step 3: Build.** `cd ~/farder/client/src-tauri && cargo build 2>&1 | tail -4` — expect it builds (the `#[allow(dead_code)]` keeps it warning-free while unconsumed).

- [ ] **Step 4: Commit:**
```bash
cd ~/farder && git add client/src-tauri/src/default_relay.rs client/src-tauri/src/main.rs && \
git commit -m "client: default-relay config scaffold (filled in post-deploy)"
```

---

## Final verification

- [ ] **Relay + server green:** `cd ~/farder && cargo test -p farder-relay 2>&1 | tail -8 && cargo test -p farder-server --test relay_mode 2>&1 | tail -5`.
- [ ] **Whole workspace + client build:** `cd ~/farder && cargo build --workspace 2>&1 | tail -3 && cd client/src-tauri && cargo build 2>&1 | tail -3`.
- [ ] **Artifacts present:** `ls deploy/relay/ docs/deploy/relay.md client/src-tauri/src/default_relay.rs`.
- [ ] **Docs:** note in `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md` that the default relay is deploy-ready (abuse controls + artifacts + guide) and that provisioning is a user step. Optionally add a `docs/modules/relay-deploy.md` pointer.
- [ ] **Tell the user the provisioning is theirs:** state plainly that the code/artifacts are done but **deploying the relay (renting a host, DNS, running it, reading the fingerprint, filling `default_relay.rs`) is the hands-on step in `docs/deploy/relay.md`** — offer to walk through it when they have a host. The Docker image build is verified on their host (WSL may lack Docker).
- [ ] **Finish the branch:** use superpowers:finishing-a-development-branch.

## Notes for the implementer
- The abuse-control logic (Task 1) is the only deeply unit-tested part; the limiter's injected clock makes it deterministic — do not use wall-clock sleeps in those tests.
- Task 3 is a real bug fix (idle-but-live registered servers could drop) — keep the keep-alive interval (15 s) well under the idle timeout (60 s).
- Provisioning is OUT OF SCOPE — do not attempt to deploy anything; just produce the artifacts + guide.
- `default_relay.rs` stays `None` and unconsumed — do not wire it into any feature (that's a later phase).
