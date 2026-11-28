# server-backup — `crates/farder-server/src/backup.rs`

Backup and restore of a whole server (database snapshot via `VACUUM INTO`, uploaded files, identity folder,
`MANIFEST.json` with a SHA-256). A backup is a plain folder, written to `.partial` and renamed when complete.

| Function | What it does |
|---|---|
| `create_backup(conn, storage_dir, data_dir, out_root) -> PathBuf` | One backup into `out_root`. Safe while the server runs. |
| `list_backups(out_root)` | Complete backups, oldest first, with manifests. |
| `prune(out_root, keep)` | Delete all but the newest `keep` (minimum 1); returns how many were removed. |
| `verify_backup(dir)` | Check checksum and integrity without restoring. |
| `restore_backup(backup, db_path, storage_dir, data_dir, force)` | Verified restore; old data is moved aside. |
| `spawn_backup_task(db_path, storage_dir, data_dir, backup_dir, every, keep) -> JoinHandle<()>` | Automatic backups: one right away, then one every `every`, pruned to `keep`. Opens its own DB connection; logs `Backup written to ...` or warns on failure. |

`spawn_backup_task` is started by `main.rs` (via `promotion::BackupPlan::start`) for a writable server with
`--backup-dir`, and by `promotion::promoted_hook` when a read-only copy started with `--backup-dir` is promoted
(see `replica.md` § Promotion). A read-only copy that is never promoted writes no backups.
