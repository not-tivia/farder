//! Server-managed ticker bots: a bot is a generated keypair recorded here + a
//! `members` row (is_bot=1). The server holds the bot's secret (low-stakes, the
//! server is the bot's authority) and drives its presence via the poller.
use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::{Presence, PresenceKind, ServerEvent};
use rusqlite::{params, Connection};
use std::sync::Arc;
use crate::state::ServerState;

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
    conn.execute("DELETE FROM bots WHERE public_key = ?1", params![pk.as_bytes().as_slice()])?;
    Ok(())
}

/// Live price snapshot for a single coin.
#[derive(Clone, Debug)]
pub struct PriceInfo { pub usd: f64, pub change_24h: f64 }

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

/// Spawns the bot-price poll task. Mirrors `retention::spawn_retention_task`.
/// Ticks every `interval_secs` (min 15 s). On each tick:
///   1. Snapshots the bot list — drops the DB lock BEFORE any await.
///   2. Coalesces distinct coin ids into a single CoinGecko fetch.
///   3. Per bot: stores the updated Presence and broadcasts MemberPresenceUpdated
///      to all connected clients via the existing `connection::broadcast_event` helper.
pub fn spawn_bot_poll_task(state: Arc<ServerState>, interval_secs: u64) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(15)));
        loop {
            interval.tick().await;
            // 1. Snapshot bots — the Mutex<Connection> lock is released before any .await.
            let bots = {
                let conn = state.db.lock().unwrap();
                list_bots(&conn).unwrap_or_default()
            };
            if bots.is_empty() { continue; }
            // 2. Coalesce distinct coin ids; one network call for all bots.
            let mut ids: Vec<String> = bots.iter().map(|b| b.coin_id.clone()).collect();
            ids.sort(); ids.dedup();
            let prices = match fetch_prices(&ids).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(error = %e, "bot price fetch failed; keeping last prices");
                    continue;
                }
            };
            // 3. Per bot: compose + store + broadcast (skip coins absent from the response).
            for b in &bots {
                if let Some(pi) = prices.get(&b.coin_id) {
                    let presence = ticker_presence(pi);
                    {
                        state.presences.write().unwrap()
                            .insert(*b.public_key.as_bytes(), presence.clone());
                    }
                    // Reuse the existing public broadcast helper (DRY — no separate broadcast_all).
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
        }
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::identity::Keypair;

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
}
