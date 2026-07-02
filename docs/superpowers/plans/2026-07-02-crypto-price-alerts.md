# Crypto Price Alerts (v1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Owner-defined price alerts on ticker bots that DM opt-in subscribers when a threshold is crossed — the first feature where a bot posts a message.

**Architecture:** A source-agnostic alert engine (`{metric, comparator, threshold, armed}`, fire-once + re-arm) folded into the existing bot poll loop. On a fire, the server composes an E2EE DM **from the bot** to each subscriber using the bot's server-held key (`derive_dm_shared_secret` + `encryption::encrypt`, both in the shared `farder-crypto` crate), stores it as a normal message in a bot↔user DM channel, and delivers it targeted to the recipient via `EventTarget::Members`.

**Tech Stack:** Rust (`farder-server`, `farder-protocol`, `farder-crypto`), rusqlite, tokio, Tauri, React/TS.

## Global Constraints

- **Verify-before-done:** the bot→DM path and the frontend↔backend seam need the owner's Windows runtime test; mark UNVERIFIED until then.
- **DM delivery target:** bot-DM events (`DmCreated`, `NewMessage`) are delivered with `EventTarget::Members(vec![recipient_pk])` (NOT `Subscribers(channel_id)` — that only reaches clients currently viewing that channel).
- **DM encryption is server-side, on the bot's behalf:** `farder_crypto::key_exchange::derive_dm_shared_secret(our_ed_sk: &[u8;32], their_ed_pk: &[u8;32]) -> Result<[u8;32], &'static str>` then `farder_crypto::encryption::encrypt(key: &[u8;32], plaintext: &[u8]) -> Result<Vec<u8>>`, output `hex::encode`d (matches the client's `dm_decrypt`). Both are in `farder-crypto` (already a `farder-server` dep). The bot's Ed25519 secret is `bots.secret_key` (written by `register_bot` as `signing_key_bytes()`).
- **No DB lock across `.await`:** `state.db` is a `std::sync::Mutex`; do all DB work in a scoped block, drop the guard, THEN await `broadcast_event` (async).
- **Alerts evaluate only on a SUCCESSFUL price fetch** (a fetch error changes nothing — matches the unknown-coin discipline).
- **Owner-gated writes:** alert add/remove use `require_base_perm(..., MANAGE_SERVER, ...)`. Subscribe/unsubscribe/list-my-subscriptions are any-member.
- **Seam rule:** new Tauri commands registered in `generate_handler!` with matching invoke names.
- **Casing:** TS server types snake_case; invoke args camelCase.
- **Build/test:** `cargo test -p farder-server`; `cargo test -p farder-crypto`; `cargo build --workspace`; `cd client && npx tsc --noEmit`.

---

### Task 1: DM delivery spike — server-side bot→user E2EE DM

**Files:**
- Modify: `crates/farder-server/src/bots.rs` (`get_bot_secret`, `encrypt_bot_dm`, `send_bot_dm`; tests)

**Interfaces:**
- Produces: `bots::get_bot_secret(conn, pk: &PublicKey) -> Result<Option<[u8;32]>>`; `bots::encrypt_bot_dm(bot_ed_sk: &[u8;32], recipient_ed_pk: &[u8;32], text: &str) -> Result<String>` (hex ciphertext); `bots::send_bot_dm(state: &Arc<ServerState>, bot_pk: &PublicKey, recipient_pk: &PublicKey, text: &str) -> Result<()>` (async).
- Consumes: `channels::open_dm_channel(conn, a: &PublicKey, b: &PublicKey) -> Result<(u64, bool)>`; `messages::insert_message(conn, channel_id: u64, author: &PublicKey, content: &str, reply_to: Option<u64>) -> Result<u64>`; `messages::get_message(conn, id, viewer: &PublicKey) -> Result<Option<MessageInfo>>`; `connection::broadcast_event(state, EventTarget, ServerEvent)` (async); the OpenDm handler's participant-MemberInfo build (`handlers.rs:1330-1340`).

- [ ] **Step 1: Failing crypto round-trip test**

In `bots.rs` `mod tests`:

```rust
    #[test]
    fn bot_dm_encrypts_so_recipient_decrypts() {
        use farder_crypto::identity::Keypair;
        let bot = Keypair::generate();
        let user = Keypair::generate();
        let hex_ct = encrypt_bot_dm(&bot.signing_key_bytes(), user.public_key().as_bytes(), "hello from BTC bot").unwrap();
        // recipient decrypts with (their sk, bot pk) — symmetric ECDH
        let shared = farder_crypto::key_exchange::derive_dm_shared_secret(
            &user.signing_key_bytes(), bot.public_key().as_bytes()).unwrap();
        let ct = hex::decode(&hex_ct).unwrap();
        let pt = farder_crypto::encryption::decrypt(&shared, &ct).unwrap();
        assert_eq!(String::from_utf8(pt).unwrap(), "hello from BTC bot");
    }
```

Run: `cargo test -p farder-server bot_dm_encrypts 2>&1 | tail` → FAIL (fn absent).

- [ ] **Step 2: Implement `get_bot_secret` + `encrypt_bot_dm`**

```rust
/// The bot's stored Ed25519 secret (32 bytes), if the bot exists.
pub fn get_bot_secret(conn: &Connection, pk: &PublicKey) -> Result<Option<[u8; 32]>> {
    use rusqlite::OptionalExtension;
    let row: Option<Vec<u8>> = conn
        .query_row("SELECT secret_key FROM bots WHERE public_key = ?1", params![pk.as_bytes().as_slice()], |r| r.get(0))
        .optional()?;
    match row {
        Some(bytes) => {
            let arr: [u8; 32] = bytes.try_into().map_err(|_| anyhow::anyhow!("bad bot secret length"))?;
            Ok(Some(arr))
        }
        None => Ok(None),
    }
}

/// Encrypt `text` as a DM FROM the bot TO the recipient, returning hex ciphertext
/// (matching the client `dm_decrypt` format: nonce||ct+tag, hex-encoded).
pub fn encrypt_bot_dm(bot_ed_sk: &[u8; 32], recipient_ed_pk: &[u8; 32], text: &str) -> Result<String> {
    let shared = farder_crypto::key_exchange::derive_dm_shared_secret(bot_ed_sk, recipient_ed_pk)
        .map_err(|e| anyhow::anyhow!("dm key exchange failed: {e}"))?;
    let ct = farder_crypto::encryption::encrypt(&shared, text.as_bytes())?;
    Ok(hex::encode(ct))
}
```

Run: `cargo test -p farder-server bot_dm_encrypts 2>&1 | tail` → PASS.

- [ ] **Step 3: Implement `send_bot_dm` (full path — build-verified, runtime-gated)**

```rust
/// Send an E2EE DM from a server-managed bot to a user, delivered targeted at
/// the recipient (so it reaches them regardless of which channel they're viewing).
pub async fn send_bot_dm(state: &Arc<ServerState>, bot_pk: &PublicKey, recipient_pk: &PublicKey, text: &str) -> Result<()> {
    // --- all DB + crypto work under the lock; collect what the broadcasts need ---
    let (channel_info, was_created, message, bot_member) = {
        let conn = state.db.lock().unwrap();
        let bot_sk = match get_bot_secret(&conn, bot_pk)? { Some(s) => s, None => return Ok(()) };
        let (channel_id, was_created) = crate::channels::open_dm_channel(&conn, bot_pk, recipient_pk)?;
        let hex_ct = encrypt_bot_dm(&bot_sk, recipient_pk.as_bytes(), text)?;
        let msg_id = crate::messages::insert_message(&conn, channel_id, bot_pk, &hex_ct, None)?;
        let message = crate::messages::get_message(&conn, msg_id, recipient_pk)?
            .ok_or_else(|| anyhow::anyhow!("bot dm message vanished"))?;
        // Build the bot's MemberInfo for DmCreated.participant (mirror the OpenDm handler, handlers.rs:1330-1340).
        let channel_info = crate::channels::get_channel(&conn, channel_id)?
            .ok_or_else(|| anyhow::anyhow!("dm channel vanished"))?;
        let bot_member = crate::handlers::build_member_info(&conn, state, bot_pk)?; // see note
        (channel_info, was_created, message, bot_member)
    };
    // --- broadcasts (async), targeted at the recipient only ---
    if was_created {
        crate::connection::broadcast_event(state, EventTarget::Members(vec![recipient_pk.clone()]),
            ServerEvent::DmCreated { channel: channel_info, participant: bot_member }).await;
    }
    crate::connection::broadcast_event(state, EventTarget::Members(vec![recipient_pk.clone()]),
        ServerEvent::NewMessage { message }).await;
    Ok(())
}
```

> Implementer note: there is no shared `build_member_info` helper — the OpenDm handler builds `participant` inline (handlers.rs:1330-1340) reading role_ids/presence/is_bot. Either (a) extract that into `pub fn build_member_info(conn, state, pk) -> Result<MemberInfo>` in handlers.rs and call it from both, or (b) inline the same construction here. Prefer (a) (DRY). Confirm `channels::get_channel` returns a `ChannelInfo`; match the real `DmCreated`/`NewMessage` variant field names and `messages::insert_message`/`get_message` signatures. `EventTarget` is `crate::events::EventTarget`.

- [ ] **Step 4: Build + test**

Run: `cargo test -p farder-server bot_dm 2>&1 | tail` (crypto test passes) AND `cargo build --workspace 2>&1 | tail` (clean). `send_bot_dm`'s full path is runtime-gated (owner test) — the crypto round-trip is the unit proof.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-server/src/bots.rs crates/farder-server/src/handlers.rs
git commit -m "feat(alerts): server-side bot->user E2EE DM (send_bot_dm)"
```

---

### Task 2: Alert engine + data model + poll-loop wiring

**Files:**
- Modify: `crates/farder-server/src/db.rs` (bot_alerts + bot_subscriptions tables)
- Modify: `crates/farder-server/src/bots.rs` (evaluator, metric_value, alert/subscription DB helpers, poll-loop eval)

**Interfaces:**
- Produces: `bots::evaluate_alert(value: f64, comparator: &str, threshold: f64, armed: bool) -> (bool, bool)` (returns `(fired, new_armed)`); `bots::metric_value(p: &PriceInfo, metric: &str) -> Option<f64>`; `bots::AlertRow { id: i64, bot_public_key: PublicKey, metric: String, comparator: String, threshold: f64, armed: bool }`; `bots::list_alerts_for_bot(conn, &PublicKey) -> Result<Vec<AlertRow>>`, `set_alert_armed(conn, id: i64, armed: bool) -> Result<()>`, `list_subscribers_for_bot(conn, &PublicKey) -> Result<Vec<PublicKey>>`.
- Consumes: `send_bot_dm` (Task 1); the existing poll loop + `PriceInfo`.

- [ ] **Step 1: Tables**

In `db.rs` schema:

```rust
        CREATE TABLE IF NOT EXISTS bot_alerts (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            bot_public_key BLOB    NOT NULL,
            metric         TEXT    NOT NULL,
            comparator     TEXT    NOT NULL,
            threshold      REAL    NOT NULL,
            armed          INTEGER NOT NULL DEFAULT 1,
            created_at     INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS bot_subscriptions (
            bot_public_key        BLOB NOT NULL,
            subscriber_public_key BLOB NOT NULL,
            created_at            INTEGER NOT NULL,
            PRIMARY KEY (bot_public_key, subscriber_public_key)
        );
```

- [ ] **Step 2: Failing evaluator + metric tests**

```rust
    #[test]
    fn evaluate_alert_fires_once_and_rearms() {
        // above 70000, armed: fires when value exceeds, disarms; re-arms when back below.
        assert_eq!(evaluate_alert(70001.0, "above", 70000.0, true), (true, false));  // fire, disarm
        assert_eq!(evaluate_alert(70050.0, "above", 70000.0, false), (false, false)); // still over, no re-fire
        assert_eq!(evaluate_alert(69900.0, "above", 70000.0, false), (false, true));  // recovered -> re-arm
        // below -5 (24h %), armed
        assert_eq!(evaluate_alert(-6.0, "below", -5.0, true), (true, false));
        assert_eq!(evaluate_alert(-4.0, "below", -5.0, false), (false, true));
    }

    #[test]
    fn metric_value_maps_keys() {
        let p = PriceInfo { usd: 67432.0, change_24h: 2.1 };
        assert_eq!(metric_value(&p, "price_usd"), Some(67432.0));
        assert_eq!(metric_value(&p, "change_24h"), Some(2.1));
        assert_eq!(metric_value(&p, "bogus"), None);
    }
```

Run: `cargo test -p farder-server evaluate_alert 2>&1 | tail` → FAIL.

- [ ] **Step 3: Implement evaluator + metric + DB helpers**

```rust
/// Fire-once with hysteresis: returns (fired, new_armed).
pub fn evaluate_alert(value: f64, comparator: &str, threshold: f64, armed: bool) -> (bool, bool) {
    let condition = match comparator { "above" => value > threshold, "below" => value < threshold, _ => false };
    if armed && condition { (true, false) }        // fire, disarm
    else if !armed && !condition { (false, true) } // recovered, re-arm
    else { (false, armed) }                        // no change
}

pub fn metric_value(p: &PriceInfo, metric: &str) -> Option<f64> {
    match metric { "price_usd" => Some(p.usd), "change_24h" => Some(p.change_24h), _ => None }
}

#[derive(Clone, Debug)]
pub struct AlertRow { pub id: i64, pub bot_public_key: PublicKey, pub metric: String, pub comparator: String, pub threshold: f64, pub armed: bool }

pub fn list_alerts_for_bot(conn: &Connection, bot: &PublicKey) -> Result<Vec<AlertRow>> {
    let mut stmt = conn.prepare("SELECT id, metric, comparator, threshold, armed FROM bot_alerts WHERE bot_public_key = ?1")?;
    let rows = stmt.query_map(params![bot.as_bytes().as_slice()], |r| Ok((
        r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, f64>(3)?, r.get::<_, i64>(4)? != 0)))?;
    let mut out = Vec::new();
    for row in rows { let (id, metric, comparator, threshold, armed) = row?; out.push(AlertRow { id, bot_public_key: bot.clone(), metric, comparator, threshold, armed }); }
    Ok(out)
}

pub fn set_alert_armed(conn: &Connection, id: i64, armed: bool) -> Result<()> {
    conn.execute("UPDATE bot_alerts SET armed = ?1 WHERE id = ?2", params![armed as i64, id])?; Ok(())
}

pub fn list_subscribers_for_bot(conn: &Connection, bot: &PublicKey) -> Result<Vec<PublicKey>> {
    let mut stmt = conn.prepare("SELECT subscriber_public_key FROM bot_subscriptions WHERE bot_public_key = ?1")?;
    let rows = stmt.query_map(params![bot.as_bytes().as_slice()], |r| r.get::<_, Vec<u8>>(0))?;
    let mut out = Vec::new();
    for row in rows { let b = row?; let arr: [u8;32] = b.try_into().map_err(|_| anyhow::anyhow!("bad subscriber pk"))?; out.push(PublicKey::from_bytes(arr)); }
    Ok(out)
}

/// Human-readable alert message for a fired alert.
pub fn format_alert_message(label: &str, metric: &str, comparator: &str, threshold: f64, p: &PriceInfo) -> String {
    match metric {
        "price_usd" => format!("\u{1F514} {label} crossed {} ${:.2} \u{2014} now ${:.2}",
            if comparator == "above" { "above" } else { "below" }, threshold, p.usd),
        "change_24h" => format!("\u{1F514} {label} 24h change {} {:.1}% \u{2014} now {:+.1}% (${:.2})",
            if comparator == "above" { "above" } else { "below" }, threshold, p.change_24h, p.usd),
        _ => format!("\u{1F514} {label} alert"),
    }
}
```

Run: `cargo test -p farder-server evaluate_alert metric_value 2>&1 | tail` → PASS.

- [ ] **Step 4: Wire into the poll loop**

In `spawn_bot_poll_task`, inside the `Ok(prices)` branch, after presence is set for a bot with a known `PriceInfo pi` (only bots WITH a real price — skip unknown-coin bots for alerts), evaluate alerts. Structure it to do DB work under the lock, then await the DMs:

```rust
                // (inside `for b in &bots`, only when `prices.get(&b.coin_id)` was Some(pi))
                // 1. DB: evaluate alerts, persist armed changes, collect fires + subscribers.
                let fires: Vec<(String, String, f64)> = { // (metric, comparator, threshold) that fired
                    let conn = state.db.lock().unwrap();
                    let alerts = list_alerts_for_bot(&conn, &b.public_key).unwrap_or_default();
                    let mut fired = Vec::new();
                    for a in &alerts {
                        if let Some(v) = metric_value(pi, &a.metric) {
                            let (did_fire, new_armed) = evaluate_alert(v, &a.comparator, a.threshold, a.armed);
                            if new_armed != a.armed { let _ = set_alert_armed(&conn, a.id, new_armed); }
                            if did_fire { fired.push((a.metric.clone(), a.comparator.clone(), a.threshold)); }
                        }
                    }
                    fired
                };
                if !fires.is_empty() {
                    let subscribers = { let conn = state.db.lock().unwrap(); list_subscribers_for_bot(&conn, &b.public_key).unwrap_or_default() };
                    for (metric, comparator, threshold) in &fires {
                        let text = format_alert_message(&b.label, metric, comparator, *threshold, pi);
                        for sub in &subscribers {
                            if let Err(e) = send_bot_dm(&state, &b.public_key, sub, &text).await {
                                tracing::warn!(error = %e, "bot alert DM failed");
                            }
                        }
                    }
                }
```

> Implementer note: `b.label` — `BotRecord` has `label`. Ensure `pi` (the `&PriceInfo` for this bot) is in scope in the branch you add this to; if the current loop shadows it, capture it. Do NOT hold the DB lock across the `send_bot_dm(...).await`.

- [ ] **Step 5: Build + test + commit**

Run: `cargo test -p farder-server 2>&1 | tail` (pass) AND `cargo build --workspace 2>&1 | tail` (clean).

```bash
git add crates/farder-server/src/db.rs crates/farder-server/src/bots.rs
git commit -m "feat(alerts): alert engine (fire-once/re-arm) + poll-loop evaluation -> bot DM"
```

---

### Task 3: Server CRUD (alerts + subscriptions)

**Files:**
- Modify: `crates/farder-protocol/src/server.rs` (requests, response, `BotAlertInfo`)
- Modify: `crates/farder-server/src/bots.rs` (add/remove alert, subscribe/unsubscribe, list-my-subscriptions, cascade delete)
- Modify: `crates/farder-server/src/handlers.rs` (arms; cascade in RemoveBot)

**Interfaces:**
- Produces: `ServerRequest::{AddBotAlert{bot_public_key, metric, comparator, threshold}, RemoveBotAlert{alert_id: i64}, ListBotAlerts{bot_public_key}, SubscribeBot{bot_public_key}, UnsubscribeBot{bot_public_key}, ListMySubscriptions}`; `ServerResponse::{BotAlerts{alerts: Vec<BotAlertInfo>}, MySubscriptions{bot_public_keys: Vec<PublicKey>}}`; `BotAlertInfo { id: i64, metric: String, comparator: String, threshold: f64 }`.

- [ ] **Step 1: Protocol types + requests/response**

Add `BotAlertInfo` struct (`#[derive(Serialize, Deserialize, Clone)]`) and the 6 `ServerRequest` variants + 2 `ServerResponse` variants above (near the existing `AddBot`/`BotPollInterval`).

- [ ] **Step 2: bots.rs DB helpers + failing test**

Add: `add_alert(conn, bot, metric, comparator, threshold) -> Result<i64>` (INSERT, return id), `remove_alert(conn, id) -> Result<()>`, `subscribe(conn, bot, subscriber) -> Result<()>` (INSERT OR IGNORE), `unsubscribe(conn, bot, subscriber) -> Result<()>`, `list_subscriptions_for_user(conn, subscriber) -> Result<Vec<PublicKey>>`, and extend the bot-removal path to `DELETE FROM bot_alerts / bot_subscriptions WHERE bot_public_key = ?`. Test (mirror the existing `register_list_remove_roundtrip`): add two alerts + a subscription, list them, remove the bot, assert both tables are empty for that bot.

- [ ] **Step 3: Handler arms**

Add arms near the `AddBot` arm. Alert writes gate on `require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")?`; validate `metric ∈ {"price_usd","change_24h"}` and `comparator ∈ {"above","below"}` (else `err(...)`). `SubscribeBot`/`UnsubscribeBot`/`ListMySubscriptions` are any-member (no gate) — `subscribe(conn, &bot_public_key, member)` uses the authenticated `member`. Extend the existing `RemoveBot` arm to also delete alerts + subscriptions (the Step-2 cascade). Return `ServerResponse::BotAlerts`/`MySubscriptions`/`Ok`.

> Implementer note: mirror the existing `AddBot`/`RemoveBot` arms exactly for the gate + `ok_with`/`ok`/`err` helpers. `SubscribeBot` records the caller (`member`) as the subscriber — never trust a client-supplied subscriber key.

- [ ] **Step 4: Build + test + commit**

Run: `cargo test -p farder-server 2>&1 | tail` (pass) AND `cargo build --workspace 2>&1 | tail` (clean — new request/response variants; fix exhaustive matches).

```bash
git add crates/farder-protocol/src/server.rs crates/farder-server/src/bots.rs crates/farder-server/src/handlers.rs
git commit -m "feat(alerts): alert + subscription CRUD requests/handlers + cascade"
```

---

### Task 4: Client — alert config, subscribe toggle, my-subscriptions view

**Files:**
- Modify: `client/src-tauri/src/commands.rs` + `main.rs` (6 commands)
- Modify: `client/src/lib/tauri-bridge.ts` (6 bridge fns) + `client/src/lib/types.ts` (`BotAlertInfo`)
- Modify: `client/src/components/BotsTab.tsx` (owner Alerts sub-section per bot)
- Modify: `client/src/components/MemberContextMenu.tsx` (bot 🔔 toggle)
- Modify: `client/src/components/settings/SettingsModal.tsx` + a new `AlertSubscriptions.tsx` (my-subscriptions view)

**Interfaces:**
- Consumes: the Task 3 requests/response; `MemberInfo.is_bot`.

- [ ] **Step 1: Commands + bridge + type**

Mirror `add_bot`/`get_bot_poll_interval` for: `add_bot_alert(server_id, bot_public_key, metric, comparator, threshold) -> ()`, `remove_bot_alert(server_id, alert_id) -> ()`, `list_bot_alerts(server_id, bot_public_key) -> Vec<BotAlertInfo>` (matches `ServerResponse::BotAlerts`), `subscribe_bot(server_id, bot_public_key) -> ()`, `unsubscribe_bot(server_id, bot_public_key) -> ()`, `list_my_subscriptions(server_id) -> string[]` (bot public keys, matches `MySubscriptions`). Register all 6 in `generate_handler!`. Bridge fns (camelCase args), and add TS `interface BotAlertInfo { id: number; metric: string; comparator: string; threshold: number }` in types.ts. Use the member-key passing convention `remove_bot` uses for `bot_public_key`.

- [ ] **Step 2: BotsTab Alerts sub-section (owner)**

Inside each bot's `.organizer-row` block (BotsTab.tsx ~90-103), add an expandable "Alerts" area: `listBotAlerts` on expand; render each alert (`metric` · `comparator` · `threshold`) with a remove button; an add row (metric `<select>` Price/24h change → `"price_usd"`/`"change_24h"`; comparator `<select>` above/below; a number input; Add → `addBotAlert`). Reuse `organizer-*`/`connect-*` classes.

- [ ] **Step 3: MemberContextMenu 🔔 toggle**

Add a `rows.push({ kind: "item", ... })` guarded by `target.is_bot` (any member; no permission gate). Determine current state by calling `listMySubscriptions(serverId)` (or track it) and show "🔔 Notify me" vs "🔕 Unsubscribe"; onClick calls `subscribeBot`/`unsubscribeBot` with `publicKeyToString(target.public_key)`. (Item shape `{ kind:"item"; label; onClick; danger? }` per the file.)

- [ ] **Step 4: My-subscriptions settings section**

Add a `{ id: "alerts", label: "Alerts" }` entry to `SECTIONS`/`SectionId` in `SettingsModal.tsx` and render a new `AlertSubscriptions.tsx`: `listMySubscriptions(serverId)` → for each bot key, find the bot in `activeServer.members` (is_bot) for its label → list with an Unsubscribe button (`unsubscribeBot`). Mirror a sibling settings section component's shape.

- [ ] **Step 5: Build + seam + tsc + commit**

Run: `cd client/src-tauri && cargo build 2>&1 | tail` (clean); `grep -n 'bot_alert\|subscribe_bot\|list_my_subscriptions' client/src-tauri/src/main.rs` (all registered); `cd client && npx tsc --noEmit 2>&1 | tail` (clean).

```bash
git add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src/lib/tauri-bridge.ts client/src/lib/types.ts client/src/components/BotsTab.tsx client/src/components/MemberContextMenu.tsx client/src/components/settings/
git commit -m "feat(alerts): client alert config + subscribe toggle + my-subscriptions"
```

---

### Task 5: Docs

- [ ] **Step 1:** Update `tauri-commands.md` (6 commands), the bridge doc (6 fns + `BotAlertInfo`), `protocol.md` (the 6 requests + 2 responses + `BotAlertInfo` + `bot_alerts`/`bot_subscriptions` tables), a server doc (the alert engine, poll-loop evaluation, `send_bot_dm` bot→user E2EE DM via `EventTarget::Members`, cascade on bot removal), and `ARCHITECTURE.md` (the alert → bot-DM flow). Commit `docs(alerts): price alerts, subscriptions, bot DMs`.

---

## Owner runtime verification (server changed → full rebuild incl. sidecar)

`git pull` → `cargo build -p farder-server` → STOP app → `copy-sidecar.ps1` (repo root) → `cd client; npm run tauri dev` → Ctrl+Shift+R. On a mesh server with a **BTC** bot showing a live price:
1. Server Settings → Bots → BTC → Alerts → add "Price **below** $<just above the current price>" → right-click the BTC bot → **🔔 Notify me**.
2. Within a poll cycle you get a **DM from the BTC bot** (persists; you get it even if you weren't looking at that DM). It fires **once**, not every cycle.
3. Raise/lower so the condition clears then trips again → it re-arms and fires again.
4. Settings → **Alerts** lists the BTC bot; Unsubscribe → DMs stop. A member who never subscribed gets nothing.

## Self-review notes

- Spec "source-agnostic engine (metric+comparator+threshold, fire-once/re-arm)" → Task 2 (`evaluate_alert`).
- Spec "absolute + 24h-% metrics" → Task 2 (`metric_value` price_usd/change_24h).
- Spec "owner defines alerts, members opt in" → Task 3 (MANAGE_SERVER on alert writes; any-member subscribe).
- Spec "quiet DM delivery, won't-miss" → Task 1 (`send_bot_dm` via `EventTarget::Members` → persisted + targeted).
- Spec "verify bot→DM path FIRST" → Task 1 is the spike (crypto round-trip proven; full path build-verified).
- Spec "per-bot 🔔 toggle + my-subscriptions view" → Task 4.
- Spec "evaluate only on successful fetch" → Task 2 Step 4 (inside the `Ok(prices)` branch, only bots with a real `pi`).
- Spec "cascade on bot removal" → Task 3.
- Deferred (no tasks): per-user thresholds, channel/role delivery, custom API sources, alert history. Correct.
