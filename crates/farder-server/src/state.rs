use crate::db;
use crate::media_stream;
use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::{Presence, ServerEvent};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, RwLock as StdRwLock};
use tokio::sync::{mpsc, RwLock};

/// Where a member's voice datagrams are sent. Direct clients have their own
/// QUIC connection; relayed clients share the server's single relay connection
/// and are addressed by their relay-assigned routing handle (Phase 5b).
pub enum VoiceSink {
    Direct(quinn::Connection),
    Relayed { relay: quinn::Connection, handle: u32 },
}

impl VoiceSink {
    pub fn send_datagram(&self, frame: bytes::Bytes) -> Result<(), quinn::SendDatagramError> {
        match self {
            VoiceSink::Direct(c) => c.send_datagram(frame),
            VoiceSink::Relayed { relay, handle } => {
                let mut tagged = Vec::with_capacity(4 + frame.len());
                tagged.extend_from_slice(&handle.to_be_bytes());
                tagged.extend_from_slice(&frame);
                relay.send_datagram(tagged.into())
            }
        }
    }
}

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
        let queue = users.entry(*user).or_default();
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
    pub voice_connections: RwLock<HashMap<[u8; 32], VoiceSink>>,
    pub relay_voice_handles: RwLock<HashMap<u32, [u8; 32]>>,
    pub subscriptions: RwLock<HashMap<u64, HashSet<[u8; 32]>>>,
    pub owner: RwLock<Option<PublicKey>>,
    pub setup_token: Mutex<Option<[u8; 32]>>,
    pub server_name: String,
    pub storage_dir: String,
    pub max_file_size: u64,
    pub upload_limiter: RateLimiter,    // 10/min per user
    pub reaction_limiter: RateLimiter,  // 60/min per user
    pub presences: StdRwLock<HashMap<[u8; 32], Presence>>,
    pub presence_limiter: RateLimiter,  // 2/sec per user
    pub command_limiter: RateLimiter,   // 5/10s per user
    pub widget_limiter: RateLimiter,    // 10/10s per user (widget interactions: vote/enter/leave)
    pub media: media_stream::MediaStateMap,
    pub genesis: std::sync::Mutex<Option<farder_crypto::event_log::Genesis>>,
    pub log_state: std::sync::Mutex<Option<farder_crypto::event_log_state::LogState>>,
    /// Hex of the server's stable RELAY registration id (the value the relay routes
    /// by, distinct from the mesh genesis). Set at startup in relay mode; used to
    /// build webhook URLs. None for direct (non-relay) servers.
    pub relay_server_id: std::sync::Mutex<Option<String>>,
}

impl ServerState {
    pub fn new(conn: Connection, server_name: String, storage_dir: String, max_file_size: u64) -> Self {
        Self {
            db: Mutex::new(conn),
            sessions: RwLock::new(HashMap::new()),
            clients: RwLock::new(HashMap::new()),
            voice_connections: RwLock::new(HashMap::new()),
            relay_voice_handles: RwLock::new(HashMap::new()),
            subscriptions: RwLock::new(HashMap::new()),
            owner: RwLock::new(None),
            setup_token: Mutex::new(None),
            server_name,
            storage_dir,
            max_file_size,
            upload_limiter: RateLimiter::new(10, 60),
            reaction_limiter: RateLimiter::new(60, 60),
            presences: StdRwLock::new(HashMap::new()),
            presence_limiter: RateLimiter::new(2, 1),
            command_limiter: RateLimiter::new(5, 10),
            widget_limiter: RateLimiter::new(10, 10),
            media: media_stream::MediaStateMap::new(),
            genesis: std::sync::Mutex::new(None),
            log_state: std::sync::Mutex::new(None),
            relay_server_id: std::sync::Mutex::new(None),
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
mod voice_sink_tests {
    use super::*;
    use bytes::Bytes;

    async fn loopback_pair() -> (quinn::Connection, quinn::Connection) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec!["t".into()]).unwrap();
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
        let key = rustls::pki_types::PrivateKeyDer::try_from(cert.key_pair.serialize_der()).unwrap();
        let mut scfg = quinn::ServerConfig::with_single_cert(vec![cert_der], key).unwrap();
        {
            let mut t = quinn::TransportConfig::default();
            t.datagram_receive_buffer_size(Some(1 << 20));
            t.datagram_send_buffer_size(1 << 20);
            scfg.transport_config(std::sync::Arc::new(t));
        }
        let sep = quinn::Endpoint::server(scfg, "127.0.0.1:0".parse().unwrap()).unwrap();
        let saddr = sep.local_addr().unwrap();
        #[derive(Debug)] struct Skip;
        impl rustls::client::danger::ServerCertVerifier for Skip {
            fn verify_server_cert(&self,_:&rustls::pki_types::CertificateDer<'_>,_:&[rustls::pki_types::CertificateDer<'_>],_:&rustls::pki_types::ServerName<'_>,_:&[u8],_:rustls::pki_types::UnixTime)->std::result::Result<rustls::client::danger::ServerCertVerified,rustls::Error>{Ok(rustls::client::danger::ServerCertVerified::assertion())}
            fn verify_tls12_signature(&self,_:&[u8],_:&rustls::pki_types::CertificateDer<'_>,_:&rustls::DigitallySignedStruct)->std::result::Result<rustls::client::danger::HandshakeSignatureValid,rustls::Error>{Ok(rustls::client::danger::HandshakeSignatureValid::assertion())}
            fn verify_tls13_signature(&self,_:&[u8],_:&rustls::pki_types::CertificateDer<'_>,_:&rustls::DigitallySignedStruct)->std::result::Result<rustls::client::danger::HandshakeSignatureValid,rustls::Error>{Ok(rustls::client::danger::HandshakeSignatureValid::assertion())}
            fn supported_verify_schemes(&self)->Vec<rustls::SignatureScheme>{rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()}
        }
        let crypto = rustls::ClientConfig::builder().dangerous().with_custom_certificate_verifier(std::sync::Arc::new(Skip)).with_no_client_auth();
        let mut ccfg = quinn::ClientConfig::new(std::sync::Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap()));
        let mut t = quinn::TransportConfig::default();
        t.datagram_receive_buffer_size(Some(1 << 20));
        t.datagram_send_buffer_size(1 << 20);
        ccfg.transport_config(std::sync::Arc::new(t));
        let mut cep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        cep.set_default_client_config(ccfg);
        let server_fut = tokio::spawn(async move { sep.accept().await.unwrap().await.unwrap() });
        let client = cep.connect(saddr, "t").unwrap().await.unwrap();
        let server = server_fut.await.unwrap();
        std::mem::forget(cep);
        (client, server)
    }

    #[tokio::test]
    async fn relayed_sink_prefixes_handle_direct_does_not() {
        let (a, b) = loopback_pair().await;
        let sink = VoiceSink::Relayed { relay: a.clone(), handle: 0x01020304 };
        sink.send_datagram(Bytes::from_static(b"frame")).unwrap();
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), b.read_datagram()).await.unwrap().unwrap();
        assert_eq!(got.as_ref(), &[0x01,0x02,0x03,0x04, b'f',b'r',b'a',b'm',b'e']);

        let sink = VoiceSink::Direct(a.clone());
        sink.send_datagram(Bytes::from_static(b"frame")).unwrap();
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), b.read_datagram()).await.unwrap().unwrap();
        assert_eq!(got.as_ref(), b"frame");
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
