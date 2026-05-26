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
    use cpal::SampleRate;
    let want_sr = SampleRate(format.sample_rate);
    let want_channels = format.channels;
    let configs = device.supported_input_configs()
        .map_err(|e| format!("supported_input_configs: {e}"))?;
    let mut exact_match: Option<cpal::SupportedStreamConfigRange> = None;
    let mut stereo_match: Option<cpal::SupportedStreamConfigRange> = None;
    for cfg in configs {
        if cfg.sample_format() != cpal::SampleFormat::F32 {
            continue;
        }
        if cfg.min_sample_rate() <= want_sr && want_sr <= cfg.max_sample_rate() {
            if cfg.channels() == want_channels {
                exact_match = Some(cfg);
                break;
            }
            if want_channels == 1 && cfg.channels() == 2 && stereo_match.is_none() {
                stereo_match = Some(cfg);
            }
        }
    }
    let chosen = exact_match.or(stereo_match)
        .ok_or_else(|| format!("no supported input config for {format:?}"))?;
    Ok(chosen.with_sample_rate(want_sr).config())
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
                    let sample = if dev_channels == want_channels {
                        raw[i]
                    } else if dev_channels == 2 && want_channels == 1 {
                        (raw[i] + raw[i + 1]) / 2.0
                    } else {
                        0.0
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
        _device_id: Option<&str>,
        _format: AudioFormat,
    ) -> Result<mpsc::SyncSender<PcmChunk>, String> {
        Err("not yet implemented".into())
    }
    fn stop_playback(&self) -> Result<(), String> {
        Err("not yet implemented".into())
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
}
