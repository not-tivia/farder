use anyhow::Result;
use farder_protocol::{codec, messages::Message};
use quinn::{Connection, RecvStream, SendStream};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

pub type ConnectionMap = Arc<RwLock<HashMap<Vec<u8>, Connection>>>;

pub fn new_connection_map() -> ConnectionMap {
    Arc::new(RwLock::new(HashMap::new()))
}

pub async fn handle_connection(conn: Connection, connections: ConnectionMap) -> Result<()> {
    let remote = conn.remote_address();
    info!("New connection from {}", remote);
    let (mut send, mut recv) = conn.accept_bi().await?;
    let buf = read_message(&mut recv).await?;
    let msg: Message = codec::decode(&buf)?;
    match msg {
        Message::RelayConnect { destination_id } => {
            let dest_conn = {
                let map = connections.read().await;
                map.get(&destination_id).cloned()
            };
            if let Some(dest) = dest_conn {
                let response = Message::RelayConnected;
                let response_bytes = codec::encode(&response)?;
                write_message(&mut send, &response_bytes).await?;
                bridge_connections(conn, dest).await?;
            } else {
                let response = Message::RelayError {
                    reason: "destination not connected".to_string(),
                };
                let response_bytes = codec::encode(&response)?;
                write_message(&mut send, &response_bytes).await?;
            }
        }
        _ => {
            warn!("Unexpected first message from {}", remote);
        }
    }
    Ok(())
}

pub async fn register_connection(
    identity: Vec<u8>,
    conn: Connection,
    connections: ConnectionMap,
) {
    let mut map = connections.write().await;
    map.insert(identity, conn);
}

pub async fn unregister_connection(identity: &[u8], connections: ConnectionMap) {
    let mut map = connections.write().await;
    map.remove(identity);
}

async fn bridge_connections(a: Connection, b: Connection) -> Result<()> {
    let a2b = bridge_one_direction(a.clone(), b.clone());
    let b2a = bridge_one_direction(b, a);
    tokio::select! {
        r = a2b => r,
        r = b2a => r,
    }
}

async fn bridge_one_direction(from: Connection, to: Connection) -> Result<()> {
    loop {
        let (mut from_send, mut from_recv) = from.accept_bi().await?;
        let (mut to_send, mut to_recv) = to.open_bi().await?;
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut from_recv, &mut to_send).await;
        });
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut to_recv, &mut from_send).await;
        });
    }
}

pub async fn read_message(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        anyhow::bail!("message too large: {} bytes", len);
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn write_message(send: &mut SendStream, data: &[u8]) -> Result<()> {
    let len = (data.len() as u32).to_be_bytes();
    send.write_all(&len).await?;
    send.write_all(data).await?;
    Ok(())
}
