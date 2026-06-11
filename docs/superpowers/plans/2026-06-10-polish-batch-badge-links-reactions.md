# Polish Batch: Join Badge + Short Links + Reaction Migration — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Three independent polish items: (1) the Join-confirm dialog discloses relayed vs direct, (2) invites for default-relay servers use a compact `farder://relayd/...` form, (3) the reactions table allows multiple different custom-image reactions per user per message.

**Architecture:** (A) frontend-only badge driven by a `relayed` prop computed in App.tsx. (B) a new `relayd` deep-link prefix expanded from `default_relay()` at parse time; generation only when the server's relay matches the compiled default. (C) SQLite table rebuild moving `file_id` (0-sentinel) into the reactions PK, with an idempotent migration and an Option<u64>↔0 mapping at the storage boundary.

**Tech Stack:** React/TS, Rust (Tauri client + farder-server), SQLite (rusqlite).

**Spec:** `docs/superpowers/specs/2026-06-10-polish-batch-badge-links-reactions-design.md`

---

## Context for the implementer

- Gates: `cd client/src-tauri && cargo build`, `cd client && npx tsc --noEmit`, `cargo test -p farder-server`, and for B `cargo test -p farder-client relay` (bin-crate tests live in-file). GUI is NOT runnable here (WSL) — A is UNVERIFIED until the user's Windows run; B and C are headlessly tested.
- CLAUDE.md styling rule: any NEW className must be styled in ALL THREE `client/src/themes/*/theme.css` files using `var(--xp-…)` colors. Reuse `learn-more-toggle`/`learn-more-body` (already styled in all themes by the relay-choice work).
- `default_relay()` (in `client/src-tauri/src/default_relay.rs`) returns `Option<(SocketAddr, Vec<u8>)>` — the compiled default relay addr + 32-byte cert fingerprint.

---

## File structure

- A: `client/src/components/JoinConfirmModal.tsx`, `client/src/App.tsx`, `client/src/themes/*/theme.css` (x3).
- B: `client/src-tauri/src/connection.rs` (parse + new build helper + tests), `client/src-tauri/src/commands.rs` (`create_invite`), `client/src/lib/invite.ts`.
- C: `crates/farder-server/src/db.rs` (migration), `crates/farder-server/src/reactions.rs` (0-sentinel boundary + tests).
- Docs: `docs/modules/client-relay.md` (compact link form), `docs/modules/tauri-commands.md` if signatures change (none expected).

---

## Task 1 (C first — most mechanical): reactions uniqueness migration

**Files:** Modify `crates/farder-server/src/db.rs` (migration section, after the existing `has_file_id` ALTER block ~line 195) and `crates/farder-server/src/reactions.rs`.

- [ ] **Step 1: Write the failing tests** — in `reactions.rs`'s `#[cfg(test)] mod tests` add:

```rust
    #[test]
    fn same_user_two_different_custom_images_both_insert() {
        let conn = test_db();
        let (msg_id, user1, _user2) = seed(&conn);
        assert!(add_reaction(&conn, msg_id, &user1, ":custom:", Some(11)).unwrap());
        assert!(add_reaction(&conn, msg_id, &user1, ":custom:", Some(22)).unwrap(), "a second DIFFERENT custom image must insert");
        // Same image again is still deduped.
        assert!(!add_reaction(&conn, msg_id, &user1, ":custom:", Some(11)).unwrap());
        let reactions = list_reactions(&conn, msg_id, &user1).unwrap();
        let customs: Vec<_> = reactions.iter().filter(|r| r.emoji == ":custom:").collect();
        assert_eq!(customs.len(), 2, "two distinct custom reaction groups");
    }

    #[test]
    fn standard_emoji_dedupe_still_holds() {
        let conn = test_db();
        let (msg_id, user1, _user2) = seed(&conn);
        assert!(add_reaction(&conn, msg_id, &user1, "\u{1F44D}", None).unwrap());
        assert!(!add_reaction(&conn, msg_id, &user1, "\u{1F44D}", None).unwrap(), "duplicate standard emoji must be ignored");
    }
```

(Adapt `test_db()`/`seed(...)`/`list_reactions` names to the helpers ALREADY in that test module — read it first; there are existing tests like the duplicate-🔥 one to model on. If `add_reaction` currently returns `Result<bool>` — it does, from the phantom-broadcast fix — assert on the bool as above.)

- [ ] **Step 2: Run to verify failure** — `cargo test -p farder-server reactions 2>&1 | tail -8`. Expected: `same_user_two_different_custom_images_both_insert` FAILS (the second image is ignored by the old PK).

- [ ] **Step 3: Add the rebuild migration** — in `db.rs`, AFTER the existing `has_file_id` ALTER block, add:

```rust
    // Reactions: move file_id into the uniqueness key (Reaction Book phase 2).
    // The original PK (message_id, user_key, emoji) ignored file_id, so a user
    // could hold only ONE ":custom:" reaction per message. SQLite cannot alter
    // a PK -> rebuild. file_id uses a 0 sentinel (= standard emoji; real file
    // ids start at 1) because NULLs in a composite PK are mutually distinct
    // (no dedupe). Idempotent: skip when file_id is already NOT NULL.
    let file_id_not_null: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(reactions)")?;
        let rows: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get::<_, String>(1)?, row.get::<_, i64>(3)?)))?
            .filter_map(|r| r.ok())
            .collect();
        rows.iter().any(|(name, notnull)| name == "file_id" && *notnull == 1)
    };
    if !file_id_not_null {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE reactions_new (
                 message_id INTEGER NOT NULL,
                 user_key BLOB NOT NULL,
                 emoji TEXT NOT NULL,
                 file_id INTEGER NOT NULL DEFAULT 0 REFERENCES files(id),
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY (message_id, user_key, emoji, file_id),
                 FOREIGN KEY (message_id) REFERENCES messages(id)
             );
             INSERT OR IGNORE INTO reactions_new (message_id, user_key, emoji, file_id, created_at)
                 SELECT message_id, user_key, emoji, COALESCE(file_id, 0), created_at FROM reactions;
             DROP TABLE reactions;
             ALTER TABLE reactions_new RENAME TO reactions;
             CREATE INDEX IF NOT EXISTS idx_reactions_message ON reactions(message_id);
             COMMIT;",
        )?;
    }
```

NOTE: the `file_id INTEGER ... REFERENCES files(id)` FK with a 0 sentinel: row 0 never exists in `files`. SQLite only enforces FKs when `PRAGMA foreign_keys=ON` — check what the server sets (grep `foreign_keys` in db.rs). If it is ON, DROP the `REFERENCES files(id)` clause from the new table (the old ALTER-added column carried it, but the sentinel makes it unenforceable) and note that in the migration comment. Verify with the tests either way.

- [ ] **Step 4: Map Option<u64> <-> 0 at the storage boundary** — in `reactions.rs`, update every SQL touching `file_id`:
  - `add_reaction`: replace the `((file_id IS NULL AND ?3 IS NULL) OR file_id = ?3)` style predicates with `file_id = ?3` binding `file_id.unwrap_or(0) as i64`; the INSERT binds `file_id.unwrap_or(0) as i64` instead of `file_id.map(|v| v as i64)`.
  - `remove_reaction` and any list/aggregate queries: same mapping on write/compare; on READ, convert `0 -> None`, `n -> Some(n)` wherever rows are turned into the `Option<u64>` API shape (find the row-mapping closures).
  - The public fn signatures keep `Option<u64>` — callers and the wire protocol are unchanged.

- [ ] **Step 5: Run the tests** — `cargo test -p farder-server reactions 2>&1 | tail -6` (all PASS, incl. the existing duplicate/limit tests) then the full crate: `cargo test -p farder-server 2>&1 | grep "test result"` (all green; the migration runs on every fresh test DB so existing tests exercise the new schema).

- [ ] **Step 6: Commit** — `git add crates/farder-server && git commit -m "Server: reactions uniqueness includes file_id (table-rebuild migration)"`.

---

## Task 2 (B): compact default-relay invite links

**Files:** Modify `client/src-tauri/src/connection.rs`, `client/src-tauri/src/commands.rs` (`create_invite`), `client/src/lib/invite.ts`.

- [ ] **Step 1: Write the failing Rust tests** — in `connection.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn compact_relayd_link_expands_from_the_default_relay() {
        let (def_addr, def_fp) = crate::default_relay::default_relay().expect("default relay configured");
        let sid = vec![7u8; 32];
        let link = build_compact_relay_link(&sid, "tok123");
        assert_eq!(link, format!("farder://relayd/{}/tok123", hex::encode(&sid)));
        let parsed = parse_relay_target(&link).expect("relayd link must parse");
        assert_eq!(parsed.relay_addr, def_addr);
        assert_eq!(parsed.cert_fp, def_fp);
        assert_eq!(parsed.server_id, sid);
        assert_eq!(parsed.invite_token, "tok123");
    }

    #[test]
    fn relayd_link_with_empty_token_parses() {
        let link = build_compact_relay_link(&vec![7u8; 32], "");
        let parsed = parse_relay_target(&link).expect("empty-token relayd link must parse");
        assert!(parsed.invite_token.is_empty());
    }

    #[test]
    fn full_form_relay_links_still_parse() {
        let target = RelayTarget {
            relay_addr: "1.2.3.4:4433".parse().unwrap(),
            server_id: vec![1u8; 32],
            cert_fp: vec![2u8; 32],
            invite_token: "abc".into(),
        };
        let link = build_relay_link(&target, "abc");
        assert!(parse_relay_target(&link).is_some(), "backward compat: full form must keep parsing");
    }
```

- [ ] **Step 2: Run to verify failure** — `cd client/src-tauri && cargo test relayd 2>&1 | tail -6`. Expected: FAIL to compile (`build_compact_relay_link` missing).

- [ ] **Step 3: Implement parse + build** — in `connection.rs`:

Add to `parse_relay_target`, BEFORE the existing `strip_prefix("farder://relay/")` handling:

```rust
    // Compact default-relay form: farder://relayd/<server_id_hex>/<token>
    // (token may be empty). Expanded from the compiled-in default relay; a
    // build with no default relay cannot resolve these (returns None).
    if let Some(rest) = s.strip_prefix("farder://relayd/") {
        let (relay_addr, cert_fp) = crate::default_relay::default_relay()?;
        let mut parts = rest.splitn(2, '/');
        let server_id = hex::decode(parts.next()?).ok()?;
        if server_id.is_empty() {
            return None;
        }
        let invite_token = parts.next().unwrap_or("").to_string();
        return Some(RelayTarget { relay_addr, server_id, cert_fp, invite_token });
    }
```

(NOTE: `parse_relay_target` is a plain fn returning Option — the `?` on `default_relay()` and `parts.next()` works inside it. Keep the existing full-form logic untouched below this block.)

Add next to `build_relay_link`:

```rust
/// Build the COMPACT deep link for a server on the compiled-in default relay:
/// `farder://relayd/<server_id_hex>/<token>`. Only valid when the server's
/// relay is the default (the parser re-expands from default_relay()).
pub fn build_compact_relay_link(server_id: &[u8], code: &str) -> String {
    format!("farder://relayd/{}/{}", hex::encode(server_id), code)
}
```

- [ ] **Step 4: Generate compact links in `create_invite`** — in `commands.rs` (~line 1614), change the relay branch:

```rust
                if let Some(target) = crate::connection::parse_relay_target(&server_id) {
                    // Servers on the compiled-in default relay get the compact
                    // form (drops the embedded addr + 64-char fingerprint).
                    let on_default = crate::default_relay::default_relay()
                        .map(|(addr, fp)| addr == target.relay_addr && fp == target.cert_fp)
                        .unwrap_or(false);
                    let deep_link = if on_default {
                        crate::connection::build_compact_relay_link(&target.server_id, &code)
                    } else {
                        crate::connection::build_relay_link(&target, &code)
                    };
                    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(deep_link.as_bytes());
                    (encoded, deep_link)
                } else {
```

(The `else` direct branch and the `farder.gg/join` wrapping stay exactly as-is.)

- [ ] **Step 5: Frontend parser accepts `relayd`** — in `client/src/lib/invite.ts`, change the relay regex:

```ts
  // Relay deep link (full or compact default-relay form): return the whole
  // URL as the address (token embedded; the Rust parser expands relayd/).
  if (/^farder:\/\/relayd?\//i.test(trimmed)) {
    return { address: trimmed };
  }
```

(One character: `relay` -> `relayd?`. The comment update matters — the OLD regex would have mis-routed `relayd` links into the direct-link branch.)

- [ ] **Step 6: Run tests + gates** — `cargo test -p farder-client relay 2>&1 | tail -8` (new + existing relay link tests PASS), `npx tsc --noEmit` (clean), `cargo build` (clean).

- [ ] **Step 7: Commit** — `git add client/src-tauri/src client/src/lib/invite.ts && git commit -m "Client: compact farder://relayd invite links for the default relay"`.

---

## Task 3 (A): join-side relay badge

**Files:** Modify `client/src/components/JoinConfirmModal.tsx`, `client/src/App.tsx`, all three `client/src/themes/*/theme.css`.

- [ ] **Step 1: Pass `relayed` from App.tsx** — `App.tsx` already imports `parseInviteLink` and renders the modal (~line 130). Compute and pass:

```tsx
  const confirmModal = joinConfirm ? (
    <JoinConfirmModal
      relayed={/^farder:\/\/relayd?\//i.test(parseInviteLink(joinConfirm).address ?? "")}
      onConfirm={() => { const u = joinConfirm; setJoinConfirm(null); void joinFromInvite(u); }}
      onCancel={() => setJoinConfirm(null)}
    />
  ) : null;
```

(Verify `joinConfirm` is the raw invite string — read its useState; if it's already a parsed object, adapt the expression to its `address` field.)

- [ ] **Step 2: Render the disclosure in the modal** — rewrite `JoinConfirmModal.tsx`:

```tsx
import { useState } from "react";

export default function JoinConfirmModal({
  relayed,
  onConfirm,
  onCancel,
}: {
  relayed: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const [showInfo, setShowInfo] = useState(false);
  return (
    <div className="modal-overlay" onClick={onCancel}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>Join server</span>
          <button className="modal-close" onClick={onCancel}>X</button>
        </div>
        <div className="modal-body">
          <p>You&apos;ve been invited to a Farder server. Join it?</p>
          <div className={`join-relay-note ${relayed ? "relayed" : "direct"}`}>
            <span className="join-relay-badge">{relayed ? "RELAYED" : "DIRECT"}</span>
            <span>
              {relayed
                ? "This server uses a relay — your IP address stays hidden from the host."
                : "Direct server — the host can see your IP address."}
            </span>
          </div>
          <button type="button" className="learn-more-toggle" onClick={() => setShowInfo(!showInfo)}>
            {showInfo ? "Hide details" : "Learn more"}
          </button>
          {showInfo && (
            <div className="learn-more-body">
              <p>A relay is a neutral middle server. Connecting through it means the server&apos;s host never learns your IP address (and you never learn theirs).</p>
              <p>Your direct messages and voice are end-to-end encrypted either way. Community channel messages are readable by the server host &mdash; and, today, by the relay operator on relayed servers; hardening that is on the roadmap.</p>
            </div>
          )}
          <div className="connect-actions">
            <button className="xp-button" onClick={onConfirm}>Join</button>
            <button className="xp-button" onClick={onCancel}>Cancel</button>
          </div>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Style the new classes in ALL THREE themes** — append to each of `client/src/themes/{xp-luna-blue,discord-dark,hello-kitty}/theme.css` (next to the existing `.learn-more-*` rules added by the relay-choice work), colors via theme vars only:

```css
/* Join-confirm relay disclosure */
.join-relay-note {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 8px 0;
  padding: 6px 8px;
  border: 1px solid var(--xp-border);
  border-radius: 4px;
  background: var(--xp-panel-bg);
  font-size: 11px;
  color: var(--xp-text-normal);
}
.join-relay-badge {
  font-weight: bold;
  font-size: 9px;
  letter-spacing: 0.5px;
  padding: 2px 6px;
  border-radius: 3px;
  flex-shrink: 0;
}
.join-relay-note.relayed .join-relay-badge {
  background: var(--xp-blue);
  color: var(--xp-white);
}
.join-relay-note.direct .join-relay-badge {
  background: var(--xp-sidebar-dark);
  color: var(--xp-text-normal);
}
```

- [ ] **Step 4: Gates** — `npx tsc --noEmit` (clean); confirm styling coverage: `grep -l "join-relay-note" client/src/themes/*/theme.css` lists all three.

- [ ] **Step 5: Commit** — `git add client/src && git commit -m "Client UI: Join-confirm dialog discloses relayed vs direct"`.

---

## Task 4: docs + final gates

- [ ] **Step 1: Docs** — `docs/modules/client-relay.md`: add the compact `farder://relayd/<sid>/<token>` form to the connection-info/invite sections (generated only for default-relay servers; expanded from `default_relay()`; full form still parses; UNVERIFIED visually until a Windows run). One line on the join-side badge in the Relay UX section.
- [ ] **Step 2: Final gates** — `cargo test -p farder-server` (incl. migration tests), `cargo test -p farder-client relay`, `cargo build` (client), `npx tsc --noEmit`, themes grep for `join-relay-note` (3 hits).
- [ ] **Step 3: Commit** — `git add docs && git commit -m "Docs: compact relayd links + join-side relay disclosure"`.

---

## Final verification

- [ ] All gates green (server tests + client relay tests + build + tsc + themes coverage).
- [ ] UNVERIFIED by design (user's Windows run): the badge's look in each theme; a real invite producing a visibly shorter link; multi-custom-reactions in the live UI. The link logic and migration are headlessly proven.

After all tasks: use **superpowers:finishing-a-development-branch**. Commit messages end with the project's standard Co-Authored-By trailer.
