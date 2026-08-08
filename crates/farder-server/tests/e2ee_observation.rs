//! Observation tests: no plaintext reaches an E2EE channel, on any path.
//!
//! CLAUDE.md requires privacy guarantees be verified by OBSERVING bytes, not by
//! reading code. **A test that asserts a function was called does not count.**
//! So every test here drives the REAL path — the same `handle_request` the wire
//! calls, the same sweeper the runtime ticks — and then observes the bytes that
//! actually landed.
//!
//! The observer ([`assert_no_plaintext_anywhere`]) enumerates the schema from
//! `sqlite_master` at runtime and scans EVERY value of EVERY row of EVERY table
//! at the byte level. That matters more than the specific tests: a path added
//! later that writes into a NEW table is caught without anyone remembering to
//! extend a list, and a needle buried inside a serialized blob (`events.event_body`,
//! a widget's JSON) trips it just as loudly as one in a `content` column.
//!
//! These sit alongside the unit tests in `handlers.rs`, which pin the refusal
//! STRINGS. This file pins the thing the strings are supposed to guarantee.

use farder_crypto::event_log::ChannelClass;
use farder_crypto::identity::{Keypair, PublicKey};
use farder_protocol::server::{ChannelType, ServerRequest, ServerResponse};
use farder_server::state::ServerState;
use farder_server::{channel_class, channels, db, handlers, members, messages, permissions};
use rusqlite::Connection;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// The observer
// ---------------------------------------------------------------------------

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && needle.len() <= haystack.len()
        && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Every table in the database, read from the schema rather than a hand-written
/// list, so a table added by a future feature is scanned automatically.
fn all_tables(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.map(|r| r.unwrap())
        // FTS5 shadow tables reject a bare `SELECT *`; the FTS content itself is
        // reached through the `messages_fts` virtual table, which IS scanned.
        .filter(|t| !t.ends_with("_data") && !t.ends_with("_idx") && !t.ends_with("_docsize")
            && !t.ends_with("_config") && !t.ends_with("_content"))
        .collect()
}

/// **The observation.** Assert `needle` appears nowhere in the database, at the
/// byte level, in any table, column, or serialized blob.
fn assert_no_plaintext_anywhere(conn: &Connection, needle: &str) {
    let needle_bytes = needle.as_bytes();
    for table in all_tables(conn) {
        let sql = format!("SELECT * FROM \"{table}\"");
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            // A virtual table that cannot be scanned this way cannot be a hiding
            // place we can inspect either; skipping silently would be the wrong
            // answer, so make it loud.
            Err(e) => panic!("cannot scan table {table}: {e}"),
        };
        let col_count = stmt.column_count();
        let col_names: Vec<String> = (0..col_count)
            .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
            .collect();
        let mut rows = stmt.query([]).unwrap();
        let mut row_idx = 0usize;
        while let Some(row) = rows.next().unwrap() {
            for (i, col) in col_names.iter().enumerate() {
                let bytes: Option<Vec<u8>> = match row.get_ref(i).unwrap() {
                    rusqlite::types::ValueRef::Text(t) => Some(t.to_vec()),
                    rusqlite::types::ValueRef::Blob(b) => Some(b.to_vec()),
                    _ => None,
                };
                if let Some(b) = bytes {
                    assert!(
                        !contains_subslice(&b, needle_bytes),
                        "PLAINTEXT LEAK: {needle:?} found in {table}.{col} (row {row_idx})"
                    );
                }
            }
            row_idx += 1;
        }
    }
}

/// Self-check: the observer must actually be able to FIND a needle, or every
/// test in this file is vacuously green. Runs first in spirit, and is the reason
/// the other assertions mean anything.
#[test]
fn the_observer_finds_a_needle_that_is_really_there() {
    let (conn, owner, _state) = setup();
    let plain = make_channel(&conn);
    let needle = "CANARY-observer-self-check-9f3a";
    messages::insert_message(&conn, plain, &owner, needle, None).unwrap();

    let found = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_no_plaintext_anywhere(&conn, needle)
    }))
    .is_err();
    assert!(found, "the observer failed to find a needle written in plaintext");
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn setup() -> (Connection, PublicKey, Arc<ServerState>) {
    let conn = db::open_in_memory().unwrap();
    let everyone = members::create_role(
        &conn,
        "@everyone",
        permissions::DEFAULT_EVERYONE,
        None,
        0,
        true,
        false,
    )
    .unwrap();
    let owner = Keypair::generate().public_key();
    members::register_member(&conn, &owner, "Owner").unwrap();
    members::assign_role(&conn, &owner, everyone).unwrap();

    // A v2-negotiated connection throughout: refusing an OLD client is the easy
    // half. Every assertion here is about a fully capable, E2EE-aware client
    // still being unable to put plaintext into a sealed channel.
    let state = Arc::new(ServerState::new_for_test().unwrap());
    state
        .client_protocol
        .write()
        .unwrap()
        .insert(*owner.as_bytes(), farder_protocol::server::SERVER_PROTOCOL_VERSION);
    (conn, owner, state)
}

fn make_channel(conn: &Connection) -> u64 {
    channels::create_channel(conn, "general", ChannelType::Text, None, 0).unwrap()
}

fn make_e2ee_channel(conn: &Connection) -> u64 {
    let id = channels::create_channel(conn, "sealed", ChannelType::Text, None, 0).unwrap();
    channel_class::set_class(conn, id, ChannelClass::E2ee).unwrap();
    id
}

fn run(
    conn: &Connection,
    owner: &PublicKey,
    state: &Arc<ServerState>,
    req: ServerRequest,
) -> ServerResponse {
    handlers::handle_request(conn, owner, true, req, "", state)
        .unwrap()
        .response
}

fn is_error(resp: &ServerResponse) -> bool {
    matches!(resp, ServerResponse::Error { .. })
}

fn row_count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |r| r.get(0))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Server-authored write paths — each drives the REAL request, then observes
// ---------------------------------------------------------------------------

/// The needle every "writes nothing" test hunts for. Long and distinctive so a
/// chance collision inside a hash or a serialized blob is effectively zero.
const NEEDLE: &str = "FARDER-CANARY-plaintext-must-never-land-here-7b21e9";

#[test]
fn legacy_send_writes_nothing_into_an_e2ee_channel() {
    let (conn, owner, state) = setup();
    let sealed = make_e2ee_channel(&conn);

    let resp = run(
        &conn,
        &owner,
        &state,
        ServerRequest::SendMessage {
            channel_id: sealed,
            content: NEEDLE.to_string(),
            reply_to: None,
            attachment_ids: vec![],
        },
    );
    assert!(is_error(&resp), "a plaintext send into a sealed channel must be refused");
    assert_no_plaintext_anywhere(&conn, NEEDLE);
    assert_eq!(row_count(&conn, "messages"), 0);
}

/// The interesting half of the edit path: the message EXISTS (posted while the
/// channel was plaintext) and the channel is sealed afterwards. The gate must
/// resolve the class from the ROW's channel, not from anything the request said.
#[test]
fn legacy_edit_writes_nothing_into_an_e2ee_channel() {
    let (conn, owner, state) = setup();
    let channel = make_channel(&conn);
    let id = messages::insert_message(&conn, channel, &owner, "before", None).unwrap();
    channel_class::set_class(&conn, channel, ChannelClass::E2ee).unwrap();

    let resp = run(
        &conn,
        &owner,
        &state,
        ServerRequest::EditMessage { message_id: id, new_content: NEEDLE.to_string() },
    );
    assert!(is_error(&resp), "an edit into a now-sealed channel must be refused");
    assert_no_plaintext_anywhere(&conn, NEEDLE);
    let content: String = conn
        .query_row("SELECT content FROM messages WHERE id = ?1", [id as i64], |r| r.get(0))
        .unwrap();
    assert_eq!(content, "before", "the refused edit rewrote nothing");
}

/// A reaction is a tiny write, but it is still server-readable content ABOUT a
/// sealed message — and the reactor's identity is metadata the channel did not
/// consent to publish. Observe that neither the emoji nor the reactor lands.
#[test]
fn reaction_attempt_writes_nothing_and_reveals_no_reactor() {
    let (conn, owner, state) = setup();
    let channel = make_channel(&conn);
    let id = messages::insert_message(&conn, channel, &owner, "m", None).unwrap();
    channel_class::set_class(&conn, channel, ChannelClass::E2ee).unwrap();

    let resp = run(
        &conn,
        &owner,
        &state,
        ServerRequest::AddReaction {
            message_id: id,
            emoji: NEEDLE.to_string(),
            file_id: None,
        },
    );
    assert!(is_error(&resp));
    assert_no_plaintext_anywhere(&conn, NEEDLE);
    assert_eq!(row_count(&conn, "reactions"), 0, "no reactor identity was recorded");
}

#[test]
fn thread_create_writes_no_plaintext_child_under_a_sealed_parent() {
    let (conn, owner, state) = setup();
    let channel = make_channel(&conn);
    let id = messages::insert_message(&conn, channel, &owner, "parent", None).unwrap();
    channel_class::set_class(&conn, channel, ChannelClass::E2ee).unwrap();

    let before = row_count(&conn, "channels");
    let resp = run(
        &conn,
        &owner,
        &state,
        ServerRequest::CreateThread { message_id: id, name: Some(NEEDLE.to_string()) },
    );
    assert!(is_error(&resp));
    assert_no_plaintext_anywhere(&conn, NEEDLE);
    assert_eq!(
        row_count(&conn, "channels"),
        before,
        "a thread child channel was created under a sealed parent"
    );
}

/// Every slash-command kind, including the two that post NO message at all
/// (`reminder` stores `reminders.text` server-side; `api` stores a fetched
/// body). A message-count assertion would miss both — the byte scan does not.
#[test]
fn slash_command_of_every_kind_writes_nothing_into_an_e2ee_channel() {
    let (conn, owner, state) = setup();
    let sealed = make_e2ee_channel(&conn);

    for kind in ["text", "api", "poll", "giveaway", "event", "reminder"] {
        let resp = run(
            &conn,
            &owner,
            &state,
            ServerRequest::AddCommand {
                name: format!("cmd {kind}"),
                trigger: kind.to_string(),
                description: String::new(),
                kind: kind.to_string(),
                body_text: Some("body".to_string()),
                url_template: Some("https://example.com/x".to_string()),
                value_path: Some("a.b".to_string()),
                response_template: None,
                unit: None,
            },
        );
        assert!(!is_error(&resp), "AddCommand {kind} should succeed: {resp:?}");

        let resp = run(
            &conn,
            &owner,
            &state,
            ServerRequest::RunCommand {
                trigger: kind.to_string(),
                channel_id: sealed,
                args: NEEDLE.to_string(),
            },
        );
        assert!(is_error(&resp), "/{kind} in a sealed channel must be refused");
        assert_no_plaintext_anywhere(&conn, NEEDLE);
    }

    assert_eq!(row_count(&conn, "messages"), 0);
    assert_eq!(row_count(&conn, "reminders"), 0, "no plaintext reminder text stored");
    assert_eq!(row_count(&conn, "polls"), 0);
    assert_eq!(row_count(&conn, "giveaways"), 0);
    assert_eq!(row_count(&conn, "channel_events"), 0);
}

#[test]
fn webhook_creation_writes_nothing_for_an_e2ee_channel() {
    let (conn, owner, state) = setup();
    let sealed = make_e2ee_channel(&conn);

    let resp = run(
        &conn,
        &owner,
        &state,
        ServerRequest::CreateWebhook { channel_id: sealed, name: NEEDLE.to_string() },
    );
    assert!(is_error(&resp), "a webhook bound to a sealed channel must be refused");
    assert_no_plaintext_anywhere(&conn, NEEDLE);
    assert_eq!(
        row_count(&conn, "webhooks"),
        0,
        "a webhook row is a standing plaintext delivery route into the channel"
    );
}

#[test]
fn fetch_url_stores_no_blob_for_an_e2ee_channel() {
    let (conn, owner, state) = setup();
    let sealed = make_e2ee_channel(&conn);

    let resp = run(
        &conn,
        &owner,
        &state,
        ServerRequest::FetchUrl {
            url: format!("https://example.com/{NEEDLE}"),
            channel_id: sealed,
        },
    );
    assert!(is_error(&resp), "FetchUrl into a sealed channel must be refused");
    assert_no_plaintext_anywhere(&conn, NEEDLE);
    assert_eq!(row_count(&conn, "files"), 0, "no fetched blob was stored");
}

/// Search is the one surface whose entire job is reading content, so it gets the
/// harshest version of the test: a sealed row is planted AND a matching FTS index
/// entry is planted for it — the exact state a corrupted or hostile index would
/// be in — and search must STILL not surface it.
///
/// That is what the `AND is_e2ee = 0` belt-and-braces in `search_messages` is
/// for. The FTS skip on the write side is the primary guard (pinned by
/// `a_sealed_row_never_enters_the_fts_index` in `event_ingest.rs`, which can
/// reach the `pub(crate)` sealed door); this test proves the SECOND guard holds
/// on its own, with the first one deliberately defeated.
#[test]
fn a_sealed_row_is_unreachable_by_search_even_with_a_poisoned_fts_index() {
    let (conn, owner, state) = setup();
    let sealed_channel = make_e2ee_channel(&conn);
    // Alphanumeric: FTS5 parses its query string, and punctuation in a term is a
    // syntax error rather than a miss.
    let term = "canarysealedneedle7b21e9";

    // Plant the sealed row directly. This is FIXTURE setup, not a bypass of the
    // thing under test: it fabricates the state the sealed derive door produces
    // (`content = ''`, ciphertext in `sealed`, `is_e2ee = 1`).
    conn.execute(
        "INSERT INTO messages (channel_id, author, content, timestamp, pinned, is_e2ee, sealed) \
         VALUES (?1, ?2, '', 1, 0, 1, ?3)",
        rusqlite::params![
            sealed_channel as i64,
            owner.as_bytes().as_slice(),
            term.as_bytes()
        ],
    )
    .unwrap();
    let id: i64 = conn
        .query_row("SELECT id FROM messages WHERE is_e2ee = 1", [], |r| r.get(0))
        .unwrap();

    // Now POISON the index: give the sealed row a real, matching FTS entry, as
    // if the write-side skip had failed or been tampered with.
    conn.execute(
        "INSERT INTO messages_fts (rowid, content) VALUES (?1, ?2)",
        rusqlite::params![id, term],
    )
    .unwrap();
    let matches: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH ?1",
            [term],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(matches, 1, "the index really is poisoned, or this test proves nothing");

    // OBSERVATION: search still cannot reach it, server-wide or channel-scoped.
    for scope in [None, Some(sealed_channel)] {
        let resp = run(
            &conn,
            &owner,
            &state,
            ServerRequest::Search { query: term.to_string(), channel_id: scope, limit: 50 },
        );
        match resp {
            ServerResponse::SearchResults { messages } => assert!(
                messages.is_empty(),
                "search surfaced a sealed row (scope {scope:?}): {messages:?}"
            ),
            other => panic!("expected SearchResults, got {other:?}"),
        }
    }
}

/// The host-injection case, at the integration level: there is no public door
/// into a sealed channel that accepts a plaintext body. Every `insert_message*`
/// entry point is tried against a real sealed channel and must refuse.
///
/// This is what covers every server-authored writer at once — bot alert DMs,
/// reminder DMs and the server's own system identity included. Those paths have
/// no user request to refuse and no special-case gate of their own; they are safe
/// because the doors they must go through are AUTHOR-AGNOSTIC, which is why the
/// author here is the server's own system identity rather than a member. A
/// writer added later inherits the same refusal without being edited, and the
/// source-level guard `no_insert_into_messages_sql_outside_the_choke_point`
/// stops one from being added that skips the doors entirely.
#[test]
fn no_public_message_door_accepts_plaintext_into_a_sealed_channel() {
    let (conn, _owner, _state) = setup();
    let sealed = make_e2ee_channel(&conn);
    let author = farder_server::bots::get_or_create_system_identity(&conn).unwrap();
    assert!(farder_server::bots::is_system_identity(&conn, &author).unwrap());

    assert!(messages::insert_message(&conn, sealed, &author, NEEDLE, None).is_err());
    assert!(
        messages::insert_message_with_ts(&conn, sealed, &author, NEEDLE, None, 123).is_err()
    );
    assert!(messages::insert_message_with_author_name(
        &conn,
        sealed,
        &author,
        NEEDLE,
        None,
        Some(NEEDLE),
        Some("WEBHOOK"),
    )
    .is_err());

    assert_no_plaintext_anywhere(&conn, NEEDLE);
    assert_eq!(row_count(&conn, "messages"), 0);
}

/// Retention and anonymization are moderation mechanisms that must keep working
/// in a sealed channel — and they do, because they operate on the ROW, never on
/// its meaning. Observe both: the ciphertext is gone afterwards, and neither
/// mechanism ever had to read it.
#[test]
fn retention_and_anonymize_operate_on_ciphertext_without_reading_it() {
    let (conn, owner, _state) = setup();
    let sealed_channel = make_e2ee_channel(&conn);
    let ciphertext = b"\x00\x01\x02sealed-bytes-canary-7b21e9\xff\xfe";

    conn.execute(
        "INSERT INTO messages (channel_id, author, content, timestamp, pinned, is_e2ee, sealed) \
         VALUES (?1, ?2, '', 100, 0, 1, ?3)",
        rusqlite::params![
            sealed_channel as i64,
            owner.as_bytes().as_slice(),
            ciphertext.as_slice()
        ],
    )
    .unwrap();
    assert_eq!(row_count(&conn, "messages"), 1);

    // 1. Anonymization: the AUTHOR is scrubbed, and a sealed row's ciphertext is
    //    left intact rather than being replaced with a plaintext "[deleted]"
    //    marker the host would be writing into a channel it cannot read.
    messages::anonymize_messages_by_author(&conn, &owner).unwrap();
    let (content, sealed_after): (String, Option<Vec<u8>>) = conn
        .query_row("SELECT content, sealed FROM messages", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(content, "", "a sealed row must not gain a plaintext body");
    assert_eq!(
        sealed_after.as_deref(),
        Some(ciphertext.as_slice()),
        "anonymization rewrote ciphertext it cannot read"
    );

    // 2. Retention: the row goes, ciphertext and all.
    let purged = messages::delete_messages_before(&conn, sealed_channel, 1_000).unwrap();
    assert_eq!(purged, 1);
    assert_eq!(row_count(&conn, "messages"), 0);
    assert_no_plaintext_anywhere(&conn, "sealed-bytes-canary-7b21e9");
}

/// The sweeper is the one writer with no user request behind it — nobody is
/// there to be told "refused" — so it gets its own observation.
///
/// The event row is planted directly, because the real `/event` path already
/// refuses a sealed channel (see the slash-command test): this is the DRIFT case,
/// where a row exists that the current rules would never have created. The
/// sweeper must still refuse to announce into it, and must not flip its status
/// either — the flip and the announcement share a transaction, so a status flip
/// with no announcement would strand the event forever.
#[test]
fn the_sweeper_announces_nothing_into_a_sealed_channel_even_on_drift() {
    let (mut conn, owner, _state) = setup();
    let title = "canary-drifted-event-title-7b21e9";
    let now = 1_000_000u64;

    // Build the event while the channel is PLAINTEXT — through the real doors,
    // so the fixture is a state the server genuinely produces — then reclassify.
    // That is the drift: a row that exists which today's rules would refuse.
    let sealed_channel = make_channel(&conn);
    let card = messages::insert_message(&conn, sealed_channel, &owner, "card", None).unwrap();
    conn.execute(
        "INSERT INTO channel_events \
         (channel_id, message_id, creator, title, description, location, starts_at, status, created_at) \
         VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, 'upcoming', 1)",
        rusqlite::params![
            sealed_channel as i64,
            card as i64,
            owner.as_bytes().as_slice(),
            title,
            (now - 5) as i64
        ],
    )
    .unwrap();
    channel_class::set_class(&conn, sealed_channel, ChannelClass::E2ee).unwrap();

    let out = farder_server::widgets::sweep_once(&mut conn, now);

    assert!(
        out.broadcasts.is_empty(),
        "the sweeper broadcast into a sealed channel: {:?}",
        out.broadcasts.len()
    );
    assert_eq!(row_count(&conn, "messages"), 1, "an announcement row landed beside the card");
    let status: String = conn
        .query_row("SELECT status FROM channel_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        status, "upcoming",
        "the status flipped without the announcement it shares a transaction with"
    );

    // The event title is the only place this string is allowed to live: the row
    // that was planted. Nothing may have COPIED it into the channel.
    let copies: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE content LIKE ?1",
            [format!("%{title}%")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(copies, 0);
}
