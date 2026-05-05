use anyhow::Result;
use farder_crypto::identity::PublicKey;
use rusqlite::Connection;

// ---------------------------------------------------------------------------
// Token / challenge generators
// ---------------------------------------------------------------------------

pub fn generate_challenge() -> [u8; 32] {
    rand::random()
}

pub fn generate_session_token() -> [u8; 32] {
    rand::random()
}

pub fn generate_setup_token() -> [u8; 32] {
    rand::random()
}

// ---------------------------------------------------------------------------
// Signature verification
// ---------------------------------------------------------------------------

pub fn verify_challenge(public_key: &PublicKey, challenge: &[u8], signature: &[u8]) -> Result<()> {
    public_key.verify(challenge, signature)
}

// ---------------------------------------------------------------------------
// New-member authentication
// ---------------------------------------------------------------------------

/// Attempt to authenticate and register a new member.
///
/// Returns:
/// - `Ok(Ok(true))` — authenticated via auto-claim (first member on empty server).
/// - `Ok(Ok(false))` — authenticated successfully via setup token or invite code.
/// - `Ok(Err(reason))` — authentication rejected (bad token, bad invite, etc.).
/// - `Err(e)` — unexpected database or other error.
pub fn authenticate_new_member(
    conn: &Connection,
    public_key: &PublicKey,
    display_name: &str,
    invite_code: Option<&str>,
    setup_token_hex: Option<&str>,
    active_setup_token: Option<&[u8; 32]>,
) -> Result<Result<bool, String>> {
    if let Some(provided_hex) = setup_token_hex {
        // Setup-token path (first owner claim).
        match active_setup_token {
            None => {
                // No active token means the server already has an owner.
                return Ok(Err("server already has an owner".to_string()));
            }
            Some(token) => {
                let expected_hex = hex::encode(token);
                if provided_hex != expected_hex {
                    return Ok(Err("invalid setup token".to_string()));
                }
                crate::members::register_member(conn, public_key, display_name)?;
                let everyone_id: Option<u64> = conn.query_row(
                    "SELECT id FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).ok();
                if let Some(eid) = everyone_id {
                    crate::members::assign_role(conn, public_key, eid)?;
                }
                return Ok(Ok(false));
            }
        }
    }

    if let Some(code) = invite_code {
        // Invite-code path.
        match crate::invites::use_invite(conn, code)? {
            Ok(_info) => {
                crate::members::register_member(conn, public_key, display_name)?;
                let everyone_id: Option<u64> = conn.query_row(
                    "SELECT id FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).ok();
                if let Some(eid) = everyone_id {
                    crate::members::assign_role(conn, public_key, eid)?;
                }
                return Ok(Ok(false));
            }
            Err(reason) => {
                return Ok(Err(format!("invalid invite: {}", reason)));
            }
        }
    }

    // Auto-claim: if the server has zero members, the first connection becomes owner.
    let member_count: i64 = conn
        .query_row("SELECT count(*) FROM members", [], |row| row.get(0))?;
    if member_count == 0 {
        crate::members::register_member(conn, public_key, display_name)?;
        let everyone_id: Option<u64> = conn.query_row(
            "SELECT id FROM roles WHERE name = '@everyone' AND builtin = 1",
            [],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        ).ok();
        if let Some(eid) = everyone_id {
            crate::members::assign_role(conn, public_key, eid)?;
        }
        return Ok(Ok(true));
    }

    Ok(Err("no invite code or setup token provided".to_string()))
}

// ---------------------------------------------------------------------------
// Existing-member authentication
// ---------------------------------------------------------------------------

/// Verify that an existing member is allowed to connect.
///
/// Returns:
/// - `Ok(Ok(()))` — member exists and is in good standing.
/// - `Ok(Err(reason))` — member is banned, revoked, or not found.
/// - `Err(e)` — unexpected database error.
pub fn authenticate_existing_member(
    conn: &Connection,
    public_key: &PublicKey,
) -> Result<Result<(), String>> {
    match crate::members::get_member(conn, public_key)? {
        None => Ok(Err("member not found".to_string())),
        Some(member) => {
            if member.banned {
                return Ok(Err("member is banned".to_string()));
            }
            if member.revoked {
                return Ok(Err("member is revoked".to_string()));
            }
            Ok(Ok(()))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::members;
    use crate::invites;
    use farder_crypto::identity::Keypair;

    // -----------------------------------------------------------------------
    // Token generation tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_generate_challenge() {
        let c1 = generate_challenge();
        let c2 = generate_challenge();
        assert_eq!(c1.len(), 32);
        assert_ne!(c1, c2, "two challenges should differ");
    }

    #[test]
    fn test_generate_session_token() {
        let t1 = generate_session_token();
        let t2 = generate_session_token();
        assert_eq!(t1.len(), 32);
        assert_ne!(t1, t2, "two session tokens should differ");
    }

    #[test]
    fn test_generate_setup_token() {
        let s1 = generate_setup_token();
        let s2 = generate_setup_token();
        assert_eq!(s1.len(), 32);
        assert_ne!(s1, s2, "two setup tokens should differ");
    }

    // -----------------------------------------------------------------------
    // Challenge-response verification tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_verify_challenge_success() {
        let keypair = Keypair::generate();
        let pk = keypair.public_key();
        let challenge = generate_challenge();
        let sig = keypair.sign(&challenge);
        assert!(
            verify_challenge(&pk, &challenge, &sig).is_ok(),
            "valid signature should verify"
        );
    }

    #[test]
    fn test_verify_challenge_wrong_sig_fails() {
        let keypair = Keypair::generate();
        let pk = keypair.public_key();
        let challenge = generate_challenge();
        // Sign a *different* challenge so the signature is wrong for this one.
        let other_challenge = generate_challenge();
        let sig = keypair.sign(&other_challenge);
        assert!(
            verify_challenge(&pk, &challenge, &sig).is_err(),
            "wrong signature should fail verification"
        );
    }

    // -----------------------------------------------------------------------
    // Setup-token tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_setup_token_claim_owner() {
        let conn = db::open_in_memory().unwrap();
        let keypair = Keypair::generate();
        let pk = keypair.public_key();
        let token = generate_setup_token();
        let token_hex = hex::encode(&token);

        let result = authenticate_new_member(
            &conn,
            &pk,
            "Owner",
            None,
            Some(&token_hex),
            Some(&token),
        )
        .unwrap();

        assert_eq!(result, Ok(false), "setup token claim should succeed with auto_claimed=false");

        // Member should now exist in the database.
        let member = members::get_member(&conn, &pk).unwrap();
        assert!(member.is_some(), "member should be registered after setup token claim");
        assert_eq!(member.unwrap().display_name, "Owner");
    }

    #[test]
    fn test_setup_token_wrong_token_fails() {
        let conn = db::open_in_memory().unwrap();
        let keypair = Keypair::generate();
        let pk = keypair.public_key();
        let token = generate_setup_token();
        // Provide a different hex string.
        let wrong_hex = hex::encode([0u8; 32]);

        let result = authenticate_new_member(
            &conn,
            &pk,
            "Owner",
            None,
            Some(&wrong_hex),
            Some(&token),
        )
        .unwrap();

        assert!(result.is_err(), "wrong setup token should be rejected");
        assert_eq!(result.unwrap_err(), "invalid setup token");
    }

    #[test]
    fn test_setup_token_no_active_token_fails() {
        // active_setup_token = None means server already has an owner.
        let conn = db::open_in_memory().unwrap();
        let keypair = Keypair::generate();
        let pk = keypair.public_key();

        let result = authenticate_new_member(
            &conn,
            &pk,
            "Interloper",
            None,
            Some("deadbeef"),
            None, // no active setup token
        )
        .unwrap();

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "server already has an owner");
    }

    // -----------------------------------------------------------------------
    // Invite-code join test
    // -----------------------------------------------------------------------

    #[test]
    fn test_invite_code_join() {
        let conn = db::open_in_memory().unwrap();

        // Register the owner (needed as invite creator).
        let owner_kp = Keypair::generate();
        let owner_pk = owner_kp.public_key();
        members::register_member(&conn, &owner_pk, "Owner").unwrap();

        // Create an invite.
        let code = invites::create_invite(&conn, &owner_pk, None, None, None).unwrap();

        // New user joins with the invite code.
        let new_kp = Keypair::generate();
        let new_pk = new_kp.public_key();

        let result = authenticate_new_member(
            &conn,
            &new_pk,
            "NewUser",
            Some(&code),
            None,
            None,
        )
        .unwrap();

        assert_eq!(result, Ok(false), "invite join should succeed with auto_claimed=false");

        let member = members::get_member(&conn, &new_pk).unwrap();
        assert!(member.is_some(), "new member should be registered");
        assert_eq!(member.unwrap().display_name, "NewUser");
    }

    // -----------------------------------------------------------------------
    // Existing-member authentication tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_existing_member_auth() {
        let conn = db::open_in_memory().unwrap();
        let keypair = Keypair::generate();
        let pk = keypair.public_key();
        members::register_member(&conn, &pk, "Alice").unwrap();

        let result = authenticate_existing_member(&conn, &pk).unwrap();
        assert!(result.is_ok(), "registered member should authenticate: {:?}", result.err());
    }

    #[test]
    fn test_banned_member_rejected() {
        let conn = db::open_in_memory().unwrap();
        let keypair = Keypair::generate();
        let pk = keypair.public_key();
        members::register_member(&conn, &pk, "BadActor").unwrap();
        members::ban_member(&conn, &pk, None).unwrap();

        let result = authenticate_existing_member(&conn, &pk).unwrap();
        assert!(result.is_err(), "banned member should be rejected");
        assert_eq!(result.unwrap_err(), "member is banned");
    }

    // -----------------------------------------------------------------------
    // Auto-claim owner tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_auto_claim_owner_empty_server() {
        let conn = db::open_in_memory().unwrap();
        let keypair = Keypair::generate();
        let pk = keypair.public_key();

        // No invite, no setup token, but server is empty (0 members)
        let member_count: i64 = conn
            .query_row("SELECT count(*) FROM members", [], |row| row.get(0))
            .unwrap();
        assert_eq!(member_count, 0);

        let result = authenticate_new_member(
            &conn,
            &pk,
            "FirstUser",
            None,  // no invite
            None,  // no setup token hex
            None,  // no active setup token
        )
        .unwrap();

        assert_eq!(result, Ok(true), "first connection to empty server should auto-claim with auto_claimed=true");

        let member = members::get_member(&conn, &pk).unwrap();
        assert!(member.is_some(), "member should be registered");
        assert_eq!(member.unwrap().display_name, "FirstUser");
    }

    #[test]
    fn test_no_auto_claim_when_members_exist() {
        let conn = db::open_in_memory().unwrap();

        // Register an existing member first
        let owner_kp = Keypair::generate();
        let owner_pk = owner_kp.public_key();
        members::register_member(&conn, &owner_pk, "Owner").unwrap();

        // Second user tries to connect with no invite/token
        let new_kp = Keypair::generate();
        let new_pk = new_kp.public_key();

        let result = authenticate_new_member(
            &conn,
            &new_pk,
            "Intruder",
            None,
            None,
            None,
        )
        .unwrap();

        assert!(result.is_err(), "should reject when members exist and no invite provided");
        assert_eq!(result.unwrap_err(), "no invite code or setup token provided");
    }
}
