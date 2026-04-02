use anyhow::Result;
use farder_crypto::identity::PublicKey;
use rusqlite::Connection;

pub struct StoredMessage {
    pub peer: PublicKey,
    pub is_outgoing: bool,
    pub ciphertext: Vec<u8>,
    pub timestamp: u64,
}

pub struct LocalMessageStore {
    conn: Connection,
}

impl LocalMessageStore {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                peer BLOB NOT NULL,
                is_outgoing INTEGER NOT NULL,
                ciphertext BLOB NOT NULL,
                timestamp INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_peer_ts ON messages(peer, timestamp);"
        )?;
        Ok(())
    }

    pub fn save_message(&self, peer: &PublicKey, is_outgoing: bool, ciphertext: &[u8], timestamp: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO messages (peer, is_outgoing, ciphertext, timestamp) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                peer.as_bytes().as_slice(),
                is_outgoing as i64,
                ciphertext,
                timestamp as i64
            ],
        )?;
        Ok(())
    }

    pub fn get_messages(&self, peer: &PublicKey, limit: u32, offset: u32) -> Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT peer, is_outgoing, ciphertext, timestamp FROM messages
             WHERE peer = ?1
             ORDER BY timestamp ASC
             LIMIT ?2 OFFSET ?3"
        )?;

        let rows = stmt.query_map(
            rusqlite::params![peer.as_bytes().as_slice(), limit, offset],
            |row| {
                let peer_bytes: Vec<u8> = row.get(0)?;
                let is_outgoing: i64 = row.get(1)?;
                let ciphertext: Vec<u8> = row.get(2)?;
                let timestamp: i64 = row.get(3)?;
                Ok((peer_bytes, is_outgoing, ciphertext, timestamp))
            },
        )?;

        let mut messages = Vec::new();
        for row in rows {
            let (peer_bytes, is_outgoing, ciphertext, timestamp) = row?;
            let peer_arr: [u8; 32] = peer_bytes.try_into()
                .map_err(|_| rusqlite::Error::InvalidColumnType(0, "peer".to_string(), rusqlite::types::Type::Blob))?;
            messages.push(StoredMessage {
                peer: PublicKey::from_bytes(peer_arr),
                is_outgoing: is_outgoing != 0,
                ciphertext,
                timestamp: timestamp as u64,
            });
        }

        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::identity::Keypair;

    fn make_key() -> PublicKey {
        Keypair::generate().public_key()
    }

    #[test]
    fn test_store_and_retrieve_messages() {
        let store = LocalMessageStore::open_in_memory().unwrap();
        let peer = make_key();
        store.save_message(&peer, true, b"outgoing ciphertext", 1000).unwrap();
        store.save_message(&peer, false, b"incoming ciphertext", 2000).unwrap();

        let messages = store.get_messages(&peer, 10, 0).unwrap();
        assert_eq!(messages.len(), 2);
        assert!(messages[0].is_outgoing);
        assert!(!messages[1].is_outgoing);
    }

    #[test]
    fn test_messages_ordered_by_timestamp() {
        let store = LocalMessageStore::open_in_memory().unwrap();
        let peer = make_key();
        store.save_message(&peer, true, b"msg3", 3000).unwrap();
        store.save_message(&peer, true, b"msg1", 1000).unwrap();
        store.save_message(&peer, true, b"msg2", 2000).unwrap();

        let messages = store.get_messages(&peer, 10, 0).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].timestamp, 1000);
        assert_eq!(messages[1].timestamp, 2000);
        assert_eq!(messages[2].timestamp, 3000);
    }

    #[test]
    fn test_pagination() {
        let store = LocalMessageStore::open_in_memory().unwrap();
        let peer = make_key();
        for i in 0..10u64 {
            store.save_message(&peer, true, b"msg", i * 1000).unwrap();
        }

        let page1 = store.get_messages(&peer, 3, 0).unwrap();
        assert_eq!(page1.len(), 3);
        assert_eq!(page1[0].timestamp, 0);
        assert_eq!(page1[2].timestamp, 2000);

        let page2 = store.get_messages(&peer, 3, 3).unwrap();
        assert_eq!(page2.len(), 3);
        assert_eq!(page2[0].timestamp, 3000);
        assert_eq!(page2[2].timestamp, 5000);
    }

    #[test]
    fn test_messages_isolated_by_peer() {
        let store = LocalMessageStore::open_in_memory().unwrap();
        let alice = make_key();
        let bob = make_key();

        store.save_message(&alice, true, b"alice msg", 1000).unwrap();
        store.save_message(&alice, false, b"alice msg 2", 2000).unwrap();
        store.save_message(&bob, true, b"bob msg", 1500).unwrap();

        let alice_msgs = store.get_messages(&alice, 10, 0).unwrap();
        let bob_msgs = store.get_messages(&bob, 10, 0).unwrap();

        assert_eq!(alice_msgs.len(), 2);
        assert_eq!(bob_msgs.len(), 1);
    }
}
