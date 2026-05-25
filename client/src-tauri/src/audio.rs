// client/src-tauri/src/audio.rs
//
// AudioBackend trait + mock implementation.
//
// Voice (Phase 3) replaces the `_ => mock` arm in make_audio_backend with
// a real cpal/audiopus-backed implementation. Until then, the factory
// returns the mock so dev work in WSL (no audio hardware) isn't blocked.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

#[derive(Debug, Clone)]
pub struct AudioInputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone)]
pub struct AudioOutputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
    /// Total f32 samples per chunk, interleaved across channels.
    /// e.g. 48000 sample_rate * 1 channel * 20ms / 1000 = 960
    pub samples_per_chunk: usize,
}

/// A chunk of f32 PCM samples in [-1.0, 1.0], interleaved across channels.
pub struct PcmChunk {
    pub samples: Vec<f32>,
    pub timestamp_ms: u64,
}

pub trait AudioBackend: Send + Sync {
    fn enumerate_input_devices(&self) -> Result<Vec<AudioInputDevice>, String>;
    fn enumerate_output_devices(&self) -> Result<Vec<AudioOutputDevice>, String>;
    fn start_capture(
        &self,
        device_id: Option<&str>,
        format: AudioFormat,
    ) -> Result<mpsc::Receiver<PcmChunk>, String>;
    fn stop_capture(&self) -> Result<(), String>;
    fn start_playback(
        &self,
        device_id: Option<&str>,
        format: AudioFormat,
    ) -> Result<mpsc::SyncSender<PcmChunk>, String>;
    fn stop_playback(&self) -> Result<(), String>;
    fn backend_name(&self) -> &'static str;
}

pub struct MockAudioBackend {
    capture: Mutex<Option<JoinHandle<()>>>,
    capture_stop: Mutex<Option<Arc<AtomicBool>>>,
    playback: Mutex<Option<JoinHandle<()>>>,
    playback_stop: Mutex<Option<Arc<AtomicBool>>>,
}

impl MockAudioBackend {
    pub fn new() -> Self {
        Self {
            capture: Mutex::new(None),
            capture_stop: Mutex::new(None),
            playback: Mutex::new(None),
            playback_stop: Mutex::new(None),
        }
    }
}

impl Default for MockAudioBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioBackend for MockAudioBackend {
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
        "mock"
    }
}
