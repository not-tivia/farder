# Crypto Price-Ticker Bot (v1, server-driven info bot) — design

**Date:** 2026-07-01
**Status:** design (awaiting owner review)
**Context:** first slice of the parked **bots** project (see [[project_farder_mesh_hosting]] — bots were parked to layer on the mesh; this is the piece buildable today without mesh). Prior bot brainstorm settled the model: in-client/server "info bots" = Bot-tagged members auto-updating name/status on an interval from curated data (crypto first).

## Problem

Farder has no bots yet (greenfield — the original "bot keypair accounts" idea was never built). The owner wants a first, genuinely useful bot: a **crypto price ticker** that appears as a member and shows a live price. The open architectural question — who keeps it alive without mesh hosting — is resolved to **server-driven**: the `farder-server` daemon (already the always-on owner of a server's state) runs the poller and broadcasts the price, so bot-liveness == server-liveness (the correct coupling; a VPS-hosted server gets always-on bots today). The relay stays a dumb IP-masking pipe; the client is not required to be open.

## What already exists (v1 builds on this)

- **Presence rail** (shipped with rich presence): `Presence { kind: PresenceKind, details: String, state: Option<String> }`, `PresenceKind { Music, Game }` (protocol); `MemberInfo.presence: Option<Presence>`; `ServerState.presences: RwLock<HashMap<[u8;32], Presence>>` (in-memory, ephemeral, keyed by pubkey); `ServerEvent::MemberPresenceUpdated { public_key, presence }` broadcast; client renders a member's presence. This is the lightweight frequent-update channel the ticker rides.
- **Members**: `members` table (`public_key` PK, `display_name`, `avatar`, `joined_at`, `banned`, `revoked`); `MemberInfo` roster shape; `GetMembers` returns the roster (on a mesh/log server it is **filtered to log members** — a 4b/3a content-gating behavior the bot injection must account for).
- **Server outbound HTTP**, SSRF-guarded via `ssrf::resolves_to_global(url)` (already used for link-embed / URL fetch). The price fetch reuses this — the **server** calls the price API, so no user IP is ever exposed.

## Goals

1. The server owner can add one or more **crypto ticker bots** to a server; each appears in the member list as a **Bot-tagged member** whose name line shows a **live price** (e.g. `BTC $67,432 ▲2.1%`).
2. The **server** polls each bot's coin on an interval (server-side fetch, no user IP exposed) and broadcasts the price live to connected members; the bot is offline exactly when the server is.
3. Bots are managed from a server-settings panel: add (majors dropdown or a Custom free-form coin), remove; multiple per server.

## Non-goals (v1)

- Bots that **post messages** or respond to **commands** / slash-commands.
- **Moderation / utility / music / feed** bots (the always-on-heavy classes — later, on the mesh).
- An **external bot API** (third-party-authored bots) — v1 bots are built-in, server-managed.
- **Non-crypto** data sources.
- **Mesh-always-on** hosting — v1 is server-driven; bots upgrade to mesh-hosted (alive without a single host) for free when mesh lands, not a rewrite.
- Per-user or per-role bot config, alerts/thresholds, historical charts.

## Design

### Bot identity + data model

A bot is a **server-managed member with its own generated keypair** (so it occupies a normal roster slot keyed by `PublicKey` and is forward-compatible with the future mesh/E2EE identity model — no rework when bots move onto the log).

- New `bots` table: `public_key BLOB PK`, `secret_key BLOB` (the server-held Ed25519 secret — low-stakes plaintext, same trust level as the client device key; the server *is* the bot's authority), `kind TEXT` (`"crypto_ticker"`), `coin_id TEXT` (CoinGecko id, e.g. `bitcoin`), `display_name TEXT` (the static label, e.g. `BTC`), `created_at INTEGER`.
- The bot is **also inserted into `members`** (so the existing roster/member-list machinery includes it) with a new `is_bot INTEGER NOT NULL DEFAULT 0` column = 1. `MemberInfo` gains `is_bot: bool` (`#[serde(default)]`).
- **Mesh-roster interaction:** the `GetMembers` log-member filter (4b/3a) must **whitelist `is_bot` rows** so bots appear on both legacy and mesh servers. Bots are server-vouched, not gated content; the roster is only ever served to authorized members, so including them is safe. For v1 a bot is NOT a log identity (no `MemberJoined` event) — it is a server-local roster+presence entity; promotion to a log identity is a mesh-era follow-on.

### Server-driven poller

- On startup and every **~60s** (a `tokio` interval task in the server), the poller collects the **distinct `coin_id`s across all bots** and fetches them in **one coalesced CoinGecko call** (`/simple/price?ids=<comma-list>&vs_currencies=usd&include_24hr_change=true`), SSRF-guarded. N bots on the same coin → one fetch; many coins → one call. This bounds API load regardless of bot count.
- For each bot, it composes a `Presence` (see below), writes it into `ServerState.presences[bot_pk]`, and broadcasts `MemberPresenceUpdated { public_key: bot_pk, presence: Some(..) }` to the server's members.
- **Failure handling:** a failed/timed-out fetch leaves the **last-known** presence in place (the ticker shows the previous value rather than vanishing); after repeated failures the presence carries a subtle stale marker (e.g. `details` unchanged, `state` set to `"stale"`), and `tracing::warn!` logs it. A newly-added bot with no successful fetch yet shows `"…"`.

### Presence shape for tickers

Add `PresenceKind::Ticker`. For a ticker bot: `details` = the price string (e.g. `"$67,432 ▲2.1%"`), `state` = an optional secondary (e.g. `"24h"` or a stale marker). Rendering (below) places `display_name` + `details` together on the bot's name line, satisfying "price in the name" via the cheap ephemeral rail rather than mutating the signed display-name each tick.

### Client — management UI

A **"Bots"** section in server settings (owner-gated; reuse the existing settings-panel + permission gating the roles/other settings use). Lists current bots; an **Add bot** control offers a **curated majors dropdown** (BTC, ETH, SOL, LTC, XRP, DOGE, ADA — mapped to CoinGecko ids) plus a **"Custom…"** entry that reveals a free-form field to type any coin (resolved against CoinGecko's coin list / validated on add). Optional custom label (defaults to the coin's symbol). Remove button per bot. New Tauri commands + bridge: `add_bot(server_id, coin_id, label)`, `remove_bot(server_id, bot_public_key)`, and bots surface in the existing member roster (no separate list fetch needed for display). New `ServerRequest`/handlers: `AddBot`/`RemoveBot` (owner-gated server-side too).

### Client — rendering

- A **`BOT` badge** next to the name for `member.is_bot` (reuse an existing badge/pill class, themed in all three theme files if a new class is needed).
- For a bot, render the presence **`details` inline on the name line** (e.g. `BTC  $67,432 ▲2.1%`) rather than as the hover/secondary activity line used for humans. Live-updates arrive through the existing `MemberPresenceUpdated` listener (no new plumbing).
- Human-only actions (kick / ban / timeout / DM) are hidden on a bot; management is via the settings panel (Remove).

## Data flow (owner adds a BTC ticker)

```
owner: server settings → Bots → Add → BTC
  client add_bot(server, "bitcoin", "BTC")
    server: generate keypair; INSERT bots + members(is_bot=1); return roster update
    broadcast the new member (BOT badge appears)
  poll cycle (server, ~60s):
    fetch CoinGecko simple/price?ids=bitcoin,... (SSRF-guarded, server IP)
    presences[bot_pk] = Presence{Ticker, "$67,432 ▲2.1%", "24h"}
    broadcast MemberPresenceUpdated{bot_pk, ...}
  every client: bot row renders "BTC $67,432 ▲2.1% · BOT", live
server restarts / goes down → bot offline with the server (expected)
```

## Testing

**Rust (`farder-server`):**
- Bot CRUD: `add_bot` inserts `bots` + `members(is_bot=1)` with a generated keypair; `remove_bot` deletes both; only the owner may add/remove (non-owner rejected).
- Roster: `GetMembers` includes `is_bot` bots on BOTH a legacy and a mesh/log server (the log-member filter whitelists bots).
- Poller: given a stubbed/injected price response, the poller composes the expected `Presence` and updates `ServerState.presences[bot_pk]`; distinct coins are coalesced into one fetch (assert one request for two bots sharing a coin); a fetch failure preserves the last presence. (Factor the CoinGecko call behind a trait/fn so tests inject a fake — do not hit the network in tests.)
- Presence round-trips with `PresenceKind::Ticker`.

**Client:** `cargo build` (client crate) + `npx tsc --noEmit`. Runtime (owner Windows) is the real gate per CLAUDE.md.

**Docs** (same commit as the code that changes a surface): `tauri-commands.md` (`add_bot`/`remove_bot`), the bridge doc, `protocol.md` (`PresenceKind::Ticker`, `MemberInfo.is_bot`, `AddBot`/`RemoveBot`), a server doc (the poller + roster whitelist), `ARCHITECTURE.md` (the bot data path).

## Owner runtime verification (server changed → full rebuild incl. sidecar)

`git pull` → `cargo build -p farder-server` → STOP app → `copy-sidecar.ps1` (from repo root) → `cd client; npm run tauri dev` → `Ctrl+Shift+R`. Then:
1. Server settings → Bots → Add → **BTC** → a `BTC · BOT` member appears; within ~60s its name line shows a live price; a second client sees the same.
2. Add a **Custom** coin (a Solana token) → its ticker appears.
3. Remove a bot → it disappears for everyone.
4. Restart the server → bots return and resume ticking (config persisted).
5. A non-owner has no Bots settings section / cannot add.

## Decomposition (for the implementation plan)

1. **Server data model + CRUD:** `bots` table, `members.is_bot` + `MemberInfo.is_bot`, generated keypair, `AddBot`/`RemoveBot` requests + Tauri commands (owner-gated), roster whitelist for bots on mesh servers.
2. **Server poller + price source:** `PresenceKind::Ticker`, the coalesced SSRF-guarded CoinGecko fetch behind an injectable seam, the interval task, presence broadcast, failure/stale handling.
3. **Client management UI:** the server-settings Bots panel (majors dropdown + Custom, add/remove/list), bridge + commands.
4. **Client rendering:** BOT badge + inline ticker on the name line; hide human-only actions on bots; theming.

## Carry-forward / known limitations

- **Bot-liveness = server-liveness** (by design). Always-on for VPS-hosted servers now; mesh hosting later makes it always-on without a single dedicated host.
- **CoinGecko dependency + rate limits:** coalesced fetch + ~60s interval keep well within the free tier; a hard outage shows stale prices, not a crash. (A relay-proxied / multi-source abstraction is a later hardening.)
- **Bot keypair held plaintext by the server** — low-stakes (same as the client device key); the server is the bot's authority. Not an E2EE identity yet.
- **v1 bots are server-local, not log identities** — they don't appear in the signed event log; promotion to log identities is a mesh-era follow-on.
- Later bot classes (message-posting, commands, moderation, external API, non-crypto sources) are explicitly out of v1.
