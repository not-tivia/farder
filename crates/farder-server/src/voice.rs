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

pub const VOICE_FRAME_VERSION: u8 = 0x01;
pub const VOICE_FRAME_TYPE_AUDIO: u8 = 0x01;
pub const VOICE_FRAME_HEADER_LEN: usize = 1 + 1 + 8 + 32;

#[derive(Debug, PartialEq)]
pub struct VoiceFrame<'a> {
    pub seq: u64,
    pub speaker_pk: [u8; 32],
    pub opus_payload: &'a [u8],
}

pub fn parse_voice_frame(buf: &[u8]) -> Result<VoiceFrame<'_>, &'static str> {
    if buf.len() < VOICE_FRAME_HEADER_LEN {
        return Err("frame too short");
    }
    if buf[0] != VOICE_FRAME_VERSION { return Err("bad version"); }
    if buf[1] != VOICE_FRAME_TYPE_AUDIO { return Err("bad type"); }
    let seq = u64::from_be_bytes(buf[2..10].try_into().unwrap());
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&buf[10..42]);
    Ok(VoiceFrame { seq, speaker_pk: pk, opus_payload: &buf[42..] })
}

pub fn build_voice_frame(seq: u64, speaker_pk: &[u8; 32], opus: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(VOICE_FRAME_HEADER_LEN + opus.len());
    buf.push(VOICE_FRAME_VERSION);
    buf.push(VOICE_FRAME_TYPE_AUDIO);
    buf.extend_from_slice(&seq.to_be_bytes());
    buf.extend_from_slice(speaker_pk);
    buf.extend_from_slice(opus);
    buf
}

/// Rewrite the speaker_pk field of an existing frame in place.
/// Used by the server to ensure forwarded frames carry the validated pubkey
/// (defense-in-depth — the speaker_pk is already validated before forwarding).
pub fn rewrite_speaker_pk(buf: &mut [u8], pk: &[u8; 32]) -> Result<(), &'static str> {
    if buf.len() < VOICE_FRAME_HEADER_LEN { return Err("frame too short"); }
    buf[10..42].copy_from_slice(pk);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_frame() {
        let mut buf = vec![0x01, 0x01]; // version, type
        buf.extend_from_slice(&42u64.to_be_bytes()); // seq
        buf.extend_from_slice(&[0xab; 32]); // pk
        buf.extend_from_slice(&[0xcc; 80]); // opus payload
        let parsed = parse_voice_frame(&buf).unwrap();
        assert_eq!(parsed.seq, 42);
        assert_eq!(parsed.speaker_pk, [0xab; 32]);
        assert_eq!(parsed.opus_payload.len(), 80);
    }

    #[test]
    fn parse_rejects_short() {
        let buf = vec![0x01, 0x01, 0, 0, 0, 0]; // 6 bytes — too short for header
        assert!(parse_voice_frame(&buf).is_err());
    }

    #[test]
    fn parse_rejects_wrong_version() {
        let mut buf = vec![0x02, 0x01]; // wrong version
        buf.extend_from_slice(&[0u8; 40]);
        assert!(parse_voice_frame(&buf).is_err());
    }

    #[test]
    fn parse_rejects_wrong_type() {
        let mut buf = vec![0x01, 0x02]; // wrong type
        buf.extend_from_slice(&[0u8; 40]);
        assert!(parse_voice_frame(&buf).is_err());
    }

    #[test]
    fn build_round_trips() {
        let pk = [0xab; 32];
        let opus = vec![0xcc; 100];
        let buf = build_voice_frame(123, &pk, &opus);
        let parsed = parse_voice_frame(&buf).unwrap();
        assert_eq!(parsed.seq, 123);
        assert_eq!(parsed.speaker_pk, pk);
        assert_eq!(parsed.opus_payload, opus.as_slice());
    }
}
