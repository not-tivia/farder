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
}

impl ServerConnection {
    pub fn next_id(&self) -> u32 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }
}

pub struct AppState {
    pub signing_key_bytes: Mutex<Option<[u8; 32]>>,
    pub servers: Mutex<HashMap<String, Arc<ServerConnection>>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            signing_key_bytes: Mutex::new(None),
            servers: Mutex::new(HashMap::new()),
        }
    }

    pub fn get_server(&self, server_id: &str) -> Result<Arc<ServerConnection>, String> {
        let servers = self.servers.lock().map_err(|e| e.to_string())?;
        servers.get(server_id).cloned().ok_or_else(|| format!("not connected to {}", server_id))
    }
}
