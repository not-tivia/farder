//! Local screenshare PREVIEW (Phase B loopback): capture → H.264 encode →
//! emit each encoded frame to the webview, which decodes it via WebCodecs and
//! paints a canvas. No networking — this proves the capture/codec/decode path
//! end to end on one machine.

use crate::display::{make_display_backend, DisplayBackend, DisplayFormat, VideoFrame};
use crate::video_encoder::{EncodedFrame, H264Encoder};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Largest (even) `WxH` that fits `(sw, sh)` within `(max_w, max_h)` keeping the
/// aspect ratio. NEVER upscales (returns the source dims unchanged when it
/// already fits). Downscaled dims are forced even (H.264 / YUV420 requires it).
pub fn fit_dims(sw: u32, sh: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if sw == 0 || sh == 0 || (sw <= max_w && sh <= max_h) {
        return (sw, sh);
    }
    let scale = f32::min(max_w as f32 / sw as f32, max_h as f32 / sh as f32);
    let dw = (((sw as f32 * scale).round() as u32).max(2)) & !1;
    let dh = (((sh as f32 * scale).round() as u32).max(2)) & !1;
    (dw, dh)
}

/// Nearest-neighbour downscale of a packed-RGBA `VideoFrame` to `dw x dh`.
/// Cheap (no filtering) — fine for a live screen share and far cheaper than
/// encoding native resolution on a software encoder.
pub fn downscale_nearest(frame: &VideoFrame, dw: u32, dh: u32) -> VideoFrame {
    let (sw, sh) = (frame.width, frame.height);
    let mut pixels = vec![0u8; dw as usize * dh as usize * 4];
    for dy in 0..dh {
        let sy = (((dy as u64 * sh as u64) / dh as u64) as u32).min(sh.saturating_sub(1));
        for dx in 0..dw {
            let sx = (((dx as u64 * sw as u64) / dw as u64) as u32).min(sw.saturating_sub(1));
            let si = sy as usize * frame.stride + sx as usize * 4;
            let di = (dy as usize * dw as usize + dx as usize) * 4;
            pixels[di..di + 4].copy_from_slice(&frame.pixels[si..si + 4]);
        }
    }
    VideoFrame { width: dw, height: dh, stride: dw as usize * 4, pixels, timestamp_ms: frame.timestamp_ms }
}

/// The capture→encode loop. Factored to take a sink callback so it's testable
/// without a Tauri runtime, and to construct the encoder ON THIS THREAD (the
/// openh264 Encoder is !Send and must never cross a thread boundary). Forces a
/// keyframe first; downscales each frame to fit `(max_width, max_height)` and
/// caps the encode rate to ~`target_fps` (a CPU encoder can't keep up with a
/// 1440p/4K monitor at the source's native frame rate). Encode errors drop that
/// frame (live policy).
#[allow(clippy::too_many_arguments)]
pub fn run_encode_loop(
    rx: Receiver<VideoFrame>,
    mut encoder: H264Encoder,
    stop: Arc<AtomicBool>,
    force_keyframe: Arc<AtomicBool>,
    target_fps: u32,
    max_width: u32,
    max_height: u32,
    mut sink: impl FnMut(EncodedFrame),
) {
    encoder.force_keyframe(); // first frame is always a keyframe
    let min_interval = Duration::from_millis(1000 / target_fps.max(1) as u64);
    let mut last_encode: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        let frame = match rx.recv() {
            Ok(f) => f,
            Err(_) => break, // capture ended
        };
        // A viewer joined mid-stream: emit a fresh IDR so they can start.
        let want_keyframe = force_keyframe.swap(false, Ordering::AcqRel);
        // Rate-cap: the source can deliver far faster than target_fps (e.g. a
        // 144 Hz game). Drop frames that arrive too soon so we don't grind the
        // CPU encoder on frames we'd never want. A forced keyframe always goes.
        let now = Instant::now();
        if !want_keyframe {
            if let Some(prev) = last_encode {
                if now.duration_since(prev) < min_interval {
                    continue;
                }
            }
        }
        last_encode = Some(now);
        if want_keyframe {
            encoder.force_keyframe();
        }
        // Downscale to the target before encoding (native res is too heavy).
        let (dw, dh) = fit_dims(frame.width, frame.height, max_width, max_height);
        let result = if dw == frame.width && dh == frame.height {
            encoder.encode(&frame)
        } else {
            encoder.encode(&downscale_nearest(&frame, dw, dh))
        };
        match result {
            Ok(encoded) => sink(encoded),
            Err(e) => eprintln!("[screenshare] encode dropped a frame: {e}"),
        }
    }
}

// --- live preview state (one at a time) -------------------------------------

struct ActivePreview {
    stop: Arc<AtomicBool>,
    backend: Box<dyn DisplayBackend>,
}

fn active() -> &'static Mutex<Option<ActivePreview>> {
    static ACTIVE: OnceLock<Mutex<Option<ActivePreview>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(None))
}

#[tauri::command]
pub async fn start_screenshare_preview(
    app: AppHandle,
    fps: u32,
    max_width: u32,
    max_height: u32,
) -> Result<(), String> {
    {
        let guard = active().lock().map_err(|e| e.to_string())?;
        if guard.is_some() {
            return Err("a screenshare preview is already running".into());
        }
    }
    // Fail-fast: validate the encoder can init before we start capturing.
    // (The actual encoder is constructed INSIDE the thread below — it's !Send.)
    drop(H264Encoder::new(30, 3_000_000)?);

    let backend = make_display_backend();
    let sources = backend.enumerate_sources()?;
    let source_id = sources.first().map(|s| s.id.clone()).ok_or("no capture source")?;
    let format = DisplayFormat { fps, max_width, max_height };
    let rx = backend.start_capture(&source_id, format)?;
    let stop = Arc::new(AtomicBool::new(false));

    {
        let mut guard = active().lock().map_err(|e| e.to_string())?;
        *guard = Some(ActivePreview { stop: stop.clone(), backend });
    }

    let app_for_loop = app.clone();
    std::thread::spawn(move || {
        // Encoder built HERE (never crosses a thread boundary — !Send).
        let encoder = match H264Encoder::new(fps, 3_000_000) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[screenshare] encoder init failed in thread: {e}");
                return;
            }
        };
        run_encode_loop(rx, encoder, stop, Arc::new(AtomicBool::new(false)), fps, max_width, max_height, move |enc| {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&enc.data);
            let _ = app_for_loop.emit(
                "screenshare:frame",
                serde_json::json!({ "data": b64, "key": enc.is_keyframe, "ts": enc.timestamp_ms }),
            );
        });
    });
    Ok(())
}

#[tauri::command]
pub async fn stop_screenshare_preview() -> Result<(), String> {
    let preview = {
        let mut guard = active().lock().map_err(|e| e.to_string())?;
        guard.take()
    };
    if let Some(p) = preview {
        p.stop.store(true, Ordering::Relaxed);
        p.backend.stop_capture()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::MockDisplayBackend;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn mock_capture_through_encoder_yields_decodable_keyframe_first() {
        use openh264::decoder::Decoder;
        use openh264::nal_units;

        // Mock backend → real encoder → collect encoded frames via the sink.
        let backend = MockDisplayBackend::new();
        let rx = backend
            .start_capture("mock-display", DisplayFormat { fps: 30, max_width: 320, max_height: 240 })
            .unwrap();
        let encoder = H264Encoder::new(30, 3_000_000).unwrap();
        let stop = Arc::new(AtomicBool::new(false));

        let count = Arc::new(AtomicUsize::new(0));
        let collected = Arc::new(Mutex::new(Vec::<EncodedFrame>::new()));
        let stop_for_sink = stop.clone();
        let count_for_sink = count.clone();
        let collected_for_sink = collected.clone();
        run_encode_loop(rx, encoder, stop.clone(), Arc::new(AtomicBool::new(false)), 100000, 9999, 9999, move |enc| {
            collected_for_sink.lock().unwrap().push(enc);
            if count_for_sink.fetch_add(1, Ordering::Relaxed) + 1 >= 5 {
                stop_for_sink.store(true, Ordering::Relaxed);
            }
        });
        backend.stop_capture().unwrap();

        let frames = collected.lock().unwrap();
        assert!(frames.len() >= 5, "expected >=5 encoded frames, got {}", frames.len());
        assert!(frames[0].is_keyframe, "first frame must be a keyframe");

        let mut dec = Decoder::new().unwrap();
        let mut decoded_any = false;
        for f in frames.iter() {
            for nal in nal_units(&f.data) {
                if let Ok(Some(_)) = dec.decode(nal) {
                    decoded_any = true;
                }
            }
        }
        assert!(decoded_any, "encoded preview frames must decode");
    }

    #[test]
    fn force_keyframe_flag_injects_a_midstream_keyframe() {
        let backend = MockDisplayBackend::new();
        let rx = backend
            .start_capture("mock-display", DisplayFormat { fps: 30, max_width: 160, max_height: 120 })
            .unwrap();
        let encoder = H264Encoder::new(30, 3_000_000).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let force = Arc::new(AtomicBool::new(false));

        let count = Arc::new(AtomicUsize::new(0));
        let keyframe_after_force = Arc::new(AtomicBool::new(false));
        let stop_s = stop.clone();
        let force_s = force.clone();
        let count_s = count.clone();
        let kaf = keyframe_after_force.clone();
        run_encode_loop(rx, encoder, stop.clone(), force.clone(), 100000, 9999, 9999, move |enc| {
            let n = count_s.fetch_add(1, Ordering::Relaxed);
            // After a few frames, request a keyframe; the NEXT frame must be one.
            if n == 3 { force_s.store(true, Ordering::Relaxed); }
            if n == 4 && enc.is_keyframe { kaf.store(true, Ordering::Relaxed); }
            if n + 1 >= 6 { stop_s.store(true, Ordering::Relaxed); }
        });
        backend.stop_capture().unwrap();
        assert!(keyframe_after_force.load(Ordering::Relaxed), "frame after force flag must be a keyframe");
    }

    #[test]
    fn no_force_flag_keeps_midstream_frames_as_deltas() {
        let backend = MockDisplayBackend::new();
        let rx = backend
            .start_capture("mock-display", DisplayFormat { fps: 30, max_width: 160, max_height: 120 })
            .unwrap();
        let encoder = H264Encoder::new(30, 3_000_000).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let force = Arc::new(AtomicBool::new(false)); // never set

        let count = Arc::new(AtomicUsize::new(0));
        let midstream_was_key = Arc::new(AtomicBool::new(false));
        let stop_s = stop.clone();
        let count_s = count.clone();
        let mwk = midstream_was_key.clone();
        run_encode_loop(rx, encoder, stop.clone(), force.clone(), 100000, 9999, 9999, move |enc| {
            let n = count_s.fetch_add(1, Ordering::Relaxed);
            if n == 4 && enc.is_keyframe { mwk.store(true, Ordering::Relaxed); }
            if n + 1 >= 6 { stop_s.store(true, Ordering::Relaxed); }
        });
        backend.stop_capture().unwrap();
        assert!(!midstream_was_key.load(Ordering::Relaxed), "without the force flag, frame 4 must be a delta, not a keyframe");
    }

    #[test]
    fn fit_dims_caps_oversize_and_passes_through_small() {
        // Already within bounds → unchanged.
        assert_eq!(fit_dims(1280, 720, 1920, 1080), (1280, 720));
        // 1440p capped to a 720p box, 16:9 preserved, even dims.
        assert_eq!(fit_dims(2560, 1440, 1280, 720), (1280, 720));
        // 4K capped to 1080p.
        assert_eq!(fit_dims(3840, 2160, 1920, 1080), (1920, 1080));
        // Portrait window bounded by height; width even and within bound.
        let (w, h) = fit_dims(1080, 1920, 1280, 720);
        assert_eq!(h, 720);
        assert!(w <= 1280 && w % 2 == 0, "got {w}");
    }

    #[test]
    fn downscale_nearest_produces_a_packed_target_frame() {
        // 4x4 solid red → 2x2; output is packed RGBA at the target size.
        let pixels = [255u8, 0, 0, 255].repeat(4 * 4);
        let frame = VideoFrame { width: 4, height: 4, stride: 16, pixels, timestamp_ms: 7 };
        let out = downscale_nearest(&frame, 2, 2);
        assert_eq!((out.width, out.height), (2, 2));
        assert_eq!(out.stride, 8);
        assert_eq!(out.pixels.len(), 2 * 2 * 4);
        assert_eq!(out.timestamp_ms, 7);
        assert_eq!(&out.pixels[..4], &[255, 0, 0, 255], "sampled pixel stays red");
    }
}
