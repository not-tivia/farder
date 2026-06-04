// client/src-tauri/src/audio_cpal.rs
//
// Real AudioBackend backed by cpal. Bridges cpal's callback-based audio
// API into the AudioBackend trait's `mpsc` channel model.
//
// See docs/superpowers/specs/2026-05-25-cpal-audio-backend-design.md.

use crate::audio::{
    AudioBackend, AudioFormat, AudioInputDevice, AudioOutputDevice, PcmChunk,
};
use cpal::traits::{DeviceTrait, HostTrait};
use send_wrapper::SendWrapper;
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Instant;

pub struct CpalAudioBackend {
    host: SendWrapper<cpal::Host>,
    capture_stream: Mutex<Option<SendWrapper<cpal::Stream>>>,
    playback_stream: Mutex<Option<SendWrapper<cpal::Stream>>>,
}

impl CpalAudioBackend {
    pub fn new() -> Self {
        Self {
            host: SendWrapper::new(cpal::default_host()),
            capture_stream: Mutex::new(None),
            playback_stream: Mutex::new(None),
        }
    }
}

impl Default for CpalAudioBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Average all interleaved channels of one device frame down to a single mono
/// sample. Empty frame -> silence.
fn downmix_frame_to_mono(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    frame.iter().sum::<f32>() / frame.len() as f32
}

/// Write one mono sample across `channels` interleaved output slots,
/// replicating it to every channel. Always writes at least one slot.
fn upmix_mono_into(sample: f32, channels: usize, out: &mut Vec<f32>) {
    for _ in 0..channels.max(1) {
        out.push(sample);
    }
}

/// Choose an f32 `StreamConfig` whose sample-rate range covers `want_sr`,
/// preferring `want_channels`, then stereo, then ANY channel count (the
/// callbacks down/upmix between mono and whatever the device offers). On
/// failure the error lists what the device actually exposes, for diagnosis.
fn choose_stream_config(
    configs: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
    want_sr: cpal::SampleRate,
    want_channels: u16,
    label: &str,
    format: &AudioFormat,
) -> Result<cpal::StreamConfig, String> {
    let mut exact: Option<cpal::SupportedStreamConfigRange> = None;
    let mut stereo: Option<cpal::SupportedStreamConfigRange> = None;
    let mut any: Option<cpal::SupportedStreamConfigRange> = None;
    let mut available: Vec<String> = Vec::new();
    for cfg in configs {
        available.push(format!(
            "{:?}/{}ch/{}-{}Hz",
            cfg.sample_format(),
            cfg.channels(),
            cfg.min_sample_rate().0,
            cfg.max_sample_rate().0
        ));
        if cfg.sample_format() != cpal::SampleFormat::F32 {
            continue;
        }
        if cfg.min_sample_rate() > want_sr || want_sr > cfg.max_sample_rate() {
            continue;
        }
        if cfg.channels() == want_channels {
            if exact.is_none() {
                exact = Some(cfg);
            }
        } else if cfg.channels() == 2 {
            if stereo.is_none() {
                stereo = Some(cfg);
            }
        } else if any.is_none() {
            any = Some(cfg);
        }
    }
    let chosen = exact.or(stereo).or(any).ok_or_else(|| {
        format!(
            "no supported {label} config for {format:?}; device offers: [{}]",
            available.join(", ")
        )
    })?;
    Ok(chosen.with_sample_rate(want_sr).config())
}

/// Pick an input device by name, or the host's default if `device_id` is None.
fn pick_input_device(host: &cpal::Host, device_id: Option<&str>) -> Result<cpal::Device, String> {
    match device_id {
        None => host.default_input_device()
            .ok_or_else(|| "no default input device".to_string()),
        Some(name) => host.input_devices()
            .map_err(|e| format!("input_devices: {e}"))?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| format!("input device not found: {name}")),
    }
}

/// Find a StreamConfig on `device` whose sample rate matches
/// `format.sample_rate` and whose channel count is either `format.channels`
/// (preferred) or 2 (we'll downmix to mono).
fn build_input_config(
    device: &cpal::Device,
    format: &AudioFormat,
) -> Result<cpal::StreamConfig, String> {
    let want_sr = cpal::SampleRate(format.sample_rate);
    let configs = device.supported_input_configs()
        .map_err(|e| format!("supported_input_configs: {e}"))?;
    choose_stream_config(configs, want_sr, format.channels, "input", format)
}

fn pick_output_device(host: &cpal::Host, device_id: Option<&str>) -> Result<cpal::Device, String> {
    match device_id {
        None => host.default_output_device()
            .ok_or_else(|| "no default output device".to_string()),
        Some(name) => host.output_devices()
            .map_err(|e| format!("output_devices: {e}"))?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| format!("output device not found: {name}")),
    }
}

fn build_output_config(
    device: &cpal::Device,
    format: &AudioFormat,
) -> Result<cpal::StreamConfig, String> {
    let want_sr = cpal::SampleRate(format.sample_rate);
    let configs = device.supported_output_configs()
        .map_err(|e| format!("supported_output_configs: {e}"))?;
    choose_stream_config(configs, want_sr, format.channels, "output", format)
}

impl AudioBackend for CpalAudioBackend {
    fn enumerate_input_devices(&self) -> Result<Vec<AudioInputDevice>, String> {
        let devices = self.host.input_devices()
            .map_err(|e| format!("input_devices: {e}"))?;
        let default = self.host.default_input_device()
            .and_then(|d| d.name().ok());
        let mut out = Vec::new();
        for (i, dev) in devices.enumerate() {
            let name = dev.name().unwrap_or_else(|_| format!("device-{i}"));
            let is_default = default.as_deref() == Some(name.as_str());
            out.push(AudioInputDevice {
                id: name.clone(),
                name: name.clone(),
                is_default,
            });
        }
        Ok(out)
    }

    fn enumerate_output_devices(&self) -> Result<Vec<AudioOutputDevice>, String> {
        let devices = self.host.output_devices()
            .map_err(|e| format!("output_devices: {e}"))?;
        let default = self.host.default_output_device()
            .and_then(|d| d.name().ok());
        let mut out = Vec::new();
        for (i, dev) in devices.enumerate() {
            let name = dev.name().unwrap_or_else(|_| format!("device-{i}"));
            let is_default = default.as_deref() == Some(name.as_str());
            out.push(AudioOutputDevice {
                id: name.clone(),
                name: name.clone(),
                is_default,
            });
        }
        Ok(out)
    }
    fn start_capture(
        &self,
        device_id: Option<&str>,
        format: AudioFormat,
    ) -> Result<mpsc::Receiver<PcmChunk>, String> {
        let mut slot = self.capture_stream.lock()
            .map_err(|e| format!("capture lock: {e}"))?;
        if slot.is_some() {
            return Err("capture already active".into());
        }
        if format.channels == 0 || format.samples_per_chunk == 0 {
            return Err(format!("invalid AudioFormat: {format:?}"));
        }

        let device = pick_input_device(&self.host, device_id)?;
        let cpal_config = build_input_config(&device, &format)?;
        let dev_channels = cpal_config.channels as usize;
        let want_channels = format.channels as usize;
        let samples_per_chunk = format.samples_per_chunk;
        let (tx, rx) = mpsc::sync_channel::<PcmChunk>(8);

        let started = Instant::now();
        let mut buffered: Vec<f32> = Vec::with_capacity(samples_per_chunk);

        let stream = device.build_input_stream(
            &cpal_config,
            move |raw: &[f32], _info: &cpal::InputCallbackInfo| {
                // Walk cpal's interleaved samples one frame at a time. Downmix
                // stereo->mono if needed. Emit a PcmChunk each time we've
                // accumulated samples_per_chunk samples worth of output.
                let mut i = 0;
                while i + dev_channels <= raw.len() {
                    // Engine wants mono; average the device's channels for
                    // this frame down to one sample (handles stereo, 5.1, etc.).
                    let sample = if dev_channels == want_channels {
                        raw[i]
                    } else {
                        downmix_frame_to_mono(&raw[i..i + dev_channels])
                    };
                    buffered.push(sample);
                    i += dev_channels;

                    if buffered.len() >= samples_per_chunk {
                        let chunk = PcmChunk {
                            samples: std::mem::take(&mut buffered),
                            timestamp_ms: started.elapsed().as_millis() as u64,
                        };
                        buffered.reserve(samples_per_chunk);
                        let _ = tx.try_send(chunk);
                    }
                }
            },
            |err| eprintln!("[audio] cpal capture error: {err}"),
            None,
        ).map_err(|e| format!("build_input_stream: {e}"))?;

        use cpal::traits::StreamTrait;
        stream.play().map_err(|e| format!("stream.play: {e}"))?;
        *slot = Some(SendWrapper::new(stream));
        Ok(rx)
    }

    fn stop_capture(&self) -> Result<(), String> {
        let mut slot = self.capture_stream.lock()
            .map_err(|e| format!("capture lock: {e}"))?;
        slot.take();
        Ok(())
    }
    fn start_playback(
        &self,
        device_id: Option<&str>,
        format: AudioFormat,
    ) -> Result<mpsc::SyncSender<PcmChunk>, String> {
        let mut slot = self.playback_stream.lock()
            .map_err(|e| format!("playback lock: {e}"))?;
        if slot.is_some() {
            return Err("playback already active".into());
        }
        if format.channels == 0 || format.samples_per_chunk == 0 {
            return Err(format!("invalid AudioFormat: {format:?}"));
        }

        let device = pick_output_device(&self.host, device_id)?;
        let cpal_config = build_output_config(&device, &format)?;
        let dev_channels = cpal_config.channels as usize;
        let want_channels = format.channels as usize;

        // Buffer sized for ~500ms of audio (matches the mock's behavior).
        let frames_per_chunk = (format.samples_per_chunk / want_channels).max(1);
        let chunks_per_500ms = ((format.sample_rate as f32 * 0.5)
            / frames_per_chunk as f32).ceil() as usize;
        let buf = chunks_per_500ms.max(2);
        let (tx, rx) = mpsc::sync_channel::<PcmChunk>(buf);

        let mut pending: Vec<f32> = Vec::new();

        let stream = device.build_output_stream(
            &cpal_config,
            move |out: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                // Refill `pending` from the channel as needed.
                while pending.len() < out.len() {
                    match rx.try_recv() {
                        Ok(chunk) => {
                            if dev_channels == want_channels {
                                pending.extend_from_slice(&chunk.samples);
                            } else {
                                // Engine produces mono; replicate each sample
                                // across the device's channels (stereo, 5.1...).
                                for &s in &chunk.samples {
                                    upmix_mono_into(s, dev_channels, &mut pending);
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                let take = pending.len().min(out.len());
                out[..take].copy_from_slice(&pending[..take]);
                pending.drain(..take);
                // Underrun -> silence.
                for s in &mut out[take..] {
                    *s = 0.0;
                }
            },
            |err| eprintln!("[audio] cpal playback error: {err}"),
            None,
        ).map_err(|e| format!("build_output_stream: {e}"))?;

        use cpal::traits::StreamTrait;
        stream.play().map_err(|e| format!("stream.play: {e}"))?;
        *slot = Some(SendWrapper::new(stream));
        Ok(tx)
    }

    fn stop_playback(&self) -> Result<(), String> {
        let mut slot = self.playback_stream.lock()
            .map_err(|e| format!("playback lock: {e}"))?;
        slot.take();
        Ok(())
    }
    fn backend_name(&self) -> &'static str {
        "cpal"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpal_backend_constructs_without_panicking() {
        let _backend = CpalAudioBackend::new();
    }

    #[test]
    fn cpal_backend_name_is_cpal() {
        let backend = CpalAudioBackend::new();
        assert_eq!(backend.backend_name(), "cpal");
    }

    #[test]
    fn cpal_enumerate_input_devices_returns_vec() {
        let backend = CpalAudioBackend::new();
        let _devices = backend.enumerate_input_devices().expect("enumerate input");
    }

    #[test]
    fn cpal_enumerate_output_devices_returns_vec() {
        let backend = CpalAudioBackend::new();
        let _devices = backend.enumerate_output_devices().expect("enumerate output");
    }

    #[test]
    fn downmix_stereo_frame_averages_channels() {
        assert!((downmix_frame_to_mono(&[0.4, 0.6]) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn downmix_surround_frame_averages_all_channels() {
        // 6-channel (5.1) frame averages to 0.5.
        let frame = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0];
        assert!((downmix_frame_to_mono(&frame) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn downmix_mono_frame_passes_through() {
        assert!((downmix_frame_to_mono(&[0.7]) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn downmix_empty_frame_is_silent() {
        assert_eq!(downmix_frame_to_mono(&[]), 0.0);
    }

    #[test]
    fn upmix_mono_to_stereo_duplicates_sample() {
        let mut out = Vec::new();
        upmix_mono_into(0.7, 2, &mut out);
        assert_eq!(out, vec![0.7, 0.7]);
    }

    #[test]
    fn upmix_mono_to_surround_replicates_to_all_channels() {
        let mut out = Vec::new();
        upmix_mono_into(0.3, 6, &mut out);
        assert_eq!(out, vec![0.3; 6]);
    }

    #[test]
    fn upmix_zero_channels_emits_at_least_one() {
        // Defensive: never silently drop a sample.
        let mut out = Vec::new();
        upmix_mono_into(0.9, 0, &mut out);
        assert_eq!(out, vec![0.9]);
    }
}
