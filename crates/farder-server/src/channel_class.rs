//! The channel content class as the SERVER sees it, and the fail-closed rule
//! every message writer funnels through (spec rev 2, C8/F1).
//!
//! The class is a property of the channel's identity in the LOG
//! (`EventPayload::ChannelCreated { class }`). It is mirrored into
//! `channels.content_class` inside the SAME transaction that accepts the event,
//! so a writer holding only a `&Connection` (or a `&Transaction`, or the widget
//! sweeper, which has no `ServerState` at all) can resolve it without reaching
//! across the log-state mutex. The log stays the authority: the column is
//! *derived* from an accepted event, never authored by an operator.
//!
//! FAIL CLOSED: anything that is not a definite `'plaintext'` is refused. There
//! is no branch in which missing information yields a plaintext write.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use farder_crypto::event_log::ChannelClass;

/// How the server must treat writes into a channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelWriteClass {
    /// Declared plaintext, or a legacy channel the log never knew (Q8 carve-out).
    Plaintext,
    /// Declared `ChannelClass::E2ee` — server-authored content is forbidden.
    E2ee,
    /// No row, unrecognised value, or a failed read. TREATED AS ENCRYPTED.
    Unresolvable,
}

impl ChannelWriteClass {
    /// The single predicate every writer asks. `Unresolvable` answers `true`:
    /// a class we cannot determine is encrypted, never plaintext.
    pub fn refuses_server_authored_content(self) -> bool {
        !matches!(self, ChannelWriteClass::Plaintext)
    }
}

/// The ONE refusal string for every class-based rejection, byte-identical so a
/// channel id never becomes an existence oracle.
pub const E2EE_REFUSED: &str = "not available in encrypted channels";

/// Read the mirrored class. A failed read, a missing row, or an unrecognised
/// value all collapse to `Unresolvable` — which every caller treats as `E2ee`.
pub fn resolve(conn: &Connection, channel_id: u64) -> ChannelWriteClass {
    let row: Option<String> = conn
        .query_row(
            "SELECT content_class FROM channels WHERE id = ?1",
            params![channel_id as i64],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None); // a failed read is UNRESOLVABLE, never plaintext
    match row.as_deref() {
        Some("plaintext") => ChannelWriteClass::Plaintext,
        Some("e2ee") => ChannelWriteClass::E2ee,
        _ => ChannelWriteClass::Unresolvable,
    }
}

/// The choke point's guard: `Ok(())` only for a definitely-plaintext channel.
/// Used by every server-authored write door in `messages.rs`.
pub fn require_plaintext(conn: &Connection, channel_id: u64) -> Result<()> {
    if resolve(conn, channel_id).refuses_server_authored_content() {
        bail!("{E2EE_REFUSED} (channel {channel_id})");
    }
    Ok(())
}

/// The guard for the LOG-DERIVED plaintext door (`MessagePosted` →
/// `messages::insert_derived_row`).
///
/// It is deliberately one notch weaker than [`require_plaintext`], and only in
/// the one case the fold itself carves out: a channel with **no `channels` row
/// at all**. Rung 1 accepts a `MessagePosted` into a channel the log has never
/// seen a `ChannelCreated` for (`event_log_state.rs:880-885` gates only when
/// `self.channels` knows the id), and reconcile re-derives such rows after a
/// channel row is gone. Refusing them here would silently drop legitimate
/// Rung-1 history, so the carve-out is mirrored **exactly**:
///
/// - `'plaintext'` ⇒ allowed;
/// - `'e2ee'` ⇒ refused;
/// - row **present** with an unrecognised value, or a failed read ⇒ refused;
/// - row **absent** ⇒ allowed, and the class authority for that case is the
///   fold, which refuses a plaintext `MessagePosted` in any channel it knows as
///   E2ee. The mirror can only say `'e2ee'` because an accepted `ChannelCreated`
///   wrote it, so the two backstops cannot both be blind at once.
pub fn require_plaintext_for_derived(conn: &Connection, channel_id: u64) -> Result<()> {
    match resolve(conn, channel_id) {
        ChannelWriteClass::Plaintext => Ok(()),
        ChannelWriteClass::E2ee => bail!("{E2EE_REFUSED} (channel {channel_id})"),
        ChannelWriteClass::Unresolvable => {
            if channel_row_exists(conn, channel_id)? {
                // A row exists but its class is unreadable: corrupted mirror.
                bail!("{E2EE_REFUSED} (channel {channel_id})");
            }
            Ok(())
        }
    }
}

/// The E2EE door's guard: `Ok(())` only for a definitely-E2EE channel. The
/// sealed door is not a general bypass — it refuses plaintext channels too.
pub fn require_e2ee(conn: &Connection, channel_id: u64) -> Result<()> {
    if resolve(conn, channel_id) != ChannelWriteClass::E2ee {
        bail!("sealed content is only accepted in encrypted channels (channel {channel_id})");
    }
    Ok(())
}

/// Mirror an accepted `ChannelCreated` class onto the channel row. Called ONLY
/// from `event_ingest` inside the ingest transaction (Task 3).
pub fn set_class(conn: &Connection, channel_id: u64, class: ChannelClass) -> Result<()> {
    let s = match class {
        ChannelClass::Plaintext => "plaintext",
        ChannelClass::E2ee => "e2ee",
    };
    conn.execute(
        "UPDATE channels SET content_class = ?2 WHERE id = ?1",
        params![channel_id as i64, s],
    )?;
    Ok(())
}

/// Startup re-derivation of the mirror from the log (called from `main.rs`
/// beside `reconcile_messages`). Returns the number of rows repaired.
///
/// The log is the authority; `channels.content_class` is only its mirror, and a
/// mirror can drift — a restored DB snapshot, an operator with `sqlite3`, a
/// half-applied migration. So every stored `ChannelCreated` is replayed and its
/// declared class re-asserted against the row.
///
/// The repair rule is deliberately ONE-WAY, because "the log is the authority"
/// and "fail closed" only ever point the same direction downward:
///
/// - mirror agrees with the log ⇒ nothing to do;
/// - mirror is anything **other than** `'e2ee'` while the log says otherwise ⇒
///   repaired to the log's value (this is the direction that can only ever
///   refuse MORE: `plaintext`/garbage/missing-value → `e2ee`, or garbage →
///   `plaintext` for a channel the log itself declared plaintext);
/// - mirror says `'e2ee'` and the log disagrees ⇒ **left alone** and logged at
///   `error!`. Widening a channel the DB currently treats as sealed is the one
///   move that could expose content, so a disagreement there is refused, not
///   resolved: `resolve` keeps returning `E2ee` and every writer keeps refusing.
///
/// A `ChannelCreated` whose `channels` row is **missing** is logged and skipped,
/// never re-created: absence resolves to `Unresolvable` ⇒ refuse, which is the
/// fail-closed answer, whereas re-materializing could resurrect a deleted
/// channel from the log.
pub fn reconcile_channel_classes(conn: &Connection) -> Result<usize> {
    use farder_crypto::event_log::{Event, EventPayload};

    let bodies: Vec<Vec<u8>> = {
        let mut stmt = conn.prepare(
            "SELECT event_body FROM events WHERE payload_type = 'ChannelCreated' \
             ORDER BY accept_seq ASC",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        let mut v = Vec::new();
        for row in rows {
            v.push(row?);
        }
        v
    };

    let mut repaired = 0usize;
    for body in bodies {
        let event = Event::from_bytes(&body).context("decode ChannelCreated for reconcile")?;
        let EventPayload::ChannelCreated { channel_id, class, .. } = &event.core.payload else {
            continue;
        };
        let declared = match class {
            ChannelClass::Plaintext => "plaintext",
            ChannelClass::E2ee => "e2ee",
        };
        let mirrored: Option<String> = conn
            .query_row(
                "SELECT content_class FROM channels WHERE id = ?1",
                params![*channel_id as i64],
                |r| r.get(0),
            )
            .optional()?;
        match mirrored.as_deref() {
            None => tracing::error!(
                channel_id = *channel_id,
                "the log declares a channel with no `channels` row; leaving it \
                 unresolvable (every writer refuses it) rather than re-creating it"
            ),
            Some(current) if current == declared => {}
            Some("e2ee") => tracing::error!(
                channel_id = *channel_id,
                declared,
                "channel class mirror says 'e2ee' but the log declares otherwise; \
                 refusing to widen it — the channel stays refused to every writer"
            ),
            Some(_) => {
                set_class(conn, *channel_id, *class)?;
                repaired += 1;
                tracing::warn!(
                    channel_id = *channel_id,
                    declared,
                    "repaired a drifted channel class mirror from the log"
                );
            }
        }
    }
    Ok(repaired)
}

/// `true` iff a `channels` row exists for this id. A read error propagates (the
/// caller refuses) rather than being flattened to `false`.
fn channel_row_exists(conn: &Connection, channel_id: u64) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM channels WHERE id = ?1",
            params![channel_id as i64],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::create_channel;
    use crate::db;
    use farder_protocol::server::ChannelType;

    fn e2ee_channel(conn: &Connection, name: &str) -> u64 {
        let id = create_channel(conn, name, ChannelType::Text, None, 0).unwrap();
        set_class(conn, id, ChannelClass::E2ee).unwrap();
        id
    }

    #[test]
    fn a_channel_whose_class_cannot_be_resolved_is_treated_as_encrypted() {
        let conn = db::open_in_memory().unwrap();

        // (a) No `channels` row at all.
        assert_eq!(resolve(&conn, 4242), ChannelWriteClass::Unresolvable);
        assert!(resolve(&conn, 4242).refuses_server_authored_content());
        assert!(require_plaintext(&conn, 4242).is_err());

        // (b) A row whose content_class is garbage (corrupted / hostile mirror).
        let id = create_channel(&conn, "weird", ChannelType::Text, None, 0).unwrap();
        conn.execute(
            "UPDATE channels SET content_class = 'quantum' WHERE id = ?1",
            params![id as i64],
        )
        .unwrap();
        assert_eq!(resolve(&conn, id), ChannelWriteClass::Unresolvable);
        assert!(require_plaintext(&conn, id).is_err());
        // Even the log-derive carve-out refuses a PRESENT row with a bad class.
        assert!(require_plaintext_for_derived(&conn, id).is_err());
        // The sealed door refuses it too — unresolvable is not "encrypted enough".
        assert!(require_e2ee(&conn, id).is_err());
    }

    #[test]
    fn a_legacy_channel_absent_from_the_log_is_plaintext_class() {
        let conn = db::open_in_memory().unwrap();
        let id = create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        assert_eq!(resolve(&conn, id), ChannelWriteClass::Plaintext);
        assert!(!resolve(&conn, id).refuses_server_authored_content());
        require_plaintext(&conn, id).expect("Q8 carve-out: legacy channels stay writable");
        require_plaintext_for_derived(&conn, id).unwrap();
    }

    #[test]
    fn set_class_mirrors_the_declared_class_and_resolve_reads_it_back() {
        let conn = db::open_in_memory().unwrap();
        let id = create_channel(&conn, "secrets", ChannelType::Text, None, 0).unwrap();
        set_class(&conn, id, ChannelClass::E2ee).unwrap();
        assert_eq!(resolve(&conn, id), ChannelWriteClass::E2ee);
        assert!(require_plaintext(&conn, id).is_err());
        require_e2ee(&conn, id).unwrap();

        set_class(&conn, id, ChannelClass::Plaintext).unwrap();
        assert_eq!(resolve(&conn, id), ChannelWriteClass::Plaintext);
        require_plaintext(&conn, id).unwrap();
        assert!(require_e2ee(&conn, id).is_err());
    }

    #[test]
    fn the_refusal_string_is_byte_identical_across_channels_so_it_is_no_oracle() {
        let conn = db::open_in_memory().unwrap();
        let sealed = e2ee_channel(&conn, "sealed");
        let missing = 999_999u64;
        let a = require_plaintext(&conn, sealed).unwrap_err().to_string();
        let b = require_plaintext(&conn, missing).unwrap_err().to_string();
        // Same family prefix; only the id (already known to the caller) differs.
        assert!(a.starts_with(E2EE_REFUSED), "{a}");
        assert!(b.starts_with(E2EE_REFUSED), "{b}");
        assert_eq!(
            a.trim_end_matches(|c: char| c != ' '),
            b.trim_end_matches(|c: char| c != ' '),
            "an existing sealed channel and a non-existent one must not be distinguishable"
        );
    }

    // -----------------------------------------------------------------------
    // Startup reconcile: the log re-asserts its mirror (Task 3).
    // -----------------------------------------------------------------------

    /// Store a `ChannelCreated` event in the `events` table (no fold involved —
    /// reconcile reads stored events, which is exactly what a restart does), and
    /// separately plant the `channels` row with whatever class the test wants to
    /// simulate drifting to.
    fn plant(conn: &Connection, channel_id: u64, class: ChannelClass, mirror: Option<&str>) {
        use farder_crypto::event_log::{Event, EventPayload as EP};
        use farder_crypto::identity::Keypair;
        let kp = Keypair::generate();
        let e = Event::next(
            &kp,
            kp.public_key(),
            "sid".into(),
            None,
            0,
            1,
            EP::ChannelCreated {
                channel_id,
                name: "c".into(),
                kind: "text".into(),
                class,
                parent: None,
            },
        );
        crate::event_ingest::store_event(conn, &e).unwrap();
        if let Some(m) = mirror {
            conn.execute(
                "INSERT INTO channels (id, name, channel_type, position, content_class) \
                 VALUES (?1, 'c', 'text', 0, ?2)",
                params![channel_id as i64, m],
            )
            .unwrap();
        }
    }

    fn mirror_of(conn: &Connection, channel_id: u64) -> Option<String> {
        conn.query_row(
            "SELECT content_class FROM channels WHERE id = ?1",
            params![channel_id as i64],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
    }

    #[test]
    fn reconcile_repairs_a_mirror_that_drifted_away_from_the_log() {
        let conn = db::open_in_memory().unwrap();
        // The log says E2ee; the DB was restored/tampered to say plaintext —
        // i.e. a sealed channel that every server-authored writer could write to.
        plant(&conn, 100, ChannelClass::E2ee, Some("plaintext"));
        // The log says E2ee; the mirror is unreadable garbage.
        plant(&conn, 101, ChannelClass::E2ee, Some("quantum"));
        // The log says Plaintext; the mirror is garbage (refused today, and the
        // log is entitled to say what it declared).
        plant(&conn, 102, ChannelClass::Plaintext, Some("quantum"));
        // Already correct — must NOT be counted as a repair.
        plant(&conn, 103, ChannelClass::E2ee, Some("e2ee"));

        assert_eq!(reconcile_channel_classes(&conn).unwrap(), 3);
        assert_eq!(resolve(&conn, 100), ChannelWriteClass::E2ee);
        assert!(require_plaintext(&conn, 100).is_err(), "the repair really gates writers");
        assert_eq!(resolve(&conn, 101), ChannelWriteClass::E2ee);
        assert_eq!(resolve(&conn, 102), ChannelWriteClass::Plaintext);
        assert_eq!(resolve(&conn, 103), ChannelWriteClass::E2ee);
        // Idempotent: a second pass finds nothing left to repair.
        assert_eq!(reconcile_channel_classes(&conn).unwrap(), 0);
    }

    #[test]
    fn reconcile_refuses_to_widen_a_channel_the_mirror_calls_encrypted() {
        let conn = db::open_in_memory().unwrap();
        // The one disagreement that could EXPOSE content if "the log wins" were
        // applied blindly: the DB currently treats the channel as sealed.
        // Fail-closed beats log-authority here — the row is left alone.
        plant(&conn, 200, ChannelClass::Plaintext, Some("e2ee"));
        assert_eq!(reconcile_channel_classes(&conn).unwrap(), 0);
        assert_eq!(mirror_of(&conn, 200).as_deref(), Some("e2ee"));
        assert_eq!(resolve(&conn, 200), ChannelWriteClass::E2ee);
        assert!(require_plaintext(&conn, 200).is_err());
    }

    #[test]
    fn reconcile_never_recreates_a_channel_row_the_log_declares() {
        let conn = db::open_in_memory().unwrap();
        plant(&conn, 300, ChannelClass::E2ee, None);
        assert_eq!(reconcile_channel_classes(&conn).unwrap(), 0);
        assert_eq!(mirror_of(&conn, 300), None, "a deleted channel is not resurrected");
        // ...and absence is still the fail-closed answer.
        assert_eq!(resolve(&conn, 300), ChannelWriteClass::Unresolvable);
        assert!(require_plaintext(&conn, 300).is_err());
    }

    #[test]
    fn the_derive_carve_out_mirrors_the_folds_unknown_channel_rule() {
        let conn = db::open_in_memory().unwrap();
        // Rung-1 reality: a MessagePosted into a channel with no `channels` row.
        assert_eq!(resolve(&conn, 1), ChannelWriteClass::Unresolvable);
        require_plaintext_for_derived(&conn, 1)
            .expect("the fold accepts this; the derive door must not drop it");
        // But a declared E2EE channel is refused on the derive path as well.
        let sealed = e2ee_channel(&conn, "sealed");
        assert!(require_plaintext_for_derived(&conn, sealed).is_err());
    }
}
