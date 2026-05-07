use std::collections::{HashMap, HashSet};
use tokio::sync::RwLock;

/// Ephemeral voice state. Not persisted to DB.
pub struct VoiceState {
    pub channels: RwLock<HashMap<u64, HashSet<[u8; 32]>>>,
    pub deafened: RwLock<HashSet<[u8; 32]>>,
    pub muted: RwLock<HashSet<[u8; 32]>>,
    pub speaking_last_frame_ms: RwLock<HashMap<[u8; 32], u64>>,
    pub speaking_now: RwLock<HashSet<[u8; 32]>>,
    pub speaker_channel: RwLock<HashMap<[u8; 32], u64>>,
}

impl VoiceState {
    pub fn new() -> Self {
        Self {
            channels: RwLock::new(HashMap::new()),
            deafened: RwLock::new(HashSet::new()),
            muted: RwLock::new(HashSet::new()),
            speaking_last_frame_ms: RwLock::new(HashMap::new()),
            speaking_now: RwLock::new(HashSet::new()),
            speaker_channel: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for VoiceState {
    fn default() -> Self { Self::new() }
}

pub async fn start_transmit(state: &VoiceState, pk: [u8; 32], channel_id: u64) {
    let mut speaker_channel = state.speaker_channel.write().await;
    let mut channels = state.channels.write().await;
    if let Some(prev) = speaker_channel.get(&pk).copied() {
        if let Some(prev_set) = channels.get_mut(&prev) {
            prev_set.remove(&pk);
            if prev_set.is_empty() { channels.remove(&prev); }
        }
    }
    channels.entry(channel_id).or_insert_with(HashSet::new).insert(pk);
    speaker_channel.insert(pk, channel_id);
}

pub async fn stop_transmit(state: &VoiceState, pk: [u8; 32]) {
    let mut speaker_channel = state.speaker_channel.write().await;
    if let Some(channel_id) = speaker_channel.remove(&pk) {
        let mut channels = state.channels.write().await;
        if let Some(set) = channels.get_mut(&channel_id) {
            set.remove(&pk);
            if set.is_empty() { channels.remove(&channel_id); }
        }
    }
    state.muted.write().await.remove(&pk);
    state.speaking_last_frame_ms.write().await.remove(&pk);
    state.speaking_now.write().await.remove(&pk);
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
