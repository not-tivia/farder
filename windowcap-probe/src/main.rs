//! THROWAWAY probe — validate the windows-capture 2.0.0 `Window` API before the
//! screenshare-UX feature builds against it (window capture is new cfg(windows)
//! code that can't compile/run on the Linux side; the monitor + WASAPI paths both
//! had API surprises that only showed on the owner's Windows build).
//!
//! It enumerates open windows (title + size + process) and confirms a capture
//! `Settings` can be constructed against a `Window` (the type-check that matters
//! — it proves Window satisfies the same capture-item trait Monitor does, so the
//! real backend can `start_free_threaded` it). It does NOT start a real capture.

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows-only probe (windows-capture). Run it on the Windows box.");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    use windows_capture::window::Window;

    let windows = match Window::enumerate() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Window::enumerate failed: {e}");
            std::process::exit(1);
        }
    };

    println!("Found {} window(s):", windows.len());
    for (i, w) in windows.iter().enumerate() {
        let title = w.title().unwrap_or_else(|_| "<no title>".into());
        let proc = w.process_name().unwrap_or_else(|_| "<?>".into());
        let ww = w.width().unwrap_or(-1);
        let hh = w.height().unwrap_or(-1);
        println!("[{i}] {ww}x{hh}  \"{title}\"  ({proc})");
    }

    // The type-check that matters: a Window must be accepted as the capture item
    // by Settings::new (same trait as Monitor). We build a Settings but never
    // start it — the real app supplies real flags + start_free_threaded.
    if let Some(w) = windows
        .into_iter()
        .find(|w| w.title().map(|t| !t.trim().is_empty()).unwrap_or(false))
    {
        use windows_capture::settings::{
            ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
            MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
        };
        let title = w.title().unwrap_or_default();
        let _settings = Settings::new(
            w,
            CursorCaptureSettings::Default,
            DrawBorderSettings::Default,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Rgba8,
            (), // flags: unit is fine for a type-check
        );
        println!("OK: Settings::new(Window=\"{title}\", ...) type-checks.");
    } else {
        println!("(no titled window to type-check Settings against — enumeration still validated)");
    }

    println!("\nPROBE OK: window enumeration + Settings-against-Window compile/run on this machine.");
    println!("Paste this whole output back.");
}
