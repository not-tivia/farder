use crate::db;
use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::ServerEvent;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tokio::sync::{mpsc, RwLock};

pub struct SessionInfo {
    pub public_key: PublicKey,
    pub expires_at: u64,
}

pub type EventSender = mpsc::Sender<ServerEvent>;

pub struct ServerState {
    pub db: Mutex<Connection>,
    pub sessions: RwLock<HashMap<[u8; 32], SessionInfo>>,
    pub clients: RwLock<HashMap<[u8; 32], EventSender>>,
    pub subscriptions: RwLock<HashMap<u64, HashSet<[u8; 32]>>>,
    pub owner: RwLock<Option<PublicKey>>,
    pub setup_token: Mutex<Option<[u8; 32]>>,
    pub server_name: String,
}

impl ServerState {
    pub fn new(conn: Connection, server_name: String) -> Self {
        Self {
            db: Mutex::new(conn),
            sessions: RwLock::new(HashMap::new()),
            clients: RwLock::new(HashMap::new()),
            subscriptions: RwLock::new(HashMap::new()),
            owner: RwLock::new(None),
            setup_token: Mutex::new(None),
            server_name,
        }
    }

    pub fn new_for_test() -> Result<Self> {
        let conn = db::open_in_memory()?;
        Ok(Self::new(conn, "Test Server".to_string()))
    }
}
