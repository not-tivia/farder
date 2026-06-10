# Relay Phase 4 piece 1 — In-App Relayed-Server Creation — Design Spec

**Date:** 2026-06-10
**Status:** Approved (design); ready to plan
**Parent design:** `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md`
**Builds on:** relay phases 1-5 (relay routing, server relay-mode, `connect_via_relay`,
relay links, voice over relay) — all merged. The **default relay is deployed**
(Vultr `45.77.70.199:4433`) and `client/src-tauri/src/default_relay.rs` `DEFAULT_RELAY`
is populated (addr + cert fingerprint) but unconsumed.

## Problem

The app can only create **direct** servers — `create_local_server` spawns a local
`farder-server` with `--bind 0.0.0.0:<port>` and connects to `127.0.0.1:<port>`. For a
typical home user behind NAT, that server is **unreachable by anyone else** (no inbound
without port-forwarding) and exposes IPs. Every relay building block exists
(`--relay` server mode, `connect_via_relay`, relay links, `DEFAULT_RELAY`) but nothing
wires them into the create flow. This phase lets a user **create a relayed server** —
one that dials *out* to the default relay and registers, so members reach it through the
relay (works behind NAT, IPs hidden) — with an honest relay-choice UX at create time.

## Decisions (settled with the owner)

| Decision | Choice |
|----------|--------|
| Default framing | **Relay is the recommended default**; Direct is an *advanced* option labelled "same network only / your IP is visible". (Relay = private AND the only thing that's reachable for home users.) |
| Relay options | **Three:** (1) Use the Farder relay [recommended/default], (2) Self-host your own relay [advanced — custom addr + fingerprint], (3) Direct [advanced]. |
| Explanations | **Layered** — a one-line summary per option + a **"learn more"** expandable with the honest trust detail (relay hides IPs; must be a neutral third party; today the relay operator can read community content since it's not E2EE — ties to the E2E-tunnel hardening backlog). |
| `server_id` origin | **Client generates** it (32 random bytes) and pre-writes `<data_dir>/server_id` before spawning — no race, no read-back; the client already knows it for the relay link. |
| Join-side disclosure | **Split out** — the relay badge in `JoinConfirmModal` is a separate small follow-up, NOT this piece. |

## Architecture

### Part A — `default_relay` accessor (`client/src-tauri/src/default_relay.rs`)

Add `pub fn default_relay() -> Option<(SocketAddr, Vec<u8>)>` that, when `DEFAULT_RELAY`
is `Some`, parses `addr` to a `SocketAddr` and hex-decodes `cert_fp_hex` to bytes;
returns `None` if unset or malformed. Remove the `#[allow(dead_code)]` (now consumed).

### Part B — `spawn_server` relay mode (`client/src-tauri/src/server_manager.rs`)

`spawn_server` gains a relay parameter: `relay: Option<(SocketAddr, [u8; 32])>` (relay
addr + the client-generated `server_id`).
- **Relay mode (`Some`):** create the data dir, write the 32-byte `server_id` to
  `<data_dir>/server_id`, and spawn with args `--relay <addr> --data-dir <data_dir>
  --name <name> --template <template> --db <data_dir>/server.db --storage-dir
  <data_dir>/files` (NO `--bind`, no port scan). The server's `load_or_generate_server_id`
  reads the pre-written id.
- **Direct mode (`None`):** unchanged (find a port, `--bind 0.0.0.0:<port>`).
- **Extract the CLI-arg construction into a pure function** (e.g.
  `build_server_args(name, template, data_dir, mode) -> Vec<String>`) so it's
  unit-testable without spawning. `ManagedServer` carries enough for the caller to know
  it's relayed (e.g. an `Option` relay field or the absence of a port).

### Part C — `create_local_server` branch (`client/src-tauri/src/commands.rs`)

The command gains the relay choice (see Part E for the wire shape) and resolves it to an
`Option<(SocketAddr, Vec<u8>)>` relay target (`None` = direct):
- **Farder:** `default_relay()`. If it returns `None` (relay not configured), error
  clearly ("no default relay configured").
- **Self-host:** parse the user's `relay_addr` (`SocketAddr`) and `relay_fp` (64 hex →
  bytes); error on malformed input *before* spawning.
- **Direct:** `None`.

**Relayed path:**
1. Generate `server_id` (32 random bytes).
2. `spawn_server(..., Some((relay_addr, server_id)))`.
3. **Wait by retrying `connect_via_relay`** against `RelayTarget { relay_addr,
   server_id, cert_fp, invite_token: "" }` until it succeeds (server has registered) or
   a timeout (~20-30 s) — replacing the direct `127.0.0.1:<port>` QUIC probe, which
   doesn't apply to a relay-mode server (it has no local port). The owner connects with
   **no invite** (empty token → `invite_code = None`), so `authenticate` auto-claims
   owner exactly as in the direct flow.
4. Build the owner's relay link `farder://relay/<addr>/<server_id_hex>/<fp_hex>/`
   (empty invite slot — the owner reconnects by identity, not an invite) and **save it as
   the server entry id** (with the existing `LocalServerConfig { data_dir, template }`).
5. Return the connect payload with `relayed: true`.

**Direct path:** unchanged (`relayed: false`).

A small fix to `parse_relay_target` / `build_relay_link` so the link round-trips an
**empty invite token** (the owner's own entry has none); `connect_via_relay` treats an
empty token as `invite_code = None`.

### Part D — pinned relay endpoint for the owner connect

The relayed connect uses `tls::make_pinned_relay_endpoint(cert_fp)` (already exists, and
since 5b-client it enables datagrams), pinning the relay's cert — same path a normal
relayed-client join uses. Reused as-is.

### Part E — Create-server UX (`client/src/components/AddServerModal.tsx` + `tauri-bridge.ts`)

Add a **"How will people reach your server?"** step (after template/privacy) with three
radio options, Farder pre-selected:
- **Use the Farder relay** — *Recommended*. "Hidden IPs, and it works even behind a home
  router." + `learn more`.
- **Self-host your own relay** — *Advanced*. Reveals two inputs **only when selected**:
  relay address (`host:port`) and cert fingerprint (64 hex). + `learn more`.
- **Direct — same network only** — *Advanced*. "Connects straight to your machine; only
  reachable on your LAN or with port-forwarding; your IP is visible." + `learn more`.

The **`learn more`** expandable carries the layered honest explanation (a relay hides
IPs; it must be a neutral third party to do so; and today the relay operator can read
community content because it isn't end-to-end encrypted — a hardening is planned).

`tauri-bridge.ts` `createLocalServer(...)` gains the relay choice — passed as
`relayMode: "farder" | "selfhost" | "direct"` plus optional `relayAddr` / `relayFp` for
self-host — and `invoke("create_local_server", ...)` carries them. The frontend blocks
"Create" on self-host until both fields are non-empty.

### Tauri seam (CLAUDE.md failure mode)

**No new commands** — `create_local_server` (already in `generate_handler!`) only gains
parameters, so there is no new `invoke`/handler to register. The plan re-confirms the
`invoke("create_local_server")` arg names match the command signature.

## File structure

- `client/src-tauri/src/default_relay.rs` — `default_relay()` accessor; drop `allow(dead_code)`.
- `client/src-tauri/src/server_manager.rs` — `spawn_server` relay param + `server_id`
  pre-write + extracted `build_server_args`.
- `client/src-tauri/src/commands.rs` — `create_local_server` relay branch; self-host
  validation; relayed connect-retry; save relay link.
- `client/src-tauri/src/connection.rs` — empty-invite-token handling in
  `parse_relay_target`/`build_relay_link`/`connect_via_relay`.
- `client/src/components/AddServerModal.tsx` — the reachability step + learn-more.
- `client/src/lib/tauri-bridge.ts` — `createLocalServer` relay params.
- `docs/modules/client-relay.md` — document in-app relayed-server creation.

## Testing

**Headless (real, runnable here):**
- `default_relay()` parses the configured addr + fingerprint; returns `None` on
  malformed/unset.
- `server_id` generation is 32 bytes; pre-write places `<data_dir>/server_id`.
- `build_server_args` produces `--relay <addr> --data-dir <dir>` (and **no** `--bind`)
  in relay mode, and `--bind 0.0.0.0:<port>` (no `--relay`) in direct mode.
- Relay-link construction for the owner round-trips through
  `parse_relay_target`/`build_relay_link` **with an empty invite token**.
- Self-host validation rejects a bad address / non-64-hex fingerprint.
- Frontend `npx tsc --noEmit` passes; the `invoke("create_local_server")` arg names
  match the command signature (seam check).

**Windows-verified (now genuinely possible — the relay is live):** the full click-through
— create a relayed server → it registers with `45.77.70.199:4433` → owner auto-claimed →
invite a second client → join → text + voice. **Flag UNVERIFIED until that run**, but it
is a real, runnable end-to-end test now (not deferred-forever).

## Out of scope

- **Join-side relay disclosure** (badge in `JoinConfirmModal`) — separate follow-up.
- **The E2E-tunnel content hardening** — backlog (the `learn more` text references it).
- **Migrating an existing direct server to relayed** — not in this piece.
