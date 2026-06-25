//! Windows GSMTC (Global System Media Transport Controls) music source.
//!
//! Reads the system "now playing" session and reports it as a `Presence` when
//! something is actively playing.  The implementation mirrors the proven
//! presence-probe (`presence-probe/src/main.rs`) exactly — same API calls,
//! same field access.
//!
//! This module is cfg-gated to Windows; it is never compiled on Linux/macOS.
#![cfg(windows)]

use farder_protocol::server::{Presence, PresenceKind};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as Mgr,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};

pub struct MusicSource;

impl crate::presence::PresenceSource for MusicSource {
    fn current(&self) -> Option<Presence> {
        let p = read_now_playing().ok().flatten();
        log_change(&p);
        p
    }
}

/// Log GSMTC detection only when it CHANGES (song start/stop/switch), so a
/// verifier can see what the music source is reading without a line every 5s.
fn log_change(p: &Option<Presence>) {
    use std::sync::Mutex;
    static LAST: Mutex<Option<String>> = Mutex::new(None);
    let key = p.as_ref().map(|p| match &p.state {
        Some(a) => format!("{} \u{2014} {}", p.details, a),
        None => p.details.clone(),
    });
    if let Ok(mut last) = LAST.lock() {
        if *last != key {
            match &key {
                Some(k) => eprintln!("[presence] GSMTC detected: {}", k),
                None => eprintln!("[presence] GSMTC: nothing playing"),
            }
            *last = key;
        }
    }
}

fn read_now_playing() -> windows::core::Result<Option<Presence>> {
    let mgr = Mgr::RequestAsync()?.get()?;
    let session = match mgr.GetCurrentSession() {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    if session.GetPlaybackInfo()?.PlaybackStatus()? != Status::Playing {
        return Ok(None);
    }
    let props = session.TryGetMediaPropertiesAsync()?.get()?;
    let title = props.Title()?.to_string();
    if title.trim().is_empty() {
        return Ok(None);
    }
    let artist = props
        .Artist()
        .ok()
        .map(|h| h.to_string())
        .filter(|s| !s.trim().is_empty());
    Ok(Some(Presence {
        kind: PresenceKind::Music,
        details: title,
        state: artist,
    }))
}
