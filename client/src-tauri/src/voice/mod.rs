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
pub mod recv_video;
pub mod send;
pub mod send_screen_audio;
pub mod send_video;

use farder_crypto::identity::PublicKey;
use serde::{Deserialize, Serialize};

pub type ChannelId = [u8; 16];
pub type SessionId = [u8; 16];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoicePeer {
    pub pubkey: PublicKey,
    pub speaking: bool,
    pub muted: bool,
    pub deafened: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoiceState {
    pub channel_id: Option<ChannelId>,
    pub muted: bool,
    pub deafened: bool,
    pub transmitting: bool,
    pub peers: Vec<VoicePeer>,
}

/// Mic transmission mode, read from settings at join time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceMode {
    OpenMic,
    PushToTalk,
}

impl Default for VoiceMode {
    fn default() -> Self {
        VoiceMode::OpenMic
    }
}

/// Default speaking RMS threshold (sensitivity ~85%). Used until a join reads
/// the persisted mic-sensitivity setting via `set_speak_threshold`.
pub const DEFAULT_SPEAK_THRESHOLD: f32 = 0.008;

/// Map a mic-sensitivity slider value (0-100, higher = more sensitive) to the
/// speaking RMS threshold (lower = easier to trigger).
pub fn sensitivity_to_threshold(sensitivity: u32) -> f32 {
    let s = (sensitivity.min(100) as f32) / 100.0;
    (0.05 - 0.049 * s).max(0.0005)
}

/// Choose the send-path gate for a mode. PTT uses the shared transmit flag;
/// Open Mic ignores it and always passes.
pub fn gate_for_mode(
    mode: VoiceMode,
    transmit: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> crate::voice::gate::GateMode {
    match mode {
        VoiceMode::OpenMic => crate::voice::gate::GateMode::Open,
        VoiceMode::PushToTalk => crate::voice::gate::GateMode::Ptt(transmit),
    }
}

/// Convert raw QUIC path stats into (rtt_ms, loss_pct). Cumulative estimate;
/// loss = lost / max(1, sent) * 100. Display-only. Pure so it is unit-testable
/// without a live connection.
pub fn quality_from_stats(rtt: std::time::Duration, lost: u64, sent: u64) -> (f64, f64) {
    let rtt_ms = rtt.as_secs_f64() * 1000.0;
    let loss_pct = (lost as f64 / sent.max(1) as f64) * 100.0;
    (rtt_ms, loss_pct)
}

/// Per-join configuration resolved from settings by the command layer.
/// Kept separate from `join`'s args so tests can inject it directly.
#[derive(Clone, Default)]
pub struct JoinConfig {
    pub mode: VoiceMode,
    /// pubkey hex -> gain (1.0 = 100%). Absent peer = 1.0.
    pub peer_volumes: std::collections::HashMap<String, f32>,
    /// QUIC connection for the connection-quality poller. `None` (e.g. in
    /// tests) means no poller is spawned. Cheaply clonable.
    pub connection: Option<quinn::Connection>,
    /// Saved input device name (None = system default).
    pub input_device: Option<String>,
    /// Saved output device name (None = system default).
    pub output_device: Option<String>,
}

// ────────────────────────────────────────────────────────────────────────────
// MediaInboundDispatcher — re-keyed by (session_id, track_kind) in Phase C1.
// ────────────────────────────────────────────────────────────────────────────

use bytes::Bytes;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// Routes inbound media datagrams to the right RecvTask by (session_id, track_kind).
/// A peer's session carries BOTH audio and video frame streams (distinguished by
/// the cleartext outer header's track_kind); each track gets its own recv task.
#[derive(Default)]
pub struct MediaInboundDispatcher {
    routes: Mutex<HashMap<(SessionId, TrackKind), mpsc::UnboundedSender<Bytes>>>,
}

impl MediaInboundDispatcher {
    pub async fn register(&self, session_id: SessionId, kind: TrackKind, tx: mpsc::UnboundedSender<Bytes>) {
        self.routes.lock().await.insert((session_id, kind), tx);
    }

    pub async fn unregister(&self, session_id: &SessionId, kind: TrackKind) {
        self.routes.lock().await.remove(&(*session_id, kind));
    }

    pub async fn dispatch(&self, bytes: Bytes) {
        use farder_protocol::media_datagram::OuterHeader;
        let (session_id, track_kind) = match OuterHeader::parse(&bytes) {
            Ok((header, _payload)) => (header.session_id, header.track_kind),
            Err(_) => return, // not a valid media datagram
        };
        let routes = self.routes.lock().await;
        if let Some(tx) = routes.get(&(session_id, track_kind)) {
            let _ = tx.send(bytes);
        }
    }
}

#[cfg(test)]
mod dispatcher_tests {
    use super::*;

    fn outer_audio_dgram(session: &SessionId) -> Bytes {
        use farder_protocol::media_datagram::OuterHeader;
        use farder_protocol::server::TrackKind;
        let mut v = Vec::new();
        OuterHeader {
            track_kind: TrackKind::Audio,
            session_id: *session,
            frame_id: 0,
            frag_index: 0,
            frag_count: 1,
        }
        .write_to(&mut v);
        v.extend_from_slice(b"opaque-sealed-frame-bytes");
        Bytes::from(v)
    }

    #[tokio::test]
    async fn dispatch_routes_to_registered_session() {
        let dispatcher = MediaInboundDispatcher::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sid: SessionId = [7u8; 16];
        dispatcher.register(sid, TrackKind::Audio, tx).await;

        let frame = outer_audio_dgram(&sid);
        dispatcher.dispatch(frame.clone()).await;

        let received = rx.try_recv().expect("dispatched frame must arrive");
        assert_eq!(received.len(), frame.len());
    }

    #[tokio::test]
    async fn dispatch_drops_unknown_session() {
        let dispatcher = MediaInboundDispatcher::default();
        dispatcher.dispatch(outer_audio_dgram(&[9u8; 16])).await; // must not panic
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
        dispatcher.register(sid, TrackKind::Audio, tx).await;
        dispatcher.unregister(&sid, TrackKind::Audio).await;

        dispatcher.dispatch(outer_audio_dgram(&sid)).await;
        assert!(rx.try_recv().is_err(), "unregistered session must not receive");
    }

    #[tokio::test]
    async fn dispatch_separates_audio_and_video_for_same_session() {
        use farder_protocol::media_datagram::OuterHeader;
        use farder_protocol::server::TrackKind;
        let dispatcher = MediaInboundDispatcher::default();
        let sid: SessionId = [5u8; 16];
        let (atx, mut arx) = mpsc::unbounded_channel();
        let (vtx, mut vrx) = mpsc::unbounded_channel();
        dispatcher.register(sid, TrackKind::Audio, atx).await;
        dispatcher.register(sid, TrackKind::Video, vtx).await;

        fn dgram(sid: &SessionId, kind: TrackKind) -> Bytes {
            let mut v = Vec::new();
            OuterHeader { track_kind: kind, session_id: *sid, frame_id: 0, frag_index: 0, frag_count: 1 }
                .write_to(&mut v);
            v.extend_from_slice(b"payload");
            Bytes::from(v)
        }
        dispatcher.dispatch(dgram(&sid, TrackKind::Video)).await;
        dispatcher.dispatch(dgram(&sid, TrackKind::Audio)).await;
        assert!(arx.try_recv().is_ok(), "audio route gets the audio datagram");
        assert!(vrx.try_recv().is_ok(), "video route gets the video datagram");
        assert!(arx.try_recv().is_err(), "audio route does NOT get the video datagram");
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
    /// Tell the server we (un)muted, so it can relay the state to peers.
    async fn set_mute(&self, muted: bool) -> Result<(), String>;
    /// Tell the server we (un)deafened (relayed to peers + suppresses fanout to us).
    async fn set_deafen(&self, deafened: bool) -> Result<(), String>;
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
    /// Screen/game audio rings, mixed alongside `peer_rings` with independent gain.
    pub screen_audio_rings: crate::voice::mixer::PeerRings,
    pub muted: Arc<std::sync::atomic::AtomicBool>,
    pub gate: crate::voice::gate::GateMode,
    pub speak_threshold: Arc<std::sync::atomic::AtomicU32>,
    pub local_speaking_tx: tokio::sync::watch::Sender<bool>,
    pub input_level_tx: tokio::sync::watch::Sender<f32>,
    pub datagram_sink: Box<dyn Fn(Bytes) + Send + Sync + 'static>,
    /// Saved input device name (None = system default). Passed to `start_capture`.
    pub input_device: Option<String>,
    /// Saved output device name (None = system default). Passed to `start_playback`.
    pub output_device: Option<String>,
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
        // Use the saved device name if one was set; fall back to None (system default).
        let pcm_rx = backend.start_capture(params.input_device.as_deref(), format)?;
        let playback_tx = backend.start_playback(params.output_device.as_deref(), format)?;

        let aec_ref = Arc::new(std::sync::Mutex::new(vec![0.0f32; OPUS_FRAME_SAMPLES_MONO]));

        // Mixer.
        let mixer_aec = aec_ref.clone();
        let mixer_rings = params.peer_rings.clone();
        let mixer_screen_rings = params.screen_audio_rings.clone();
        tokio::task::spawn_blocking(move || {
            crate::voice::mixer::run(crate::voice::mixer::MixerTaskConfig {
                peer_rings: mixer_rings,
                screen_audio_rings: mixer_screen_rings,
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
        let gate = params.gate;
        let speak_threshold = params.speak_threshold;
        let speak_tx = params.local_speaking_tx;
        let input_level_tx = params.input_level_tx;
        let datagram_sink = params.datagram_sink;
        tokio::task::spawn_blocking(move || {
            crate::voice::send::run(
                crate::voice::send::SendTaskConfig {
                    pcm_rx,
                    apm: crate::voice::apm::AudioProcessor::new(),
                    gate,
                    session_id,
                    stream_key,
                    speaker_pk,
                    aec_ref: send_aec,
                    speak_threshold,
                    input_level_tx,
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
    /// Live speaking RMS threshold (`f32::to_bits`), shared with the active
    /// send task. Updated by the mic-sensitivity slider; read each frame.
    speak_threshold: Arc<std::sync::atomic::AtomicU32>,
}

struct Inner {
    state: VoiceState,
    muted: Arc<AtomicBool>,
    deafened: Arc<AtomicBool>,
    transmit: Arc<AtomicBool>,
    pre_deafen_muted: bool,
    /// Set while a call is active.
    active: Option<ActiveCall>,
}

struct ActiveCall {
    server: Arc<dyn ServerSession>,
    pipeline: Option<Box<dyn VoicePipelineHandle>>,
    peer_rings: crate::voice::mixer::PeerRings,
    /// Per-(peer-session) screen/game audio rings, each with its OWN gain
    /// (independent of that peer's voice gain). Summed by the mixer alongside
    /// `peer_rings`.
    screen_audio_rings: crate::voice::mixer::PeerRings,
    peers: HashMap<SessionId, PeerEntry>,
    /// Per-(peer-session, track) stream keys delivered via StreamKeyOffer events.
    /// A peer's session has independent audio + video keys.
    peer_keys: HashMap<(SessionId, TrackKind), [u8; 32]>,
    /// Per-session peer public key (set on the first key offer for the session).
    peer_pubkeys: HashMap<SessionId, PublicKey>,
    /// Per-session video recv tasks (independent of the audio `peers` map).
    video_peers: HashMap<SessionId, VideoPeerEntry>,
    /// Per-session screen-audio recv tasks (independent of the `peers` map).
    screen_audio_peers: HashMap<SessionId, ScreenAudioPeerEntry>,
    /// session_id → (muted, deafened) learned from StreamJoined / StreamStateChanged.
    /// Seeds VoicePeer when the matching TrackEnabled registers the peer.
    peer_status: HashMap<SessionId, (bool, bool)>,
    /// pubkey hex -> gain, snapshotted from settings at join. Seeds new peers.
    peer_volumes: HashMap<String, f32>,
    gate_is_ptt: bool,
    /// Our own stream session id (from JoinStream). Video share seals frames
    /// under this session, same as audio.
    my_session_id: SessionId,
    /// The channel we're in (for re-querying members on a late-joiner re-offer).
    channel_id: u64,
    /// Connection-quality poller task; aborted on leave. `None` when no
    /// QUIC connection was supplied (e.g. tests).
    quality_poller: Option<JoinHandle<()>>,
    /// Active outbound screen share (None = not sharing). One per call.
    video_share: Option<VideoShareState>,
}

struct PeerEntry {
    pubkey: PublicKey,
    recv_handle: JoinHandle<()>,
    /// Held so the dispatcher's route stays valid for this peer.
    #[allow(dead_code)]
    datagram_tx: mpsc::UnboundedSender<Bytes>,
}

struct VideoPeerEntry {
    recv_handle: JoinHandle<()>,
    #[allow(dead_code)]
    datagram_tx: mpsc::UnboundedSender<Bytes>,
}

struct ScreenAudioPeerEntry {
    recv_handle: JoinHandle<()>,
    /// Held so the dispatcher's route stays valid for this peer.
    #[allow(dead_code)]
    datagram_tx: mpsc::UnboundedSender<Bytes>,
}

struct VideoShareState {
    stop: Arc<AtomicBool>,
    #[allow(dead_code)]
    force_keyframe: Arc<AtomicBool>,
    backend: Box<dyn crate::display::DisplayBackend>,
    /// The video stream key (for re-offering to late joiners).
    #[allow(dead_code)]
    video_key: [u8; 32],
    #[allow(dead_code)]
    thread: std::thread::JoinHandle<()>,
    /// Screen-audio stream key (for late-joiner re-offer). None if capture failed.
    screen_audio_key: Option<[u8; 32]>,
    /// Stop flag for the screen-audio send loop.
    screen_audio_stop: Arc<AtomicBool>,
    /// Running WASAPI loopback capture (dropping/stopping it ends the capture thread).
    #[allow(dead_code)]
    screen_audio_capture: Option<Box<dyn crate::screen_audio::ScreenAudioCapture>>,
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
                    transmitting: false,
                    peers: vec![],
                },
                muted: Arc::new(AtomicBool::new(false)),
                deafened: Arc::new(AtomicBool::new(false)),
                transmit: Arc::new(AtomicBool::new(false)),
                pre_deafen_muted: false,
                active: None,
            })),
            emitter,
            pipeline_factory,
            speak_threshold: Arc::new(std::sync::atomic::AtomicU32::new(
                DEFAULT_SPEAK_THRESHOLD.to_bits(),
            )),
        }
    }

    /// Set the live speaking-detection threshold (the mic-sensitivity slider).
    /// Takes effect immediately for the active call's send task.
    pub fn set_speak_threshold(&self, threshold: f32) {
        self.speak_threshold
            .store(threshold.to_bits(), std::sync::atomic::Ordering::Release);
    }

    pub async fn state(&self) -> VoiceState {
        self.inner.lock().await.state.clone()
    }

    #[cfg(test)]
    pub async fn current_gate_is_ptt(&self) -> bool {
        self.inner
            .lock()
            .await
            .active
            .as_ref()
            .map(|c| c.gate_is_ptt)
            .unwrap_or(false)
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
            .and_then(|c| c.peer_pubkeys.get(session_id).cloned())
    }

    /// Back-compat join used by existing tests: Open Mic, no saved volumes.
    pub async fn join(
        &self,
        channel_id: u64,
        server: Arc<dyn ServerSession>,
    ) -> Result<(), String> {
        self.join_with_config(channel_id, server, JoinConfig::default())
            .await
    }

    pub async fn join_with_config(
        &self,
        channel_id: u64,
        server: Arc<dyn ServerSession>,
        config: JoinConfig,
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
        let screen_audio_rings: crate::voice::mixer::PeerRings = Default::default();
        let muted_flag = self.inner.lock().await.muted.clone();
        let transmit_flag = self.inner.lock().await.transmit.clone();
        let (speak_tx, mut speak_rx) = tokio::sync::watch::channel(false);
        let (level_tx, mut level_rx) = tokio::sync::watch::channel(0.0f32);
        let server_for_sink = server.clone();
        let pipeline = self.pipeline_factory.spawn(PipelineParams {
            session_id,
            stream_key,
            speaker_pk: my_pk_bytes,
            peer_rings: peer_rings.clone(),
            screen_audio_rings: screen_audio_rings.clone(),
            muted: muted_flag,
            gate: gate_for_mode(config.mode, transmit_flag),
            speak_threshold: self.speak_threshold.clone(),
            local_speaking_tx: speak_tx,
            input_level_tx: level_tx,
            datagram_sink: Box::new(move |b: Bytes| {
                let _ = server_for_sink.send_datagram(b);
            }),
            input_device: config.input_device.clone(),
            output_device: config.output_device.clone(),
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

        // 5a. Input-level forwarder (drives the mic-sensitivity meter in
        // settings). Emits the raw RMS ~10x/sec while the call is active.
        let emitter_for_level = self.emitter.clone();
        tokio::spawn(async move {
            while level_rx.changed().await.is_ok() {
                let level = *level_rx.borrow();
                emitter_for_level.emit(
                    "voice://input-level",
                    serde_json::json!({ "level": level }),
                );
            }
        });

        // 5b. Connection-quality poller (only when a real connection is
        // supplied). Samples QUIC path stats ~1/s and emits the tier numbers.
        // Aborted in leave().
        let quality_poller = config.connection.as_ref().map(|conn| {
            let conn = conn.clone();
            let emitter = self.emitter.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
                loop {
                    tick.tick().await;
                    let p = conn.stats().path;
                    let (rtt_ms, loss_pct) =
                        quality_from_stats(p.rtt, p.lost_packets, p.sent_packets);
                    emitter.emit(
                        "voice://connection-quality",
                        serde_json::json!({ "rtt_ms": rtt_ms, "loss_pct": loss_pct }),
                    );
                }
            })
        });

        // 6. Commit state + emit.
        let snap = {
            let mut inner = self.inner.lock().await;
            inner.state.channel_id = Some(channel_id_bytes(channel_id));
            inner.active = Some(ActiveCall {
                server,
                pipeline: Some(pipeline),
                peer_rings,
                screen_audio_rings,
                peers: HashMap::new(),
                peer_keys: HashMap::new(),
                peer_pubkeys: HashMap::new(),
                video_peers: HashMap::new(),
                screen_audio_peers: HashMap::new(),
                peer_status: HashMap::new(),
                peer_volumes: config.peer_volumes.clone(),
                gate_is_ptt: matches!(config.mode, VoiceMode::PushToTalk),
                my_session_id: session_id,
                channel_id,
                quality_poller,
                video_share: None,
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
            // Tear down any active screen share BEFORE the call is dropped, so
            // the capture+encode thread ends and stops emitting datagrams on the
            // old connection. Dropping VideoShareState does NOT stop it; we must
            // set stop + call stop_capture explicitly (the whole stream is going
            // away so we don't bother disabling the Video track here).
            if let Some(s) = call.video_share.take() {
                shutdown_video_share(s);
            }
            let _ = call.server.leave_stream().await;
            // Stop the audio pipeline (closes channels → send/mixer exit).
            if let Some(p) = call.pipeline.take() {
                p.stop();
            }
            // Stop the connection-quality poller.
            if let Some(h) = call.quality_poller.take() {
                h.abort();
            }
            // Abort every peer recv task and unregister its dispatcher route.
            let dispatcher = call.server.dispatcher();
            for (sid, peer) in call.peers.drain() {
                peer.recv_handle.abort();
                let d = dispatcher.clone();
                tokio::spawn(async move {
                    d.unregister(&sid, TrackKind::Audio).await;
                });
            }
            for (sid, entry) in call.video_peers.drain() {
                entry.recv_handle.abort();
                let d = dispatcher.clone();
                tokio::spawn(async move {
                    d.unregister(&sid, TrackKind::Video).await;
                });
            }
            for (sid, entry) in call.screen_audio_peers.drain() {
                entry.recv_handle.abort();
                let d = dispatcher.clone();
                tokio::spawn(async move {
                    d.unregister(&sid, TrackKind::ScreenAudio).await;
                });
            }
            call.peer_rings.lock().expect("peer_rings poisoned").clear();
            call.screen_audio_rings
                .lock()
                .expect("screen_audio_rings poisoned")
                .clear();
        }

        // Phase 3: re-lock to commit the cleared state, then emit.
        let snap = {
            let mut inner = self.inner.lock().await;
            inner.state = VoiceState {
                channel_id: None,
                muted: false,
                deafened: false,
                transmitting: false,
                peers: vec![],
            };
            inner.muted.store(false, Ordering::Release);
            inner.deafened.store(false, Ordering::Release);
            inner.transmit.store(false, Ordering::Release);
            inner.pre_deafen_muted = false;
            inner.state.clone()
        };
        self.emitter
            .emit("voice://state-changed", serde_json::to_value(&snap).unwrap_or_default());
        Ok(())
    }

    pub async fn set_mute(&self, muted: bool) -> Result<(), String> {
        let (snap, server) = {
            let mut inner = self.inner.lock().await;
            inner.muted.store(muted, Ordering::Release);
            inner.state.muted = muted;
            let server = inner.active.as_ref().map(|c| c.server.clone());
            (inner.state.clone(), server)
        };
        if let Some(server) = server {
            if let Err(e) = server.set_mute(muted).await {
                eprintln!("[voice] set_mute server notify failed: {e}");
            }
        }
        self.emitter
            .emit("voice://state-changed", serde_json::to_value(&snap).unwrap_or_default());
        Ok(())
    }

    pub async fn set_deafen(&self, deafened: bool) -> Result<(), String> {
        let (snap, server) = {
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
            let server = inner.active.as_ref().map(|c| c.server.clone());
            (inner.state.clone(), server)
        };
        if let Some(server) = server {
            if let Err(e) = server.set_deafen(deafened).await {
                eprintln!("[voice] set_deafen server notify failed: {e}");
            }
        }
        self.emitter
            .emit("voice://state-changed", serde_json::to_value(&snap).unwrap_or_default());
        Ok(())
    }

    /// Set the PTT transmit flag explicitly and re-emit state.
    pub async fn set_transmitting(&self, transmitting: bool) -> Result<(), String> {
        let snap = {
            let mut inner = self.inner.lock().await;
            inner.transmit.store(transmitting, Ordering::Release);
            inner.state.transmitting = transmitting;
            inner.state.clone()
        };
        self.emitter.emit(
            "voice://state-changed",
            serde_json::to_value(&snap).unwrap_or_default(),
        );
        Ok(())
    }

    /// Toggle the PTT transmit flag and return the new value.
    pub async fn toggle_transmit(&self) -> bool {
        let new_val = {
            let inner = self.inner.lock().await;
            !inner.transmit.load(Ordering::Acquire)
        };
        let _ = self.set_transmitting(new_val).await;
        new_val
    }

    /// Clamp a per-peer volume (keyed by pubkey hex) and, if that peer is
    /// currently in the call, update its live mixer gain. Persistence is
    /// handled by the command layer so the controller stays independent of
    /// the settings file in tests.
    pub async fn set_peer_volume(&self, pubkey_hex: String, volume: f32) -> Result<(), String> {
        let clamped = volume.clamp(0.0, 2.0);
        let inner = self.inner.lock().await;
        if let Some(call) = inner.active.as_ref() {
            let rings = call.peer_rings.lock().expect("peer_rings poisoned");
            for (sid, (_, gain)) in rings.iter() {
                if let Some(entry) = call.peers.get(sid) {
                    if entry.pubkey.to_string() == pubkey_hex {
                        gain.store(clamped.to_bits(), Ordering::Release);
                    }
                }
            }
        }
        Ok(())
    }

    // ── Inbound media-event handlers (Task 11 wires server events to these) ──

    /// Server told us a peer offered us a wrapped stream key. Unwrap it with
    /// our long-lived keypair and remember it for the matching TrackEnabled.
    pub async fn on_stream_key_offer(
        &self,
        session_id: SessionId,
        kind: TrackKind,
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
                call.peer_keys.insert((session_id, kind), key);
                call.peer_pubkeys.insert(session_id, sender_pubkey);
            }
            Err(e) => {
                eprintln!("[voice] unwrap_stream_key: {e}");
            }
        }
    }

    /// Server announced a peer enabled a track. Dispatches to the audio or
    /// video handler based on `kind`.
    pub async fn on_peer_track_enabled(
        &self,
        session_id: SessionId,
        peer_pubkey: PublicKey,
        kind: TrackKind,
    ) {
        match kind {
            TrackKind::Audio => self.on_peer_audio_track_enabled(session_id, peer_pubkey).await,
            TrackKind::Video => self.on_peer_video_track_enabled(session_id, peer_pubkey).await,
            TrackKind::ScreenAudio => self.on_peer_screen_audio_track_enabled(session_id, peer_pubkey).await,
        }
    }

    /// Spawn an audio RecvTask, register a per-peer ring with the mixer, and
    /// register a dispatcher route so inbound datagrams get to this peer.
    async fn on_peer_audio_track_enabled(&self, session_id: SessionId, peer_pubkey: PublicKey) {
        let mut inner = self.inner.lock().await;
        let deafened_flag = inner.deafened.clone();
        let call = match inner.active.as_mut() {
            Some(c) => c,
            None => return,
        };
        if call.peers.contains_key(&session_id) {
            return; // already running
        }
        let stream_key = match call.peer_keys.get(&(session_id, TrackKind::Audio)) {
            Some(k) => *k,
            None => {
                eprintln!(
                    "[voice] TrackEnabled(Audio) for unknown session; missing StreamKeyOffer?"
                );
                return;
            }
        };
        let ring = Arc::new(PeerPcmRing::new(10));
        let pubkey_hex = peer_pubkey.to_string();
        let seed_gain = call
            .peer_volumes
            .get(&pubkey_hex)
            .copied()
            .unwrap_or(1.0)
            .clamp(0.0, 2.0);
        let gain = Arc::new(std::sync::atomic::AtomicU32::new(seed_gain.to_bits()));
        call.peer_rings
            .lock()
            .expect("peer_rings poisoned")
            .insert(session_id, (ring.clone(), gain));

        let (tx, rx) = mpsc::unbounded_channel::<Bytes>();
        let dispatcher = call.server.dispatcher();
        let tx_for_register = tx.clone();
        tokio::spawn(async move {
            dispatcher.register(session_id, TrackKind::Audio, tx_for_register).await;
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
        let (m, d) = call
            .peer_status
            .get(&session_id)
            .copied()
            .unwrap_or((false, false));
        if !inner.state.peers.iter().any(|p| p.pubkey == peer_pubkey) {
            inner.state.peers.push(VoicePeer {
                pubkey: peer_pubkey,
                speaking: false,
                muted: m,
                deafened: d,
            });
        }
        let snap = inner.state.clone();
        drop(inner);
        self.emitter
            .emit("voice://state-changed", serde_json::to_value(&snap).unwrap_or_default());
    }

    /// Spawn a video recv task for the peer and register a dispatcher route
    /// so inbound video datagrams get to this peer's recv task.
    async fn on_peer_video_track_enabled(&self, session_id: SessionId, peer_pubkey: PublicKey) {
        let emitter = self.emitter.clone();
        let mut inner = self.inner.lock().await;
        let call = match inner.active.as_mut() {
            Some(c) => c,
            None => return,
        };
        if call.video_peers.contains_key(&session_id) {
            return; // already running
        }
        let stream_key = match call.peer_keys.get(&(session_id, TrackKind::Video)) {
            Some(k) => *k,
            None => {
                eprintln!("[voice] TrackEnabled(Video) for session with no video key; missing StreamKeyOffer(Video)?");
                return;
            }
        };
        let (tx, rx) = mpsc::unbounded_channel::<Bytes>();
        let dispatcher = call.server.dispatcher();
        let tx_for_register = tx.clone();
        tokio::spawn(async move {
            dispatcher.register(session_id, TrackKind::Video, tx_for_register).await;
        });

        let session_hex = hex::encode(session_id);
        let peer_hex = peer_pubkey.to_string();
        let recv_handle = tokio::spawn(async move {
            crate::voice::recv_video::run(
                crate::voice::recv_video::RecvVideoConfig { session_id, stream_key, datagram_rx: rx },
                move |v| {
                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&v.data);
                    emitter.emit(
                        "voice://peer-video-frame",
                        serde_json::json!({ "session": session_hex, "pubkey": peer_hex, "data": b64, "key": v.is_keyframe, "seq": v.seq }),
                    );
                },
            )
            .await;
        });

        call.video_peers.insert(session_id, VideoPeerEntry { recv_handle, datagram_tx: tx });
    }

    /// Spawn a screen/game-audio RecvTask for the peer, register an independent
    /// ring + gain (NOT tied to that peer's voice gain) with the mixer, and
    /// register a dispatcher route so inbound ScreenAudio datagrams reach this
    /// task. Mirrors `on_peer_audio_track_enabled` minus the VoicePeer /
    /// peer_status / peer_volumes / UI-state plumbing.
    async fn on_peer_screen_audio_track_enabled(&self, session_id: SessionId, _peer_pubkey: PublicKey) {
        let mut inner = self.inner.lock().await;
        let deafened_flag = inner.deafened.clone();
        let call = match inner.active.as_mut() {
            Some(c) => c,
            None => return,
        };
        if call.screen_audio_peers.contains_key(&session_id) {
            return; // already running
        }
        let stream_key = match call.peer_keys.get(&(session_id, TrackKind::ScreenAudio)) {
            Some(k) => *k,
            None => {
                eprintln!("[voice] TrackEnabled(ScreenAudio) for session with no key; missing StreamKeyOffer(ScreenAudio)?");
                return;
            }
        };
        let ring = Arc::new(PeerPcmRing::new(10));
        // Fresh, independent gain (default unity). The screen-audio volume
        // slider (Phase E) will drive this ring's gain, separate from voice.
        let gain = Arc::new(std::sync::atomic::AtomicU32::new(1.0f32.to_bits()));
        call.screen_audio_rings
            .lock()
            .expect("screen_audio_rings poisoned")
            .insert(session_id, (ring.clone(), gain));

        let (tx, rx) = mpsc::unbounded_channel::<Bytes>();
        let dispatcher = call.server.dispatcher();
        let tx_for_register = tx.clone();
        tokio::spawn(async move {
            dispatcher.register(session_id, TrackKind::ScreenAudio, tx_for_register).await;
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

        call.screen_audio_peers
            .insert(session_id, ScreenAudioPeerEntry { recv_handle, datagram_tx: tx });
    }

    /// Peer disabled a track. For Video: tears down the video recv task +
    /// dispatcher route. For Audio: tears down their recv task + dispatcher
    /// route + drops their ring, then re-emits the full state snapshot.
    pub async fn on_peer_track_disabled(&self, session_id: SessionId, kind: TrackKind) {
        if matches!(kind, TrackKind::Video) {
            let mut inner = self.inner.lock().await;
            if let Some(call) = inner.active.as_mut() {
                if let Some(entry) = call.video_peers.remove(&session_id) {
                    entry.recv_handle.abort();
                    let dispatcher = call.server.dispatcher();
                    tokio::spawn(async move {
                        dispatcher.unregister(&session_id, TrackKind::Video).await;
                    });
                }
            }
            return;
        }
        if matches!(kind, TrackKind::ScreenAudio) {
            let mut inner = self.inner.lock().await;
            if let Some(call) = inner.active.as_mut() {
                if let Some(entry) = call.screen_audio_peers.remove(&session_id) {
                    entry.recv_handle.abort();
                    call.screen_audio_rings
                        .lock()
                        .expect("screen_audio_rings poisoned")
                        .remove(&session_id);
                    let dispatcher = call.server.dispatcher();
                    tokio::spawn(async move {
                        dispatcher.unregister(&session_id, TrackKind::ScreenAudio).await;
                    });
                }
            }
            return;
        }
        // Audio teardown: remove peer, ring, dispatcher route, and re-emit state.
        let snap = {
            let mut inner = self.inner.lock().await;
            let removed_pubkey: Option<PublicKey> = {
                let call = match inner.active.as_mut() {
                    Some(c) => c,
                    None => return,
                };
                // Drop any mute/deafen seed for this session so it can't leak or
                // seed a rebuilt VoicePeer with stale state if the id recurs.
                call.peer_status.remove(&session_id);
                if let Some(peer) = call.peers.remove(&session_id) {
                    peer.recv_handle.abort();
                    call.peer_rings
                        .lock()
                        .expect("peer_rings poisoned")
                        .remove(&session_id);
                    let dispatcher = call.server.dispatcher();
                    tokio::spawn(async move {
                        dispatcher.unregister(&session_id, TrackKind::Audio).await;
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
        // A peer leaving the call emits only StreamLeft (no per-track TrackDisabled),
        // so we must tear down every track kind for this session here — otherwise a
        // sharing peer's video/screen-audio recv task + ring leak (the screen-audio
        // ring would keep being mixed as silence forever). Each on_peer_track_disabled
        // arm guards on presence, so calling it for a kind the peer never had is a no-op.
        self.on_peer_track_disabled(session_id, TrackKind::Audio).await;
        self.on_peer_track_disabled(session_id, TrackKind::Video).await;
        self.on_peer_track_disabled(session_id, TrackKind::ScreenAudio).await;
    }

    /// A peer's StreamJoined arrived — remember their initial mute/deafen so
    /// it seeds the VoicePeer when their TrackEnabled registers them.
    pub async fn on_peer_stream_joined(&self, session_id: SessionId, muted: bool, deafened: bool) {
        let reoffer = {
            let mut inner = self.inner.lock().await;
            match inner.active.as_mut() {
                Some(call) => {
                    call.peer_status.insert(session_id, (muted, deafened));
                    // If we're sharing, re-offer the video key to everyone (the
                    // new member is now in get_media_state) and force a keyframe
                    // so they can start decoding immediately. Capture what we need
                    // to offer AFTER releasing the lock (don't hold the inner lock
                    // across the offer_track_key await).
                    if let Some(s) = call.video_share.as_ref() {
                        s.force_keyframe.store(true, Ordering::Relaxed);
                        Some((call.server.clone(), call.channel_id, s.video_key, s.screen_audio_key))
                    } else {
                        None
                    }
                }
                None => None,
            }
        };
        if let Some((server, channel_id, video_key, screen_audio_key)) = reoffer {
            if let Err(e) = offer_track_key(&server, channel_id, TrackKind::Video, &video_key).await {
                eprintln!("[voice] re-offer of video key on peer join failed: {e}");
            }
            // Re-enable the Video track so the server re-broadcasts TrackEnabled
            // to the new joiner (the server doesn't replay existing tracks on
            // join). The key offer above is sent first on this ordered stream, so
            // the joiner's on_peer_video_track_enabled finds it and spins up the
            // recv route. Existing viewers get a duplicate TrackEnabled, which is
            // a no-op (their video_peers route already exists).
            if let Err(e) = server.enable_track(TrackKind::Video).await {
                eprintln!("[voice] re-enable Video on peer join failed: {e}");
            }
            // If we're ALSO sharing screen/game audio, re-offer its key + re-enable
            // the ScreenAudio track so the late joiner can decrypt + receive it too.
            if let Some(sa_key) = screen_audio_key {
                if let Err(e) = offer_track_key(&server, channel_id, TrackKind::ScreenAudio, &sa_key).await {
                    eprintln!("[voice] re-offer screen-audio key on join failed: {e}");
                }
                if let Err(e) = server.enable_track(TrackKind::ScreenAudio).await {
                    eprintln!("[voice] re-enable ScreenAudio on join failed: {e}");
                }
            }
        }
    }

    /// A peer toggled mute/deafen — update the live peer (if registered) and
    /// the seed map, then re-emit the full state snapshot.
    pub async fn on_peer_stream_state(&self, session_id: SessionId, muted: bool, deafened: bool) {
        let snap = {
            let mut inner = self.inner.lock().await;
            let pk = inner
                .active
                .as_ref()
                .and_then(|c| c.peers.get(&session_id).map(|p| p.pubkey.clone()));
            if let Some(call) = inner.active.as_mut() {
                call.peer_status.insert(session_id, (muted, deafened));
            }
            if let Some(pk) = pk {
                for peer in inner.state.peers.iter_mut() {
                    if peer.pubkey == pk {
                        peer.muted = muted;
                        peer.deafened = deafened;
                        break;
                    }
                }
            }
            inner.state.clone()
        };
        self.emitter
            .emit("voice://state-changed", serde_json::to_value(&snap).unwrap_or_default());
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

    /// Start sharing our screen into the active call: derive a video key, offer
    /// it to current members, enable the Video track, and drive the Phase B
    /// capture->encode loop into the C1 VideoSender over this call's connection.
    pub async fn start_screen_share(&self, fps: u32, max_width: u32, max_height: u32, source_id: Option<String>, audio_device_id: Option<String>) -> Result<(), String> {
        // Fail-fast: validate the encoder can init (it's built inside the thread).
        drop(crate::video_encoder::H264Encoder::new()?);

        let (server, my_session_id, channel_id, my_pk_bytes, already) = {
            let inner = self.inner.lock().await;
            match inner.active.as_ref() {
                Some(c) => {
                    let kp = c.server.my_keypair();
                    (c.server.clone(), c.my_session_id, c.channel_id, *kp.public_key().as_bytes(), c.video_share.is_some())
                }
                None => return Err("not in a voice channel".into()),
            }
        };
        if already {
            return Err("already sharing your screen".into());
        }

        // Derive + offer the video key to current members.
        let video_key = farder_crypto::media::derive_stream_key();
        offer_track_key(&server, channel_id, TrackKind::Video, &video_key).await?;

        // Start capture.
        let backend = crate::display::make_display_backend();
        let source_id = match source_id {
            Some(id) => id,
            None => {
                let sources = backend.enumerate_sources()?;
                sources.first().map(|s| s.id.clone()).ok_or("no capture source")?
            }
        };
        let rx = backend.start_capture(&source_id, crate::display::DisplayFormat { fps, max_width, max_height })?;

        let stop = Arc::new(AtomicBool::new(false));
        let force_keyframe = Arc::new(AtomicBool::new(false));

        // Spawn the capture->encode->send thread (encoder is !Send -> built inside).
        let stop_t = stop.clone();
        let force_t = force_keyframe.clone();
        let server_t = server.clone();
        let thread = std::thread::spawn(move || {
            let encoder = match crate::video_encoder::H264Encoder::new() {
                Ok(e) => e,
                Err(e) => { eprintln!("[voice] video encoder init failed: {e}"); return; }
            };
            let mut sender = crate::voice::send_video::VideoSender::new(video_key, my_session_id, my_pk_bytes);
            crate::screenshare::run_encode_loop(rx, encoder, stop_t, force_t, move |enc| {
                sender.send(&enc, |b| { let _ = server_t.send_datagram(b); });
            });
        });

        // Enable the Video track (after capture is up). If this fails the thread
        // is already running with no handle stored, so tear it down before
        // returning rather than leaking the capture+encode thread + backend.
        if let Err(e) = server.enable_track(TrackKind::Video).await {
            stop.store(true, Ordering::Relaxed);
            let _ = backend.stop_capture();
            return Err(e);
        }

        // Best-effort screen/game audio: capture the selected output device and
        // spin up the ScreenAudio send loop under its OWN stream key. If capture
        // fails to start, log and share VIDEO ONLY (don't abort the whole share).
        let screen_audio_stop = Arc::new(AtomicBool::new(false));
        let mut screen_audio_key: Option<[u8; 32]> = None;
        let mut screen_audio_capture: Option<Box<dyn crate::screen_audio::ScreenAudioCapture>> = None;
        match crate::screen_audio::start_capture(audio_device_id) {
            Ok((cap, pcm_rx)) => {
                let sa_key = farder_crypto::media::derive_stream_key();
                if let Err(e) = offer_track_key(&server, channel_id, TrackKind::ScreenAudio, &sa_key).await {
                    eprintln!("[voice] offer screen-audio key failed: {e}");
                }
                let sa_stop_t = screen_audio_stop.clone();
                let server_sa = server.clone();
                std::thread::spawn(move || {
                    crate::voice::send_screen_audio::run(
                        crate::voice::send_screen_audio::ScreenAudioSendConfig {
                            pcm_rx,
                            session_id: my_session_id,
                            stream_key: sa_key,
                            speaker_pk: my_pk_bytes,
                            datagram_sink: Box::new(move |b| { let _ = server_sa.send_datagram(b); }),
                        },
                        sa_stop_t,
                    );
                });
                screen_audio_key = Some(sa_key);
                screen_audio_capture = Some(cap);
                let _ = server.enable_track(TrackKind::ScreenAudio).await;
            }
            Err(e) => eprintln!("[voice] screen-audio capture failed to start (sharing video only): {e}"),
        }

        // Record share state. Decide the outcome while borrowing `inner`; only
        // hand the just-built share to the call when we're actually storing it.
        // Held in an Option so the borrow checker tracks the move via `.take()`
        // (unconditional from its view): on a bail path the Option still owns the
        // share so we can tear it down after releasing the lock (we can't drop the
        // lock while `call` borrows it). `bail` carries the error for that path.
        let mut share = Some(VideoShareState {
            stop,
            force_keyframe,
            backend,
            video_key,
            thread,
            screen_audio_key,
            screen_audio_stop,
            screen_audio_capture,
        });
        // On a bail path we carry both the error message AND whether to disable
        // the Video track during teardown. The track toggle is a single
        // per-connection on/off (not refcounted), so we must only disable it when
        // nobody else owns it — i.e. the call ended. On a lost start-share race
        // the winning start legitimately enabled the track; disabling it here
        // would clobber the winner's live share.
        let bail: Option<(&'static str, bool)> = {
            let mut inner = self.inner.lock().await;
            match inner.active.as_mut() {
                // Lost a concurrent-start race: another start already installed a
                // video_share. Do NOT overwrite (that would leak theirs) — bail
                // and tear down the share we just built. Leave the track enabled:
                // it's the winner's, not ours.
                Some(call) if call.video_share.is_some() => {
                    Some(("already sharing your screen", false))
                }
                Some(call) => {
                    call.video_share = share.take();
                    None
                }
                // The call ended while we were starting. Nobody else owns the
                // track, so disable the one we enabled above.
                None => Some(("not in a voice channel", true)),
            }
            // `inner` lock dropped here.
        };
        if let Some((msg, disable_track)) = bail {
            // `share` was not stored on a bail path, so the Option still owns it.
            // Note whether screen audio was enabled before the share is consumed,
            // so we mirror the Video track disable for ScreenAudio below.
            let had_screen_audio = share.as_ref().map(|s| s.screen_audio_key.is_some()).unwrap_or(false);
            if let Some(s) = share.take() {
                // shutdown_video_share also stops the screen-audio send loop +
                // loopback capture thread (best-effort).
                shutdown_video_share(s);
            }
            // We enabled the Video track above; best-effort disable it only when
            // nobody else owns it (call ended), so we don't leave it enabled with
            // no share behind it — but never clobber a race winner's track. Mirror
            // the same logic for the ScreenAudio track when it was enabled.
            if disable_track {
                let _ = server.disable_track(TrackKind::Video).await;
                if had_screen_audio {
                    let _ = server.disable_track(TrackKind::ScreenAudio).await;
                }
            }
            return Err(msg.into());
        }
        Ok(())
    }

    /// Stop sharing: tear down capture, stop the send loop, disable the track.
    pub async fn stop_screen_share(&self) -> Result<(), String> {
        let (share, server) = {
            let mut inner = self.inner.lock().await;
            match inner.active.as_mut() {
                Some(c) => (c.video_share.take(), Some(c.server.clone())),
                None => (None, None),
            }
        };
        if let Some(s) = share {
            // Remember whether this share also carried screen audio before the
            // share is consumed, so we only disable the ScreenAudio track when it
            // was actually enabled (capture may have failed at start).
            let had_screen_audio = s.screen_audio_key.is_some();
            shutdown_video_share(s);
            if let Some(server) = server {
                let _ = server.disable_track(TrackKind::Video).await;
                if had_screen_audio {
                    let _ = server.disable_track(TrackKind::ScreenAudio).await;
                }
            }
        }
        Ok(())
    }
}

/// Tear down a screen share: stop the capture+encode thread by setting `stop`
/// AND calling `backend.stop_capture()` (which drops the capture sender so
/// `rx.recv()` errors and `run_encode_loop` breaks). Both are required — see
/// the module note: dropping a VideoShareState stops nothing on its own.
/// (Video track disable is handled by the caller where it has the server; on a
/// full call teardown the whole stream is going away so the track dies with it.)
fn shutdown_video_share(s: VideoShareState) {
    s.stop.store(true, Ordering::Relaxed);
    let _ = s.backend.stop_capture();
    // Best-effort screen-audio teardown: stop the send loop + the loopback
    // capture thread. No-ops when screen audio never started (capture None).
    s.screen_audio_stop.store(true, Ordering::Relaxed);
    if let Some(cap) = s.screen_audio_capture.as_ref() {
        cap.stop();
    }
}

/// Wrap `key` for every current channel member (except self) and offer it for
/// the given `kind` track.
async fn offer_track_key(server: &Arc<dyn ServerSession>, channel_id: u64, kind: TrackKind, key: &[u8; 32]) -> Result<(), String> {
    let participants = server.get_media_state(channel_id).await?;
    let keypair = server.my_keypair();
    let my_sk = *keypair.signing_key_bytes();
    let my_pk = *keypair.public_key().as_bytes();
    let wrapped: Vec<(PublicKey, Vec<u8>)> = participants
        .iter()
        .filter(|m| m.public_key.as_bytes() != &my_pk)
        .filter_map(|m| {
            match farder_crypto::media::wrap_stream_key_for_peer(key, &my_sk, m.public_key.as_bytes()) {
                Ok(w) => Some((m.public_key.clone(), w)),
                Err(e) => {
                    eprintln!("[voice] failed to wrap key for a peer: {e}");
                    None
                }
            }
        })
        .collect();
    if !wrapped.is_empty() {
        server.offer_stream_key(kind, wrapped).await?;
    }
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// Tests — controller state machine + event emissions.
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod controller_tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    // ── FakeServerSession ────────────────────────────────────────────────

    struct FakeServerSession {
        keypair: Option<Arc<Keypair>>,
        dispatcher: Arc<MediaInboundDispatcher>,
        calls: StdMutex<Vec<String>>,
        canned_session_id: SessionId,
        set_mute_calls: StdMutex<Vec<bool>>,
        set_deafen_calls: StdMutex<Vec<bool>>,
        /// Extra members returned from get_media_state (besides ourselves).
        members: StdMutex<Vec<VoiceMember>>,
        /// TrackKinds passed to offer_stream_key / enable_track / disable_track,
        /// so video-share tests can assert the Video track specifically.
        offered_kinds: StdMutex<Vec<TrackKind>>,
        enabled_kinds: StdMutex<Vec<TrackKind>>,
        disabled_kinds: StdMutex<Vec<TrackKind>>,
    }

    impl FakeServerSession {
        fn new() -> Arc<Self> {
            Self::build([9u8; 16], Vec::new())
        }
        fn new_with_sid(sid: SessionId) -> Arc<Self> {
            Self::build(sid, Vec::new())
        }
        /// Build a fake whose get_media_state returns the given extra members.
        fn new_with_members(members: Vec<VoiceMember>) -> Arc<Self> {
            Self::build([9u8; 16], members)
        }
        fn build(sid: SessionId, members: Vec<VoiceMember>) -> Arc<Self> {
            Arc::new(Self {
                keypair: Some(Arc::new(Keypair::generate())),
                dispatcher: Arc::new(MediaInboundDispatcher::default()),
                calls: StdMutex::new(Vec::new()),
                canned_session_id: sid,
                set_mute_calls: StdMutex::new(Vec::new()),
                set_deafen_calls: StdMutex::new(Vec::new()),
                members: StdMutex::new(members),
                offered_kinds: StdMutex::new(Vec::new()),
                enabled_kinds: StdMutex::new(Vec::new()),
                disabled_kinds: StdMutex::new(Vec::new()),
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
            Ok(self.members.lock().unwrap().clone())
        }
        async fn offer_stream_key(
            &self,
            k: TrackKind,
            _w: Vec<(PublicKey, Vec<u8>)>,
        ) -> Result<(), String> {
            self.log("offer_stream_key");
            self.offered_kinds.lock().unwrap().push(k);
            Ok(())
        }
        async fn enable_track(&self, k: TrackKind) -> Result<(), String> {
            self.log("enable_track");
            self.enabled_kinds.lock().unwrap().push(k);
            Ok(())
        }
        async fn disable_track(&self, k: TrackKind) -> Result<(), String> {
            self.log("disable_track");
            self.disabled_kinds.lock().unwrap().push(k);
            Ok(())
        }
        async fn set_mute(&self, muted: bool) -> Result<(), String> {
            self.set_mute_calls.lock().unwrap().push(muted);
            Ok(())
        }
        async fn set_deafen(&self, deafened: bool) -> Result<(), String> {
            self.set_deafen_calls.lock().unwrap().push(deafened);
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

    /// Spin up a controller and join a call, returning the controller, the
    /// fake server session (for asserting set_mute/set_deafen relays), and
    /// the emitter. Drains the join emit by leaving callers to account for it.
    async fn setup_joined_call(
    ) -> (Arc<VoiceController>, Arc<FakeServerSession>, Arc<MockEmitter>) {
        let (ctrl, emitter) = make_controller();
        let server = FakeServerSession::new();
        ctrl.join(7, server.clone()).await.unwrap();
        (ctrl, server, emitter)
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

    #[tokio::test]
    async fn fresh_controller_reports_not_transmitting() {
        let (ctrl, _emitter) = make_controller();
        let st = ctrl.state().await;
        assert!(!st.transmitting, "fresh controller must not be transmitting");
    }

    #[test]
    fn gate_for_mode_picks_ptt_only_when_push_to_talk() {
        let flag = Arc::new(AtomicBool::new(false));
        // Open mic -> Open (always passes), transmit flag ignored.
        let g_open = super::gate_for_mode(super::VoiceMode::OpenMic, flag.clone());
        assert!(g_open.pass(&[0.0; 960]));
        // PTT -> Ptt(flag); closed blocks, open passes.
        let g_ptt = super::gate_for_mode(super::VoiceMode::PushToTalk, flag.clone());
        assert!(!g_ptt.pass(&[0.0; 960]));
        flag.store(true, Ordering::Release);
        assert!(g_ptt.pass(&[0.0; 960]));
    }

    #[tokio::test]
    async fn toggle_transmit_flips_state_and_emits() {
        let (ctrl, server, emitter) = setup_joined_call().await;
        let _ = server; // server relay not needed for transmit
        let before = emitter.count("voice://state-changed");

        let now_on = ctrl.toggle_transmit().await;
        assert!(now_on, "first toggle turns transmit on");
        assert!(ctrl.state().await.transmitting);

        let now_off = ctrl.toggle_transmit().await;
        assert!(!now_off, "second toggle turns transmit off");
        assert!(!ctrl.state().await.transmitting);

        assert_eq!(
            emitter.count("voice://state-changed"),
            before + 2,
            "each toggle re-emits state"
        );
    }

    #[tokio::test]
    async fn join_with_ptt_mode_selects_ptt_gate() {
        let (ctrl, _emitter) = make_controller();
        let server = FakeServerSession::new();
        ctrl.join_with_config(
            7,
            server.clone(),
            super::JoinConfig {
                mode: super::VoiceMode::PushToTalk,
                peer_volumes: std::collections::HashMap::new(),
                connection: None,
                input_device: None,
                output_device: None,
            },
        )
        .await
        .unwrap();
        assert!(ctrl.current_gate_is_ptt().await, "PTT mode selects Ptt gate");
        ctrl.leave().await.unwrap();
    }

    #[tokio::test]
    async fn join_with_open_mic_mode_selects_open_gate() {
        let (ctrl, _emitter) = make_controller();
        let server = FakeServerSession::new();
        ctrl.join_with_config(
            7,
            server.clone(),
            super::JoinConfig {
                mode: super::VoiceMode::OpenMic,
                peer_volumes: std::collections::HashMap::new(),
                connection: None,
                input_device: None,
                output_device: None,
            },
        )
        .await
        .unwrap();
        assert!(!ctrl.current_gate_is_ptt().await, "Open Mic selects Open gate");
        ctrl.leave().await.unwrap();
    }

    #[test]
    fn quality_from_stats_computes_ms_and_loss_pct() {
        use std::time::Duration;
        // 50 ms rtt, 3 lost of 100 sent => 50.0 ms, 3.0%.
        let (rtt_ms, loss_pct) = super::quality_from_stats(Duration::from_millis(50), 3, 100);
        assert!((rtt_ms - 50.0).abs() < 1e-6);
        assert!((loss_pct - 3.0).abs() < 1e-6);
        // 10 lost of 1000 sent => 1.0%.
        let (_, loss1) = super::quality_from_stats(Duration::from_millis(50), 10, 1000);
        assert!((loss1 - 1.0).abs() < 1e-6);
        // Zero sent must not divide-by-zero: max(1, sent) => 0.0% loss.
        let (_, loss0) = super::quality_from_stats(Duration::from_millis(10), 0, 0);
        assert_eq!(loss0, 0.0);
    }

    // Join/leave with a `None` connection (the only path testable on this
    // mock/WSL box) must not spawn a poller and must round-trip cleanly.
    #[tokio::test]
    async fn join_with_config_none_connection_round_trips() {
        let (ctrl, _emitter) = make_controller();
        let server = FakeServerSession::new();
        ctrl.join_with_config(7, server.clone(), super::JoinConfig::default())
            .await
            .unwrap();
        assert!(ctrl.state().await.channel_id.is_some());
        ctrl.leave().await.unwrap();
        assert!(ctrl.state().await.channel_id.is_none());
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
        ctrl.on_stream_key_offer([0xAA; 16], TrackKind::Audio, peer_kp.public_key(), wrapped)
            .await;
        // No public accessor to peek peer_keys, but if we got here without
        // panicking, the unwrap path is exercised.
        ctrl.leave().await.unwrap();
    }

    #[tokio::test]
    async fn set_mute_notifies_server_and_emits_state() {
        let (ctrl, fake, emitter) = setup_joined_call().await;
        ctrl.set_mute(true).await.unwrap();
        assert_eq!(fake.set_mute_calls.lock().unwrap().as_slice(), &[true]);
        assert!(emitter.count("voice://state-changed") >= 1);
    }

    #[tokio::test]
    async fn on_peer_stream_state_updates_peer_and_emits() {
        let (ctrl, _fake, emitter) = setup_joined_call().await;
        let sid: SessionId = [9u8; 16];
        let pk = PublicKey::from_bytes([3u8; 32]);

        // Register the peer first: offer a stream key (so peer_keys has the
        // session_id), then TrackEnabled registers the VoicePeer. The wrapped
        // key must round-trip against the fake server's keypair.
        let peer_kp = Keypair::generate();
        let our_kp = _fake.my_keypair();
        let key = farder_crypto::media::derive_stream_key();
        let wrapped = farder_crypto::media::wrap_stream_key_for_peer(
            &key,
            peer_kp.signing_key_bytes(),
            our_kp.public_key().as_bytes(),
        )
        .unwrap();
        // We register with `pk` as the peer pubkey for the UI VoicePeer; the
        // key offer's sender pubkey only matters for the unwrap round-trip.
        ctrl.on_stream_key_offer(sid, TrackKind::Audio, peer_kp.public_key(), wrapped)
            .await;
        ctrl.on_peer_track_enabled(sid, pk.clone(), TrackKind::Audio)
            .await;

        ctrl.on_peer_stream_state(sid, true, false).await;

        let state = ctrl.state().await;
        let peer = state
            .peers
            .iter()
            .find(|p| p.pubkey == pk)
            .expect("peer present");
        assert!(peer.muted);
        assert!(!peer.deafened);
        assert!(emitter.count("voice://state-changed") >= 1);
    }

    #[tokio::test]
    async fn on_peer_stream_joined_seeds_late_registered_peer() {
        let (ctrl, fake, _emitter) = setup_joined_call().await;
        let sid: SessionId = [9u8; 16];
        let pk = PublicKey::from_bytes([4u8; 32]);

        // StreamJoined arrives BEFORE the peer's TrackEnabled — its mute/deafen
        // must be remembered and applied when the VoicePeer is later created.
        ctrl.on_peer_stream_joined(sid, true, true).await;

        let peer_kp = Keypair::generate();
        let our_kp = fake.my_keypair();
        let key = farder_crypto::media::derive_stream_key();
        let wrapped = farder_crypto::media::wrap_stream_key_for_peer(
            &key,
            peer_kp.signing_key_bytes(),
            our_kp.public_key().as_bytes(),
        )
        .unwrap();
        ctrl.on_stream_key_offer(sid, TrackKind::Audio, peer_kp.public_key(), wrapped)
            .await;
        ctrl.on_peer_track_enabled(sid, pk.clone(), TrackKind::Audio)
            .await;

        let state = ctrl.state().await;
        let peer = state
            .peers
            .iter()
            .find(|p| p.pubkey == pk)
            .expect("peer present");
        assert!(peer.muted, "seeded mute must carry into the new VoicePeer");
        assert!(peer.deafened, "seeded deafen must carry into the new VoicePeer");
    }

    // When a viewer joins mid-share, on_peer_stream_joined must (a) re-offer the
    // Video stream key so the late joiner can decrypt (the new member is now in
    // get_media_state), and (b) set the force_keyframe flag so the encoder emits
    // a fresh IDR the new viewer can start decoding from. Uses the mock display
    // backend so it runs headless in WSL (same as the Task 3 share test).
    #[tokio::test]
    async fn on_peer_stream_joined_reoffers_video_key_and_forces_keyframe_mid_share() {
        std::env::set_var("FARDER_DISPLAY_BACKEND", "mock");
        let (ctrl, _emitter) = make_controller();

        // One remote member already in the channel so start_screen_share wraps+
        // offers the video key once.
        let peer = VoiceMember {
            public_key: Keypair::generate().public_key(),
            display_name: "peer".into(),
            joined_at: 0,
        };
        let server = FakeServerSession::new_with_members(vec![peer]);
        ctrl.join(7, server.clone()).await.unwrap();
        ctrl.start_screen_share(15, 320, 240, None, None).await.unwrap();

        // Exactly one Video offer so far (from start_screen_share).
        assert_eq!(
            server
                .offered_kinds
                .lock()
                .unwrap()
                .iter()
                .filter(|k| **k == TrackKind::Video)
                .count(),
            1,
            "precondition: one Video offer after start_screen_share"
        );

        // A new viewer joins the channel mid-share.
        let new_sid: SessionId = [42u8; 16];
        ctrl.on_peer_stream_joined(new_sid, false, false).await;

        // (a) A SECOND Video offer must have been recorded (the late-joiner
        // re-offer), so the new member can decrypt the stream.
        assert_eq!(
            server
                .offered_kinds
                .lock()
                .unwrap()
                .iter()
                .filter(|k| **k == TrackKind::Video)
                .count(),
            2,
            "joining mid-share must re-offer the Video key; offered={:?}",
            server.offered_kinds.lock().unwrap()
        );

        // (a') The Video track must also be re-enabled so the server
        // re-broadcasts TrackEnabled(Video) to the new joiner (who then builds
        // its recv route). One enable from start_screen_share + one from the
        // join re-enable == 2.
        assert_eq!(
            server
                .enabled_kinds
                .lock()
                .unwrap()
                .iter()
                .filter(|k| **k == TrackKind::Video)
                .count(),
            2,
            "joining mid-share must re-enable the Video track; enabled={:?}",
            server.enabled_kinds.lock().unwrap()
        );

        // The seed map still got the new peer's mute/deafen (unchanged behavior).
        // (b) force_keyframe must be set so the encoder emits a fresh IDR.
        {
            let inner = ctrl.inner.lock().await;
            let call = inner.active.as_ref().expect("still in call");
            assert_eq!(
                call.peer_status.get(&new_sid),
                Some(&(false, false)),
                "peer_status seeding must still happen for the new peer"
            );
            let share = call.video_share.as_ref().expect("sharing");
            assert!(
                share.force_keyframe.load(Ordering::Relaxed),
                "force_keyframe must be set when a viewer joins mid-share"
            );
        }

        ctrl.leave().await.unwrap();
    }

    #[tokio::test]
    async fn peer_leave_clears_mute_deafen_seed() {
        let (ctrl, fake, _emitter) = setup_joined_call().await;
        let sid: SessionId = [9u8; 16];
        let pk = PublicKey::from_bytes([5u8; 32]);

        // Seed mute+deafen, then the peer leaves before ever registering. The
        // seed must be cleared so a later re-registration on the same session_id
        // does not inherit the stale state.
        ctrl.on_peer_stream_joined(sid, true, true).await;
        ctrl.on_peer_stream_left(sid).await;

        let peer_kp = Keypair::generate();
        let our_kp = fake.my_keypair();
        let key = farder_crypto::media::derive_stream_key();
        let wrapped = farder_crypto::media::wrap_stream_key_for_peer(
            &key,
            peer_kp.signing_key_bytes(),
            our_kp.public_key().as_bytes(),
        )
        .unwrap();
        ctrl.on_stream_key_offer(sid, TrackKind::Audio, peer_kp.public_key(), wrapped)
            .await;
        ctrl.on_peer_track_enabled(sid, pk.clone(), TrackKind::Audio)
            .await;

        let state = ctrl.state().await;
        let peer = state
            .peers
            .iter()
            .find(|p| p.pubkey == pk)
            .expect("peer present");
        assert!(!peer.muted, "stale mute seed must not survive a leave");
        assert!(!peer.deafened, "stale deafen seed must not survive a leave");
    }

    #[tokio::test]
    async fn peer_stream_left_tears_down_screen_audio_ring() {
        // A peer sharing screen audio leaves the call (StreamLeft only — no
        // per-track TrackDisabled). on_peer_stream_left must tear down the
        // ScreenAudio recv task + ring, not just Audio. Otherwise the orphaned
        // screen-audio ring keeps being summed into the mix forever.
        let (ctrl, fake, _emitter) = setup_joined_call().await;
        let sid: SessionId = [11u8; 16];
        let pk = PublicKey::from_bytes([7u8; 32]);

        // Seed a ScreenAudio track the same way audio is seeded: offer the
        // ScreenAudio key for this session, then TrackEnabled(ScreenAudio).
        let peer_kp = Keypair::generate();
        let our_kp = fake.my_keypair();
        let key = farder_crypto::media::derive_stream_key();
        let wrapped = farder_crypto::media::wrap_stream_key_for_peer(
            &key,
            peer_kp.signing_key_bytes(),
            our_kp.public_key().as_bytes(),
        )
        .unwrap();
        ctrl.on_stream_key_offer(sid, TrackKind::ScreenAudio, peer_kp.public_key(), wrapped)
            .await;
        ctrl.on_peer_track_enabled(sid, pk.clone(), TrackKind::ScreenAudio)
            .await;

        // Precondition: the screen-audio ring + peer entry are present.
        {
            let inner = ctrl.inner.lock().await;
            let call = inner.active.as_ref().expect("still in call");
            assert_eq!(
                call.screen_audio_rings
                    .lock()
                    .expect("screen_audio_rings poisoned")
                    .len(),
                1,
                "ScreenAudio ring must be seeded before the leave"
            );
            assert_eq!(
                call.screen_audio_peers.len(),
                1,
                "ScreenAudio peer entry must be seeded before the leave"
            );
        }

        // The peer leaves the call: StreamLeft only.
        ctrl.on_peer_stream_left(sid).await;

        // The screen-audio ring AND the peer entry must both be gone.
        {
            let inner = ctrl.inner.lock().await;
            let call = inner.active.as_ref().expect("still in call");
            assert!(
                call.screen_audio_rings
                    .lock()
                    .expect("screen_audio_rings poisoned")
                    .is_empty(),
                "on_peer_stream_left must drop the ScreenAudio ring (mix leak)"
            );
            assert!(
                call.screen_audio_peers.is_empty(),
                "on_peer_stream_left must drop the ScreenAudio peer entry (recv-task leak)"
            );
        }

        ctrl.leave().await.unwrap();
    }

    async fn live_gain_for(ctrl: &VoiceController, sid: &SessionId) -> Option<f32> {
        let inner = ctrl.inner.lock().await;
        inner.active.as_ref().and_then(|c| {
            c.peer_rings
                .lock()
                .ok()
                .and_then(|r| r.get(sid).map(|(_, g)| f32::from_bits(g.load(Ordering::Acquire))))
        })
    }

    #[tokio::test]
    async fn peer_join_seeds_gain_from_saved_volume_and_set_peer_volume_clamps() {
        let (ctrl, _emitter) = make_controller();
        let server = FakeServerSession::new();
        let pk = PublicKey::from_bytes([6u8; 32]);
        let hex = pk.to_string(); // controller keys peer_volumes by this exact string
        let mut vols = std::collections::HashMap::new();
        vols.insert(hex.clone(), 0.5f32);
        ctrl.join_with_config(
            3,
            server.clone(),
            super::JoinConfig {
                mode: super::VoiceMode::OpenMic,
                peer_volumes: vols,
                connection: None,
                input_device: None,
                output_device: None,
            },
        )
        .await
        .unwrap();

        let sid: SessionId = [9u8; 16];
        // Register the peer (offer key then TrackEnabled), reusing the existing
        // round-trip helper pattern.
        let peer_kp = Keypair::generate();
        let our_kp = server.my_keypair();
        let key = farder_crypto::media::derive_stream_key();
        let wrapped = farder_crypto::media::wrap_stream_key_for_peer(
            &key,
            peer_kp.signing_key_bytes(),
            our_kp.public_key().as_bytes(),
        )
        .unwrap();
        ctrl.on_stream_key_offer(sid, TrackKind::Audio, peer_kp.public_key(), wrapped).await;
        ctrl.on_peer_track_enabled(sid, pk.clone(), TrackKind::Audio).await;

        assert_eq!(live_gain_for(&ctrl, &sid).await, Some(0.5), "seeded from saved volume");

        // Over-range clamps to 2.0; live gain updates for the present peer.
        ctrl.set_peer_volume(hex.clone(), 5.0).await.unwrap();
        assert_eq!(live_gain_for(&ctrl, &sid).await, Some(2.0), "clamped to 2.0");

        // Negative clamps to 0.0.
        ctrl.set_peer_volume(hex.clone(), -1.0).await.unwrap();
        assert_eq!(live_gain_for(&ctrl, &sid).await, Some(0.0), "clamped to 0.0");

        ctrl.leave().await.unwrap();
    }

    // Full controller path for screen share: join (1 peer in the channel),
    // start sharing, assert the Video stream-key offer + Video TrackEnabled
    // were recorded, then stop and assert Video TrackDisabled. Uses the mock
    // display backend (headless) and the bundled openh264 software encoder so
    // this runs without a GPU/display in WSL.
    #[tokio::test]
    async fn start_then_stop_screen_share_offers_enables_and_disables_video() {
        std::env::set_var("FARDER_DISPLAY_BACKEND", "mock");
        let (ctrl, _emitter) = make_controller();

        // One remote member in the channel so the video key gets wrapped+offered.
        let peer = VoiceMember {
            public_key: Keypair::generate().public_key(),
            display_name: "peer".into(),
            joined_at: 0,
        };
        let server = FakeServerSession::new_with_members(vec![peer]);
        ctrl.join(7, server.clone()).await.unwrap();

        ctrl.start_screen_share(15, 320, 240, None, None).await.unwrap();

        assert!(
            server.offered_kinds.lock().unwrap().contains(&TrackKind::Video),
            "start_screen_share must offer a Video stream key; offered={:?}",
            server.offered_kinds.lock().unwrap()
        );
        assert!(
            server.enabled_kinds.lock().unwrap().contains(&TrackKind::Video),
            "start_screen_share must enable the Video track; enabled={:?}",
            server.enabled_kinds.lock().unwrap()
        );

        // Starting again while sharing must be rejected.
        assert!(
            ctrl.start_screen_share(15, 320, 240, None, None).await.is_err(),
            "second start_screen_share must error while already sharing"
        );

        ctrl.stop_screen_share().await.unwrap();
        assert!(
            server.disabled_kinds.lock().unwrap().contains(&TrackKind::Video),
            "stop_screen_share must disable the Video track; disabled={:?}",
            server.disabled_kinds.lock().unwrap()
        );

        ctrl.leave().await.unwrap();
    }

    // Task 5: the SAME share must also start screen/game audio under its OWN
    // stream key. With FARDER_DISPLAY_BACKEND=mock (headless display) and the
    // non-Windows mock screen-audio capture, start_screen_share must offer a
    // ScreenAudio stream key + enable the ScreenAudio track, and stop_screen_share
    // must disable it. Mirrors the Video share test exactly, asserting ScreenAudio.
    #[tokio::test]
    async fn start_then_stop_screen_share_offers_enables_and_disables_screen_audio() {
        std::env::set_var("FARDER_DISPLAY_BACKEND", "mock");
        let (ctrl, _emitter) = make_controller();

        // One remote member in the channel so the screen-audio key gets wrapped+offered.
        let peer = VoiceMember {
            public_key: Keypair::generate().public_key(),
            display_name: "peer".into(),
            joined_at: 0,
        };
        let server = FakeServerSession::new_with_members(vec![peer]);
        ctrl.join(7, server.clone()).await.unwrap();

        ctrl.start_screen_share(15, 320, 240, None, None).await.unwrap();

        assert!(
            server.offered_kinds.lock().unwrap().contains(&TrackKind::ScreenAudio),
            "start_screen_share must offer a ScreenAudio stream key; offered={:?}",
            server.offered_kinds.lock().unwrap()
        );
        assert!(
            server.enabled_kinds.lock().unwrap().contains(&TrackKind::ScreenAudio),
            "start_screen_share must enable the ScreenAudio track; enabled={:?}",
            server.enabled_kinds.lock().unwrap()
        );

        ctrl.stop_screen_share().await.unwrap();
        assert!(
            server.disabled_kinds.lock().unwrap().contains(&TrackKind::ScreenAudio),
            "stop_screen_share must disable the ScreenAudio track; disabled={:?}",
            server.disabled_kinds.lock().unwrap()
        );

        ctrl.leave().await.unwrap();
    }

    // Regression for C1: leave() must tear down an active screen share, not
    // leak it. The MockDisplayBackend exposes no public stop-capture counter,
    // so the strongest available observable is behavioral: after start +
    // leave() while sharing, the share state must be gone — proven by (a)
    // stop_screen_share() being a clean no-op (nothing left to stop) and (b) a
    // fresh start_screen_share after rejoining succeeding rather than hitting
    // stale "already sharing" state. A leaked share (video_share not taken on
    // leave) would carry over and a fresh start would have to overwrite it.
    #[tokio::test]
    async fn leave_while_sharing_tears_down_share_no_stale_state() {
        std::env::set_var("FARDER_DISPLAY_BACKEND", "mock");
        let (ctrl, _emitter) = make_controller();

        let peer = VoiceMember {
            public_key: Keypair::generate().public_key(),
            display_name: "peer".into(),
            joined_at: 0,
        };
        let server = FakeServerSession::new_with_members(vec![peer]);
        ctrl.join(7, server.clone()).await.unwrap();
        ctrl.start_screen_share(15, 320, 240, None, None).await.unwrap();

        // Leave WITHOUT first calling stop_screen_share: leave() must shut down
        // the active share itself.
        ctrl.leave().await.unwrap();
        assert!(ctrl.state().await.channel_id.is_none(), "left the call");

        // No active call → stop_screen_share is a clean no-op (the share was
        // already torn down by leave(), not still dangling).
        ctrl.stop_screen_share().await.unwrap();

        // Rejoin and start again: must succeed (no stale "already sharing"
        // VideoShareState carried over from the previous call).
        ctrl.join(7, server.clone()).await.unwrap();
        ctrl.start_screen_share(15, 320, 240, None, None)
            .await
            .expect("fresh start after leave must succeed; stale share would block it");

        // And starting a SECOND time in this fresh call is still correctly
        // rejected (proving the new share installed cleanly, not the old one).
        assert!(
            ctrl.start_screen_share(15, 320, 240, None, None).await.is_err(),
            "second start in the fresh call must still error while sharing"
        );

        ctrl.stop_screen_share().await.unwrap();
        ctrl.leave().await.unwrap();
    }
}
