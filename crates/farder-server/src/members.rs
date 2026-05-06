use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};

use farder_crypto::identity::PublicKey;
use farder_protocol::server::RoleInfo;

use crate::db::now;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

pub struct MemberRecord {
    pub public_key: PublicKey,
    pub display_name: String,
    pub joined_at: u64,
    pub banned: bool,
    pub revoked: bool,
}

// ---------------------------------------------------------------------------
// Member operations
// ---------------------------------------------------------------------------

pub fn register_member(conn: &Connection, pk: &PublicKey, display_name: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO members (public_key, display_name, joined_at) VALUES (?1, ?2, ?3)",
        params![pk.as_bytes().as_slice(), display_name, now() as i64],
    )?;
    Ok(())
}

pub fn get_member(conn: &Connection, pk: &PublicKey) -> Result<Option<MemberRecord>> {
    let row = conn
        .query_row(
            "SELECT public_key, display_name, joined_at, banned, revoked \
             FROM members WHERE public_key = ?1",
            params![pk.as_bytes().as_slice()],
            |row| {
                let key_bytes: Vec<u8> = row.get(0)?;
                let display_name: String = row.get(1)?;
                let joined_at: i64 = row.get(2)?;
                let banned: i64 = row.get(3)?;
                let revoked: i64 = row.get(4)?;
                Ok((key_bytes, display_name, joined_at, banned, revoked))
            },
        )
        .optional()?;

    match row {
        None => Ok(None),
        Some((key_bytes, display_name, joined_at, banned, revoked)) => {
            let arr: [u8; 32] = key_bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("public_key blob wrong length"))?;
            Ok(Some(MemberRecord {
                public_key: PublicKey::from_bytes(arr),
                display_name,
                joined_at: joined_at as u64,
                banned: banned != 0,
                revoked: revoked != 0,
            }))
        }
    }
}

pub fn list_members(conn: &Connection) -> Result<Vec<MemberRecord>> {
    let mut stmt = conn.prepare(
        "SELECT public_key, display_name, joined_at, banned, revoked \
         FROM members WHERE banned = 0 AND revoked = 0",
    )?;

    let rows = stmt.query_map([], |row| {
        let key_bytes: Vec<u8> = row.get(0)?;
        let display_name: String = row.get(1)?;
        let joined_at: i64 = row.get(2)?;
        let banned: i64 = row.get(3)?;
        let revoked: i64 = row.get(4)?;
        Ok((key_bytes, display_name, joined_at, banned, revoked))
    })?;

    let mut members = Vec::new();
    for row in rows {
        let (key_bytes, display_name, joined_at, banned, revoked) = row?;
        let arr: [u8; 32] = key_bytes
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidColumnType(0, "public_key".into(), rusqlite::types::Type::Blob))?;
        members.push(MemberRecord {
            public_key: PublicKey::from_bytes(arr),
            display_name,
            joined_at: joined_at as u64,
            banned: banned != 0,
            revoked: revoked != 0,
        });
    }
    Ok(members)
}

pub fn ban_member(conn: &Connection, pk: &PublicKey, reason: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE members SET banned = 1, ban_reason = ?2 WHERE public_key = ?1",
        rusqlite::params![pk.as_bytes().as_slice(), reason],
    )?;
    Ok(())
}

pub fn unban_member(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute(
        "UPDATE members SET banned = 0, ban_reason = NULL WHERE public_key = ?1",
        rusqlite::params![pk.as_bytes().as_slice()],
    )?;
    Ok(())
}

pub fn set_timeout(conn: &Connection, pk: &PublicKey, until_ms: u64, reason: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE members SET timeout_until = ?1, timeout_reason = ?2 WHERE public_key = ?3",
        rusqlite::params![until_ms as i64, reason, pk.as_bytes().as_slice()],
    )?;
    Ok(())
}

pub fn clear_timeout(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute(
        "UPDATE members SET timeout_until = NULL, timeout_reason = NULL WHERE public_key = ?1",
        rusqlite::params![pk.as_bytes().as_slice()],
    )?;
    Ok(())
}

/// Returns active timeout details if `now_ms < timeout_until`. If the timeout
/// has expired, lazily clears the column and returns None.
pub fn is_timed_out(conn: &Connection, pk: &PublicKey, now_ms: u64) -> Result<Option<(u64, Option<String>)>> {
    let row: Option<(Option<i64>, Option<String>)> = conn
        .query_row(
            "SELECT timeout_until, timeout_reason FROM members WHERE public_key = ?1",
            rusqlite::params![pk.as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    match row {
        Some((Some(until_i64), reason)) => {
            let until_ms_db = until_i64 as u64;
            if now_ms < until_ms_db {
                Ok(Some((until_ms_db, reason)))
            } else {
                clear_timeout(conn, pk)?;
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

pub fn list_banned(conn: &Connection) -> Result<Vec<farder_protocol::server::BannedMember>> {
    let mut stmt = conn.prepare(
        "SELECT public_key, display_name, ban_reason, joined_at \
         FROM members WHERE banned = 1 \
         ORDER BY joined_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        let pk_bytes: Vec<u8> = row.get(0)?;
        let display_name: String = row.get(1)?;
        let ban_reason: Option<String> = row.get(2)?;
        let banned_at: i64 = row.get(3)?;
        let pk_array: [u8; 32] = pk_bytes.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?;
        let pk = farder_crypto::identity::PublicKey::from_bytes(pk_array);
        Ok(farder_protocol::server::BannedMember {
            public_key: pk,
            display_name,
            ban_reason,
            banned_at: banned_at as u64,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}

pub fn revoke_member(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute(
        "UPDATE members SET revoked = 1 WHERE public_key = ?1",
        params![pk.as_bytes().as_slice()],
    )?;
    Ok(())
}

pub fn remove_member(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute(
        "DELETE FROM member_roles WHERE member_key = ?1",
        params![pk.as_bytes().as_slice()],
    )?;
    conn.execute(
        "DELETE FROM members WHERE public_key = ?1",
        params![pk.as_bytes().as_slice()],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Role operations
// ---------------------------------------------------------------------------

pub fn create_role(
    conn: &Connection,
    name: &str,
    permissions: u64,
    color: Option<&str>,
    position: u32,
    builtin: bool,
) -> Result<u64> {
    conn.execute(
        "INSERT INTO roles (name, permissions, color, position, builtin) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            name,
            permissions as i64,
            color,
            position as i64,
            builtin as i64,
        ],
    )?;
    Ok(conn.last_insert_rowid() as u64)
}

pub fn get_role(conn: &Connection, id: u64) -> Result<Option<RoleInfo>> {
    let row = conn
        .query_row(
            "SELECT id, name, permissions, color, position FROM roles WHERE id = ?1",
            params![id as i64],
            |row| {
                let id: i64 = row.get(0)?;
                let name: String = row.get(1)?;
                let permissions: i64 = row.get(2)?;
                let color: Option<String> = row.get(3)?;
                let position: i64 = row.get(4)?;
                Ok(RoleInfo {
                    id: id as u64,
                    name,
                    permissions: permissions as u64,
                    color,
                    position: position as u32,
                })
            },
        )
        .optional()?;
    Ok(row)
}

pub fn list_roles(conn: &Connection) -> Result<Vec<RoleInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, permissions, color, position FROM roles ORDER BY position ASC",
    )?;

    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        let permissions: i64 = row.get(2)?;
        let color: Option<String> = row.get(3)?;
        let position: i64 = row.get(4)?;
        Ok(RoleInfo {
            id: id as u64,
            name,
            permissions: permissions as u64,
            color,
            position: position as u32,
        })
    })?;

    let mut roles = Vec::new();
    for row in rows {
        roles.push(row?);
    }
    Ok(roles)
}

pub fn update_role(
    conn: &Connection,
    id: u64,
    name: Option<&str>,
    permissions: Option<u64>,
    color: Option<Option<&str>>,
    position: Option<u32>,
) -> Result<()> {
    // Build a dynamic UPDATE statement from only the provided fields.
    let mut sets: Vec<String> = Vec::new();

    if name.is_some() {
        sets.push("name = ?2".to_string());
    }
    if permissions.is_some() {
        sets.push(format!("permissions = ?{}", sets.len() + 2));
    }
    if color.is_some() {
        sets.push(format!("color = ?{}", sets.len() + 2));
    }
    if position.is_some() {
        sets.push(format!("position = ?{}", sets.len() + 2));
    }

    if sets.is_empty() {
        return Ok(());
    }

    // We rebuild with correct indices after knowing the final count.
    // Simpler: just use a manual approach with concrete param positions.
    // Recompute correctly:
    let mut sets2: Vec<String> = Vec::new();
    let mut param_idx: usize = 2; // ?1 is the id
    if name.is_some() {
        sets2.push(format!("name = ?{}", param_idx));
        param_idx += 1;
    }
    if permissions.is_some() {
        sets2.push(format!("permissions = ?{}", param_idx));
        param_idx += 1;
    }
    if color.is_some() {
        sets2.push(format!("color = ?{}", param_idx));
        param_idx += 1;
    }
    if position.is_some() {
        sets2.push(format!("position = ?{}", param_idx));
    }

    let sql = format!("UPDATE roles SET {} WHERE id = ?1", sets2.join(", "));

    let mut stmt = conn.prepare(&sql)?;

    // Bind parameters in order: id first (position ?1), then each optional.
    let mut idx: usize = 1;
    stmt.raw_bind_parameter(idx, id as i64)?;
    idx += 1;

    if let Some(n) = name {
        stmt.raw_bind_parameter(idx, n)?;
        idx += 1;
    }
    if let Some(p) = permissions {
        stmt.raw_bind_parameter(idx, p as i64)?;
        idx += 1;
    }
    if let Some(c) = color {
        match c {
            Some(s) => stmt.raw_bind_parameter(idx, s)?,
            None => stmt.raw_bind_parameter(idx, rusqlite::types::Null)?,
        }
        idx += 1;
    }
    if let Some(pos) = position {
        stmt.raw_bind_parameter(idx, pos as i64)?;
    }

    stmt.raw_execute()?;
    Ok(())
}

pub fn delete_role(conn: &Connection, id: u64) -> Result<()> {
    // Check builtin flag.
    let builtin: i64 = conn.query_row(
        "SELECT builtin FROM roles WHERE id = ?1",
        params![id as i64],
        |row| row.get(0),
    )?;

    if builtin != 0 {
        bail!("cannot delete a builtin role");
    }

    conn.execute(
        "DELETE FROM member_roles WHERE role_id = ?1",
        params![id as i64],
    )?;
    conn.execute(
        "DELETE FROM channel_overrides WHERE role_id = ?1",
        params![id as i64],
    )?;
    conn.execute(
        "DELETE FROM category_overrides WHERE role_id = ?1",
        params![id as i64],
    )?;
    conn.execute("DELETE FROM roles WHERE id = ?1", params![id as i64])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Member-Role mapping
// ---------------------------------------------------------------------------

pub fn assign_role(conn: &Connection, pk: &PublicKey, role_id: u64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO member_roles (member_key, role_id) VALUES (?1, ?2)",
        params![pk.as_bytes().as_slice(), role_id as i64],
    )?;
    Ok(())
}

pub fn unassign_role(conn: &Connection, pk: &PublicKey, role_id: u64) -> Result<()> {
    conn.execute(
        "DELETE FROM member_roles WHERE member_key = ?1 AND role_id = ?2",
        params![pk.as_bytes().as_slice(), role_id as i64],
    )?;
    Ok(())
}

pub fn get_member_role_ids(conn: &Connection, pk: &PublicKey) -> Result<Vec<u64>> {
    let mut stmt =
        conn.prepare("SELECT role_id FROM member_roles WHERE member_key = ?1")?;
    let rows = stmt.query_map(params![pk.as_bytes().as_slice()], |row| {
        let id: i64 = row.get(0)?;
        Ok(id as u64)
    })?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(ids)
}

/// Get the highest role position for a member. Returns 0 if no roles assigned.
pub fn get_highest_role_position(conn: &Connection, pk: &PublicKey) -> Result<u32> {
    let pos: Option<i64> = conn.query_row(
        "SELECT MAX(r.position) FROM roles r
         INNER JOIN member_roles mr ON mr.role_id = r.id
         WHERE mr.member_key = ?1",
        params![pk.as_bytes().as_slice()],
        |row| row.get(0),
    ).ok().flatten();
    Ok(pos.unwrap_or(0) as u32)
}

pub fn get_member_role_permissions(conn: &Connection, pk: &PublicKey) -> Result<Vec<u64>> {
    let mut stmt = conn.prepare(
        "SELECT r.permissions FROM member_roles mr \
         JOIN roles r ON mr.role_id = r.id \
         WHERE mr.member_key = ?1",
    )?;
    let rows = stmt.query_map(params![pk.as_bytes().as_slice()], |row| {
        let p: i64 = row.get(0)?;
        Ok(p as u64)
    })?;
    let mut perms = Vec::new();
    for row in rows {
        perms.push(row?);
    }
    Ok(perms)
}

// ---------------------------------------------------------------------------
// Deletion request operations
// ---------------------------------------------------------------------------

pub struct DeletionRequest {
    pub member_key: PublicKey,
    pub requested_at: u64,
    pub expires_at: u64,
}

const DELETION_GRACE_PERIOD_SECS: u64 = 72 * 3600;

pub fn create_deletion_request(conn: &Connection, pk: &PublicKey) -> Result<DeletionRequest> {
    let requested_at = now();
    let expires_at = requested_at + DELETION_GRACE_PERIOD_SECS;
    create_deletion_request_with_expires(conn, pk, requested_at, expires_at)
}

pub fn create_deletion_request_with_expires(
    conn: &Connection,
    pk: &PublicKey,
    requested_at: u64,
    expires_at: u64,
) -> Result<DeletionRequest> {
    conn.execute(
        "INSERT INTO deletion_requests (member_key, requested_at, expires_at) VALUES (?1, ?2, ?3)",
        params![pk.as_bytes().as_slice(), requested_at as i64, expires_at as i64],
    )?;
    Ok(DeletionRequest {
        member_key: pk.clone(),
        requested_at,
        expires_at,
    })
}

pub fn get_deletion_request(conn: &Connection, pk: &PublicKey) -> Result<Option<DeletionRequest>> {
    let row = conn
        .query_row(
            "SELECT member_key, requested_at, expires_at FROM deletion_requests WHERE member_key = ?1",
            params![pk.as_bytes().as_slice()],
            |row| {
                let key_bytes: Vec<u8> = row.get(0)?;
                let requested_at: i64 = row.get(1)?;
                let expires_at: i64 = row.get(2)?;
                Ok((key_bytes, requested_at, expires_at))
            },
        )
        .optional()?;

    match row {
        None => Ok(None),
        Some((key_bytes, requested_at, expires_at)) => {
            let arr: [u8; 32] = key_bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("member_key blob wrong length"))?;
            Ok(Some(DeletionRequest {
                member_key: PublicKey::from_bytes(arr),
                requested_at: requested_at as u64,
                expires_at: expires_at as u64,
            }))
        }
    }
}

pub fn cancel_deletion_request(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute(
        "DELETE FROM deletion_requests WHERE member_key = ?1",
        params![pk.as_bytes().as_slice()],
    )?;
    Ok(())
}

pub fn list_expired_deletion_requests(conn: &Connection) -> Result<Vec<DeletionRequest>> {
    let now_ts = now();
    let mut stmt = conn.prepare(
        "SELECT member_key, requested_at, expires_at FROM deletion_requests WHERE expires_at < ?1",
    )?;

    let rows = stmt.query_map(params![now_ts as i64], |row| {
        let key_bytes: Vec<u8> = row.get(0)?;
        let requested_at: i64 = row.get(1)?;
        let expires_at: i64 = row.get(2)?;
        Ok((key_bytes, requested_at, expires_at))
    })?;

    let mut requests = Vec::new();
    for row in rows {
        let (key_bytes, requested_at, expires_at) = row?;
        let arr: [u8; 32] = key_bytes
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidColumnType(0, "member_key".into(), rusqlite::types::Type::Blob))?;
        requests.push(DeletionRequest {
            member_key: PublicKey::from_bytes(arr),
            requested_at: requested_at as u64,
            expires_at: expires_at as u64,
        });
    }
    Ok(requests)
}

pub fn delete_deletion_request(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute(
        "DELETE FROM deletion_requests WHERE member_key = ?1",
        params![pk.as_bytes().as_slice()],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Block operations
// ---------------------------------------------------------------------------

pub fn block_user(conn: &Connection, blocker: &PublicKey, blocked: &PublicKey) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO blocked_users (blocker_key, blocked_key, blocked_at) VALUES (?1, ?2, ?3)",
        params![blocker.as_bytes().as_slice(), blocked.as_bytes().as_slice(), crate::db::now() as i64],
    )?;
    Ok(())
}

pub fn unblock_user(conn: &Connection, blocker: &PublicKey, blocked: &PublicKey) -> Result<()> {
    conn.execute(
        "DELETE FROM blocked_users WHERE blocker_key = ?1 AND blocked_key = ?2",
        params![blocker.as_bytes().as_slice(), blocked.as_bytes().as_slice()],
    )?;
    Ok(())
}

/// Returns true if either user has blocked the other.
pub fn is_blocked(conn: &Connection, user_a: &PublicKey, user_b: &PublicKey) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM blocked_users \
         WHERE (blocker_key = ?1 AND blocked_key = ?2) \
            OR (blocker_key = ?2 AND blocked_key = ?1)",
        params![user_a.as_bytes().as_slice(), user_b.as_bytes().as_slice()],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use farder_crypto::identity::Keypair;

    fn gen_pk() -> PublicKey {
        Keypair::generate().public_key()
    }

    // -----------------------------------------------------------------------
    // Member tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_register_and_get_member() {
        let conn = db::open_in_memory().unwrap();
        let pk = gen_pk();
        register_member(&conn, &pk, "Alice").unwrap();
        let rec = get_member(&conn, &pk).unwrap().expect("member should exist");
        assert_eq!(rec.display_name, "Alice");
        assert_eq!(rec.public_key, pk);
        assert!(!rec.banned);
        assert!(!rec.revoked);
        assert!(rec.joined_at > 0);
    }

    #[test]
    fn test_get_nonexistent_member_returns_none() {
        let conn = db::open_in_memory().unwrap();
        let pk = gen_pk();
        let result = get_member(&conn, &pk).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_list_members() {
        let conn = db::open_in_memory().unwrap();
        let pk1 = gen_pk();
        let pk2 = gen_pk();
        let pk3 = gen_pk();
        register_member(&conn, &pk1, "Alice").unwrap();
        register_member(&conn, &pk2, "Bob").unwrap();
        register_member(&conn, &pk3, "Carol").unwrap();

        // Ban Carol, revoke Bob — only Alice should appear.
        ban_member(&conn, &pk3, None).unwrap();
        revoke_member(&conn, &pk2).unwrap();

        let members = list_members(&conn).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].display_name, "Alice");
    }

    #[test]
    fn test_ban_member() {
        let conn = db::open_in_memory().unwrap();
        let pk = gen_pk();
        register_member(&conn, &pk, "Dave").unwrap();
        ban_member(&conn, &pk, None).unwrap();
        let rec = get_member(&conn, &pk).unwrap().unwrap();
        assert!(rec.banned);
    }

    #[test]
    fn test_revoke_member() {
        let conn = db::open_in_memory().unwrap();
        let pk = gen_pk();
        register_member(&conn, &pk, "Eve").unwrap();
        revoke_member(&conn, &pk).unwrap();
        let rec = get_member(&conn, &pk).unwrap().unwrap();
        assert!(rec.revoked);
    }

    #[test]
    fn test_remove_member() {
        let conn = db::open_in_memory().unwrap();
        let pk = gen_pk();
        register_member(&conn, &pk, "Frank").unwrap();

        // Assign a role so member_roles row is created too.
        let role_id = create_role(&conn, "Mod", 0xFF, None, 1, false).unwrap();
        assign_role(&conn, &pk, role_id).unwrap();

        remove_member(&conn, &pk).unwrap();

        assert!(get_member(&conn, &pk).unwrap().is_none());
        let role_ids = get_member_role_ids(&conn, &pk).unwrap();
        assert!(role_ids.is_empty());
    }

    // -----------------------------------------------------------------------
    // Role tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_create_and_get_role() {
        let conn = db::open_in_memory().unwrap();
        let id = create_role(&conn, "Admin", 0xFFFF, Some("#FF0000"), 0, false).unwrap();
        let role = get_role(&conn, id).unwrap().expect("role should exist");
        assert_eq!(role.name, "Admin");
        assert_eq!(role.permissions, 0xFFFF);
        assert_eq!(role.color, Some("#FF0000".to_string()));
        assert_eq!(role.position, 0);
    }

    #[test]
    fn test_list_roles() {
        let conn = db::open_in_memory().unwrap();
        create_role(&conn, "C", 0, None, 2, false).unwrap();
        create_role(&conn, "A", 0, None, 0, false).unwrap();
        create_role(&conn, "B", 0, None, 1, false).unwrap();

        let roles = list_roles(&conn).unwrap();
        assert_eq!(roles.len(), 3);
        assert_eq!(roles[0].name, "A");
        assert_eq!(roles[1].name, "B");
        assert_eq!(roles[2].name, "C");
    }

    #[test]
    fn test_update_role() {
        let conn = db::open_in_memory().unwrap();
        let id = create_role(&conn, "Guest", 0x01, Some("#AAAAAA"), 5, false).unwrap();

        // Update name and permissions only.
        update_role(&conn, id, Some("Member"), Some(0x0F), None, None).unwrap();
        let role = get_role(&conn, id).unwrap().unwrap();
        assert_eq!(role.name, "Member");
        assert_eq!(role.permissions, 0x0F);
        // color unchanged
        assert_eq!(role.color, Some("#AAAAAA".to_string()));
        // position unchanged
        assert_eq!(role.position, 5);

        // Update color to None (clear it).
        update_role(&conn, id, None, None, Some(None), None).unwrap();
        let role = get_role(&conn, id).unwrap().unwrap();
        assert!(role.color.is_none());
    }

    #[test]
    fn test_delete_role() {
        let conn = db::open_in_memory().unwrap();
        let id = create_role(&conn, "Temp", 0, None, 0, false).unwrap();
        delete_role(&conn, id).unwrap();
        assert!(get_role(&conn, id).unwrap().is_none());
    }

    #[test]
    fn test_cannot_delete_builtin_role() {
        let conn = db::open_in_memory().unwrap();
        let id = create_role(&conn, "everyone", 0, None, 0, true).unwrap();
        let result = delete_role(&conn, id);
        assert!(result.is_err());
        // Role still exists.
        assert!(get_role(&conn, id).unwrap().is_some());
    }

    // -----------------------------------------------------------------------
    // Member-Role mapping tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_assign_and_get_member_roles() {
        let conn = db::open_in_memory().unwrap();
        let pk = gen_pk();
        register_member(&conn, &pk, "Grace").unwrap();

        let r1 = create_role(&conn, "Mod", 0x10, None, 1, false).unwrap();
        let r2 = create_role(&conn, "VIP", 0x20, None, 2, false).unwrap();

        assign_role(&conn, &pk, r1).unwrap();
        assign_role(&conn, &pk, r2).unwrap();
        // Idempotent second assign.
        assign_role(&conn, &pk, r1).unwrap();

        let mut ids = get_member_role_ids(&conn, &pk).unwrap();
        ids.sort();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&r1));
        assert!(ids.contains(&r2));
    }

    #[test]
    fn test_remove_role_from_member() {
        let conn = db::open_in_memory().unwrap();
        let pk = gen_pk();
        register_member(&conn, &pk, "Heidi").unwrap();
        let r1 = create_role(&conn, "Mod", 0x10, None, 1, false).unwrap();
        assign_role(&conn, &pk, r1).unwrap();

        unassign_role(&conn, &pk, r1).unwrap();
        let ids = get_member_role_ids(&conn, &pk).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn test_get_member_permissions_from_roles() {
        let conn = db::open_in_memory().unwrap();
        let pk = gen_pk();
        register_member(&conn, &pk, "Ivan").unwrap();

        let r1 = create_role(&conn, "ReadOnly", 0x01, None, 1, false).unwrap();
        let r2 = create_role(&conn, "Write", 0x02, None, 2, false).unwrap();

        assign_role(&conn, &pk, r1).unwrap();
        assign_role(&conn, &pk, r2).unwrap();

        let mut perms = get_member_role_permissions(&conn, &pk).unwrap();
        perms.sort();
        assert_eq!(perms, vec![0x01, 0x02]);
    }

    // -----------------------------------------------------------------------
    // Deletion request tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_create_and_get_deletion_request() {
        let conn = db::open_in_memory().unwrap();
        let pk = gen_pk();
        register_member(&conn, &pk, "Alice").unwrap();

        let req = create_deletion_request(&conn, &pk).unwrap();
        assert_eq!(req.member_key, pk);
        assert!(req.requested_at > 0);
        assert_eq!(req.expires_at, req.requested_at + 72 * 3600);

        let fetched = get_deletion_request(&conn, &pk).unwrap().expect("deletion request should exist");
        assert_eq!(fetched.member_key, pk);
        assert_eq!(fetched.requested_at, req.requested_at);
        assert_eq!(fetched.expires_at, req.expires_at);
    }

    #[test]
    fn test_duplicate_deletion_request_fails() {
        let conn = db::open_in_memory().unwrap();
        let pk = gen_pk();
        register_member(&conn, &pk, "Bob").unwrap();

        create_deletion_request(&conn, &pk).unwrap();
        let result = create_deletion_request(&conn, &pk);
        assert!(result.is_err(), "duplicate deletion request should fail (UNIQUE constraint)");
    }

    #[test]
    fn test_cancel_deletion_request() {
        let conn = db::open_in_memory().unwrap();
        let pk = gen_pk();
        register_member(&conn, &pk, "Carol").unwrap();

        create_deletion_request(&conn, &pk).unwrap();
        cancel_deletion_request(&conn, &pk).unwrap();

        let result = get_deletion_request(&conn, &pk).unwrap();
        assert!(result.is_none(), "deletion request should be gone after cancel");
    }

    #[test]
    fn test_list_expired_deletion_requests() {
        let conn = db::open_in_memory().unwrap();
        let pk1 = gen_pk();
        let pk2 = gen_pk();
        let pk3 = gen_pk();
        register_member(&conn, &pk1, "Dave").unwrap();
        register_member(&conn, &pk2, "Eve").unwrap();
        register_member(&conn, &pk3, "Frank").unwrap();

        // pk1 and pk2: already expired (timestamps in the past).
        create_deletion_request_with_expires(&conn, &pk1, 1000, 2000).unwrap();
        create_deletion_request_with_expires(&conn, &pk2, 1000, 3000).unwrap();

        // pk3: far in the future (not expired).
        let future = crate::db::now() + 100_000;
        create_deletion_request_with_expires(&conn, &pk3, crate::db::now(), future).unwrap();

        let expired = list_expired_deletion_requests(&conn).unwrap();
        assert_eq!(expired.len(), 2, "only the two past-expired requests should appear");

        let expired_keys: Vec<PublicKey> = expired.into_iter().map(|r| r.member_key).collect();
        assert!(expired_keys.contains(&pk1));
        assert!(expired_keys.contains(&pk2));
        assert!(!expired_keys.contains(&pk3));
    }

    // -----------------------------------------------------------------------
    // Block/unblock tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_block_and_is_blocked() {
        let conn = db::open_in_memory().unwrap();
        let pk_a = gen_pk();
        let pk_b = gen_pk();

        // Initially not blocked in either direction.
        assert!(!is_blocked(&conn, &pk_a, &pk_b).unwrap());
        assert!(!is_blocked(&conn, &pk_b, &pk_a).unwrap());

        block_user(&conn, &pk_a, &pk_b).unwrap();

        // Now blocked — bidirectional check should return true.
        assert!(is_blocked(&conn, &pk_a, &pk_b).unwrap());
        assert!(is_blocked(&conn, &pk_b, &pk_a).unwrap());
    }

    #[test]
    fn test_unblock_user() {
        let conn = db::open_in_memory().unwrap();
        let pk_a = gen_pk();
        let pk_b = gen_pk();

        block_user(&conn, &pk_a, &pk_b).unwrap();
        assert!(is_blocked(&conn, &pk_a, &pk_b).unwrap());

        unblock_user(&conn, &pk_a, &pk_b).unwrap();
        assert!(!is_blocked(&conn, &pk_a, &pk_b).unwrap());
    }

    #[test]
    fn test_block_is_idempotent() {
        let conn = db::open_in_memory().unwrap();
        let pk_a = gen_pk();
        let pk_b = gen_pk();

        block_user(&conn, &pk_a, &pk_b).unwrap();
        // Second call should not fail (INSERT OR IGNORE).
        block_user(&conn, &pk_a, &pk_b).unwrap();
        assert!(is_blocked(&conn, &pk_a, &pk_b).unwrap());
    }

    #[test]
    fn test_is_blocked_bidirectional() {
        let conn = db::open_in_memory().unwrap();
        let pk_a = gen_pk();
        let pk_b = gen_pk();

        // B blocks A (not A blocks B).
        block_user(&conn, &pk_b, &pk_a).unwrap();

        // Still detected from A's perspective.
        assert!(is_blocked(&conn, &pk_a, &pk_b).unwrap());
        assert!(is_blocked(&conn, &pk_b, &pk_a).unwrap());
    }

    // -----------------------------------------------------------------------
    // Ban / unban / list_banned tests
    // -----------------------------------------------------------------------

    #[test]
    fn ban_with_reason_persists() {
        let conn = db::open_in_memory().unwrap();
        let pk = farder_crypto::identity::PublicKey::from_bytes([1u8; 32]);
        conn.execute(
            "INSERT INTO members (public_key, display_name, joined_at, banned, revoked) VALUES (?1, ?2, ?3, 0, 0)",
            rusqlite::params![pk.as_bytes().as_slice(), "TestUser", 1000i64],
        ).unwrap();

        ban_member(&conn, &pk, Some("spamming")).unwrap();
        let banned = list_banned(&conn).unwrap();
        assert_eq!(banned.len(), 1);
        assert_eq!(banned[0].ban_reason.as_deref(), Some("spamming"));
        assert_eq!(banned[0].display_name, "TestUser");
    }

    #[test]
    fn unban_clears_flag_and_reason() {
        let conn = db::open_in_memory().unwrap();
        let pk = farder_crypto::identity::PublicKey::from_bytes([2u8; 32]);
        conn.execute(
            "INSERT INTO members (public_key, display_name, joined_at, banned, revoked) VALUES (?1, ?2, ?3, 0, 0)",
            rusqlite::params![pk.as_bytes().as_slice(), "Other", 1000i64],
        ).unwrap();

        ban_member(&conn, &pk, Some("test")).unwrap();
        assert_eq!(list_banned(&conn).unwrap().len(), 1);
        unban_member(&conn, &pk).unwrap();
        assert_eq!(list_banned(&conn).unwrap().len(), 0);

        let m = get_member(&conn, &pk).unwrap().unwrap();
        assert!(!m.banned);
    }

    // -----------------------------------------------------------------------
    // Timeout tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_set_and_get_timeout() {
        let conn = db::open_in_memory().unwrap();
        let kp = farder_crypto::identity::Keypair::generate();
        let pk = kp.public_key();
        register_member(&conn, &pk, "alice").unwrap();

        // No timeout initially.
        assert_eq!(is_timed_out(&conn, &pk, 1000).unwrap(), None);

        // Set a timeout that hasn't expired.
        set_timeout(&conn, &pk, 5000, Some("warning")).unwrap();
        let active = is_timed_out(&conn, &pk, 1000).unwrap();
        assert_eq!(active, Some((5000, Some("warning".to_string()))));

        // Past `until_ms` -> returns None and lazily clears the column.
        let active = is_timed_out(&conn, &pk, 6000).unwrap();
        assert_eq!(active, None);
        // Re-checking confirms the column is cleared.
        let active = is_timed_out(&conn, &pk, 1000).unwrap();
        assert_eq!(active, None);
    }

    #[test]
    fn test_clear_timeout() {
        let conn = db::open_in_memory().unwrap();
        let kp = farder_crypto::identity::Keypair::generate();
        let pk = kp.public_key();
        register_member(&conn, &pk, "bob").unwrap();
        set_timeout(&conn, &pk, 9999, None).unwrap();

        clear_timeout(&conn, &pk).unwrap();
        assert_eq!(is_timed_out(&conn, &pk, 1000).unwrap(), None);
    }
}
