//! Promotion, server side: a read-only copy accepts an owner-signed `HostTerm` and becomes the
//! writable host without a restart. Design: `docs/specs/2026-11-27-promotion-design.md`.
//!
//! The term is stored in `server_settings` under `host_term` (the full signed record, hex).

use crate::state::ServerState;
use farder_crypto::host_term::HostTerm;
use rusqlite::Connection;
use std::sync::atomic::Ordering;

const SETTING: &str = "host_term";

/// The newest term this database holds, or `None` (the original host, term 0).
pub fn stored_term(conn: &Connection) -> anyhow::Result<Option<HostTerm>> {
    match crate::db::get_setting(conn, SETTING)? {
        None => Ok(None),
        Some(h) => Ok(Some(HostTerm::from_bytes(&hex::decode(h)?)?)),
    }
}

/// The hash of the newest accepted event (highest acceptance position), or `""` for an empty log.
pub fn log_head(conn: &Connection) -> anyhow::Result<String> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row("SELECT event_hash FROM events ORDER BY accept_seq DESC LIMIT 1", [], |r| r.get::<_, String>(0))
        .optional()?
        .unwrap_or_default())
}

/// Start-up rule: a server whose stored term names another machine as host must not accept writes.
/// (Asking peers for a higher term is a later step.)
pub fn must_start_read_only(conn: &Connection, own_identity: &[u8; 32]) -> anyhow::Result<bool> {
    Ok(stored_term(conn)?.is_some_and(|t| &t.host_key != own_identity))
}

/// The checks every term must pass, whoever it is for: signed by the owner, this server's id, higher
/// than the stored term.
fn check_basic(conn: &Connection, state: &ServerState, term: &HostTerm) -> Result<(), String> {
    let (owner, server_id) = {
        let guard = state.log_state.lock().unwrap();
        let Some(ls) = guard.as_ref() else { return Err("this copy has no signed history, so it cannot tell who the owner is".into()) };
        (ls.owner().clone(), ls.server_id().clone())
    };
    if term.verify(&owner).is_err() {
        return Err("that term is not signed by the owner of this server".into());
    }
    if term.server_id != server_id {
        return Err("that term is for a different server".into());
    }
    let held = stored_term(conn).map_err(|e| format!("could not read the stored term: {e}"))?.map_or(0, |t| t.term);
    if term.term <= held {
        return Err(format!("that term ({}) is not higher than the one this copy already holds ({held})", term.term));
    }
    Ok(())
}

/// Check a term against everything the copy knows. `Err` is a plain reason for the caller.
fn check(conn: &Connection, state: &ServerState, term: &HostTerm) -> Result<(), String> {
    check_basic(conn, state, term)?;
    let own = *state.host_identity.lock().unwrap();
    if own != Some(term.host_key) {
        return Err("that term names a different machine as the host, not this copy".into());
    }
    let head = log_head(conn).map_err(|e| format!("could not read the log head: {e}"))?;
    if term.after_event != head {
        return Err("that term continues after an event that is not the head of this copy's history".into());
    }
    Ok(())
}

/// Handle `PromoteToHost`. Called with the database connection already locked by the caller.
pub fn promote(conn: &Connection, state: &ServerState, term: HostTerm) -> Result<(), String> {
    if !state.is_read_only() {
        return Err("this server is already the writable host; only a read-only copy can be promoted".into());
    }
    check(conn, state, &term)?;
    // The copy's database is `query_only`: lift that just long enough to save the term, and put it
    // back if saving fails, so a failed promotion leaves a copy that still cannot change.
    conn.pragma_update(None, "query_only", false).map_err(|e| format!("could not make the copy writable: {e}"))?;
    let saved = crate::db::set_setting(conn, SETTING, &hex::encode(term.to_bytes()));
    if let Err(e) = saved {
        let _ = conn.pragma_update(None, "query_only", true);
        return Err(format!("could not save the term: {e}"));
    }
    state.read_only.store(false, Ordering::SeqCst);
    tracing::info!("PROMOTED: this copy is now the writable host (term {}, continuing after event {})", term.term, term.after_event);
    if let Some(hook) = state.on_promoted.lock().unwrap().as_ref() {
        hook();
    }
    Ok(())
}

/// Handle `NotifyHostTerm`: a newer term that names ANOTHER machine as host means this server was
/// replaced. Store it, go read-only (also at the database level) and say so in the log.
pub fn notify(conn: &Connection, state: &ServerState, term: HostTerm) -> Result<(), String> {
    check_basic(conn, state, &term)?;
    if *state.host_identity.lock().unwrap() == Some(term.host_key) {
        return Err("that term names this server as the host: only PromoteToHost can do that".into());
    }
    // Lift `query_only` just long enough to save (a copy has it on); it is switched on again below.
    conn.pragma_update(None, "query_only", false).map_err(|e| format!("could not save the term: {e}"))?;
    let saved = crate::db::set_setting(conn, SETTING, &hex::encode(term.to_bytes()));
    let _ = conn.pragma_update(None, "query_only", true);
    saved.map_err(|e| format!("could not save the term: {e}"))?;
    let addrs = if term.addresses.is_empty() { "address unknown".to_string() } else { term.addresses.join(", ") };
    *state.moved_to.lock().unwrap() = Some(addrs.clone());
    state.read_only.store(true, Ordering::SeqCst);
    tracing::warn!("REPLACED: this server was replaced by a newer host (term {}) at {addrs}; it is now read-only", term.term);
    Ok(())
}
