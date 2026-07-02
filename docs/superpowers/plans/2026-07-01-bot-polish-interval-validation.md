# Bot Polish — Configurable Poll Interval + Coin-ID Validation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a server owner set the crypto-ticker poll interval, and make a bad/typo'd coin id fail loudly ("unknown coin") instead of sitting on "fetching price…" forever.

**Architecture:** A new `server_settings` KV table holds a per-server `bot_poll_interval` (owner-settable, floor 30s); the poll loop reads it each cycle (poll-then-sleep, so changes apply live). Coin validation is server-side only (the handler is sync + a client-side resolve would leak the user IP): a charset/length guard on `AddBot`, plus the poller marking any coin CoinGecko's response omits as an "unknown coin" presence.

**Tech Stack:** Rust (`farder-server`, `farder-protocol`), rusqlite, tokio, Tauri commands, React/TS.

## Global Constraints

- **Verify-before-done:** the frontend↔backend seam + real behavior need the owner's Windows runtime test; mark UNVERIFIED until then.
- **Seam rule:** new commands `get_bot_poll_interval`, `set_bot_poll_interval` must be registered in `client/src-tauri/src/main.rs` `generate_handler!` with matching invoke names.
- **Casing:** TS server types snake_case; invoke args camelCase.
- **Privacy:** the SERVER polls CoinGecko (never the client). No client-side coin resolve.
- **Rate-limit floor:** the poll interval is clamped to **≥30s** on both set and read (CoinGecko free-tier). Default 60s when unset.
- **No new server-level config existed before** — `retention_secs` is per-channel; this adds a general `server_settings` KV table.
- **UI styling:** reuse existing `connect-*`/`organizer-*` classes (BotsTab already uses them); no new class expected.
- **Build/test:** `cargo test -p farder-server`; `cargo build --workspace`; `cd client && npx tsc --noEmit`.

---

### Task 1: Server — configurable interval + coin-id validation + unknown-coin marking

**Files:**
- Modify: `crates/farder-server/src/db.rs` (server_settings table + get/set_setting)
- Modify: `crates/farder-server/src/bots.rs` (interval helpers; poll-loop restructure; unknown_coin_presence)
- Modify: `crates/farder-server/src/main.rs:143` (poller spawn — drop the hardcoded arg)
- Modify: `crates/farder-protocol/src/server.rs` (`SetBotPollInterval`/`GetBotPollInterval` requests + `BotPollInterval` response)
- Modify: `crates/farder-server/src/handlers.rs` (two handler arms + the AddBot guard)

**Interfaces:**
- Produces: `db::get_setting(conn,key)->Result<Option<String>>`, `db::set_setting(conn,key,value)->Result<()>`; `bots::get_poll_interval(conn)->u64`, `bots::set_poll_interval(conn,secs)->Result<()>`, `bots::unknown_coin_presence()->Presence`, `bots::POLL_INTERVAL_FLOOR=30`, `POLL_INTERVAL_DEFAULT=60`; `ServerRequest::{SetBotPollInterval{secs:u64}, GetBotPollInterval}`, `ServerResponse::BotPollInterval{secs:u64}`; `spawn_bot_poll_task(state: Arc<ServerState>)` (no interval arg).

- [ ] **Step 1: server_settings table + KV helpers**

In `db.rs`, add to the schema (near the other `CREATE TABLE IF NOT EXISTS`):

```rust
        CREATE TABLE IF NOT EXISTS server_settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
```

Add helpers (near other `pub fn` in db.rs; `OptionalExtension` is already imported for `.optional()`):

```rust
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO server_settings (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

pub fn get_setting(conn: &Connection, key: &str) -> anyhow::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row("SELECT value FROM server_settings WHERE key = ?1", rusqlite::params![key], |r| r.get::<_, String>(0))
        .optional()?)
}
```

- [ ] **Step 2: interval helpers + unknown-coin presence + failing tests**

In `bots.rs`, add (near `ticker_presence`):

```rust
pub const POLL_INTERVAL_FLOOR: u64 = 30;
pub const POLL_INTERVAL_DEFAULT: u64 = 60;

/// The current per-server bot poll interval (seconds), floored at POLL_INTERVAL_FLOOR,
/// defaulting to POLL_INTERVAL_DEFAULT when unset.
pub fn get_poll_interval(conn: &Connection) -> u64 {
    crate::db::get_setting(conn, "bot_poll_interval")
        .ok()
        .flatten()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|v| v.max(POLL_INTERVAL_FLOOR))
        .unwrap_or(POLL_INTERVAL_DEFAULT)
}

/// Set the poll interval (clamped to >= POLL_INTERVAL_FLOOR).
pub fn set_poll_interval(conn: &Connection, secs: u64) -> anyhow::Result<()> {
    crate::db::set_setting(conn, "bot_poll_interval", &secs.max(POLL_INTERVAL_FLOOR).to_string())
}

/// Presence for a bot whose coin CoinGecko did not return (bad/unknown id).
pub fn unknown_coin_presence() -> Presence {
    Presence { kind: PresenceKind::Ticker, details: "unknown coin".into(), state: None }
}
```

Add tests in the `mod tests` (uses `crate::db::open_in_memory`):

```rust
    #[test]
    fn poll_interval_defaults_and_clamps() {
        let conn = crate::db::open_in_memory().unwrap();
        assert_eq!(get_poll_interval(&conn), POLL_INTERVAL_DEFAULT); // unset -> default
        set_poll_interval(&conn, 120).unwrap();
        assert_eq!(get_poll_interval(&conn), 120);
        set_poll_interval(&conn, 5).unwrap();                        // below floor -> clamped
        assert_eq!(get_poll_interval(&conn), POLL_INTERVAL_FLOOR);
    }
```

Run: `cargo test -p farder-server poll_interval 2>&1 | tail` → FAIL then PASS after Step 1-2 compile.

- [ ] **Step 3: Restructure the poll loop (poll-then-sleep, live interval, unknown-coin)**

In `bots.rs`, replace the current `spawn_bot_poll_task` body. Change the signature to drop `interval_secs`, poll immediately then sleep the configured interval, and mark unknown coins:

```rust
pub fn spawn_bot_poll_task(state: Arc<ServerState>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tracing::info!("bot price poller started");
        loop {
            // --- poll cycle (immediate on first iteration) ---
            let bots = {
                let conn = state.db.lock().unwrap();
                list_bots(&conn).unwrap_or_default()
            };
            if !bots.is_empty() {
                let mut ids: Vec<String> = bots.iter().map(|b| b.coin_id.clone()).collect();
                ids.sort(); ids.dedup();
                tracing::info!(bots = bots.len(), coins = ?ids, "bot poller: fetching prices");
                match fetch_prices(&ids).await {
                    Ok(prices) => {
                        tracing::info!(fetched = prices.len(), "bot poller: prices fetched, broadcasting");
                        for b in &bots {
                            // A coin CoinGecko omitted (after a SUCCESSFUL fetch) is unknown/typo'd.
                            let presence = match prices.get(&b.coin_id) {
                                Some(pi) => ticker_presence(pi),
                                None => unknown_coin_presence(),
                            };
                            {
                                state.presences.write().unwrap()
                                    .insert(*b.public_key.as_bytes(), presence.clone());
                            }
                            crate::connection::broadcast_event(
                                &state,
                                crate::events::EventTarget::All,
                                ServerEvent::MemberPresenceUpdated {
                                    public_key: b.public_key.clone(),
                                    presence: Some(presence),
                                },
                            ).await;
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "bot price fetch failed; keeping last prices"),
                }
            }
            // --- sleep the current (live-read) interval ---
            let secs = {
                let conn = state.db.lock().unwrap();
                get_poll_interval(&conn)
            };
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        }
    })
}
```

In `crates/farder-server/src/main.rs:143`, change the spawn to drop the arg:

```rust
    let _bot_poller = farder_server::bots::spawn_bot_poll_task(Arc::clone(&state));
```

- [ ] **Step 4: Requests + response + handler arms + AddBot guard**

In `crates/farder-protocol/src/server.rs`: add to `ServerRequest` (near `AddBot`):

```rust
    SetBotPollInterval { secs: u64 },
    GetBotPollInterval,
```

and to `ServerResponse`:

```rust
    BotPollInterval { secs: u64 },
```

In `handlers.rs`, add two arms (near the AddBot arm):

```rust
        ServerRequest::SetBotPollInterval { secs } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            crate::bots::set_poll_interval(conn, secs)?;
            ok(ServerResponse::Ok)
        }
        ServerRequest::GetBotPollInterval => {
            ok(ServerResponse::BotPollInterval { secs: crate::bots::get_poll_interval(conn) })
        }
```

In the existing `AddBot` arm, after the owner gate and before inserting, add the guard (normalize + validate coin_id and label):

```rust
            let coin_id = coin_id.trim().to_lowercase();
            if coin_id.is_empty()
                || coin_id.len() > 64
                || !coin_id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                return err("invalid coin id (use a CoinGecko id: lowercase letters, digits, hyphens)");
            }
            let label = label.trim().to_string();
            if label.is_empty() || label.len() > 64 {
                return err("bot label must be 1-64 characters");
            }
```

Then use the normalized `coin_id`/`label` in the existing `register_bot`/`register_bot_member` calls.

> Implementer note: match the real `ok`/`err`/`require_base_perm`/`ok_with` helper names + `permissions::MANAGE_SERVER` already used by the AddBot/CreateRole arms. `ok(ServerResponse::...)` may be spelled `ok_with(..., vec![])` — use whatever the file's read-only responses use.

- [ ] **Step 5: build + tests**

Run: `cargo test -p farder-server poll_interval 2>&1 | tail` (pass) AND `cargo build --workspace 2>&1 | tail -5` (clean — the new `ServerRequest`/`ServerResponse` variants break exhaustive matches; add arms; the `spawn_bot_poll_task` signature change is handled at main.rs:143; pristine).

- [ ] **Step 6: Commit**

```bash
git add crates/farder-server/src/db.rs crates/farder-server/src/bots.rs crates/farder-server/src/main.rs crates/farder-protocol/src/server.rs crates/farder-server/src/handlers.rs
git commit -m "feat(bots): configurable poll interval + coin-id validation + unknown-coin state"
```

---

### Task 2: Client — interval input + commands/bridge

**Files:**
- Modify: `client/src-tauri/src/commands.rs` (`get_bot_poll_interval`, `set_bot_poll_interval`)
- Modify: `client/src-tauri/src/main.rs` (register both)
- Modify: `client/src/lib/tauri-bridge.ts` (`getBotPollInterval`, `setBotPollInterval`)
- Modify: `client/src/components/BotsTab.tsx` (interval field)

**Interfaces:**
- Consumes: `ServerRequest::SetBotPollInterval`/`GetBotPollInterval`, `ServerResponse::BotPollInterval` (Task 1).

- [ ] **Step 1: Tauri commands (mirror the add_bot/remove_bot command shape)**

```rust
#[tauri::command]
pub async fn get_bot_poll_interval(state: State<'_, Arc<AppState>>, server_id: String) -> Result<u64, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::GetBotPollInterval).await.map_err(|e| e.to_string())? {
        ServerResponse::BotPollInterval { secs } => Ok(secs),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn set_bot_poll_interval(state: State<'_, Arc<AppState>>, server_id: String, secs: u64) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::SetBotPollInterval { secs }).await.map_err(|e| e.to_string())? {
        ServerResponse::Ok => Ok(()),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}
```

Register both in `client/src-tauri/src/main.rs` `generate_handler!` (next to `commands::add_bot`).

- [ ] **Step 2: Bridge fns**

In `tauri-bridge.ts` (near `addBot`):

```ts
export async function getBotPollInterval(serverId: string): Promise<number> {
  return invoke("get_bot_poll_interval", { serverId });
}
export async function setBotPollInterval(serverId: string, secs: number): Promise<void> {
  return invoke("set_bot_poll_interval", { serverId, secs });
}
```

- [ ] **Step 3: Interval field in BotsTab**

In `BotsTab.tsx`, add state + load-on-mount + a save control. Near the other `useState`s:

```tsx
  const [interval, setIntervalSecs] = useState<number>(60);
  useEffect(() => {
    api.getBotPollInterval(serverId).then(setIntervalSecs).catch(() => {});
  }, [serverId]);
```

Add a small section (reuse `connect-*` classes) below the section title, above the bot list:

```tsx
      <div style={{ display: "flex", gap: 6, alignItems: "flex-end", marginBottom: 10 }}>
        <div>
          <label className="connect-label">Update interval (seconds, min 30)</label>
          <input
            className="connect-input"
            type="number"
            min={30}
            value={interval}
            onChange={(e) => setIntervalSecs(Number(e.target.value))}
          />
        </div>
        <button
          className="organizer-btn"
          onClick={async () => {
            try { await api.setBotPollInterval(serverId, Math.max(30, Math.floor(interval))); }
            catch (e) { console.error("[bots:set-interval]", e); }
          }}
        >Save</button>
      </div>
```

(Import `useEffect` if not already imported. The server clamps to ≥30 regardless; the `min={30}` + `Math.max` are UI guards.)

- [ ] **Step 4: build + seam + tsc**

Run: `cd client/src-tauri && cargo build 2>&1 | tail` (clean); `grep -n 'bot_poll_interval' client/src-tauri/src/main.rs` (both commands registered); `cd client && npx tsc --noEmit 2>&1 | tail` (clean).

- [ ] **Step 5: Commit**

```bash
git add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src/lib/tauri-bridge.ts client/src/components/BotsTab.tsx
git commit -m "feat(bots): Bots panel poll-interval control"
```

---

### Task 3: Docs

**Files:** `docs/modules/tauri-commands.md`, the bridge doc, `docs/modules/protocol.md`, a server doc.

- [ ] **Step 1: Document the new surfaces**

- `tauri-commands.md`: `get_bot_poll_interval(server_id) -> u64`, `set_bot_poll_interval(server_id, secs)` (owner-gated; clamped ≥30).
- bridge doc: `getBotPollInterval`/`setBotPollInterval`.
- `protocol.md`: `ServerRequest::SetBotPollInterval`/`GetBotPollInterval`, `ServerResponse::BotPollInterval`; note the `server_settings` KV table.
- server doc: the configurable interval (poll-then-sleep, live-read, floor 30s), the AddBot coin-id/label validation, and the "unknown coin" presence for coins CoinGecko omits.

- [ ] **Step 2: Commit**

```bash
git add docs/
git commit -m "docs(bots): configurable poll interval + coin validation"
```

---

## Owner runtime verification (server changed → full rebuild incl. sidecar)

`git pull` → `cargo build -p farder-server` → STOP app → `.\client\src-tauri\binaries\copy-sidecar.ps1` (repo root) → `cd client; npm run tauri dev` → Ctrl+Shift+R. Then:
1. Server settings → Bots → set **Update interval** to 30 → Save → prices refresh roughly every 30s (watch a bot tick).
2. Add a **Custom** coin with a bogus id (e.g. `notacoin`) → within one cycle it shows **"unknown coin"** (not stuck on "fetching price…").
3. Existing valid tickers keep working; a non-owner still has no Bots tab.

## Self-review notes

- Interval: server_settings KV + get/set (floor 30, default 60) + owner-gated Set/Get requests + poll-loop reads it live → Task 1; client input → Task 2.
- Coin validation: charset/length guard on AddBot (Task 1) + poller marks CoinGecko-omitted coins "unknown coin" (Task 1); no client render change needed (formatPresence already renders `Ticker.details` inline, so "unknown coin" shows automatically).
- Length-bounding label/coin_id (prior fast-follow #2) folded into the AddBot guard (Task 1).
- Privacy preserved: validation is server-side; no client-side CoinGecko call.
