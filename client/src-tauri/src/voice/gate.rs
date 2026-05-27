// client/src-tauri/src/voice/gate.rs
//
// Transmission gating: decides whether each captured frame is forwarded to
// the encoder. v1 ships `Open`; `Vad` and `Ptt` are wired for future work.
// See docs/superpowers/specs/2026-05-26-voice-client-pipeline-design.md.

use std::sync::{atomic::{AtomicBool, Ordering}, Arc};

#[derive(Clone, Debug)]
pub struct VadConfig {
    pub rms_threshold: f32,
}

#[derive(Clone, Debug)]
pub enum GateMode {
    Open,
    Vad(VadConfig),
    Ptt(Arc<AtomicBool>),
}

impl GateMode {
    pub fn pass(&self, _pcm: &[f32]) -> bool {
        match self {
            GateMode::Open => true,
            GateMode::Vad(_) => true, // v1 stub
            GateMode::Ptt(flag) => flag.load(Ordering::Acquire),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_always_passes() {
        let g = GateMode::Open;
        assert!(g.pass(&[0.0; 960]));
        assert!(g.pass(&[]));
    }

    #[test]
    fn ptt_false_blocks_true_passes() {
        let flag = Arc::new(AtomicBool::new(false));
        let g = GateMode::Ptt(flag.clone());
        assert!(!g.pass(&[0.5; 960]));
        flag.store(true, Ordering::Release);
        assert!(g.pass(&[0.5; 960]));
    }

    #[test]
    fn vad_v1_stub_always_passes() {
        let g = GateMode::Vad(VadConfig { rms_threshold: 0.01 });
        assert!(g.pass(&[0.5; 960]));
        assert!(g.pass(&[0.0; 960]));
    }
}
