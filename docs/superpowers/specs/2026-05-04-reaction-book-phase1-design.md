# Reaction Book — Phase 1 Design

**Status:** Approved 2026-05-04
**Scope:** Farder client (Tauri + React) and server (farder-server crate)
**Phase:** 1 of 2. Phase 2 (inline `:name:` rendering, send-as-sticker mode, Unicode favoriting) is a separate spec written after Phase 1 ships.

## Goal

Give every Farder user a personal "Reaction Book" — an account-level collection of images they can use as reactions on any server. Solves the most-asked Discord feature (custom emoji) without locking them to a paid Nitro tier or a single server. Items live locally, are uploaded as regular file attachments to whichever server they're used on, and other clients render them like any image attachment.

## Non-Goals (Phase 1)

- Inline `:name:` rendering in message text. Phase 2.
- "Send as sticker" — picking a book item and sending it as a standalone message. Phase 2 (replaces existing favorites send behavior).
- Unicode emoji favoriting (heart icon in the standard picker to pin Unicode emoji to the book). Phase 2.
- Decentralized hosting on personal nodes (the "right" answer eventually but blocked on Phase 5+ infra). The current model is auto-upload-on-use to whichever server the item is being used on.
- Server-side image re-encoding for normalization. Magic-byte + dimension + frame-count checks cover the realistic attack surface; revisit if real polyglot or codec attacks emerge.
- AV scanning, content moderation, NSFW detection.
- Per-server custom-emoji concept (Discord-style). The book is account-level; items are not "owned" by a server even though they're stored as attachments there.

## Architecture

### Item lifecycle

1. **Add to book** (three sources, all unified into one item type):
   - **Upload from disk** — file picker, choose images, name them.
   - **Save from chat** — right-click any image attachment in chat → "Save to book." Replaces the existing "Favorite" action; the existing `~/.farder/favorites/` system is migrated into the new book on first launch.
   - **Migration on first run** — the existing `favorites.json` index is read and each entry is imported as a book item with `source: "favorites-migration"`. Files are copied (not moved) to `~/.farder/book/files/`. The old index file is renamed to `favorites.json.bak`.
2. **Use as a reaction** on a message in any server:
   - Client checks `item.server_files[<server_id>]` for a cached file_id from a previous upload.
   - If absent, client uploads the item's image bytes via the existing `upload_file` Tauri command, gets back a file_id, caches it in `server_files`.
   - Client calls `add_reaction(message_id, ":custom:", file_id=Some(<id>))`.
   - Server stores the reaction with the file_id, broadcasts `ReactionAdded` to channel subscribers including the file_id.
3. **Render** on receiving clients:
   - For `file_id == None` reactions: render the unicode `emoji` string as text (current behavior, unchanged).
   - For `file_id != None` reactions: fetch the file via existing attachment-download path, render as a small `<img>` (~22px) in the reaction bar. The `emoji` column value (`:custom:`) is just an opaque grouping key — never displayed.

### On-disk layout

```
~/.farder/book/
  items.json           — index of all items
  files/
    <item-id>.png      — actual image files (original extension preserved)
    <item-id>.gif
    ...
~/.farder/favorites.json.bak    — old favorites index, preserved for one release
~/.farder/favorites/             — old favorites image dir (migration source)
```

### Item shape (in `items.json`)

```ts
interface BookItem {
  id: string;              // local UUID, also the filename stem
  name: string;            // [a-z0-9_-]{1,32}; required (auto-generated from filename if not supplied)
  ext: "png" | "jpg" | "jpeg" | "gif" | "webp";
  width?: number;
  height?: number;
  animated: boolean;       // computed once at upload; PNG/JPG always false; GIF always true; WebP requires header parse
  added_at: number;        // unix seconds
  source: "upload" | "chat" | "favorites-migration";
  // Per-server upload cache. First time you use this item on server X,
  // the client uploads and stores the resulting file_id here. Subsequent
  // uses skip re-upload entirely.
  server_files: Record<string, number>;  // server_id → file_id
  // Phase 2 will add a `use_count: number` field for the most-used sort.
}
```

### Limits

- **Max file size:** 2 MB hard cap, soft warning at 512 KB. Custom emoji broadcast through every reaction event — much higher amplification than a typical attachment, so we cap aggressively. (Compare: Discord's emoji cap is 256 KB.)
- **Supported formats:** PNG, JPG/JPEG, GIF, WebP. Covers static + animated. SVG is **not** in the allowlist (would let user-supplied scripts execute in the renderer).
- **Decoded dimensions:** width ≤ 4096, height ≤ 4096, total pixels ≤ 4,000,000 (defends against decompression bombs).
- **Animation frame count:** GIF / animated WebP ≤ 200 frames.
- **Name:** required, sanitized to `[a-z0-9_-]`, max 32 chars; auto-generated from filename stem (lowercased, sanitized) if not supplied.
- **Items per book:** no hard cap; UI paginates at 200.

## Server changes

### Schema migration

```sql
ALTER TABLE reactions ADD COLUMN file_id INTEGER NULL REFERENCES files(id);
```

`NULL` for Unicode reactions (current behavior preserved). Set when the reaction is a custom emoji. The existing `INSERT OR IGNORE` uniqueness of `(message_id, user_key, emoji)` is unchanged — for custom emoji we use the sentinel emoji string `:custom:` so the conceptual key becomes `(message_id, user_key, ":custom:", file_id)`. Two different custom emoji from the same user produce two distinct rows.

### Protocol changes (`farder-protocol/src/server.rs`)

- `ServerRequest::AddReaction { message_id, emoji, file_id: Option<u64> }` — new optional field.
- `ServerEvent::ReactionAdded { ..., file_id: Option<u64> }` — new optional field.
- `ServerEvent::ReactionRemoved { ..., file_id: Option<u64> }` — new optional field (so removal targets the right custom-emoji row).
- `ReactionGroup { emoji, count, me, file_id: Option<u64> }` — new optional field; included in fetch_history responses.

### Backwards compatibility

- Old client → new server: optional fields default to `None`, server treats as Unicode-only. Works unchanged.
- New client → old server: server doesn't know `file_id`, deserializes optional field as `None`, treats request as Unicode-only. Custom emoji uses fail with a server error (which the client surfaces as "this server doesn't support custom emoji").

### Server-side validation (added at upload time, applies to all attachments — not book-specific)

These checks run in `add_theme_asset`-style at the existing `upload_file` Tauri command's server-side entry point. They protect every attachment, not just book items.

- **Magic-byte check:** read first 16 bytes; reject if not one of: PNG (`89 50 4E 47`), JPEG (`FF D8 FF`), GIF (`47 49 46 38`), WebP (`52 49 46 46 .. .. .. .. 57 45 42 50`), or one of the existing allowed non-image types if any.
- **Decoded dimensions:** use `image::Reader::with_format(...).into_dimensions()` (header-only — does not allocate a pixel buffer). Reject if either axis > 4096 or total pixels > 4M.
- **Animated frame count:** for GIF and WebP, walk chunk headers (cheap, no full decode). Reject > 200 frames.
- **Per-user upload throttling:** token-bucket. 10 uploads / minute, 60 / hour per user per server. Already-cached items (re-using a cached file_id) don't count.
- **Per-user storage quota:** 50 MB total of book-item-uploaded files per user per server. **When exceeded, the server returns an error and the client surfaces a dialog: "You've used 50 MB of book-item storage on this server. Pick items to delete to free space."** No automatic reaping — user always confirms. (This matches the user's stated preference for warn-before-delete.)
- **Reaction rate limit:** 60 reaction adds per channel per minute per user. Removal not throttled.

### Server-side handler validation

`AddReaction` handler verifies (when `file_id` is provided):
- The file exists in the `files` table.
- The file is an image type (mime starts with `image/`).
- The file's owner is the requester OR the file is already attached to a message the requester can see (prevents using random file_ids from other channels as reactions where the requester wouldn't normally have access).

## Client changes

### New Rust commands (in a new module `client/src-tauri/src/book.rs`)

```rust
book_list_items() -> Vec<BookItem>
book_upload_item(source_path: String, name: Option<String>) -> Result<BookItem, String>
book_delete_item(id: String) -> Result<(), String>
book_rename_item(id: String, new_name: String) -> Result<(), String>
book_save_from_url(server_id: String, url: String, name: Option<String>) -> Result<BookItem, String>
book_get_file_for_server(server_id: String, item_id: String) -> Result<u64, String>
//  ^ checks item.server_files[server_id]; uploads and caches if missing; returns file_id
book_migrate_legacy_favorites() -> Result<u32, String>
//  ^ run once on app startup; returns number of imported items
```

### New TS module (`client/src/lib/book/`)

- `types.ts` — `BookItem`, `ImageFit`, `BookFilter` (all/static/animated), `BookSort` (recent/a-z/most-used)
- `client.ts` — typed wrappers around the Rust commands
- `useBook.ts` — React hook that loads items, exposes filter/sort state, refreshes on mutation

### New components

- `BookBrowser.tsx` — full management modal (tabs, sort, search, grid, upload, item detail popover)
- `BookItemTile.tsx` — single grid tile (image + name)
- `BookItemDetail.tsx` — popover with image preview, name editor, dimensions, "Open file location"
- `SaveToBookDialog.tsx` — small modal for "Save from chat" flow (thumbnail + name input)

### Modified components

- `ReactionPicker.tsx` — adds the book strip above the Unicode strip. "+" tile opens BookBrowser. Up to 12 most-recently-used items shown.
- `Message.tsx` — reaction bar renders `<img>` for reactions where `file_id != null`, fetched via existing attachment URL pattern.
- `ChannelSidebar.tsx` (UserFooter) — adds a third icon button (📚 or similar) next to the existing ⚙ (Appearance) and N (Notifications). Opens BookBrowser.
- `ChatPanel.tsx` (image attachment context menu) — "Favorite" → "Save to book."

### Reducer changes (`ServerContext.tsx`)

`REACTION_ADDED` payload gains `fileId?: number`. The dedup logic stays the same but groups by `(emoji, fileId)` rather than just `emoji` — two different custom emoji are distinct groups even though both have `emoji = ":custom:"`.

```ts
const existing = m.reactions.find(
  (r) => r.emoji === emoji && (r.file_id ?? null) === (fileId ?? null)
);
```

`REACTION_REMOVED` likewise gains `fileId?: number` and uses the same paired key.

## UX

### Reaction picker layout

```
┌─────────────────────────────────────┐
│  YOUR BOOK                           │
│  [item] [item] [item] [item]         │  ← up to 12, most-recently-used first
│  [item] [item] [+]                   │  ← "+" opens BookBrowser
│  ───────────────────────────────     │
│  COMMON                              │
│  😀 😎 ❤️ 🔥 👍 🎉 ...              │  ← existing strip, unchanged
└─────────────────────────────────────┘
```

If the book is empty, the YOUR BOOK section is replaced with a small "Add custom emoji" link that opens BookBrowser.

### BookBrowser layout

- **Title bar:** "Reaction Book" + close (✕)
- **Tabs:** All · Static · Animated  (filter by `item.animated`)
- **Toolbar:** Upload button · Search input · Sort dropdown (recent / a-z / most-used)
- **Grid:** ~5 columns of 80×80px tiles, name underneath
- **Item interactions:**
  - Click → opens detail popover
  - Right-click → context menu: Rename · Delete (with confirm) · Open file location
- **Footer:** count of items + total disk usage

### Save-from-chat flow

Right-click any image attachment → "Save to book…" → small modal with thumbnail + name input + Save / Cancel.

### Onboarding

First time the BookBrowser opens: one-time intro overlay (same pattern as the Customizer): *"This is your reaction book. Upload images to react with them on any server. Right-click any image in chat to save it here."* Dismissable, re-openable from a "?" icon. Dismissal stored in `~/.farder/settings.json` under `bookIntroDismissed`.

## Persistence

- `items.json` — full book index, written on every mutation.
- `files/` — image files, named `<item-id>.<ext>`. Item id is the source of truth for file existence; if `items.json` references an id whose file is missing, the item is silently skipped at load time and the user is shown a warning at the next BookBrowser open.
- `~/.farder/settings.json`:
  - `bookIntroDismissed: true` once dismissed
- Per-item `server_files` cache lives in `items.json` itself, NOT in `settings.json` (it's per-item, not per-app).

## Security summary

| Layer | Defense | Catches |
|---|---|---|
| Client upload | 2 MB cap; format allowlist | Obvious junk, oversized files |
| Server upload | Magic-byte check | Polyglot files (PE/JS pretending to be PNG) |
| Server upload | Dimension cap (4096×4096, 4M total) | Decompression bombs |
| Server upload | Frame count cap (200) | Pathological GIF/WebP |
| Server reaction add | Per-user rate limit (60/min) | Reaction spam DoS |
| Server upload | Per-user throttle (10/min) | Upload spam DoS |
| Server upload | Per-user storage quota (50 MB) | Disk exhaustion |
| Quota exceeded | UI prompt to user-pick deletions | Avoid silent data loss |
| Client render | OS webview's PNG/JPEG/GIF/WebP decoders | Format-decoder RCE (battle-tested in Chromium/WebKit) |

## Out of scope (revisit if abuse appears)

- Server-side re-encoding to a normalized form (strongest defense, expensive).
- Antivirus scanning.
- Content moderation (NSFW, etc.).
- Federation reputation (server-level blocklists).
- Perceptual hashing for known-bad content.

## Phase 2 sketch (separate spec, written after Phase 1 ships)

- **Inline `:name:` in messages:** typing `:po` autocompletes from book item names; on send, message text contains `:my-cat:` plus an attachment ref. Receiving clients render the matching attachment inline at ~22px.
- **Send as sticker:** "Send" button in BookBrowser → sends the item as a standalone image message in the current channel. Replaces the existing FavoritesPanel send behavior, which is removed.
- **Unicode emoji favoriting:** heart icon in the standard emoji picker pins a Unicode emoji to the front of the book. Stored as a Book item with `ext: "unicode"` and a special `unicode_codepoint` field instead of a file. No server upload (Unicode emoji are just text).
- **Most-used sort:** add `use_count` field to BookItem, increment on every use.
- **Drag-to-reorder:** like the theme picker, persist user-chosen item ordering.

## Success criteria

- A user can upload a PNG to their book, react with it on a message, and other connected users see the actual image (not a placeholder text) in the reaction bar.
- The same user, on a different server, can react with the same item — file is uploaded to that second server on first use, cached afterwards.
- Existing favorites are migrated on first launch with no data loss; original `~/.farder/favorites/` is preserved as `.bak`.
- Right-clicking a chat image and choosing "Save to book" downloads it into the book in one click.
- Uploading a 5 MB file is rejected with a clear error message.
- Uploading a `.exe` renamed to `.png` is rejected at the server with "unsupported image format".
- Uploading a 1×1px image that decodes to 100,000×100,000 is rejected as "image dimensions exceed limit".
- Quota limit (50MB per user per server) reached → user gets a prompt to delete items, never silent eviction.
- Reaction picker shows book items above Unicode emoji; clicking a book item produces a working reaction visible to other clients.
- Old clients connecting to the new server can still react with Unicode emoji (backwards compatibility).

## Implementation notes (non-binding, for the planner)

- `book.rs` lives parallel to `themes.rs`, both consuming the `farder_data_dir_pub()` helper from `commands.rs`.
- Reuse the existing attachment upload path (`upload_file` Tauri command + the bi-directional QUIC stream) — don't introduce a separate "book item upload" channel. The book item is just a regular file from the server's perspective.
- The `image` crate is already a transitive dep of `tauri-plugin-shell`-adjacent crates; the dimension and frame-count checks need it as a direct dependency on the server. Versioning: pin to whatever version is in the Cargo.lock if compatible.
- For computing `animated` on upload (client-side, before sending): for WebP, parse the RIFF header and look for an `ANIM` chunk; for GIF, count Image Descriptor blocks. Both are simple chunk-walk operations.
