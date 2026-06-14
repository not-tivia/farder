//! THROWAWAY Phase D probe — WASAPI output-device LOOPBACK capture.
//!
//! Goal: prove we can capture the system's OWN output audio (what a game plays)
//! on Windows as 48 kHz float, BEFORE writing the Phase D plan. cpal 0.15 (the
//! mic path) can't do loopback, so screen-audio needs this WASAPI path.
//!
//! It scans EVERY output (render) device and captures briefly from each,
//! reporting the peak level — so it finds which device your sound is actually
//! on. Each device is probed on a throwaway worker thread with a timeout, so a
//! wedged/virtual endpoint (e.g. a Steam virtual speaker) is skipped instead of
//! freezing the whole probe.
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
    // Hard-exit so any detached, wedged worker thread can't keep us alive.
    std::process::exit(0);
}

#[cfg(windows)]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use std::sync::mpsc;
    use std::time::Duration;
    use wasapi::*;

    initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;
    let default_id = enumerator
        .get_default_device(&Direction::Render)
        .ok()
        .and_then(|d| d.get_id().ok());

    let collection = enumerator.get_device_collection(&Direction::Render)?;
    let n = collection.get_nbr_devices()?;
    println!("Found {n} output (render) device(s). Probing each (max ~2.5s each).");
    println!("Make sure audio is PLAYING now.\n");

    for i in 0..n {
        // Read the name/id in the MAIN thread (quick, safe).
        let (name, id) = match collection.get_device_at_index(i) {
            Ok(d) => (
                d.get_friendlyname().unwrap_or_else(|_| "<unknown>".into()),
                d.get_id().ok(),
            ),
            Err(e) => {
                println!("[{i}] <error: {e}>");
                continue;
            }
        };
        let tag = if id.as_deref() == default_id.as_deref() { " (DEFAULT)" } else { "" };
        print!("[{i}]{tag} {name} ... ");
        let _ = std::io::stdout().flush();

        // Capture on a worker thread so a wedged device can't freeze us.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(capture_index(i));
        });
        match rx.recv_timeout(Duration::from_millis(2500)) {
            Ok(Ok((frames, peak, native))) => {
                let verdict = if peak > 0.0001 { "<== AUDIO IS HERE" } else { "(silent)" };
                println!("native {native} | {frames} frames | peak {peak:.5}  {verdict}");
            }
            Ok(Err(e)) => println!("(could not capture: {e})"),
            Err(_) => println!("(timed out / device wedged — skipped)"),
        }
    }

    println!("\nPROBE OK: wasapi 0.23 loopback builds + runs. The line marked");
    println!("'<== AUDIO IS HERE' is the device your sound plays on.");
    println!("Paste this whole output back.");
    Ok(())
}

/// Worker-thread entry: own COM init, re-enumerate, capture device `i`.
/// Returns a String error (Send) so it can cross the channel.
#[cfg(windows)]
fn capture_index(i: u32) -> Result<(u64, f32, String), String> {
    capture_index_inner(i).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn capture_index_inner(i: u32) -> Result<(u64, f32, String), Box<dyn std::error::Error>> {
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};
    use wasapi::*;

    initialize_mta().ok()?; // COM for THIS thread
    let enumerator = DeviceEnumerator::new()?;
    let collection = enumerator.get_device_collection(&Direction::Render)?;
    let device = collection.get_device_at_index(i)?;

    let mut audio_client = device.get_iaudioclient()?;
    let mix = audio_client.get_mixformat()?;
    let native = format!(
        "{} Hz/{} ch/{} bit",
        mix.get_samplespersec(),
        mix.get_nchannels(),
        mix.get_bitspersample(),
    );

    let want_rate: usize = 48_000;
    let want_channels: usize = 2;
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
        let _ = h_event.wait_for_event(1_000_000); // ~100ms poll
    }
    audio_client.stop_stream()?;
    Ok((frames, peak, native))
}
