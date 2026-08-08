//! Incoming webhook delivery: external HTTP POST → relay → server → posted message.
//!
//! Each webhook has a randomly-generated token (64-hex, 256-bit entropy) and a
//! per-webhook Ed25519 public key that is stored as the message author. The
//! author is NOT a roster member; `author_name_override` carries the display
//! name so clients can render it without a member lookup.
//!
//! Rate limiting (per-webhook / per-IP) is deferred to the relay side (Task 2).

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use farder_crypto::identity::{Keypair, PublicKey};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct WebhookRow {
    pub id: i64,
    pub channel_id: u64,
    pub name: String,
    pub public_key: PublicKey,
}

pub struct WebhookPayload {
    pub content: String,
    pub username: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum WebhookAck {
    Ok,
    Unauthorized,
    BadRequest,
    TooLarge,
}

// ---------------------------------------------------------------------------
// Parse
// ---------------------------------------------------------------------------

/// Parse a Discord-compatible webhook body. Requires a non-empty `content`
/// field; ignores all other fields except `username`. Rejects malformed JSON,
/// missing `content`, and whitespace-only content.
pub fn parse_webhook_payload(body: &[u8]) -> Result<WebhookPayload> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| anyhow::anyhow!("invalid json"))?;
    let content = v
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if content.is_empty() {
        bail!("missing or empty content");
    }
    let username = v
        .get("username")
        .and_then(|u| u.as_str())
        // Cap the display-name override (untrusted external input) at Discord's 80-char limit.
        .map(|s| s.trim().chars().take(80).collect::<String>())
        .filter(|s| !s.is_empty());
    Ok(WebhookPayload { content, username })
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

/// Generate a high-entropy 64-hex token from random Ed25519 key material.
fn gen_token() -> String {
    hex::encode(Keypair::generate().public_key().as_bytes())
}

/// Create a new webhook for `channel_id` with the given display `name`.
/// Returns `(id, token)` — the token is the caller's only chance to see it.
pub fn create(conn: &Connection, channel_id: u64, name: &str) -> Result<(i64, String)> {
    let token = gen_token();
    let pk = Keypair::generate().public_key();
    conn.execute(
        "INSERT INTO webhooks (channel_id, token, name, public_key, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            channel_id as i64,
            token,
            name,
            pk.as_bytes().as_slice(),
            crate::db::now() as i64,
        ],
    )?;
    Ok((conn.last_insert_rowid(), token))
}

/// Rotate the token for the webhook with the given `id`.
/// Returns `Some(new_token)` if the webhook exists, `None` otherwise.
pub fn regenerate_token(conn: &Connection, id: i64) -> Result<Option<String>> {
    let token = gen_token();
    let n = conn.execute(
        "UPDATE webhooks SET token = ?1 WHERE id = ?2",
        params![token, id],
    )?;
    Ok((n > 0).then_some(token))
}

/// Delete the webhook with the given `id`.
pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM webhooks WHERE id = ?1", params![id])?;
    Ok(())
}

/// List all webhooks for a channel (no token field — tokens are write-only after creation).
pub fn list_for_channel(conn: &Connection, channel_id: u64) -> Result<Vec<WebhookRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, channel_id, name, public_key FROM webhooks WHERE channel_id = ?1",
    )?;
    let rows = stmt.query_map(params![channel_id as i64], row_to_webhook)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Look up a webhook by its secret token (used during delivery).
pub fn find_by_token(conn: &Connection, token: &str) -> Result<Option<WebhookRow>> {
    conn.query_row(
        "SELECT id, channel_id, name, public_key FROM webhooks WHERE token = ?1",
        params![token],
        row_to_webhook,
    )
    .optional()
    .map_err(Into::into)
}

fn row_to_webhook(r: &rusqlite::Row) -> rusqlite::Result<WebhookRow> {
    let pk_b: Vec<u8> = r.get(3)?;
    let arr: [u8; 32] = pk_b
        .try_into()
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(WebhookRow {
        id: r.get(0)?,
        channel_id: r.get::<_, i64>(1)? as u64,
        name: r.get(2)?,
        public_key: PublicKey::from_bytes(arr),
    })
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

/// Validate and post a webhook payload as a message. `body` is the raw HTTP
/// request body (max 64 KiB). Returns an ack code the relay writes back.
///
/// The db lock is dropped **before** the broadcast await so no lock is held
/// across an async boundary.
pub async fn deliver(
    state: &std::sync::Arc<crate::state::ServerState>,
    token: &str,
    body: &[u8],
) -> WebhookAck {
    if body.len() > 64 * 1024 {
        return WebhookAck::TooLarge;
    }

    // All DB work is done inside a synchronous block so the MutexGuard is
    // dropped before we hit the broadcast await.
    let (channel_id, message) = {
        let conn = state.db.lock().unwrap();

        let Some(wh) = find_by_token(&conn, token).ok().flatten() else {
            return WebhookAck::Unauthorized;
        };

        // Class gate. `CreateWebhook` already refuses an E2EE channel, but a
        // token issued before a class change (or a channel row that has become
        // unresolvable) must not deliver either — the choke point in
        // `messages.rs` would hard-error, and a hard error is not an answer to
        // give an external HTTP caller.
        //
        // The ack is the EXISTING opaque `Unauthorized`, deliberately identical
        // to a bad token: a distinct "encrypted channel" ack would let anyone
        // holding a token — or spraying tokens — classify channels from outside
        // the server. FAIL CLOSED: an unresolvable class refuses.
        if crate::channel_class::resolve(&conn, wh.channel_id).refuses_server_authored_content() {
            return WebhookAck::Unauthorized;
        }

        let payload = match parse_webhook_payload(body) {
            Ok(p) => p,
            Err(_) => return WebhookAck::BadRequest,
        };

        // Cap content to 8000 chars (Discord's limit; good default).
        let content: String = payload.content.chars().take(8000).collect();
        // Prefer the per-delivery username over the registered webhook name.
        let display = payload.username.unwrap_or_else(|| wh.name.clone());

        let mid = match crate::messages::insert_message_with_author_name(
            &conn,
            wh.channel_id,
            &wh.public_key,
            &content,
            None,
            Some(&display),
            Some("WEBHOOK"),
        ) {
            Ok(m) => m,
            Err(_) => return WebhookAck::BadRequest,
        };

        let message = match crate::messages::get_message(&conn, mid, &wh.public_key) {
            Ok(Some(m)) => m,
            _ => return WebhookAck::BadRequest,
        };

        (wh.channel_id, message)
    };
    // Lock is released here — safe to await.

    crate::connection::broadcast_event(
        state,
        crate::events::EventTarget::Subscribers(channel_id),
        farder_protocol::server::ServerEvent::NewMessage { message },
    )
    .await;

    WebhookAck::Ok
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_content_ignores_extras() {
        let p = parse_webhook_payload(
            br#"{"content":"hi","username":"CI","embeds":[{"x":1}]}"#,
        )
        .unwrap();
        assert_eq!(p.content, "hi");
        assert_eq!(p.username.as_deref(), Some("CI"));

        let p2 = parse_webhook_payload(br#"{"content":"only"}"#).unwrap();
        assert_eq!(p2.content, "only");
        assert!(p2.username.is_none());
    }

    #[test]
    fn parse_caps_username_length() {
        let long = "x".repeat(500);
        let body = format!(r#"{{"content":"c","username":"{long}"}}"#);
        let p = parse_webhook_payload(body.as_bytes()).unwrap();
        assert_eq!(p.username.as_deref().map(|u| u.chars().count()), Some(80));
    }

    #[test]
    fn parse_rejects_missing_or_empty_content() {
        assert!(parse_webhook_payload(br#"{"username":"x"}"#).is_err());
        assert!(parse_webhook_payload(br#"{"content":""}"#).is_err());
        assert!(parse_webhook_payload(b"not json").is_err());
    }

    #[test]
    fn create_find_delete_roundtrip() {
        let conn = crate::db::open_in_memory().unwrap();
        // Create a channel to satisfy the foreign-key intent (webhooks table
        // has no FK, but we want a real channel_id for the query).
        let ch = crate::channels::create_channel(
            &conn,
            "gen",
            farder_protocol::server::ChannelType::Text,
            None,
            0,
        )
        .unwrap();

        let (id, token) = create(&conn, ch, "GH").unwrap();
        let wh = find_by_token(&conn, &token).unwrap().unwrap();
        assert_eq!(wh.channel_id, ch);
        assert_eq!(wh.name, "GH");

        // Wrong token returns None.
        assert!(find_by_token(&conn, "wrong").unwrap().is_none());

        // list_for_channel returns it.
        let list = list_for_channel(&conn, ch).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);

        // Regenerate token — old token no longer works.
        let new_token = regenerate_token(&conn, id).unwrap().unwrap();
        assert_ne!(new_token, token);
        assert!(find_by_token(&conn, &token).unwrap().is_none());
        assert!(find_by_token(&conn, &new_token).unwrap().is_some());

        delete(&conn, id).unwrap();
        assert!(find_by_token(&conn, &new_token).unwrap().is_none());
    }

    /// Rung 2: an external HTTP caller holding a VALID token for a channel that
    /// has become E2EE-class must be refused, and must write nothing.
    ///
    /// This is the case `CreateWebhook`'s refusal alone cannot cover: a token
    /// minted while the channel was plaintext. The ack is the existing opaque
    /// `Unauthorized` — byte-identical to a bad token — so token-holders and
    /// token-sprayers alike cannot classify channels from outside the server.
    #[tokio::test]
    async fn webhook_delivery_into_an_e2ee_channel_is_refused_and_writes_nothing() {
        let state = std::sync::Arc::new(crate::state::ServerState::new_for_test().unwrap());
        let (ch, token) = {
            let conn = state.db.lock().unwrap();
            let ch = crate::channels::create_channel(
                &conn,
                "gen",
                farder_protocol::server::ChannelType::Text,
                None,
                0,
            )
            .unwrap();
            let (_id, token) = create(&conn, ch, "CI").unwrap();
            (ch, token)
        };

        // Control FIRST, while the channel is still plaintext: delivery works,
        // so a later failure is the class gate and not a broken fixture.
        let ack = deliver(&state, &token, br#"{"content":"before"}"#).await;
        assert!(matches!(ack, WebhookAck::Ok), "plaintext control must deliver");
        {
            let conn = state.db.lock().unwrap();
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE channel_id = ?1",
                    rusqlite::params![ch as i64],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1);
            // The token survives the class change — only the class stops it.
            crate::channel_class::set_class(
                &conn,
                ch,
                farder_crypto::event_log::ChannelClass::E2ee,
            )
            .unwrap();
            assert!(find_by_token(&conn, &token).unwrap().is_some());
        }

        let ack = deliver(&state, &token, br#"{"content":"SECRET-NEEDLE"}"#).await;
        assert!(
            matches!(ack, WebhookAck::Unauthorized),
            "a sealed channel must answer exactly like a bad token, got {ack:?}"
        );

        // Observation: the payload reached no storage at all — not the message
        // table, not the FTS index. (Scoped block: the guard must not survive
        // to the next `.await`.)
        {
            let conn = state.db.lock().unwrap();
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE channel_id = ?1",
                    rusqlite::params![ch as i64],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "the refused delivery added no row");
            let leaked: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE content LIKE '%SECRET-NEEDLE%'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(leaked, 0, "webhook plaintext must not be anywhere in messages");
            let fts: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'SECRET'",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            assert_eq!(fts, 0, "webhook plaintext must not enter the search index");
        }

        // And a bad token is indistinguishable from the refusal above.
        let bad = deliver(&state, "not-a-token", br#"{"content":"x"}"#).await;
        assert_eq!(format!("{bad:?}"), format!("{:?}", WebhookAck::Unauthorized));
    }
}
