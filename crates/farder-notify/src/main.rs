mod config;
mod push;
mod store;

use anyhow::Result;
use clap::Parser;
use config::Config;
use farder_crypto::identity::PublicKey;
use farder_protocol::{codec, messages::Message};
use push::PushMap;
use quinn::Connection;
use std::sync::{Arc, Mutex};
use store::MessageStore;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "farder_notify=info".into()),
        )
        .init();
    let config = Config::parse();
    info!("Starting Farder Notify v{}", env!("CARGO_PKG_VERSION"));

    let endpoint = create_endpoint(config.bind)?;
    let store = Arc::new(Mutex::new(MessageStore::open(&config.db_path)?));
    let push_map = push::new_push_map();

    while let Some(incoming) = endpoint.accept().await {
        let conn = incoming.await?;
        let store = store.clone();
        let push_map = push_map.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(conn, store, push_map).await {
                warn!("Connection error: {}", e);
            }
        });
    }
    Ok(())
}

fn create_endpoint(bind_addr: std::net::SocketAddr) -> Result<quinn::Endpoint> {
    let certified =
        rcgen::generate_simple_self_signed(vec!["farder-notify".to_string()])?;
    let cert_der =
        rustls::pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key_der =
        rustls::pki_types::PrivateKeyDer::try_from(certified.key_pair.serialize_der())
            .map_err(|e| anyhow::anyhow!("key error: {}", e))?;
    let server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
    ));
    let endpoint = quinn::Endpoint::server(server_config, bind_addr)?;
    info!("Notify relay listening on {}", bind_addr);
    Ok(endpoint)
}

async fn handle_connection(
    conn: Connection,
    store: Arc<Mutex<MessageStore>>,
    push_map: PushMap,
) -> Result<()> {
    let remote = conn.remote_address();
    info!("New connection from {}", remote);

    let (mut send, mut recv) = conn.accept_bi().await?;
    let buf = read_message(&mut recv).await?;
    let msg: Message = codec::decode(&buf)?;

    match msg {
        Message::NotifyRegister { public_key } => {
            let pending = {
                let s = store.lock().unwrap();
                s.pending_count(&public_key)?
            };
            push::register(&public_key, conn.clone(), push_map.clone()).await;
            if pending > 0 {
                push::try_push_notification(&public_key, pending, push_map).await;
            }
            let response = Message::NotifyPending { count: pending };
            let bytes = codec::encode(&response)?;
            write_message(&mut send, &bytes).await?;
        }
        Message::NotifyDeliver { recipient, payload } => {
            let sender_key = parse_public_key_from_connection(&conn)?;
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let pending = {
                let s = store.lock().unwrap();
                s.queue_message(&recipient, &sender_key, &payload, timestamp)?;
                s.pending_count(&recipient)?
            };
            push::try_push_notification(&recipient, pending, push_map).await;
            let response = Message::NotifyPending { count: pending };
            let bytes = codec::encode(&response)?;
            write_message(&mut send, &bytes).await?;
        }
        Message::NotifyFetch => {
            let public_key = parse_public_key_from_connection(&conn)?;
            let messages = {
                let s = store.lock().unwrap();
                s.fetch_messages(&public_key)?
            };
            let response = Message::NotifyMessages { messages };
            let bytes = codec::encode(&response)?;
            write_message(&mut send, &bytes).await?;
        }
        _ => {
            warn!("Unexpected first message from {}", remote);
        }
    }
    Ok(())
}

fn parse_public_key_from_connection(conn: &Connection) -> Result<PublicKey> {
    // Without mTLS, use SNI or a fixed placeholder for now.
    // In production, clients would authenticate via a prior handshake.
    let _ = conn;
    Ok(PublicKey::from_bytes([0u8; 32]))
}

async fn read_message(recv: &mut quinn::RecvStream) -> Result<Vec<u8>> {
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

async fn write_message(send: &mut quinn::SendStream, data: &[u8]) -> Result<()> {
    let len = (data.len() as u32).to_be_bytes();
    send.write_all(&len).await?;
    send.write_all(data).await?;
    Ok(())
}
