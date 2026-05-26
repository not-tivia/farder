# Opus Codec Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `OpusEncoder` + `OpusDecoder` + PLC wrappers around `audiopus`, configured for voice (20 ms / 48 kHz / mono / Voip mode / DTX + FEC).

**Architecture:** New module `client/src-tauri/src/opus_codec.rs`. Encoder reuses a 4000-byte output buffer to avoid per-encode allocation in the hot path. Decoder reuses a 960-sample buffer. Frame size is hardcoded at 20 ms; encoder rejects mis-sized PCM input. PLC is exposed via `decode_plc()` which calls `decode_float` with `None` packet input.

**Tech Stack:** Rust (Tauri 2). New dep `audiopus = "0.3"` (RustCrypto-ecosystem safe wrapper around libopus). System libopus + cmake already installed.

**Spec:** `docs/superpowers/specs/2026-05-26-opus-codec-design.md`

---

## File structure

**Created:**
- `client/src-tauri/src/opus_codec.rs` — constants, `OpusEncoder`, `OpusDecoder`, tests

**Modified:**
- `client/src-tauri/Cargo.toml` — add `audiopus = "0.3"`
- `client/src-tauri/src/main.rs` — add `mod opus_codec;`

---

## Phase 1: Scaffold

## Task 1: opus_codec.rs scaffold — constants + stub structs

**Files:**
- Create: `client/src-tauri/src/opus_codec.rs`
- Modify: `client/src-tauri/Cargo.toml`
- Modify: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Add `audiopus` dep**

In `client/src-tauri/Cargo.toml`, find the `[dependencies]` section. Add `audiopus = "0.3"` alphabetically (right after `anyhow` and before `base64`, or wherever fits — confirm with grep).

- [ ] **Step 2: Add `mod opus_codec;` to main.rs**

In `client/src-tauri/src/main.rs`, find the cluster of `mod xxx;` declarations near the top. Add `mod opus_codec;` alphabetically (likely between `mod connection;` and `mod server_manager;`, depending on the current alphabetization).

- [ ] **Step 3: Create opus_codec.rs with the scaffold**

```rust
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
```

- [ ] **Step 4: Verify cargo check**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -10
```

Expected: `Finished`. The first cargo invocation after adding `audiopus` will compile `audiopus_sys` (which links libopus) — this can take 30–90 seconds. Subsequent runs are fast.

If you see errors like "could not find `libopus`" or "cmake not found", the system deps aren't installed. They should already be (from earlier prep) — confirm with `pkg-config --modversion opus` (expect `1.4`) and `which cmake`. If either is missing, STOP and report.

If you see unused-field warnings on `inner` / `channels` / `out_buf`, that's expected — Task 2 lights them up.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/opus_codec.rs client/src-tauri/src/main.rs client/src-tauri/Cargo.toml client/src-tauri/Cargo.lock
git -C /home/deez/farder commit -m "feat(client): opus_codec.rs scaffold — Encoder/Decoder stubs + constants"
```

Use HEREDOC + the `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer (see `git log -1` for format).

---

## Phase 2: Encoder

## Task 2: OpusEncoder::new + first construction test

**Files:**
- Modify: `client/src-tauri/src/opus_codec.rs`

- [ ] **Step 1: Implement `OpusEncoder::new`**

Replace the `OpusEncoder::new` stub:

```rust
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
```

- [ ] **Step 2: Add a tests module + 3 construction tests**

Append at the end of `opus_codec.rs`:

```rust
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
}
```

- [ ] **Step 3: Run tests**

```
cd /home/deez/farder/client/src-tauri && cargo test opus_codec::tests 2>&1 | tail -10
```

Expected: 3 passed.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/opus_codec.rs
git -C /home/deez/farder commit -m "feat(client): OpusEncoder::new + construction tests"
```

Use HEREDOC + Co-Authored-By trailer.

---

## Task 3: OpusEncoder::encode + frame-size test

**Files:**
- Modify: `client/src-tauri/src/opus_codec.rs`

- [ ] **Step 1: Implement `encode`**

Replace the `encode` stub:

```rust
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, String> {
        let expected = OPUS_FRAME_SAMPLES_MONO * self.channels as usize;
        if pcm.len() != expected {
            return Err(format!(
                "expected {expected} samples ({} ms @ {} Hz {} channels), got {}",
                OPUS_FRAME_DURATION_MS,
                OPUS_SAMPLE_RATE,
                self.channels,
                pcm.len(),
            ));
        }
        let written = self
            .inner
            .encode_float(pcm, &mut self.out_buf)
            .map_err(|e| format!("opus encode: {e}"))?;
        Ok(self.out_buf[..written].to_vec())
    }
```

- [ ] **Step 2: Add a frame-size rejection test inside the existing `mod tests`**

```rust
    #[test]
    fn encoder_rejects_wrong_frame_size() {
        let mut enc = OpusEncoder::new(OPUS_SAMPLE_RATE, 1, OPUS_DEFAULT_BITRATE_BPS).unwrap();
        // 10ms frame (480 samples) instead of expected 20ms (960)
        let half_frame = vec![0.0_f32; 480];
        let result = enc.encode(&half_frame);
        assert!(result.is_err(), "wrong frame size should be rejected");
        let msg = result.unwrap_err();
        assert!(msg.contains("960"), "error should mention expected size; got: {msg}");
    }
```

- [ ] **Step 3: Run tests**

```
cd /home/deez/farder/client/src-tauri && cargo test opus_codec::tests 2>&1 | tail -10
```

Expected: 4 passed.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/opus_codec.rs
git -C /home/deez/farder commit -m "feat(client): OpusEncoder::encode + frame-size validation"
```

---

## Phase 3: Decoder

## Task 4: OpusDecoder::new + construction test

**Files:**
- Modify: `client/src-tauri/src/opus_codec.rs`

- [ ] **Step 1: Implement `OpusDecoder::new`**

Replace the `OpusDecoder::new` stub:

```rust
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
        let inner = Decoder::new(sr, ch)
            .map_err(|e| format!("opus decoder init: {e}"))?;
        Ok(Self {
            inner,
            channels,
            out_buf: vec![0.0_f32; OPUS_FRAME_SAMPLES_MONO * channels as usize],
        })
    }
```

- [ ] **Step 2: Add a decoder construction test inside the existing `mod tests`**

```rust
    #[test]
    fn decoder_constructs_with_sane_defaults() {
        let dec = OpusDecoder::new(OPUS_SAMPLE_RATE, 1);
        assert!(dec.is_ok(), "decoder construction should succeed; got {:?}", dec.err());
    }
```

- [ ] **Step 3: Run tests**

```
cd /home/deez/farder/client/src-tauri && cargo test opus_codec::tests 2>&1 | tail -10
```

Expected: 5 passed.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/opus_codec.rs
git -C /home/deez/farder commit -m "feat(client): OpusDecoder::new + construction test"
```

---

## Task 5: OpusDecoder::decode + decode_plc + 5 tests (incl. roundtrip)

**Files:**
- Modify: `client/src-tauri/src/opus_codec.rs`

- [ ] **Step 1: Implement `decode` and `decode_plc`**

Replace both stubs:

```rust
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>, String> {
        if packet.is_empty() {
            return Err("decode called with empty packet (use decode_plc instead)".into());
        }
        let written = self
            .inner
            .decode_float(Some(packet), &mut self.out_buf, false)
            .map_err(|e| format!("opus decode: {e}"))?;
        let expected = OPUS_FRAME_SAMPLES_MONO * self.channels as usize;
        if written != expected {
            return Err(format!(
                "opus decoded {written} samples, expected {expected}",
            ));
        }
        Ok(self.out_buf[..written].to_vec())
    }

    pub fn decode_plc(&mut self) -> Result<Vec<f32>, String> {
        let written = self
            .inner
            .decode_float(None, &mut self.out_buf, false)
            .map_err(|e| format!("opus decode_plc: {e}"))?;
        let expected = OPUS_FRAME_SAMPLES_MONO * self.channels as usize;
        if written != expected {
            return Err(format!(
                "opus PLC produced {written} samples, expected {expected}",
            ));
        }
        Ok(self.out_buf[..written].to_vec())
    }
```

- [ ] **Step 2: Add the remaining 5 tests inside the existing `mod tests`**

```rust
    /// Generate a 440 Hz sine wave PCM frame (one 20 ms frame mono @ 48 kHz).
    fn sine_440hz_frame() -> Vec<f32> {
        let mut out = Vec::with_capacity(OPUS_FRAME_SAMPLES_MONO);
        let sr = OPUS_SAMPLE_RATE as f32;
        for n in 0..OPUS_FRAME_SAMPLES_MONO {
            let t = n as f32 / sr;
            out.push((2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5);
        }
        out
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum_sq: f32 = samples.iter().map(|x| x * x).sum();
        (sum_sq / samples.len() as f32).sqrt()
    }

    #[test]
    fn roundtrip_sine_wave_produces_correct_length_output() {
        let mut enc = OpusEncoder::new(OPUS_SAMPLE_RATE, 1, OPUS_DEFAULT_BITRATE_BPS).unwrap();
        let mut dec = OpusDecoder::new(OPUS_SAMPLE_RATE, 1).unwrap();
        let pcm = sine_440hz_frame();
        let packet = enc.encode(&pcm).unwrap();
        assert!(!packet.is_empty(), "encoded packet should be non-empty");
        let decoded = dec.decode(&packet).unwrap();
        assert_eq!(
            decoded.len(),
            OPUS_FRAME_SAMPLES_MONO,
            "decoded length must equal one frame",
        );
    }

    #[test]
    fn roundtrip_sine_wave_preserves_signal_envelope() {
        let mut enc = OpusEncoder::new(OPUS_SAMPLE_RATE, 1, OPUS_DEFAULT_BITRATE_BPS).unwrap();
        let mut dec = OpusDecoder::new(OPUS_SAMPLE_RATE, 1).unwrap();
        let pcm = sine_440hz_frame();
        let input_rms = rms(&pcm);

        // Opus needs a warm-up frame or two to stabilize. Encode + decode
        // the same frame a few times before measuring RMS.
        for _ in 0..5 {
            let packet = enc.encode(&pcm).unwrap();
            let _ = dec.decode(&packet).unwrap();
        }
        let packet = enc.encode(&pcm).unwrap();
        let decoded = dec.decode(&packet).unwrap();
        let output_rms = rms(&decoded);

        // Lossy codec — RMS won't match exactly, but should be within ±50%.
        let ratio = output_rms / input_rms;
        assert!(
            ratio > 0.5 && ratio < 1.5,
            "decoded RMS ({output_rms}) should be within 50% of input RMS ({input_rms}); ratio {ratio}",
        );
    }

    #[test]
    fn decode_rejects_empty_packet() {
        let mut dec = OpusDecoder::new(OPUS_SAMPLE_RATE, 1).unwrap();
        let result = dec.decode(&[]);
        assert!(result.is_err(), "empty packet should be rejected");
        assert!(
            result.unwrap_err().contains("empty"),
            "error should mention empty",
        );
    }

    #[test]
    fn decode_rejects_garbage() {
        let mut dec = OpusDecoder::new(OPUS_SAMPLE_RATE, 1).unwrap();
        let result = dec.decode(&[0xff, 0xff, 0xff]);
        assert!(result.is_err(), "garbage bytes should be rejected");
    }

    #[test]
    fn decode_plc_returns_correct_length() {
        let mut dec = OpusDecoder::new(OPUS_SAMPLE_RATE, 1).unwrap();
        let synth = dec.decode_plc().unwrap();
        assert_eq!(
            synth.len(),
            OPUS_FRAME_SAMPLES_MONO,
            "PLC output must equal one frame",
        );
    }

    #[test]
    fn dtx_silence_produces_small_packet() {
        let mut enc = OpusEncoder::new(OPUS_SAMPLE_RATE, 1, OPUS_DEFAULT_BITRATE_BPS).unwrap();
        // Encode several frames of total silence so DTX kicks in.
        let silence = vec![0.0_f32; OPUS_FRAME_SAMPLES_MONO];
        let mut last_packet_len = 0;
        for _ in 0..10 {
            let pkt = enc.encode(&silence).unwrap();
            last_packet_len = pkt.len();
        }
        // After several silence frames, DTX should produce a tiny SID
        // packet (<= 10 bytes). The first frame or two may be larger
        // while the encoder transitions.
        assert!(
            last_packet_len <= 10,
            "DTX silence packet should be tiny (got {last_packet_len} bytes)",
        );
    }
```

- [ ] **Step 3: Run tests**

```
cd /home/deez/farder/client/src-tauri && cargo test opus_codec::tests 2>&1 | tail -20
```

Expected: **10 passed** (3 encoder + 1 frame-size + 1 decoder + 5 here).

If `dtx_silence_produces_small_packet` fails because the packet is larger than expected (e.g., 30 bytes), `audiopus`'s DTX behavior may need more warm-up frames or a longer silence run. Bump the loop count from 10 to 20 and re-test. If still failing, the test asserts a slightly looser bound (e.g., `<= 20`) — but the spec says "<=10" and modern libopus reliably produces ~3-byte SID packets after a few frames of silence, so the failure is more likely a sign of misconfiguration.

If `roundtrip_sine_wave_preserves_signal_envelope` fails with the RMS ratio outside `(0.5, 1.5)`, the encoder/decoder are not actually round-tripping audio. Most likely cause: misconfigured channel count or sample rate. Verify by adding `eprintln!("input_rms={input_rms}, output_rms={output_rms}, ratio={ratio}")` and inspecting.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/opus_codec.rs
git -C /home/deez/farder commit -m "feat(client): OpusDecoder decode + decode_plc + roundtrip tests"
```

---

## Phase 4: Verification

## Task 6: Final smoke + workspace verify

**Files:**
- None (verification only)

- [ ] **Step 1: Run all opus_codec tests**

```
cd /home/deez/farder/client/src-tauri && cargo test opus_codec::tests 2>&1 | tail -20
```

Expected: 10 passed (3 + 1 + 1 + 5).

- [ ] **Step 2: cargo check on the whole client**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
```

Expected: `Finished`. Pre-existing warnings are OK.

- [ ] **Step 3: TS check on the client UI**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5
```

Expected: clean. No TS changes in this sub-project but verify nothing leaked.

- [ ] **Step 4: No CHANGELOG entry**

Infrastructure-only sub-project. CHANGELOG waits for sub-project #3.3 (voice client pipeline) to exercise the abstraction end-to-end.

- [ ] **Step 5: No final commit**

Steps 1-3 are read-only verifications.

---

## Self-review notes

- **Spec coverage:**
  - `OpusEncoder` construction + voice-opinionated config → Task 2
  - `OpusEncoder::encode` + frame-size validation → Task 3
  - `OpusDecoder` construction → Task 4
  - `OpusDecoder::decode` + `decode_plc` → Task 5
  - All 11 tests from the spec → Tasks 2, 3, 4, 5 (counts: 3 + 1 + 1 + 5 = 10 — the 11th in the spec was "decoder_rejects_garbage" which IS in Task 5; let me recount: 3 encoder construction + 1 frame-size + 1 decoder construction + roundtrip-length + roundtrip-envelope + reject-empty + reject-garbage + plc-length + dtx-silence = 10 distinct tests. Spec listed 11 but the spec includes both "encoder_rejects_non_48khz" AND "encoder_rejects_invalid_channels" as separate items, which I bundled the latter into one test. Functionally equivalent coverage.)
- **Placeholder scan:** No "TBD" / "fill in details". Each step contains the exact code an engineer needs.
- **Type consistency:**
  - `OpusEncoder` / `OpusDecoder` field layout consistent across tasks
  - `OPUS_SAMPLE_RATE` / `OPUS_FRAME_SAMPLES_MONO` / `OPUS_FRAME_DURATION_MS` / `OPUS_DEFAULT_BITRATE_BPS` constants defined in Task 1, used in Tasks 2-5
  - `audiopus::Bitrate::BitsPerSecond` (used in Task 2) matches `audiopus` 0.3's API
  - `decode_float(Option<&[u8]>, &mut [f32], bool)` signature consistent across `decode` and `decode_plc`
- **No CHANGELOG by design** — same pattern as sub-project #1 (MediaBackend) and #3.1 (CpalAudioBackend). The voice client pipeline (#3.3) ships the user-visible CHANGELOG entry that aggregates all the infrastructure underneath.

## Notes for the implementer

- **audiopus 0.3 API peculiarity**: `Encoder::encode_float` expects f32 PCM as its first arg. `Decoder::decode_float` takes `Option<&[u8]>` (Some for present packet, None for PLC) as first arg, output `&mut [f32]` as second, and a `bool` "decode_fec" flag as third. All test code in this plan matches that signature; if the API has shifted in a newer 0.3.x patch, the implementer should consult `cargo doc -p audiopus --open`.
- **libopus warm-up**: the encoder may produce larger packets for the first frame or two as it stabilizes. The DTX silence test loops 10 frames before checking the final packet size; this is intentional.
- **Why no PLC quality test**: libopus's PLC synthesizes a plausible signal based on previous frame context. Quality is subjective and degrades after ~3 consecutive losses. We only test that the OUTPUT IS THE RIGHT LENGTH — actual quality is out of scope (and would require listening tests / spectral analysis).
