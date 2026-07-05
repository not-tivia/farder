//! Slash-command CRUD — server-configured `/trigger` commands.
//!
//! Each command has a `kind` of either `"text"` (fixed reply body) or `"api"`
//! (fetches a JSON API and formats the result). The `CommandInfo` returned by
//! `list_infos` deliberately omits `url_template` and `body_text` so API keys
//! are never exposed to members.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use farder_crypto::identity::{Keypair, PublicKey};
use farder_protocol::server::CommandInfo;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct CommandRow {
    pub id: i64,
    pub trigger: String,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub body_text: Option<String>,
    pub url_template: Option<String>,
    pub value_path: Option<String>,
    pub response_template: Option<String>,
    pub unit: Option<String>,
    pub public_key: PublicKey,
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

/// Create a new slash command. Returns the new row id.
pub fn create(
    conn: &Connection,
    name: &str,
    trigger: &str,
    description: &str,
    kind: &str,
    body_text: Option<&str>,
    url_template: Option<&str>,
    value_path: Option<&str>,
    response_template: Option<&str>,
    unit: Option<&str>,
) -> Result<i64> {
    let pk = Keypair::generate().public_key();
    conn.execute(
        "INSERT INTO commands (trigger, name, description, kind, body_text, url_template, \
         value_path, response_template, unit, public_key, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            trigger,
            name,
            description,
            kind,
            body_text,
            url_template,
            value_path,
            response_template,
            unit,
            pk.as_bytes().as_slice(),
            crate::db::now() as i64,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Delete a slash command by id.
pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM commands WHERE id = ?1", params![id])?;
    Ok(())
}

/// List all commands ordered by trigger, returning full rows (includes secrets).
pub fn list_rows(conn: &Connection) -> Result<Vec<CommandRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, trigger, name, description, kind, body_text, url_template, \
         value_path, response_template, unit, public_key \
         FROM commands ORDER BY trigger",
    )?;
    let rows = stmt.query_map([], row_to_command)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// List all commands as `CommandInfo` (safe fields only — no secrets).
/// `takes_arg` is `true` for `"api"` commands (the arg is interpolated into the url).
pub fn list_infos(conn: &Connection) -> Result<Vec<CommandInfo>> {
    Ok(list_rows(conn)?
        .into_iter()
        .map(|r| CommandInfo {
            id: r.id,
            trigger: r.trigger,
            description: r.description,
            takes_arg: r.kind == "api",
        })
        .collect())
}

/// Look up a command by its trigger string. Returns `None` if not found.
pub fn find_by_trigger(conn: &Connection, trigger: &str) -> Result<Option<CommandRow>> {
    conn.query_row(
        "SELECT id, trigger, name, description, kind, body_text, url_template, \
         value_path, response_template, unit, public_key \
         FROM commands WHERE trigger = ?1",
        params![trigger],
        row_to_command,
    )
    .optional()
    .map_err(Into::into)
}

fn row_to_command(r: &rusqlite::Row) -> rusqlite::Result<CommandRow> {
    let pk_b: Vec<u8> = r.get(10)?;
    let arr: [u8; 32] = pk_b
        .try_into()
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(CommandRow {
        id: r.get(0)?,
        trigger: r.get(1)?,
        name: r.get(2)?,
        description: r.get(3)?,
        kind: r.get(4)?,
        body_text: r.get(5)?,
        url_template: r.get(6)?,
        value_path: r.get(7)?,
        response_template: r.get(8)?,
        unit: r.get(9)?,
        public_key: PublicKey::from_bytes(arr),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_list_find_delete_roundtrip() {
        let conn = crate::db::open_in_memory().unwrap();
        let id = create(
            &conn,
            "Rules",
            "rules",
            "server rules",
            "text",
            Some("be nice"),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let infos = list_infos(&conn).unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].trigger, "rules");
        assert_eq!(infos[0].takes_arg, false); // text -> no arg
        let row = find_by_trigger(&conn, "rules").unwrap().unwrap();
        assert_eq!(row.body_text.as_deref(), Some("be nice"));
        // api command -> takes_arg true
        create(
            &conn,
            "Stars",
            "stars",
            "gh stars",
            "api",
            None,
            Some("https://api.github.com/repos/{arg}"),
            Some("stargazers_count"),
            None,
            Some("stars"),
        )
        .unwrap();
        assert!(list_infos(&conn)
            .unwrap()
            .iter()
            .find(|c| c.trigger == "stars")
            .unwrap()
            .takes_arg);
        delete(&conn, id).unwrap();
        assert!(find_by_trigger(&conn, "rules").unwrap().is_none());
    }

    #[test]
    fn list_infos_excludes_secrets() {
        // CommandInfo has no url_template/body_text fields at all — a compile+shape guard.
        let conn = crate::db::open_in_memory().unwrap();
        create(
            &conn,
            "S",
            "s",
            "d",
            "api",
            None,
            Some("https://x/{arg}?key=SECRET"),
            Some("v"),
            None,
            None,
        )
        .unwrap();
        let infos = list_infos(&conn).unwrap();
        assert_eq!(infos[0].trigger, "s"); // only safe fields exposed
    }
}
