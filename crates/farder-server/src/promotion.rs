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

// ---------------------------------------------------------------------------------------------
// Step 4: copies announce their address; a (restarted) old host asks them for a newer term.
// ---------------------------------------------------------------------------------------------

const COPIES_SETTING: &str = "known_copies";
/// How many copy addresses the host remembers (newest first).
pub const MAX_KNOWN_COPIES: usize = 8;
/// How long the host waits for one copy before skipping it.
pub const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Time between probes while this server is the host.
pub const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The copy addresses the host knows, newest first (`server_settings.known_copies`, a JSON list).
pub fn known_copies(conn: &Connection) -> anyhow::Result<Vec<String>> {
    Ok(match crate::db::get_setting(conn, COPIES_SETTING)? {
        None => vec![],
        Some(j) => serde_json::from_str(&j).unwrap_or_default(),
    })
}

/// Handle `AnnounceCopy`: remember `address` as the newest copy (de-duplicated, at most 8). `Err` is a
/// plain reason for the caller.
pub fn add_known_copy(conn: &Connection, address: &str) -> Result<(), String> {
    let address = address.trim();
    let sane = !address.is_empty()
        && address.len() <= 255
        && address.rsplit_once(':').is_some_and(|(h, p)| !h.is_empty() && p.parse::<u16>().is_ok())
        && !address.chars().any(|c| c.is_whitespace() || c.is_control());
    if !sane {
        return Err("that is not a host:port address".into());
    }
    let mut list = known_copies(conn).map_err(|e| format!("could not read the known copies: {e}"))?;
    list.retain(|a| a != address);
    list.insert(0, address.to_string());
    list.truncate(MAX_KNOWN_COPIES);
    crate::db::set_setting(conn, COPIES_SETTING, &serde_json::to_string(&list).unwrap())
        .map_err(|e| format!("could not save the known copies: {e}"))
}

async fn read_frame(r: &mut quinn::RecvStream) -> anyhow::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let n = u32::from_be_bytes(len) as usize;
    anyhow::ensure!(n <= 16 * 1024 * 1024, "frame too large");
    let mut b = vec![0u8; n];
    r.read_exact(&mut b).await?;
    Ok(b)
}

/// Accepts any certificate: the answer is an owner-signed term, which the caller verifies itself, so
/// the connection needs no identity (a wrong peer can only say nothing).
#[derive(Debug)]
struct AnyCert;
impl rustls::client::danger::ServerCertVerifier for AnyCert {
    fn verify_server_cert(&self, _: &rustls::pki_types::CertificateDer<'_>, _: &[rustls::pki_types::CertificateDer<'_>], _: &rustls::pki_types::ServerName<'_>, _: &[u8], _: rustls::pki_types::UnixTime) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &rustls::pki_types::CertificateDer<'_>, _: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> { Ok(rustls::client::danger::HandshakeSignatureValid::assertion()) }
    fn verify_tls13_signature(&self, _: &[u8], _: &rustls::pki_types::CertificateDer<'_>, _: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> { Ok(rustls::client::danger::HandshakeSignatureValid::assertion()) }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> { rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes() }
}

/// Ask one copy for the newest term it holds, without logging in (pre-auth `ClientFrame::GetHostTerm`).
async fn fetch_term(address: &str) -> anyhow::Result<Option<HostTerm>> {
    use farder_protocol::{codec, server::{ClientFrame, ServerFrame}};
    let addr = tokio::net::lookup_host(address).await?.next().ok_or_else(|| anyhow::anyhow!("no address"))?;
    let crypto = rustls::ClientConfig::builder().dangerous().with_custom_certificate_verifier(std::sync::Arc::new(AnyCert)).with_no_client_auth();
    let cfg = quinn::ClientConfig::new(std::sync::Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?));
    let bind = if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let mut ep = quinn::Endpoint::client(bind.parse().unwrap())?;
    ep.set_default_client_config(cfg);
    let conn = ep.connect(addr, "farder-server")?.await?;
    let (mut send, mut recv) = conn.accept_bi().await?;
    let ServerFrame::Challenge { .. } = codec::decode(&read_frame(&mut recv).await?)? else { anyhow::bail!("expected a Challenge") };
    let body = codec::encode(&ClientFrame::GetHostTerm)?;
    send.write_all(&(body.len() as u32).to_be_bytes()).await?;
    send.write_all(&body).await?;
    match codec::decode(&read_frame(&mut recv).await?)? {
        ServerFrame::HostTerm { term } => Ok(term),
        other => anyhow::bail!("unexpected {other:?}"),
    }
}

/// One round: ask every known copy (in parallel, each with `PROBE_TIMEOUT`) for its term. A term that
/// passes `notify`'s checks and names ANOTHER host replaces this server. Offline copies are skipped
/// quietly. Returns true if this server was replaced.
pub async fn probe_copies(state: &std::sync::Arc<ServerState>) -> bool {
    if state.is_read_only() {
        return false;
    }
    let copies = { let conn = state.db.lock().unwrap(); known_copies(&conn).unwrap_or_default() };
    // One task per copy, so the slowest copy (not the sum) bounds the round.
    let mut asks = tokio::task::JoinSet::new();
    for a in copies {
        asks.spawn(async move {
            match tokio::time::timeout(PROBE_TIMEOUT, fetch_term(&a)).await {
                Ok(Ok(t)) => t.map(|t| (a, t)),
                Ok(Err(e)) => { tracing::debug!("copy {a} not reachable: {e:#}"); None }
                Err(_) => { tracing::debug!("copy {a} did not answer in time"); None }
            }
        });
    }
    while let Some(done) = asks.join_next().await {
        let Ok(Some((addr, term))) = done else { continue };
        let conn = state.db.lock().unwrap();
        match notify(&conn, state, term) {
            Ok(()) => { tracing::info!("learned the newer term from the copy at {addr}"); return true; }
            // Not newer, not signed by the owner, or names this very server: nothing to learn.
            Err(why) => tracing::debug!("the term from the copy at {addr} was not used: {why}"),
        }
    }
    false
}

/// Probe now, then every `PROBE_INTERVAL` while this server is the host.
pub fn spawn_copy_probe(state: std::sync::Arc<ServerState>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if probe_copies(&state).await { return; }
            tokio::time::sleep(PROBE_INTERVAL).await;
        }
    })
}
