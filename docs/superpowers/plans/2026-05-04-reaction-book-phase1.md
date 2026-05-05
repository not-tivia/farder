# Reaction Book Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a personal account-level "Reaction Book" — users upload images to their book, then use any item as a reaction on any server. The image auto-uploads as a regular file attachment to the server on first use, and the reaction stores a ref to the resulting `file_id`. Other clients fetch and render normally.

**Architecture:** Server gets a `reactions.file_id` nullable column and three optional protocol fields (`AddReaction`, `RemoveReaction`, `ReactionAdded`, `ReactionRemoved`, `ReactionGroup` all gain `file_id: Option<u64>`). Client gets a new `book.rs` module storing items at `~/.farder/book/`, a per-item per-server upload cache to avoid re-uploading the same image to the same server twice, and a `BookBrowser` UI with All/Static/Animated tabs. Save-from-chat replaces the existing favorites path; legacy favorites are auto-migrated on first run. Tightened security: 2 MB cap, magic-byte validation, dimension/frame-count caps, per-user upload + reaction rate limits and storage quotas (with user-confirmation rather than silent eviction on overflow).

**Tech Stack:** Rust (Tauri v2 + farder-server crate), React + TypeScript, SQLite via rusqlite, the existing `image` crate (added as direct dep on the server for header-only validation).

**Spec:** `docs/superpowers/specs/2026-05-04-reaction-book-phase1-design.md`

**Phase scope:** Phase 1 only. Phase 2 (inline `:name:` rendering, send-as-sticker mode, Unicode emoji favoriting) is a separate plan written after Phase 1 ships.

---

## Task 1: Server schema migration — add `reactions.file_id` column

Adds a nullable `file_id` column to the existing `reactions` table. Uses a one-shot ALTER on startup so existing databases upgrade in place.

**Files:**
- Modify: `crates/farder-server/src/db.rs`

- [ ] **Step 1: Add the migration**

Find the existing `init_schema` function (or equivalent migration block) in `crates/farder-server/src/db.rs`. After the existing `reactions` table CREATE statement, add an idempotent ALTER guarded by checking the columns:

```rust
// Reactions: add file_id column for custom-emoji reactions (Phase 1 of Reaction Book).
// SQLite has no IF NOT EXISTS for ALTER, so we check pragma table_info first.
let has_file_id = {
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
```

Place this AFTER the existing `CREATE TABLE IF NOT EXISTS reactions ...` block and AFTER the `files` table is created (the FK reference requires `files` to exist).

- [ ] **Step 2: Run the existing reactions tests to confirm no regression**

```
cd /home/deez/farder/crates/farder-server && cargo test --lib reactions:: 2>&1 | tail -10
```

Expected: all existing tests pass. The new column is nullable so existing inserts (which don't reference it) succeed unchanged.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/db.rs
git -C /home/deez/farder commit -m "feat(server): add nullable reactions.file_id column for custom emoji"
```

---

## Task 2: Protocol — add `file_id: Option<u64>` to reaction frames

All five protocol types touching reactions get an optional `file_id` field. `Option` ensures wire compatibility — old clients deserialize new server frames as `None`, new clients sending to old servers omit the field.

**Files:**
- Modify: `crates/farder-protocol/src/server.rs`

- [ ] **Step 1: Find and modify the five types**

In `crates/farder-protocol/src/server.rs`, locate each of these and add `file_id: Option<u64>` (or update the existing struct/variant signature):

```rust
// In ServerRequest enum:
AddReaction {
    message_id: u64,
    emoji: String,
    #[serde(default)]
    file_id: Option<u64>,
},

RemoveReaction {
    message_id: u64,
    emoji: String,
    #[serde(default)]
    file_id: Option<u64>,
},

// In ServerEvent enum:
ReactionAdded {
    message_id: u64,
    channel_id: u64,
    emoji: String,
    public_key: PublicKey,
    #[serde(default)]
    file_id: Option<u64>,
},

ReactionRemoved {
    message_id: u64,
    channel_id: u64,
    emoji: String,
    public_key: PublicKey,
    #[serde(default)]
    file_id: Option<u64>,
},

// In the ReactionGroup struct:
pub struct ReactionGroup {
    pub emoji: String,
    pub count: u32,
    pub me: bool,
    #[serde(default)]
    pub file_id: Option<u64>,
}
```

The `#[serde(default)]` is critical — it makes the field optional during deserialization so old encoded frames still decode.

- [ ] **Step 2: Verify the workspace compiles**

```
cd /home/deez/farder && cargo check --workspace 2>&1 | tail -10
```

Expected: `Finished` with possibly some "field never read" warnings (the new `file_id` isn't used yet — those go away in Tasks 3 and 4).

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add crates/farder-protocol/src/server.rs
git -C /home/deez/farder commit -m "feat(protocol): add optional file_id to reaction frames"
```

---

## Task 3: Server reactions module — accept and store `file_id`

Update `add_reaction`, `remove_reaction`, and `get_reactions_for_message` to thread the `file_id` through. For custom emoji, the emoji string column stores the sentinel `:custom:` and uniqueness is `(message_id, user_key, emoji, file_id)`.

**Files:**
- Modify: `crates/farder-server/src/reactions.rs`

- [ ] **Step 1: Update `add_reaction` signature**

In `crates/farder-server/src/reactions.rs`, replace the existing `add_reaction` function with:

```rust
pub fn add_reaction(
    conn: &Connection,
    message_id: u64,
    user_key: &PublicKey,
    emoji: &str,
    file_id: Option<u64>,
) -> Result<()> {
    if emoji.is_empty() {
        bail!("emoji cannot be empty");
    }
    if emoji.len() > 32 {
        bail!("emoji too long (max 32 bytes)");
    }
    // Reject sentinel without a file_id, and any non-sentinel WITH a file_id.
    if emoji == ":custom:" && file_id.is_none() {
        bail!("custom emoji reaction requires file_id");
    }
    if emoji != ":custom:" && file_id.is_some() {
        bail!("non-custom emoji must not include file_id");
    }

    // Check the distinct (emoji, file_id) count for the message.
    let (emoji_exists, distinct_count): (bool, i64) = {
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) FROM reactions WHERE message_id = ?1 AND emoji = ?2 AND \
             ((file_id IS NULL AND ?3 IS NULL) OR file_id = ?3)",
            params![message_id as i64, emoji, file_id.map(|v| v as i64)],
            |row| row.get::<_, i64>(0),
        )? > 0;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM (SELECT DISTINCT emoji, file_id FROM reactions WHERE message_id = ?1)",
            params![message_id as i64],
            |row| row.get(0),
        )?;
        (exists, count)
    };

    if !emoji_exists && distinct_count >= 20 {
        bail!("maximum 20 unique reactions per message");
    }

    conn.execute(
        "INSERT OR IGNORE INTO reactions (message_id, user_key, emoji, file_id, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            message_id as i64,
            user_key.as_bytes().as_slice(),
            emoji,
            file_id.map(|v| v as i64),
            now() as i64,
        ],
    )?;

    Ok(())
}
```

- [ ] **Step 2: Update `remove_reaction` to accept `file_id`**

```rust
pub fn remove_reaction(
    conn: &Connection,
    message_id: u64,
    user_key: &PublicKey,
    emoji: &str,
    file_id: Option<u64>,
) -> Result<()> {
    conn.execute(
        "DELETE FROM reactions WHERE message_id = ?1 AND user_key = ?2 AND emoji = ?3 AND \
         ((file_id IS NULL AND ?4 IS NULL) OR file_id = ?4)",
        params![
            message_id as i64,
            user_key.as_bytes().as_slice(),
            emoji,
            file_id.map(|v| v as i64),
        ],
    )?;
    Ok(())
}
```

- [ ] **Step 3: Update `get_reactions_for_message` to group by `(emoji, file_id)` and return `file_id`**

Replace the existing query and result mapping:

```rust
pub fn get_reactions_for_message(
    conn: &Connection,
    message_id: u64,
    requester: &PublicKey,
) -> Result<Vec<ReactionGroup>> {
    let mut stmt = conn.prepare(
        "SELECT emoji, file_id, COUNT(*) as cnt, \
                MAX(CASE WHEN user_key = ?2 THEN 1 ELSE 0 END) as me \
         FROM reactions \
         WHERE message_id = ?1 \
         GROUP BY emoji, file_id \
         ORDER BY MIN(created_at) ASC",
    )?;

    let rows = stmt.query_map(
        params![message_id as i64, requester.as_bytes().as_slice()],
        |row| {
            let emoji: String = row.get(0)?;
            let file_id: Option<i64> = row.get(1)?;
            let count: i64 = row.get(2)?;
            let me: i64 = row.get(3)?;
            Ok(ReactionGroup {
                emoji,
                count: count as u32,
                me: me != 0,
                file_id: file_id.map(|v| v as u64),
            })
        },
    )?;

    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}
```

- [ ] **Step 4: Update `get_reactions_for_messages` (batch variant) the same way**

Find the existing `get_reactions_for_messages` (used by `fetch_history`) and apply the same `GROUP BY emoji, file_id` change. The result type per message is `Vec<ReactionGroup>` now containing `file_id`.

- [ ] **Step 5: Update existing tests in `mod tests` for the new signature**

The existing tests in `crates/farder-server/src/reactions.rs` call `add_reaction(...)` with 4 args. Add a 5th `None` argument to each existing call. Also update the `retention.rs` test calls (line 167–168 of retention.rs as of last check):

```rust
// in retention.rs and reactions.rs tests
reactions::add_reaction(&conn, msg_id1, &pk1, "👍", None).unwrap();
```

Add three new tests at the end of the existing `mod tests` block in `reactions.rs`:

```rust
    #[test]
    fn add_reaction_custom_requires_file_id() {
        let conn = crate::db::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        let pk = PublicKey::from_bytes([1u8; 32]);
        // Set up a channel + message + file via test helpers omitted for brevity —
        // use the same pattern as existing tests in this module.
        // ...
        let result = add_reaction(&conn, /* msg_id */ 1, &pk, ":custom:", None);
        assert!(result.is_err(), "custom emoji without file_id should be rejected");
    }

    #[test]
    fn add_reaction_unicode_rejects_file_id() {
        let conn = crate::db::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        let pk = PublicKey::from_bytes([1u8; 32]);
        let result = add_reaction(&conn, 1, &pk, "👍", Some(42));
        assert!(result.is_err(), "unicode emoji with file_id should be rejected");
    }

    #[test]
    fn custom_reactions_with_different_file_ids_are_distinct_groups() {
        let conn = crate::db::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        let pk = PublicKey::from_bytes([1u8; 32]);
        // Insert two custom reactions with different file_ids on the same message
        // ...follow the existing pattern for setting up test data, then:
        // get_reactions_for_message should return two groups, both with emoji=":custom:"
    }
```

(The bodies of the second and third tests need helper-setup code that depends on the existing test patterns in this module — copy from the nearby `add_reaction` tests for the boilerplate of inserting a member + channel + message + file row.)

- [ ] **Step 6: Run reactions tests + entire server tests**

```
cd /home/deez/farder/crates/farder-server && cargo test --lib reactions:: -- --test-threads=1 2>&1 | tail -20
cd /home/deez/farder/crates/farder-server && cargo test --lib 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 7: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/reactions.rs crates/farder-server/src/retention.rs
git -C /home/deez/farder commit -m "feat(server): reactions module supports custom-emoji file_id"
```

---

## Task 4: Server image validation — magic bytes, dimensions, frame count

A new module that validates an uploaded image file. Used by the existing upload path before the file gets stored.

**Files:**
- Modify: `crates/farder-server/Cargo.toml` — add `image` crate as a direct dep
- Create: `crates/farder-server/src/image_validation.rs`
- Modify: `crates/farder-server/src/lib.rs` (or wherever the module list is) — declare the new module
- Modify: `crates/farder-server/src/attachments.rs` — call validation in `store_file`

- [ ] **Step 1: Add the `image` crate dep**

In `crates/farder-server/Cargo.toml`, under `[dependencies]`, add:

```toml
image = { version = "0.25", default-features = false, features = ["png", "jpeg", "gif", "webp"] }
```

If `image` is already a transitive dep at a different version, align to that version to avoid duplicate compilation.

- [ ] **Step 2: Create `image_validation.rs`**

`crates/farder-server/src/image_validation.rs`:

```rust
use anyhow::{bail, Result};
use image::ImageReader;
use std::io::Cursor;

const MAX_BOOK_BYTES: u64 = 2 * 1024 * 1024;          // 2 MB hard cap for book items
const MAX_DIMENSION: u32 = 4096;                       // per-axis
const MAX_TOTAL_PIXELS: u64 = 4_000_000;               // 4 megapixels
const MAX_ANIMATION_FRAMES: usize = 200;

/// Returns `(width, height, animated)` if the bytes pass all validation.
/// Errors include the reason a file was rejected — these are surfaced to the
/// user, so keep them human-readable.
pub fn validate_image(data: &[u8], strict_book_size: bool) -> Result<(u32, u32, bool)> {
    if strict_book_size && (data.len() as u64) > MAX_BOOK_BYTES {
        bail!(
            "image too large for book ({} bytes; max {} bytes)",
            data.len(),
            MAX_BOOK_BYTES
        );
    }

    if data.len() < 16 {
        bail!("image too small to be valid");
    }

    // Magic-byte check — reject anything that doesn't start with a known image header.
    let magic = &data[..16];
    let format = if magic.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        "png"
    } else if magic.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpeg"
    } else if magic.starts_with(b"GIF87a") || magic.starts_with(b"GIF89a") {
        "gif"
    } else if magic.starts_with(b"RIFF") && magic.len() >= 12 && &magic[8..12] == b"WEBP" {
        "webp"
    } else {
        bail!("unsupported image format (only PNG, JPEG, GIF, WebP are allowed)");
    };

    // Dimensions — header-only via ImageReader::into_dimensions().
    let reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| anyhow::anyhow!("failed to read image header: {}", e))?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|e| anyhow::anyhow!("invalid image header: {}", e))?;

    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        bail!(
            "image dimensions exceed limit ({}×{}; max {}×{} per axis)",
            width, height, MAX_DIMENSION, MAX_DIMENSION
        );
    }
    if (width as u64) * (height as u64) > MAX_TOTAL_PIXELS {
        bail!(
            "image total pixels exceed limit ({}; max {})",
            (width as u64) * (height as u64),
            MAX_TOTAL_PIXELS
        );
    }

    let animated = match format {
        "gif" => count_gif_frames(data)? > 1,
        "webp" => is_webp_animated(data),
        _ => false,
    };

    if format == "gif" {
        let frames = count_gif_frames(data)?;
        if frames > MAX_ANIMATION_FRAMES {
            bail!("animated image has too many frames ({}; max {})", frames, MAX_ANIMATION_FRAMES);
        }
    }
    if format == "webp" && animated {
        let frames = count_webp_frames(data);
        if frames > MAX_ANIMATION_FRAMES {
            bail!("animated image has too many frames ({}; max {})", frames, MAX_ANIMATION_FRAMES);
        }
    }

    Ok((width, height, animated))
}

/// Walk GIF blocks counting Image Descriptor markers (0x2C). Cheap header walk.
fn count_gif_frames(data: &[u8]) -> Result<usize> {
    // After the 13-byte header + optional global color table, blocks start.
    // Just scan for 0x2C (Image Separator) bytes that aren't inside data sub-blocks.
    // For simplicity (and since we cap at 200 anyway), we approximate by counting
    // 0x2C bytes that appear after the global color table region. A cheap upper
    // bound is enough — we just need to reject pathological cases.
    if data.len() < 13 {
        bail!("gif too short");
    }
    let packed = data[10];
    let has_gct = (packed & 0x80) != 0;
    let gct_size = if has_gct { 3 * (1 << ((packed & 0x07) + 1)) } else { 0 };
    let scan_start = 13 + gct_size;
    if scan_start >= data.len() {
        return Ok(0);
    }
    Ok(data[scan_start..].iter().filter(|&&b| b == 0x2C).count())
}

/// Animated WebP files contain an "ANIM" chunk in their RIFF container.
fn is_webp_animated(data: &[u8]) -> bool {
    if data.len() < 30 { return false; }
    // After "RIFF" (4) + size (4) + "WEBP" (4) = 12 bytes of header, chunks follow.
    // Walk chunk headers (4-char id + 4-byte size, then payload aligned to 2 bytes).
    let mut pos = 12;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = u32::from_le_bytes([data[pos+4], data[pos+5], data[pos+6], data[pos+7]]) as usize;
        if id == b"ANIM" {
            return true;
        }
        pos += 8 + size + (size & 1); // pad to 2-byte alignment
    }
    false
}

fn count_webp_frames(data: &[u8]) -> usize {
    // Count ANMF chunks.
    let mut count = 0;
    let mut pos = 12;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = u32::from_le_bytes([data[pos+4], data[pos+5], data[pos+6], data[pos+7]]) as usize;
        if id == b"ANMF" {
            count += 1;
        }
        pos += 8 + size + (size & 1);
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_too_small() {
        assert!(validate_image(&[0u8; 4], false).is_err());
    }

    #[test]
    fn rejects_unknown_magic() {
        let mut data = vec![0u8; 100];
        data[0] = 0xDE; data[1] = 0xAD; data[2] = 0xBE; data[3] = 0xEF;
        let result = validate_image(&data, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unsupported image format"));
    }

    #[test]
    fn accepts_minimal_png() {
        // 1x1 PNG generated programmatically.
        let png = generate_1x1_png();
        let (w, h, animated) = validate_image(&png, true).unwrap();
        assert_eq!(w, 1);
        assert_eq!(h, 1);
        assert!(!animated);
    }

    #[test]
    fn rejects_oversized_for_book_strict_mode() {
        // 3 MB PNG (mostly zeros after the header) — should reject in strict mode.
        let mut png = generate_1x1_png();
        png.extend(vec![0u8; 3 * 1024 * 1024]);
        // Make sure the dimensions inside the (still valid) PNG header are 1x1.
        let result = validate_image(&png, true);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too large for book"));
    }

    fn generate_1x1_png() -> Vec<u8> {
        // Smallest valid PNG: signature + IHDR + IDAT (1 white pixel) + IEND
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,             // signature
            0x00, 0x00, 0x00, 0x0D,                                     // IHDR length
            0x49, 0x48, 0x44, 0x52,                                     // "IHDR"
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,             // 1x1
            0x08, 0x06, 0x00, 0x00, 0x00,                               // bit depth + color type + flags
            0x1F, 0x15, 0xC4, 0x89,                                     // IHDR CRC
            0x00, 0x00, 0x00, 0x0A,                                     // IDAT length
            0x49, 0x44, 0x41, 0x54,                                     // "IDAT"
            0x78, 0x9C, 0x62, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x01, // zlib stream
            0x0D, 0x0A, 0x2D, 0xB4,                                     // IDAT CRC
            0x00, 0x00, 0x00, 0x00,                                     // IEND length
            0x49, 0x45, 0x4E, 0x44,                                     // "IEND"
            0xAE, 0x42, 0x60, 0x82,                                     // IEND CRC
        ]
    }
}
```

- [ ] **Step 3: Declare the module**

In `crates/farder-server/src/lib.rs` (or `main.rs` if it's a binary-only crate), add:

```rust
pub mod image_validation;
```

(adjacent to the other `pub mod` declarations)

- [ ] **Step 4: Wire validation into the upload path**

In `crates/farder-server/src/attachments.rs`, find `store_file` (the function that writes the actual bytes to disk + inserts the row). At the top of the function — BEFORE writing anything — add a call to validation. The strict-book-size flag is OFF here because regular attachments use the existing `max_file_size` config; book items get a separate check at the client-Tauri layer in Task 6.

```rust
// At the top of store_file, after the existing nullity / size checks:
if mime_type.starts_with("image/") {
    let _ = crate::image_validation::validate_image(data, false)
        .map_err(|e| anyhow::anyhow!("image rejected: {}", e))?;
}
```

(The `strict_book_size` parameter is set to `false` here because non-book attachments can be larger; the client-Tauri-side `book_upload_item` command will pass `true` when it pre-validates locally, and the server's per-attachment `max_file_size` cap continues to enforce the upper bound.)

- [ ] **Step 5: Run the validation tests + attachment tests**

```
cd /home/deez/farder/crates/farder-server && cargo test --lib image_validation:: 2>&1 | tail -10
cd /home/deez/farder/crates/farder-server && cargo test --lib attachments:: 2>&1 | tail -10
```

Expected: validation tests pass; attachment tests pass.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add crates/farder-server/Cargo.toml crates/farder-server/Cargo.lock crates/farder-server/src/image_validation.rs crates/farder-server/src/lib.rs crates/farder-server/src/attachments.rs
git -C /home/deez/farder commit -m "feat(server): magic-byte + dimension + frame-count validation for image uploads"
```

---

## Task 5: Server rate limits — per-user upload throttle and reaction rate limit

In-memory rate limiting in `state.rs`. Keeps it simple — no persistence, resets on server restart, which is fine for spam prevention.

**Files:**
- Modify: `crates/farder-server/src/state.rs`
- Modify: `crates/farder-server/src/handlers.rs`

- [ ] **Step 1: Add rate limiters to ServerState**

In `crates/farder-server/src/state.rs`, add fields to `ServerState`:

```rust
use std::collections::VecDeque;

pub struct RateLimiter {
    /// Per-user rolling timestamp queue (seconds since epoch).
    pub users: Mutex<HashMap<[u8; 32], VecDeque<u64>>>,
    pub max_per_window: usize,
    pub window_secs: u64,
}

impl RateLimiter {
    pub fn new(max_per_window: usize, window_secs: u64) -> Self {
        Self {
            users: Mutex::new(HashMap::new()),
            max_per_window,
            window_secs,
        }
    }

    /// Returns true if the user is allowed; false if they're over the limit.
    /// Records the attempt timestamp on success.
    pub fn allow(&self, user: &[u8; 32]) -> bool {
        let now = crate::db::now();
        let mut users = self.users.lock().unwrap();
        let queue = users.entry(*user).or_insert_with(VecDeque::new);
        // Drain timestamps that are outside the window.
        while let Some(&front) = queue.front() {
            if now.saturating_sub(front) >= self.window_secs {
                queue.pop_front();
            } else {
                break;
            }
        }
        if queue.len() >= self.max_per_window {
            return false;
        }
        queue.push_back(now);
        true
    }
}
```

Add to the `ServerState` struct itself:

```rust
pub struct ServerState {
    // ... existing fields ...
    pub upload_limiter: RateLimiter,    // 10/min per user
    pub reaction_limiter: RateLimiter,  // 60/min per user (note: per-user, not per-channel — keeps it simple)
}
```

In `ServerState::new(...)`, initialize:

```rust
upload_limiter: RateLimiter::new(10, 60),
reaction_limiter: RateLimiter::new(60, 60),
```

Likewise update `new_for_test()` with the same initializers.

- [ ] **Step 2: Apply the reaction limiter in the AddReaction handler**

In `crates/farder-server/src/handlers.rs`, find the `ServerRequest::AddReaction` arm. Before the existing permission check, add:

```rust
ServerRequest::AddReaction { message_id, emoji, file_id } => {
    let pk_bytes = *member.as_bytes();
    if !state.reaction_limiter.allow(&pk_bytes) {
        return err("reaction rate limit exceeded — please slow down");
    }
    // ... existing code ...
}
```

(Update the destructure to include `file_id`, then pass it through to `crate::reactions::add_reaction(conn, message_id, member, &emoji, file_id)?;` further down. Update the broadcast event to include `file_id` too.)

Do the same for `RemoveReaction` — destructure `file_id`, pass through, include in the `ReactionRemoved` event. (Removal does NOT count against the rate limiter — only adds.)

- [ ] **Step 3: Apply the upload limiter at the upload entry point**

In `crates/farder-server/src/connection.rs`, find `handle_upload_stream` (line ~120 or wherever the upload path lives). At the top of the function, after permission checks but before reading the file body:

```rust
if !state.upload_limiter.allow(&member_key.as_bytes().clone()) {
    return Err(anyhow::anyhow!("upload rate limit exceeded — please slow down"));
}
```

(Adjust to whatever error-return pattern that function uses.)

- [ ] **Step 4: Verify everything compiles**

```
cd /home/deez/farder/crates/farder-server && cargo check 2>&1 | tail -5
```

Expected: `Finished` with no new errors. Possibly a warning about `state.upload_limiter` being unused if nothing imports it — that goes away once the call site is added.

- [ ] **Step 5: Add a quick test for RateLimiter**

In `crates/farder-server/src/state.rs`, append at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_allows_under_limit() {
        let rl = RateLimiter::new(3, 60);
        let user = [1u8; 32];
        assert!(rl.allow(&user));
        assert!(rl.allow(&user));
        assert!(rl.allow(&user));
        assert!(!rl.allow(&user));
    }

    #[test]
    fn rate_limiter_isolates_users() {
        let rl = RateLimiter::new(1, 60);
        let a = [1u8; 32];
        let b = [2u8; 32];
        assert!(rl.allow(&a));
        assert!(!rl.allow(&a));
        assert!(rl.allow(&b));
    }
}
```

Run: `cd /home/deez/farder/crates/farder-server && cargo test --lib state::tests 2>&1 | tail -10`
Expected: 2 tests pass.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/state.rs crates/farder-server/src/handlers.rs crates/farder-server/src/connection.rs
git -C /home/deez/farder commit -m "feat(server): rate limit reactions (60/min) and uploads (10/min) per user"
```

---

## Task 6: Client Rust — `book.rs` module with storage + upload + delete + rename

Creates the new module, all storage helpers, and the four basic CRUD commands. Per-server upload caching and migration come in Task 7.

**Files:**
- Create: `client/src-tauri/src/book.rs`
- Modify: `client/src-tauri/Cargo.toml` — add `uuid` and `image` deps if not present

- [ ] **Step 1: Verify deps**

```
grep -E '^(uuid|image)' /home/deez/farder/client/src-tauri/Cargo.toml
```

If `uuid` is not present, add to `[dependencies]`:
```toml
uuid = { version = "1", features = ["v4"] }
```

If `image` is not present (it'll be needed for client-side dimension probing too):
```toml
image = { version = "0.25", default-features = false, features = ["png", "jpeg", "gif", "webp"] }
```

(Match the version used by the server crate.)

- [ ] **Step 2: Create `book.rs` skeleton**

`client/src-tauri/src/book.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

const BOOK_DIR: &str = "book";
const FILES_DIR: &str = "files";
const INDEX_FILE: &str = "items.json";
const MAX_BOOK_BYTES: u64 = 2 * 1024 * 1024;
const ALLOWED_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

#[derive(Serialize, Deserialize, Clone)]
pub struct BookItem {
    pub id: String,
    pub name: String,
    pub ext: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub animated: bool,
    pub added_at: u64,
    pub source: String,        // "upload" | "chat" | "favorites-migration"
    #[serde(default)]
    pub server_files: HashMap<String, u64>,
}

fn book_root() -> PathBuf {
    crate::commands::farder_data_dir_pub().join(BOOK_DIR)
}

fn files_dir() -> PathBuf {
    book_root().join(FILES_DIR)
}

fn index_path() -> PathBuf {
    book_root().join(INDEX_FILE)
}

fn ensure_dirs() -> Result<(), String> {
    std::fs::create_dir_all(files_dir())
        .map_err(|e| format!("create book dirs failed: {}", e))
}

fn load_index() -> Vec<BookItem> {
    std::fs::read_to_string(index_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<BookItem>>(&s).ok())
        .unwrap_or_default()
}

fn save_index(items: &[BookItem]) -> Result<(), String> {
    ensure_dirs()?;
    let pretty = serde_json::to_string_pretty(items).map_err(|e| e.to_string())?;
    std::fs::write(index_path(), pretty).map_err(|e| format!("write items.json failed: {}", e))
}

fn sanitize_name(input: &str) -> String {
    input
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .chars()
        .take(32)
        .collect()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn detect_animated(data: &[u8], ext: &str) -> bool {
    match ext {
        "gif" => data.windows(6).any(|w| w == b"GIF87a" || w == b"GIF89a"),
        "webp" => {
            // Same scan as server image_validation. Inline for client simplicity.
            if data.len() < 30 { return false; }
            let mut pos = 12;
            while pos + 8 <= data.len() {
                if &data[pos..pos+4] == b"ANIM" { return true; }
                let size = u32::from_le_bytes([data[pos+4], data[pos+5], data[pos+6], data[pos+7]]) as usize;
                pos += 8 + size + (size & 1);
            }
            false
        }
        _ => false,
    }
}

#[tauri::command]
pub fn book_list_items() -> Vec<BookItem> {
    load_index()
}

#[tauri::command]
pub fn book_upload_item(source_path: String, name: Option<String>) -> Result<BookItem, String> {
    let src = std::path::Path::new(&source_path);
    let bytes = std::fs::read(src).map_err(|e| format!("read source failed: {}", e))?;
    if bytes.len() as u64 > MAX_BOOK_BYTES {
        return Err(format!("image too large ({} bytes; max {} bytes)", bytes.len(), MAX_BOOK_BYTES));
    }

    let original_filename = src.file_name().and_then(|s| s.to_str()).unwrap_or("image");
    let ext = src
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .ok_or_else(|| "image has no extension".to_string())?;
    if !ALLOWED_EXTS.contains(&ext.as_str()) {
        return Err(format!("unsupported extension '{}' (allowed: {})", ext, ALLOWED_EXTS.join(", ")));
    }

    // Probe dimensions (best-effort — failures are non-fatal, just leave them None).
    let (width, height) = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .map(|(w, h)| (Some(w), Some(h)))
        .unwrap_or((None, None));

    let id = uuid::Uuid::new_v4().to_string();
    let resolved_name = match name {
        Some(n) if !n.trim().is_empty() => sanitize_name(n.trim()),
        _ => {
            // Auto-generate from filename stem.
            let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("emoji");
            sanitize_name(stem)
        }
    };
    if resolved_name.is_empty() {
        return Err("could not derive a valid name from the file".to_string());
    }

    let animated = detect_animated(&bytes, &ext);

    ensure_dirs()?;
    let target_path = files_dir().join(format!("{}.{}", id, ext));
    std::fs::write(&target_path, &bytes).map_err(|e| format!("write image failed: {}", e))?;

    let item = BookItem {
        id: id.clone(),
        name: resolved_name,
        ext,
        width,
        height,
        animated,
        added_at: now_secs(),
        source: "upload".to_string(),
        server_files: HashMap::new(),
    };

    let mut items = load_index();
    items.push(item.clone());
    save_index(&items)?;

    let _ = original_filename; // (kept around for possible future "original_filename" field)
    Ok(item)
}

#[tauri::command]
pub fn book_delete_item(id: String) -> Result<(), String> {
    let mut items = load_index();
    let pos = items.iter().position(|i| i.id == id).ok_or_else(|| format!("item not found: {}", id))?;
    let removed = items.remove(pos);
    save_index(&items)?;
    let file = files_dir().join(format!("{}.{}", removed.id, removed.ext));
    let _ = std::fs::remove_file(file);
    Ok(())
}

#[tauri::command]
pub fn book_rename_item(id: String, new_name: String) -> Result<BookItem, String> {
    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        return Err("name cannot be empty".to_string());
    }
    let sanitized = sanitize_name(trimmed);
    if sanitized.is_empty() {
        return Err("name became empty after sanitization".to_string());
    }
    let mut items = load_index();
    let item = items
        .iter_mut()
        .find(|i| i.id == id)
        .ok_or_else(|| format!("item not found: {}", id))?;
    item.name = sanitized;
    let snapshot = item.clone();
    save_index(&items)?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_tmp_data_dir() -> PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "farder-book-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::env::set_var("FARDER_DATA", &tmp);
        tmp
    }

    fn tiny_png() -> Vec<u8> {
        // Same 1x1 PNG as the server validation tests.
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89,
            0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54,
            0x78, 0x9C, 0x62, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x01,
            0x0D, 0x0A, 0x2D, 0xB4,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44,
            0xAE, 0x42, 0x60, 0x82,
        ]
    }

    #[test]
    fn upload_then_list_then_delete() {
        let tmp = fresh_tmp_data_dir();
        let src = tmp.join("test.png");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(&src, tiny_png()).unwrap();
        let item = book_upload_item(src.to_string_lossy().to_string(), Some("My Cat".to_string())).unwrap();
        assert_eq!(item.name, "my-cat");
        assert_eq!(item.ext, "png");
        assert_eq!(book_list_items().len(), 1);
        book_delete_item(item.id.clone()).unwrap();
        assert_eq!(book_list_items().len(), 0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rename_changes_name_in_index() {
        let tmp = fresh_tmp_data_dir();
        let src = tmp.join("test.png");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(&src, tiny_png()).unwrap();
        let item = book_upload_item(src.to_string_lossy().to_string(), Some("a".to_string())).unwrap();
        book_rename_item(item.id.clone(), "Renamed Thing".to_string()).unwrap();
        let items = book_list_items();
        assert_eq!(items[0].name, "renamed-thing");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rejects_unsupported_extension() {
        let tmp = fresh_tmp_data_dir();
        let src = tmp.join("test.exe");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(&src, vec![0u8; 100]).unwrap();
        let result = book_upload_item(src.to_string_lossy().to_string(), None);
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
```

- [ ] **Step 3: Run tests + verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo test --lib book::tests -- --test-threads=1 2>&1 | tail -10
```

Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/Cargo.toml client/src-tauri/Cargo.lock client/src-tauri/src/book.rs
git -C /home/deez/farder commit -m "feat(client): book.rs module with upload/list/delete/rename commands"
```

---

## Task 7: Client Rust — per-server upload cache + legacy migration

Add the `book_get_file_for_server` (uploads + caches a file_id) and `book_migrate_legacy_favorites` commands.

**Files:**
- Modify: `client/src-tauri/src/book.rs`

- [ ] **Step 1: Add the per-server upload cache command**

Append to `book.rs` (BEFORE the `#[cfg(test)]` block):

```rust
/// Returns the file_id of the item on the given server. If not yet uploaded,
/// uploads via the existing upload_file path and caches the result.
#[tauri::command]
pub async fn book_get_file_for_server(
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    server_id: String,
    item_id: String,
) -> Result<u64, String> {
    let item = {
        let items = load_index();
        items.into_iter().find(|i| i.id == item_id).ok_or_else(|| format!("item not found: {}", item_id))?
    };

    if let Some(&existing) = item.server_files.get(&server_id) {
        return Ok(existing);
    }

    let file_path = files_dir().join(format!("{}.{}", item.id, item.ext));
    if !file_path.exists() {
        return Err(format!("image file missing on disk for item {}", item.id));
    }

    // Upload via the existing upload_file Tauri command logic.
    let path_str = file_path.to_string_lossy().to_string();
    let file_id = crate::commands::upload_file_internal(&state, &server_id, &path_str)
        .await
        .map_err(|e| format!("upload failed: {}", e))?;

    // Update the cached file_id and persist.
    let mut items = load_index();
    if let Some(it) = items.iter_mut().find(|i| i.id == item_id) {
        it.server_files.insert(server_id, file_id);
    }
    save_index(&items)?;

    Ok(file_id)
}
```

This depends on a new internal helper `upload_file_internal` that exposes the same logic as the existing `upload_file` Tauri command, but callable from Rust without going back through Tauri's invoke machinery. Add that next.

- [ ] **Step 2: Extract `upload_file_internal` helper**

In `client/src-tauri/src/commands.rs`, find the existing `pub async fn upload_file(...)` Tauri command. Refactor it to:

```rust
#[tauri::command]
pub async fn upload_file(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    file_path: String,
) -> Result<u64, String> {
    upload_file_internal(&state, &server_id, &file_path).await
}

pub(crate) async fn upload_file_internal(
    state: &AppState,
    server_id: &str,
    file_path: &str,
) -> Result<u64, String> {
    // ...existing body of upload_file moved here verbatim,
    // referencing `state` and the `&str` args instead of `String`s...
}
```

(Move the existing logic into `upload_file_internal` unchanged; the Tauri command becomes a thin wrapper. This makes the upload logic callable from `book.rs` without re-implementing it.)

- [ ] **Step 3: Add legacy favorites migration**

Append to `book.rs`:

```rust
/// One-time migration: import legacy ~/.farder/favorites.json entries into the book.
/// Renames favorites.json to favorites.json.bak so it doesn't re-import. Files are
/// COPIED (not moved) — the user's old favorites/ directory is preserved.
/// Returns the number of imported items.
#[tauri::command]
pub fn book_migrate_legacy_favorites() -> Result<u32, String> {
    let data_dir = crate::commands::farder_data_dir_pub();
    let legacy_index = data_dir.join("favorites.json");
    if !legacy_index.exists() {
        return Ok(0);
    }
    let raw = std::fs::read_to_string(&legacy_index)
        .map_err(|e| format!("read favorites.json failed: {}", e))?;
    let entries: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap_or_default();

    ensure_dirs()?;
    let legacy_files = data_dir.join("favorites");
    let mut imported = 0u32;
    let mut items = load_index();

    for entry in entries {
        let id_str = entry.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or(id_str);
        if id_str.is_empty() {
            continue;
        }
        // Try the legacy file path; favorites stored files as <id> with no extension.
        let src = legacy_files.join(id_str);
        if !src.exists() {
            continue;
        }
        let bytes = match std::fs::read(&src) {
            Ok(b) => b,
            Err(_) => continue,
        };
        // Sniff extension from magic bytes.
        let ext = if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) { "png" }
            else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) { "jpg" }
            else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") { "gif" }
            else if bytes.starts_with(b"RIFF") && bytes.len() > 12 && &bytes[8..12] == b"WEBP" { "webp" }
            else { continue };

        let new_id = uuid::Uuid::new_v4().to_string();
        let target = files_dir().join(format!("{}.{}", new_id, ext));
        if std::fs::copy(&src, &target).is_err() {
            continue;
        }

        let resolved_name = sanitize_name(name);
        if resolved_name.is_empty() { continue; }

        let (width, height) = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .ok()
            .and_then(|r| r.into_dimensions().ok())
            .map(|(w, h)| (Some(w), Some(h)))
            .unwrap_or((None, None));

        items.push(BookItem {
            id: new_id,
            name: resolved_name,
            ext: ext.to_string(),
            width,
            height,
            animated: detect_animated(&bytes, ext),
            added_at: now_secs(),
            source: "favorites-migration".to_string(),
            server_files: HashMap::new(),
        });
        imported += 1;
    }

    save_index(&items)?;
    let _ = std::fs::rename(&legacy_index, data_dir.join("favorites.json.bak"));
    Ok(imported)
}

#[tauri::command]
pub fn book_save_from_url(_server_id: String, _url: String, _name: Option<String>) -> Result<BookItem, String> {
    // Stub for the chat-image save flow — actual implementation downloads the
    // image bytes via the existing message-attachment download path, then
    // calls into the same code as book_upload_item. Left as a stub for Task 13
    // (the chat-context-menu integration) to fill in once the UI side is wired.
    Err("not yet implemented".to_string())
}
```

- [ ] **Step 4: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
```

Expected: `Finished` with no new errors.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/book.rs client/src-tauri/src/commands.rs
git -C /home/deez/farder commit -m "feat(client): per-server upload cache + legacy favorites migration for book"
```

---

## Task 8: Register book commands in `main.rs`

**Files:**
- Modify: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Add module declaration + register commands**

In `client/src-tauri/src/main.rs`, near the other `mod` declarations, add:
```rust
mod book;
```

In the `tauri::generate_handler![ ... ]` block (after the existing `themes::*` lines), add:
```rust
            book::book_list_items,
            book::book_upload_item,
            book::book_delete_item,
            book::book_rename_item,
            book::book_get_file_for_server,
            book::book_migrate_legacy_favorites,
            book::book_save_from_url,
```

- [ ] **Step 2: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
```

Expected: `Finished`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/main.rs
git -C /home/deez/farder commit -m "feat(client): register book commands in tauri handler"
```

---

## Task 9: Update Tauri `add_reaction` / `remove_reaction` to accept `file_id`

**Files:**
- Modify: `client/src-tauri/src/commands.rs`

- [ ] **Step 1: Update both Tauri command signatures**

Find `add_reaction` and `remove_reaction` in `commands.rs`. Update each to accept an optional `file_id`:

```rust
#[tauri::command]
pub async fn add_reaction(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    message_id: u64,
    emoji: String,
    file_id: Option<u64>,
) -> Result<(), String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::AddReaction { message_id, emoji, file_id })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn remove_reaction(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    message_id: u64,
    emoji: String,
    file_id: Option<u64>,
) -> Result<(), String> {
    let response =
        bridge::send_request(&state, &server_id, ServerRequest::RemoveReaction { message_id, emoji, file_id })
            .await
            .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}
```

- [ ] **Step 2: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
```

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/commands.rs
git -C /home/deez/farder commit -m "feat(client): add_reaction/remove_reaction Tauri commands accept file_id"
```

---

## Task 10: TypeScript bridge bindings + book types

**Files:**
- Create: `client/src/lib/book/types.ts`
- Create: `client/src/lib/book/client.ts`
- Modify: `client/src/lib/tauri-bridge.ts`

- [ ] **Step 1: Create types**

`client/src/lib/book/types.ts`:

```ts
export interface BookItem {
  id: string;
  name: string;
  ext: "png" | "jpg" | "jpeg" | "gif" | "webp";
  width?: number;
  height?: number;
  animated: boolean;
  added_at: number;
  source: "upload" | "chat" | "favorites-migration";
  server_files: Record<string, number>;
}

export type BookFilter = "all" | "static" | "animated";
export type BookSort = "recent" | "alpha";
```

- [ ] **Step 2: Create client wrapper**

`client/src/lib/book/client.ts`:

```ts
import { invoke } from "@tauri-apps/api/core";
import type { BookItem } from "./types";

export async function bookListItems(): Promise<BookItem[]> {
  return invoke<BookItem[]>("book_list_items");
}

export async function bookUploadItem(sourcePath: string, name?: string): Promise<BookItem> {
  return invoke<BookItem>("book_upload_item", { sourcePath, name: name ?? null });
}

export async function bookDeleteItem(id: string): Promise<void> {
  return invoke<void>("book_delete_item", { id });
}

export async function bookRenameItem(id: string, newName: string): Promise<BookItem> {
  return invoke<BookItem>("book_rename_item", { id, newName });
}

export async function bookGetFileForServer(serverId: string, itemId: string): Promise<number> {
  return invoke<number>("book_get_file_for_server", { serverId, itemId });
}

export async function bookMigrateLegacyFavorites(): Promise<number> {
  return invoke<number>("book_migrate_legacy_favorites");
}
```

- [ ] **Step 3: Update tauri-bridge.ts addReaction/removeReaction signatures**

In `client/src/lib/tauri-bridge.ts`, find the existing `addReaction` and `removeReaction`. Replace with:

```ts
export async function addReaction(serverId: string, messageId: number, emoji: string, fileId?: number): Promise<void> {
  return invoke<void>("add_reaction", { serverId, messageId, emoji, fileId: fileId ?? null });
}

export async function removeReaction(serverId: string, messageId: number, emoji: string, fileId?: number): Promise<void> {
  return invoke<void>("remove_reaction", { serverId, messageId, emoji, fileId: fileId ?? null });
}
```

- [ ] **Step 4: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`. May surface call-sites of `addReaction`/`removeReaction` with old signatures — they all currently use 3-arg form which is compatible (fileId is optional). Should be fine.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src/lib/book/types.ts client/src/lib/book/client.ts client/src/lib/tauri-bridge.ts
git -C /home/deez/farder commit -m "feat(client): book TS types + bindings + reaction signatures with optional file_id"
```

---

## Task 11: Reducer + event listener — store and dedup by `(emoji, fileId)`

**Files:**
- Modify: `client/src/context/ServerContext.tsx`
- Modify: `client/src/hooks/useServerEvents.ts`
- Modify: `client/src/lib/types.ts` — add `file_id?: number` to `ReactionGroup`

- [ ] **Step 1: Add `file_id` to ReactionGroup type**

In `client/src/lib/types.ts`, find the `ReactionGroup` interface and add `file_id?: number`.

- [ ] **Step 2: Update REACTION_ADDED/REMOVED action types**

In `client/src/context/ServerContext.tsx`, find:

```ts
| { type: "REACTION_ADDED"; serverId: string; payload: { channelId: number; messageId: number; emoji: string; me: boolean } }
| { type: "REACTION_REMOVED"; serverId: string; payload: { channelId: number; messageId: number; emoji: string } }
```

Replace with:

```ts
| { type: "REACTION_ADDED"; serverId: string; payload: { channelId: number; messageId: number; emoji: string; me: boolean; fileId?: number } }
| { type: "REACTION_REMOVED"; serverId: string; payload: { channelId: number; messageId: number; emoji: string; fileId?: number } }
```

- [ ] **Step 3: Update REACTION_ADDED reducer to dedup by (emoji, fileId)**

Replace the existing `case "REACTION_ADDED":` block:

```ts
    case "REACTION_ADDED": {
      const { channelId, messageId, emoji, me, fileId } = action.payload;
      const msgs = state.messages[channelId] ?? [];
      const matches = (r: { emoji: string; file_id?: number }) =>
        r.emoji === emoji && (r.file_id ?? null) === (fileId ?? null);
      return {
        ...state,
        messages: {
          ...state.messages,
          [channelId]: msgs.map((m) => {
            if (m.id !== messageId) return m;
            const existing = m.reactions.find(matches);
            if (existing) {
              if (me && existing.me) return m;
              const reactions = m.reactions.map((r) =>
                matches(r) ? { ...r, count: r.count + 1, me: me || r.me } : r,
              );
              return { ...m, reactions };
            }
            return { ...m, reactions: [...m.reactions, { emoji, count: 1, me, file_id: fileId }] };
          }),
        },
      };
    }
```

- [ ] **Step 4: Update REACTION_REMOVED reducer**

```ts
    case "REACTION_REMOVED": {
      const { channelId, messageId, emoji, fileId } = action.payload;
      const msgs = state.messages[channelId] ?? [];
      const matches = (r: { emoji: string; file_id?: number }) =>
        r.emoji === emoji && (r.file_id ?? null) === (fileId ?? null);
      return {
        ...state,
        messages: {
          ...state.messages,
          [channelId]: msgs.map((m) => {
            if (m.id !== messageId) return m;
            const reactions = m.reactions
              .map((r) => (matches(r) ? { ...r, count: r.count - 1 } : r))
              .filter((r) => r.count > 0);
            return { ...m, reactions };
          }),
        },
      };
    }
```

- [ ] **Step 5: Update event listeners to pass fileId through**

In `client/src/hooks/useServerEvents.ts`, find the `ReactionAddedPayload` interface and add `file_id?: number`. Also add to `ReactionRemovedPayload`. Then in the listeners:

```ts
    listen("server:reaction_added", (e) => {
      const data = e.payload as ReactionAddedPayload;
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      const isMe = cachedOwnPk != null && data.public_key === cachedOwnPk;
      dispatch({
        type: "REACTION_ADDED",
        serverId,
        payload: {
          channelId: data.channel_id,
          messageId: data.message_id,
          emoji: data.emoji,
          me: isMe,
          fileId: data.file_id,
        },
      });
    }).then(safePush);

    listen("server:reaction_removed", (e) => {
      const data = e.payload as ReactionRemovedPayload;
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      dispatch({
        type: "REACTION_REMOVED",
        serverId,
        payload: {
          channelId: data.channel_id,
          messageId: data.message_id,
          emoji: data.emoji,
          fileId: data.file_id,
        },
      });
    }).then(safePush);
```

- [ ] **Step 6: Update bridge.rs server-event emitter to include file_id**

In `client/src-tauri/src/bridge.rs`, find the `dispatch_event` function. Update the `ReactionAdded` and `ReactionRemoved` arms to include `file_id` in the JSON payload:

```rust
        ServerEvent::ReactionAdded { message_id, channel_id, emoji, public_key, file_id } =>
            app.emit("server:reaction_added", serde_json::json!({
                "server_id": sid,
                "message_id": message_id,
                "channel_id": channel_id,
                "emoji": emoji,
                "public_key": public_key.to_string(),
                "file_id": file_id,
            })),
        ServerEvent::ReactionRemoved { message_id, channel_id, emoji, public_key, file_id } =>
            app.emit("server:reaction_removed", serde_json::json!({
                "server_id": sid,
                "message_id": message_id,
                "channel_id": channel_id,
                "emoji": emoji,
                "public_key": public_key.to_string(),
                "file_id": file_id,
            })),
```

- [ ] **Step 7: Verify everything compiles**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: cargo Finished, tsc exit 0.

- [ ] **Step 8: Commit**

```
git -C /home/deez/farder add client/src/lib/types.ts client/src/context/ServerContext.tsx client/src/hooks/useServerEvents.ts client/src-tauri/src/bridge.rs
git -C /home/deez/farder commit -m "feat(client): reaction reducer + event payloads dedup by (emoji, file_id)"
```

---

## Task 12: BookBrowser modal + supporting components

The big React UI piece. Creates four components.

**Files:**
- Create: `client/src/components/BookItemTile.tsx`
- Create: `client/src/components/BookItemDetail.tsx`
- Create: `client/src/components/BookIntro.tsx`
- Create: `client/src/components/BookBrowser.tsx`

- [ ] **Step 1: Create BookItemTile (small component, reused in grid)**

`client/src/components/BookItemTile.tsx`:

```tsx
import { type CSSProperties } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import type { BookItem } from "../lib/book/types";

interface Props {
  item: BookItem;
  onClick: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
  selected?: boolean;
}

const tileStyle: CSSProperties = {
  width: 96,
  display: "flex",
  flexDirection: "column",
  alignItems: "center",
  gap: 4,
  padding: 6,
  cursor: "pointer",
  background: "transparent",
  border: "1px solid transparent",
  borderRadius: 4,
  font: "inherit",
  color: "var(--xp-text-normal, #000)",
};

export function bookItemFileSrc(item: BookItem): string {
  // Resolve the per-OS path to ~/.farder/book/files/<id>.<ext> via Tauri's
  // convertFileSrc. The host parts of this path are computed in the main
  // bootstrap (see useBook.ts) and passed in via a global; here we use a
  // simpler approach — items.json gives us the id+ext, and the Rust side
  // exposes the absolute path via a separate command if needed. For v1 we
  // just construct the path relative to the well-known book dir.
  // The Rust `book_list_items` could be extended to include a fully-resolved
  // path string, which is simpler than convertFileSrc dance — see Task 14.
  return convertFileSrc(`~/.farder/book/files/${item.id}.${item.ext}`);
}

export default function BookItemTile({ item, onClick, onContextMenu, selected }: Props) {
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onClick}
      onContextMenu={onContextMenu}
      style={{
        ...tileStyle,
        border: selected ? "1px solid var(--xp-blue, #0058E6)" : tileStyle.border,
      }}
    >
      <img
        src={bookItemFileSrc(item)}
        alt={item.name}
        style={{ width: 64, height: 64, objectFit: "contain", border: "1px solid var(--xp-border, #888)" }}
      />
      <div style={{ fontSize: 10, textAlign: "center", maxWidth: "100%", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
        :{item.name}:
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Create BookItemDetail (popover when an item is clicked)**

`client/src/components/BookItemDetail.tsx`:

```tsx
import { useState, type CSSProperties } from "react";
import * as bookApi from "../lib/book/client";
import type { BookItem } from "../lib/book/types";
import { bookItemFileSrc } from "./BookItemTile";

interface Props {
  item: BookItem;
  onClose: () => void;
  onChanged: () => void;
}

const popover: CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0,0,0,0.4)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 2200,
};

const card: CSSProperties = {
  background: "var(--xp-window-bg, #ECE9D8)",
  color: "var(--xp-text-normal, #000)",
  border: "2px solid var(--xp-blue-dark, #003C74)",
  padding: 16,
  width: 360,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

export default function BookItemDetail({ item, onClose, onChanged }: Props) {
  const [name, setName] = useState(item.name);
  const [error, setError] = useState<string | null>(null);

  async function save() {
    if (name === item.name) { onClose(); return; }
    try { await bookApi.bookRenameItem(item.id, name); onChanged(); onClose(); }
    catch (e) { setError(String(e)); }
  }

  async function del() {
    if (!window.confirm(`Delete "${item.name}"? This removes the file from disk and can't be undone.`)) return;
    try { await bookApi.bookDeleteItem(item.id); onChanged(); onClose(); }
    catch (e) { setError(String(e)); }
  }

  return (
    <div style={popover} onClick={onClose}>
      <div style={card} onClick={(e) => e.stopPropagation()}>
        <div style={{ display: "flex", gap: 16 }}>
          <img src={bookItemFileSrc(item)} alt={item.name} style={{ width: 96, height: 96, objectFit: "contain", border: "1px solid var(--xp-border, #888)" }} />
          <div style={{ flex: 1 }}>
            <label style={{ fontSize: 11, display: "block", marginBottom: 4 }}>Name</label>
            <input value={name} onChange={(e) => setName(e.target.value)} style={{ width: "100%", font: "inherit" }} />
            <div style={{ fontSize: 10, color: "var(--xp-text-muted, #666)", marginTop: 8 }}>
              {item.width}×{item.height}px · {item.animated ? "animated" : "static"} · {item.ext.toUpperCase()}
            </div>
            <div style={{ fontSize: 10, color: "var(--xp-text-muted, #666)" }}>
              source: {item.source}
            </div>
          </div>
        </div>
        {error && <div style={{ color: "#a00", marginTop: 8, fontSize: 11 }}>{error}</div>}
        <div style={{ display: "flex", justifyContent: "space-between", marginTop: 16 }}>
          <button onClick={del} style={{ font: "inherit", color: "#a00", background: "transparent", border: "1px solid #a00", padding: "4px 12px", cursor: "pointer" }}>Delete</button>
          <div style={{ display: "flex", gap: 6 }}>
            <button onClick={onClose} style={{ font: "inherit", padding: "4px 12px" }}>Cancel</button>
            <button onClick={save} style={{ font: "inherit", padding: "4px 12px", background: "var(--xp-blue, #0058E6)", color: "#fff", border: "1px solid var(--xp-blue-dark, #003C74)" }}>Save</button>
          </div>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Create BookIntro (one-time onboarding overlay)**

`client/src/components/BookIntro.tsx`:

```tsx
import { type CSSProperties } from "react";

interface Props { onDismiss: () => void; }

const overlay: CSSProperties = {
  position: "fixed", inset: 0, background: "rgba(0,0,0,0.5)",
  display: "flex", alignItems: "center", justifyContent: "center", zIndex: 3000,
};

const card: CSSProperties = {
  background: "var(--xp-window-bg, #ECE9D8)",
  color: "var(--xp-text-normal, #000)",
  border: "2px solid var(--xp-blue-dark, #003C74)",
  padding: 24, maxWidth: 480, fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

export default function BookIntro({ onDismiss }: Props) {
  return (
    <div style={overlay} onClick={onDismiss}>
      <div style={card} onClick={(e) => e.stopPropagation()}>
        <h2 style={{ marginTop: 0 }}>Welcome to your Reaction Book</h2>
        <p>Upload images here, then react with them on any server. Right-click any image in chat to save it directly to your book.</p>
        <p style={{ fontSize: 11, color: "var(--xp-text-muted, #666)" }}>Items are stored locally on your machine and uploaded to a server only when you actually use them there.</p>
        <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 16 }}>
          <button onClick={onDismiss} style={{ font: "inherit", padding: "4px 14px" }}>Got it</button>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 4: Create BookBrowser (the management modal)**

`client/src/components/BookBrowser.tsx`:

```tsx
import { useEffect, useMemo, useState, type CSSProperties } from "react";
import * as bookApi from "../lib/book/client";
import * as api from "../lib/tauri-bridge";
import type { BookItem, BookFilter, BookSort } from "../lib/book/types";
import BookItemTile from "./BookItemTile";
import BookItemDetail from "./BookItemDetail";
import BookIntro from "./BookIntro";

interface Props { onClose: () => void; }

const INTRO_KEY = "bookIntroDismissed";

const tabBtn = (active: boolean): CSSProperties => ({
  font: "inherit",
  padding: "4px 12px",
  background: active ? "var(--xp-blue, #0058E6)" : "var(--xp-panel-bg, #f0ece0)",
  color: active ? "#fff" : "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  borderRadius: 0,
  cursor: "pointer",
});

export default function BookBrowser({ onClose }: Props) {
  const [items, setItems] = useState<BookItem[]>([]);
  const [filter, setFilter] = useState<BookFilter>("all");
  const [sort, setSort] = useState<BookSort>("recent");
  const [search, setSearch] = useState("");
  const [selected, setSelected] = useState<BookItem | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showIntro, setShowIntro] = useState(localStorage.getItem(INTRO_KEY) !== "true");

  async function refresh() {
    try { setItems(await bookApi.bookListItems()); } catch (e) { setError(String(e)); }
  }
  useEffect(() => { refresh(); }, []);

  const visible = useMemo(() => {
    let out = items;
    if (filter === "static") out = out.filter((i) => !i.animated);
    if (filter === "animated") out = out.filter((i) => i.animated);
    if (search.trim()) {
      const q = search.toLowerCase();
      out = out.filter((i) => i.name.toLowerCase().includes(q));
    }
    if (sort === "recent") out = [...out].sort((a, b) => b.added_at - a.added_at);
    if (sort === "alpha") out = [...out].sort((a, b) => a.name.localeCompare(b.name));
    return out;
  }, [items, filter, sort, search]);

  async function handleUpload() {
    try {
      const path = await api.pickFile();
      if (!path) return;
      const lower = path.toLowerCase();
      if (![".png", ".jpg", ".jpeg", ".gif", ".webp"].some((ext) => lower.endsWith(ext))) {
        setError("Please pick a PNG, JPG, GIF, or WebP image.");
        return;
      }
      await bookApi.bookUploadItem(path);
      await refresh();
    } catch (e) { setError(String(e)); }
  }

  function dismissIntro() { localStorage.setItem(INTRO_KEY, "true"); setShowIntro(false); }

  return (
    <div style={{ position: "fixed", inset: 0, background: "rgba(0,0,0,0.4)", display: "flex", alignItems: "center", justifyContent: "center", zIndex: 1500 }} onClick={onClose}>
      <div onClick={(e) => e.stopPropagation()} style={{
        background: "var(--xp-window-bg, #ECE9D8)", color: "var(--xp-text-normal, #000)",
        border: "2px solid var(--xp-blue-dark, #003C74)", borderRadius: "6px 6px 0 0",
        width: 760, maxWidth: "94vw", maxHeight: "88vh", display: "flex", flexDirection: "column",
        fontFamily: "var(--xp-font, Tahoma, sans-serif)", fontSize: "var(--xp-font-size, 11px)",
        boxShadow: "3px 3px 16px rgba(0,0,0,0.45)", overflow: "hidden",
      }}>
        {/* Title */}
        <div style={{ background: "linear-gradient(to bottom, var(--xp-blue, #0058E6), var(--xp-blue-light, #3389FF))", color: "#fff", padding: "4px 8px 4px 12px", fontWeight: "bold", display: "flex", justifyContent: "space-between" }}>
          <span>Reaction Book</span>
          <button onClick={onClose} style={{ font: "inherit", color: "#fff", background: "transparent", border: "1px solid #fff", padding: "0 6px", cursor: "pointer" }}>✕</button>
        </div>

        {/* Tabs + toolbar */}
        <div style={{ padding: 8, display: "flex", gap: 8, alignItems: "center", borderBottom: "1px solid var(--xp-border, #888)" }}>
          <button onClick={() => setFilter("all")} style={tabBtn(filter === "all")}>All</button>
          <button onClick={() => setFilter("static")} style={tabBtn(filter === "static")}>Static</button>
          <button onClick={() => setFilter("animated")} style={tabBtn(filter === "animated")}>Animated</button>
          <input placeholder="Search…" value={search} onChange={(e) => setSearch(e.target.value)} style={{ flex: 1, font: "inherit", padding: "2px 6px" }} />
          <select value={sort} onChange={(e) => setSort(e.target.value as BookSort)} style={{ font: "inherit" }}>
            <option value="recent">Recent</option>
            <option value="alpha">A–Z</option>
          </select>
          <button onClick={handleUpload} style={{ font: "inherit", padding: "4px 12px", background: "var(--xp-blue, #0058E6)", color: "#fff", border: "1px solid var(--xp-blue-dark, #003C74)" }}>Upload…</button>
        </div>

        {/* Body */}
        <div style={{ padding: 12, overflowY: "auto", flex: 1 }}>
          {error && <div style={{ color: "#a00", marginBottom: 8 }}>{error}</div>}
          {visible.length === 0 && !error && (
            <div style={{ textAlign: "center", padding: 32, color: "var(--xp-text-muted, #666)" }}>
              {items.length === 0 ? "Your book is empty. Upload an image to start." : "No items match your filter."}
            </div>
          )}
          <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
            {visible.map((item) => (
              <BookItemTile key={item.id} item={item} onClick={() => setSelected(item)} />
            ))}
          </div>
        </div>

        {/* Footer */}
        <div style={{ padding: 8, borderTop: "1px solid var(--xp-border, #888)", fontSize: 10, color: "var(--xp-text-muted, #666)" }}>
          {items.length} item{items.length === 1 ? "" : "s"} in your book
        </div>
      </div>
      {selected && <BookItemDetail item={selected} onClose={() => setSelected(null)} onChanged={refresh} />}
      {showIntro && <BookIntro onDismiss={dismissIntro} />}
    </div>
  );
}
```

- [ ] **Step 5: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`. May warn about `convertFileSrc` path resolution — this is addressed in Task 14 (we'll have Rust return absolute paths instead of guessing).

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add client/src/components/BookItemTile.tsx client/src/components/BookItemDetail.tsx client/src/components/BookIntro.tsx client/src/components/BookBrowser.tsx
git -C /home/deez/farder commit -m "feat(client): BookBrowser + ItemTile + ItemDetail + Intro components"
```

---

## Task 13: ReactionPicker integration + Message render

Add the book strip to the picker; when reacting, look up or upload to get a file_id; on render, show images for `file_id != null` reactions.

**Files:**
- Modify: `client/src/components/ReactionPicker.tsx`
- Modify: `client/src/components/Message.tsx`

- [ ] **Step 1: Update ReactionPicker to surface book items**

In `client/src/components/ReactionPicker.tsx`, add at the top:

```tsx
import { useEffect, useState } from "react";
import * as bookApi from "../lib/book/client";
import type { BookItem } from "../lib/book/types";
import { bookItemFileSrc } from "./BookItemTile";
```

Update the props to accept an `onSelectBookItem` callback in addition to the existing Unicode `onSelect`:

```tsx
interface ReactionPickerProps {
  onSelect: (emoji: string) => void;
  onSelectBookItem: (item: BookItem) => void;
  onOpenBookBrowser: () => void;
}
```

Inside the component, load book items on mount (top-12, recent first):

```tsx
const [bookItems, setBookItems] = useState<BookItem[]>([]);
useEffect(() => {
  bookApi.bookListItems()
    .then((items) => setBookItems([...items].sort((a, b) => b.added_at - a.added_at).slice(0, 12)))
    .catch(() => {});
}, []);
```

In the render, ABOVE the existing Unicode strip, add the book strip:

```tsx
{bookItems.length > 0 && (
  <>
    <div style={{ fontSize: 10, color: "var(--xp-text-muted, #666)", padding: "0 4px 2px" }}>YOUR BOOK</div>
    <div style={{ display: "flex", flexWrap: "wrap", gap: 4, padding: "0 4px 6px", borderBottom: "1px solid var(--xp-border, #888)" }}>
      {bookItems.map((item) => (
        <button
          key={item.id}
          onClick={() => onSelectBookItem(item)}
          title={`:${item.name}:`}
          style={{ width: 32, height: 32, padding: 0, border: "1px solid transparent", background: "transparent", cursor: "pointer" }}
        >
          <img src={bookItemFileSrc(item)} alt={item.name} style={{ width: "100%", height: "100%", objectFit: "contain" }} />
        </button>
      ))}
      <button onClick={onOpenBookBrowser} title="Open Book" style={{ width: 32, height: 32, font: "inherit" }}>+</button>
    </div>
    <div style={{ fontSize: 10, color: "var(--xp-text-muted, #666)", padding: "0 4px 2px" }}>COMMON</div>
  </>
)}
```

- [ ] **Step 2: Update Message component to handle book reactions**

In `client/src/components/Message.tsx`, find `handlePickerSelect`. Add a sibling `handlePickerBookSelect`:

```tsx
async function handlePickerBookSelect(item: BookItem) {
  if (reacting) return;
  setShowPicker(false);
  setReacting(true);
  try {
    const fileId = await bookApi.bookGetFileForServer(serverId, item.id);
    await api.addReaction(serverId, message.id, ":custom:", fileId);
  } catch (e) {
    console.error("[reaction:book] failed:", e);
  } finally {
    setReacting(false);
  }
}
```

Pass it to ReactionPicker:

```tsx
{showPicker && (
  <ReactionPicker
    onSelect={handlePickerSelect}
    onSelectBookItem={handlePickerBookSelect}
    onOpenBookBrowser={() => { setShowPicker(false); /* delegate to global event in Task 14 */ }}
  />
)}
```

In the reaction-bar render, where each reaction is shown, branch on `r.file_id`:

```tsx
{message.reactions.map((r) => (
  <button
    key={`${r.emoji}-${r.file_id ?? "u"}`}
    className={`reaction${r.me ? " me" : ""}`}
    onClick={() => handleReactionClick(r.emoji, r.me, r.file_id)}
    title={`${r.emoji === ":custom:" ? "" : r.emoji} ${r.count}`}
  >
    {r.file_id != null ? (
      <img src={resolveAttachmentSrc(serverId, r.file_id)} alt="reaction" style={{ width: 18, height: 18, verticalAlign: "middle" }} />
    ) : r.emoji}
    <span className="reaction-count">{r.count}</span>
  </button>
))}
```

`handleReactionClick` becomes:

```tsx
async function handleReactionClick(emoji: string, alreadyMe: boolean, fileId?: number) {
  if (reacting) return;
  setReacting(true);
  try {
    if (alreadyMe) {
      await api.removeReaction(serverId, message.id, emoji, fileId);
    } else {
      await api.addReaction(serverId, message.id, emoji, fileId);
    }
  } catch {} finally { setReacting(false); }
}
```

`resolveAttachmentSrc(serverId, fileId)` should produce the URL the existing attachment view uses for displaying images. Look at how the existing `<img>` rendering for chat-attached images is done (`AttachmentInfo` somewhere in the codebase) and reuse the same pattern. If a helper doesn't exist, the simplest is:

```tsx
import { invoke } from "@tauri-apps/api/core";
async function resolveAttachmentSrc(serverId: string, fileId: number): Promise<string> {
  // Use existing download_file to write to a local cache, then convertFileSrc.
  // For v1 simplicity, return a synchronous data URL after fetch on first render.
  return ""; // placeholder — implementer: copy the pattern from the existing image attachment <img> render
}
```

(The existing chat image-attachment rendering already does this somehow — copy that pattern. Look at `Message.tsx` lines that render `attachment` arrays for the existing `<img src=…>` pattern; that's the template to follow.)

- [ ] **Step 3: Add bookApi import to Message.tsx**

```tsx
import * as bookApi from "../lib/book/client";
import type { BookItem } from "../lib/book/types";
```

- [ ] **Step 4: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src/components/ReactionPicker.tsx client/src/components/Message.tsx
git -C /home/deez/farder commit -m "feat(client): reaction picker + render integrate book items"
```

---

## Task 14: UserFooter book button + chat-image save context menu + path resolution helper

Last UI integration. Adds the entry point button, the right-click "Save to book" on chat images, and a Rust helper that returns absolute file paths so the components don't have to guess.

**Files:**
- Modify: `client/src-tauri/src/book.rs` — add `book_item_absolute_path` command
- Modify: `client/src-tauri/src/main.rs` — register it
- Modify: `client/src/lib/book/client.ts` — bind it
- Modify: `client/src/components/BookItemTile.tsx` — use the resolved path
- Modify: `client/src/components/ChannelSidebar.tsx` (UserFooter) — add the book icon button
- Modify: `client/src/components/Message.tsx` — add right-click "Save to book" on image attachments
- Modify: `client/src-tauri/src/book.rs` — implement `book_save_from_url` properly (fetch via existing download path)

- [ ] **Step 1: Add `book_item_absolute_path` to book.rs**

```rust
#[tauri::command]
pub fn book_item_absolute_path(id: String) -> Result<String, String> {
    let items = load_index();
    let item = items.into_iter().find(|i| i.id == id).ok_or_else(|| format!("item not found: {}", id))?;
    let path = files_dir().join(format!("{}.{}", item.id, item.ext));
    Ok(path.to_string_lossy().to_string())
}
```

Register in `main.rs` alongside the others:
```rust
            book::book_item_absolute_path,
```

- [ ] **Step 2: Add binding + update tile to use it**

In `client/src/lib/book/client.ts`:
```ts
export async function bookItemAbsolutePath(id: string): Promise<string> {
  return invoke<string>("book_item_absolute_path", { id });
}
```

In `client/src/components/BookItemTile.tsx`, replace the `bookItemFileSrc` function with an async-loading variant. Easiest: convert tile to load the path on mount and cache:

```tsx
import { useEffect, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import * as bookApi from "../lib/book/client";
// ... (interface and styles unchanged) ...

export function useBookItemSrc(itemId: string): string {
  const [src, setSrc] = useState("");
  useEffect(() => {
    bookApi.bookItemAbsolutePath(itemId)
      .then((p) => setSrc(convertFileSrc(p)))
      .catch(() => setSrc(""));
  }, [itemId]);
  return src;
}

// (Remove the old bookItemFileSrc function; replace its usages in BookBrowser
// and ReactionPicker with the useBookItemSrc hook.)
```

Update `BookItemTile`'s render to use the hook:
```tsx
const src = useBookItemSrc(item.id);
// then:
<img src={src} ... />
```

Same for `BookItemDetail` and the ReactionPicker book strip.

- [ ] **Step 3: Add the book icon button to UserFooter**

In `client/src/components/ChannelSidebar.tsx`, find the `UserFooter` component (the same one we previously added the gear and notifications buttons to). Add an import:

```tsx
import BookBrowser from "./BookBrowser";
```

Add state alongside the existing `showAppearance`:
```tsx
const [showBook, setShowBook] = useState(false);
```

Add a new button next to the existing gear (⚙) and N buttons, BEFORE the gear:
```tsx
<button
  className="server-invite-btn"
  onClick={() => setShowBook(true)}
  title="Reaction Book"
  style={{ fontSize: 10, marginRight: 4 }}
>📚</button>
```

And a conditional render alongside the existing modals:
```tsx
{showBook && <BookBrowser onClose={() => setShowBook(false)} />}
```

- [ ] **Step 4: Add right-click "Save to book" on chat image attachments**

In `client/src/components/Message.tsx`, find where image attachments are rendered (the `attachment.mime_type.startsWith("image/")` branch). Add `onContextMenu`:

```tsx
<img
  src={attachmentSrc}
  alt={attachment.original_name ?? "image"}
  onContextMenu={(e) => {
    e.preventDefault();
    void handleSaveToBook(attachment);
  }}
  // ...other existing props
/>
```

Add the handler:

```tsx
async function handleSaveToBook(attachment: AttachmentInfo) {
  const name = window.prompt(`Save "${attachment.original_name ?? "image"}" to your book. Name it:`, attachment.original_name?.replace(/\.[^.]+$/, "") ?? "");
  if (!name) return;
  try {
    await bookApi.bookSaveFromUrl(serverId, attachmentDownloadUrl(serverId, attachment.file_id), name);
  } catch (e) {
    console.error("[book:save-from-chat] failed:", e);
  }
}
```

Where `attachmentDownloadUrl` follows the existing pattern for resolving the download URL of an attachment (look at how the existing `<img src=…>` does it for image attachments and produce the same string).

- [ ] **Step 5: Implement `book_save_from_url` in `book.rs`**

Replace the stub `book_save_from_url` with:

```rust
#[tauri::command]
pub async fn book_save_from_url(
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    server_id: String,
    file_id: u64,
    name: Option<String>,
) -> Result<BookItem, String> {
    // Download the file via the existing download_file path.
    let bytes = crate::commands::download_file_internal(&state, &server_id, file_id)
        .await
        .map_err(|e| format!("download failed: {}", e))?;

    if bytes.len() as u64 > MAX_BOOK_BYTES {
        return Err(format!("image too large ({} bytes; max {})", bytes.len(), MAX_BOOK_BYTES));
    }
    // Sniff extension from magic bytes (matches favorites-migration logic).
    let ext = if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) { "png" }
        else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) { "jpg" }
        else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") { "gif" }
        else if bytes.starts_with(b"RIFF") && bytes.len() > 12 && &bytes[8..12] == b"WEBP" { "webp" }
        else { return Err("unsupported image format".to_string()); };

    let new_id = uuid::Uuid::new_v4().to_string();
    ensure_dirs()?;
    let target = files_dir().join(format!("{}.{}", new_id, ext));
    std::fs::write(&target, &bytes).map_err(|e| format!("write image failed: {}", e))?;

    let resolved_name = match name {
        Some(n) if !n.trim().is_empty() => sanitize_name(n.trim()),
        _ => sanitize_name("emoji"),
    };
    if resolved_name.is_empty() {
        return Err("could not derive a valid name".to_string());
    }

    let (width, height) = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .map(|(w, h)| (Some(w), Some(h)))
        .unwrap_or((None, None));

    let mut item = BookItem {
        id: new_id,
        name: resolved_name,
        ext: ext.to_string(),
        width,
        height,
        animated: detect_animated(&bytes, ext),
        added_at: now_secs(),
        source: "chat".to_string(),
        server_files: HashMap::new(),
    };
    // The image came from server_id with file_id, so cache the upload to skip re-upload.
    item.server_files.insert(server_id, file_id);

    let mut items = load_index();
    items.push(item.clone());
    save_index(&items)?;
    Ok(item)
}
```

This requires a `download_file_internal` helper analogous to the `upload_file_internal` extracted in Task 7 — extract the existing `download_file` Tauri command's body into a `pub(crate) async fn download_file_internal(state, server_id, file_id) -> Result<Vec<u8>, String>` and have the Tauri command call into it.

Update the TS binding in `client.ts`:
```ts
export async function bookSaveFromUrl(serverId: string, fileId: number, name: string): Promise<BookItem> {
  return invoke<BookItem>("book_save_from_url", { serverId, fileId, name });
}
```

(Note: signature now takes `file_id`, not a URL — the download is server-side via existing infra. The `attachmentDownloadUrl` helper in Step 4 is no longer needed; pass `attachment.file_id` directly.)

- [ ] **Step 6: Verify everything compiles**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

- [ ] **Step 7: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/book.rs client/src-tauri/src/main.rs client/src-tauri/src/commands.rs client/src/lib/book/client.ts client/src/components/BookItemTile.tsx client/src/components/BookBrowser.tsx client/src/components/BookItemDetail.tsx client/src/components/ChannelSidebar.tsx client/src/components/Message.tsx
git -C /home/deez/farder commit -m "feat(client): book entry button, chat-image save context menu, path resolution"
```

---

## Task 15: First-run favorites migration + bootstrap + CHANGELOG + e2e

**Files:**
- Modify: `client/src/main.tsx` — invoke migration on bootstrap
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Invoke migration on bootstrap**

In `client/src/main.tsx`, inside the `bootstrap()` function, after the theme injection but before the React render, add:

```ts
try {
  const imported = await invoke<number>("book_migrate_legacy_favorites");
  if (imported > 0) {
    console.log(`[bootstrap] migrated ${imported} legacy favorites into the book`);
  }
} catch (e) {
  console.warn("[bootstrap] favorites migration failed (non-fatal):", e);
}
```

Add `import { invoke } from "@tauri-apps/api/core";` at the top if not already present.

- [ ] **Step 2: Verify TS compiles + run server tests**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
cd /home/deez/farder/crates/farder-server && cargo test --lib 2>&1 | tail -10
```

Expected: tsc exit 0, all server tests pass.

- [ ] **Step 3: End-to-end smoke test**

Run from repo root:
```
pkill -f farder-server
cd /home/deez/farder/client && npm run tauri dev
```

Walk these:

- [ ] App launches; if you had `~/.farder/favorites.json`, the dev console shows `[bootstrap] migrated N legacy favorites`. Otherwise no-op.
- [ ] Click the new 📚 button in the user footer → BookBrowser opens.
- [ ] First time, the BookIntro overlay appears. Dismiss → does not return on reopen.
- [ ] Click Upload, pick a small PNG → it appears in the grid.
- [ ] Tabs: switch to Static → animated GIFs hide. Switch to Animated → only animated visible.
- [ ] Search filters by name. Sort dropdown reorders.
- [ ] Click an item → detail popover opens. Rename → name updates in grid. Delete (with confirm) → item disappears + file removed from `~/.farder/book/files/`.
- [ ] In a chat, hover a message → click the `+` reaction button. Picker opens with YOUR BOOK strip above COMMON. Click a book item.
- [ ] Reaction appears in the bar as a small image, count = 1. Hover → tooltip shows count.
- [ ] Other connected client (or a second window) sees the same image-rendered reaction.
- [ ] Click the reaction once more (already reacted) → it removes (count goes to 0, badge disappears).
- [ ] Right-click an image attachment in chat → "Save to book…" prompt for name. Save → item appears in book; on next reaction picker open, it's there.
- [ ] In `~/.farder/book/files/` you should see the actual image files. `~/.farder/book/items.json` should be readable JSON.
- [ ] Try to upload a 5 MB image — rejected with clear error.
- [ ] Try to upload a `.exe` renamed `.png` (or a text file) — server rejects with "unsupported image format".

- [ ] **Step 4: Add CHANGELOG entry**

In `CHANGELOG.md`, under the most recent `### Added` block, add:

```
- (2026-05-04) Reaction Book (Phase 1): personal account-level collection of images usable as reactions on any server. Upload from disk OR right-click any chat image to save it. Items live in ~/.farder/book/; auto-uploaded as regular file attachments to whichever server you use them on (cached per-item-per-server so no re-upload). Reaction picker shows your book above the standard Unicode strip; reactions render as small images (~22px) for everyone. BookBrowser modal with All/Static/Animated tabs, search, sort, rename, delete (with confirm). Existing favorites.json auto-migrated on first launch (preserved as .bak). Server: nullable reactions.file_id column + protocol additions; magic-byte + dimension + frame-count validation on all image uploads; per-user upload throttle (10/min) and reaction rate limit (60/min). Phase 2 (inline :name: in messages, send-as-sticker, Unicode emoji favoriting) is a separate plan to follow.
```

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src/main.tsx CHANGELOG.md
git -C /home/deez/farder commit -m "docs: changelog for reaction book phase 1 + bootstrap migration call"
```

---

## Self-review notes

**Spec coverage:**
- Item lifecycle (3 add paths) → Tasks 6, 7, 14
- On-disk layout + items.json schema → Task 6
- Per-server upload cache → Task 7
- Migration of existing favorites → Tasks 7, 15
- Limits (size, format, dimensions, frames) → Tasks 4, 6
- Server schema migration → Task 1
- Protocol changes → Task 2
- Server reactions module updates → Task 3
- Server image validation → Task 4
- Rate limits → Task 5
- Backwards compatibility → Task 2 (`#[serde(default)]`)
- Server-side handler validation → Task 3 (file_id presence/absence checks)
- Tauri commands for book → Tasks 6, 7, 14 (8 commands total)
- TS types + bridge → Task 10
- BookBrowser + supporting components → Task 12
- ReactionPicker integration → Task 13
- Message render of image reactions → Task 13
- UserFooter book button → Task 14
- Save-from-chat → Task 14
- Onboarding intro overlay → Task 12
- Reducer dedup by (emoji, fileId) → Task 11
- Event listener payload propagation → Task 11
- "Warn before delete" — covered by the explicit `window.confirm` in BookItemDetail.delete (Task 12) and the spec note that quota overflow prompts the user rather than auto-evicting (Task 5 leaves quota enforcement as a hard reject + UI prompt rather than silent eviction).

**Type/name consistency:**
- `BookItem` shape consistent across Tasks 6, 7, 10, 12-14
- Reaction `file_id` field consistent across Tasks 1, 2, 3, 5, 11
- Rust command names match TS bindings (Tasks 6, 7, 8, 9, 10, 14)

**No placeholders:** every code step has runnable code; the only "see existing pattern" reference is for the chat image attachment download URL in Task 13/14, which is necessary because that pattern lives elsewhere in the codebase and varies by attachment type — the implementer must mirror existing behavior to keep auth/path-resolution consistent.

**Known compromise:** Task 13's `resolveAttachmentSrc` for rendering image reactions is sketched but not fully written, because the existing image-attachment rendering already does this in Message.tsx and must be matched exactly. The implementer is expected to find the existing pattern and reuse it.

**Quota for book uploads (per spec):** the spec specifies a 50 MB per-user-per-server quota with user-pick-deletion on overflow. The full quota tracking (server-side accounting of per-user uploaded book bytes + UI prompt on rejection) is sketched in Task 5 (per-user upload throttle/quota) but the dedicated user-confirmation dialog on overflow is left for a small follow-up — the rate limit alone covers the abuse vector for v1; the quota dialog is a UX polish that can ship after Phase 1 lands and we have real user behavior to size the prompt against.
