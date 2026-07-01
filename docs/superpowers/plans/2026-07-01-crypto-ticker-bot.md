# Crypto Price-Ticker Bot (v1, server-driven) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A server owner can add crypto ticker bots that appear as Bot-tagged members whose name line shows a live price, polled server-side on an interval.

**Architecture:** A bot is a server-managed member (generated keypair, `is_bot` flag) stored in a `bots` table + a `members` row. The `farder-server` daemon runs a `tokio::interval` poller that fetches all bots' coins from CoinGecko in one coalesced SSRF-guarded call, writes each bot's price into the in-memory presence map, and broadcasts `MemberPresenceUpdated` over the client channels. The client renders a BOT badge + the price inline on the bot's name line.

**Tech Stack:** Rust (`farder-server`, `farder-protocol`, `farder-crypto`), rusqlite/SQLite, reqwest + serde_json, tokio, Tauri commands, React/TypeScript.

## Global Constraints

- **Verify-before-done (CLAUDE.md):** compiling + unit tests ≠ done; the frontend↔backend seam + real behavior need the owner's Windows runtime test. Mark client runtime UNVERIFIED until then.
- **Seam rule:** every `invoke("X")` has a matching `#[tauri::command] fn X` registered in `client/src-tauri/src/main.rs` `generate_handler!`. New commands: `add_bot`, `remove_bot`.
- **Server drives the bot; the relay stays a dumb pipe; no client need be open.** The server makes the outbound price call (no user IP exposed), SSRF-guarded via `ssrf::resolves_to_global`.
- **Bot-liveness = server-liveness** (offline exactly when the server is). v1 bots are server-local, NOT event-log identities.
- **Casing:** TS server-struct types are snake_case (`is_bot`, `public_key`); `invoke()` arg objects are camelCase (`serverId`, `coinId`).
- **UI styling (CLAUDE.md):** any new className is added to all three theme files (`client/src/themes/{xp-luna-blue,discord-dark,hello-kitty}/theme.css`), driven by `var(--xp-…)`; prefer reusing existing classes (`member-item`, `member-name`, `member-presence`, `organizer-*`, `tab-btn`).
- **No network in tests:** the CoinGecko fetch sits behind an injectable seam; poller tests inject a fake.
- **reqwest in farder-server has NO `json` feature** (Cargo.toml:34) — use `resp.text().await?` + `serde_json` (already a dep), do not add the feature.
- **Docs discipline:** changed public surface updates its `docs/modules/*.md` (Task 5).
- **Build/test:** `cargo test -p farder-server`; `cargo build --workspace` (a new `PresenceKind`/`ServerRequest` variant breaks exhaustive matches across the workspace); client crate `cd client/src-tauri && cargo build`; frontend `cd client && npx tsc --noEmit`.

---

### Task 1: Server — bots as members (schema, `is_bot` plumbing, AddBot/RemoveBot, roster whitelist)

**Files:**
- Modify: `crates/farder-server/src/db.rs` (migration: `members.is_bot`; create `bots` table)
- Create: `crates/farder-server/src/bots.rs` (bot DB helpers)
- Modify: `crates/farder-server/src/members.rs` (`MemberRecord.is_bot` + SELECTs)
- Modify: `crates/farder-protocol/src/server.rs` (`MemberInfo.is_bot`; `ServerRequest::AddBot`/`RemoveBot`)
- Modify: `crates/farder-server/src/handlers.rs` (AddBot/RemoveBot arms; roster whitelist; `is_bot` in MemberInfo build sites)
- Modify: `crates/farder-server/src/lib.rs` (add `pub mod bots;`)

**Interfaces:**
- Produces: `bots::register_bot(conn, pk: &PublicKey, secret: &[u8], coin_id: &str, label: &str) -> Result<()>`; `bots::list_bots(conn) -> Result<Vec<BotRecord>>` (`BotRecord { public_key: PublicKey, coin_id: String, label: String }`); `bots::remove_bot(conn, pk: &PublicKey) -> Result<()>`; `MemberRecord.is_bot: bool`; `MemberInfo.is_bot: bool`; `ServerRequest::AddBot { coin_id: String, label: String }`, `ServerRequest::RemoveBot { bot_public_key: PublicKey }`.

- [ ] **Step 1: Migrations**

In `db.rs`, after the `profile_hash` migration (~line 308), add an idempotent `is_bot` column + the `bots` table (mirror the guarded pattern at db.rs:261-308):

```rust
    // Bots: mark server-managed ticker members.
    let has_is_bot: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(members)")?;
        let cols: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(1))?.filter_map(|r| r.ok()).collect();
        cols.iter().any(|c| c == "is_bot")
    };
    if !has_is_bot {
        conn.execute("ALTER TABLE members ADD COLUMN is_bot INTEGER NOT NULL DEFAULT 0", [])?;
    }
    conn.execute(
        "CREATE TABLE IF NOT EXISTS bots (
            public_key BLOB PRIMARY KEY,
            secret_key BLOB NOT NULL,
            kind       TEXT NOT NULL,
            coin_id    TEXT NOT NULL,
            label      TEXT NOT NULL,
            created_at INTEGER NOT NULL
        )",
        [],
    )?;
```

- [ ] **Step 2: `bots.rs` module + failing test**

Create `crates/farder-server/src/bots.rs`:

```rust
//! Server-managed ticker bots: a bot is a generated keypair recorded here + a
//! `members` row (is_bot=1). The server holds the bot's secret (low-stakes, the
//! server is the bot's authority) and drives its presence via the poller.
use anyhow::Result;
use farder_crypto::identity::PublicKey;
use rusqlite::{params, Connection};

pub struct BotRecord {
    pub public_key: PublicKey,
    pub coin_id: String,
    pub label: String,
}

pub fn register_bot(conn: &Connection, pk: &PublicKey, secret: &[u8], coin_id: &str, label: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO bots (public_key, secret_key, kind, coin_id, label, created_at) \
         VALUES (?1, ?2, 'crypto_ticker', ?3, ?4, ?5)",
        params![pk.as_bytes().as_slice(), secret, coin_id, label, crate::db::now() as i64],
    )?;
    Ok(())
}

pub fn list_bots(conn: &Connection) -> Result<Vec<BotRecord>> {
    let mut stmt = conn.prepare("SELECT public_key, coin_id, label FROM bots")?;
    let rows = stmt.query_map([], |r| {
        let pk_bytes: Vec<u8> = r.get(0)?;
        Ok((pk_bytes, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (pk_bytes, coin_id, label) = row?;
        let pk = PublicKey::from_bytes(&pk_bytes).map_err(|e| anyhow::anyhow!("bad bot pk: {e}"))?;
        out.push(BotRecord { public_key: pk, coin_id, label });
    }
    Ok(out)
}

pub fn remove_bot(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute("DELETE FROM bots WHERE public_key = ?1", params![pk.as_bytes().as_slice()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::identity::Keypair;

    #[test]
    fn register_list_remove_roundtrip() {
        let conn = crate::db::open_in_memory().unwrap();
        let kp = Keypair::generate();
        register_bot(&conn, &kp.public_key(), &kp.secret_bytes(), "bitcoin", "BTC").unwrap();
        let bots = list_bots(&conn).unwrap();
        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0].coin_id, "bitcoin");
        assert_eq!(bots[0].label, "BTC");
        remove_bot(&conn, &kp.public_key()).unwrap();
        assert!(list_bots(&conn).unwrap().is_empty());
    }
}
```

Add `pub mod bots;` to `crates/farder-server/src/lib.rs` (next to the other `pub mod` lines).

> Implementer note: confirm the exact `PublicKey`/`Keypair` API — `PublicKey::from_bytes`, `Keypair::secret_bytes()`/`Keypair::public_key()`. If the method names differ (e.g. `to_bytes`/`from_slice`/`secret_key_bytes`), use the real ones from `crates/farder-crypto/src/identity.rs` and keep the same behavior (store 32-byte secret, reconstruct pk from 32-byte pk). If a bot needs to reconstruct a signing keypair later, store whatever `Keypair` round-trips from.

- [ ] **Step 3: Run the bots test — verify it fails, then passes**

Run: `cargo test -p farder-server bots:: 2>&1 | tail -15`
Expected: FAIL first (module/fn absent), PASS after Steps 1-2 compile. (If it fails on a crypto API name, fix per the implementer note.)

- [ ] **Step 4: `MemberRecord.is_bot` + SELECTs**

In `crates/farder-server/src/members.rs`: add `pub is_bot: bool,` to `struct MemberRecord` (lines 13-20). In `get_member` (SELECT ~line 65) and `list_members` (SELECT ~line 100), add `is_bot` to the column list and read it in the row closure as `is_bot: row.get::<_, i64>(<idx>)? != 0` (append it as the last selected column so existing indices don't shift). `list_members`' `WHERE banned = 0 AND revoked = 0` stays (bots are neither).

- [ ] **Step 5: `MemberInfo.is_bot` (protocol) + the request variants**

In `crates/farder-protocol/src/server.rs`: add to `struct MemberInfo` (after `presence`):

```rust
    #[serde(default)]
    pub is_bot: bool,
```

In `enum ServerRequest` (near `AssignRole`, line 256), add:

```rust
    AddBot { coin_id: String, label: String },
    RemoveBot { bot_public_key: PublicKey },
```

- [ ] **Step 6: Roster whitelist + `is_bot` in the build sites**

In `handlers.rs` `GetMembers` (line 1072-1077), change the filter predicate to keep bots:

```rust
            all_members.retain(|m| m.is_bot || ls.is_member(&m.public_key));
```

In the MemberInfo construction in the GetMembers loop (~line 1089), add `is_bot: m.is_bot,`. Do the same in the other `MemberInfo { … }` construction sites the compiler flags — `GetPendingMembers` (~1116) and any `DmOpened` participant build (~1335/1374); grep `MemberInfo {` across `crates/farder-server` and `crates/farder-protocol` (the protocol roundtrip test) and add `is_bot: false` (or the record's value) to each.

- [ ] **Step 7: AddBot / RemoveBot handler arms + failing test**

Add a test in `handlers.rs` `mod tests` (mirror an existing owner-gated request test — find one that builds `state` + calls `handle_request` with `is_owner`):

```rust
    #[test]
    fn add_bot_creates_member_and_remove_deletes_it() {
        // ... build state + conn as the adjacent owner-gated handler tests do ...
        let owner = /* the owner key the harness set up */;
        let res = handle_request(&conn, &owner, true, ServerRequest::AddBot { coin_id: "bitcoin".into(), label: "BTC".into() }, "", &state).unwrap();
        assert!(matches!(res.response, ServerResponse::Ok));
        // a bot member now exists and is a bot
        let bots = crate::bots::list_bots(&conn).unwrap();
        assert_eq!(bots.len(), 1);
        let bot_pk = bots[0].public_key.clone();
        let members = crate::members::list_members(&conn).unwrap();
        assert!(members.iter().any(|m| m.public_key == bot_pk && m.is_bot));
        // non-owner cannot add
        let stranger = farder_crypto::identity::Keypair::generate().public_key();
        let denied = handle_request(&conn, &stranger, false, ServerRequest::AddBot { coin_id: "ethereum".into(), label: "ETH".into() }, "", &state).unwrap();
        assert!(!matches!(denied.response, ServerResponse::Ok));
        // remove
        let res2 = handle_request(&conn, &owner, true, ServerRequest::RemoveBot { bot_public_key: bot_pk.clone() }, "", &state).unwrap();
        assert!(matches!(res2.response, ServerResponse::Ok));
        assert!(crate::bots::list_bots(&conn).unwrap().is_empty());
        assert!(!crate::members::list_members(&conn).unwrap().iter().any(|m| m.public_key == bot_pk));
    }
```

Then add the handler arms in the `match request` (near the CreateRole arm ~741). Use the owner gate `require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")?`:

```rust
        ServerRequest::AddBot { coin_id, label } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            let kp = farder_crypto::identity::Keypair::generate();
            let pk = kp.public_key();
            crate::members::register_bot(conn, &pk, &label)?;   // inserts members row with is_bot=1
            crate::bots::register_bot(conn, &pk, &kp.secret_bytes(), &coin_id, &label)?;
            ok_with(ServerResponse::Ok, vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberJoined { public_key: pk, display_name: label },
            }])
        }
        ServerRequest::RemoveBot { bot_public_key } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            crate::bots::remove_bot(conn, &bot_public_key)?;
            crate::members::remove_member_row(conn, &bot_public_key)?; // hard-delete the bot's member row
            {
                let mut map = state.presences.write().unwrap();
                map.remove(bot_public_key.as_bytes());
            }
            ok_with(ServerResponse::Ok, vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberLeft { public_key: bot_public_key },
            }])
        }
```

Add the two `members` helpers this needs, in `members.rs`:

```rust
/// Register a server-managed bot as a member (is_bot = 1).
pub fn register_bot(conn: &Connection, pk: &PublicKey, display_name: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO members (public_key, display_name, joined_at, is_bot) VALUES (?1, ?2, ?3, 1)",
        params![pk.as_bytes().as_slice(), display_name, now() as i64],
    )?;
    Ok(())
}

/// Hard-delete a member row (used to remove a bot).
pub fn remove_member_row(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute("DELETE FROM members WHERE public_key = ?1", params![pk.as_bytes().as_slice()])?;
    Ok(())
}
```

> Implementer notes: (1) match the real `Keypair` API (`generate`, `public_key`, `secret_bytes`) — see the crypto identity module; (2) confirm `require_base_perm`/`ok_with`/`BroadcastEvent`/`EventTarget`/`ServerEvent::MemberJoined` names against handlers.rs (they're used by CreateRole/AssignRole); (3) if `ServerResponse` needs a non-Ok for "denied", `require_base_perm` already returns the denial `HandleResult` — return it as shown.

- [ ] **Step 8: Run tests + workspace build**

Run: `cargo test -p farder-server bots:: 2>&1 | tail` and `cargo test -p farder-server add_bot 2>&1 | tail` → PASS.
Run: `cargo build --workspace 2>&1 | tail -5` → builds (all `MemberInfo {` sites + `ServerRequest` matches updated; pristine).

- [ ] **Step 9: Commit**

```bash
git add crates/farder-server/src/db.rs crates/farder-server/src/bots.rs crates/farder-server/src/members.rs crates/farder-server/src/lib.rs crates/farder-protocol/src/server.rs crates/farder-server/src/handlers.rs
git commit -m "feat(bots): bots as server-managed members + AddBot/RemoveBot + roster whitelist"
```

---

### Task 2: Server — the price poller (PresenceKind::Ticker, coalesced CoinGecko fetch, broadcast)

**Files:**
- Modify: `crates/farder-protocol/src/server.rs` (`PresenceKind::Ticker`)
- Modify: `crates/farder-server/src/bots.rs` (poller + price seam)
- Modify: `crates/farder-server/src/main.rs:142` (spawn the poller)

**Interfaces:**
- Consumes: `bots::list_bots` (Task 1); `ServerState.presences`; `ssrf::resolves_to_global`; the client-broadcast path.
- Produces: `bots::spawn_bot_poll_task(state: Arc<ServerState>, interval_secs: u64) -> tokio::task::JoinHandle<()>`; `bots::PriceInfo { usd: f64, change_24h: f64 }`; `bots::ticker_presence(&PriceInfo) -> Presence`.

- [ ] **Step 1: `PresenceKind::Ticker`**

In `crates/farder-protocol/src/server.rs`, `enum PresenceKind { Music, Game }` → add `Ticker`:

```rust
pub enum PresenceKind { Music, Game, Ticker }
```

- [ ] **Step 2: Presence composition + failing test**

In `bots.rs`, add the price type + presence composer + a test (pure, no network):

```rust
use farder_protocol::server::{Presence, PresenceKind};

#[derive(Clone, Debug)]
pub struct PriceInfo { pub usd: f64, pub change_24h: f64 }

/// Compose a ticker presence: details = "$<price> <arrow><pct>%", state = "24h".
pub fn ticker_presence(p: &PriceInfo) -> Presence {
    let arrow = if p.change_24h >= 0.0 { '\u{25B2}' } else { '\u{25BC}' }; // ▲ / ▼
    let details = format!("${:.2} {}{:.2}%", p.usd, arrow, p.change_24h.abs());
    Presence { kind: PresenceKind::Ticker, details, state: Some("24h".into()) }
}

#[cfg(test)]
mod ticker_tests {
    use super::*;
    #[test]
    fn ticker_presence_formats_up_and_down() {
        let up = ticker_presence(&PriceInfo { usd: 67432.0, change_24h: 2.1 });
        assert_eq!(up.details, "$67432.00 \u{25B2}2.10%");
        let down = ticker_presence(&PriceInfo { usd: 3200.5, change_24h: -1.4 });
        assert_eq!(down.details, "$3200.50 \u{25BC}1.40%");
    }
}
```

Run: `cargo test -p farder-server ticker_presence 2>&1 | tail` → FAIL then PASS.

- [ ] **Step 3: Price fetch seam (real CoinGecko impl, not unit-tested)**

In `bots.rs`, add the coalesced fetch. Reuse the SSRF-guarded reqwest pattern from `connection.rs:346-391` (http(s)-only, `resolves_to_global`, 10s timeout). Parse via `serde_json` (no `json` feature):

```rust
/// Fetch USD price + 24h change for the given CoinGecko ids in ONE call.
/// Returns a map coin_id -> PriceInfo. Network — not unit-tested; SSRF-guarded.
pub async fn fetch_prices(coin_ids: &[String]) -> anyhow::Result<std::collections::HashMap<String, PriceInfo>> {
    use std::collections::HashMap;
    if coin_ids.is_empty() { return Ok(HashMap::new()); }
    let ids = coin_ids.join(",");
    let url = format!("https://api.coingecko.com/api/v3/simple/price?ids={}&vs_currencies=usd&include_24hr_change=true",
        urlencoding::encode(&ids));
    if !crate::ssrf::resolves_to_global(&url).await {
        anyhow::bail!("coingecko url did not resolve to a global address");
    }
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none()).build()?;
    let body = client.get(&url).header("accept", "application/json").send().await?.text().await?;
    let v: serde_json::Value = serde_json::from_str(&body)?;
    let mut out = HashMap::new();
    if let Some(obj) = v.as_object() {
        for (id, data) in obj {
            let usd = data.get("usd").and_then(|x| x.as_f64());
            let chg = data.get("usd_24h_change").and_then(|x| x.as_f64()).unwrap_or(0.0);
            if let Some(usd) = usd {
                out.insert(id.clone(), PriceInfo { usd, change_24h: chg });
            }
        }
    }
    Ok(out)
}
```

> Implementer note: `urlencoding` may not be a dep — CoinGecko ids are `[a-z0-9-]` so encoding is a no-op; if `urlencoding` isn't available, drop it and interpolate `ids` directly (the ids come from a curated list / validated input, not arbitrary user text). Confirm `reqwest::redirect::Policy` path matches connection.rs. If `resolves_to_global` needs a host not a full URL, match its real signature (ssrf.rs:54).

- [ ] **Step 4: The poll task + broadcast helper**

In `bots.rs`, add the interval task (mirror `retention::spawn_retention_task`, retention.rs:68-100). It must broadcast `MemberPresenceUpdated` directly over the client channels (the poller has no request context). Locate how a handler's `BroadcastEvent { target: All }` is delivered to clients (the fan-out over `state.clients: RwLock<HashMap<[u8;32], mpsc::Sender<ServerEvent>>>`, connection.rs:648) and reuse/mirror that as a helper:

```rust
use std::sync::Arc;
use crate::state::ServerState;
use farder_protocol::server::ServerEvent;

/// Send an event to every connected client (mirrors the handler broadcast fan-out).
fn broadcast_all(state: &ServerState, event: ServerEvent) {
    let clients = state.clients.read().unwrap(); // match the real lock type/name
    for tx in clients.values() {
        let _ = tx.try_send(event.clone());
    }
}

pub fn spawn_bot_poll_task(state: Arc<ServerState>, interval_secs: u64) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(15)));
        loop {
            interval.tick().await;
            // 1. snapshot bots (drop the DB lock before awaiting the fetch)
            let bots = { let conn = state.db.lock().unwrap(); list_bots(&conn).unwrap_or_default() };
            if bots.is_empty() { continue; }
            // 2. coalesce distinct coin ids, one fetch
            let mut ids: Vec<String> = bots.iter().map(|b| b.coin_id.clone()).collect();
            ids.sort(); ids.dedup();
            let prices = match fetch_prices(&ids).await {
                Ok(p) => p,
                Err(e) => { tracing::warn!(error = %e, "bot price fetch failed; keeping last prices"); continue; }
            };
            // 3. per bot: compose + store + broadcast (skip a coin missing from the response — keep last)
            for b in &bots {
                if let Some(pi) = prices.get(&b.coin_id) {
                    let presence = ticker_presence(pi);
                    { state.presences.write().unwrap().insert(*b.public_key.as_bytes(), presence.clone()); }
                    broadcast_all(&state, ServerEvent::MemberPresenceUpdated { public_key: b.public_key.clone(), presence: Some(presence) });
                }
            }
        }
    })
}
```

> Implementer notes: (1) match the REAL `state.clients` field name + lock type + sender type (`try_send` vs `send`), and whether an existing `broadcast`/`fan_out` helper already exists (reuse it if so — DRY). (2) `PublicKey::as_bytes()` returns `&[u8;32]` (used in Task 1) — `*b.public_key.as_bytes()` copies it for the map key; match the map's key type (`[u8;32]`). (3) Do NOT hold `state.db.lock()` across `.await` (the snapshot in step 1 drops it first).

- [ ] **Step 5: Spawn the poller at startup**

In `crates/farder-server/src/main.rs`, right after line 142 (`let _retention = retention::spawn_retention_task(...)`), before the relay-only `return` (~147):

```rust
    let _bot_poller = farder_server::bots::spawn_bot_poll_task(Arc::clone(&state), 60);
```

- [ ] **Step 6: Run tests + workspace build**

Run: `cargo test -p farder-server ticker 2>&1 | tail` → PASS. Run: `cargo build --workspace 2>&1 | tail -5` → builds (the new `PresenceKind::Ticker` breaks any exhaustive match on PresenceKind — e.g. `formatPresence` is TS not Rust, but check Rust matches; add arms as needed; pristine).

- [ ] **Step 7: Commit**

```bash
git add crates/farder-protocol/src/server.rs crates/farder-server/src/bots.rs crates/farder-server/src/main.rs
git commit -m "feat(bots): server price poller (coalesced CoinGecko fetch) + ticker presence broadcast"
```

---

### Task 3: Client — bot management UI + commands

**Files:**
- Modify: `client/src-tauri/src/commands.rs` (`add_bot`, `remove_bot`)
- Modify: `client/src-tauri/src/main.rs` (register in `generate_handler!`)
- Modify: `client/src/lib/tauri-bridge.ts` (`addBot`, `removeBot`)
- Modify: `client/src/lib/types.ts` (`MemberInfo.is_bot`)
- Create: `client/src/components/BotsTab.tsx`
- Modify: `client/src/components/ServerSettingsDialog.tsx` (wire the Bots tab)

**Interfaces:**
- Consumes: `ServerRequest::AddBot`/`RemoveBot` (Task 1).
- Produces: `addBot(serverId, coinId, label)`, `removeBot(serverId, botPublicKey)` bridge fns; the Bots settings tab.

- [ ] **Step 1: Tauri commands (mirror `create_role` / `delete_role`, commands.rs:2297-2332)**

```rust
#[tauri::command]
pub async fn add_bot(state: State<'_, Arc<AppState>>, server_id: String, coin_id: String, label: String) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::AddBot { coin_id, label }).await
        .map_err(|e| e.to_string())? {
        ServerResponse::Ok => Ok(()),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn remove_bot(state: State<'_, Arc<AppState>>, server_id: String, bot_public_key: String) -> Result<(), String> {
    let pk = /* parse bot_public_key string -> PublicKey, mirroring how kick_member/assign_role parse a member key from the client */;
    match bridge::send_request(&state, &server_id, ServerRequest::RemoveBot { bot_public_key: pk }).await
        .map_err(|e| e.to_string())? {
        ServerResponse::Ok => Ok(()),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}
```

> Implementer note: find how an existing member-targeting client command (e.g. `kick_member`, `assign_role`) receives a `PublicKey` from JS and parses it (hex string via `publicKeyToString` ↔ `PublicKey::from_...`, or a `{bytes}` shape). Use the SAME convention for `bot_public_key` so the bridge call matches. Register both commands in `main.rs` `generate_handler!` (next to `commands::create_role,` line ~151).

- [ ] **Step 2: Bridge fns + TS type**

`tauri-bridge.ts` (near `createRole`, 346):

```ts
export async function addBot(serverId: string, coinId: string, label: string): Promise<void> {
  return invoke("add_bot", { serverId, coinId, label });
}
export async function removeBot(serverId: string, botPublicKey: string): Promise<void> {
  return invoke("remove_bot", { serverId, botPublicKey });
}
```

`client/src/lib/types.ts`, `interface MemberInfo` (32-41): add `is_bot?: boolean;`. Add `"Ticker"` to the `PresenceKind` union (types.ts:30 area).

- [ ] **Step 3: `BotsTab.tsx`**

Create a component that lists the server's bots (filter `activeServer.members` for `is_bot`) and offers Add (majors dropdown + Custom) / Remove. Reuse `organizer-*` + `connect-*` classes (styled in all themes). Majors:

```tsx
const MAJORS: { id: string; label: string }[] = [
  { id: "bitcoin", label: "BTC" }, { id: "ethereum", label: "ETH" }, { id: "solana", label: "SOL" },
  { id: "litecoin", label: "LTC" }, { id: "ripple", label: "XRP" }, { id: "dogecoin", label: "DOGE" }, { id: "cardano", label: "ADA" },
];
```

The Add control: a `<select>` of MAJORS plus a `"custom"` option that reveals a text input for a CoinGecko id + a label field; on submit call `api.addBot(serverId, coinId, label)`. The list: each bot member with a Remove button calling `api.removeBot(serverId, publicKeyToString(member.public_key))`. Match `AuditLogTab`'s props/shape (`{ serverId }`) and the `organizer-row`/`organizer-btn` markup from the Roles section (ServerSettingsDialog.tsx:282-360). Read `activeServer` from the same context those components use.

> Implementer note: this is the one component with real layout freedom — keep it consistent with the Roles section's structure and classes; do NOT introduce new classes unless necessary (if you do, add them to all three theme files).

- [ ] **Step 4: Wire the tab into `ServerSettingsDialog.tsx`**

- Add `"bots"` to the `activeTab` union (line 25).
- Add a tab button gated by `canManageServer` in the tab bar (169-193), mirroring the Audit Log button (185-192): label "Bots", `onClick={() => setActiveTab("bots")}`.
- Add the body (195-202): `{activeTab === "bots" && serverId && <BotsTab serverId={serverId} />}`.
- Import `BotsTab`.

- [ ] **Step 5: Build + seam + tsc**

Run: `cd client/src-tauri && cargo build 2>&1 | tail` (clean); `grep -n 'add_bot\|remove_bot' client/src-tauri/src/main.rs` (both in generate_handler!); `cd client && npx tsc --noEmit 2>&1 | tail` (clean).

- [ ] **Step 6: Commit**

```bash
git add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src/lib/tauri-bridge.ts client/src/lib/types.ts client/src/components/BotsTab.tsx client/src/components/ServerSettingsDialog.tsx
git commit -m "feat(bots): server-settings Bots panel + add_bot/remove_bot commands"
```

---

### Task 4: Client — BOT badge + inline ticker render

**Files:**
- Modify: `client/src/components/MemberSidebar.tsx` (`MemberRow`, 34-65)
- Modify: `client/src/lib/presence.ts` (`formatPresence` Ticker branch, 9-14)
- Modify: `client/src/themes/{xp-luna-blue,discord-dark,hello-kitty}/theme.css` (`.member-bot-badge`)

**Interfaces:**
- Consumes: `MemberInfo.is_bot` (Task 1/3), `PresenceKind.Ticker` (Task 2/3).

- [ ] **Step 1: `formatPresence` Ticker branch**

In `client/src/lib/presence.ts` (9-14), add a branch so a Ticker presence renders just its `details` (the price string) with no "Listening to" prefix:

```ts
  if (presence.kind === "Ticker") return presence.details;
```

- [ ] **Step 2: BOT badge + inline price in `MemberRow`**

In `MemberSidebar.tsx` `MemberRow` (34-65): after the `member-name` span (line 55), when `member.is_bot`, render a small BOT badge (mirror the `TimedOutBadge` placement at 60-62):

```tsx
{member.is_bot && <span className="member-bot-badge">BOT</span>}
```

The presence line (56-58) already renders `formatPresence(member.presence)` in `.member-presence` — for a Ticker that yields the inline price, so the row reads `BTC  $67,432.00 ▲2.10%  BOT`. No change needed there beyond Step 1. (Optionally place the badge before the presence so order is name · BOT · price; match the visual the owner expects — name then price then BOT is fine.)

- [ ] **Step 3: `.member-bot-badge` in all three themes**

Add to EACH of `client/src/themes/{xp-luna-blue,discord-dark,hello-kitty}/theme.css`, using that theme's variables (mirror an existing small pill/badge class in the file, e.g. the timed-out badge or a role pill):

```css
.member-bot-badge {
  font-size: 9px;
  font-weight: 700;
  padding: 0 4px;
  margin-left: 4px;
  border-radius: 3px;
  background: var(--xp-blue, #5865f2);
  color: #fff;
  text-transform: uppercase;
}
```

(Use the correct per-theme accent variable — check what each theme.css uses for accent/badge backgrounds; do not hard-code a color if a variable exists.)

- [ ] **Step 4: Verify**

Run: `cd client && npx tsc --noEmit 2>&1 | tail` (clean). Run: `grep -l "member-bot-badge" client/src/themes/*/theme.css` → lists all three.

- [ ] **Step 5: Commit**

```bash
git add client/src/components/MemberSidebar.tsx client/src/lib/presence.ts client/src/themes/
git commit -m "feat(bots): BOT badge + inline ticker price in the member list"
```

---

### Task 5: Docs

**Files:** `docs/modules/tauri-commands.md`, the bridge doc, `docs/modules/protocol.md`, a server doc, `ARCHITECTURE.md`.

- [ ] **Step 1: Document the surfaces**

- `tauri-commands.md`: `add_bot(server_id, coin_id, label)`, `remove_bot(server_id, bot_public_key)` (owner-gated).
- bridge doc: `addBot`/`removeBot`; `MemberInfo.is_bot`.
- `protocol.md`: `PresenceKind::Ticker`, `MemberInfo.is_bot`, `ServerRequest::AddBot`/`RemoveBot`.
- server doc: bots-as-members model, the poll task (coalesced SSRF-guarded CoinGecko fetch, presence broadcast), the roster whitelist for `is_bot`.
- `ARCHITECTURE.md`: the server-driven bot data path (owner adds → server generates keypair + members row → poller fetches → presence broadcast → inline render).

- [ ] **Step 2: Commit**

```bash
git add docs/
git commit -m "docs(bots): crypto ticker bot data model, commands, poller"
```

---

## Owner runtime verification (server changed → full rebuild incl. sidecar)

`git pull` → `cargo build -p farder-server` → STOP app → `.\client\src-tauri\binaries\copy-sidecar.ps1` (from repo root) → `cd client; npm run tauri dev` → `Ctrl+Shift+R`. Then:
1. Server settings → **Bots** → Add → **BTC** → a `BTC · BOT` member appears; within ~60s its name line shows a live price; a 2nd client sees it.
2. Add a **Custom** coin (a CoinGecko id like `solana`) → its ticker appears.
3. Remove a bot → gone for everyone.
4. Restart the server → bots return and resume ticking (config persisted; price repopulates within ~60s).
5. A non-owner has no Bots tab / cannot add.

## Self-review notes (coverage vs spec)

- Spec "bot = server-managed member, generated keypair, is_bot, bots table" → Task 1.
- Spec "owner adds/removes, multiple per server" → Task 1 (handlers) + Task 3 (UI).
- Spec "mesh roster whitelist for bots" → Task 1 Step 6.
- Spec "server poller, ~60s, coalesced SSRF-guarded CoinGecko fetch, no user IP" → Task 2.
- Spec "PresenceKind::Ticker, price in name via presence rail" → Task 2 (presence) + Task 4 (inline render).
- Spec "majors dropdown + Custom" → Task 3.
- Spec "BOT badge, hide human-only actions on bots" → Task 4 (badge; hiding human-only actions on bots is via `is_bot` in the member context menu — if a member context menu shows kick/ban, gate those off for `is_bot`; note for Task 4 implementer).
- Spec "failure → last-known/stale, not crash" → Task 2 Step 4 (skip missing coin, keep last; warn on fetch error).
- Out of scope (no tasks): message/command bots, moderation, external API, non-crypto, mesh-always-on. Correct.
