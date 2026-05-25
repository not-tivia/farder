// client/src-tauri/src/display.rs
//
// DisplayBackend trait + mock implementation.
//
// Screensharing replaces the `_ => mock` arm in make_display_backend with
// a real scrap/native-backed implementation. Until then, the factory
// returns the mock so dev work in WSL (no display capture) isn't blocked.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub enum DisplaySourceKind {
    Screen,
    Window,
}

#[derive(Debug, Clone)]
pub struct DisplaySource {
    pub id: String,
    pub kind: DisplaySourceKind,
    pub label: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct DisplayFormat {
    pub fps: u32,
    pub max_width: u32,
    pub max_height: u32,
}

/// A captured frame in RGBA8888, row-major, packed (stride = width * 4).
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub pixels: Vec<u8>,
    pub timestamp_ms: u64,
}

pub trait DisplayBackend: Send + Sync {
    fn enumerate_sources(&self) -> Result<Vec<DisplaySource>, String>;
    fn start_capture(
        &self,
        source_id: &str,
        format: DisplayFormat,
    ) -> Result<mpsc::Receiver<VideoFrame>, String>;
    fn stop_capture(&self) -> Result<(), String>;
    fn backend_name(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// 5×7 bitmap font for digits 0–9. Each row is a u8 where the low 5 bits map
// to pixels left-to-right (bit 4 = leftmost). Used by the mock display to
// render a frame counter overlay; that's all — no full font system.
// ---------------------------------------------------------------------------
const DIGIT_FONT: [[u8; 7]; 10] = [
    // 0
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
    // 1
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    // 2
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    // 3
    [0b01110, 0b10001, 0b00001, 0b00110, 0b00001, 0b10001, 0b01110],
    // 4
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    // 5
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    // 6
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    // 7
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    // 8
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    // 9
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
];

pub struct MockDisplayBackend {
    capture: Mutex<Option<JoinHandle<()>>>,
    capture_stop: Mutex<Option<Arc<AtomicBool>>>,
}

impl MockDisplayBackend {
    pub fn new() -> Self {
        Self {
            capture: Mutex::new(None),
            capture_stop: Mutex::new(None),
        }
    }
}

impl Default for MockDisplayBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayBackend for MockDisplayBackend {
    fn enumerate_sources(&self) -> Result<Vec<DisplaySource>, String> {
        Ok(vec![DisplaySource {
            id: "mock-display".into(),
            kind: DisplaySourceKind::Screen,
            label: "Mock Display 1280×720".into(),
            width: 1280,
            height: 720,
        }])
    }
    fn start_capture(
        &self,
        _source_id: &str,
        _format: DisplayFormat,
    ) -> Result<mpsc::Receiver<VideoFrame>, String> {
        Err("not yet implemented".into())
    }
    fn stop_capture(&self) -> Result<(), String> {
        Err("not yet implemented".into())
    }
    fn backend_name(&self) -> &'static str {
        "mock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_enumerate_returns_one_source() {
        let backend = MockDisplayBackend::new();
        let sources = backend.enumerate_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id, "mock-display");
        assert_eq!(sources[0].width, 1280);
        assert_eq!(sources[0].height, 720);
        assert!(matches!(sources[0].kind, DisplaySourceKind::Screen));
    }
}
