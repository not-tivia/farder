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
        _device_id: Option<&str>,
        _format: AudioFormat,
    ) -> Result<mpsc::Receiver<PcmChunk>, String> {
        Err("not yet implemented".into())
    }
    fn stop_capture(&self) -> Result<(), String> {
        Err("not yet implemented".into())
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
