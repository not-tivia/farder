// client/src-tauri/src/voice/apm.rs
//
// AudioProcessor: pluggable post-capture / pre-mix processing stage.
// v1 ships a no-op fallback. WebRTC-based AEC/NS/AGC is the eventual target
// (blocked on libwebrtc-audio-processing-2 system dep). See
// docs/superpowers/specs/2026-05-26-voice-client-pipeline-design.md and
// the migration note in VOICE-4 of the plan.

use crate::opus_codec::OPUS_FRAME_SAMPLES_MONO;

pub struct AudioProcessor {
    fallback: bool,
}

impl AudioProcessor {
    /// Construct an AudioProcessor. v1 always returns the no-op fallback.
    pub fn new() -> Self {
        Self { fallback: true }
    }

    /// Push a render frame (the mixed playback PCM) into APM for AEC reference.
    /// Must be called before `process_capture` for the next 20 ms tick.
    /// v1 fallback: no-op.
    pub fn process_render(&mut self, _pcm: &mut [f32]) {
        // no-op until WebRTC APM is wired
    }

    /// Process one capture frame in place.
    /// v1 fallback: no-op (passes PCM through unchanged).
    pub fn process_capture(&mut self, _pcm: &mut [f32]) {
        // no-op until WebRTC APM is wired
    }

    pub fn is_fallback(&self) -> bool {
        self.fallback
    }
}

impl Default for AudioProcessor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_in_fallback_mode_in_v1() {
        let apm = AudioProcessor::new();
        assert!(apm.is_fallback(), "v1 always returns the no-op fallback");
    }

    #[test]
    fn process_capture_preserves_frame_length() {
        let mut apm = AudioProcessor::new();
        let mut frame = vec![0.0f32; OPUS_FRAME_SAMPLES_MONO];
        apm.process_capture(&mut frame);
        assert_eq!(frame.len(), OPUS_FRAME_SAMPLES_MONO, "frame length must be preserved");
    }

    #[test]
    fn process_render_does_not_panic_on_correct_frame_size() {
        let mut apm = AudioProcessor::new();
        let mut frame = vec![0.0f32; OPUS_FRAME_SAMPLES_MONO];
        apm.process_render(&mut frame);
        assert_eq!(frame.len(), OPUS_FRAME_SAMPLES_MONO);
    }

    #[test]
    fn fallback_mode_passes_pcm_through_unchanged() {
        let mut apm = AudioProcessor::new();
        let original: Vec<f32> = (0..OPUS_FRAME_SAMPLES_MONO).map(|i| i as f32 * 0.001).collect();
        let mut frame = original.clone();
        apm.process_capture(&mut frame);
        assert_eq!(frame, original, "fallback must pass PCM through unchanged");
    }
}
