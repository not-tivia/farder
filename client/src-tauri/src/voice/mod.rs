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
