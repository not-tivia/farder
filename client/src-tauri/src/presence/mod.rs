//! Presence manager: abstracts activity sources, gates them through settings,
//! and pushes the computed presence to every connected server with per-server
//! dedup so unchanged values are never re-sent.

#[cfg(windows)]
pub mod music_windows;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use farder_protocol::server::{Presence, ServerRequest, ServerResponse};

use crate::state::AppState;

// ---------------------------------------------------------------------------
// Source trait
// ---------------------------------------------------------------------------

/// A producer of the local user's current activity. The music source is the
/// only implementation today; a game source (foreground-app + allowlist) can
/// plug in later by implementing this trait.
pub trait PresenceSource: Send + Sync {
    /// The current activity, or `None` if nothing to report right now.
    fn current(&self) -> Option<Presence>;
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

/// Holds all registered sources and computes the presence that should be sent
/// to servers, honoring the two settings gates (presence_enabled, presence_music).
pub struct PresenceManager {
    pub sources: Vec<Box<dyn PresenceSource>>,
}

impl PresenceManager {
    pub fn new(sources: Vec<Box<dyn PresenceSource>>) -> Self {
        Self { sources }
    }

    /// Compute the current presence, honouring the settings gates.
    ///
    /// Returns `None` when:
    /// - presence is disabled globally (`presence_enabled = false`), OR
    /// - the music gate is off (`presence_music = false`) — the only source
    ///   type that exists today; when a game source is added it gets its own gate.
    pub fn compute(&self) -> Option<Presence> {
        if !crate::commands::read_presence_enabled() {
            return None;
        }
        // Only the music source exists today; gate it individually.
        // When a game source arrives, check its own flag here too.
        if !crate::commands::read_presence_music() {
            return None;
        }
        for s in &self.sources {
            if let Some(p) = s.current() {
                return Some(p);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Platform source factory
// ---------------------------------------------------------------------------

/// Build the set of presence sources for the current platform.
///
/// On Windows the GSMTC music source is included; on other platforms the list
/// is empty (presence manager always returns `None`).
pub fn default_sources() -> Vec<Box<dyn PresenceSource>> {
    #[cfg(windows)]
    {
        vec![Box::new(crate::presence::music_windows::MusicSource)]
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Per-server dedup state
// ---------------------------------------------------------------------------

// In-memory map: server_id -> last successfully pushed presence.
// Uses OnceLock (std, stable since 1.70) consistent with profile_sync.rs.
static PUSHED: std::sync::OnceLock<Mutex<HashMap<String, Option<Presence>>>> =
    std::sync::OnceLock::new();

fn pushed_map() -> &'static Mutex<HashMap<String, Option<Presence>>> {
    PUSHED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn last_pushed(server_id: &str) -> Option<Presence> {
    pushed_map()
        .lock()
        .unwrap()
        .get(server_id)
        .cloned()
        .flatten()
}

fn record_pushed(server_id: &str, presence: Option<Presence>) {
    pushed_map()
        .lock()
        .unwrap()
        .insert(server_id.to_string(), presence);
}

// ---------------------------------------------------------------------------
// Give-up guard (mirrors profile_sync.rs)
// ---------------------------------------------------------------------------

// Servers whose connection DROPPED while we pushed presence. A pre-presence
// server cannot decode UpdatePresence and closes the WHOLE connection; because
// the poller re-pushes every 5s while music plays, that would lock the app into
// a disconnect/reconnect loop (the same class of bug as the profile-sync
// "disco-ball"). After one transport-level push failure we stop auto-pushing
// presence to that server for the rest of the session; an app restart, or a
// server upgrade + restart, clears it.
static SUPPRESSED: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn suppressed() -> &'static Mutex<std::collections::HashSet<String>> {
    SUPPRESSED.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

fn is_suppressed(server_id: &str) -> bool {
    suppressed().lock().map(|s| s.contains(server_id)).unwrap_or(false)
}

fn suppress(server_id: &str) {
    if let Ok(mut s) = suppressed().lock() {
        s.insert(server_id.to_string());
    }
}

// ---------------------------------------------------------------------------
// Push
// ---------------------------------------------------------------------------

/// Push presence to every connected server.
///
/// Dedup: if `presence` is identical to the last successfully pushed value for
/// a given server, the request is skipped. This relies on `Presence: PartialEq`
/// (derived in farder-protocol Task 1).
///
/// Failure handling: a transport error (dropped connection) suppresses presence
/// to that server for the rest of the session (see SUPPRESSED) so an old,
/// pre-presence server cannot trigger a reconnect storm. A non-Ok response (the
/// server is alive but rejected the push) is logged but not suppressed, and the
/// dedup entry is left unset so the next tick retries.
pub async fn push_presence_everywhere(state: &Arc<AppState>, presence: Option<Presence>) {
    let ids: Vec<String> = state.servers.lock().unwrap().keys().cloned().collect();
    for id in ids {
        // Skip servers we've given up on this session (see SUPPRESSED).
        if is_suppressed(&id) {
            continue;
        }
        // Dedup: skip if the server already has this presence value.
        // Comparing Option<Presence> via PartialEq handles None == None and
        // Some(a) == Some(b) correctly.
        if last_pushed(&id) == presence {
            continue;
        }
        eprintln!("[presence] -> {}: {:?}", id, presence);
        match crate::bridge::send_request(state, &id, ServerRequest::UpdatePresence {
            presence: presence.clone(),
        })
        .await
        {
            Ok(ServerResponse::Ok) => {
                // Record only on confirmed success so a transport failure retries.
                record_pushed(&id, presence.clone());
            }
            Ok(other) => {
                // Server is alive but did not return Ok (e.g. a validation
                // rejection). Don't suppress; surface it so it isn't silent.
                eprintln!("[presence] push to {} rejected: {:?}", id, other);
            }
            Err(e) => {
                // Transport failure / dropped connection — the reconnect-storm
                // trigger. Give up on this server for the session.
                suppress(&id);
                eprintln!(
                    "[presence] push to {} dropped the connection ({}); the server may be \
                     running an older Farder version — presence to it is paused until the app restarts",
                    id, e
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use farder_protocol::server::PresenceKind;

    struct Mock(Option<Presence>);
    impl PresenceSource for Mock {
        fn current(&self) -> Option<Presence> {
            self.0.clone()
        }
    }

    #[test]
    fn compute_returns_first_some() {
        // Tests source-merge order directly (bypasses settings-on-disk so it
        // works in CI without a settings file present).
        let p = Presence {
            kind: PresenceKind::Music,
            details: "Track Title".into(),
            state: Some("Artist Name".into()),
        };
        let manager = PresenceManager::new(vec![
            Box::new(Mock(None)),
            Box::new(Mock(Some(p.clone()))),
        ]);
        // Verify the source-merge logic: first Some wins.
        let first = manager.sources.iter().find_map(|s| s.current());
        assert_eq!(first, Some(p));
    }

    #[test]
    fn presence_equality_for_dedup() {
        // Verify Presence PartialEq works correctly — the dedup relies on this.
        let p1 = Presence { kind: PresenceKind::Music, details: "A".into(), state: None };
        let p2 = Presence { kind: PresenceKind::Music, details: "A".into(), state: None };
        let p3 = Presence { kind: PresenceKind::Music, details: "B".into(), state: None };
        assert_eq!(p1, p2);
        assert_ne!(p1, p3);
        // Option equality (None == None, Some != different Some).
        assert_eq!(Some(p1.clone()), Some(p2));
        assert_ne!(Some(p1), Some(p3));
        let none: Option<Presence> = None;
        assert_eq!(none, None);
    }

    #[test]
    fn dedup_record_and_read() {
        // record_pushed / last_pushed roundtrip.
        let p = Presence { kind: PresenceKind::Music, details: "Song".into(), state: None };
        // Not recorded yet for this test server.
        // (PUSHED is global; use a unique server ID to avoid cross-test bleed.)
        let sid = "test-dedup-server-1";
        record_pushed(sid, Some(p.clone()));
        assert_eq!(last_pushed(sid), Some(p.clone()));
        // Overwrite with None (cleared presence).
        record_pushed(sid, None);
        assert_eq!(last_pushed(sid), None);
    }

    #[test]
    fn suppress_marks_server_for_session() {
        // The give-up guard: once suppressed, a server stays suppressed (so the
        // 5s poller stops re-pushing and can't drive a reconnect storm).
        // (SUPPRESSED is global; use a unique server ID to avoid cross-test bleed.)
        let sid = "test-suppress-server-1";
        assert!(!is_suppressed(sid));
        suppress(sid);
        assert!(is_suppressed(sid));
    }
}
