# Opus Codec Layer — Design

**Status:** Drafted 2026-05-26
**Scope:** Farder client (`client/src-tauri`). New module `opus_codec.rs` exposing `OpusEncoder` + `OpusDecoder` wrappers around the `audiopus` crate (which wraps libopus). Pure data transformation: f32 PCM ↔ Opus packet bytes. No protocol, server, or UI changes.
**Position in roadmap:** Sub-project #3.2 of the voice + screensharing track. Sits between sub-project #3.1's `AudioBackend` (raw PCM) and sub-project #3.3's voice client pipeline (which encrypts/transmits Opus packets via the #2 media-stream transport).

## Goal

Provide a minimal, voice-optimized Opus encode/decode API. `OpusEncoder` turns 20 ms PCM frames into Opus packets (~50–80 bytes each at the default 24 kbps bitrate); `OpusDecoder` turns them back. Packet-loss concealment (PLC) is included so a missing frame synthesizes plausible audio instead of silence — important for UDP-delivered voice where occasional drops are normal.

## Non-Goals

- **Encoder configuration beyond bitrate.** v1 hardcodes Application::Voip, DTX on, FEC on, complexity 10. These are sane voice defaults. Future v1.5 can expose them if needed.
- **Variable frame sizes.** v1 enforces 20 ms (960 samples mono @ 48 kHz). Opus supports 2.5/5/10/20/40/60 ms but voice almost always uses 20 ms — a frame-size choice in the encode pipeline, not a runtime knob.
- **Stereo encoding.** v1 voice is mono. The `channels` parameter is in the API for future use, but only `channels == 1` is tested.
- **Multi-stream / surround.** Opus supports multi-stream (5.1, etc.). Out of scope.
- **Streaming/buffering layer.** Caller is responsible for collecting PCM into 960-sample chunks before calling `encode`. Sub-project #3.3 owns that buffering.
- **Encoder hot-reconfiguration.** Bitrate is set at construction; changing it mid-stream means dropping + recreating the encoder. v1.
- **In-band FEC payload decoding.** When FEC is enabled and a packet is lost, Opus can recover the previous packet from redundancy embedded in the next packet. v1 exposes only basic PLC; using FEC properly requires a "current packet has redundancy for the previous" hint API that we defer.
- **Decoder PLC count tracking.** Real-world PLC sometimes needs to track how many consecutive frames were lost (decoder quality degrades after ~3 consecutive). v1 just calls `decode_loss` blindly; sub-project #3.3 can layer in a counter if needed.

## Architecture

```
┌─── client/src-tauri/src/opus_codec.rs ──────────────────────────────────┐
│                                                                          │
│  use audiopus::{coder::Encoder, coder::Decoder, Application,             │
│                 Channels, SampleRate};                                   │
│                                                                          │
│  pub const OPUS_SAMPLE_RATE: u32 = 48_000;                              │
│  pub const OPUS_FRAME_SAMPLES_MONO: usize = 960;  // 20ms @ 48kHz       │
│  pub const OPUS_FRAME_DURATION_MS: u32 = 20;                            │
│  pub const OPUS_DEFAULT_BITRATE_BPS: i32 = 24_000;                       │
│                                                                          │
│  pub struct OpusEncoder {                                                │
│      inner: audiopus::coder::Encoder,                                    │
│      channels: u16,                                                      │
│      // 4000 byte buffer for encode output — Opus max packet size       │
│      out_buf: Vec<u8>,                                                   │
│  }                                                                       │
│                                                                          │
│  impl OpusEncoder {                                                      │
│      pub fn new(sample_rate, channels, bitrate) -> Result<Self>          │
│      pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>>            │
│  }                                                                       │
│                                                                          │
│  pub struct OpusDecoder {                                                │
│      inner: audiopus::coder::Decoder,                                    │
│      channels: u16,                                                      │
│      out_buf: Vec<f32>, // OPUS_FRAME_SAMPLES * channels                 │
│  }                                                                       │
│                                                                          │
│  impl OpusDecoder {                                                      │
│      pub fn new(sample_rate, channels) -> Result<Self>                   │
│      pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>>         │
│      pub fn decode_plc(&mut self) -> Result<Vec<f32>>                    │
│  }                                                                       │
└──────────────────────────────────────────────────────────────────────────┘
```

### Why a thin wrapper

`audiopus` itself is already a safe Rust wrapper around libopus's C API — closures around `Box<[u8]>` buffers, typed `SampleRate` / `Channels` enums, etc. Our wrapper adds:

1. Pre-allocated output buffers (avoid per-encode allocation in the hot path).
2. Voice-opinionated defaults (Application::Voip, DTX, FEC, complexity).
3. Error messages tailored to Farder (String errors that bubble through to the user).
4. Frame-size validation (reject mis-sized PCM input with a clear error).

It does NOT add: codec selection, format negotiation, streaming. Those are caller concerns.

### Why hardcode 20 ms / 48 kHz

The combination is the default everywhere voice happens: Discord, Zoom, Slack, WebRTC. Compatible with virtually every cpal device (which sub-project #3.1 already configures to 48 kHz). Making it runtime-configurable adds complexity for zero current benefit. v1.5 can revisit if someone needs 10 ms (lower latency, higher overhead) or 40 ms (efficient batching for low-bandwidth links).

## OpusEncoder

### Construction

```rust
pub fn new(
    sample_rate: u32,
    channels: u16,
    bitrate_bps: i32,
) -> Result<Self, String> {
    if sample_rate != OPUS_SAMPLE_RATE {
        return Err(format!("only 48 kHz supported; got {sample_rate}"));
    }
    if channels != 1 && channels != 2 {
        return Err(format!("only mono or stereo supported; got channels={channels}"));
    }
    let sr = SampleRate::Hz48000;
    let ch = match channels { 1 => Channels::Mono, 2 => Channels::Stereo, _ => unreachable!() };
    let mut inner = audiopus::coder::Encoder::new(sr, ch, Application::Voip)
        .map_err(|e| format!("opus encoder init: {e}"))?;
    inner.set_bitrate(audiopus::Bitrate::BitsPerSecond(bitrate_bps))
        .map_err(|e| format!("opus set_bitrate: {e}"))?;
    inner.enable_dtx().map_err(|e| format!("opus enable_dtx: {e}"))?;
    inner.set_inband_fec(true).map_err(|e| format!("opus set_inband_fec: {e}"))?;
    Ok(Self {
        inner,
        channels,
        out_buf: vec![0u8; 4000], // Opus max packet ~= 4000 bytes
    })
}
```

Application choice: `Voip` enables speech-optimized internal config (SILK mode at low bitrates, voice-tuned VAD). `Audio` would be wrong for voice; `LowDelay` sacrifices quality for sub-20ms latency we don't need here.

DTX + FEC are unconditional for voice. DTX saves bandwidth during silence (Opus emits a tiny ~3-byte SID packet); FEC adds ~10% bitrate overhead but lets the receiver recover dropped packets when the next one arrives intact.

Complexity defaults to 10 (max) — the encoder picks the best quality given the bitrate budget. Lowering it reduces CPU but degrades quality; not exposed in v1.

### `encode`

```rust
pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, String> {
    let expected = OPUS_FRAME_SAMPLES_MONO * self.channels as usize;
    if pcm.len() != expected {
        return Err(format!("expected {expected} samples ({}ms @ 48 kHz {} channels), got {}",
            OPUS_FRAME_DURATION_MS, self.channels, pcm.len()));
    }
    let written = self.inner.encode_float(pcm, &mut self.out_buf)
        .map_err(|e| format!("opus encode: {e}"))?;
    Ok(self.out_buf[..written].to_vec())
}
```

The encoder hot path avoids allocating a fresh buffer per call by reusing `self.out_buf`. Returns a `Vec<u8>` so the caller can hold onto the packet bytes; cloning the `[..written]` slice is the unavoidable copy. (The caller will hand this off to the AEAD seal step, which makes its own copy regardless.)

Empty PCM (length 0) is treated as a hard error — DTX silence is the encoder's job, not the caller's. If the caller wants to skip transmission, it shouldn't call `encode` at all.

### Output size

At 24 kbps with 20 ms frames, expect ~60 bytes per packet typical, with DTX SID packets ~3 bytes during silence. The 4000-byte output buffer matches libopus's documented max.

## OpusDecoder

### Construction

```rust
pub fn new(sample_rate: u32, channels: u16) -> Result<Self, String> {
    if sample_rate != OPUS_SAMPLE_RATE {
        return Err(format!("only 48 kHz supported; got {sample_rate}"));
    }
    if channels != 1 && channels != 2 {
        return Err(format!("only mono or stereo supported; got channels={channels}"));
    }
    let sr = SampleRate::Hz48000;
    let ch = match channels { 1 => Channels::Mono, 2 => Channels::Stereo, _ => unreachable!() };
    let inner = audiopus::coder::Decoder::new(sr, ch)
        .map_err(|e| format!("opus decoder init: {e}"))?;
    Ok(Self {
        inner,
        channels,
        out_buf: vec![0.0_f32; OPUS_FRAME_SAMPLES_MONO * channels as usize],
    })
}
```

### `decode`

```rust
pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>, String> {
    if packet.is_empty() {
        return Err("decode called with empty packet (use decode_plc instead)".into());
    }
    let written = self.inner.decode_float(Some(packet), &mut self.out_buf, false)
        .map_err(|e| format!("opus decode: {e}"))?;
    let expected = OPUS_FRAME_SAMPLES_MONO * self.channels as usize;
    if written != expected {
        return Err(format!("opus decoded {written} samples, expected {expected}"));
    }
    Ok(self.out_buf[..written].to_vec())
}
```

The `false` parameter to `decode_float` is `decode_fec`: we're not using FEC-recovered decode in v1 (see Non-Goals). It's set false so we always decode the current packet straightforwardly.

### `decode_plc` (packet-loss concealment)

```rust
pub fn decode_plc(&mut self) -> Result<Vec<f32>, String> {
    let written = self.inner.decode_float(None, &mut self.out_buf, false)
        .map_err(|e| format!("opus decode_plc: {e}"))?;
    let expected = OPUS_FRAME_SAMPLES_MONO * self.channels as usize;
    if written != expected {
        return Err(format!("opus PLC produced {written} samples, expected {expected}"));
    }
    Ok(self.out_buf[..written].to_vec())
}
```

Calling `decode_float` with `None` for the input packet tells libopus to synthesize a plausible frame based on the previous frame's signal. Quality degrades after ~3 consecutive losses; caller (sub-project #3.3) should track gap length and possibly fall back to silence if too many frames are missed in a row. v1 doesn't enforce this.

## Cargo dependency

```toml
# client/src-tauri/Cargo.toml
audiopus = "0.3"
```

`audiopus = "0.3"` is the current stable. Depends on `audiopus_sys` which requires libopus + cmake at build time — both already installed in the dev environment.

## Testing

### Unit tests (`#[cfg(test)] mod tests` in `opus_codec.rs`)

- `encoder_constructs_with_sane_defaults` — `OpusEncoder::new(48000, 1, 24000)` succeeds.
- `encoder_rejects_non_48khz_sample_rate` — `new(44100, 1, 24000)` returns Err.
- `encoder_rejects_invalid_channels` — `new(48000, 0, 24000)` returns Err; same for 3+.
- `encoder_rejects_wrong_frame_size` — pass a 480-sample slice (10 ms) into a 20 ms encoder, expect Err with clear message.
- `decoder_constructs_with_sane_defaults` — `OpusDecoder::new(48000, 1)` succeeds.
- `roundtrip_sine_wave_produces_correct_length_output` — encode a 960-sample sine wave at 440 Hz, then decode; assert decoded.len() == 960. Byte-for-byte equality isn't possible (Opus is lossy) but length must match.
- `roundtrip_sine_wave_preserves_signal_envelope` — same as above, but check that the decoded RMS is within 50% of the input RMS. A sanity check that the codec is actually producing audio of the expected amplitude order.
- `decode_rejects_empty_packet` — `decode(&[])` returns Err.
- `decode_rejects_garbage` — `decode(&[0xff, 0xff, 0xff])` returns Err.
- `decode_plc_returns_correct_length` — `decoder.decode_plc()` returns 960 samples (libopus synthesizes silence if no prior context).
- `dtx_silence_produces_small_packet` — encode 960 samples of zero PCM; assert encoded packet is <= 10 bytes (DTX SID packet).

### What's NOT tested

- Bitrate accuracy under sustained encoding (would require timing + statistical analysis).
- Real-world packet loss scenarios (PLC quality after N consecutive losses).
- Stereo encoding paths (the API supports it but voice uses mono; channel == 1 is the only exercised path).
- Live cpal → encoder → decoder → cpal smoke. That's sub-project #3.3 territory on a real OS.

## Migration / rollout

Pure addition. No existing code is affected. Sub-project #3.3 (voice pipeline) will be the first consumer.

## Future considerations

- **Variable frame sizes** (10 ms for ultra-low-latency, 40 ms for low-bandwidth efficiency).
- **FEC-aware decode** — when packet is lost, decode FEC from the *next* packet for higher-quality recovery than PLC.
- **Encoder hot-reconfiguration** — change bitrate / complexity mid-stream without dropping + recreating.
- **Consecutive-loss tracking** — fall back to silence after N PLC frames in a row.
- **Stereo voice** for screen-share-with-system-audio scenarios.
- **Tone-detection sidechannel** — Opus exposes signal-type detection (voice vs music vs noise); could feed UI ("background noise detected").

---

This spec is intentionally tight: encoder + decoder + PLC + tests. Larger voice flow (capture pipeline, transmit lifecycle, key exchange) ships in sub-project #3.3.
