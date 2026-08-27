//! Shared test helper: the byte-level "no plaintext anywhere" observer.
//!
//! `assert_no_plaintext_anywhere` was factored out of
//! `crates/farder-server/tests/e2ee_observation.rs` so the Task 8 two-client
//! harness (`tests/e2ee_two_client.rs`) can run the SAME observation against
//! the in-process server's database after driving the shipped E2EE vertical end
//! to end. Integration tests do not share a crate root automatically, so this
//! lives as a `mod` file both test crates include (`mod common;` from the root
//! harness, `#[path]` from the farder-server observation test).
//!
//! The observer enumerates the schema from `sqlite_master` at runtime and scans
//! EVERY value of EVERY row of EVERY table at the byte level. A table added by a
//! future feature is scanned without anyone extending a list, and a needle
//! buried inside a serialized blob (`events.event_body`, a widget's JSON) trips
//! it just as loudly as one in a `content` column.

use rusqlite::Connection;

/// Whether `needle` appears as a contiguous byte slice anywhere in `haystack`.
/// An empty needle never matches (so an empty assertion target cannot
/// vacuously pass).
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && needle.len() <= haystack.len()
        && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Every table in the database, read from the schema rather than a hand-written
/// list, so a table added by a future feature is scanned automatically.
pub fn all_tables(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.map(|r| r.unwrap())
        // FTS5 shadow tables reject a bare `SELECT *`; the FTS content itself is
        // reached through the `messages_fts` virtual table, which IS scanned.
        .filter(|t| {
            !t.ends_with("_data")
                && !t.ends_with("_idx")
                && !t.ends_with("_docsize")
                && !t.ends_with("_config")
                && !t.ends_with("_content")
        })
        .collect()
}

/// **The observation.** Assert `needle` appears nowhere in the database, at the
/// byte level, in any table, column, or serialized blob.
///
/// The self-check that proves this assertion is not vacuously green lives in
/// `crates/farder-server/tests/e2ee_observation.rs`
/// (`the_observer_finds_a_needle_that_is_really_there`): it writes a needle in
/// plaintext and asserts this function DOES panic. Keep that test alive.
pub fn assert_no_plaintext_anywhere(conn: &Connection, needle: &str) {
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
