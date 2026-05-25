use crate::db;
use crate::media_stream;
use crate::voice;
use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::ServerEvent;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use tokio::sync::{mpsc, RwLock};

pub struct RateLimiter {
    pub users: Mutex<HashMap<[u8; 32], VecDeque<u64>>>,
    pub max_per_window: usize,
    pub window_secs: u64,
}

impl RateLimiter {
    pub fn new(max_per_window: usize, window_secs: u64) -> Self {
        Self {
            users: Mutex::new(HashMap::new()),
            max_per_window,
            window_secs,
        }
    }

    /// Returns true if allowed; false if over the limit. Records the timestamp on success.
    pub fn allow(&self, user: &[u8; 32]) -> bool {
        let now = crate::db::now();
        let mut users = self.users.lock().unwrap();
        let queue = users.entry(*user).or_insert_with(VecDeque::new);
        while let Some(&front) = queue.front() {
            if now.saturating_sub(front) >= self.window_secs {
                queue.pop_front();
            } else {
                break;
            }
        }
        if queue.len() >= self.max_per_window {
            return false;
        }
        queue.push_back(now);
        true
    }
}

pub struct SessionInfo {
    pub public_key: PublicKey,
    pub expires_at: u64,
}

pub type EventSender = mpsc::Sender<ServerEvent>;

pub struct ServerState {
    pub db: Mutex<Connection>,
    pub sessions: RwLock<HashMap<[u8; 32], SessionInfo>>,
    pub clients: RwLock<HashMap<[u8; 32], EventSender>>,
    pub voice_connections: RwLock<HashMap<[u8; 32], quinn::Connection>>,
    pub subscriptions: RwLock<HashMap<u64, HashSet<[u8; 32]>>>,
    pub owner: RwLock<Option<PublicKey>>,
    pub setup_token: Mutex<Option<[u8; 32]>>,
    pub server_name: String,
    pub storage_dir: String,
    pub max_file_size: u64,
    pub upload_limiter: RateLimiter,    // 10/min per user
    pub reaction_limiter: RateLimiter,  // 60/min per user
    pub voice: voice::VoiceState,
    pub media: media_stream::MediaStateMap,
}

impl ServerState {
    pub fn new(conn: Connection, server_name: String, storage_dir: String, max_file_size: u64) -> Self {
        Self {
            db: Mutex::new(conn),
            sessions: RwLock::new(HashMap::new()),
            clients: RwLock::new(HashMap::new()),
            voice_connections: RwLock::new(HashMap::new()),
            subscriptions: RwLock::new(HashMap::new()),
            owner: RwLock::new(None),
            setup_token: Mutex::new(None),
            server_name,
            storage_dir,
            max_file_size,
            upload_limiter: RateLimiter::new(10, 60),
            reaction_limiter: RateLimiter::new(60, 60),
            voice: voice::VoiceState::new(),
            media: media_stream::MediaStateMap::new(),
        }
    }

    pub fn new_for_test() -> Result<Self> {
        let conn = db::open_in_memory()?;
        let tmp = std::env::temp_dir().join(format!("farder-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp)?;
        Ok(Self::new(conn, "Test Server".to_string(), tmp.to_string_lossy().to_string(), 50 * 1024 * 1024))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_allows_under_limit() {
        let rl = RateLimiter::new(3, 60);
        let user = [1u8; 32];
        assert!(rl.allow(&user));
        assert!(rl.allow(&user));
        assert!(rl.allow(&user));
        assert!(!rl.allow(&user));
    }

    #[test]
    fn rate_limiter_isolates_users() {
        let rl = RateLimiter::new(1, 60);
        let a = [1u8; 32];
        let b = [2u8; 32];
        assert!(rl.allow(&a));
        assert!(!rl.allow(&a));
        assert!(rl.allow(&b));
    }
}
