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
            builtin     INTEGER NOT NULL DEFAULT 0,
            hoist       INTEGER NOT NULL DEFAULT 0
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

        CREATE TABLE IF NOT EXISTS events (
            accept_seq   INTEGER PRIMARY KEY AUTOINCREMENT,  -- server acceptance order (replay order)
            event_hash   TEXT    UNIQUE NOT NULL,            -- content id (SHA-256 hex of the signed Event)
            server_id    TEXT    NOT NULL,
            author       BLOB    NOT NULL,                   -- identity pubkey (32 bytes)
            device       TEXT    NOT NULL,                   -- device id
            seq          INTEGER NOT NULL,
            lamport      INTEGER NOT NULL,
            payload_type TEXT    NOT NULL,                   -- e.g. 'MessagePosted', 'MemberJoined'
            channel_id   INTEGER,                            -- denormalized for message lookups (NULL for non-message)
            event_body   BLOB    NOT NULL,                   -- rmp_serde(Event)
            created_at   INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_events_author_seq ON events(author, device, seq);

        CREATE TABLE IF NOT EXISTS genesis (
            singleton    INTEGER PRIMARY KEY CHECK (singleton = 0),  -- exactly one row
            genesis_body BLOB NOT NULL,                              -- rmp_serde(Genesis)
            server_id    TEXT NOT NULL
        );

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

        CREATE TABLE IF NOT EXISTS server_settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
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

    // Bots: mark server-managed ticker members.
    let has_is_bot: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(members)")?;
        let cols: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(1))?.filter_map(|r| r.ok()).collect();
        cols.iter().any(|c| c == "is_bot")
    };
    if !has_is_bot {
        conn.execute("ALTER TABLE members ADD COLUMN is_bot INTEGER NOT NULL DEFAULT 0", [])?;
    }
    conn.execute(
        "CREATE TABLE IF NOT EXISTS bots (
            public_key BLOB PRIMARY KEY,
            secret_key BLOB NOT NULL,
            kind       TEXT NOT NULL,
            coin_id    TEXT NOT NULL,
            label      TEXT NOT NULL,
            created_at INTEGER NOT NULL
        )",
        [],
    )?;

    // Exactly one server system identity (`bots.kind = 'system'`, see
    // `bots::get_or_create_system_identity`). The single `state.db` mutex already
    // serializes lookup-then-insert; this partial unique index is belt-and-braces
    // so no future concurrent path can ever mint a second one.
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_bots_system ON bots(kind) WHERE kind = 'system'",
        [],
    )?;

    // Bots: add source_url, value_path, unit columns for custom_api bots.
    for col in ["source_url", "value_path", "unit"] {
        let has = {
            let mut stmt = conn.prepare("PRAGMA table_info(bots)")?;
            let cols: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(1))?.filter_map(|r| r.ok()).collect();
            cols.iter().any(|c| c == col)
        };
        if !has { conn.execute(&format!("ALTER TABLE bots ADD COLUMN {col} TEXT"), [])?; }
    }

    // Roles: add hoist column (display role members as a named group in the member list).
    let has_role_hoist = {
        let mut stmt = conn.prepare("PRAGMA table_info(roles)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "hoist")
    };
    if !has_role_hoist {
        conn.execute("ALTER TABLE roles ADD COLUMN hoist INTEGER NOT NULL DEFAULT 0", [])?;
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

    // Migration: link event-sourced messages back to their event (NULL for legacy rows).
    let has_event_hash: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(messages)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "event_hash")
    };
    if !has_event_hash {
        conn.execute("ALTER TABLE messages ADD COLUMN event_hash TEXT", [])?;
    }

    // Rung 2: the channel's content class, DERIVED from an accepted `ChannelCreated`
    // inside the same transaction that accepts the event. Legacy rows default to
    // 'plaintext' (Q8 carve-out: a channel the log never knew is permanently
    // plaintext). A missing row or an unrecognised value is UNRESOLVABLE =>
    // refuse (see `channel_class::resolve`) — fail closed, never fail plaintext.
    let has_content_class: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(channels)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "content_class")
    };
    if !has_content_class {
        conn.execute(
            "ALTER TABLE channels ADD COLUMN content_class TEXT NOT NULL DEFAULT 'plaintext'",
            [],
        )?;
    }

    // Rung 2: the sealed message columns. `is_e2ee = 1` marks a row derived from
    // a signature-verified `MessagePostedE2ee`; `sealed` holds the opaque MLS
    // ciphertext. Sealed rows keep `content = ''` and never enter `messages_fts`.
    let msg_cols: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(messages)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols
    };
    if !msg_cols.iter().any(|c| c == "is_e2ee") {
        conn.execute(
            "ALTER TABLE messages ADD COLUMN is_e2ee INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !msg_cols.iter().any(|c| c == "sealed") {
        conn.execute("ALTER TABLE messages ADD COLUMN sealed BLOB", [])?;
    }

    // Rung 2: EXACTLY ONE derived row per log event. Reply mapping, sealed edits
    // and content-blind tombstones all address a row THROUGH its `event_hash`, so
    // a second row carrying the same hash would make "the message this event
    // derived" ambiguous — and on the moderation path (blind delete is the ONLY
    // moderation tool in a sealed channel) an ambiguous target is a correctness
    // bug, not a performance one. It is also what makes those three lookups
    // O(log n) instead of a table scan.
    //
    // SQLite treats NULLs as distinct in a UNIQUE index, so every legacy
    // (non-log) row keeps its NULL `event_hash` and is unaffected. Duplicates are
    // structurally impossible today — `insert_derived_row`/`insert_sealed_row`
    // are the only writers of the column and reconcile derives only events with
    // no row — so a failure here means the derived view is already corrupt, and
    // refusing to boot is the loud, fail-closed answer.
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_event_hash ON messages(event_hash)",
        [],
    )?;

    // Migration: attachment redaction — who took the blob down (NULL = live).
    let has_redacted_by: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(files)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "redacted_by")
    };
    if !has_redacted_by {
        conn.execute("ALTER TABLE files ADD COLUMN redacted_by BLOB", [])?;
    }

    // Webhooks: incoming-webhook delivery (token-authenticated external POST → message).
    conn.execute(
        "CREATE TABLE IF NOT EXISTS webhooks (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            channel_id INTEGER NOT NULL,
            token      TEXT    NOT NULL UNIQUE,
            name       TEXT    NOT NULL,
            public_key BLOB    NOT NULL,
            created_at INTEGER NOT NULL
        )",
        [],
    )?;

    // Migration: author_name_override for webhook-posted messages (non-member display name).
    let has_author_name_override: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(messages)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "author_name_override")
    };
    if !has_author_name_override {
        conn.execute("ALTER TABLE messages ADD COLUMN author_name_override TEXT", [])?;
    }

    // Migration: author_badge for system/bot/webhook message badges (e.g. "WEBHOOK", "BOT").
    let has_author_badge: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(messages)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "author_badge")
    };
    if !has_author_badge {
        conn.execute("ALTER TABLE messages ADD COLUMN author_badge TEXT", [])?;
    }

    // Migration: widget JSON pointer for interactive widget messages (polls/giveaways),
    // e.g. {"type":"poll","id":3}. Server-written only; NULL for all plain messages.
    let has_widget: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(messages)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "widget")
    };
    if !has_widget {
        conn.execute("ALTER TABLE messages ADD COLUMN widget TEXT", [])?;
    }

    // Slash commands: owner-configurable /trigger commands.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS commands (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            trigger           TEXT    NOT NULL UNIQUE,
            name              TEXT    NOT NULL,
            description       TEXT    NOT NULL,
            kind              TEXT    NOT NULL,
            body_text         TEXT,
            url_template      TEXT,
            value_path        TEXT,
            response_template TEXT,
            unit              TEXT,
            public_key        BLOB    NOT NULL,
            created_at        INTEGER NOT NULL
        )",
        [],
    )?;

    // Polls: interactive poll widgets (widget messages point here via
    // messages.widget = {"type":"poll","id":N}). Timestamps are unix seconds.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS polls (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            channel_id  INTEGER NOT NULL,
            message_id  INTEGER NOT NULL,
            creator     BLOB    NOT NULL,
            question    TEXT    NOT NULL,
            options     TEXT    NOT NULL,
            created_at  INTEGER NOT NULL,
            closes_at   INTEGER,
            closed_at   INTEGER
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS poll_votes (
            poll_id      INTEGER NOT NULL,
            voter        BLOB    NOT NULL,
            option_index INTEGER NOT NULL,
            voted_at     INTEGER NOT NULL,
            PRIMARY KEY (poll_id, voter)
        )",
        [],
    )?;

    // Giveaways: interactive giveaway widgets (widget messages point here via
    // messages.widget = {"type":"giveaway","id":N}). Timestamps are unix seconds.
    // No FK to messages: deleting the card cancels an open giveaway via the
    // DeleteMessage hook, rows retained (audit) — no cascade delete.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS giveaways (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            channel_id INTEGER NOT NULL,
            message_id INTEGER NOT NULL,
            creator    BLOB    NOT NULL,
            prize      TEXT    NOT NULL,
            ends_at    INTEGER NOT NULL,
            status     TEXT    NOT NULL DEFAULT 'open',
            winner     BLOB,
            created_at INTEGER NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS giveaway_entries (
            giveaway_id INTEGER NOT NULL,
            member      BLOB    NOT NULL,
            entered_at  INTEGER NOT NULL,
            PRIMARY KEY (giveaway_id, member)
        )",
        [],
    )?;

    // Personal reminders: private, per-owner nudges delivered as a DM from the
    // server system identity when they come due (`reminders.rs`, swept by
    // `widgets::sweep_once`). Nothing is ever posted in a channel; `owner` is the
    // only recipient AND the only reader. Timestamps are unix seconds.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS reminders (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            owner      BLOB    NOT NULL,
            channel_id INTEGER NOT NULL,
            text       TEXT    NOT NULL,
            created_at INTEGER NOT NULL,
            due_at     INTEGER NOT NULL,
            status     TEXT    NOT NULL DEFAULT 'pending',
            sent_at    INTEGER
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_reminders_due ON reminders(status, due_at)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_reminders_owner ON reminders(owner, status, due_at)",
        [],
    )?;

    // Channel events: RSVP event cards (widget messages point here via
    // messages.widget = {"type":"event","id":N}). NOTE the name — the `events`
    // table is the mesh signed log; these are the product's event cards.
    // `starts_at` is an ABSOLUTE unix second: no timezone is stored anywhere,
    // every client renders it in its own local time. The three nullable `*_at`
    // guard columns (`reminded_at`, `started_at`, `cancel_notified_at`) are what
    // make each sweeper action exactly-once. No FK to messages: deleting the card
    // CANCELS the event via the DeleteMessage hook, rows retained (audit).
    conn.execute(
        "CREATE TABLE IF NOT EXISTS channel_events (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            channel_id          INTEGER NOT NULL,
            message_id          INTEGER NOT NULL,
            creator             BLOB    NOT NULL,
            title               TEXT    NOT NULL,
            description         TEXT,
            location            TEXT,
            starts_at           INTEGER NOT NULL,
            remind_lead         INTEGER,
            reminded_at         INTEGER,
            status              TEXT    NOT NULL DEFAULT 'upcoming',
            started_at          INTEGER,
            cancelled_at        INTEGER,
            cancel_notified_at  INTEGER,
            created_at          INTEGER NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_channel_events_due ON channel_events(status, starts_at)",
        [],
    )?;
    // One RSVP per member at the schema level (the poll_votes idiom); changing
    // your mind is an upsert, clearing it is a DELETE.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS channel_event_rsvps (
            event_id   INTEGER NOT NULL,
            member     BLOB    NOT NULL,
            response   TEXT    NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (event_id, member)
        )",
        [],
    )?;

    // Bot alerts + subscriptions (price-alert engine, Task 2).
    conn.execute(
        "CREATE TABLE IF NOT EXISTS bot_alerts (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            bot_public_key BLOB    NOT NULL,
            metric         TEXT    NOT NULL,
            comparator     TEXT    NOT NULL,
            threshold      REAL    NOT NULL,
            armed          INTEGER NOT NULL DEFAULT 1,
            created_at     INTEGER NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS bot_subscriptions (
            bot_public_key        BLOB NOT NULL,
            subscriber_public_key BLOB NOT NULL,
            created_at            INTEGER NOT NULL,
            PRIMARY KEY (bot_public_key, subscriber_public_key)
        )",
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

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO server_settings (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

pub fn get_setting(conn: &Connection, key: &str) -> anyhow::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row("SELECT value FROM server_settings WHERE key = ?1", rusqlite::params![key], |r| r.get::<_, String>(0))
        .optional()?)
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

    #[test]
    fn events_and_genesis_schema_and_message_event_hash_migration() {
        let conn = open_in_memory().unwrap();
        // events table accepts a row.
        conn.execute(
            "INSERT INTO events (event_hash, server_id, author, device, seq, lamport, payload_type, channel_id, event_body, created_at) \
             VALUES ('h0','srv',X'00',  'd', 0, 1, 'MessagePosted', 1, X'01', 100)",
            [],
        ).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // genesis singleton.
        conn.execute("INSERT INTO genesis (singleton, genesis_body, server_id) VALUES (0, X'02', 'srv')", []).unwrap();
        assert!(conn.execute("INSERT INTO genesis (singleton, genesis_body, server_id) VALUES (0, X'03', 'srv')", []).is_err(),
            "genesis must be a singleton");
        // messages.event_hash column exists and defaults NULL.
        let has: bool = conn.prepare("PRAGMA table_info(messages)").unwrap()
            .query_map([], |r| r.get::<_, String>(1)).unwrap()
            .filter_map(|r| r.ok()).any(|c| c == "event_hash");
        assert!(has, "messages.event_hash column must exist");
    }
}
