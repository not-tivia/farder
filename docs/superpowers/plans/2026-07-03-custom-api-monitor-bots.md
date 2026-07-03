# Custom API-Source Monitor Bots (v1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A bot that monitors any numeric value from an owner-supplied API URL (JSON dot-path), displays it, and alerts on it — reusing the crypto alert/subscribe/DM engine.

**Architecture:** Add `kind='custom_api'` bots with `{source_url, value_path, unit}`. The poll loop branches by kind: crypto keeps the coalesced CoinGecko path; custom fetches its URL (SSRF-guarded) + extracts a number via dot-path. Both feed a shared alert-eval-and-DM helper (metric `"value"` for custom, `price_usd`/`change_24h` for crypto).

**Tech Stack:** Rust (`farder-server`, `farder-protocol`), rusqlite, reqwest + serde_json, tokio, Tauri, React/TS.

## Global Constraints

- **Verify-before-done:** frontend↔backend seam + real behavior need the owner's Windows test; server changed → sidecar rebuild.
- **Reuse, don't rebuild:** the alert engine (`evaluate_alert`, `bot_alerts`, `bot_subscriptions`, `send_bot_dm`, the 🔔 toggle, "My subscriptions", the per-server interval) is used unchanged. Do NOT add new alert/subscribe/DM code.
- **Privacy + SSRF:** the custom fetch is server-side (owner/user IPs never exposed) and MUST go through `ssrf::resolves_to_global(url)` (http(s)-only, refuse non-global) with a 10s timeout + response-size cap (256 KiB). Failures degrade to "unavailable" — never crash the poll loop.
- **Owner-gated:** `AddCustomBot` gates on `MANAGE_SERVER` (mirror `AddBot`).
- **v1 scope:** numeric single value; dot-path JSON only; URL-only auth (no headers). `source_url` may embed a key — stored plaintext, never logged.
- **No DB lock (`std::sync::Mutex`) across `.await`** in the poll loop (existing discipline).
- **Build/test:** `cargo test -p farder-server`; `cargo build --workspace`; `cd client/src-tauri && cargo build`; `cd client && npx tsc --noEmit`.

---

### Task 1: fetch_json + extract_dot_path + custom_value_presence

**Files:** Modify `crates/farder-server/src/bots.rs` (+ tests).

**Interfaces:**
- Produces: `bots::fetch_json(url: &str) -> anyhow::Result<serde_json::Value>` (async, SSRF-guarded); `bots::extract_dot_path(v: &serde_json::Value, path: &str) -> Option<f64>`; `bots::custom_value_presence(value: f64, unit: Option<&str>) -> Presence`.

- [ ] **Step 1: Failing tests (pure fns)**

```rust
    #[test]
    fn extract_dot_path_walks_and_coerces() {
        let v: serde_json::Value = serde_json::from_str(r#"{"data":{"online":{"count":102433}},"n":"42","x":{"y":true}}"#).unwrap();
        assert_eq!(extract_dot_path(&v, "data.online.count"), Some(102433.0));
        assert_eq!(extract_dot_path(&v, "n"), Some(42.0));            // numeric string coerces
        assert_eq!(extract_dot_path(&v, "missing"), None);
        assert_eq!(extract_dot_path(&v, "data.nope.count"), None);    // missing mid-path
        assert_eq!(extract_dot_path(&v, "x.y"), None);                // non-numeric leaf
        assert_eq!(extract_dot_path(&v, ""), None);
    }
    #[test]
    fn custom_value_presence_formats() {
        assert_eq!(custom_value_presence(102433.0, Some("players")).details, "102,433 players");
        assert_eq!(custom_value_presence(1234.5, None).details, "1234.50");
        assert_eq!(custom_value_presence(42.0, Some("")).details, "42");
    }
```

Run: `cargo test -p farder-server extract_dot_path custom_value_presence 2>&1 | tail` → FAIL.

- [ ] **Step 2: Implement**

```rust
/// Walk a dot-path into a JSON value; the leaf must be a number (or numeric string) -> f64.
pub fn extract_dot_path(v: &serde_json::Value, path: &str) -> Option<f64> {
    if path.is_empty() { return None; }
    let mut cur = v;
    for seg in path.split('.') {
        if seg.is_empty() { return None; }
        cur = cur.get(seg)?;
    }
    match cur {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn format_thousands(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        let n = v as i64;
        let s = n.unsigned_abs().to_string();
        let mut out = String::new();
        for (i, ch) in s.chars().enumerate() {
            if i > 0 && (s.len() - i) % 3 == 0 { out.push(','); }
            out.push(ch);
        }
        if n < 0 { format!("-{out}") } else { out }
    } else {
        format!("{v:.2}")
    }
}

/// Inline display for a custom monitor bot: "<value> <unit>".
pub fn custom_value_presence(value: f64, unit: Option<&str>) -> Presence {
    let num = format_thousands(value);
    let details = match unit { Some(u) if !u.is_empty() => format!("{num} {u}"), _ => num };
    Presence { kind: PresenceKind::Ticker, details, state: None }
}

/// Fetch + parse an owner-supplied API URL as JSON. Server-side, SSRF-guarded.
pub async fn fetch_json(url: &str) -> anyhow::Result<serde_json::Value> {
    if !crate::ssrf::resolves_to_global(url).await {
        anyhow::bail!("url did not resolve to a global address");
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let resp = client.get(url)
        .header("accept", "application/json")
        .header("user-agent", "farder-bot/1.0")
        .send().await?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() { anyhow::bail!("fetch returned {}", status); }
    if body.len() > 256 * 1024 { anyhow::bail!("response too large"); }
    Ok(serde_json::from_str(&body)?)
}
```

> Implementer note: match the reqwest pattern already in `fetch_prices` (same file) — confirm `reqwest::redirect::Policy` path etc. The 256 KiB cap is post-read (soft); acceptable for v1 (owner-only, SSRF-guarded) — note it.

Run: `cargo test -p farder-server extract_dot_path custom_value_presence 2>&1 | tail` → PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/farder-server/src/bots.rs
git commit -m "feat(custom-bots): fetch_json (SSRF-guarded) + dot-path extract + value presence"
```

---

### Task 2: data model + poller kind-branch (shared alert helper)

**Files:** Modify `crates/farder-server/src/db.rs` (bots columns), `crates/farder-server/src/bots.rs` (`BotRecord`/`list_bots`/`register_custom_bot`; poll-loop restructure; `format_custom_alert_message`).

**Interfaces:**
- Produces: `BotRecord` gains `kind: String, source_url: Option<String>, value_path: Option<String>, unit: Option<String>`; `bots::register_custom_bot(conn, pk, secret, name, source_url, value_path, unit) -> Result<()>`; `bots::format_custom_alert_message(label, comparator, threshold, value, unit) -> String`.
- Consumes: `fetch_json`, `extract_dot_path`, `custom_value_presence` (Task 1); the existing alert helpers (`list_alerts_for_bot`, `evaluate_alert`, `set_alert_armed`, `list_subscribers_for_bot`, `send_bot_dm`, `format_alert_message`, `unknown_coin_presence`, `metric_value`).

- [ ] **Step 1: Schema columns**

In `db.rs`, after the `bots` table create, add nullable columns (guarded):

```rust
    for col in ["source_url", "value_path", "unit"] {
        let has = {
            let mut stmt = conn.prepare("PRAGMA table_info(bots)")?;
            let cols: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(1))?.filter_map(|r| r.ok()).collect();
            cols.iter().any(|c| c == col)
        };
        if !has { conn.execute(&format!("ALTER TABLE bots ADD COLUMN {col} TEXT"), [])?; }
    }
```

- [ ] **Step 2: `BotRecord` + `list_bots` + `register_custom_bot`**

Extend `BotRecord` with `pub kind: String, pub source_url: Option<String>, pub value_path: Option<String>, pub unit: Option<String>`. Update `list_bots` SELECT to `SELECT public_key, coin_id, label, kind, source_url, value_path, unit FROM bots` and read them (kind at idx 3, source_url 4, value_path 5, unit 6). Add:

```rust
pub fn register_custom_bot(conn: &Connection, pk: &PublicKey, secret: &[u8], name: &str, source_url: &str, value_path: &str, unit: Option<&str>) -> Result<()> {
    conn.execute(
        "INSERT INTO bots (public_key, secret_key, kind, coin_id, label, source_url, value_path, unit, created_at) \
         VALUES (?1, ?2, 'custom_api', '', ?3, ?4, ?5, ?6, ?7)",
        params![pk.as_bytes().as_slice(), secret, name, source_url, value_path, unit, crate::db::now() as i64],
    )?;
    Ok(())
}
```

Also set `kind: "crypto_ticker".into()` (and the three `None`s) in the existing `register_bot` construction path if the compiler flags `BotRecord` literals; `register_bot`'s INSERT already sets kind='crypto_ticker'.

- [ ] **Step 3: `format_custom_alert_message` + failing test**

```rust
    #[test]
    fn custom_alert_message_formats() {
        let m = format_custom_alert_message("RuneScape", "above", 100000.0, 102433.0, Some("players"));
        assert!(m.contains("RuneScape") && m.contains("above") && m.contains("102,433") && m.contains("players"));
    }
```

```rust
pub fn format_custom_alert_message(label: &str, comparator: &str, threshold: f64, value: f64, unit: Option<&str>) -> String {
    let u = unit.filter(|s| !s.is_empty()).map(|s| format!(" {s}")).unwrap_or_default();
    format!("\u{1F514} {label} {} {}{u} \u{2014} now {}{u}",
        if comparator == "above" { "crossed above" } else { "crossed below" },
        format_thousands(threshold), format_thousands(value))
}
```

- [ ] **Step 4: Restructure the poll loop (branch by kind, shared alert helper)**

Add a shared helper that does alert-eval + DM given a per-bot metric lookup + a fire-message builder (both branches call it; DB lock never held across the DM await):

```rust
async fn eval_and_notify_alerts(
    state: &Arc<ServerState>,
    bot: &BotRecord,
    metrics: &[(&str, f64)],
    make_message: impl Fn(&str, &str, f64) -> String, // (metric, comparator, threshold) -> text
) {
    let fires: Vec<(String, String, f64)> = {
        let conn = state.db.lock().unwrap();
        let alerts = list_alerts_for_bot(&conn, &bot.public_key).unwrap_or_default();
        let mut fired = Vec::new();
        for a in &alerts {
            if let Some((_, v)) = metrics.iter().find(|(m, _)| *m == a.metric) {
                let (did_fire, new_armed) = evaluate_alert(*v, &a.comparator, a.threshold, a.armed);
                if new_armed != a.armed { let _ = set_alert_armed(&conn, a.id, new_armed); }
                if did_fire { fired.push((a.metric.clone(), a.comparator.clone(), a.threshold)); }
            }
        }
        fired
    };
    if fires.is_empty() { return; }
    let subscribers = { let conn = state.db.lock().unwrap(); list_subscribers_for_bot(&conn, &bot.public_key).unwrap_or_default() };
    for (metric, comparator, threshold) in &fires {
        let text = make_message(metric, comparator, *threshold);
        for sub in &subscribers {
            if let Err(e) = send_bot_dm(state, &bot.public_key, sub, &text).await {
                tracing::warn!(error = %e, "bot alert DM failed");
            }
        }
    }
}
```

Then rewrite the per-bot handling in `spawn_bot_poll_task`. Partition: crypto bots use the coalesced `fetch_prices`; custom bots each `fetch_json`+extract. Replace the current `for b in &bots { ... }` block with:

```rust
                // Coalesce crypto coin ids only.
                let crypto_ids: Vec<String> = bots.iter().filter(|b| b.kind == "crypto_ticker").map(|b| b.coin_id.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                let prices = if crypto_ids.is_empty() { std::collections::HashMap::new() }
                    else { match fetch_prices(&crypto_ids).await { Ok(p) => p, Err(e) => { tracing::warn!(error=%e, "crypto fetch failed; keeping last"); std::collections::HashMap::new() } } };

                for b in &bots {
                    let presence: Presence;
                    // Compute presence + evaluate alerts per kind.
                    if b.kind == "custom_api" {
                        let value: Option<f64> = match (&b.source_url, &b.value_path) {
                            (Some(url), Some(path)) => match fetch_json(url).await {
                                Ok(v) => extract_dot_path(&v, path),
                                Err(e) => { tracing::warn!(error=%e, bot=%b.label, "custom bot fetch failed"); None }
                            },
                            _ => None,
                        };
                        presence = match value {
                            Some(v) => custom_value_presence(v, b.unit.as_deref()),
                            None => unknown_coin_presence(), // reuse the "unavailable" style; consider a rename if desired
                        };
                        broadcast_presence(state.clone(), b, presence.clone()).await;   // see note
                        if let Some(v) = value {
                            let unit = b.unit.clone();
                            eval_and_notify_alerts(&state, b, &[("value", v)],
                                |_, comp, thr| format_custom_alert_message(&b.label, comp, thr, v, unit.as_deref())).await;
                        }
                    } else {
                        // crypto_ticker (unchanged behavior)
                        let pi = prices.get(&b.coin_id);
                        presence = match pi { Some(pi) => ticker_presence(pi), None => unknown_coin_presence() };
                        broadcast_presence(state.clone(), b, presence.clone()).await;
                        if let Some(pi) = pi {
                            eval_and_notify_alerts(&state, b, &[("price_usd", pi.usd), ("change_24h", pi.change_24h)],
                                |m, comp, thr| format_alert_message(&b.label, m, comp, thr, pi)).await;
                        }
                    }
                }
```

Add the tiny presence-broadcast helper (extracted from the current inline code, to DRY):

```rust
async fn broadcast_presence(state: Arc<ServerState>, bot: &BotRecord, presence: Presence) {
    { state.presences.write().unwrap().insert(*bot.public_key.as_bytes(), presence.clone()); }
    crate::connection::broadcast_event(&state, crate::events::EventTarget::All,
        ServerEvent::MemberPresenceUpdated { public_key: bot.public_key.clone(), presence: Some(presence) }).await;
}
```

> Implementer notes: (1) preserve the existing "unavailable" behaviour — you may keep `unknown_coin_presence` for custom-bot failures or add a `unavailable_presence()` alias returning `details: "unavailable"`; do NOT leave a custom bot blank. (2) the `make_message` closures capture `pi`/`v`/`unit` — they're called within the same awaited task (not spawned), so lifetimes are local; if the borrow checker objects to capturing `pi` across the await, bind the needed values (`let usd = pi.usd; let chg = pi.change_24h;`) before the closure. (3) do NOT hold `state.db.lock()` across any `.await` (the helper scopes each lock). (4) confirm `Presence`/`ServerEvent`/`EventTarget` import paths match the existing loop.

- [ ] **Step 5: Build + tests**

Run: `cargo test -p farder-server 2>&1 | tail` (all pass incl. the new custom tests + the existing alert tests) AND `cargo build --workspace 2>&1 | tail` (clean).

- [ ] **Step 6: Commit**

```bash
git add crates/farder-server/src/db.rs crates/farder-server/src/bots.rs
git commit -m "feat(custom-bots): bots source columns + poll-loop kind branch + shared alert helper"
```

---

### Task 3: CRUD request/handler + client (Add Custom Monitor)

**Files:** `crates/farder-protocol/src/server.rs` (`AddCustomBot`), `crates/farder-server/src/handlers.rs` (arm), `client/src-tauri/src/commands.rs` + `main.rs`, `client/src/lib/tauri-bridge.ts`, `client/src/components/BotsTab.tsx`.

**Interfaces:**
- Produces: `ServerRequest::AddCustomBot { name: String, source_url: String, value_path: String, unit: Option<String> }`; Tauri `add_custom_bot(server_id, name, source_url, value_path, unit)`; bridge `addCustomBot(...)`.

- [ ] **Step 1: Request + handler**

Add the `ServerRequest::AddCustomBot` variant. Handler arm (mirror the `AddBot` arm — owner-gated `MANAGE_SERVER`, generate a keypair, `members::register_bot_member` + `bots::register_custom_bot`, broadcast `MemberJoined`):

```rust
        ServerRequest::AddCustomBot { name, source_url, value_path, unit } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            let name = name.trim().to_string();
            let source_url = source_url.trim().to_string();
            let value_path = value_path.trim().to_string();
            if name.is_empty() || name.len() > 64 { return err("bot name must be 1-64 characters"); }
            if !(source_url.starts_with("http://") || source_url.starts_with("https://")) || source_url.len() > 2048 {
                return err("source url must be http(s) and <= 2048 chars");
            }
            if value_path.is_empty() || value_path.len() > 256 { return err("value path required (<=256 chars)"); }
            let unit = unit.map(|u| u.trim().chars().take(24).collect::<String>()).filter(|u| !u.is_empty());
            let kp = farder_crypto::identity::Keypair::generate();
            let pk = kp.public_key();
            crate::members::register_bot_member(conn, &pk, &name)?;
            crate::bots::register_custom_bot(conn, &pk, &kp.signing_key_bytes(), &name, &source_url, &value_path, unit.as_deref())?;
            ok_with(ServerResponse::Ok, vec![BroadcastEvent { target: EventTarget::All, event: ServerEvent::MemberJoined { public_key: pk, display_name: name } }])
        }
```

> Implementer note: match the real `AddBot` arm helpers/names (`register_bot_member`, `signing_key_bytes`, `ok_with`, `BroadcastEvent`, `EventTarget::All`). Basic SSRF happens at fetch time (`fetch_json`); the handler only does cheap syntactic validation (http(s), lengths).

- [ ] **Step 2: Tauri command + bridge**

`commands.rs` (mirror `add_bot`): `add_custom_bot(server_id, name, source_url, value_path, unit: Option<String>) -> ()` → `ServerRequest::AddCustomBot{..}` → `ServerResponse::Ok`. Register in `generate_handler!`. Bridge `addCustomBot(serverId, name, sourceUrl, valuePath, unit)` (camelCase; `unit` optional → `unit ?? null`).

- [ ] **Step 3: BotsTab "Add Custom Monitor" form**

In `BotsTab.tsx`, below the "Add Ticker Bot" section, add an "Add Custom Monitor" section (reuse `connect-*`/`xp-button`): inputs for **name**, **API URL**, **value path** (placeholder e.g. `data.players`), **unit** (optional); an Add button → `api.addCustomBot(serverId, name, url, path, unit || null)`; on error show it (reuse the existing `error-text`). Custom bots then appear in the same bot list (already `activeServer.members.filter(is_bot)`), and alerts/subscribe use the existing per-bot UI unchanged.

- [ ] **Step 4: Build + seam + tsc + commit**

Run: `cargo build --workspace 2>&1 | tail`; `cd client/src-tauri && cargo build 2>&1 | tail`; `grep -n 'add_custom_bot' client/src-tauri/src/main.rs`; `cd client && npx tsc --noEmit 2>&1 | tail`.

```bash
git add crates/farder-protocol/src/server.rs crates/farder-server/src/handlers.rs client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src/lib/tauri-bridge.ts client/src/components/BotsTab.tsx
git commit -m "feat(custom-bots): AddCustomBot request/handler + Add Custom Monitor UI"
```

---

### Task 4: Docs

- [ ] Update `docs/modules/tauri-commands.md` (`add_custom_bot`), the bridge doc (`addCustomBot`), `docs/modules/protocol.md` (`AddCustomBot`, the `bots` `source_url`/`value_path`/`unit` columns), a server doc (custom-bot kind branch in the poller, `fetch_json`/`extract_dot_path`/`custom_value_presence`, the shared `eval_and_notify_alerts` helper, alert metric `"value"`), and `ARCHITECTURE.md` (custom monitor bot data path). Commit `docs(custom-bots): API-source monitor bots`.

---

## Owner runtime verification (server changed → sidecar rebuild)

`git pull` → `cargo build -p farder-server` → sidecar copy → `npm run tauri dev` → Ctrl+Shift+R. Bots → **Add Custom Monitor**: name, a real public JSON API (e.g. `https://api.github.com/repos/rust-lang/rust` path `stargazers_count`, unit `stars`) → within a poll cycle the bot's name line shows the value + unit; a bad URL/path shows "unavailable". Add an alert "value above/below X", 🔔 subscribe → a DM fires once on a crossing. SSRF: a `http://127.0.0.1/` URL stays "unavailable" (refused).

## Self-review notes

- Spec "custom bot {name, url, dot-path, unit}, kind=custom_api" → Task 2 (schema + register_custom_bot).
- Spec "fetch SSRF-guarded + dot-path extract + unavailable on failure" → Task 1 + Task 2 branch.
- Spec "display value+unit inline" → Task 1 (`custom_value_presence`) + Task 2 broadcast.
- Spec "alerts fully reused, metric=value" → Task 2 (`eval_and_notify_alerts` with `[("value", v)]`; existing engine/subscribe/DM untouched).
- Spec "Add Custom Monitor UI, owner-gated" → Task 3.
- Spec security (owner-only, SSRF, http(s), timeout, size cap, degrade-to-unavailable) → Task 1 (`fetch_json`) + Task 3 (gate + syntactic validation).
- Deferred (no tasks): text values, JSONPath/regex, auth headers, per-bot interval. Correct.
