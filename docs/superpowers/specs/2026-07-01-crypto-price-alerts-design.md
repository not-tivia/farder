# Crypto Price Alerts (v1) — design

**Date:** 2026-07-01
**Status:** design (awaiting owner review)
**Context:** the next slice of the bots project (see [[project_farder_bots]]). This is the **first feature where a bot posts a message** (as a DM), which also lays the foundation for webhooks and command bots. It is deliberately built on a **source-agnostic alert engine** so the future "custom API-source monitor" bot (owner's RuneScape-player-count idea) reuses it unchanged — only the data source differs.

## Problem

Ticker bots show a live price but can't tell you when something *happens*. The owner wants **opt-in, quiet alerts**: "DM me when BTC crosses $70k / drops 5%", without spamming a channel or forcing alerts on people who don't want them. Bots today only update their presence (status) — they cannot post or DM. This feature adds that.

## What already exists (v1 builds on this)

- **Ticker bots** (`bots` table, `is_bot` members, server-held keypair) + the **poll loop** (`bots::spawn_bot_poll_task`) that already fetches each coin's `usd` price and `usd_24h_change` every cycle and holds a per-bot `PriceInfo { usd, change_24h }`.
- **DM crypto, server-usable:** `farder_crypto::key_exchange::derive_dm_shared_secret(our_ed_sk, their_ed_pk)` (shared crate) — symmetric X25519 ECDH from an Ed25519 key. The server holds the bot's Ed25519 key and each member's public key, so it can derive the bot↔subscriber shared secret and AES-GCM-encrypt a DM on the bot's behalf; the recipient decrypts normally.
- **DM plumbing:** `ServerRequest::OpenDm{target_key}` → `ServerEvent::DmCreated{channel, participant}`; `messages::insert_message`; `connection::broadcast_event`.
- **Bot management:** the owner-gated `BotsTab` + `AddBot`/`RemoveBot`; the async poll loop already broadcasts to clients.

## Goals

1. A server owner can attach **alerts** to a ticker bot: an absolute price level (above/below $X) and/or a 24h-% move (≥ +X% / ≤ −X%).
2. Any member can **opt in** to a bot's alerts (and see/manage their subscriptions).
3. When an alert trips, the bot **DMs each subscriber** once (fire-once, re-arm on recovery) — quiet, personal, persistent.
4. The alert engine is **source-agnostic** (metric + comparator + threshold), so future non-crypto/custom-API bots reuse it.

## Non-goals (v1)

- **Per-user personal thresholds** (each user their own number) — v1 is owner-defined shared alerts + member opt-in. (Natural later upgrade.)
- **Channel/role delivery** — v1 delivers only by DM. (Channel posting is the webhooks project.)
- **Custom / user-supplied API data sources** — the next bot project (has its own SSRF + extraction design). v1's source is CoinGecko.
- Alert history/log, digests/batching, per-alert (vs per-bot) subscriptions, cross-server subscription view.

## Design

### Alert engine (source-agnostic core)

An alert is `{ metric, comparator, threshold, armed }`:
- `metric`: a string key — v1: `"price_usd"` or `"change_24h"`. (Future bots add keys.)
- `comparator`: `"above"` (>) or `"below"` (<).
- `threshold`: `f64`.
- `armed`: `bool` — persisted, drives fire-once/re-arm.

Each poll cycle, after the bot's value(s) are known, for each alert: read `value = metric_of(bot)`; `condition = comparator(value, threshold)`. If `armed && condition` → **fire** (see delivery), set `armed = false`. If `!armed && !condition` → **re-arm** (`armed = true`). So one DM per crossing; it re-arms only when the value recovers past the line. `armed` starts true.

- **Absolute level:** `metric="price_usd"`, e.g. `above 70000` or `below 60000`.
- **24h %:** `metric="change_24h"`, e.g. `above 5` (up ≥5%) or `below -5` (down ≥5%). Uses the `usd_24h_change` already fetched — no extra state, no "since when" ambiguity.

Because it is purely "a number vs a threshold with hysteresis", the same engine + evaluator serves any future metric; only "how the value is fetched" changes.

### Who defines alerts vs who receives them

- **Owner defines alerts** on a bot (owner-gated, in the Bots panel), stored in `bot_alerts`.
- **Members opt in** per bot (`bot_subscriptions`); only subscribers get DMs. Anyone may subscribe/unsubscribe themselves.

### Delivery — quiet DM (the new "bot posts a message" capability)

When an alert fires, for each subscriber the server, on the bot's behalf:
1. Ensures a **DM channel** exists between the bot and the subscriber (reuse the `OpenDm`/`DmCreated` path; create if absent).
2. Derives the shared secret via `derive_dm_shared_secret(bot_ed_sk, subscriber_ed_pk)`, **AES-GCM-encrypts** the alert text (e.g. *"🔔 BTC crossed above $70,000 — now $70,142"* / *"🔔 BTC is down 6.2% in 24h — now $58,900"*), inserts it as a message in that DM channel, and broadcasts so the subscriber's client shows it.
3. The subscriber's client decrypts with its own key (symmetric ECDH) — no client change to the decrypt path.

This is the **riskiest piece** (first server-side bot-initiated E2EE DM). **The implementation plan verifies this full path — derive → encrypt → store in a bot↔user DM channel → subscriber decrypts + sees it — as its FIRST task (a spike)**, before building the alert config/subscription surface on top. If the DM-channel bootstrap for a bot proves impractical, we revisit delivery before investing in the rest.

Rate/abuse: an alert fires at most once per crossing per subscriber; the poll interval (≥30s) bounds frequency; a fired alert re-arms only on recovery.

### Evaluation

Folds into the existing poll loop: after `fetch_prices` succeeds and each bot's `PriceInfo` is known, load that bot's alerts, evaluate + update `armed` (persist), and for any that fire, DM its subscribers. Alerts are only evaluated on a **successful** fetch (a fetch error changes nothing — matches the "unknown coin" discipline).

### Subscriptions + UI

- **Per-bot toggle:** a **"🔔 Notify me" / "🔕 Unsubscribe"** action on a bot (member-facing, via the bot's context menu in the member list — bots already suppress human-only actions, so this is the bot-specific action). Calls `subscribe_bot`/`unsubscribe_bot`.
- **"My alert subscriptions" view:** a per-user list (this server) of bots you're subscribed to, each with one-click unsubscribe. (A section in user/server settings; per-server for v1.)

### Alert-config UI (owner)

In `BotsTab`, under each bot, an **Alerts** sub-section: add an alert (metric dropdown **Price** / **24h change**; comparator **above** / **below**; a value field), list existing alerts, remove. Owner-gated (server config).

### Data model

- `bot_alerts (id, bot_public_key, metric TEXT, comparator TEXT, threshold REAL, armed INTEGER NOT NULL DEFAULT 1, created_at)`.
- `bot_subscriptions (bot_public_key, subscriber_public_key, created_at, PRIMARY KEY(bot_public_key, subscriber_public_key))`.

Both keyed to the bot's public key (cascade-delete when a bot is removed).

## Protocol / commands (surface)

- `ServerRequest`: `AddBotAlert{bot_public_key, metric, comparator, threshold}`, `RemoveBotAlert{alert_id}`, `ListBotAlerts{bot_public_key}` (owner-gated writes); `SubscribeBot{bot_public_key}` / `UnsubscribeBot{bot_public_key}` / `ListMySubscriptions` (any member).
- `ServerResponse`: `BotAlerts{alerts}`, `MySubscriptions{bot_public_keys}` (+ `Ok`).
- Tauri commands + bridge mirror each; the plan pins exact shapes.

## Testing

- **Alert engine (unit):** fire-once (armed→fire→disarm), re-arm only on recovery, above/below for both metrics, no fire while disarmed; a fetch error evaluates nothing. Pure `(value, comparator, threshold, armed) -> (fired, new_armed)`.
- **DM spike (Task 1):** a server-composed bot→user DM is decryptable by the recipient's key (round-trip test in `farder-crypto`/server using `derive_dm_shared_secret` both directions).
- **CRUD:** add/remove/list alerts (owner-gated; non-owner rejected); subscribe/unsubscribe/list (any member); removing a bot cascades its alerts + subscriptions.
- **Client:** `cargo build` + `tsc`. Runtime (owner Windows) is the real gate.

## Owner runtime verification (server changed → sidecar rebuild)

Add a BTC bot → add an alert "Price below $<slightly above current>" → **you** subscribe (🔔) → within a poll cycle you get a **DM** from the bot; it fires once (not every cycle); raise the price target above current and confirm it re-arms + fires again on the next crossing. A member who didn't subscribe gets nothing. Check "My alert subscriptions" lists the bot and unsubscribe stops the DMs.

## Decomposition (for the plan)

1. **DM delivery spike** — server-side bot→user E2EE DM (derive + encrypt + DM-channel bootstrap + insert + broadcast), proven by a decrypt round-trip. *(Do first — highest risk.)*
2. **Alert engine + data model** — `bot_alerts`/`bot_subscriptions` tables, the pure evaluator (fire/re-arm), wired into the poll loop to DM subscribers on fire.
3. **Server CRUD** — Add/Remove/List alerts (owner-gated) + Subscribe/Unsubscribe/ListMySubscriptions requests/handlers.
4. **Client** — Alerts sub-section in BotsTab (owner), the per-bot 🔔 toggle, the "My subscriptions" view, commands + bridge.
5. **Docs.**

## Carry-forward / known limitations

- v1 = owner-defined shared alerts + member opt-in (not per-user thresholds).
- DM-only delivery (channel/role posting = webhooks project).
- CoinGecko-only metrics; the engine is generic so a **custom API-source bot** (next project: user URL + JSON value path + SSRF/extraction design) feeds the same engine.
- No alert history, no batching/digest, per-bot (not per-alert) subscription granularity.
- The bot→DM path is the novel/risky piece — verified first.
