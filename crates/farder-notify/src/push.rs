use farder_crypto::identity::PublicKey;
use farder_protocol::{codec, messages::Message};
use quinn::Connection;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

pub type PushMap = Arc<RwLock<HashMap<[u8; 32], Connection>>>;

pub fn new_push_map() -> PushMap {
    Arc::new(RwLock::new(HashMap::new()))
}

pub async fn register(public_key: &PublicKey, conn: Connection, push_map: PushMap) {
    let mut map = push_map.write().await;
    map.insert(*public_key.as_bytes(), conn);
    info!("Registered push for {}", public_key);
}

pub async fn try_push_notification(
    recipient: &PublicKey,
    pending_count: u32,
    push_map: PushMap,
) -> bool {
    let map = push_map.read().await;
    if let Some(conn) = map.get(recipient.as_bytes()) {
        let msg = Message::NotifyPending { count: pending_count };
        if let Ok(bytes) = codec::encode(&msg) {
            if let Ok(mut send) = conn.open_uni().await {
                let len = (bytes.len() as u32).to_be_bytes();
                let _ = send.write_all(&len).await;
                let _ = send.write_all(&bytes).await;
                let _ = send.finish();
                return true;
            }
        }
    }
    false
}
