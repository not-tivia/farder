# Incoming Webhooks (v1) — design

**Date:** 2026-07-02
**Status:** design (awaiting owner review)
**Context:** next in the bots project (see [[project_farder_bots]]). Builds on "a bot posts a message" (price-alerts DMs) but posts to a **channel** and is triggered by an **inbound HTTP POST** from an external service. Discord-webhook compatible so existing integrations work by swapping the URL.

## Problem

People want external services (GitHub, CI, monitoring, scripts) to post into a Farder channel. That needs an **inbound HTTP endpoint** — but Farder servers are QUIC + relay-masked and expose **no public HTTP** (the IP-privacy promise). Resolved: the **relay** (already the server's front door, and already makes outbound HTTP for embeds) grows a small inbound webhook endpoint and forwards to the server over the existing tunnel, so the server's IP is never exposed.

## What already exists (v1 builds on this)

- **Relay** (`farder-relay`, QUIC-only today): registers servers under a stable `server_id` (`router.rs handle_register`), holds each server's QUIC `Connection`, and **already opens relay-originated streams to a registered server** — `fetch_preview` does `server_conn.open_bi()` then writes handle `0` (relay-originated marker) + a `RelayStreamRole` frame (`proxy.rs:196-201`). The relay also has an outbound HTTP client + SSRF guard + a per-IP rate limiter (`limits.rs`).
- **Server relay client** (`farder-server/src/relay.rs serve_via_relay`): accepts relay bi-streams, reads the routing handle then a `RelayStreamRole`, and dispatches (`Primary`, `Session{token}`). A new `Webhook` role slots in here.
- **Message posting:** `messages::insert_message(conn, channel_id, author: &PublicKey, content, reply_to)` + `ServerEvent::NewMessage` broadcast to `EventTarget::Subscribers(channel_id)` (the same path `send_bot_dm` reused).
- **Server-managed identities:** bots already have server-held generated keypairs; a webhook reuses that idea (a generated author key).

## Goals

1. An owner can create a **per-channel webhook** and get a URL `https://<relay>/webhook/<server_id>/<token>`.
2. An external service POSTing a **Discord-compatible** body (`{"content": "...", "username": "..."}`) to that URL results in a message in the webhook's channel.
3. The server's IP is never exposed (relay-mediated); the relay stores no webhook secrets (the server validates the token).
4. A webhook post shows as a message **tagged `WEBHOOK`** with a display name (the per-post `username`, else the webhook's name) — **not** as a member in the sidebar (matches Discord).

## Non-goals (v1)

- **Direct (non-relay) servers** — webhooks require the relay ingress. (Direct servers exposing HTTP would leak their IP — rejected.)
- **Rich embeds / `avatar_url` / per-webhook avatar** — v1 accepts `content` + `username`, ignores the richer Discord fields without erroring (so those posts still deliver their text). No custom avatar (default webhook avatar).
- **Outgoing webhooks** (Farder → external on events) — a separate future feature.
- Webhook-in-roster, slash-command/interaction webhooks, multipart/file uploads.

## Design

### Ingress (relay)

- The relay gains a **minimal inbound HTTP listener** (a lightweight HTTP server; adds an HTTP-server dependency — e.g. `hyper`/`axum` — to `farder-relay`, which currently has only outbound HTTP). It handles `POST /webhook/{server_id}/{token}` with a JSON (or form) body.
- **Routing:** look up the registered server by `server_id` (hex). Unknown/offline → `404`/`503`, no forward. `server_id` is the same non-secret routing id already used in relay deep links; the **`token` is the secret**.
- **Forward:** open a bi-stream to the server's `Connection` (mirror `fetch_preview`: handle `0` marker), write a new `RelayStreamRole::Webhook { token, body, source_ip }`, await the server's small ack, and return the corresponding HTTP status (`204` ok / `401` bad token / `404` unknown / `429` rate-limited / `413` too large).
- **Abuse controls at the relay:** reuse/extend the per-IP rate limiter; hard **body-size cap** (e.g. 64 KiB) read before forwarding; short timeout. The relay never inspects or stores the token beyond passing it through.

### Server side

- New `webhooks` table: `{ id, channel_id, token TEXT (random secret), name TEXT, public_key BLOB (generated author identity), created_at }`.
- `serve_via_relay` dispatch gains a `RelayStreamRole::Webhook { token, body, source_ip }` arm → `webhooks::handle_delivery(state, token, body) -> WebhookAck`:
  1. Look up the webhook by `token` (constant-time compare); unknown → `Unauthorized`.
  2. Per-webhook **rate limit** + `content` length cap (reuse the message length limit).
  3. Parse the body as Discord-compatible JSON: `content` (required, non-empty), `username` (optional). Ignore unknown fields. Malformed/empty → `BadRequest`.
  4. Post: `messages::insert_message(conn, webhook.channel_id, &webhook.public_key, &content, None)` with an **`author_name_override`** = `username` or `webhook.name`; broadcast `NewMessage` to `Subscribers(channel_id)`.
  5. Return `Ok`.
- The webhook's `public_key` is the message `author` (a generated, non-member key). No presence, no roster entry.

### Display: `author_name_override` + WEBHOOK tag

- Add a nullable `author_name_override TEXT` column to `messages` (+ `MessageInfo.author_name_override: Option<String>`, `#[serde(default)]`). Set for webhook posts, NULL for normal messages.
- Client render: when a message has `author_name_override`, show that name + a small **`WEBHOOK`** badge (reuse the BOT-badge styling) and a default webhook avatar, instead of resolving the author against the member list. Normal messages unchanged. This is why webhooks need no roster entry and why per-post `username` "just works."

### Management UX (per-channel)

- In **channel settings**, a **Webhooks** section (owner / `MANAGE_SERVER`-gated): **Create** (name it) → the full URL incl. token is shown **once** to copy; **list** existing webhooks (name, channel); **delete** (revokes) and **regenerate token** (rotate). New requests: `CreateWebhook{channel_id, name}` → returns the token/URL, `ListWebhooks{channel_id}`, `DeleteWebhook{id}`, `RegenerateWebhookToken{id}`.

### Security

- The token is a high-entropy secret carried in the URL (standard webhook model); knowledge of the URL = ability to post. Revoke by delete or regenerate. Shown once (owner copies it).
- Untrusted external `content` is posted as a **plain message** (Farder renders messages as text/markdown, not code) — no execution. Length + size caps. Rate limits at relay (per-IP) and server (per-webhook).
- The relay 404s unknown `server_id` and never holds tokens; the server does constant-time token comparison.

## Testing

- **Server webhook delivery (unit/integration):** `handle_delivery` with a valid token + `{content}` posts a message with `author_name_override`; `{content, username}` overrides the name; unknown token → Unauthorized; empty/malformed body → BadRequest; over-length content rejected; rate limit trips. (Inject the DB/state; no network.)
- **Discord-compat parse:** `{content}` ok; extra fields (`embeds`, `avatar_url`) ignored, not errored; missing `content` rejected.
- **Relay HTTP + forward (spike):** the relay endpoint routes by `server_id`, forwards the `Webhook` frame, and maps the server ack to an HTTP status — proven by a local relay↔server integration (relay HTTP is otherwise **deploy-gated**: the relay runs on the VPS and must be redeployed to runtime-test, like the embed changes were).
- **Client:** `cargo build` + `tsc`; render an `author_name_override` message with the WEBHOOK badge.

## Owner runtime verification (relay REDEPLOY + server sidecar rebuild)

The relay changed → it must be **redeployed** on the VPS (`docker compose ... up -d --build` per the embed-redeploy pattern), plus the usual server sidecar rebuild. Then: channel settings → Webhooks → Create → copy the URL → `curl -X POST -H 'content-type: application/json' -d '{"content":"hello from curl"}' <url>` → a message "hello from curl" (WEBHOOK-tagged) appears in the channel; `{"content":"hi","username":"CI"}` shows as "CI"; a bad token → 401; delete the webhook → the URL 401s.

## Decomposition (for the plan)

1. **Webhook delivery spike (server side, testable):** the `webhooks` table + `RelayStreamRole::Webhook` frame + `webhooks::handle_delivery` (token validate → Discord parse → post with `author_name_override`) + the `serve_via_relay` dispatch arm. Proven by a `handle_delivery` integration test. *(Highest risk after the relay; do early.)*
2. **Relay HTTP ingress:** the inbound HTTP listener + `POST /webhook/{server_id}/{token}` routing + forward the `Webhook` frame + relay-side rate/size limits + ack→HTTP-status mapping. *(Deploy-gated.)*
3. **`author_name_override`** end to end: `messages` column + `MessageInfo` field + insert-with-override + client render (WEBHOOK badge + default avatar).
4. **Webhook CRUD + management UI:** Create/List/Delete/RegenerateToken requests/handlers (owner-gated) + Tauri commands + the channel-settings Webhooks section (create → show-URL-once, list, delete, regenerate).
5. **Docs.**

## Carry-forward / known limitations

- Relay-reachable servers only; the relay must be redeployed to ship this.
- v1 honors `content` + `username`; ignores `embeds`/`avatar_url` (text still delivers). No per-webhook avatar.
- The relay gains an inbound HTTP surface — a new public attack surface; mitigated by size cap + rate limit + unknown-handle 404 + the server-held token. Worth its own hardening pass if abused.
- Outgoing webhooks are a separate future feature.
