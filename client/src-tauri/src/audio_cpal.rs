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
        Err("not yet implemented".into())
    }
    fn enumerate_output_devices(&self) -> Result<Vec<AudioOutputDevice>, String> {
        Err("not yet implemented".into())
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
