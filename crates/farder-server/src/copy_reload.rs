//! A running READ-ONLY copy follows the file at its `--db` path.
//!
//! `farder-replica` installs each verified pull by renaming a new file over `latest.db`, so the
//! connection a copy opened at start-up ends up on `previous.db` and then on an unlinked file.
//! [`spawn_copy_watcher`] checks the path every few seconds and, when the file was replaced,
//! calls [`reload`]: open the new file (`query_only`), load its signed log, and only then swap it
//! into `state.db` and rebuild `genesis`, `log_state`, `owner` and `server_name`. A file that
//! does not open or whose log does not load is ignored (one warning per file) and the old
//! database keeps serving. A WRITABLE server (the host, or a promoted copy) never reloads.
use crate::{db, event_ingest, state::ServerState};
use std::sync::Arc;
use std::time::Duration;

/// How often the path is checked.
pub const CHECK_EVERY: Duration = Duration::from_secs(2);

/// What identifies "the file we opened": (device, inode) on Unix; (length, mtime) elsewhere.
#[derive(PartialEq, Eq, Clone, Debug)]
pub struct FileSig(Vec<u128>);

pub fn file_sig(path: &str) -> Option<FileSig> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(FileSig(vec![m.dev() as u128, m.ino() as u128, m.len() as u128, mtime]))
    }
    #[cfg(not(unix))]
    {
        Some(FileSig(vec![m.len() as u128, mtime]))
    }
}

/// Watch `path` for as long as the server runs. Does nothing while the server is writable.
pub fn spawn_copy_watcher(state: Arc<ServerState>, path: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut seen = file_sig(&path);
        loop {
            tokio::time::sleep(CHECK_EVERY).await;
            if !state.is_read_only() { return; } // promoted: this database is its own now
            let now = file_sig(&path);
            if now.is_none() || now == seen { continue; }
            let (st, p) = (Arc::clone(&state), path.clone());
            let res = tokio::task::spawn_blocking(move || reload(&st, &p)).await;
            match res {
                Ok(Ok(n)) => tracing::info!("copy reloaded: {} ({} events)", path, n),
                Ok(Err(e)) => tracing::warn!("copy at {} changed but cannot be used, still serving the previous one: {:#}", path, e),
                Err(e) => tracing::warn!("copy reload task failed: {}", e),
            }
            seen = now; // success or not, do not retry (or warn about) this same file again
        }
    })
}

/// Switch `state` to the database at `path`. Returns the number of events in its signed log.
/// Leaves `state` untouched on any error, and when the server is no longer read-only.
pub fn reload(state: &ServerState, path: &str) -> anyhow::Result<usize> {
    let conn = db::open_file(path)?;
    conn.pragma_update(None, "query_only", true)?;
    let genesis = event_ingest::load_genesis(&conn)?;
    let log_state = match &genesis { Some(_) => event_ingest::build_log_state(&conn)?, None => None };
    let events: i64 = conn.query_row("SELECT count(*) FROM events", [], |r| r.get(0)).unwrap_or(0);
    let owner = conn.query_row(
        "SELECT public_key FROM members WHERE banned = 0 AND revoked = 0 ORDER BY joined_at ASC LIMIT 1",
        [], |r| r.get::<_, Vec<u8>>(0),
    ).ok()
        .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
        .map(farder_crypto::identity::PublicKey::from_bytes);
    let name = db::get_setting(&conn, "server_name")?.unwrap_or_else(|| "My Farder Server".to_string());

    let mut slot = state.db.lock().unwrap();
    if genesis.is_none() && state.genesis.lock().unwrap().is_some() { anyhow::bail!("the new file has no signed log"); }
    if !state.is_read_only() { anyhow::bail!("this server is writable now"); }
    *state.genesis.lock().unwrap() = genesis;
    *state.log_state.lock().unwrap() = log_state;
    *state.owner.blocking_write() = owner;
    *state.server_name.lock().unwrap() = name;
    *slot = conn;
    Ok(events as usize)
}
