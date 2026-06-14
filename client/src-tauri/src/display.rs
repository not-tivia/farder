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

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub enum DisplaySourceKind {
    Screen,
    Window,
}

#[derive(Debug, Clone, serde::Serialize)]
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

/// HSV→RGB conversion. h in [0, 360), s+v in [0, 1]. Returns (r, g, b) as u8.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let h_prime = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (h_prime % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match h_prime as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to_u8 = |f: f32| ((f + m) * 255.0).clamp(0.0, 255.0) as u8;
    (to_u8(r1), to_u8(g1), to_u8(b1))
}

/// Draw `digit` (0..=9) at position (x, y) into a packed RGBA8888 buffer of
/// `stride` bytes per row. Each lit pixel becomes black on white background
/// (digit is 5×7 with a 1px white margin on all sides → 7×9 footprint).
fn draw_digit(buf: &mut [u8], stride: usize, x: usize, y: usize, digit: u32) {
    let glyph_idx = (digit % 10) as usize;
    let glyph = &DIGIT_FONT[glyph_idx];
    // 7x9 white background.
    for dy in 0..9 {
        for dx in 0..7 {
            let px = (y + dy) * stride + (x + dx) * 4;
            if px + 3 < buf.len() {
                buf[px] = 255;
                buf[px + 1] = 255;
                buf[px + 2] = 255;
                buf[px + 3] = 255;
            }
        }
    }
    // 5x7 glyph in the inner area (offset by 1, 1).
    for (row_idx, row_bits) in glyph.iter().enumerate() {
        for col_idx in 0..5 {
            let lit = (row_bits >> (4 - col_idx)) & 1 == 1;
            if lit {
                let px = (y + 1 + row_idx) * stride + (x + 1 + col_idx) * 4;
                if px + 3 < buf.len() {
                    buf[px] = 0;
                    buf[px + 1] = 0;
                    buf[px + 2] = 0;
                    buf[px + 3] = 255;
                }
            }
        }
    }
}

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
        format: DisplayFormat,
    ) -> Result<mpsc::Receiver<VideoFrame>, String> {
        let mut capture_slot = self.capture.lock().map_err(|e| e.to_string())?;
        if capture_slot.is_some() {
            return Err("capture already active".into());
        }
        if format.fps == 0 || format.max_width == 0 || format.max_height == 0 {
            return Err(format!("invalid DisplayFormat: {:?}", format));
        }

        let width = format.max_width.min(1280) as usize;
        let height = format.max_height.min(720) as usize;
        let stride = width * 4;
        let frame_period = Duration::from_secs_f32(1.0 / format.fps as f32);

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        // Bounded channel — 4 frames of slack.
        let (tx, rx) = mpsc::sync_channel::<VideoFrame>(4);

        let started = Instant::now();
        let handle = std::thread::spawn(move || {
            // Reusable pixel buffer, cloned per frame for the channel.
            let mut buf = vec![0u8; height * stride];
            let mut frame_count: u32 = 0;
            while !stop_clone.load(Ordering::Relaxed) {
                let frame_start = Instant::now();

                // HSV gradient: hue rotates with frame_count; saturation 1.0;
                // value gradient diagonally across the frame.
                let hue_base = (frame_count as f32 * 2.0) % 360.0;
                for y in 0..height {
                    for x in 0..width {
                        let h = (hue_base + (x as f32 / width as f32) * 60.0) % 360.0;
                        let v = (x as f32 + y as f32) / (width as f32 + height as f32);
                        let (r, g, b) = hsv_to_rgb(h, 1.0, v);
                        let px = y * stride + x * 4;
                        buf[px] = r;
                        buf[px + 1] = g;
                        buf[px + 2] = b;
                        buf[px + 3] = 255;
                    }
                }

                // Render frame counter in top-left corner. Each digit is 7px
                // wide (font 5px + 1px margin each side); space them with 1
                // additional pixel of gap.
                let digits: Vec<u32> = {
                    let mut n = frame_count;
                    if n == 0 {
                        vec![0]
                    } else {
                        let mut d = Vec::new();
                        while n > 0 {
                            d.push(n % 10);
                            n /= 10;
                        }
                        d.reverse();
                        d
                    }
                };
                let mut text_x = 4usize;
                for digit in digits {
                    draw_digit(&mut buf, stride, text_x, 4, digit);
                    text_x += 8;
                    if text_x + 8 >= width {
                        break;
                    }
                }

                let frame = VideoFrame {
                    width: width as u32,
                    height: height as u32,
                    stride,
                    pixels: buf.clone(),
                    timestamp_ms: started.elapsed().as_millis() as u64,
                };
                if tx.send(frame).is_err() {
                    break;
                }
                frame_count = frame_count.wrapping_add(1);

                let elapsed = frame_start.elapsed();
                if elapsed < frame_period {
                    std::thread::sleep(frame_period - elapsed);
                }
            }
        });

        *capture_slot = Some(handle);
        *self.capture_stop.lock().map_err(|e| e.to_string())? = Some(stop);
        Ok(rx)
    }

    fn stop_capture(&self) -> Result<(), String> {
        if let Some(stop) = self.capture_stop.lock().map_err(|e| e.to_string())?.take() {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(handle) = self.capture.lock().map_err(|e| e.to_string())?.take() {
            let (done_tx, done_rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = handle.join();
                let _ = done_tx.send(());
            });
            match done_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(()) => {}
                Err(_) => eprintln!("[display] mock capture thread did not join within 200ms"),
            }
        }
        Ok(())
    }
    fn backend_name(&self) -> &'static str {
        "mock"
    }
}

/// Construct a DisplayBackend based on the FARDER_DISPLAY_BACKEND env var.
/// - "mock" → MockDisplayBackend
/// - anything else (or unset) → real backend if shipped, else mock-with-warn
///
/// The screensharing sub-project replaces the fallback arm with a real
/// scrap/native-backed implementation.
pub fn make_display_backend() -> Box<dyn DisplayBackend> {
    match std::env::var("FARDER_DISPLAY_BACKEND").as_deref() {
        Ok("mock") => Box::new(MockDisplayBackend::new()),
        _ => {
            #[cfg(windows)]
            {
                return Box::new(crate::display_wgc::WgcDisplayBackend::new());
            }
            #[allow(unreachable_code)]
            {
                crate::audio::log_once(
                    "display.real_not_shipped",
                    "[display] real backend only on Windows; using mock",
                );
                Box::new(MockDisplayBackend::new())
            }
        }
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

    #[test]
    fn mock_capture_emits_frames_at_expected_fps() {
        let backend = MockDisplayBackend::new();
        let format = DisplayFormat {
            fps: 30,
            max_width: 1280,
            max_height: 720,
        };
        let rx = backend.start_capture("mock-display", format).unwrap();

        let start = Instant::now();
        for _ in 0..5 {
            rx.recv_timeout(Duration::from_millis(500)).unwrap();
        }
        let elapsed = start.elapsed();
        backend.stop_capture().unwrap();

        assert!(
            elapsed >= Duration::from_millis(100),
            "5 frames @30fps should take at least ~100ms, got {elapsed:?}",
        );
        // Generous upper bound: 5 frames @30fps is ~133ms ideal; the cap only
        // catches a gross mis-pacing/slowdown. The old ~400ms bound flaked under
        // WSL scheduler jitter (which adds tens of ms), so it's widened to 800ms
        // — still well under a real regression (e.g. seconds) but jitter-proof.
        assert!(
            elapsed <= Duration::from_millis(800),
            "5 frames @30fps should take no more than ~800ms (WSL scheduler slack), got {elapsed:?}",
        );
    }

    #[test]
    fn mock_capture_frames_are_correct_size() {
        let backend = MockDisplayBackend::new();
        let format = DisplayFormat {
            fps: 30,
            max_width: 640,
            max_height: 360,
        };
        let rx = backend.start_capture("mock-display", format).unwrap();
        let frame = rx.recv_timeout(Duration::from_millis(500)).unwrap();
        backend.stop_capture().unwrap();

        assert_eq!(frame.width, 640);
        assert_eq!(frame.height, 360);
        assert_eq!(frame.stride, 640 * 4);
        assert_eq!(frame.pixels.len(), 640 * 360 * 4);
    }

    #[test]
    fn mock_capture_frames_have_visible_gradient() {
        let backend = MockDisplayBackend::new();
        let format = DisplayFormat {
            fps: 30,
            max_width: 1280,
            max_height: 720,
        };
        let rx = backend.start_capture("mock-display", format).unwrap();
        let frame = rx.recv_timeout(Duration::from_millis(500)).unwrap();
        backend.stop_capture().unwrap();

        // Sample 100 evenly-spaced pixels. Compute variance of R values; if
        // the frame is uniform black/white the variance is ~0. A gradient
        // produces variance well above 100 (R values span 0..255 ranges).
        let stride = frame.stride;
        let mut r_values: Vec<u8> = Vec::with_capacity(100);
        for i in 0..100 {
            let x = (i * frame.width as usize / 100).min(frame.width as usize - 1);
            let y = (i * frame.height as usize / 100).min(frame.height as usize - 1);
            r_values.push(frame.pixels[y * stride + x * 4]);
        }
        let mean = r_values.iter().map(|&v| v as f32).sum::<f32>() / 100.0;
        let variance = r_values
            .iter()
            .map(|&v| (v as f32 - mean).powi(2))
            .sum::<f32>()
            / 100.0;
        assert!(
            variance > 100.0,
            "expected gradient variance > 100; got {variance}",
        );
    }

    #[test]
    fn mock_stop_display_capture_terminates_within_200ms() {
        let backend = MockDisplayBackend::new();
        let format = DisplayFormat {
            fps: 30,
            max_width: 1280,
            max_height: 720,
        };
        let _rx = backend.start_capture("mock-display", format).unwrap();
        std::thread::sleep(Duration::from_millis(50));

        let stop_start = Instant::now();
        backend.stop_capture().unwrap();
        let stop_elapsed = stop_start.elapsed();
        assert!(
            stop_elapsed < Duration::from_millis(200),
            "stop_capture took {stop_elapsed:?}",
        );
    }

    #[test]
    fn mock_double_start_display_capture_returns_err() {
        let backend = MockDisplayBackend::new();
        let format = DisplayFormat {
            fps: 30,
            max_width: 1280,
            max_height: 720,
        };
        let _rx = backend.start_capture("mock-display", format).unwrap();
        let result = backend.start_capture("mock-display", format);
        backend.stop_capture().unwrap();
        assert!(result.is_err(), "second start_capture should be Err");
    }
}
