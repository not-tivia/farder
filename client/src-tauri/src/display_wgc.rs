//! Real screen capture via Windows Graphics Capture (`windows-capture`).
//! Implements the DisplayBackend seam; delivers packed RGBA VideoFrames.
//! Windows-only — the mock backend covers every other host (incl. Linux CI).
#![cfg(windows)]

use crate::display::{DisplayBackend, DisplayFormat, DisplaySource, DisplaySourceKind, VideoFrame};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::window::Window;
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
        // Belt-and-braces stop check: WGC may still deliver one or two frames
        // after stop_capture returns (in-flight delivery). The primary stop
        // mechanism is CaptureControl::stop() called from outside; this flag
        // catch handles any such stragglers.
        if self.stop.load(Ordering::Relaxed) {
            capture_control.stop();
            return Ok(());
        }
        let width = frame.width();
        let height = frame.height();
        // We need PACKED RGBA (stride == width*4). windows-capture 2.0.0's
        // `as_nopadding_buffer(&mut Vec<u8>) -> &[u8]` gives exactly that: it
        // returns the frame's internal slice when already unpadded, otherwise
        // de-pads row-by-row into the scratch Vec we pass. Either way `raw` is
        // packed RGBA. (NOTE: a fresh scratch per frame; reuse via a field on
        // self is a possible later optimization.)
        let buffer = frame.buffer()?;
        let mut nopad: Vec<u8> = Vec::new();
        let raw: &[u8] = buffer.as_nopadding_buffer(&mut nopad);
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

/// Captures at the monitor's native resolution; DisplayFormat.max_width/max_height
/// are advisory in Phase B (the drop-if-full frame channel bounds throughput).
/// Downscaling to the target size is a Phase C concern.
//
// DESIGN NOTE — why start_free_threaded (external CaptureControl) rather than
// a flag-in-frame pattern:
//
// WGC only delivers on_frame_arrived callbacks when the screen content changes.
// On a static/idle screen no frames are delivered, so a stop flag checked
// inside on_frame_arrived is NEVER seen — stop_capture would block forever on
// join(), and the WGC session would keep running (privacy bug: "stop" doesn't
// stop). start_free_threaded returns a CaptureControl handle that can tear down
// the session from OUTSIDE the frame callback, eliminating the static-screen
// hang entirely. The AtomicBool flag is kept only as belt-and-braces for
// in-flight frames that arrive after control.stop() is called.
//
// OWNER-CONFIRM (Windows only — cannot verify on Linux):
//   - CaptureControl<FrameHandler, <FrameHandler as GraphicsCaptureApiHandler>::Error>
//     is the concrete type; check windows-capture 2.0.0 for exact generics.
//   - FrameHandler::start_free_threaded(settings) -> Result<CaptureControl<...>, Error>
//   - CaptureControl::stop() -> Result<(), Error>  (stops session + joins thread)
//   - frame.buffer() / buffer.as_nopadding_buffer(&mut Vec<u8>) accessor names
//   - Monitor::enumerate(), Monitor::from_index(usize) (1-based, see below)
//   - Monitor::name(), Monitor::width(), Monitor::height()
pub struct WgcDisplayBackend {
    stop: Mutex<Option<Arc<AtomicBool>>>,
    /// External capture control returned by start_free_threaded. Stored so
    /// stop_capture can tear down the WGC session without relying on frame
    /// delivery (which WGC halts on a static screen — see DESIGN NOTE above).
    ///
    // OWNER-CONFIRM: exact generic params of CaptureControl on Windows.
    control: Mutex<Option<CaptureControl<FrameHandler, <FrameHandler as GraphicsCaptureApiHandler>::Error>>>,
    started: Mutex<Option<Instant>>,
}

impl WgcDisplayBackend {
    pub fn new() -> Self {
        Self {
            stop: Mutex::new(None),
            control: Mutex::new(None),
            started: Mutex::new(None),
        }
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
                // 1-based: windows-capture Monitor::from_index expects a 1-based index (confirm on Windows).
                id: format!("monitor:{}", i + 1),
                kind: DisplaySourceKind::Screen,
                label,
                width,
                height,
            });
        }
        // Windows (single-window capture). Skip empty-title + tiny/tray windows
        // (the probe showed explorer.exe "" and 160x28 entries — junk).
        if let Ok(windows) = Window::enumerate() {
            for (i, w) in windows.into_iter().enumerate() {
                let title = match w.title() {
                    Ok(t) if !t.trim().is_empty() => t,
                    _ => continue,
                };
                let width = w.width().unwrap_or(0).max(0) as u32;
                let height = w.height().unwrap_or(0).max(0) as u32;
                if width < 100 || height < 100 {
                    continue; // tray/hidden window, not shareable
                }
                out.push(DisplaySource {
                    id: format!("window:{}", i),
                    kind: DisplaySourceKind::Window,
                    label: format!("\u{1FA9F} {}", title), // window icon prefix
                    width,
                    height,
                });
            }
        }
        Ok(out)
    }

    fn start_capture(
        &self,
        source_id: &str,
        format: DisplayFormat,
    ) -> Result<mpsc::Receiver<VideoFrame>, String> {
        // Validate format fields — fps=0 or zero dims are caller bugs.
        if format.fps == 0 || format.max_width == 0 || format.max_height == 0 {
            return Err(format!("invalid DisplayFormat: {:?}", format));
        }

        let mut control_slot = self.control.lock().map_err(|e| e.to_string())?;
        if control_slot.is_some() {
            return Err("capture already active".into());
        }

        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel::<VideoFrame>(4); // 4 frames of slack
        let now = Instant::now();
        let flags = CaptureFlags { sink: tx, stop: stop.clone(), started: now };

        // start_free_threaded spawns the WGC capture thread and returns a
        // CaptureControl handle immediately. This lets stop_capture call
        // control.stop() from outside the frame callback — the only reliable
        // way to halt WGC when the screen is static (no frames → flag never
        // seen → join blocks forever). See DESIGN NOTE on the struct.
        //
        // OWNER-CONFIRM: FrameHandler::start_free_threaded exists in
        // windows-capture 2.0.0 and has this signature. Monitor::from_index is
        // 1-based; Window satisfies the same capture-item trait as Monitor.
        let control = if let Some(idx) = source_id.strip_prefix("monitor:") {
            let idx: u32 = idx.parse().map_err(|_| format!("bad source_id: {source_id}"))?;
            if idx == 0 { return Err("monitor index must be >= 1".into()); }
            let monitor = Monitor::from_index(idx as usize).map_err(|e| format!("monitor {idx}: {e}"))?;
            let settings = Settings::new(
                monitor,
                CursorCaptureSettings::Default, DrawBorderSettings::Default,
                SecondaryWindowSettings::Default, MinimumUpdateIntervalSettings::Default,
                DirtyRegionSettings::Default, ColorFormat::Rgba8, flags,
            );
            FrameHandler::start_free_threaded(settings).map_err(|e| format!("start capture: {e:?}"))?
        } else if let Some(idx) = source_id.strip_prefix("window:") {
            let idx: usize = idx.parse().map_err(|_| format!("bad source_id: {source_id}"))?;
            let window = Window::enumerate().map_err(|e| format!("enumerate windows: {e}"))?
                .into_iter().nth(idx)
                .ok_or_else(|| "that window is no longer available \u{2014} refresh".to_string())?;
            let settings = Settings::new(
                window,
                CursorCaptureSettings::Default, DrawBorderSettings::Default,
                SecondaryWindowSettings::Default, MinimumUpdateIntervalSettings::Default,
                DirtyRegionSettings::Default, ColorFormat::Rgba8, flags,
            );
            FrameHandler::start_free_threaded(settings).map_err(|e| format!("start capture: {e:?}"))?
        } else {
            return Err(format!("bad source_id: {source_id}"));
        };

        *control_slot = Some(control);
        *self.stop.lock().map_err(|e| e.to_string())? = Some(stop);
        *self.started.lock().map_err(|e| e.to_string())? = Some(now);
        Ok(rx)
    }

    fn stop_capture(&self) -> Result<(), String> {
        // Set the belt-and-braces flag first so any in-flight on_frame_arrived
        // callbacks also bail quickly once they see it.
        if let Some(stop) = self.stop.lock().map_err(|e| e.to_string())?.take() {
            stop.store(true, Ordering::Relaxed);
        }
        // Primary stop: tell WGC to end the session and join its thread.
        // This works even when the screen is static (no frames delivered).
        // OWNER-CONFIRM: CaptureControl::stop() stops the session and joins.
        if let Some(control) = self.control.lock().map_err(|e| e.to_string())?.take() {
            control.stop().map_err(|e| format!("stop capture: {e:?}"))?;
        }
        *self.started.lock().map_err(|e| e.to_string())? = None;
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "wgc"
    }
}
