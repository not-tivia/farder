use crate::db;
use crate::media_stream;
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
    pub is_owner: bool,
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
            media: media_stream::MediaStateMap::new(),
        }
    }

    /// Register an authenticated session by its login token (relay file streams
    /// look this up to learn whose stream they are). The entry is removed when
    /// the primary session ends.
    pub async fn register_session(&self, token: [u8; 32], public_key: PublicKey, is_owner: bool) {
        let mut sessions = self.sessions.write().await;
        sessions.insert(token, SessionInfo { public_key, is_owner, expires_at: u64::MAX });
    }

    /// Look up a session token, returning `(public_key, is_owner)` if present.
    pub async fn lookup_session(&self, token: &[u8; 32]) -> Option<(PublicKey, bool)> {
        let sessions = self.sessions.read().await;
        sessions.get(token).map(|s| (s.public_key.clone(), s.is_owner))
    }

    /// Remove a session token (called when the primary session ends).
    pub async fn remove_session(&self, token: &[u8; 32]) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(token);
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

#[cfg(test)]
mod session_registry_tests {
    use super::*;

    fn empty_state() -> ServerState {
        ServerState::new_for_test().expect("build in-memory ServerState")
    }

    #[tokio::test]
    async fn session_register_lookup_remove() {
        let state = empty_state();
        let pk = farder_crypto::identity::Keypair::generate().public_key();
        let token = [7u8; 32];
        assert!(state.lookup_session(&token).await.is_none());
        state.register_session(token, pk.clone(), true).await;
        let got = state.lookup_session(&token).await.expect("present");
        assert_eq!(got.0, pk);
        assert!(got.1, "is_owner preserved");
        state.remove_session(&token).await;
        assert!(state.lookup_session(&token).await.is_none());
    }
}
