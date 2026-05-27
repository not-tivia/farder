// client/src-tauri/src/voice/mod.rs
//
// Voice call orchestration. Coordinates capture, encode, transport,
// receive, decode, mix, playback. See
// docs/superpowers/specs/2026-05-26-voice-client-pipeline-design.md.

pub mod apm;
pub mod gate;
pub mod jitter;
pub mod mixer;
pub mod recv;
pub mod send;

use farder_crypto::identity::PublicKey;
use serde::{Deserialize, Serialize};

pub type ChannelId = [u8; 16];
pub type SessionId = [u8; 16];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoicePeer {
    pub pubkey: PublicKey,
    pub speaking: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoiceState {
    pub channel_id: Option<ChannelId>,
    pub muted: bool,
    pub deafened: bool,
    pub peers: Vec<VoicePeer>,
}

pub struct VoiceController {
    // populated in VOICE-10
}

impl VoiceController {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for VoiceController {
    fn default() -> Self {
        Self::new()
    }
}

use bytes::Bytes;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// Routes inbound media datagrams to the right RecvTask by session_id.
/// Frame layout from sub-project #2: bytes[12..28] = session_id (16 bytes).
#[derive(Default)]
pub struct MediaInboundDispatcher {
    routes: Mutex<HashMap<SessionId, mpsc::UnboundedSender<Bytes>>>,
}

impl MediaInboundDispatcher {
    pub async fn register(&self, session_id: SessionId, tx: mpsc::UnboundedSender<Bytes>) {
        self.routes.lock().await.insert(session_id, tx);
    }

    pub async fn unregister(&self, session_id: &SessionId) {
        self.routes.lock().await.remove(session_id);
    }

    pub async fn dispatch(&self, bytes: Bytes) {
        if bytes.len() < 28 {
            return; // not a valid sealed media frame
        }
        let mut sid = [0u8; 16];
        sid.copy_from_slice(&bytes[12..28]);
        let routes = self.routes.lock().await;
        if let Some(tx) = routes.get(&sid) {
            let _ = tx.send(bytes);
        }
    }
}

#[cfg(test)]
mod dispatcher_tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_routes_to_registered_session() {
        let dispatcher = MediaInboundDispatcher::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sid: SessionId = [7u8; 16];
        dispatcher.register(sid, tx).await;

        // Build a 28-byte minimum frame with session_id at [12..28].
        let mut frame = vec![0u8; 32];
        frame[12..28].copy_from_slice(&sid);
        dispatcher.dispatch(Bytes::from(frame.clone())).await;

        let received = rx.try_recv().expect("dispatched frame must arrive");
        assert_eq!(received.len(), frame.len());
    }

    #[tokio::test]
    async fn dispatch_drops_unknown_session() {
        let dispatcher = MediaInboundDispatcher::default();
        let mut frame = vec![0u8; 32];
        frame[12..28].copy_from_slice(&[9u8; 16]);
        dispatcher.dispatch(Bytes::from(frame)).await; // must not panic
    }

    #[tokio::test]
    async fn dispatch_drops_too_short_frames() {
        let dispatcher = MediaInboundDispatcher::default();
        dispatcher.dispatch(Bytes::from(vec![0u8; 20])).await; // must not panic
    }

    #[tokio::test]
    async fn unregister_removes_route() {
        let dispatcher = MediaInboundDispatcher::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sid: SessionId = [3u8; 16];
        dispatcher.register(sid, tx).await;
        dispatcher.unregister(&sid).await;

        let mut frame = vec![0u8; 32];
        frame[12..28].copy_from_slice(&sid);
        dispatcher.dispatch(Bytes::from(frame)).await;
        assert!(rx.try_recv().is_err(), "unregistered session must not receive");
    }
}
