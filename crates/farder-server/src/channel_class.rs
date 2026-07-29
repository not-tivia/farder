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

use anyhow::{bail, Result};
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
