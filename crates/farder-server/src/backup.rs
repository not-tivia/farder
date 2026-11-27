//! Backup and restore of a whole server: its database, the files people uploaded, and the
//! folder that holds the server's stable identity.
//!
//! WHY: until now a server was one set of files on one computer, and nothing copied them
//! anywhere. A dead disk, or an owner account taken over by someone who then deleted
//! everything, meant the server was gone. A backup is a plain folder that can be copied
//! to another machine; restoring it brings the server back as it was (same history, same
//! identity), so clients still recognise it.
//!
//! A backup folder holds:
//!   - `farder-server.db`  a consistent snapshot of the database taken with SQLite's own
//!                         `VACUUM INTO`, safe while the server is running and being written to
//!   - `files/`            the uploaded files
//!   - `data/`             the server's stable identity (the relay `server_id` file, so a
//!                         restored server is the SAME server on the relay)
//!   - `MANIFEST.json`     format version, time, and a SHA-256 of the database snapshot
//!
//! A backup is written to a `.partial` folder and renamed only when complete, so a crash
//! half-way never leaves something that looks like a good backup. Restoring checks the
//! checksum and the database's own integrity check first, refuses to overwrite existing
//! data unless told to, and even then moves the old data aside instead of deleting it.
//!
//! What a backup does NOT protect against: it is a copy of the server's state, so if the
//! owner's account is taken over and the attacker deletes things, the NEXT automatic backup
//! would copy the damage. That is why backups are kept in rotation (several, not one), and
//! why the plan also has a separate recovery owner and a cooling-off period for destructive
//! actions (see docs/mesh-hosting-status.md).

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const FORMAT: u32 = 1;
const DB_NAME: &str = "farder-server.db";
const MANIFEST: &str = "MANIFEST.json";
const PREFIX: &str = "farder-backup-";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Manifest {
    pub format: u32,
    pub created_at: u64,
    pub db_sha256: String,
    pub db_bytes: u64,
    pub files_count: u64,
    pub data_files_count: u64,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sha256_file(path: &Path) -> Result<(String, u64)> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex::encode(h.finalize()), total))
}

/// Copy a folder tree (files only; symlinks are skipped on purpose so a backup can never be
/// tricked into copying something outside the folder). Returns how many files were copied.
fn copy_tree(from: &Path, to: &Path) -> Result<u64> {
    if !from.is_dir() {
        return Ok(0);
    }
    std::fs::create_dir_all(to)?;
    let mut n = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if ty.is_symlink() {
            continue;
        } else if ty.is_dir() {
            n += copy_tree(&entry.path(), &dest)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), &dest)?;
            n += 1;
        }
    }
    Ok(n)
}

fn dir_is_empty(p: &Path) -> bool {
    match std::fs::read_dir(p) {
        Ok(mut it) => it.next().is_none(),
        Err(_) => true, // missing counts as empty
    }
}

/// Make one backup of a running (or stopped) server into a new folder under `out_root`.
/// `conn` is used only to ask SQLite for a consistent snapshot; it is not locked for longer
/// than that snapshot takes. Returns the folder it created.
pub fn create_backup(conn: &Connection, storage_dir: &Path, data_dir: &Path, out_root: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out_root).with_context(|| format!("create {}", out_root.display()))?;
    let created_at = now_secs();
    // Unique even if two are made in the same second.
    let mut name = format!("{PREFIX}{created_at}");
    let mut n = 1;
    while out_root.join(&name).exists() || out_root.join(format!("{name}.partial")).exists() {
        name = format!("{PREFIX}{created_at}-{n}");
        n += 1;
    }
    let partial = out_root.join(format!("{name}.partial"));
    std::fs::create_dir_all(&partial)?;

    let result = (|| -> Result<()> {
        let db_snapshot = partial.join(DB_NAME);
        conn.execute("VACUUM INTO ?1", [db_snapshot.to_string_lossy().as_ref()])
            .context("snapshot the database")?;
        let (db_sha256, db_bytes) = sha256_file(&db_snapshot)?;
        let files_count = copy_tree(storage_dir, &partial.join("files"))?;
        let data_files_count = copy_tree(data_dir, &partial.join("data"))?;
        let manifest = Manifest { format: FORMAT, created_at, db_sha256, db_bytes, files_count, data_files_count };
        std::fs::write(partial.join(MANIFEST), serde_json::to_vec_pretty(&manifest)?)?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&partial); // never leave a half-made backup behind
        return Err(e);
    }
    let finished = out_root.join(&name);
    std::fs::rename(&partial, &finished)?;
    Ok(finished)
}

/// Every COMPLETE backup under `out_root`, oldest first. Half-made (`.partial`) folders and
/// anything else are ignored.
pub fn list_backups(out_root: &Path) -> Vec<(PathBuf, Manifest)> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(out_root) else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with(PREFIX) || name.ends_with(".partial") {
            continue;
        }
        if let Ok(bytes) = std::fs::read(e.path().join(MANIFEST)) {
            if let Ok(m) = serde_json::from_slice::<Manifest>(&bytes) {
                out.push((e.path(), m));
            }
        }
    }
    out.sort_by(|a, b| a.1.created_at.cmp(&b.1.created_at).then(a.0.cmp(&b.0)));
    out
}

/// Keep only the newest `keep` complete backups; delete the rest. Returns how many were removed.
/// `keep` of 0 is treated as 1: this never deletes the only backup there is.
pub fn prune(out_root: &Path, keep: usize) -> Result<usize> {
    let keep = keep.max(1);
    let all = list_backups(out_root);
    if all.len() <= keep {
        return Ok(0);
    }
    let remove = all.len() - keep;
    for (path, _) in all.iter().take(remove) {
        std::fs::remove_dir_all(path)?;
    }
    Ok(remove)
}

/// Check a backup folder is whole and unaltered, without restoring it.
pub fn verify_backup(dir: &Path) -> Result<Manifest> {
    let bytes = std::fs::read(dir.join(MANIFEST)).with_context(|| format!("{} is not a Farder backup (no {MANIFEST})", dir.display()))?;
    let m: Manifest = serde_json::from_slice(&bytes).context("backup manifest is unreadable")?;
    if m.format != FORMAT {
        bail!("backup format {} is not supported by this version (expects {})", m.format, FORMAT);
    }
    let db = dir.join(DB_NAME);
    let (sha, size) = sha256_file(&db).context("backup database is missing")?;
    if sha != m.db_sha256 || size != m.db_bytes {
        bail!("backup database does not match its checksum: the backup is damaged or was altered");
    }
    let conn = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let check: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if check != "ok" {
        bail!("backup database failed SQLite's integrity check: {check}");
    }
    Ok(m)
}

/// Move something out of the way instead of deleting it.
fn set_aside(path: &Path, stamp: u64) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut dest = PathBuf::from(format!("{}.before-restore-{stamp}", path.display()));
    let mut n = 1;
    while dest.exists() {
        dest = PathBuf::from(format!("{}.before-restore-{stamp}-{n}", path.display()));
        n += 1;
    }
    std::fs::rename(path, &dest).with_context(|| format!("move {} aside", path.display()))?;
    Ok(Some(dest))
}

/// Restore a backup to `db_path`, `storage_dir` and `data_dir`.
///
/// Refuses if any target already holds data unless `force` is set; with `force` the old
/// data is MOVED ASIDE (renamed `*.before-restore-<time>`), never deleted. The server must
/// not be running. The backup is verified first.
pub fn restore_backup(backup: &Path, db_path: &Path, storage_dir: &Path, data_dir: &Path, force: bool) -> Result<Manifest> {
    let manifest = verify_backup(backup)?;
    let db_exists = db_path.exists();
    let would_overwrite = db_exists || !dir_is_empty(storage_dir) || !dir_is_empty(data_dir);
    if would_overwrite && !force {
        bail!(
            "refusing to restore over an existing server (database {}, files {}, data {}). \
             Run again with --force to move the existing data aside and restore.",
            db_path.display(), storage_dir.display(), data_dir.display()
        );
    }
    let stamp = now_secs();
    // The write-ahead files belong to the old database; they must not be applied to the new one.
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", db_path.display()));
        set_aside(&side, stamp)?;
    }
    set_aside(db_path, stamp)?;
    set_aside(storage_dir, stamp)?;
    set_aside(data_dir, stamp)?;
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::copy(backup.join(DB_NAME), db_path).context("restore the database")?;
    copy_tree(&backup.join("files"), storage_dir)?;
    std::fs::create_dir_all(storage_dir)?;
    copy_tree(&backup.join("data"), data_dir)?;
    std::fs::create_dir_all(data_dir)?;
    Ok(manifest)
}

/// Automatic backups: one right away, then one every `every`, pruned to `keep` (minimum 1).
/// The task uses its OWN database connection (SQLite lets readers and the writer work side by
/// side), so a backup never makes the server wait. Failures are logged and the loop goes on.
pub fn spawn_backup_task(
    db_path: String,
    storage_dir: String,
    data_dir: PathBuf,
    backup_dir: PathBuf,
    every: std::time::Duration,
    keep: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let (d, s, dt, p) = (backup_dir.clone(), storage_dir.clone(), data_dir.clone(), db_path.clone());
            let r = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
                let conn = crate::db::open_file(&p)?;
                let made = create_backup(&conn, Path::new(&s), &dt, &d)?;
                prune(&d, keep)?;
                Ok(made)
            })
            .await;
            match r {
                Ok(Ok(made)) => tracing::info!("Backup written to {}", made.display()),
                Ok(Err(e)) => tracing::warn!("automatic backup failed: {e:#}"),
                Err(e) => tracing::warn!("automatic backup task failed: {e}"),
            }
            tokio::time::sleep(every).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, members, messages, channels};
    use farder_protocol::server::ChannelType;
    use farder_crypto::identity::Keypair;

    struct Server {
        _tmp: tempfile::TempDir,
        db: PathBuf,
        files: PathBuf,
        data: PathBuf,
        backups: PathBuf,
    }

    /// A small real server on disk: a member, a channel, messages, an uploaded file, an identity file.
    fn server(messages_n: usize) -> (Server, Connection) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let s = Server {
            db: root.join("server.db"),
            files: root.join("files"),
            data: root.join("data"),
            backups: root.join("backups"),
            _tmp: tmp,
        };
        std::fs::create_dir_all(&s.files).unwrap();
        std::fs::create_dir_all(s.files.join("sub")).unwrap();
        std::fs::create_dir_all(&s.data).unwrap();
        std::fs::write(s.files.join("a.bin"), vec![7u8; 5000]).unwrap();
        std::fs::write(s.files.join("sub").join("b.txt"), b"hello").unwrap();
        std::fs::write(s.data.join("server_id"), [9u8; 32]).unwrap();
        let conn = db::open_file(s.db.to_str().unwrap()).unwrap();
        let kp = Keypair::generate();
        members::register_member(&conn, &kp.public_key(), "Owner").unwrap();
        let ch = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        for i in 0..messages_n {
            messages::insert_message(&conn, ch, &kp.public_key(), &format!("message number {i}"), None).unwrap();
        }
        (s, conn)
    }

    fn count_messages(db: &Path) -> i64 {
        let c = Connection::open(db).unwrap();
        c.query_row("SELECT count(*) FROM messages", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn a_backup_taken_from_a_live_server_restores_the_same_history_files_and_identity() {
        let (s, conn) = server(25);
        let b = create_backup(&conn, &s.files, &s.data, &s.backups).unwrap();
        // The live server keeps writing after the backup; the backup must not change.
        let kp = Keypair::generate();
        members::register_member(&conn, &kp.public_key(), "Late").unwrap();
        messages::insert_message(&conn, 1, &kp.public_key(), "after the backup", None).unwrap();
        drop(conn);

        let m = verify_backup(&b).unwrap();
        assert_eq!(m.files_count, 2);
        assert_eq!(m.data_files_count, 1);

        // Restore into a fresh place, as on a new machine.
        let t = tempfile::tempdir().unwrap();
        let (db2, files2, data2) = (t.path().join("new.db"), t.path().join("files"), t.path().join("data"));
        restore_backup(&b, &db2, &files2, &data2, false).unwrap();
        assert_eq!(count_messages(&db2), 25, "the restored history is the history at backup time");
        assert_eq!(std::fs::read(files2.join("a.bin")).unwrap(), vec![7u8; 5000]);
        assert_eq!(std::fs::read(files2.join("sub").join("b.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(data2.join("server_id")).unwrap(), vec![9u8; 32], "same server identity");
        // And the live server's own data was untouched by the backup.
        assert_eq!(count_messages(&s.db), 26);
    }

    #[test]
    fn a_nuked_server_can_be_restored_over_itself_and_the_wreckage_is_kept() {
        let (s, conn) = server(10);
        let b = create_backup(&conn, &s.files, &s.data, &s.backups).unwrap();
        drop(conn);
        // The attack: everything deleted / emptied.
        std::fs::remove_dir_all(&s.files).unwrap();
        std::fs::write(&s.db, b"").unwrap();
        // Without --force it refuses (it will not guess it is safe to overwrite).
        let err = restore_backup(&b, &s.db, &s.files, &s.data, false).unwrap_err().to_string();
        assert!(err.contains("--force"), "{err}");
        // With --force it restores and moves the old data aside rather than deleting it.
        restore_backup(&b, &s.db, &s.files, &s.data, true).unwrap();
        assert_eq!(count_messages(&s.db), 10);
        assert!(s.files.join("a.bin").exists());
        let aside: Vec<_> = std::fs::read_dir(s.db.parent().unwrap()).unwrap().flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("before-restore")).collect();
        assert!(!aside.is_empty(), "the previous data must be kept, not destroyed");
    }

    #[test]
    fn a_damaged_or_tampered_backup_is_refused() {
        let (s, conn) = server(5);
        let b = create_backup(&conn, &s.files, &s.data, &s.backups).unwrap();
        drop(conn);
        // Flip a byte in the database snapshot.
        let db = b.join(DB_NAME);
        let mut bytes = std::fs::read(&db).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        std::fs::write(&db, bytes).unwrap();
        let err = verify_backup(&b).unwrap_err().to_string();
        assert!(err.contains("checksum"), "{err}");
        let t = tempfile::tempdir().unwrap();
        assert!(restore_backup(&b, &t.path().join("x.db"), &t.path().join("f"), &t.path().join("d"), false).is_err());
        assert!(!t.path().join("x.db").exists(), "a refused restore must not leave a half-restored database");
    }

    #[test]
    fn a_folder_that_is_not_a_backup_is_refused() {
        let t = tempfile::tempdir().unwrap();
        assert!(verify_backup(t.path()).unwrap_err().to_string().contains("not a Farder backup"));
    }

    #[test]
    fn rotation_keeps_the_newest_and_never_deletes_the_last_one() {
        let (s, conn) = server(1);
        let mut made = vec![];
        for _ in 0..5 {
            made.push(create_backup(&conn, &s.files, &s.data, &s.backups).unwrap());
        }
        assert_eq!(list_backups(&s.backups).len(), 5);
        assert_eq!(prune(&s.backups, 2).unwrap(), 3);
        let left = list_backups(&s.backups);
        assert_eq!(left.len(), 2);
        // The two newest survived.
        assert!(left.iter().any(|(p, _)| p == &made[4]) && left.iter().any(|(p, _)| p == &made[3]));
        // Asking to keep zero still keeps one.
        assert_eq!(prune(&s.backups, 0).unwrap(), 1);
        assert_eq!(list_backups(&s.backups).len(), 1);
    }

    #[test]
    fn a_half_made_backup_is_ignored_and_a_failed_one_leaves_nothing() {
        let (s, conn) = server(1);
        std::fs::create_dir_all(s.backups.join("farder-backup-1.partial")).unwrap();
        assert!(list_backups(&s.backups).is_empty(), "a .partial folder is not a backup");
        // A snapshot that cannot be written (the output location is a file, not a folder) fails cleanly.
        let blocker = s.backups.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        assert!(create_backup(&conn, &s.files, &s.data, &blocker).is_err());
        let _ = create_backup(&conn, &s.files, &s.data, &s.backups).unwrap();
        assert_eq!(list_backups(&s.backups).len(), 1);
    }

    #[test]
    fn symlinks_in_the_files_folder_are_not_followed() {
        #[cfg(unix)]
        {
            let (s, conn) = server(1);
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("secret.txt"), b"not part of the server").unwrap();
            std::os::unix::fs::symlink(outside.path(), s.files.join("escape")).unwrap();
            let b = create_backup(&conn, &s.files, &s.data, &s.backups).unwrap();
            assert!(!b.join("files").join("escape").exists(), "a link out of the folder must not be copied");
        }
    }
}
