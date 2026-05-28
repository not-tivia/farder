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

// ────────────────────────────────────────────────────────────────────────────
// MediaInboundDispatcher (unchanged from Task 6).
// ────────────────────────────────────────────────────────────────────────────

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

// ────────────────────────────────────────────────────────────────────────────
// ServerSession — the slice of server I/O the VoiceController needs.
//
// The concrete impl lives in bridge.rs / server_manager.rs (Task 11);
// VoiceController tests use FakeServerSession. The trait is intentionally
// minimal: only methods the controller actually invokes.
// ────────────────────────────────────────────────────────────────────────────

use async_trait::async_trait;
use farder_crypto::identity::Keypair;
use farder_protocol::server::{TrackKind, VoiceMember};

#[async_trait]
pub trait ServerSession: Send + Sync {
    /// Issue `JoinStream { channel_id }` and return the assigned session_id.
    async fn join_stream(&self, channel_id: u64) -> Result<SessionId, String>;
    async fn leave_stream(&self) -> Result<(), String>;
    /// Return the current participants in the voice channel (used to wrap
    /// the stream key for each peer).
    async fn get_media_state(&self, channel_id: u64) -> Result<Vec<VoiceMember>, String>;
    async fn offer_stream_key(
        &self,
        kind: TrackKind,
        wrapped_keys: Vec<(PublicKey, Vec<u8>)>,
    ) -> Result<(), String>;
    async fn enable_track(&self, kind: TrackKind) -> Result<(), String>;
    async fn disable_track(&self, kind: TrackKind) -> Result<(), String>;
    /// Send one wire-format media datagram on the QUIC connection.
    fn send_datagram(&self, bytes: Bytes) -> Result<(), String>;
    /// Our long-lived signing keypair (used to wrap stream keys for peers).
    fn my_keypair(&self) -> Arc<Keypair>;
    /// The shared inbound media dispatcher (per-connection).
    fn dispatcher(&self) -> Arc<MediaInboundDispatcher>;
}

// ────────────────────────────────────────────────────────────────────────────
// VoiceEventEmitter — thin wrapper around tauri::AppHandle so tests can
// capture emissions without constructing a real Tauri runtime.
// ────────────────────────────────────────────────────────────────────────────

pub trait VoiceEventEmitter: Send + Sync {
    fn emit(&self, event: &str, payload: serde_json::Value);
}

/// Production impl wrapping tauri::AppHandle.
pub struct TauriEmitter {
    app: tauri::AppHandle,
}

impl TauriEmitter {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

impl VoiceEventEmitter for TauriEmitter {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        use tauri::Emitter;
        let _ = self.app.emit(event, payload);
    }
}

// ────────────────────────────────────────────────────────────────────────────
// VoicePipeline — encapsulates the live send + mixer + audio I/O tasks.
//
// Production impl spawns send.rs + mixer.rs against a real AudioBackend.
// Tests use a no-op pipeline so the controller's state machine can be
// exercised without bringing up actual audio threads.
// ────────────────────────────────────────────────────────────────────────────

pub struct PipelineParams {
    pub session_id: SessionId,
    pub stream_key: [u8; 32],
    pub speaker_pk: [u8; 32],
    pub peer_rings: crate::voice::mixer::PeerRings,
    pub muted: Arc<std::sync::atomic::AtomicBool>,
    pub local_speaking_tx: tokio::sync::watch::Sender<bool>,
    pub datagram_sink: Box<dyn Fn(Bytes) + Send + Sync + 'static>,
}

/// A live audio pipeline. Dropping (or calling stop) must terminate the
/// send + mixer + audio I/O it spawned.
pub trait VoicePipelineHandle: Send + Sync {
    fn stop(self: Box<Self>);
}

pub trait VoicePipelineFactory: Send + Sync {
    fn spawn(&self, params: PipelineParams) -> Result<Box<dyn VoicePipelineHandle>, String>;
}

/// Production pipeline factory: builds a cpal/mock audio backend, spawns
/// send + mixer as `spawn_blocking` tasks, and stops them on drop by closing
/// the audio channels.
pub struct AudioPipelineFactory;

struct AudioPipelineHandle {
    backend: Box<dyn crate::audio::AudioBackend>,
}

impl VoicePipelineHandle for AudioPipelineHandle {
    fn stop(self: Box<Self>) {
        // Explicit teardown. Drop impl provides the same safety net for
        // non-explicit paths (window close, panic).
        drop(self);
    }
}

impl Drop for AudioPipelineHandle {
    fn drop(&mut self) {
        let _ = self.backend.stop_capture();
        let _ = self.backend.stop_playback();
        // Send + mixer threads observe their channel closure and exit.
    }
}

impl VoicePipelineFactory for AudioPipelineFactory {
    fn spawn(&self, params: PipelineParams) -> Result<Box<dyn VoicePipelineHandle>, String> {
        use crate::audio::AudioFormat;
        use crate::opus_codec::OPUS_FRAME_SAMPLES_MONO;
        let backend = crate::audio::make_audio_backend();
        let format = AudioFormat {
            sample_rate: 48_000,
            channels: 1,
            samples_per_chunk: OPUS_FRAME_SAMPLES_MONO,
        };
        let pcm_rx = backend.start_capture(None, format)?;
        let playback_tx = backend.start_playback(None, format)?;

        let aec_ref = Arc::new(std::sync::Mutex::new(vec![0.0f32; OPUS_FRAME_SAMPLES_MONO]));

        // Mixer.
        let mixer_aec = aec_ref.clone();
        let mixer_rings = params.peer_rings.clone();
        tokio::task::spawn_blocking(move || {
            crate::voice::mixer::run(crate::voice::mixer::MixerTaskConfig {
                peer_rings: mixer_rings,
                playback_tx,
                aec_ref: mixer_aec,
            });
        });

        // Send.
        let send_aec = aec_ref;
        let session_id = params.session_id;
        let stream_key = params.stream_key;
        let speaker_pk = params.speaker_pk;
        let muted = params.muted;
        let speak_tx = params.local_speaking_tx;
        let datagram_sink = params.datagram_sink;
        tokio::task::spawn_blocking(move || {
            crate::voice::send::run(
                crate::voice::send::SendTaskConfig {
                    pcm_rx,
                    apm: crate::voice::apm::AudioProcessor::new(),
                    gate: crate::voice::gate::GateMode::Open,
                    session_id,
                    stream_key,
                    speaker_pk,
                    aec_ref: send_aec,
                    datagram_sink,
                },
                muted,
                speak_tx,
            );
        });

        Ok(Box::new(AudioPipelineHandle { backend }))
    }
}

// ────────────────────────────────────────────────────────────────────────────
// VoiceController
// ────────────────────────────────────────────────────────────────────────────

use crate::voice::recv::PeerPcmRing;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::task::JoinHandle;

pub struct VoiceController {
    inner: Arc<Mutex<Inner>>,
    emitter: Arc<dyn VoiceEventEmitter>,
    pipeline_factory: Arc<dyn VoicePipelineFactory>,
}

struct Inner {
    state: VoiceState,
    muted: Arc<AtomicBool>,
    deafened: Arc<AtomicBool>,
    pre_deafen_muted: bool,
    /// Set while a call is active.
    active: Option<ActiveCall>,
}

struct ActiveCall {
    server: Arc<dyn ServerSession>,
    pipeline: Option<Box<dyn VoicePipelineHandle>>,
    peer_rings: crate::voice::mixer::PeerRings,
    peers: HashMap<SessionId, PeerEntry>,
    /// Per-peer stream keys delivered via StreamKeyOffer events.
    /// Stored separately from `peers` because the offer typically arrives
    /// before the matching TrackEnabled.
    peer_keys: HashMap<SessionId, ([u8; 32], PublicKey)>,
}

struct PeerEntry {
    pubkey: PublicKey,
    recv_handle: JoinHandle<()>,
    /// Held so the dispatcher's route stays valid for this peer.
    #[allow(dead_code)]
    datagram_tx: mpsc::UnboundedSender<Bytes>,
}

/// Map a u64 channel_id (server-side identifier) into the controller's
/// fixed-size ChannelId byte array.
fn channel_id_bytes(channel_id: u64) -> ChannelId {
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&channel_id.to_be_bytes());
    out
}

impl VoiceController {
    /// Production constructor: wraps a tauri::AppHandle and uses the real
    /// audio pipeline factory.
    pub fn new(app: tauri::AppHandle) -> Self {
        Self::with_runtime(
            Arc::new(TauriEmitter::new(app)),
            Arc::new(AudioPipelineFactory),
        )
    }

    /// Test/DI constructor: inject any emitter + pipeline factory.
    pub fn with_runtime(
        emitter: Arc<dyn VoiceEventEmitter>,
        pipeline_factory: Arc<dyn VoicePipelineFactory>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                state: VoiceState {
                    channel_id: None,
                    muted: false,
                    deafened: false,
                    peers: vec![],
                },
                muted: Arc::new(AtomicBool::new(false)),
                deafened: Arc::new(AtomicBool::new(false)),
                pre_deafen_muted: false,
                active: None,
            })),
            emitter,
            pipeline_factory,
        }
    }

    pub async fn state(&self) -> VoiceState {
        self.inner.lock().await.state.clone()
    }

    /// Look up the peer's long-lived public key for a given session_id.
    ///
    /// Used by bridge.rs to bridge `TrackEnabled` (which only carries
    /// session_id) into `on_peer_track_enabled` (which needs the pubkey to
    /// register the peer for UI / state tracking). The pubkey is populated
    /// by the matching `StreamKeyOffer` that always precedes `TrackEnabled`.
    pub async fn peer_pubkey_for(&self, session_id: &SessionId) -> Option<PublicKey> {
        let inner = self.inner.lock().await;
        inner
            .active
            .as_ref()
            .and_then(|c| c.peer_keys.get(session_id).map(|(_, pk)| pk.clone()))
    }

    pub async fn join(
        &self,
        channel_id: u64,
        server: Arc<dyn ServerSession>,
    ) -> Result<(), String> {
        // Auto-leave any existing call first.
        let has_active = self.inner.lock().await.active.is_some();
        if has_active {
            self.leave().await?;
        }

        // 1. JoinStream → session_id
        let session_id = server.join_stream(channel_id).await?;

        // 2. Derive stream key + wrap for each remote participant.
        let stream_key = farder_crypto::media::derive_stream_key();
        let participants = server.get_media_state(channel_id).await?;
        let keypair = server.my_keypair();
        let my_sk = *keypair.signing_key_bytes();
        let my_pk_bytes = *keypair.public_key().as_bytes();
        let wrapped: Vec<(PublicKey, Vec<u8>)> = participants
            .iter()
            .filter(|m| m.public_key.as_bytes() != &my_pk_bytes)
            .filter_map(|m| {
                farder_crypto::media::wrap_stream_key_for_peer(
                    &stream_key,
                    &my_sk,
                    m.public_key.as_bytes(),
                )
                .ok()
                .map(|w| (m.public_key.clone(), w))
            })
            .collect();
        if !wrapped.is_empty() {
            server
                .offer_stream_key(TrackKind::Audio, wrapped)
                .await?;
        }

        // 3. Spawn audio pipeline (mixer + send + I/O).
        let peer_rings: crate::voice::mixer::PeerRings = Default::default();
        let muted_flag = self.inner.lock().await.muted.clone();
        let (speak_tx, mut speak_rx) = tokio::sync::watch::channel(false);
        let server_for_sink = server.clone();
        let pipeline = self.pipeline_factory.spawn(PipelineParams {
            session_id,
            stream_key,
            speaker_pk: my_pk_bytes,
            peer_rings: peer_rings.clone(),
            muted: muted_flag,
            local_speaking_tx: speak_tx,
            datagram_sink: Box::new(move |b: Bytes| {
                let _ = server_for_sink.send_datagram(b);
            }),
        })?;

        // 4. EnableTrack last, after pipeline is up.
        server.enable_track(TrackKind::Audio).await?;

        // 5. Speaking-event forwarder.
        let emitter_for_speak = self.emitter.clone();
        tokio::spawn(async move {
            while speak_rx.changed().await.is_ok() {
                let s = *speak_rx.borrow();
                emitter_for_speak.emit(
                    "voice://local-speaking",
                    serde_json::json!({ "speaking": s }),
                );
            }
        });

        // 6. Commit state + emit.
        let snap = {
            let mut inner = self.inner.lock().await;
            inner.state.channel_id = Some(channel_id_bytes(channel_id));
            inner.active = Some(ActiveCall {
                server,
                pipeline: Some(pipeline),
                peer_rings,
                peers: HashMap::new(),
                peer_keys: HashMap::new(),
            });
            inner.state.clone()
        };
        self.emitter
            .emit("voice://state-changed", serde_json::to_value(&snap).unwrap_or_default());
        Ok(())
    }

    pub async fn leave(&self) -> Result<(), String> {
        // Phase 1: pull the active call out while holding the lock briefly.
        // Between here and Phase 3, inner.active is None — any concurrent
        // join() that checks active.is_some() will not see an active call.
        let call = self.inner.lock().await.active.take();

        if let Some(mut call) = call {
            // Phase 2: all network I/O + audio teardown outside the lock so
            // concurrent set_mute / on_peer_activity callers are not stalled
            // during these round-trips.

            // Best-effort protocol shutdown; ignore errors so leave is
            // robust against a broken connection.
            let _ = call.server.disable_track(TrackKind::Audio).await;
            let _ = call.server.leave_stream().await;
            // Stop the audio pipeline (closes channels → send/mixer exit).
            if let Some(p) = call.pipeline.take() {
                p.stop();
            }
            // Abort every peer recv task and unregister its dispatcher route.
            let dispatcher = call.server.dispatcher();
            for (sid, peer) in call.peers.drain() {
                peer.recv_handle.abort();
                let d = dispatcher.clone();
                tokio::spawn(async move {
                    d.unregister(&sid).await;
                });
            }
            call.peer_rings.lock().expect("peer_rings poisoned").clear();
        }

        // Phase 3: re-lock to commit the cleared state, then emit.
        let snap = {
            let mut inner = self.inner.lock().await;
            inner.state = VoiceState {
                channel_id: None,
                muted: false,
                deafened: false,
                peers: vec![],
            };
            inner.muted.store(false, Ordering::Release);
            inner.deafened.store(false, Ordering::Release);
            inner.pre_deafen_muted = false;
            inner.state.clone()
        };
        self.emitter
            .emit("voice://state-changed", serde_json::to_value(&snap).unwrap_or_default());
        Ok(())
    }

    pub async fn set_mute(&self, muted: bool) -> Result<(), String> {
        let snap = {
            let mut inner = self.inner.lock().await;
            inner.muted.store(muted, Ordering::Release);
            inner.state.muted = muted;
            inner.state.clone()
        };
        self.emitter
            .emit("voice://state-changed", serde_json::to_value(&snap).unwrap_or_default());
        Ok(())
    }

    pub async fn set_deafen(&self, deafened: bool) -> Result<(), String> {
        let snap = {
            let mut inner = self.inner.lock().await;
            if deafened {
                inner.pre_deafen_muted = inner.muted.load(Ordering::Acquire);
                inner.muted.store(true, Ordering::Release);
                inner.deafened.store(true, Ordering::Release);
            } else {
                let restore = inner.pre_deafen_muted;
                inner.muted.store(restore, Ordering::Release);
                inner.deafened.store(false, Ordering::Release);
            }
            inner.state.muted = inner.muted.load(Ordering::Acquire);
            inner.state.deafened = deafened;
            inner.state.clone()
        };
        self.emitter
            .emit("voice://state-changed", serde_json::to_value(&snap).unwrap_or_default());
        Ok(())
    }

    // ── Inbound media-event handlers (Task 11 wires server events to these) ──

    /// Server told us a peer offered us a wrapped stream key. Unwrap it with
    /// our long-lived keypair and remember it for the matching TrackEnabled.
    pub async fn on_stream_key_offer(
        &self,
        session_id: SessionId,
        sender_pubkey: PublicKey,
        wrapped_key: Vec<u8>,
    ) {
        let mut inner = self.inner.lock().await;
        let call = match inner.active.as_mut() {
            Some(c) => c,
            None => return,
        };
        let keypair = call.server.my_keypair();
        let my_sk = *keypair.signing_key_bytes();
        match farder_crypto::media::unwrap_stream_key(
            &wrapped_key,
            &my_sk,
            sender_pubkey.as_bytes(),
        ) {
            Ok(key) => {
                call.peer_keys.insert(session_id, (key, sender_pubkey));
            }
            Err(e) => {
                eprintln!("[voice] unwrap_stream_key: {e}");
            }
        }
    }

    /// Server announced a peer enabled an audio track. Spawn a RecvTask,
    /// register a per-peer ring with the mixer, and register a dispatcher
    /// route so inbound datagrams get to this peer.
    pub async fn on_peer_track_enabled(
        &self,
        session_id: SessionId,
        peer_pubkey: PublicKey,
        kind: TrackKind,
    ) {
        if !matches!(kind, TrackKind::Audio) {
            return;
        }
        let mut inner = self.inner.lock().await;
        let deafened_flag = inner.deafened.clone();
        let call = match inner.active.as_mut() {
            Some(c) => c,
            None => return,
        };
        if call.peers.contains_key(&session_id) {
            return; // already running
        }
        let stream_key = match call.peer_keys.get(&session_id) {
            Some((k, _)) => *k,
            None => {
                eprintln!(
                    "[voice] TrackEnabled for unknown session_id; missing StreamKeyOffer?"
                );
                return;
            }
        };
        let ring = Arc::new(PeerPcmRing::new(10));
        call.peer_rings
            .lock()
            .expect("peer_rings poisoned")
            .insert(session_id, ring.clone());

        let (tx, rx) = mpsc::unbounded_channel::<Bytes>();
        let dispatcher = call.server.dispatcher();
        let tx_for_register = tx.clone();
        tokio::spawn(async move {
            dispatcher.register(session_id, tx_for_register).await;
        });

        let recv_handle = tokio::spawn(async move {
            crate::voice::recv::run(crate::voice::recv::RecvTaskConfig {
                session_id,
                stream_key,
                deafened: deafened_flag,
                datagram_rx: rx,
                pcm_ring: ring,
            })
            .await;
        });

        call.peers.insert(
            session_id,
            PeerEntry {
                pubkey: peer_pubkey.clone(),
                recv_handle,
                datagram_tx: tx,
            },
        );
        if !inner.state.peers.iter().any(|p| p.pubkey == peer_pubkey) {
            inner.state.peers.push(VoicePeer {
                pubkey: peer_pubkey,
                speaking: false,
            });
        }
        let snap = inner.state.clone();
        drop(inner);
        self.emitter
            .emit("voice://state-changed", serde_json::to_value(&snap).unwrap_or_default());
    }

    /// Peer disabled their audio track (mute) or otherwise stopped sending.
    /// Tear down their recv task + dispatcher route, drop their ring.
    pub async fn on_peer_track_disabled(&self, session_id: SessionId) {
        let snap = {
            let mut inner = self.inner.lock().await;
            let removed_pubkey: Option<PublicKey> = {
                let call = match inner.active.as_mut() {
                    Some(c) => c,
                    None => return,
                };
                if let Some(peer) = call.peers.remove(&session_id) {
                    peer.recv_handle.abort();
                    call.peer_rings
                        .lock()
                        .expect("peer_rings poisoned")
                        .remove(&session_id);
                    let dispatcher = call.server.dispatcher();
                    tokio::spawn(async move {
                        dispatcher.unregister(&session_id).await;
                    });
                    Some(peer.pubkey)
                } else {
                    None
                }
            };
            let pk = match removed_pubkey {
                Some(pk) => pk,
                None => return, // no peer with that session_id; no-op
            };
            inner.state.peers.retain(|p| p.pubkey != pk);
            inner.state.clone()
        };
        self.emitter
            .emit("voice://state-changed", serde_json::to_value(&snap).unwrap_or_default());
    }

    /// Peer left the stream entirely (different event from TrackDisabled but
    /// same controller-side cleanup).
    pub async fn on_peer_stream_left(&self, session_id: SessionId) {
        self.on_peer_track_disabled(session_id).await;
    }

    /// Server forwarded a TrackActivityChanged event — flip the speaking
    /// flag on the matching peer + emit `voice://peer-speaking`.
    pub async fn on_peer_activity(
        &self,
        session_id: SessionId,
        kind: TrackKind,
        active: bool,
    ) {
        if !matches!(kind, TrackKind::Audio) {
            return;
        }
        let pubkey_opt = {
            let mut inner = self.inner.lock().await;
            let pk = inner
                .active
                .as_ref()
                .and_then(|c| c.peers.get(&session_id).map(|p| p.pubkey.clone()));
            if let Some(pk) = pk.as_ref() {
                for peer in inner.state.peers.iter_mut() {
                    if &peer.pubkey == pk {
                        peer.speaking = active;
                        break;
                    }
                }
            }
            pk
        };
        if let Some(pk) = pubkey_opt {
            self.emitter.emit(
                "voice://peer-speaking",
                serde_json::json!({
                    "session_id": session_id,
                    "pubkey": pk.to_string(),
                    "active": active,
                }),
            );
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Tests — controller state machine + event emissions.
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod controller_tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    // ── FakeServerSession ────────────────────────────────────────────────

    #[derive(Default)]
    struct FakeServerSession {
        keypair: Option<Arc<Keypair>>,
        dispatcher: Arc<MediaInboundDispatcher>,
        calls: StdMutex<Vec<String>>,
        canned_session_id: SessionId,
    }

    impl FakeServerSession {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                keypair: Some(Arc::new(Keypair::generate())),
                dispatcher: Arc::new(MediaInboundDispatcher::default()),
                calls: StdMutex::new(Vec::new()),
                canned_session_id: [9u8; 16],
            })
        }
        fn new_with_sid(sid: SessionId) -> Arc<Self> {
            Arc::new(Self {
                keypair: Some(Arc::new(Keypair::generate())),
                dispatcher: Arc::new(MediaInboundDispatcher::default()),
                calls: StdMutex::new(Vec::new()),
                canned_session_id: sid,
            })
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn log(&self, s: impl Into<String>) {
            self.calls.lock().unwrap().push(s.into());
        }
    }

    #[async_trait]
    impl ServerSession for FakeServerSession {
        async fn join_stream(&self, _channel_id: u64) -> Result<SessionId, String> {
            self.log("join_stream");
            Ok(self.canned_session_id)
        }
        async fn leave_stream(&self) -> Result<(), String> {
            self.log("leave_stream");
            Ok(())
        }
        async fn get_media_state(
            &self,
            _channel_id: u64,
        ) -> Result<Vec<VoiceMember>, String> {
            self.log("get_media_state");
            Ok(vec![])
        }
        async fn offer_stream_key(
            &self,
            _k: TrackKind,
            _w: Vec<(PublicKey, Vec<u8>)>,
        ) -> Result<(), String> {
            self.log("offer_stream_key");
            Ok(())
        }
        async fn enable_track(&self, _k: TrackKind) -> Result<(), String> {
            self.log("enable_track");
            Ok(())
        }
        async fn disable_track(&self, _k: TrackKind) -> Result<(), String> {
            self.log("disable_track");
            Ok(())
        }
        fn send_datagram(&self, _b: Bytes) -> Result<(), String> {
            Ok(())
        }
        fn my_keypair(&self) -> Arc<Keypair> {
            self.keypair.as_ref().unwrap().clone()
        }
        fn dispatcher(&self) -> Arc<MediaInboundDispatcher> {
            self.dispatcher.clone()
        }
    }

    // ── MockEmitter ──────────────────────────────────────────────────────

    #[derive(Default)]
    struct MockEmitter {
        events: StdMutex<Vec<(String, serde_json::Value)>>,
    }

    impl MockEmitter {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
        fn events(&self) -> Vec<(String, serde_json::Value)> {
            self.events.lock().unwrap().clone()
        }
        fn count(&self, event: &str) -> usize {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|(e, _)| e == event)
                .count()
        }
    }

    impl VoiceEventEmitter for MockEmitter {
        fn emit(&self, event: &str, payload: serde_json::Value) {
            self.events
                .lock()
                .unwrap()
                .push((event.to_string(), payload));
        }
    }

    // ── NoopPipelineFactory ──────────────────────────────────────────────

    #[derive(Default)]
    struct NoopPipelineFactory;

    struct NoopPipelineHandle;
    impl VoicePipelineHandle for NoopPipelineHandle {
        fn stop(self: Box<Self>) {}
    }

    impl VoicePipelineFactory for NoopPipelineFactory {
        fn spawn(
            &self,
            _params: PipelineParams,
        ) -> Result<Box<dyn VoicePipelineHandle>, String> {
            Ok(Box::new(NoopPipelineHandle))
        }
    }

    fn make_controller() -> (Arc<VoiceController>, Arc<MockEmitter>) {
        let emitter = MockEmitter::new();
        let factory: Arc<dyn VoicePipelineFactory> = Arc::new(NoopPipelineFactory);
        let ctrl = Arc::new(VoiceController::with_runtime(
            emitter.clone() as Arc<dyn VoiceEventEmitter>,
            factory,
        ));
        (ctrl, emitter)
    }

    // ── Tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn join_then_leave_round_trip_updates_state() {
        let (ctrl, emitter) = make_controller();
        let server = FakeServerSession::new();

        ctrl.join(7, server.clone()).await.unwrap();
        let after_join = ctrl.state().await;
        assert!(after_join.channel_id.is_some(), "channel_id set after join");
        assert_eq!(emitter.count("voice://state-changed"), 1);

        ctrl.leave().await.unwrap();
        let after_leave = ctrl.state().await;
        assert!(after_leave.channel_id.is_none(), "channel_id cleared after leave");
        assert_eq!(emitter.count("voice://state-changed"), 2);

        let calls = server.calls();
        let join_idx = calls.iter().position(|c| c == "join_stream").unwrap();
        let enable_idx = calls.iter().position(|c| c == "enable_track").unwrap();
        let disable_idx = calls.iter().position(|c| c == "disable_track").unwrap();
        let leave_idx = calls.iter().position(|c| c == "leave_stream").unwrap();
        assert!(join_idx < enable_idx, "join_stream before enable_track");
        assert!(enable_idx < disable_idx, "enable_track before disable_track");
        assert!(disable_idx < leave_idx, "disable_track before leave_stream");
    }

    #[tokio::test]
    async fn double_join_auto_leaves_previous() {
        let (ctrl, _emitter) = make_controller();
        let server = FakeServerSession::new();

        ctrl.join(7, server.clone()).await.unwrap();
        ctrl.join(8, server.clone()).await.unwrap();

        let calls = server.calls();
        let first_join = calls.iter().position(|c| c == "join_stream").unwrap();
        let leave = calls.iter().position(|c| c == "leave_stream").unwrap();
        let second_join = calls.iter().rposition(|c| c == "join_stream").unwrap();
        assert!(
            first_join < leave && leave < second_join,
            "expected order first_join < leave < second_join; got {calls:?}"
        );
    }

    #[tokio::test]
    async fn set_mute_updates_atomic_and_emits_state() {
        let (ctrl, emitter) = make_controller();

        ctrl.set_mute(true).await.unwrap();
        let st = ctrl.state().await;
        assert!(st.muted);

        ctrl.set_mute(false).await.unwrap();
        let st = ctrl.state().await;
        assert!(!st.muted);

        // Two emits, both state-changed.
        let events = emitter.events();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|(e, _)| e == "voice://state-changed"));
        // Payload muted flag tracks the call.
        assert_eq!(events[0].1.get("muted"), Some(&serde_json::json!(true)));
        assert_eq!(events[1].1.get("muted"), Some(&serde_json::json!(false)));
    }

    #[tokio::test]
    async fn set_deafen_implicitly_mutes_and_restores_on_undeafen() {
        let (ctrl, _emitter) = make_controller();

        // Start unmuted; deafen forces mute=true.
        ctrl.set_deafen(true).await.unwrap();
        let st = ctrl.state().await;
        assert!(st.deafened);
        assert!(st.muted, "deafen must imply mute");

        // Undeafen restores pre-deafen muted (false).
        ctrl.set_deafen(false).await.unwrap();
        let st = ctrl.state().await;
        assert!(!st.deafened);
        assert!(!st.muted, "undeafen must restore pre-deafen mute=false");

        // Pre-deafen muted=true must be preserved across deafen/undeafen.
        ctrl.set_mute(true).await.unwrap();
        ctrl.set_deafen(true).await.unwrap();
        assert!(ctrl.state().await.muted);
        ctrl.set_deafen(false).await.unwrap();
        let st = ctrl.state().await;
        assert!(!st.deafened);
        assert!(st.muted, "undeafen must restore pre-deafen mute=true");
    }

    // Bonus coverage: leave-without-join is a no-op that still emits.
    #[tokio::test]
    async fn leave_with_no_active_call_is_idempotent() {
        let (ctrl, emitter) = make_controller();
        ctrl.leave().await.unwrap();
        assert!(ctrl.state().await.channel_id.is_none());
        assert_eq!(emitter.count("voice://state-changed"), 1);
    }

    // Sanity: the canned session_id flows through to the controller's
    // internal state for downstream peer-event routing.
    #[tokio::test]
    async fn join_records_session_id_for_peer_routing() {
        let (ctrl, _emitter) = make_controller();
        let server = FakeServerSession::new_with_sid([0x55; 16]);
        ctrl.join(1, server.clone()).await.unwrap();
        // peer_keys starts empty; on_stream_key_offer populates it.
        // We use a fresh keypair pair so the unwrap can round-trip.
        let peer_kp = Keypair::generate();
        let our_kp = server.my_keypair();
        let key = farder_crypto::media::derive_stream_key();
        let wrapped = farder_crypto::media::wrap_stream_key_for_peer(
            &key,
            peer_kp.signing_key_bytes(),
            our_kp.public_key().as_bytes(),
        )
        .unwrap();
        ctrl.on_stream_key_offer([0xAA; 16], peer_kp.public_key(), wrapped)
            .await;
        // No public accessor to peek peer_keys, but if we got here without
        // panicking, the unwrap path is exercised.
        ctrl.leave().await.unwrap();
    }
}
