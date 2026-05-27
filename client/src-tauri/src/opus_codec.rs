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
#[derive(Debug)]
pub struct OpusEncoder {
    inner: Encoder,
    channels: u16,
    /// Reusable encode-output buffer sized to Opus's documented max.
    /// Saves a per-call allocation in the hot path.
    out_buf: Vec<u8>,
}

impl OpusEncoder {
    pub fn new(
        sample_rate: u32,
        channels: u16,
        bitrate_bps: i32,
    ) -> Result<Self, String> {
        if sample_rate != OPUS_SAMPLE_RATE {
            return Err(format!(
                "only {} Hz supported; got {sample_rate}",
                OPUS_SAMPLE_RATE,
            ));
        }
        if channels != 1 && channels != 2 {
            return Err(format!(
                "only mono or stereo supported; got channels={channels}",
            ));
        }
        let sr = SampleRate::Hz48000;
        let ch = match channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            _ => unreachable!(),
        };
        let mut inner = Encoder::new(sr, ch, Application::Voip)
            .map_err(|e| format!("opus encoder init: {e}"))?;
        inner
            .set_bitrate(Bitrate::BitsPerSecond(bitrate_bps))
            .map_err(|e| format!("opus set_bitrate: {e}"))?;
        inner
            .enable_dtx()
            .map_err(|e| format!("opus enable_dtx: {e}"))?;
        inner
            .set_inband_fec(true)
            .map_err(|e| format!("opus set_inband_fec: {e}"))?;
        Ok(Self {
            inner,
            channels,
            out_buf: vec![0u8; 4000],
        })
    }

    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, String> {
        let expected = OPUS_FRAME_SAMPLES_MONO * self.channels as usize;
        if pcm.len() != expected {
            return Err(format!(
                "expected {expected} samples for {ch}-channel 20ms frame; got {got}",
                ch = self.channels,
                got = pcm.len(),
            ));
        }
        let n = self
            .inner
            .encode_float(pcm, &mut self.out_buf)
            .map_err(|e| format!("opus encode: {e}"))?;
        Ok(self.out_buf[..n].to_vec())
    }
}

/// Voice-optimized Opus decoder. One instance per incoming audio track.
#[derive(Debug)]
pub struct OpusDecoder {
    inner: Decoder,
    channels: u16,
    /// Reusable decode-output buffer sized to one frame at the configured channel count.
    out_buf: Vec<f32>,
}

impl OpusDecoder {
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self, String> {
        if sample_rate != OPUS_SAMPLE_RATE {
            return Err(format!(
                "only {} Hz supported; got {sample_rate}",
                OPUS_SAMPLE_RATE,
            ));
        }
        if channels != 1 && channels != 2 {
            return Err(format!(
                "only mono or stereo supported; got channels={channels}",
            ));
        }
        let sr = SampleRate::Hz48000;
        let ch = match channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            _ => unreachable!(),
        };
        let inner = Decoder::new(sr, ch).map_err(|e| format!("opus decoder init: {e}"))?;
        let buf_capacity = OPUS_FRAME_SAMPLES_MONO * channels as usize;
        Ok(Self {
            inner,
            channels,
            out_buf: vec![0.0f32; buf_capacity],
        })
    }

    pub fn decode(&mut self, _packet: &[u8]) -> Result<Vec<f32>, String> {
        Err("not yet implemented".into())
    }

    pub fn decode_plc(&mut self) -> Result<Vec<f32>, String> {
        Err("not yet implemented".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_constructs_with_sane_defaults() {
        let enc = OpusEncoder::new(OPUS_SAMPLE_RATE, 1, OPUS_DEFAULT_BITRATE_BPS);
        assert!(enc.is_ok(), "encoder construction should succeed; got {:?}", enc.err());
    }

    #[test]
    fn encoder_rejects_non_48khz_sample_rate() {
        let result = OpusEncoder::new(44_100, 1, OPUS_DEFAULT_BITRATE_BPS);
        assert!(result.is_err(), "44.1 kHz should be rejected");
        let msg = result.unwrap_err();
        assert!(msg.contains("48000"), "error should mention required rate; got: {msg}");
    }

    #[test]
    fn encoder_rejects_invalid_channels() {
        let r0 = OpusEncoder::new(OPUS_SAMPLE_RATE, 0, OPUS_DEFAULT_BITRATE_BPS);
        let r3 = OpusEncoder::new(OPUS_SAMPLE_RATE, 3, OPUS_DEFAULT_BITRATE_BPS);
        assert!(r0.is_err(), "0 channels should be rejected");
        assert!(r3.is_err(), "3 channels should be rejected");
    }

    #[test]
    fn encoder_rejects_wrong_frame_size() {
        let mut enc =
            OpusEncoder::new(OPUS_SAMPLE_RATE, 1, OPUS_DEFAULT_BITRATE_BPS).expect("encoder ok");

        let too_short = vec![0.0f32; OPUS_FRAME_SAMPLES_MONO - 1];
        let too_long = vec![0.0f32; OPUS_FRAME_SAMPLES_MONO + 1];

        assert!(enc.encode(&too_short).is_err(), "short frame must error");
        assert!(enc.encode(&too_long).is_err(), "long frame must error");

        let exact = vec![0.0f32; OPUS_FRAME_SAMPLES_MONO];
        let pkt = enc.encode(&exact).expect("mono 20ms frame must encode");
        assert!(!pkt.is_empty(), "non-empty packet expected");
    }

    #[test]
    fn decoder_constructs_with_sane_defaults() {
        let dec = OpusDecoder::new(OPUS_SAMPLE_RATE, 1);
        assert!(dec.is_ok(), "decoder construction should succeed; got {:?}", dec.err());

        let bad_rate = OpusDecoder::new(44_100, 1);
        let bad_ch = OpusDecoder::new(OPUS_SAMPLE_RATE, 3);
        assert!(bad_rate.is_err(), "44.1 kHz should be rejected");
        assert!(bad_ch.is_err(), "3 channels should be rejected");
    }
}
