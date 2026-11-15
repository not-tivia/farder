//! Database snapshots for replicas (docs/mesh-hosting-status.md, step 2).
//!
//! A replica is a member the owner trusts with a full copy of the server. It asks for a snapshot
//! (`SnapshotBegin`): the server makes a consistent copy of its database with SQLite's `VACUUM INTO`,
//! remembers it under an id, and tells the replica its size and SHA-256. The replica then reads it in
//! pieces (`SnapshotChunk`) over the ordinary connection and checks the hash. Snapshots are short-lived
//! (they are deleted after a few minutes) and there are at most a handful at once.
//!
//! Why the database and not the signed log: channels, roles and categories live only in the database,
//! not in the signed history, so a copy built from the log alone would lose them.

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const MAX_CHUNK: u32 = 1024 * 1024;
const MAX_LIVE: usize = 4;
const LIFETIME: Duration = Duration::from_secs(15 * 60);

struct Entry { path: PathBuf, size: u64, made: Instant }

#[derive(Default)]
pub struct Snapshots {
    inner: Mutex<(u64, HashMap<u64, Entry>)>,
}

impl Snapshots {
    /// Make a snapshot into `dir` and register it. Returns (id, size, sha256 hex).
    pub fn begin(&self, conn: &Connection, dir: &Path) -> Result<(u64, u64, String)> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let mut g = self.inner.lock().unwrap();
        // Forget (and delete) the old ones; refuse if too many are still live.
        let stale: Vec<u64> = g.1.iter().filter(|(_, e)| e.made.elapsed() > LIFETIME).map(|(k, _)| *k).collect();
        for k in stale { if let Some(e) = g.1.remove(&k) { let _ = std::fs::remove_file(e.path); } }
        if g.1.len() >= MAX_LIVE { bail!("too many snapshots are being fetched right now; try again in a minute"); }
        g.0 += 1;
        let id = g.0;
        let path = dir.join(format!("snapshot-{id}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        conn.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()]).context("snapshot the database")?;
        let (sha, size) = sha256_file(&path)?;
        g.1.insert(id, Entry { path, size, made: Instant::now() });
        Ok((id, size, sha))
    }

    /// `len` bytes (at most `MAX_CHUNK`) of snapshot `id` from `offset`.
    pub fn chunk(&self, id: u64, offset: u64, len: u32) -> Result<Vec<u8>> {
        let (path, size) = {
            let g = self.inner.lock().unwrap();
            let e = g.1.get(&id).context("no such snapshot (it may have expired; start a new one)")?;
            (e.path.clone(), e.size)
        };
        if offset > size { bail!("offset is past the end"); }
        let n = (len.min(MAX_CHUNK) as u64).min(size - offset) as usize;
        let mut f = std::fs::File::open(&path)?;
        f.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; n];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }
}

pub fn sha256_file(p: &Path) -> Result<(String, u64)> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 { break; }
        h.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex::encode(h.finalize()), total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_is_consistent_readable_in_pieces_and_matches_its_hash() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (a TEXT); INSERT INTO t VALUES ('one'),('two');").unwrap();
        let dir = std::env::temp_dir().join(format!("farder-snap-test-{}", std::process::id()));
        let s = Snapshots::default();
        let (id, size, sha) = s.begin(&conn, &dir).unwrap();
        let mut all = Vec::new();
        loop {
            let c = s.chunk(id, all.len() as u64, 100).unwrap();
            if c.is_empty() { break; }
            all.extend(c);
        }
        assert_eq!(all.len() as u64, size);
        assert_eq!(hex::encode(Sha256::digest(&all)), sha);
        // The pieces are a real database with the rows.
        let p = dir.join("copy.db");
        std::fs::write(&p, &all).unwrap();
        let copy = Connection::open(&p).unwrap();
        assert_eq!(copy.query_row("SELECT COUNT(*) FROM t", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        assert!(s.chunk(999, 0, 10).is_err());
        assert!(s.chunk(id, size + 1, 10).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_few_snapshots_can_be_live_at_once() {
        let conn = Connection::open_in_memory().unwrap();
        let dir = std::env::temp_dir().join(format!("farder-snap-test2-{}", std::process::id()));
        let s = Snapshots::default();
        for _ in 0..MAX_LIVE { s.begin(&conn, &dir).unwrap(); }
        assert!(s.begin(&conn, &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
