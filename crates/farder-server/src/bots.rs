//! Server-managed ticker bots: a bot is a generated keypair recorded here + a
//! `members` row (is_bot=1). The server holds the bot's secret (low-stakes, the
//! server is the bot's authority) and drives its presence via the poller.
use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::{Presence, PresenceKind, ServerEvent};
use rusqlite::{params, Connection};
use std::sync::Arc;
use crate::state::ServerState;
use crate::events::EventTarget;

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
        let arr: [u8; 32] = pk_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("bad bot pk: wrong length"))?;
        let pk = PublicKey::from_bytes(arr);
        out.push(BotRecord { public_key: pk, coin_id, label });
    }
    Ok(out)
}

pub fn remove_bot(conn: &Connection, pk: &PublicKey) -> Result<()> {
    // Cascade: remove all alerts and subscriptions for this bot before removing the bot row.
    conn.execute("DELETE FROM bot_alerts WHERE bot_public_key = ?1", params![pk.as_bytes().as_slice()])?;
    conn.execute("DELETE FROM bot_subscriptions WHERE bot_public_key = ?1", params![pk.as_bytes().as_slice()])?;
    conn.execute("DELETE FROM bots WHERE public_key = ?1", params![pk.as_bytes().as_slice()])?;
    Ok(())
}

/// Insert a new price alert for a bot. Returns the newly-created alert id.
pub fn add_alert(conn: &Connection, bot: &PublicKey, metric: &str, comparator: &str, threshold: f64) -> Result<i64> {
    conn.execute(
        "INSERT INTO bot_alerts (bot_public_key, metric, comparator, threshold, armed, created_at) \
         VALUES (?1, ?2, ?3, ?4, 1, ?5)",
        params![bot.as_bytes().as_slice(), metric, comparator, threshold, crate::db::now() as i64],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Delete a price alert by id.
pub fn remove_alert(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM bot_alerts WHERE id = ?1", params![id])?;
    Ok(())
}

/// Subscribe `subscriber` to alerts for `bot` (INSERT OR IGNORE — idempotent).
pub fn subscribe(conn: &Connection, bot: &PublicKey, subscriber: &PublicKey) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO bot_subscriptions (bot_public_key, subscriber_public_key, created_at) \
         VALUES (?1, ?2, ?3)",
        params![bot.as_bytes().as_slice(), subscriber.as_bytes().as_slice(), crate::db::now() as i64],
    )?;
    Ok(())
}

/// Unsubscribe `subscriber` from alerts for `bot`.
pub fn unsubscribe(conn: &Connection, bot: &PublicKey, subscriber: &PublicKey) -> Result<()> {
    conn.execute(
        "DELETE FROM bot_subscriptions WHERE bot_public_key = ?1 AND subscriber_public_key = ?2",
        params![bot.as_bytes().as_slice(), subscriber.as_bytes().as_slice()],
    )?;
    Ok(())
}

/// List all bot public keys that `subscriber` is subscribed to.
pub fn list_subscriptions_for_user(conn: &Connection, subscriber: &PublicKey) -> Result<Vec<PublicKey>> {
    let mut stmt = conn.prepare(
        "SELECT bot_public_key FROM bot_subscriptions WHERE subscriber_public_key = ?1"
    )?;
    let rows = stmt.query_map(params![subscriber.as_bytes().as_slice()], |r| r.get::<_, Vec<u8>>(0))?;
    let mut out = Vec::new();
    for row in rows {
        let b = row?;
        let arr: [u8; 32] = b.try_into().map_err(|_| anyhow::anyhow!("bad bot pk in subscriptions"))?;
        out.push(PublicKey::from_bytes(arr));
    }
    Ok(out)
}

/// Live price snapshot for a single coin.
#[derive(Clone, Debug)]
pub struct PriceInfo { pub usd: f64, pub change_24h: f64 }

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

/// Compose a ticker presence: details = "$<price> <arrow><pct>%", state = "24h".
pub fn ticker_presence(p: &PriceInfo) -> Presence {
    let arrow = if p.change_24h >= 0.0 { '\u{25B2}' } else { '\u{25BC}' }; // up / down
    let details = format!("${:.2} {}{:.2}%", p.usd, arrow, p.change_24h.abs());
    Presence { kind: PresenceKind::Ticker, details, state: Some("24h".into()) }
}

/// Fetch USD price + 24h change for the given CoinGecko ids in ONE call.
/// Returns a map coin_id -> PriceInfo. Network — not unit-tested; SSRF-guarded.
pub async fn fetch_prices(coin_ids: &[String]) -> anyhow::Result<std::collections::HashMap<String, PriceInfo>> {
    use std::collections::HashMap;
    if coin_ids.is_empty() { return Ok(HashMap::new()); }
    // CoinGecko ids are [a-z0-9-] — safe to interpolate directly without encoding.
    let ids = coin_ids.join(",");
    let url = format!(
        "https://api.coingecko.com/api/v3/simple/price?ids={}&vs_currencies=usd&include_24hr_change=true",
        ids
    );
    if !crate::ssrf::resolves_to_global(&url).await {
        anyhow::bail!("coingecko url did not resolve to a global address");
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let resp = client
        .get(&url)
        .header("accept", "application/json")
        .header("user-agent", "farder-bot/1.0")
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await?;
    // reqwest does NOT error on a non-2xx status — surface it explicitly so a
    // rate-limit / block (429/403) becomes a logged failure, not a silent empty map.
    if !status.is_success() {
        anyhow::bail!(
            "coingecko returned {}: {}",
            status,
            body.chars().take(200).collect::<String>()
        );
    }
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

// ---------------------------------------------------------------------------
// Alert engine
// ---------------------------------------------------------------------------

/// Fire-once with hysteresis: returns (fired, new_armed).
/// - If armed and condition met  -> (true, false)  — fire and disarm.
/// - If disarmed and condition cleared -> (false, true) — re-arm.
/// - Otherwise -> (false, current armed state).
pub fn evaluate_alert(value: f64, comparator: &str, threshold: f64, armed: bool) -> (bool, bool) {
    let condition = match comparator {
        "above" => value > threshold,
        "below" => value < threshold,
        _ => false,
    };
    if armed && condition { (true, false) }
    else if !armed && !condition { (false, true) }
    else { (false, armed) }
}

/// Map a metric name to a f64 value from a PriceInfo snapshot.
pub fn metric_value(p: &PriceInfo, metric: &str) -> Option<f64> {
    match metric {
        "price_usd"  => Some(p.usd),
        "change_24h" => Some(p.change_24h),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct AlertRow {
    pub id: i64,
    pub bot_public_key: PublicKey,
    pub metric: String,
    pub comparator: String,
    pub threshold: f64,
    pub armed: bool,
}

pub fn list_alerts_for_bot(conn: &Connection, bot: &PublicKey) -> Result<Vec<AlertRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, metric, comparator, threshold, armed FROM bot_alerts WHERE bot_public_key = ?1"
    )?;
    let rows = stmt.query_map(params![bot.as_bytes().as_slice()], |r| Ok((
        r.get::<_, i64>(0)?,
        r.get::<_, String>(1)?,
        r.get::<_, String>(2)?,
        r.get::<_, f64>(3)?,
        r.get::<_, i64>(4)? != 0,
    )))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, metric, comparator, threshold, armed) = row?;
        out.push(AlertRow { id, bot_public_key: bot.clone(), metric, comparator, threshold, armed });
    }
    Ok(out)
}

pub fn set_alert_armed(conn: &Connection, id: i64, armed: bool) -> Result<()> {
    conn.execute(
        "UPDATE bot_alerts SET armed = ?1 WHERE id = ?2",
        params![armed as i64, id],
    )?;
    Ok(())
}

pub fn list_subscribers_for_bot(conn: &Connection, bot: &PublicKey) -> Result<Vec<PublicKey>> {
    let mut stmt = conn.prepare(
        "SELECT subscriber_public_key FROM bot_subscriptions WHERE bot_public_key = ?1"
    )?;
    let rows = stmt.query_map(params![bot.as_bytes().as_slice()], |r| r.get::<_, Vec<u8>>(0))?;
    let mut out = Vec::new();
    for row in rows {
        let b = row?;
        let arr: [u8; 32] = b.try_into().map_err(|_| anyhow::anyhow!("bad subscriber pk"))?;
        out.push(PublicKey::from_bytes(arr));
    }
    Ok(out)
}

/// Human-readable alert message for a fired alert.
pub fn format_alert_message(label: &str, metric: &str, comparator: &str, threshold: f64, p: &PriceInfo) -> String {
    match metric {
        "price_usd" => format!(
            "\u{1F514} {label} crossed {} ${:.2} \u{2014} now ${:.2}",
            if comparator == "above" { "above" } else { "below" }, threshold, p.usd
        ),
        "change_24h" => format!(
            "\u{1F514} {label} 24h change {} {:.1}% \u{2014} now {:+.1}% (${:.2})",
            if comparator == "above" { "above" } else { "below" }, threshold, p.change_24h, p.usd
        ),
        _ => format!("\u{1F514} {label} alert"),
    }
}

/// Spawns the bot-price poll task. Mirrors `retention::spawn_retention_task`.
/// Polls immediately then sleeps the configured interval (live-read each cycle so
/// changes apply without restart). On each poll cycle:
///   1. Snapshots the bot list — drops the DB lock BEFORE any await.
///   2. Coalesces distinct coin ids into a single CoinGecko fetch.
///   3. Per bot: stores the updated Presence and broadcasts MemberPresenceUpdated
///      to all connected clients via the existing `connection::broadcast_event` helper.
///      After a SUCCESSFUL fetch, a coin absent from the response is marked unknown.
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
                            let pi = prices.get(&b.coin_id);
                            let presence = match pi {
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

                            // Alert evaluation: only for bots with a real PriceInfo.
                            // DB work is scoped so the MutexGuard drops before any .await.
                            if let Some(pi) = pi {
                                // 1. Under the DB lock: evaluate alerts, persist armed changes, collect fires.
                                let fires: Vec<(String, String, f64)> = {
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
                                }; // MutexGuard dropped here
                                if !fires.is_empty() {
                                    // 2. Under a fresh DB lock: load subscribers (no lock held during awaits).
                                    let subscribers = {
                                        let conn = state.db.lock().unwrap();
                                        list_subscribers_for_bot(&conn, &b.public_key).unwrap_or_default()
                                    }; // MutexGuard dropped here
                                    // 3. Now await DMs — no DB lock held.
                                    for (metric, comparator, threshold) in &fires {
                                        let text = format_alert_message(&b.label, metric, comparator, *threshold, pi);
                                        for sub in &subscribers {
                                            if let Err(e) = send_bot_dm(&state, &b.public_key, sub, &text).await {
                                                tracing::warn!(error = %e, "bot alert DM failed");
                                            }
                                        }
                                    }
                                }
                            }
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

// ---------------------------------------------------------------------------
// E2EE DM delivery (bot → user)
// ---------------------------------------------------------------------------

/// Retrieve the Ed25519 secret key stored for a bot (32 raw bytes), if the bot
/// exists in the `bots` table.
pub fn get_bot_secret(conn: &Connection, pk: &PublicKey) -> Result<Option<[u8; 32]>> {
    use rusqlite::OptionalExtension;
    let row: Option<Vec<u8>> = conn
        .query_row(
            "SELECT secret_key FROM bots WHERE public_key = ?1",
            params![pk.as_bytes().as_slice()],
            |r| r.get(0),
        )
        .optional()?;
    match row {
        Some(bytes) => {
            let arr: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("bad bot secret length"))?;
            Ok(Some(arr))
        }
        None => Ok(None),
    }
}

/// Encrypt `text` as a DM from the bot to the recipient, returning hex-encoded
/// ciphertext in the format expected by the client `dm_decrypt` (nonce||ct+tag).
pub fn encrypt_bot_dm(
    bot_ed_sk: &[u8; 32],
    recipient_ed_pk: &[u8; 32],
    text: &str,
) -> Result<String> {
    let shared = farder_crypto::key_exchange::derive_dm_shared_secret(bot_ed_sk, recipient_ed_pk)
        .map_err(|e| anyhow::anyhow!("dm key exchange failed: {e}"))?;
    let ct = farder_crypto::encryption::encrypt(&shared, text.as_bytes())?;
    Ok(hex::encode(ct))
}

/// Send an E2EE DM from a server-managed bot to a user, delivered targeted at
/// the recipient (so it reaches them regardless of which channel they're viewing).
///
/// # No DB lock across `.await`
/// All DB and crypto work is done inside a scoped block that drops the
/// `MutexGuard` before the first `broadcast_event` call.
pub async fn send_bot_dm(
    state: &Arc<ServerState>,
    bot_pk: &PublicKey,
    recipient_pk: &PublicKey,
    text: &str,
) -> Result<()> {
    // --- all DB + crypto work under the lock; collect what broadcasts need ---
    let (channel_info, was_created, message, bot_member) = {
        let conn = state.db.lock().unwrap();

        let bot_sk = match get_bot_secret(&conn, bot_pk)? {
            Some(s) => s,
            None => return Ok(()), // bot not found; nothing to send
        };

        let (channel_id, was_created) =
            crate::channels::open_dm_channel(&conn, bot_pk, recipient_pk)?;

        let hex_ct = encrypt_bot_dm(&bot_sk, recipient_pk.as_bytes(), text)?;

        let msg_id =
            crate::messages::insert_message(&conn, channel_id, bot_pk, &hex_ct, None)?;

        let message = crate::messages::get_message(&conn, msg_id, recipient_pk)?
            .ok_or_else(|| anyhow::anyhow!("bot dm message vanished after insert"))?;

        let channel_info = crate::channels::get_channel(&conn, channel_id)?
            .ok_or_else(|| anyhow::anyhow!("dm channel vanished after open"))?;

        // Build the bot's MemberInfo for DmCreated.participant (DRY with OpenDm handler).
        let bot_member = crate::handlers::build_member_info(&conn, state, bot_pk)?;

        (channel_info, was_created, message, bot_member)
    }; // MutexGuard dropped here — safe to .await below

    // --- targeted broadcasts (async), only to the recipient ---
    if was_created {
        crate::connection::broadcast_event(
            state,
            EventTarget::Members(vec![recipient_pk.clone()]),
            ServerEvent::DmCreated {
                channel: channel_info,
                participant: bot_member,
            },
        )
        .await;
    }
    crate::connection::broadcast_event(
        state,
        EventTarget::Members(vec![recipient_pk.clone()]),
        ServerEvent::NewMessage { message },
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod alert_tests {
    use super::*;

    #[test]
    fn evaluate_alert_fires_once_and_rearms() {
        // above 70000, armed: fires when value exceeds, disarms; re-arms when back below.
        assert_eq!(evaluate_alert(70001.0, "above", 70000.0, true),  (true, false));  // fire, disarm
        assert_eq!(evaluate_alert(70050.0, "above", 70000.0, false), (false, false)); // still over, no re-fire
        assert_eq!(evaluate_alert(69900.0, "above", 70000.0, false), (false, true));  // recovered -> re-arm
        // below -5 (24h %), armed
        assert_eq!(evaluate_alert(-6.0, "below", -5.0, true),  (true, false));
        assert_eq!(evaluate_alert(-4.0, "below", -5.0, false), (false, true));
    }

    #[test]
    fn metric_value_maps_keys() {
        let p = PriceInfo { usd: 67432.0, change_24h: 2.1 };
        assert_eq!(metric_value(&p, "price_usd"),  Some(67432.0));
        assert_eq!(metric_value(&p, "change_24h"), Some(2.1));
        assert_eq!(metric_value(&p, "bogus"),       None);
    }
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

// ---------------------------------------------------------------------------
// Custom monitor bot helpers
// ---------------------------------------------------------------------------

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
/// Note: the 256 KiB cap is applied post-read (soft cap); acceptable for v1 since
/// only server owners can configure custom monitor URLs.
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

#[cfg(test)]
mod custom_bot_tests {
    use super::*;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::identity::Keypair;

    #[test]
    fn bot_dm_encrypts_so_recipient_decrypts() {
        let bot = Keypair::generate();
        let user = Keypair::generate();
        let hex_ct = encrypt_bot_dm(
            bot.signing_key_bytes(),
            user.public_key().as_bytes(),
            "hello from BTC bot",
        )
        .unwrap();
        // recipient decrypts with (their sk, bot pk) — symmetric ECDH
        let shared = farder_crypto::key_exchange::derive_dm_shared_secret(
            user.signing_key_bytes(),
            bot.public_key().as_bytes(),
        )
        .unwrap();
        let ct = hex::decode(&hex_ct).unwrap();
        let pt = farder_crypto::encryption::decrypt(&shared, &ct).unwrap();
        assert_eq!(String::from_utf8(pt).unwrap(), "hello from BTC bot");
    }

    #[test]
    fn poll_interval_defaults_and_clamps() {
        let conn = crate::db::open_in_memory().unwrap();
        assert_eq!(get_poll_interval(&conn), POLL_INTERVAL_DEFAULT); // unset -> default
        set_poll_interval(&conn, 120).unwrap();
        assert_eq!(get_poll_interval(&conn), 120);
        set_poll_interval(&conn, 5).unwrap();                        // below floor -> clamped
        assert_eq!(get_poll_interval(&conn), POLL_INTERVAL_FLOOR);
    }

    #[test]
    fn register_list_remove_roundtrip() {
        let conn = crate::db::open_in_memory().unwrap();
        let kp = Keypair::generate();
        register_bot(&conn, &kp.public_key(), kp.signing_key_bytes().as_slice(), "bitcoin", "BTC").unwrap();
        let bots = list_bots(&conn).unwrap();
        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0].coin_id, "bitcoin");
        assert_eq!(bots[0].label, "BTC");
        remove_bot(&conn, &kp.public_key()).unwrap();
        assert!(list_bots(&conn).unwrap().is_empty());
    }

    #[test]
    fn alert_subscription_cascade_roundtrip() {
        let conn = crate::db::open_in_memory().unwrap();
        let bot_kp = Keypair::generate();
        let bot_pk = bot_kp.public_key();
        let subscriber_kp = Keypair::generate();
        let subscriber_pk = subscriber_kp.public_key();

        register_bot(&conn, &bot_pk, bot_kp.signing_key_bytes().as_slice(), "bitcoin", "BTC").unwrap();

        // Add two alerts.
        let id1 = add_alert(&conn, &bot_pk, "price_usd", "above", 70000.0).unwrap();
        let id2 = add_alert(&conn, &bot_pk, "change_24h", "below", -5.0).unwrap();
        let alerts = list_alerts_for_bot(&conn, &bot_pk).unwrap();
        assert_eq!(alerts.len(), 2, "should have 2 alerts");
        assert!(alerts.iter().any(|a| a.id == id1));
        assert!(alerts.iter().any(|a| a.id == id2));

        // Subscribe a user.
        subscribe(&conn, &bot_pk, &subscriber_pk).unwrap();
        // Idempotent: second subscribe should be a no-op.
        subscribe(&conn, &bot_pk, &subscriber_pk).unwrap();
        let subs = list_subscribers_for_bot(&conn, &bot_pk).unwrap();
        assert_eq!(subs.len(), 1, "should have exactly 1 subscriber");
        assert_eq!(subs[0], subscriber_pk);

        // list_subscriptions_for_user returns the bot.
        let user_subs = list_subscriptions_for_user(&conn, &subscriber_pk).unwrap();
        assert_eq!(user_subs.len(), 1);
        assert_eq!(user_subs[0], bot_pk);

        // remove_alert removes a single alert.
        remove_alert(&conn, id1).unwrap();
        let alerts_after = list_alerts_for_bot(&conn, &bot_pk).unwrap();
        assert_eq!(alerts_after.len(), 1, "should have 1 alert after individual remove");
        assert_eq!(alerts_after[0].id, id2);

        // Unsubscribe.
        unsubscribe(&conn, &bot_pk, &subscriber_pk).unwrap();
        assert!(list_subscribers_for_bot(&conn, &bot_pk).unwrap().is_empty(), "should have no subscribers after unsubscribe");

        // Re-subscribe so we can verify cascade on bot removal.
        subscribe(&conn, &bot_pk, &subscriber_pk).unwrap();
        add_alert(&conn, &bot_pk, "price_usd", "below", 50000.0).unwrap();

        // remove_bot must cascade: both bot_alerts and bot_subscriptions cleared.
        remove_bot(&conn, &bot_pk).unwrap();
        assert!(list_bots(&conn).unwrap().is_empty(), "bots table must be empty");
        assert!(list_alerts_for_bot(&conn, &bot_pk).unwrap().is_empty(), "bot_alerts must be empty after cascade");
        assert!(list_subscribers_for_bot(&conn, &bot_pk).unwrap().is_empty(), "bot_subscriptions must be empty after cascade");
        assert!(list_subscriptions_for_user(&conn, &subscriber_pk).unwrap().is_empty(), "user subscriptions must be empty after cascade");
    }
}
