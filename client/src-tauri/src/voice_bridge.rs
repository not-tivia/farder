// client/src-tauri/src/voice_bridge.rs
//
// `QuinnServerSession` — concrete `ServerSession` impl that wraps an
// `Arc<ServerConnection>` so the `VoiceController` can drive the same
// authenticated QUIC connection the rest of the app uses.
//
// Lives outside `voice/mod.rs` to keep the controller free of Quinn /
// AppState concerns. Constructed per call inside `voice_join` and dropped
// alongside the `ActiveCall` it backs.

use crate::bridge::send_request;
use crate::state::{AppState, ServerConnection};
use crate::voice::{MediaInboundDispatcher, ServerSession, SessionId};
use async_trait::async_trait;
use bytes::Bytes;
use farder_crypto::identity::{Keypair, PublicKey};
use farder_protocol::server::{ServerRequest, ServerResponse, TrackKind, VoiceMember};
use std::sync::Arc;

pub struct QuinnServerSession {
    server_id: String,
    state: Arc<AppState>,
    conn: Arc<ServerConnection>,
    keypair: Arc<Keypair>,
}

impl QuinnServerSession {
    /// Build a session backed by the named `ServerConnection`. Fails if the
    /// connection has been dropped or the identity key is unset.
    pub fn new(state: Arc<AppState>, server_id: String) -> Result<Self, String> {
        let conn = state.get_server(&server_id)?;
        let keypair = {
            let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
            match lock.as_ref() {
                Some(bytes) => Arc::new(Keypair::from_signing_key_bytes(bytes)),
                None => {
                    return Err(
                        "no identity keypair set — unlock your identity first".to_string()
                    );
                }
            }
        };
        Ok(Self { server_id, state, conn, keypair })
    }
}

#[async_trait]
impl ServerSession for QuinnServerSession {
    async fn join_stream(&self, channel_id: u64) -> Result<SessionId, String> {
        let resp = send_request(&self.state, &self.server_id, ServerRequest::JoinStream { channel_id })
            .await
            .map_err(|e| e.to_string())?;
        match resp {
            ServerResponse::StreamSessionStarted { session_id } => Ok(session_id),
            ServerResponse::Error { reason } => Err(reason),
            other => Err(format!("unexpected response to JoinStream: {:?}", other)),
        }
    }

    async fn leave_stream(&self) -> Result<(), String> {
        let resp = send_request(&self.state, &self.server_id, ServerRequest::LeaveStream)
            .await
            .map_err(|e| e.to_string())?;
        match resp {
            ServerResponse::Ok => Ok(()),
            ServerResponse::Error { reason } => Err(reason),
            other => Err(format!("unexpected response to LeaveStream: {:?}", other)),
        }
    }

    async fn get_media_state(&self, channel_id: u64) -> Result<Vec<VoiceMember>, String> {
        let resp = send_request(&self.state, &self.server_id, ServerRequest::GetMediaState { channel_id })
            .await
            .map_err(|e| e.to_string())?;
        match resp {
            ServerResponse::MediaStateResp { participants } => Ok(participants),
            ServerResponse::Error { reason } => Err(reason),
            other => Err(format!("unexpected response to GetMediaState: {:?}", other)),
        }
    }

    async fn offer_stream_key(
        &self,
        kind: TrackKind,
        wrapped_keys: Vec<(PublicKey, Vec<u8>)>,
    ) -> Result<(), String> {
        let resp = send_request(
            &self.state,
            &self.server_id,
            ServerRequest::OfferStreamKey { kind, wrapped_keys },
        )
        .await
        .map_err(|e| e.to_string())?;
        match resp {
            ServerResponse::Ok => Ok(()),
            ServerResponse::Error { reason } => Err(reason),
            other => Err(format!("unexpected response to OfferStreamKey: {:?}", other)),
        }
    }

    async fn enable_track(&self, kind: TrackKind) -> Result<(), String> {
        let resp = send_request(&self.state, &self.server_id, ServerRequest::EnableTrack { kind })
            .await
            .map_err(|e| e.to_string())?;
        match resp {
            ServerResponse::Ok => Ok(()),
            ServerResponse::Error { reason } => Err(reason),
            other => Err(format!("unexpected response to EnableTrack: {:?}", other)),
        }
    }

    async fn disable_track(&self, kind: TrackKind) -> Result<(), String> {
        let resp = send_request(&self.state, &self.server_id, ServerRequest::DisableTrack { kind })
            .await
            .map_err(|e| e.to_string())?;
        match resp {
            ServerResponse::Ok => Ok(()),
            ServerResponse::Error { reason } => Err(reason),
            other => Err(format!("unexpected response to DisableTrack: {:?}", other)),
        }
    }

    async fn set_mute(&self, muted: bool) -> Result<(), String> {
        let resp = send_request(&self.state, &self.server_id, ServerRequest::SetMute { muted })
            .await
            .map_err(|e| e.to_string())?;
        match resp {
            ServerResponse::Ok => Ok(()),
            ServerResponse::Error { reason } => Err(reason),
            other => Err(format!("unexpected response to SetMute: {:?}", other)),
        }
    }

    async fn set_deafen(&self, deafened: bool) -> Result<(), String> {
        let resp = send_request(&self.state, &self.server_id, ServerRequest::SetDeafen { deafened })
            .await
            .map_err(|e| e.to_string())?;
        match resp {
            ServerResponse::Ok => Ok(()),
            ServerResponse::Error { reason } => Err(reason),
            other => Err(format!("unexpected response to SetDeafen: {:?}", other)),
        }
    }

    fn send_datagram(&self, bytes: Bytes) -> Result<(), String> {
        self.conn.connection.send_datagram(bytes).map_err(|e| e.to_string())
    }

    fn my_keypair(&self) -> Arc<Keypair> {
        self.keypair.clone()
    }

    fn dispatcher(&self) -> Arc<MediaInboundDispatcher> {
        self.conn.media_dispatcher.clone()
    }
}
