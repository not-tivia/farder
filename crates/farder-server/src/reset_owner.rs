//! Host-side owner reset: `farder-server --reset-owner vk_<member key>`.
//!
//! For a server whose owner lost the account and never made a recovery key. Whoever has a
//! shell on the host (and so the database) is trusted physically; the account key alone is
//! not enough to do this. The server's identity is a hash of its first record, which fixes the
//! owner, so the owner cannot be edited in place. Instead the old signed history is moved
//! aside (never deleted) and a NEW first record is written that names the new owner.
//!
//! Carries over: channels, plaintext messages, files, members, roles. Does not carry over:
//! the signed history itself (members' device authorizations) and end-to-end encrypted
//! channel membership; friends rejoin with a fresh invite.

use anyhow::{bail, Context, Result};
use farder_crypto::event_log::Genesis;
use farder_crypto::identity::PublicKey;
use rusqlite::Connection;

/// What a reset did, for the host's terminal.
#[derive(Debug)]
pub struct ResetReport {
    pub old_server_id: String,
    pub new_server_id: String,
    pub events_moved_aside: usize,
}

/// Parse `vk_<64 hex>` (the form the app shows) or bare 64 hex.
pub fn parse_member_key(s: &str) -> Result<PublicKey> {
    let hexs = s.trim().strip_prefix("vk_").unwrap_or(s.trim());
    let bytes = hex::decode(hexs).context("the key must be hex (vk_ followed by 64 hex characters)")?;
    let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| anyhow::anyhow!("the key must be 32 bytes (64 hex characters)"))?;
    Ok(PublicKey::from_bytes(arr))
}

/// Do the reset on an OPEN database. The caller has made a backup and stopped the server.
/// `force` allows a reset even though a recovery key exists (normally the takeover is the
/// right tool and a reset is refused).
pub fn reset_owner(conn: &mut Connection, new_owner: &PublicKey, force: bool, now: u64) -> Result<ResetReport> {
    let old = crate::event_ingest::load_genesis(conn)?
        .context("this server has no signed history yet (nothing has been claimed), so there is no owner to reset")?;
    let state = crate::event_ingest::build_log_state(conn)?.context("could not rebuild the signed history")?;
    if state.recovery_key().is_some() && !force {
        bail!("this server HAS a recovery key. Use the takeover in Server Settings > Recovery instead (it is safer). Run again with --force only if the recovery words are lost too.");
    }
    if state.owner() == new_owner {
        bail!("that key is already the owner");
    }
    let key_bytes = new_owner.as_bytes().to_vec();
    let ok: i64 = conn.query_row(
        "SELECT COUNT(*) FROM members WHERE public_key = ?1 AND banned = 0 AND revoked = 0",
        [&key_bytes], |r| r.get(0))?;
    if ok == 0 {
        bail!("that key is not an active member of this server (the new owner must already have joined)");
    }
    let old_id = old.server_id();
    let genesis = Genesis { version: old.version, name: old.name.clone(), owner: new_owner.clone(), created_at: now, nonce: rand::random::<[u8; 16]>() };
    let new_id = genesis.server_id();
    let tx = conn.transaction()?;
    let n: usize = tx.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))? as usize;
    let tag = format!("{}", now);
    // Keep the old history under a dated name (a plain copy, so nothing is lost).
    tx.execute_batch(&format!(
        "CREATE TABLE events_before_reset_{tag} AS SELECT * FROM events;
         CREATE TABLE genesis_before_reset_{tag} AS SELECT * FROM genesis;
         DELETE FROM events;
         DELETE FROM genesis;"))?;
    tx.execute("INSERT INTO genesis (singleton, genesis_body, server_id) VALUES (0, ?1, ?2)", rusqlite::params![genesis.to_bytes(), new_id])?;
    // Remember that the host reset the owner, so people who are no longer in the new history are told
    // what happened ("rejoin with a fresh invite") instead of a bare "not a member".
    tx.execute(
        "INSERT INTO server_settings (key, value) VALUES ('owner_reset_at', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![now.to_string()],
    )?;
    tx.commit()?;
    Ok(ResetReport { old_server_id: old_id, new_server_id: new_id, events_moved_aside: n })
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::identity::Keypair;

    fn db_with(owner: &Keypair, member: &Keypair) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        let g = Genesis { version: 1, name: "t".into(), owner: owner.public_key(), created_at: 1, nonce: [1; 16] };
        crate::event_ingest::save_genesis(&conn, &g).unwrap();
        for (k, n) in [(owner, "O"), (member, "M")] {
            conn.execute("INSERT INTO members (public_key, display_name, joined_at) VALUES (?1, ?2, 1)",
                rusqlite::params![k.public_key().as_bytes().to_vec(), n]).unwrap();
        }
        conn
    }

    #[test]
    fn a_reset_makes_the_member_the_owner_and_survives_a_refold() {
        let (a, b) = (Keypair::generate(), Keypair::generate());
        let mut conn = db_with(&a, &b);
        let r = reset_owner(&mut conn, &b.public_key(), false, 100).unwrap();
        assert_ne!(r.old_server_id, r.new_server_id);
        let st = crate::event_ingest::build_log_state(&conn).unwrap().unwrap();
        assert_eq!(st.owner(), &b.public_key());
        assert_ne!(st.owner(), &a.public_key());
        // The old history is kept, not deleted.
        let kept: i64 = conn.query_row("SELECT COUNT(*) FROM genesis_before_reset_100", [], |r| r.get(0)).unwrap();
        assert_eq!(kept, 1);
    }

    #[test]
    fn a_reset_refuses_a_stranger_and_the_current_owner() {
        let (a, b) = (Keypair::generate(), Keypair::generate());
        let mut conn = db_with(&a, &b);
        let stranger = Keypair::generate();
        assert!(reset_owner(&mut conn, &stranger.public_key(), false, 1).unwrap_err().to_string().contains("not an active member"));
        assert!(reset_owner(&mut conn, &a.public_key(), false, 1).unwrap_err().to_string().contains("already the owner"));
    }

    #[test]
    fn the_key_parser_takes_the_app_form() {
        let k = Keypair::generate().public_key();
        let s = format!("vk_{}", hex::encode(k.as_bytes()));
        assert_eq!(parse_member_key(&s).unwrap(), k);
        assert!(parse_member_key("vk_12").is_err());
    }
}
