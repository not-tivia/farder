use anyhow::Result;
use rusqlite::Connection;

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch("
        CREATE TABLE IF NOT EXISTS members (
            public_key   BLOB    PRIMARY KEY,
            display_name TEXT    NOT NULL,
            avatar       BLOB,
            joined_at    INTEGER NOT NULL,
            banned       INTEGER NOT NULL DEFAULT 0,
            revoked      INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS roles (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            name        TEXT    NOT NULL,
            permissions INTEGER NOT NULL,
            color       TEXT,
            position    INTEGER NOT NULL,
            builtin     INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS member_roles (
            member_key BLOB    NOT NULL,
            role_id    INTEGER NOT NULL,
            PRIMARY KEY (member_key, role_id),
            FOREIGN KEY (member_key) REFERENCES members(public_key),
            FOREIGN KEY (role_id)    REFERENCES roles(id)
        );

        CREATE TABLE IF NOT EXISTS categories (
            id       INTEGER PRIMARY KEY AUTOINCREMENT,
            name     TEXT    NOT NULL,
            position INTEGER NOT NULL,
            deleted  INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS channels (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            name           TEXT    NOT NULL,
            channel_type   TEXT    NOT NULL DEFAULT 'text',
            category_id    INTEGER,
            position       INTEGER NOT NULL,
            topic          TEXT,
            nsfw           INTEGER NOT NULL DEFAULT 0,
            slow_mode_secs INTEGER NOT NULL DEFAULT 0,
            retention_secs INTEGER,
            deleted        INTEGER NOT NULL DEFAULT 0,
            deleted_at     INTEGER,
            FOREIGN KEY (category_id) REFERENCES categories(id)
        );

        CREATE TABLE IF NOT EXISTS channel_overrides (
            channel_id INTEGER NOT NULL,
            role_id    INTEGER NOT NULL,
            allow_bits INTEGER NOT NULL DEFAULT 0,
            deny_bits  INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (channel_id, role_id)
        );

        CREATE TABLE IF NOT EXISTS category_overrides (
            category_id INTEGER NOT NULL,
            role_id     INTEGER NOT NULL,
            allow_bits  INTEGER NOT NULL DEFAULT 0,
            deny_bits   INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (category_id, role_id)
        );

        CREATE TABLE IF NOT EXISTS messages (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            channel_id INTEGER NOT NULL,
            author     BLOB    NOT NULL,
            content    TEXT    NOT NULL,
            timestamp  INTEGER NOT NULL,
            edited_at  INTEGER,
            reply_to   INTEGER,
            pinned     INTEGER NOT NULL DEFAULT 0
        );

        CREATE INDEX IF NOT EXISTS idx_messages_channel_ts ON messages(channel_id, timestamp);
        CREATE INDEX IF NOT EXISTS idx_messages_channel_id ON messages(channel_id, id);

        CREATE TABLE IF NOT EXISTS invites (
            code           TEXT    PRIMARY KEY,
            created_by     BLOB    NOT NULL,
            max_uses       INTEGER,
            use_count      INTEGER NOT NULL DEFAULT 0,
            expires_at     INTEGER,
            target_channel INTEGER
        );

        CREATE TABLE IF NOT EXISTS files (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            hash          TEXT    UNIQUE NOT NULL,
            size          INTEGER NOT NULL,
            mime_type     TEXT    NOT NULL,
            original_name TEXT    NOT NULL,
            uploaded_by   BLOB    NOT NULL,
            uploaded_at   INTEGER NOT NULL,
            ref_count     INTEGER NOT NULL DEFAULT 0,
            width         INTEGER,
            height        INTEGER,
            duration_secs REAL
        );

        CREATE TABLE IF NOT EXISTS message_attachments (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            message_id    INTEGER NOT NULL,
            file_id       INTEGER NOT NULL,
            position      INTEGER NOT NULL,
            original_name TEXT    NOT NULL,
            width         INTEGER,
            height        INTEGER,
            duration_secs REAL,
            FOREIGN KEY (message_id) REFERENCES messages(id),
            FOREIGN KEY (file_id)    REFERENCES files(id)
        );

        CREATE INDEX IF NOT EXISTS idx_message_attachments_message ON message_attachments(message_id);
        CREATE INDEX IF NOT EXISTS idx_message_attachments_file ON message_attachments(file_id);

        CREATE TABLE IF NOT EXISTS reactions (
            message_id INTEGER NOT NULL,
            user_key BLOB NOT NULL,
            emoji TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            PRIMARY KEY (message_id, user_key, emoji),
            FOREIGN KEY (message_id) REFERENCES messages(id)
        );
        CREATE INDEX IF NOT EXISTS idx_reactions_message ON reactions(message_id);

        CREATE TABLE IF NOT EXISTS deletion_requests (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            member_key BLOB UNIQUE NOT NULL,
            requested_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS dm_participants (
            channel_id INTEGER NOT NULL,
            user_key BLOB NOT NULL,
            PRIMARY KEY (channel_id, user_key),
            FOREIGN KEY (channel_id) REFERENCES channels(id)
        );

        CREATE TABLE IF NOT EXISTS blocked_users (
            blocker_key BLOB NOT NULL,
            blocked_key BLOB NOT NULL,
            blocked_at INTEGER NOT NULL,
            PRIMARY KEY (blocker_key, blocked_key)
        );

        CREATE TABLE IF NOT EXISTS voice_state (
            channel_id INTEGER NOT NULL,
            user_key BLOB NOT NULL,
            joined_at INTEGER NOT NULL,
            PRIMARY KEY (channel_id, user_key),
            FOREIGN KEY (channel_id) REFERENCES channels(id)
        );
    ")?;

    // Migration: add thread_parent_message_id column to channels if missing.
    let has_thread_col: bool = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('channels') WHERE name = 'thread_parent_message_id'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if !has_thread_col {
        conn.execute("ALTER TABLE channels ADD COLUMN thread_parent_message_id INTEGER", [])?;
    }

    // Reactions: add file_id column for custom-emoji reactions (Phase 1 of Reaction Book).
    // SQLite has no IF NOT EXISTS for ALTER, so we check pragma table_info first.
    let has_file_id: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(reactions)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "file_id")
    };
    if !has_file_id {
        conn.execute(
            "ALTER TABLE reactions ADD COLUMN file_id INTEGER NULL REFERENCES files(id)",
            [],
        )?;
    }

    // Reactions: move file_id into the uniqueness key (Reaction Book phase 2).
    // The original PK (message_id, user_key, emoji) ignored file_id, so a user
    // could hold only ONE ":custom:" reaction per message. SQLite cannot alter
    // a PK -> rebuild. file_id uses a 0 sentinel (= standard emoji; real file
    // ids start at 1) because NULLs in a composite PK are mutually distinct
    // (no dedupe). Idempotent: skip when file_id is already NOT NULL.
    //
    // FK NOTE: open_file sets PRAGMA foreign_keys=ON. Row 0 never exists in
    // the files table (autoincrement starts at 1), so keeping
    // "REFERENCES files(id)" on the new column would cause FK violations for
    // every standard-emoji reaction (file_id=0) on production databases.
    // The REFERENCES clause is therefore OMITTED from reactions_new.
    let file_id_not_null: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(reactions)")?;
        let rows: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get::<_, String>(1)?, row.get::<_, i64>(3)?)))?
            .filter_map(|r| r.ok())
            .collect();
        rows.iter().any(|(name, notnull)| name == "file_id" && *notnull == 1)
    };
    if !file_id_not_null {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE reactions_new (
                 message_id INTEGER NOT NULL,
                 user_key BLOB NOT NULL,
                 emoji TEXT NOT NULL,
                 file_id INTEGER NOT NULL DEFAULT 0,
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY (message_id, user_key, emoji, file_id),
                 FOREIGN KEY (message_id) REFERENCES messages(id)
             );
             INSERT OR IGNORE INTO reactions_new (message_id, user_key, emoji, file_id, created_at)
                 SELECT message_id, user_key, emoji, COALESCE(file_id, 0), created_at FROM reactions;
             DROP TABLE reactions;
             ALTER TABLE reactions_new RENAME TO reactions;
             CREATE INDEX IF NOT EXISTS idx_reactions_message ON reactions(message_id);
             COMMIT;",
        )?;
    }

    // Members: add ban_reason column for moderator-supplied context (Task 1 of Member Moderation).
    let has_ban_reason: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(members)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "ban_reason")
    };
    if !has_ban_reason {
        conn.execute(
            "ALTER TABLE members ADD COLUMN ban_reason TEXT NULL",
            [],
        )?;
    }

    // Members: add timeout_until + timeout_reason columns (Phase 2 moderation).
    let has_timeout_until: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(members)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "timeout_until")
    };
    if !has_timeout_until {
        conn.execute(
            "ALTER TABLE members ADD COLUMN timeout_until INTEGER NULL",
            [],
        )?;
        conn.execute(
            "ALTER TABLE members ADD COLUMN timeout_reason TEXT NULL",
            [],
        )?;
    }

    // Members: profile_hash column for signed-profile sync. The pre-existing
    // (never used) `avatar` BLOB column now stores the member's serialized
    // SignedProfile; profile_hash is its SHA-256 hex (cache key for clients).
    let has_profile_hash: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(members)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "profile_hash")
    };
    if !has_profile_hash {
        conn.execute("ALTER TABLE members ADD COLUMN profile_hash TEXT NULL", [])?;
    }

    // Audit events: forever-retention moderator action log (Phase 2).
    conn.execute(
        "CREATE TABLE IF NOT EXISTS audit_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            actor_pk BLOB NOT NULL,
            target_pk BLOB,
            action TEXT NOT NULL,
            metadata TEXT NOT NULL DEFAULT '{}',
            timestamp_ms INTEGER NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_audit_events_timestamp ON audit_events(timestamp_ms DESC)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_audit_events_actor ON audit_events(actor_pk)",
        [],
    )?;

    // FTS5 virtual table: check existence before creating (IF NOT EXISTS not
    // supported for virtual tables in older SQLite versions).
    let fts_exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='messages_fts'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;

    if !fts_exists {
        conn.execute_batch(
            "CREATE VIRTUAL TABLE messages_fts USING fts5(content, content_rowid='id', content='messages');",
        )?;
    }

    Ok(())
}

pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    init_schema(&conn)?;
    Ok(conn)
}

pub fn open_file(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    init_schema(&conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_init_succeeds() {
        let conn = open_in_memory().expect("open_in_memory failed");

        let expected_tables = [
            "members",
            "roles",
            "member_roles",
            "categories",
            "channels",
            "channel_overrides",
            "category_overrides",
            "messages",
            "invites",
            "messages_fts",
            "files",
            "message_attachments",
            "reactions",
            "deletion_requests",
            "dm_participants",
            "blocked_users",
            "voice_state",
        ];

        for table in &expected_tables {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                    rusqlite::params![table],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            assert_eq!(count, 1, "table '{}' not found in sqlite_master", table);
        }
    }

    #[test]
    fn test_schema_init_idempotent() {
        let conn = Connection::open_in_memory().expect("open_in_memory failed");
        init_schema(&conn).expect("first init_schema call failed");
        init_schema(&conn).expect("second init_schema call failed");
    }
}
