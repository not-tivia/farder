//! THROWAWAY Phase D probe — WASAPI output-device LOOPBACK capture.
//!
//! Goal: prove that we can capture the system's OWN output audio (what a game
//! plays) on Windows, get it as 48 kHz float, and confirm it builds against the
//! `wasapi` crate's current API — BEFORE writing the Phase D plan. cpal 0.15
//! (what Farder uses for the mic) does not expose loopback, so screen-audio
//! needs this WASAPI path.
//!
//! What it does: opens the DEFAULT RENDER (playback) device, initialises an
//! audio client in CAPTURE direction on it (= loopback), requests 48 kHz stereo
//! f32 with autoconvert, captures ~3 seconds, and prints the device's native
//! mix format + how many frames it got + the peak amplitude (so you can see it
//! captured real, non-silent audio).
//!
//! RUN IT WHILE SOMETHING IS PLAYING (music/video/a game) so the peak is > 0.

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
    use std::collections::VecDeque;
    use wasapi::*;

    // COM init (multithreaded apartment). initialize_mta() returns an HRESULT;
    // .ok() turns it into a Result we can propagate.
    initialize_mta().ok()?;

    // The loopback trick: take the default RENDER device, but initialise its
    // audio client in CAPTURE direction → WASAPI hands us what is being played.
    let enumerator = DeviceEnumerator::new()?;
    let device = enumerator.get_default_device(&Direction::Render)?;

    let mut audio_client = device.get_iaudioclient()?;

    // Report the device's NATIVE mix format (what we'd be converting from).
    let mix = audio_client.get_mixformat()?;
    println!(
        "Native mix format: {} Hz, {} ch, {} bits/sample (blockalign {})",
        mix.get_samplespersec(),
        mix.get_nchannels(),
        mix.get_bitspersample(),
        mix.get_blockalign(),
    );

    // What we actually want for the Opus path: 48 kHz float. Ask for 48k/2ch/f32
    // and let WASAPI autoconvert from the native mix format. (We'll downmix
    // stereo->mono in Rust in the real pipeline.)
    let want_rate: usize = 48_000;
    let want_channels: usize = 2;
    let desired = WaveFormat::new(32, 32, &SampleType::Float, want_rate, want_channels, None);

    let (_def_time, min_time) = audio_client.get_device_period()?;
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: min_time,
    };
    audio_client.initialize_client(&desired, &Direction::Capture, &mode)?;
    println!("Initialised loopback capture at {want_rate} Hz, {want_channels} ch, f32 (autoconvert on).");

    let capture_client = audio_client.get_audiocaptureclient()?;
    let h_event = audio_client.set_get_eventhandle()?;
    audio_client.start_stream()?;

    let mut bytes: VecDeque<u8> = VecDeque::new();
    let mut total_frames: u64 = 0;
    let mut peak: f32 = 0.0;
    let frames_target: u64 = (want_rate as u64) * 3; // ~3 seconds

    println!("Capturing ~3s of system output... (play some audio now)");
    while total_frames < frames_target {
        // Drain whatever WASAPI has into our byte deque.
        capture_client.read_from_device_to_deque(&mut bytes)?;

        // Interpret as interleaved f32; track peak + count frames, then clear.
        let mut chunk = [0u8; 4];
        let mut sample_idx = 0usize;
        while bytes.len() >= 4 {
            for b in chunk.iter_mut() {
                *b = bytes.pop_front().unwrap();
            }
            let s = f32::from_le_bytes(chunk);
            if s.abs() > peak {
                peak = s.abs();
            }
            sample_idx += 1;
        }
        total_frames += (sample_idx as u64) / (want_channels as u64);

        // Wait for the next buffer-ready event (timeout in 100ns units => 1s).
        if h_event.wait_for_event(10_000_000).is_err() {
            println!("(event wait timed out — stopping early)");
            break;
        }
    }
    audio_client.stop_stream()?;

    println!("\n--- RESULT ---");
    println!("Frames captured: {total_frames} (~{:.2}s at {want_rate} Hz)", total_frames as f64 / want_rate as f64);
    println!("Peak amplitude:  {peak:.5}  ({})", if peak > 0.0001 { "NON-SILENT — loopback works" } else { "SILENT — was anything playing?" });
    println!("\nPROBE OK: wasapi 0.23 loopback capture builds + runs on this machine.");
    println!("Paste this whole output back so I can lock the Phase D capture format.");
    Ok(())
}
