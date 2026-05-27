// client/src-tauri/src/opus_codec.rs
//
// Opus encode/decode wrappers (voice-optimized: 20ms / 48 kHz / mono /
// Voip mode / DTX + FEC). See
// docs/superpowers/specs/2026-05-26-opus-codec-design.md.

use audiopus::{
    coder::{Decoder, Encoder},
    Application, Bitrate, Channels, SampleRate,
};

pub const OPUS_SAMPLE_RATE: u32 = 48_000;
/// Samples per 20 ms frame at 48 kHz, MONO. For stereo, the buffer
/// length is `OPUS_FRAME_SAMPLES_MONO * 2` (interleaved).
pub const OPUS_FRAME_SAMPLES_MONO: usize = 960;
pub const OPUS_FRAME_DURATION_MS: u32 = 20;
pub const OPUS_DEFAULT_BITRATE_BPS: i32 = 24_000;

/// Voice-optimized Opus encoder. One instance per outgoing audio track.
pub struct OpusEncoder {
    inner: Encoder,
    channels: u16,
    /// Reusable encode-output buffer sized to Opus's documented max.
    /// Saves a per-call allocation in the hot path.
    out_buf: Vec<u8>,
}

impl OpusEncoder {
    pub fn new(
        _sample_rate: u32,
        _channels: u16,
        _bitrate_bps: i32,
    ) -> Result<Self, String> {
        Err("not yet implemented".into())
    }

    pub fn encode(&mut self, _pcm: &[f32]) -> Result<Vec<u8>, String> {
        Err("not yet implemented".into())
    }
}

/// Voice-optimized Opus decoder. One instance per incoming audio track.
pub struct OpusDecoder {
    inner: Decoder,
    channels: u16,
    /// Reusable decode-output buffer sized to one frame at the configured channel count.
    out_buf: Vec<f32>,
}

impl OpusDecoder {
    pub fn new(_sample_rate: u32, _channels: u16) -> Result<Self, String> {
        Err("not yet implemented".into())
    }

    pub fn decode(&mut self, _packet: &[u8]) -> Result<Vec<f32>, String> {
        Err("not yet implemented".into())
    }

    pub fn decode_plc(&mut self) -> Result<Vec<f32>, String> {
        Err("not yet implemented".into())
    }
}
