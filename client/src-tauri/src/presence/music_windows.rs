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
        read_now_playing().ok().flatten()
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
