// Probe: read the current Windows "now playing" media via GSMTC.
// Run on Windows WITH music playing:  cd presence-probe && cargo run --release
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager as Mgr;

fn main() {
    if let Err(e) = run() {
        eprintln!("presence-probe ERROR: {e:?}");
        eprintln!("(paste this whole output back)");
    }
}

fn run() -> windows::core::Result<()> {
    println!("Requesting GSMTC session manager...");
    let mgr = Mgr::RequestAsync()?.get()?;

    let session = match mgr.GetCurrentSession() {
        Ok(s) => s,
        Err(e) => {
            println!("No current media session found (is something actually playing?). {e:?}");
            return Ok(());
        }
    };

    let props = session.TryGetMediaPropertiesAsync()?.get()?;
    let title = props.Title()?.to_string();
    let artist = props.Artist()?.to_string();
    let app = session.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
    let status = session.GetPlaybackInfo()?.PlaybackStatus()?;

    println!("NOW PLAYING:");
    println!("  title : {title}");
    println!("  artist: {artist}");
    println!("  app   : {app}");
    println!("  status: {status:?}");
    println!("OK -- GSMTC works. Paste this output back.");
    Ok(())
}
