//! Screen-audio (game audio) capture seam. The real implementation is WASAPI
//! output-device loopback on Windows (screen_audio_wasapi.rs); elsewhere a mock
//! tone so the pipeline is testable headlessly. Produces 48 kHz MONO PcmChunks
//! (exactly OPUS_FRAME_SAMPLES_MONO per chunk) on an mpsc channel, matching the
//! voice send path's contract.

use crate::audio::PcmChunk;
use std::sync::mpsc;

/// A selectable system output (render) device for loopback capture.
#[derive(Clone, Debug, serde::Serialize)]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// A running loopback capture. Dropping/`stop()`-ing it ends the capture thread.
pub trait ScreenAudioCapture: Send {
    /// Stop capturing (idempotent).
    fn stop(&self);
}

/// List output devices available for loopback capture (for the picker).
pub fn list_output_devices() -> Result<Vec<OutputDevice>, String> {
    #[cfg(windows)]
    {
        crate::screen_audio_wasapi::list_output_devices()
    }
    #[cfg(not(windows))]
    {
        Ok(vec![OutputDevice { id: "mock".into(), name: "Mock Output (no WASAPI)".into(), is_default: true }])
    }
}

/// Start loopback capture of `device_id` (None = system default render device).
/// Delivers 48 kHz mono PcmChunks of OPUS_FRAME_SAMPLES_MONO samples each.
pub fn start_capture(device_id: Option<String>) -> Result<(Box<dyn ScreenAudioCapture>, mpsc::Receiver<PcmChunk>), String> {
    #[cfg(windows)]
    {
        crate::screen_audio_wasapi::start_capture(device_id)
    }
    #[cfg(not(windows))]
    {
        let _ = device_id;
        start_mock_capture()
    }
}

#[cfg(not(windows))]
fn start_mock_capture() -> Result<(Box<dyn ScreenAudioCapture>, mpsc::Receiver<PcmChunk>), String> {
    use crate::opus_codec::{OPUS_FRAME_SAMPLES_MONO, OPUS_SAMPLE_RATE};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct MockCap { stop: Arc<AtomicBool> }
    impl ScreenAudioCapture for MockCap { fn stop(&self) { self.stop.store(true, Ordering::Relaxed); } }

    let (tx, rx) = mpsc::channel::<PcmChunk>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = stop.clone();
    std::thread::spawn(move || {
        let mut phase = 0.0f32;
        while !stop_t.load(Ordering::Relaxed) {
            let mut samples = Vec::with_capacity(OPUS_FRAME_SAMPLES_MONO);
            for _ in 0..OPUS_FRAME_SAMPLES_MONO {
                samples.push((phase).sin() * 0.3);
                phase += 2.0 * std::f32::consts::PI * 440.0 / OPUS_SAMPLE_RATE as f32;
            }
            if tx.send(PcmChunk { samples, timestamp_ms: 0 }).is_err() { break; }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
    Ok((Box::new(MockCap { stop }), rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opus_codec::OPUS_FRAME_SAMPLES_MONO;
    use std::time::Duration;

    #[test]
    fn capture_delivers_mono_frames_of_the_right_size() {
        let (cap, rx) = start_capture(None).expect("capture starts");
        let chunk = rx.recv_timeout(Duration::from_secs(2)).expect("a chunk arrives");
        assert_eq!(chunk.samples.len(), OPUS_FRAME_SAMPLES_MONO, "must be one 20ms mono frame");
        cap.stop();
    }

    #[test]
    fn lists_at_least_one_output_device() {
        let devices = list_output_devices().expect("enumerate");
        assert!(!devices.is_empty(), "must list at least one output device");
    }
}
