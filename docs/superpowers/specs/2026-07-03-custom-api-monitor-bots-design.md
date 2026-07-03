# Custom API-Source Monitor Bots (v1) — design

**Date:** 2026-07-03
**Status:** design (awaiting owner review)
**Context:** the generalization the owner envisioned for the bots project (see [[project_farder_bots]] — "notify me when RuneScape player count > 100,000" with a user-supplied API link). It **reuses the crypto-ticker + alert engine wholesale**; the only new part is a configurable data source (an owner-supplied API URL + a JSON dot-path to the number).

## Problem

Ticker bots are hardwired to CoinGecko. The owner wants a bot that monitors **any** numeric value from **any** API — a game's player count, a status number, a stat — and alerts on it. The alerting, display, subscribe/DM, and interval are already built and source-agnostic by design; what's missing is: fetch an arbitrary owner-configured URL, pull a number out of the response, and feed it to the existing machinery.

## What already exists (v1 reuses these unchanged)

- **`bots` table** (`public_key, secret_key, kind ('crypto_ticker'), coin_id, label, created_at`) + `is_bot` members + the Bots-panel CRUD.
- **Server poll loop** `bots::spawn_bot_poll_task` — polls each cycle (interval configurable, floor 30s), computes each bot's value, sets its presence, and evaluates alerts.
- **Alert engine** — `evaluate_alert(value, comparator, threshold, armed) -> (fired, new_armed)` (source-agnostic), `bot_alerts`/`bot_subscriptions`, fire-once/re-arm, DM-to-subscribers (`send_bot_dm`). Alert config UI + the 🔔 subscribe toggle + "My subscriptions" all reused.
- **SSRF-guarded server fetch** — `ssrf::resolves_to_global(url)` + the reqwest pattern in `bots::fetch_prices` (http(s)-only, 10s timeout, no user IP exposed). The custom fetch reuses this exactly.
- **Inline value display** — the presence rail (`ticker_presence` renders a value on the bot's name line).

## Goals

1. An owner can add a **custom monitor bot**: `{name, API URL, value dot-path, optional unit}`.
2. The server fetches the URL each poll cycle (SSRF-guarded, owner IP-masked), extracts the number via the dot-path, and displays it inline (e.g. `RuneScape 102,433 players`).
3. Alerts work exactly as for crypto: "value above/below X" → fire-once → DM opt-in subscribers.
4. Failures (unreachable, non-2xx, bad JSON, missing path, non-numeric) degrade to **"unavailable"** and never crash the poll loop.

## Non-goals (v1)

- **Non-numeric / text values** — v1 monitors a single number (alerts need numbers). Text/status strings are display-only, later.
- **JSONPath / regex extraction** — dot-path into JSON only. (Arrays/filters/non-JSON later.)
- **Auth headers** — URL-only (a key can go in the query string). Custom headers later.
- Multiple values per bot; per-bot custom intervals (uses the shared per-server interval); response transforms/math.

## Design

### Data model

Add nullable columns to `bots`: `source_url TEXT`, `value_path TEXT`, `unit TEXT`. Crypto bots leave them NULL; custom bots set them and leave `coin_id` empty (`kind = "custom_api"` is the discriminator). `BotRecord` gains `kind`, `source_url: Option<String>`, `value_path: Option<String>`, `unit: Option<String>`; `list_bots` selects them. A `bots::register_custom_bot(conn, pk, secret, name, source_url, value_path, unit)` inserts `kind='custom_api'`.

### Fetch + extract

- `bots::fetch_json(url) -> Result<serde_json::Value>` — reuse the `fetch_prices` reqwest+SSRF pattern: `ssrf::resolves_to_global(url)` first (http(s)-only, refuse non-global), 10s timeout, redirect policy none, **response-size cap** (e.g. 256 KiB), `serde_json::from_str`.
- `bots::extract_dot_path(&Value, path) -> Option<f64>` — split `path` on `.`, walk objects; the leaf must be a JSON number (or a numeric string) → `f64`. Missing/non-numeric → `None`. Pure, unit-tested.

### Poller branching

In the poll loop, branch per bot by `kind`:
- **`crypto_ticker`** — unchanged (coalesced CoinGecko `fetch_prices`, `PriceInfo`).
- **`custom_api`** — per bot: `fetch_json(source_url)` → `extract_dot_path(value_path)` → `Option<f64>`. `Some(v)` → set a value presence + evaluate alerts; `None`/fetch-error → `unavailable` presence (keep last value; log; don't crash).

Each custom bot is one independent fetch per cycle (crypto still coalesces). The existing per-server interval (floor 30s) bounds frequency; N custom bots = N fetches/cycle.

### Display

`custom_value_presence(value: f64, unit: Option<&str>)` → the value formatted with thousands separators + the unit (e.g. `102,433 players`), rendered inline via the same presence rail as the ticker. `unavailable` → the existing "unavailable" presence style.

### Alerts (reused)

A custom bot's alert metric is the extracted **value**. The alert engine is unchanged: the poll loop evaluates each alert with `evaluate_alert(current_value, comparator, threshold, armed)`. For crypto the current value is `price_usd`/`change_24h`; for custom it is the extracted number (metric key `"value"`). Everything downstream — `bot_alerts`, fire-once/re-arm, `bot_subscriptions`, `send_bot_dm`, the 🔔 toggle, the alert-config UI, "My subscriptions" — is the exact code shipped for crypto alerts. The alert message uses the bot's name + value + unit.

### Config UI

In `BotsTab`, alongside "Add Ticker Bot," an **"Add Custom Monitor"** form (owner-gated): **name**, **API URL**, **value path**, **optional unit** → a new `AddCustomBot{name, source_url, value_path, unit}` request/command. The bot then appears in the list; alerts + subscriptions use the existing per-bot UI. Remove uses the existing `remove_bot`.

### Security

Owner-only creation (MANAGE_SERVER). The fetch is server-side (owner/user IPs never exposed to the target API) and **SSRF-guarded** (`resolves_to_global` refuses localhost/private/link-local, incl. the v4-mapped/6to4/NAT64 bypasses already covered), **http(s)-only**, 10s timeout, 256 KiB response cap. A malicious/typo URL degrades to "unavailable"; it cannot reach internal services or exfiltrate. The `source_url` may contain an embedded API key (owner's choice); stored plaintext server-side (low-stakes, owner-managed, same as bot keys) — do not log it.

## Testing

- **`extract_dot_path` (unit):** `players` / `data.online.count` resolve; numeric string coerces; missing path, non-numeric leaf, non-object mid-path → `None`; deep nesting.
- **`custom_value_presence` (unit):** formats value+unit (thousands sep); no unit → value only.
- **Alert reuse (unit):** `evaluate_alert` already tested; add a poll-branch test that a custom bot's extracted value drives fire/re-arm (inject a fake value; no network).
- **CRUD:** `register_custom_bot` roundtrips kind/source_url/value_path/unit; `AddCustomBot` owner-gated; the bot lists + removes.
- **Fetch (`fetch_json`):** network — not unit-tested (SSRF-guarded, like `fetch_prices`); the SSRF refusal is covered by the existing ssrf tests.
- **Client:** `cargo build` + `tsc`. Runtime is the owner's Windows test.

## Owner runtime verification (server changed → sidecar rebuild)

Add a Custom Monitor bot with a real public JSON API (e.g. a RuneScape/OSRS player-count endpoint or `https://api.github.com/repos/<o>/<r>` with path `stargazers_count`), value path, unit → within a poll cycle its name line shows the value + unit; a bad URL/path shows "unavailable"; add an alert "value above/below X", subscribe (🔔), and confirm a DM fires once on crossing. SSRF: a `http://127.0.0.1/...` or `http://localhost/...` URL is refused (stays "unavailable").

## Decomposition (for the plan)

1. **Fetch + extract + value presence:** `fetch_json` (SSRF-guarded, size-capped), `extract_dot_path` (TDD), `custom_value_presence` (TDD).
2. **Data model + poller branching:** `bots` columns + `BotRecord`/`list_bots` + `register_custom_bot`; poll-loop `kind` branch → value + presence + alert eval (metric `"value"`).
3. **CRUD + client:** `AddCustomBot` request/handler (owner-gated) + Tauri command + bridge; the BotsTab "Add Custom Monitor" form.
4. **Docs.**

## Carry-forward / known limitations

- Numeric single value, dot-path JSON, URL-only auth (all listed non-goals) — the natural v1.1 extensions (headers, JSONPath, text values, per-bot interval).
- Each custom bot is an independent outbound fetch per cycle; many custom bots × short interval = more outbound load (bounded by the ≥30s interval; a future concern if abused).
- Reuses the crypto alert engine entirely — no new alert/subscribe/DM code.
