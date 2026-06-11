# Polish Batch: Join Badge + Short Invite Links + Reaction Migration — Design Spec

**Date:** 2026-06-10
**Status:** Approved (design); ready to plan
**Scope:** Three independent small tasks on one branch. (A) join-side relay
disclosure, (B) compact invite links via the default relay, (C) reactions
uniqueness migration.

## A. Join-side relay badge (frontend only)

The deep-link `JoinConfirmModal` ("Join server?") discloses the privacy posture
of the server being joined — the deferred join-side half of the owner-decided
relay-UX requirement (layered + learn-more).

- **Detection:** `App.tsx` holds the pending parsed invite; it passes a new
  `relayed: boolean` prop to `JoinConfirmModal`, computed as
  `address.startsWith("farder://relay")` — which matches both the full relay
  form (`farder://relay/...`) and the new compact form (`farder://relayd/...`).
- **Display:** under the "You've been invited..." line:
  - relayed: a shield badge + "This server uses a relay — your IP address stays
    hidden from the host."
  - direct: a warning badge + "Direct server — the host can see your IP
    address."
  - a "Learn more" toggle expanding the honest two-liner (relay = neutral
    middleman that hides IPs; today's relay can read community content but
    never DMs/voice), matching the create-side copy's spirit.
- **Styling:** new classes (e.g. `.join-relay-note`, `.join-relay-badge`,
  reuse `learn-more-toggle`/`learn-more-body` where possible) must be styled in
  ALL THREE theme files using `var(--xp-…)` colors (CLAUDE.md rule).
- UNVERIFIED at runtime until the user's Windows pull (GUI).

## B. Shorter invite links (client; default relay baked in)

Invites for servers on the **default relay** drop the embedded relay address +
64-char cert fingerprint, since both are compiled into the client
(`default_relay()`).

- **New compact deep-link form:** `farder://relayd/<server_id_hex>/<token>`
  (`relayd` = relay-default). Distinct prefix — cannot collide with the
  existing `farder://relay/<addr>/<sid>/<fp>/<token>` parser.
- **Expansion:** `parse_relay_target` (connection.rs) gains a `relayd` branch:
  if the input starts with `farder://relayd/`, split into 2 segments
  (server_id hex, token — token may be empty for the owner form), look up
  `default_relay()`; `None` or malformed → parse failure (None). Returns a
  normal `RelayTarget` with the default's addr + fp.
- **Generation:** `create_invite` (commands.rs), in its existing relay branch:
  if the server's parsed `RelayTarget` has `relay_addr` AND `cert_fp` exactly
  equal to `default_relay()`, build `farder://relayd/<sid>/<code>`; otherwise
  keep the full form (self-host relays keep working unchanged). The deep link
  is then base64url-wrapped into `https://farder.gg/join/<...>` exactly as
  today (website/js/invite.js unchanged — it only opens decoded `farder://`
  links).
- **Frontend parser:** `client/src/lib/invite.ts` treats a
  `farder://relayd/...` link like a relay link — returned whole as `address`
  (connect_server's Rust parser expands it).
- **Backward compatibility:** the full form parses forever; nothing removed.
  A client built WITHOUT a default relay cannot expand `relayd` links (parse
  fails cleanly) — acceptable: all shipped builds carry the default.
- **Tests (headless):** round-trips — build compact → parse → expanded target
  matches the default relay; default-relay server produces a compact link;
  self-host target produces the full form; empty-token owner form parses;
  full-form links still parse.

## C. Reactions uniqueness migration (server only)

Fix: one user can currently have only ONE `:custom:` book-image reaction per
message, because the `reactions` PK `(message_id, user_key, emoji)` lacks
`file_id` (custom reactions all share emoji `":custom:"`).

- **New schema:** `file_id INTEGER NOT NULL DEFAULT 0` (0 = standard emoji
  sentinel; real file ids start at 1), PK
  `(message_id, user_key, emoji, file_id)`. The 0-sentinel avoids SQLite's
  composite-PK NULL trap (NULLs are mutually distinct → no dedupe).
- **Migration (db.rs):** SQLite cannot alter a PK → table rebuild:
  `CREATE TABLE reactions_new (...)` → `INSERT INTO reactions_new SELECT
  message_id, user_key, emoji, COALESCE(file_id, 0), created_at FROM reactions`
  → `DROP TABLE reactions` → `ALTER TABLE reactions_new RENAME TO reactions` →
  recreate `idx_reactions_message`. **Idempotent:** detect via
  `PRAGMA table_info(reactions)` whether `file_id` is `NOT NULL` (notnull=1);
  if so the migration already ran — skip. Runs inside the existing migration
  section.
- **Storage boundary (reactions.rs):** map `Option<u64>` ↔ the 0 sentinel at
  the SQL boundary (`file_id.unwrap_or(0)` on write; `0 → None` on read);
  callers and the wire protocol keep `Option<u64>` semantics unchanged. The
  `IS NULL`-style match arms in queries become `= ?` with the mapped value.
  `add_reaction`'s INSERT OR IGNORE dedupe now holds per
  (message, user, emoji, file). The existing "max 20 unique reaction groups"
  check keeps using the (emoji, file_id) pair.
- **Tests (headless):** seed an old-schema DB (PK without file_id, NULL
  file_ids) with rows → run migration → rows intact (counts + values);
  re-running the migration is a no-op; same-user same-image duplicate still
  ignored; same-user TWO DIFFERENT custom images now both insert; standard
  emoji dedupe unchanged.

## Out of scope

- Removing the legacy favorites backend (separate cleanup).
- Any change to website/js/invite.js or the farder.gg page.
- The two-client voice-over-relay verification (no code; a test session).
