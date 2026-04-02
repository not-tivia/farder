use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::messages::QueuedMessage;
use rusqlite::Connection;

pub struct MessageStore {
    conn: Connection,
}

impl MessageStore {
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
            "CREATE TABLE IF NOT EXISTS queued_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                recipient BLOB NOT NULL,
                sender BLOB NOT NULL,
                payload BLOB NOT NULL,
                timestamp INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_recipient ON queued_messages(recipient);",
        )?;
        Ok(())
    }

    pub fn queue_message(
        &self,
        recipient: &PublicKey,
        sender: &PublicKey,
        payload: &[u8],
        timestamp: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO queued_messages (recipient, sender, payload, timestamp) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                recipient.as_bytes().as_slice(),
                sender.as_bytes().as_slice(),
                payload,
                timestamp as i64
            ],
        )?;
        Ok(())
    }

    pub fn fetch_messages(&self, recipient: &PublicKey) -> Result<Vec<QueuedMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT sender, payload, timestamp FROM queued_messages WHERE recipient = ?1 ORDER BY id",
        )?;
        let messages: Vec<QueuedMessage> = stmt
            .query_map(
                rusqlite::params![recipient.as_bytes().as_slice()],
                |row| {
                    let sender_bytes: Vec<u8> = row.get(0)?;
                    let payload: Vec<u8> = row.get(1)?;
                    let timestamp: i64 = row.get(2)?;
                    let mut key_bytes = [0u8; 32];
                    key_bytes.copy_from_slice(&sender_bytes);
                    Ok(QueuedMessage {
                        sender: PublicKey::from_bytes(key_bytes),
                        payload,
                        timestamp: timestamp as u64,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        self.conn.execute(
            "DELETE FROM queued_messages WHERE recipient = ?1",
            rusqlite::params![recipient.as_bytes().as_slice()],
        )?;
        Ok(messages)
    }

    pub fn pending_count(&self, recipient: &PublicKey) -> Result<u32> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM queued_messages WHERE recipient = ?1",
            rusqlite::params![recipient.as_bytes().as_slice()],
            |row| row.get(0),
        )?;
        Ok(count as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::identity::Keypair;

    #[test]
    fn test_store_and_fetch_messages() {
        let store = MessageStore::open_in_memory().unwrap();
        let alice = Keypair::generate().public_key();
        let sender1 = Keypair::generate().public_key();
        let sender2 = Keypair::generate().public_key();

        store.queue_message(&alice, &sender1, b"hello", 1000).unwrap();
        store.queue_message(&alice, &sender2, b"world", 2000).unwrap();

        let messages = store.fetch_messages(&alice).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].payload, b"hello");
        assert_eq!(messages[0].timestamp, 1000);
        assert_eq!(messages[1].payload, b"world");
        assert_eq!(messages[1].timestamp, 2000);
    }

    #[test]
    fn test_fetch_clears_messages() {
        let store = MessageStore::open_in_memory().unwrap();
        let alice = Keypair::generate().public_key();
        let sender = Keypair::generate().public_key();

        store.queue_message(&alice, &sender, b"msg", 1000).unwrap();
        let first = store.fetch_messages(&alice).unwrap();
        assert_eq!(first.len(), 1);

        let second = store.fetch_messages(&alice).unwrap();
        assert_eq!(second.len(), 0);
    }

    #[test]
    fn test_pending_count() {
        let store = MessageStore::open_in_memory().unwrap();
        let alice = Keypair::generate().public_key();
        let sender = Keypair::generate().public_key();

        assert_eq!(store.pending_count(&alice).unwrap(), 0);
        store.queue_message(&alice, &sender, b"a", 1000).unwrap();
        store.queue_message(&alice, &sender, b"b", 2000).unwrap();
        assert_eq!(store.pending_count(&alice).unwrap(), 2);
    }

    #[test]
    fn test_messages_isolated_by_recipient() {
        let store = MessageStore::open_in_memory().unwrap();
        let alice = Keypair::generate().public_key();
        let bob = Keypair::generate().public_key();
        let sender = Keypair::generate().public_key();

        store.queue_message(&alice, &sender, b"for alice", 1000).unwrap();
        store.queue_message(&bob, &sender, b"for bob", 2000).unwrap();

        let alice_msgs = store.fetch_messages(&alice).unwrap();
        assert_eq!(alice_msgs.len(), 1);
        assert_eq!(alice_msgs[0].payload, b"for alice");

        let bob_msgs = store.fetch_messages(&bob).unwrap();
        assert_eq!(bob_msgs.len(), 1);
        assert_eq!(bob_msgs[0].payload, b"for bob");
    }
}
