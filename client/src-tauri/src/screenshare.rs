//! Local screenshare PREVIEW (Phase B loopback): capture → H.264 encode →
//! emit each encoded frame to the webview, which decodes it via WebCodecs and
//! paints a canvas. No networking — this proves the capture/codec/decode path
//! end to end on one machine.

use crate::display::{make_display_backend, DisplayBackend, DisplayFormat, VideoFrame};
use crate::video_encoder::{EncodedFrame, H264Encoder};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, Emitter};

/// The capture→encode loop. Factored to take a sink callback so it's testable
/// without a Tauri runtime, and to construct the encoder ON THIS THREAD (the
/// openh264 Encoder is !Send and must never cross a thread boundary). Forces a
/// keyframe first, then encodes every frame the backend delivers until `stop`
/// is set or the channel closes. Encode errors drop that frame (live policy).
pub fn run_encode_loop(
    rx: Receiver<VideoFrame>,
    mut encoder: H264Encoder,
    stop: Arc<AtomicBool>,
    mut sink: impl FnMut(EncodedFrame),
) {
    encoder.force_keyframe();
    while !stop.load(Ordering::Relaxed) {
        let frame = match rx.recv() {
            Ok(f) => f,
            Err(_) => break, // capture ended
        };
        match encoder.encode(&frame) {
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
    drop(H264Encoder::new()?);

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
        let encoder = match H264Encoder::new() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[screenshare] encoder init failed in thread: {e}");
                return;
            }
        };
        run_encode_loop(rx, encoder, stop, move |enc| {
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
        let encoder = H264Encoder::new().unwrap();
        let stop = Arc::new(AtomicBool::new(false));

        let count = Arc::new(AtomicUsize::new(0));
        let collected = Arc::new(Mutex::new(Vec::<EncodedFrame>::new()));
        let stop_for_sink = stop.clone();
        let count_for_sink = count.clone();
        let collected_for_sink = collected.clone();
        run_encode_loop(rx, encoder, stop.clone(), move |enc| {
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
}
