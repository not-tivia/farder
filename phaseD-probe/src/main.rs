//! THROWAWAY Phase D probe — WASAPI output-device LOOPBACK capture.
//!
//! Goal: prove we can capture the system's OWN output audio (what a game plays)
//! on Windows as 48 kHz float, BEFORE writing the Phase D plan. cpal 0.15 (the
//! mic path) can't do loopback, so screen-audio needs this WASAPI path.
//!
//! This version SCANS EVERY output (render) device and captures ~1.5s from each,
//! reporting the peak level — so it finds which device your sound is actually on
//! (Windows' "default" device may be an idle HDMI/AVR/surround endpoint).
//!
//! RUN IT WHILE SOMETHING IS PLAYING (music/video/a game).

#[cfg(not(windows))]
fn main() {
    eprintln!("This probe is Windows-only (WASAPI loopback). Run it on the Windows box.");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    if let Err(e) = run() {
        eprintln!("\nPROBE FAILED: {e}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use wasapi::*;

    initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;

    // Remember the default render device's id so we can mark it in the list.
    let default_id = enumerator
        .get_default_device(&Direction::Render)
        .ok()
        .and_then(|d| d.get_id().ok());

    let collection = enumerator.get_device_collection(&Direction::Render)?;
    let n = collection.get_nbr_devices()?;
    println!("Found {n} output (render) device(s). Capturing ~1.5s from each.");
    println!("Make sure audio is PLAYING now.\n");

    let want_rate: usize = 48_000;
    let want_channels: usize = 2;

    use std::io::Write;
    for i in 0..n {
        let device = match collection.get_device_at_index(i) {
            Ok(d) => d,
            Err(e) => {
                println!("[{i}] <error opening device: {e}>");
                continue;
            }
        };
        let name = device.get_friendlyname().unwrap_or_else(|_| "<unknown>".into());
        let is_default = device.get_id().ok().as_deref() == default_id.as_deref();
        let tag = if is_default { " (DEFAULT)" } else { "" };

        // Print BEFORE capturing so progress is visible and a wedged device is
        // obvious (it'll be the last name printed).
        print!("[{i}]{tag} {name} ... ");
        let _ = std::io::stdout().flush();

        match capture_peak(&device, want_rate, want_channels) {
            Ok((frames, peak, native)) => {
                let verdict = if peak > 0.0001 { "<== AUDIO IS HERE" } else { "(silent)" };
                println!("native {native} | {frames} frames | peak {peak:.5}  {verdict}");
            }
            Err(e) => println!("(could not capture: {e})"),
        }
    }

    println!("\nPROBE OK: wasapi 0.23 loopback builds + runs. The line marked");
    println!("'<== AUDIO IS HERE' is the device your sound plays on.");
    println!("Paste this whole output back.");
    Ok(())
}

/// Open one render device in loopback (Capture direction), capture ~1.5s at
/// 48 kHz/2ch f32 (autoconvert), and return (frames, peak_amplitude, native_fmt).
#[cfg(windows)]
fn capture_peak(
    device: &wasapi::Device,
    want_rate: usize,
    want_channels: usize,
) -> Result<(u64, f32, String), Box<dyn std::error::Error>> {
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};
    use wasapi::*;

    let mut audio_client = device.get_iaudioclient()?;
    let mix = audio_client.get_mixformat()?;
    let native = format!(
        "{} Hz/{} ch/{} bit",
        mix.get_samplespersec(),
        mix.get_nchannels(),
        mix.get_bitspersample(),
    );

    let desired = WaveFormat::new(32, 32, &SampleType::Float, want_rate, want_channels, None);
    let (_def_time, min_time) = audio_client.get_device_period()?;
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: min_time };
    audio_client.initialize_client(&desired, &Direction::Capture, &mode)?;

    let capture_client = audio_client.get_audiocaptureclient()?;
    let h_event = audio_client.set_get_eventhandle()?;
    audio_client.start_stream()?;

    let mut bytes: VecDeque<u8> = VecDeque::new();
    let mut frames: u64 = 0;
    let mut peak: f32 = 0.0;

    // HARD wall-clock cap: ~1.2s per device no matter what. Some phantom/idle
    // endpoints fire buffer-ready events but never deliver samples — capping on
    // real time (not on a frame target) guarantees we always move on.
    let deadline = Instant::now() + Duration::from_millis(1200);
    while Instant::now() < deadline {
        capture_client.read_from_device_to_deque(&mut bytes)?;
        let mut chunk = [0u8; 4];
        let mut samples = 0usize;
        while bytes.len() >= 4 {
            for b in chunk.iter_mut() {
                *b = bytes.pop_front().unwrap();
            }
            let s = f32::from_le_bytes(chunk).abs();
            if s > peak {
                peak = s;
            }
            samples += 1;
        }
        frames += (samples as u64) / (want_channels as u64);
        // Short wait (~100ms) so we poll and re-check the deadline promptly.
        let _ = h_event.wait_for_event(1_000_000);
    }
    audio_client.stop_stream()?;
    Ok((frames, peak, native))
}
