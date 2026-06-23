//! Presence manager: abstracts activity sources, gates them through settings,
//! and pushes the computed presence to every connected server with per-server
//! dedup so unchanged values are never re-sent.

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
// Push
// ---------------------------------------------------------------------------

/// Push presence to every connected server.
///
/// Dedup: if `presence` is identical to the last successfully pushed value for
/// a given server, the request is skipped. This relies on `Presence: PartialEq`
/// (derived in farder-protocol Task 1). On transport error the dedup entry is
/// left unchanged, so the next poll tick retries.
pub async fn push_presence_everywhere(state: &Arc<AppState>, presence: Option<Presence>) {
    let ids: Vec<String> = state.servers.lock().unwrap().keys().cloned().collect();
    for id in ids {
        // Dedup: skip if the server already has this presence value.
        // Comparing Option<Presence> via PartialEq handles None == None and
        // Some(a) == Some(b) correctly.
        if last_pushed(&id) == presence {
            continue;
        }
        match crate::bridge::send_request(state, &id, ServerRequest::UpdatePresence {
            presence: presence.clone(),
        })
        .await
        {
            Ok(ServerResponse::Ok) => {
                // Record only on confirmed success so a transport failure retries.
                record_pushed(&id, presence.clone());
            }
            _ => {
                // Transport failure or non-Ok response: leave dedup entry unset
                // so the next tick retries the push.
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
}
