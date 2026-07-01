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
