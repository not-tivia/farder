use crate::voice::MediaInboundDispatcher;
use farder_protocol::server::ServerResponse;
use quinn::{Connection, Endpoint, SendStream};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::sync::Arc;
use tokio::task::JoinHandle;

pub struct ServerConnection {
    pub endpoint: Endpoint,
    pub connection: Connection,
    pub send_stream: tokio::sync::Mutex<SendStream>,
    pub next_request_id: AtomicU32,
    pub pending_requests: Mutex<HashMap<u32, tokio::sync::oneshot::Sender<ServerResponse>>>,
    pub event_reader_handle: Mutex<Option<JoinHandle<()>>>,
    pub server_name: Mutex<String>,
    /// Dispatcher for inbound QUIC media datagrams; fed by the datagram recv
    /// loop spawned in commands.rs immediately after authentication.
    pub media_dispatcher: Arc<MediaInboundDispatcher>,
    /// Session token issued at login; presented on relay file-transfer streams.
    pub session_token: Vec<u8>,
    /// True if this connection is routed through a relay (so file streams need a
    /// RelayStreamRole::Session marker and voice is unavailable).
    pub relayed: bool,
}

impl ServerConnection {
    pub fn next_id(&self) -> u32 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }
}

pub struct AppState {
    pub signing_key_bytes: Mutex<Option<[u8; 32]>>,
    pub servers: Mutex<HashMap<String, Arc<ServerConnection>>>,
    /// Serializes all device-chain critical sections (load → mutate → save)
    /// across the three commands that write the per-(server,device) state file.
    /// Must be tokio::sync::Mutex so it can be held across `.await` points.
    pub device_chain_lock: tokio::sync::Mutex<()>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            signing_key_bytes: Mutex::new(None),
            servers: Mutex::new(HashMap::new()),
            device_chain_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn get_server(&self, server_id: &str) -> Result<Arc<ServerConnection>, String> {
        let servers = self.servers.lock().map_err(|e| e.to_string())?;
        servers.get(server_id).cloned().ok_or_else(|| format!("not connected to {}", server_id))
    }
}
