use anyhow::Result;
use farder_crypto::identity::PublicKey;
use rand::Rng;
use rusqlite::{params, Connection, OptionalExtension};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn random_code() -> String {
    // 8 chars from an alphanumeric charset that excludes visually confusable
    // characters (0/O, 1/I/l).
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    (0..8)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
        .collect()
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct InviteInfo {
    pub code: String,
    pub target_channel: Option<u64>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create an invite that expires `expires_in_secs` seconds from now (if given).
pub fn create_invite(
    conn: &Connection,
    created_by: &PublicKey,
    max_uses: Option<u32>,
    expires_in_secs: Option<u64>,
    target_channel: Option<u64>,
) -> Result<String> {
    let expires_at = expires_in_secs.map(|s| now() + s);
    create_invite_with_expires(conn, created_by, max_uses, expires_at, target_channel)
}

/// Create an invite with an absolute expiry timestamp.
pub fn create_invite_with_expires(
    conn: &Connection,
    created_by: &PublicKey,
    max_uses: Option<u32>,
    expires_at: Option<u64>,
    target_channel: Option<u64>,
) -> Result<String> {
    let code = random_code();
    conn.execute(
        "INSERT INTO invites (code, created_by, max_uses, expires_at, target_channel) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            &code,
            created_by.as_bytes().as_slice(),
            max_uses.map(|v| v as i64),
            expires_at.map(|v| v as i64),
            target_channel.map(|v| v as i64),
        ],
    )?;
    Ok(code)
}

/// Check whether an invite code is currently valid.
///
/// Returns:
/// - `Ok(Ok(InviteInfo))` — the invite is valid and usable.
/// - `Ok(Err(reason))` — the invite exists but is invalid (expired / used up / not found).
/// - `Err(e)` — a database error occurred.
pub fn validate_invite(conn: &Connection, code: &str) -> Result<Result<InviteInfo, String>> {
    let row = conn
        .query_row(
            "SELECT code, max_uses, use_count, expires_at, target_channel \
             FROM invites WHERE code = ?1",
            params![code],
            |row| {
                let code: String = row.get(0)?;
                let max_uses: Option<i64> = row.get(1)?;
                let use_count: i64 = row.get(2)?;
                let expires_at: Option<i64> = row.get(3)?;
                let target_channel: Option<i64> = row.get(4)?;
                Ok((code, max_uses, use_count, expires_at, target_channel))
            },
        )
        .optional()?;

    let (code, max_uses, use_count, expires_at, target_channel) = match row {
        None => return Ok(Err("invite not found".to_string())),
        Some(r) => r,
    };

    // Check expiry.
    if let Some(exp) = expires_at {
        if now() >= exp as u64 {
            return Ok(Err("invite has expired".to_string()));
        }
    }

    // Check max uses.
    if let Some(max) = max_uses {
        if use_count >= max {
            return Ok(Err("invite has reached its maximum uses".to_string()));
        }
    }

    Ok(Ok(InviteInfo {
        code,
        target_channel: target_channel.map(|v| v as u64),
    }))
}

/// Validate an invite and, if valid, atomically increment its use counter.
///
/// Returns the same inner `Result<InviteInfo, String>` convention as
/// `validate_invite`.
pub fn use_invite(conn: &Connection, code: &str) -> Result<Result<InviteInfo, String>> {
    // Validate first (read-only path).
    let info = match validate_invite(conn, code)? {
        Ok(info) => info,
        Err(reason) => return Ok(Err(reason)),
    };

    // Increment use_count.
    conn.execute(
        "UPDATE invites SET use_count = use_count + 1 WHERE code = ?1",
        params![code],
    )?;

    Ok(Ok(info))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::members::register_member;
    use farder_crypto::identity::Keypair;

    fn gen_pk() -> PublicKey {
        Keypair::generate().public_key()
    }

    /// Register a throwaway member so FK constraints (if any) are satisfied and
    /// return their public key for use as the `created_by` field.
    fn setup_creator(conn: &Connection) -> PublicKey {
        let pk = gen_pk();
        register_member(conn, &pk, "TestCreator").unwrap();
        pk
    }

    // -----------------------------------------------------------------------
    // test_create_and_validate_invite
    // -----------------------------------------------------------------------

    #[test]
    fn test_create_and_validate_invite() {
        let conn = db::open_in_memory().unwrap();
        let pk = setup_creator(&conn);

        let code = create_invite(&conn, &pk, None, None, None).unwrap();

        // Code must be 8 characters long.
        assert_eq!(code.len(), 8, "invite code should be 8 chars, got {:?}", code);

        // Validate should succeed.
        let result = validate_invite(&conn, &code).unwrap();
        assert!(result.is_ok(), "expected valid invite, got: {:?}", result.err());
        let info = result.unwrap();
        assert_eq!(info.code, code);
        assert!(info.target_channel.is_none());
    }

    // -----------------------------------------------------------------------
    // test_invite_with_max_uses
    // -----------------------------------------------------------------------

    #[test]
    fn test_invite_with_max_uses() {
        let conn = db::open_in_memory().unwrap();
        let pk = setup_creator(&conn);

        // max_uses = 2 — first two uses succeed, third fails.
        let code = create_invite(&conn, &pk, Some(2), None, None).unwrap();

        // First use.
        let r1 = use_invite(&conn, &code).unwrap();
        assert!(r1.is_ok(), "first use should succeed");

        // Second use.
        let r2 = use_invite(&conn, &code).unwrap();
        assert!(r2.is_ok(), "second use should succeed");

        // Third use — should fail.
        let r3 = use_invite(&conn, &code).unwrap();
        assert!(
            r3.is_err(),
            "third use should fail (max_uses reached), but got Ok"
        );
    }

    // -----------------------------------------------------------------------
    // test_invite_expired
    // -----------------------------------------------------------------------

    #[test]
    fn test_invite_expired() {
        let conn = db::open_in_memory().unwrap();
        let pk = setup_creator(&conn);

        // expires_at = 1 (well in the past).
        let code =
            create_invite_with_expires(&conn, &pk, None, Some(1), None).unwrap();

        let result = validate_invite(&conn, &code).unwrap();
        assert!(
            result.is_err(),
            "expired invite should be invalid, got Ok"
        );
        let reason = result.unwrap_err();
        assert!(
            reason.contains("expired"),
            "expected 'expired' in reason, got: {:?}",
            reason
        );
    }

    // -----------------------------------------------------------------------
    // test_invite_not_found
    // -----------------------------------------------------------------------

    #[test]
    fn test_invite_not_found() {
        let conn = db::open_in_memory().unwrap();

        let result = validate_invite(&conn, "NOTEXIST").unwrap();
        assert!(
            result.is_err(),
            "nonexistent invite should be invalid, got Ok"
        );
        let reason = result.unwrap_err();
        assert!(
            reason.contains("not found"),
            "expected 'not found' in reason, got: {:?}",
            reason
        );
    }

    // -----------------------------------------------------------------------
    // test_invite_with_target_channel
    // -----------------------------------------------------------------------

    #[test]
    fn test_invite_with_target_channel() {
        let conn = db::open_in_memory().unwrap();
        let pk = setup_creator(&conn);

        let code = create_invite(&conn, &pk, None, None, Some(42)).unwrap();

        let result = validate_invite(&conn, &code).unwrap();
        assert!(result.is_ok(), "invite with target_channel should be valid");
        let info = result.unwrap();
        assert_eq!(
            info.target_channel,
            Some(42),
            "target_channel should be 42"
        );
    }
}
