use farder_protocol::server::ServerResponse;
use quinn::{Connection, SendStream};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use tokio::task::JoinHandle;

pub struct AppState {
    /// Raw signing key bytes for the stored identity keypair.
    pub signing_key_bytes: Mutex<Option<[u8; 32]>>,
    /// The active QUIC connection (must be kept alive or streams die).
    pub connection: Mutex<Option<Connection>>,
    /// The active QUIC send stream to the server.
    pub send_stream: tokio::sync::Mutex<Option<SendStream>>,
    /// Monotonically increasing request ID counter.
    pub next_request_id: AtomicU32,
    /// Pending request oneshot senders keyed by request ID.
    pub pending_requests: Mutex<HashMap<u32, tokio::sync::oneshot::Sender<ServerResponse>>>,
    /// Handle to the background event reader task.
    pub event_reader_handle: Mutex<Option<JoinHandle<()>>>,
    /// Whether the client is currently connected to a server.
    pub connected: Mutex<bool>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            signing_key_bytes: Mutex::new(None),
            connection: Mutex::new(None),
            send_stream: tokio::sync::Mutex::new(None),
            next_request_id: AtomicU32::new(1),
            pending_requests: Mutex::new(HashMap::new()),
            event_reader_handle: Mutex::new(None),
            connected: Mutex::new(false),
        }
    }

    pub fn next_id(&self) -> u32 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }
}
