//! Windows WASAPI output-device LOOPBACK capture for screen/game audio.
//! Confirmed against the phaseD-probe run (wasapi 0.23): default/selected RENDER
//! device, initialize_client in CAPTURE direction = loopback, autoconvert to
//! 48 kHz/2ch f32, then downmix to mono + chunk to OPUS_FRAME_SAMPLES_MONO.

use crate::audio::PcmChunk;
use crate::opus_codec::{OPUS_FRAME_SAMPLES_MONO, OPUS_SAMPLE_RATE};
use crate::screen_audio::{OutputDevice, ScreenAudioCapture};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use wasapi::*;

pub fn list_output_devices() -> Result<Vec<OutputDevice>, String> {
    initialize_mta().ok().map_err(|e| format!("COM init: {e}"))?;
    let enumerator = DeviceEnumerator::new().map_err(|e| e.to_string())?;
    let default_id = enumerator
        .get_default_device(&Direction::Render)
        .ok()
        .and_then(|d| d.get_id().ok());
    let collection = enumerator
        .get_device_collection(&Direction::Render)
        .map_err(|e| e.to_string())?;
    let n = collection.get_nbr_devices().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for i in 0..n {
        if let Ok(device) = collection.get_device_at_index(i) {
            let name = device.get_friendlyname().unwrap_or_else(|_| "<unknown>".into());
            if let Ok(id) = device.get_id() {
                let is_default = Some(&id) == default_id.as_ref();
                out.push(OutputDevice { id, name, is_default });
            }
        }
    }
    Ok(out)
}

struct WasapiCapture { stop: Arc<AtomicBool> }
impl ScreenAudioCapture for WasapiCapture {
    fn stop(&self) { self.stop.store(true, Ordering::Relaxed); }
}

pub fn start_capture(device_id: Option<String>) -> Result<(Box<dyn ScreenAudioCapture>, mpsc::Receiver<PcmChunk>), String> {
    let (tx, rx) = mpsc::channel::<PcmChunk>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = stop.clone();

    std::thread::spawn(move || {
        if let Err(e) = capture_loop(device_id, &tx, &stop_t) {
            eprintln!("[screen_audio] capture loop ended: {e}");
        }
    });
    Ok((Box::new(WasapiCapture { stop }), rx))
}

fn capture_loop(device_id: Option<String>, tx: &mpsc::Sender<PcmChunk>, stop: &AtomicBool) -> Result<(), String> {
    initialize_mta().ok().map_err(|e| format!("COM init: {e}"))?;
    let enumerator = DeviceEnumerator::new().map_err(|e| e.to_string())?;

    let device = match device_id {
        Some(id) => {
            let collection = enumerator.get_device_collection(&Direction::Render).map_err(|e| e.to_string())?;
            let n = collection.get_nbr_devices().map_err(|e| e.to_string())?;
            let mut found = None;
            for i in 0..n {
                if let Ok(d) = collection.get_device_at_index(i) {
                    if d.get_id().ok().as_deref() == Some(id.as_str()) { found = Some(d); break; }
                }
            }
            found.ok_or_else(|| format!("output device not found: {id}"))?
        }
        None => enumerator.get_default_device(&Direction::Render).map_err(|e| e.to_string())?,
    };

    let mut audio_client = device.get_iaudioclient().map_err(|e| e.to_string())?;
    let desired = WaveFormat::new(32, 32, &SampleType::Float, OPUS_SAMPLE_RATE as usize, 2, None);
    let (_def, min_time) = audio_client.get_device_period().map_err(|e| e.to_string())?;
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: min_time };
    audio_client.initialize_client(&desired, &Direction::Capture, &mode).map_err(|e| e.to_string())?;

    let capture_client = audio_client.get_audiocaptureclient().map_err(|e| e.to_string())?;
    let h_event = audio_client.set_get_eventhandle().map_err(|e| e.to_string())?;
    audio_client.start_stream().map_err(|e| e.to_string())?;

    let mut bytes: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
    let mut mono: Vec<f32> = Vec::with_capacity(OPUS_FRAME_SAMPLES_MONO);
    while !stop.load(Ordering::Relaxed) {
        capture_client.read_from_device_to_deque(&mut bytes).map_err(|e| e.to_string())?;
        while bytes.len() >= 8 {
            let mut l = [0u8; 4];
            let mut r = [0u8; 4];
            for b in l.iter_mut() { *b = bytes.pop_front().unwrap(); }
            for b in r.iter_mut() { *b = bytes.pop_front().unwrap(); }
            let s = (f32::from_le_bytes(l) + f32::from_le_bytes(r)) * 0.5;
            mono.push(s);
            if mono.len() == OPUS_FRAME_SAMPLES_MONO {
                if tx.send(PcmChunk { samples: std::mem::take(&mut mono), timestamp_ms: 0 }).is_err() {
                    return Ok(());
                }
                mono = Vec::with_capacity(OPUS_FRAME_SAMPLES_MONO);
            }
        }
        let _ = h_event.wait_for_event(200_000);
    }
    let _ = audio_client.stop_stream();
    Ok(())
}
