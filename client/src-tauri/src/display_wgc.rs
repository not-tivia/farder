//! Real screen capture via Windows Graphics Capture (`windows-capture`).
//! Implements the DisplayBackend seam; delivers packed RGBA VideoFrames.
//! Windows-only — the mock backend covers every other host (incl. Linux CI).
#![cfg(windows)]

use crate::display::{DisplayBackend, DisplayFormat, DisplaySource, DisplaySourceKind, VideoFrame};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

/// Flags handed to the capture handler: the frame sink + a stop flag + the
/// capture start instant (for monotonic timestamps).
struct CaptureFlags {
    sink: SyncSender<VideoFrame>,
    stop: Arc<AtomicBool>,
    started: Instant,
}

struct FrameHandler {
    sink: SyncSender<VideoFrame>,
    stop: Arc<AtomicBool>,
    started: Instant,
}

impl GraphicsCaptureApiHandler for FrameHandler {
    type Flags = CaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { sink: ctx.flags.sink, stop: ctx.flags.stop, started: ctx.flags.started })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self.stop.load(Ordering::Relaxed) {
            capture_control.stop();
            return Ok(());
        }
        let width = frame.width();
        let height = frame.height();
        // CONFIRM the exact buffer accessor against windows-capture 2.0.0 docs:
        // we need PACKED RGBA (stride == width*4). The nopadding accessor gives
        // that; if only a padded buffer is available, copy row-by-row dropping
        // the pad. `as_raw_nopadding_buffer()` is the expected name.
        let mut buffer = frame.buffer()?;
        let raw: &[u8] = buffer.as_raw_nopadding_buffer()?;
        let packed_len = (width * height * 4) as usize;
        if raw.len() < packed_len {
            return Ok(()); // short buffer — skip this frame defensively
        }
        let vf = VideoFrame {
            width,
            height,
            stride: (width * 4) as usize,
            pixels: raw[..packed_len].to_vec(),
            timestamp_ms: self.started.elapsed().as_millis() as u64,
        };
        // Non-blocking: if the encoder is behind, drop the frame (live policy).
        let _ = self.sink.try_send(vf);
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub struct WgcDisplayBackend {
    stop: Mutex<Option<Arc<AtomicBool>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl WgcDisplayBackend {
    pub fn new() -> Self {
        Self { stop: Mutex::new(None), thread: Mutex::new(None) }
    }
}

impl Default for WgcDisplayBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayBackend for WgcDisplayBackend {
    fn enumerate_sources(&self) -> Result<Vec<DisplaySource>, String> {
        let monitors = Monitor::enumerate().map_err(|e| format!("enumerate monitors: {e}"))?;
        let mut out = Vec::new();
        for (i, m) in monitors.into_iter().enumerate() {
            let label = m
                .name()
                .map(|n| format!("Display {}: {}", i + 1, n))
                .unwrap_or_else(|_| format!("Display {}", i + 1));
            let width = m.width().unwrap_or(0);
            let height = m.height().unwrap_or(0);
            out.push(DisplaySource {
                id: format!("monitor:{}", i + 1), // 1-based index → Monitor::from_index
                kind: DisplaySourceKind::Screen,
                label,
                width,
                height,
            });
        }
        Ok(out)
    }

    fn start_capture(
        &self,
        source_id: &str,
        format: DisplayFormat,
    ) -> Result<mpsc::Receiver<VideoFrame>, String> {
        if format.fps == 0 {
            return Err("invalid DisplayFormat: fps=0".into());
        }
        let mut thread_slot = self.thread.lock().map_err(|e| e.to_string())?;
        if thread_slot.is_some() {
            return Err("capture already active".into());
        }
        let idx: u32 = source_id
            .strip_prefix("monitor:")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("bad source_id: {source_id}"))?;
        let monitor = Monitor::from_index(idx as usize).map_err(|e| format!("monitor {idx}: {e}"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel::<VideoFrame>(4); // 4 frames of slack
        let flags = CaptureFlags { sink: tx, stop: stop.clone(), started: Instant::now() };

        let settings = Settings::new(
            monitor,
            CursorCaptureSettings::Default,
            DrawBorderSettings::Default,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Rgba8,
            flags,
        );

        let handle = std::thread::spawn(move || {
            if let Err(e) = FrameHandler::start(settings) {
                eprintln!("[display_wgc] capture ended: {e:?}");
            }
        });

        *thread_slot = Some(handle);
        *self.stop.lock().map_err(|e| e.to_string())? = Some(stop);
        Ok(rx)
    }

    fn stop_capture(&self) -> Result<(), String> {
        if let Some(stop) = self.stop.lock().map_err(|e| e.to_string())?.take() {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(handle) = self.thread.lock().map_err(|e| e.to_string())?.take() {
            let _ = handle.join();
        }
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "wgc"
    }
}
