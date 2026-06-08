# Default Relay — Deploy-Readiness — Design Spec

**Date:** 2026-06-07
**Status:** Approved (design); ready to plan
**Parent design:** `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md`
**Depends on:** Phase 1 relay (binary + persistent cert) — merged.

## Problem

The relay feature works, but there is no **hosted default relay**. Deploying one
unblocks creating a relayed server in-app (a non-technical user otherwise has no
relay address/fingerprint to point at) and shorter invite links (bundle the
default relay's fingerprint instead of embedding it). Before any relay can sit on
the public internet, it needs **abuse controls** that Phase 1 explicitly deferred:
`Config.max_connections` is parsed but **unused**, there is no rate limiting, and
anyone may register. And there are **no deployment artifacts** (no Dockerfile,
systemd unit, or guide).

## Goal & scope boundary

Make the default relay **deploy-ready** — everything in code/artifacts/docs so a
deploy is turn-key — **without** provisioning any infrastructure. The actual
"rent a host, point DNS, run the deploy" is a hands-on step the **user** performs
later (guided by the deploy doc); it is explicitly **out of scope** here. This
phase ships: relay abuse controls, deployment artifacts, and an empty client
default-relay config scaffold.

## Decisions (settled)

| Decision | Choice |
|----------|--------|
| Abuse controls | **Minimal, meaningful** — enforce `max_connections`, a per-IP new-connection rate limit, and an idle timeout. Not a heavier system (no bandwidth caps / registration auth yet). |
| Deploy methods | Provide **both** Docker (primary, simplest) and a systemd unit (bare-VPS alternative). |
| Client default-relay config | An **empty, documented scaffold** (`default_relay.rs`) filled in post-deploy; not wired to any consumer yet (future phases). |
| Provisioning | **Out of scope** — guided user step, documented in the deploy guide. |

## Architecture

### Part 1 — Relay abuse controls (`crates/farder-relay`)

A new `limits.rs` module with a `ConnectionLimiter`:

- **Connection cap:** an `AtomicUsize` active-connection count + the configured
  `max_connections`. `try_admit` refuses when `active >= max`. Admission returns a
  `ConnectionGuard` that increments on admit and decrements on drop (held for the
  connection's lifetime in the spawned task).
- **Per-IP rate limit:** a `Mutex<HashMap<IpAddr, VecDeque<Instant>>>` sliding
  window — on admit, drop timestamps older than `window`, refuse if the IP already
  has `>= max_per_window` within it, else record and admit. Opportunistic prune of
  stale IP entries to bound map growth. The window/limit are constants (e.g. 30
  new connections / IP / minute) — tunable later.
- **Testability:** `try_admit(ip, now: Instant) -> Option<ConnectionGuard>` takes
  an injected `now` so unit tests are deterministic (no wall-clock flakiness);
  production passes `Instant::now()`.

`serve` (`router.rs`) change: for each `incoming`, read `incoming.remote_address().ip()`,
call `limiter.try_admit(ip, Instant::now())`; on `None`, `incoming.refuse()` and
continue; on `Some(guard)`, spawn the handler holding the guard for the
connection's lifetime. `main.rs` builds the limiter from `Config` and passes it to
`serve`.

**Idle timeout:** in `listener.rs::create_endpoint`, set a quinn `TransportConfig`
with `max_idle_timeout` (e.g. 60 s) on the server config, so registered/connected
peers that go silent are dropped and don't accumulate.

### Part 2 — Deployment artifacts (`deploy/relay/` + `docs/deploy/relay.md`)

- **`deploy/relay/Dockerfile`** — multi-stage: a Rust builder stage
  (`cargo build --release -p farder-relay`) → a slim runtime stage (Debian-slim or
  distroless) that copies the binary, `EXPOSE 4433/udp`, declares a `/data` volume
  for the persistent cert, and `ENTRYPOINT ["farder-relay","--bind","0.0.0.0:4433","--data-dir","/data"]`.
- **`deploy/relay/docker-compose.yml`** — one service, the image/build, ports
  `"4433:4433/udp"`, a named volume `relay-data:/data`, `restart: unless-stopped`.
- **`deploy/relay/farder-relay.service`** — a systemd unit (Restart=always, a
  dedicated data dir + non-root user, the binary + args) for a bare VPS.
- **`docs/deploy/relay.md`** — the step-by-step guide: choose a VPS, open the UDP
  port in the firewall/security group, run the relay (Docker *or* systemd), point a
  DNS name at it, **read out the cert fingerprint** (a documented one-liner over
  the persisted `relay_cert.der`, e.g. `sha256sum`/openssl), and paste the
  address + fingerprint into the client default-relay scaffold (Part 3). Includes
  how to verify it's running and basic operational notes (updating, logs, the
  abuse-control knobs).

### Part 3 — Client default-relay config scaffold (`client/src-tauri/src/default_relay.rs`)

A minimal, documented placeholder:

```rust
//! The hosted default Farder relay, if one is configured. Filled in AFTER the
//! default relay is deployed — see docs/deploy/relay.md. `None` means no default
//! relay is configured (users connect via custom/self-hosted relay links only).
pub struct DefaultRelay {
    pub addr: &'static str,       // e.g. "relay.farder.gg:4433"
    pub cert_fp_hex: &'static str // SHA-256 of the relay's cert DER, hex
}
pub const DEFAULT_RELAY: Option<DefaultRelay> = None;
```

Registered via `mod default_relay;` in `main.rs`. **Not consumed by any code yet**
— it is the concrete target the deploy guide's last step writes to, and the stable
anchor future phases (in-app relayed-server creation; shorter invite links) will
read. Build-gated only.

## File structure

- `crates/farder-relay/src/limits.rs` *(new)* — `ConnectionLimiter` + `ConnectionGuard` + tests.
- `crates/farder-relay/src/router.rs` — `serve` admits via the limiter; refuses over-limit.
- `crates/farder-relay/src/listener.rs` — idle timeout on the endpoint.
- `crates/farder-relay/src/main.rs` — build + pass the limiter; declare `mod limits;`.
- `crates/farder-relay/src/config.rs` — (existing `max_connections`; possibly add a rate-limit arg, or keep the rate as a constant).
- `deploy/relay/Dockerfile`, `deploy/relay/docker-compose.yml`, `deploy/relay/farder-relay.service` *(new)*.
- `docs/deploy/relay.md` *(new)* — the deploy guide.
- `client/src-tauri/src/default_relay.rs` *(new)* + `mod default_relay;` in the client `main.rs`.

## Testing

- **Abuse controls (headless):** unit tests on `ConnectionLimiter::try_admit` with
  an injected `now`: the cap admits up to `max` then refuses; a guard drop frees a
  slot; the per-IP window admits `max_per_window` then refuses within the window
  and admits again after it; map pruning bounds growth. Optionally an integration
  test (Phase 1 harness) that the (cap+1)th real connection is refused.
- **Idle timeout:** set as a quinn config value (not separately unit-tested; a
  config constant).
- **Client scaffold:** `cargo build -p farder-client` (compiles; `DEFAULT_RELAY`
  is `None`).
- **Deployment artifacts:** verified by review (a correct multi-stage Dockerfile,
  valid compose, a valid unit). Building the image needs Docker, which WSL may
  lack — the **user verifies the build/run on their host** during deploy (the guide
  walks through it). If Docker IS available in the build env, do a `docker build`
  smoke check; otherwise note it as host-verified.

## Out of scope / deferred

- **Actual provisioning** (VPS, DNS, running the public host) — the user's guided
  hands-on step.
- Wiring the `default_relay` config into consumers (in-app relayed-server creation,
  shorter links) — future phases.
- Heavier abuse controls (bandwidth caps, registration auth, IP allow/deny lists) —
  revisit if/when the relay sees real abuse.
- Voice over relay — separate phase.
