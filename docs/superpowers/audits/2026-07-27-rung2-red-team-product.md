# Rung 2 E2EE Design — Red-Team Findings (Product / Operational Lens)

**Date:** 2026-07-27
**Target:** `docs/superpowers/specs/2026-07-27-mesh-rung2-e2ee-design.md` (draft, `0a897fd`)
**Method:** every claim checked against the shipped code on `main`/`mesh-rung2-design`, not the spec's summary of it. Severity: **BLOCKER** (design wrong as written; will break real usage) / **HIGH** (breaks a real shipped flow or promise; must change before plan) / **MEDIUM** (real degradation or gap; fix cheap now, expensive later) / **LOW** (accuracy/copy). Evidence tags: **CONFIRMED** (observed in code or by experiment) / **PLAUSIBLE** (reasoned, not executed).

---

## F1. BLOCKER — The enforcement model only covers the log path; seven shipped plaintext write paths bypass the fold entirely. CONFIRMED

The spec's containment rule is "the fold rejects a plaintext `MessagePosted` into an E2EE channel." But most messages the owner's servers carry today **never become log events**. Production paths that write plaintext straight into the `messages` table with no fold involvement:

- Legacy `SendMessage` handler — `handlers.rs:484` (`messages::insert_message`). Still the live path for every non-log server, every DM, **and several log-server sends (below)**.
- Slash-command bot replies — connection-level `RunCommand` → `insert_message_with_author_name` (`connection.rs:1223`).
- Incoming webhooks — `webhooks.rs:185`.
- Poll create (`polls.rs:326`), giveaway create (`giveaways.rs:398`), giveaway sweeper winner announcement (`giveaways.rs:289`).
- Sticker / GIF / voice-message sends — always `api.sendMessage`, never `submitEvent` (`SendStickerPicker.tsx:99`, `GifPicker.tsx:171`, `MessageInput.tsx:206`).
- **The client's own send fallback actively routes around the log:** `MessageInput.tsx:277-296` falls back to legacy `sendMessage` whenever a message has a reply, a URL-image auto-fetch, or an inline `:emoji:` attachment (`hasUncappableAttachment`). In an E2EE channel as currently wired, "message with an image URL" would silently go out **plaintext over the legacy path** — plus `api.fetchUrl` makes the server fetch and store the image plaintext.

The spec's "Server changes" adds class checks to **ingest** (SubmitEvent) only; the matrix's "unavailable in E2EE channels" verdicts (webhooks, commands, polls) name no enforcement point except "config UI refuses" — UI-only enforcement of a cryptographic promise.

**Minimal change:** one server-side choke point — a channel-class gate inside `messages::insert_message` / `insert_message_with_author_name` (reject non-`MessagePostedE2ee`-derived writes into E2EE channels), plus: `RunCommand`/webhook-create/poll/giveaway handlers return a clear "not available in encrypted channels" error; the client E2EE send branch **never falls back** (URL auto-fetch and inline-emoji resolution disabled or client-sealed in E2EE channels; sticker/GIF/voice routed through the sealed attachment path or hidden). Add this legacy-path lockdown as an explicit sub-project (see F14 — today no sub-project owns it).

## F2. HIGH — Mod delete / retention GC on log messages is resurrection-prone as shipped; the matrix's 7b "works-on-ciphertext" is false. CONFIRMED

`delete_message` hard-deletes the derived row (`messages.rs:259-287`); `reconcile_messages` runs at every server startup and **re-derives a row for any `MessagePosted` event that lacks one** (`event_ingest.rs:177-199`, invoked `main.rs:116`). So a moderator-deleted (or retention-GC'd) log-path message **comes back on restart**. This is a latent Rung-1 bug, but Rung 2 turns it product-breaking: content-blind mod delete is the **only** moderation mechanism in E2EE channels, and `MessagePostedE2ee` will get the same derive+reconcile treatment. A mod deletes reported ciphertext; the sidecar restarts (which happens constantly in this dev workflow); the message is back.

**Minimal change:** deletion must be log-visible or reconcile-visible — either a `MessageDeleted { event_ref }` log event (mirror of `AttachmentRedacted`, which got this right), or a `deleted_events` table consulted by `derive_message_row`/`reconcile_messages`. Spec should correct row 7b and fold this into sub-project 3.

## F3. HIGH — Mixed-fleet protocol breakage: the spec's only compat claim covers the wrong direction. CONFIRMED (by experiment)

Empirical test (rmp_serde 1.x, same codec as `farder-protocol/src/codec.rs`):

- old reader + struct with one added trailing field → `Err(LengthMismatch(2))`
- old reader + new enum variant → `Err(Syntax("unknown variant"))`
- old reader + old variant from new code → Ok (this is the *only* direction the spec's "rmp_serde tolerates trailing additions" claim covers).

Rung 2 cannot ship its UX without touching client-visible types: the lock icon and send-routing need an `is_e2ee` on `ChannelInfo`, derivation adds a marker to `MessageInfo` (`server.rs:116-141`), and MLS delivery needs new `ServerEvent`s. Any of those, done naively, makes an **un-updated client fail to decode frames from an upgraded server — including in plaintext channels**. This owner runs a multi-machine fleet with known stale-build lag (WebView2 cache memory; separate client-crate rebuild memory); "one machine updated, one not" is his normal state, and the failure mode is undecodable frames, not a graceful "please update."

Also unstated: what an old client shows for an E2EE channel it *can* decode the metadata of — it will render derived ciphertext rows as hex garbage in a normal-looking channel and offer a working plaintext composer (rejected only if F1's server gate exists).

**Minimal change:** a short "protocol compatibility" section with rules: never add fields to existing structs or variants old clients receive; new data rides in **new** request/response/event variants that only new clients ask for; old-client behavior in an E2EE channel is defined (server-side gate + a designed failure copy), and the owner's upgrade order (server first vs client first) is stated.

## F4. HIGH — Existing servers can't use Rung 2 at all: genesis exists only for servers claimed after Rung 1 shipped. CONFIRMED

Genesis is created exactly once, at first-owner-claim (`connection.rs:587-610`: gated on `setup_token_used || auto_claimed`). A server whose owner was established before Rung 1 has no genesis, `log_state` is `None`, and `SubmitEvent` is rejected ("server is not running the event log (no genesis yet)", `handlers.rs:1952`). Since `ChannelE2eeEnabled` and everything MLS are log events, **E2EE is structurally unavailable on every pre-Rung-1 server, forever** — and the spec never says so. Second layer of the same problem: members who joined via the legacy path are absent from `LogState.members` (`event_log_state.rs:42`), so on log servers they fail the spec's own MLS Add rule ("current, approved member per the authz fold") — the owner's longest-standing members are exactly the ones who can't get keys.

**Minimal change:** either (a) an explicit owner-triggered genesis-establishment + membership-backfill event flow (owner-signed `MemberJoined`-equivalents for existing members), scoped into sub-project 2/3, or (b) an honest locked decision: "E2EE requires a post-Rung-1 server; existing servers must be recreated" — surfaced to the owner as an open question, because it's his servers.

## F5. MEDIUM-HIGH — The "immutable at creation" guard is checked against fold state that can't see most real messages; class creation is non-atomic across two truth systems. CONFIRMED

`ChannelE2eeEnabled` is valid "only if the channel has no prior message events," tracked via `channels_with_messages` built from **log** `MessagePosted` events. Legacy messages (F1's entire list) are invisible to the fold — so a busy, months-old legacy channel folds as "no messages" and can be flipped to E2EE-class, producing plaintext history under a lock icon and breaking the immutability story the UX depends on. Separately, "chosen at creation" is actually two non-atomic operations in two systems (`CreateChannel` → DB; class event → log): between them the channel exists as plaintext-class and legacy paths can post into it.

**Minimal change:** ingest of `ChannelE2eeEnabled` additionally checks the `messages` **table** is empty for that channel (server-side, Rung 2 only — fold rule stays as specced for Rung 3 fresh-replay correctness, where legacy rows won't exist); and creation-with-class is made atomic (channel hidden or read-only until its class event is accepted, one server-side transaction from the client's perspective).

## F6. MEDIUM-HIGH — The coexistence matrix is not "every plaintext-touching feature": edits, reactions, threads, pins, and this week's widget links / active bar have no row. CONFIRMED

Claimed complete ("fate of every plaintext-touching server feature"), but verified gaps:

- **Message editing** — shipped (`EditMessage`, `handlers.rs:2020` area; used daily). No `MessageEditedE2ee` variant exists or is specced → editing your own message is silently impossible in E2EE channels. Needs a row + either a new variant or an honest "no edits in E2EE channels (delete + repost)" with UI copy.
- **Reactions** — shipped (`reactions.rs`); stored server-side as plaintext emoji keyed to the message. In a "sealed" channel the server sees *who reacted with what emoji to which ciphertext* — content-adjacent leakage the threat-model section doesn't list. Needs a row: accept the leak explicitly (with copy), or disable reactions in E2EE channels.
- **Threads** — shipped (thread channels spawned from a message, `channels.rs`). A thread off an E2EE message is created via the legacy path → **plaintext thread hanging off sealed parent**. Class inheritance is unspecified. Minimal: threads inherit the parent's class; E2EE threads deferred = thread creation refused in E2EE channels this rung.
- **Pins** — id-keyed, genuinely fine; say so in a row (pinned-message *preview* surfaces would show ciphertext — check the UI).
- **Shareable widget links + active-widgets bar** — shipped this week (`fb96446`, `1c96cf1`; `Message.tsx:40` `WIDGET_LINK_REGEX`, `ListActiveWidgets` `server.rs:482`) and absent from the matrix. They mostly *work* in E2EE channels (client-side detection on decrypted content; `GetPoll` is id-keyed) — but note: interacting with a linked widget seconds after a ciphertext message arrives gives the server a timing correlation between sealed content and a named widget. A row with "works, with stated correlation caveat" is enough.

## F7. MEDIUM — Link-embed auto-fetch leaks E2EE message URLs to the relay; matrix row 6a's "works unchanged" hides it. CONFIRMED

`useLinkEmbed` fetches automatically on render (`useLinkEmbed.ts:12-25`) via relay `ProxyLinkEmbed`. URLs **are** message content; in an E2EE channel every viewer's client auto-sends every pasted URL to the relay — the spec's own named metadata chokepoint — so "what was said" partially leaks by design. **Minimal change:** in E2EE channels, embeds are click-to-load (the "Load preview" chip already exists for data-saver, `LinkEmbed.tsx:41`); one boolean plumb, no new UI.

## F8. MEDIUM — The 16 KiB ciphertext cap is smaller than the 8000-char plaintext rule it replaces. CONFIRMED (arithmetic)

8000 chars is up to **32 KiB** of UTF-8 (CJK, emoji — this fleet's usage includes emoji-heavy widget content), plus in-band attachment keys/filenames/MIME plus MLS framing overhead. A message that passes the client's 8000-char check can exceed the server's 16 KiB cap → hard bounce with no user-comprehensible cause. **Minimal change:** cap at 40 KiB, or make the client rule byte-based (8000 chars AND ≤ ~32 KiB pre-seal) and say so in the spec.

## F9. MEDIUM — Replies over the log are an unbuilt prerequisite the spec silently assumes. CONFIRMED

`MessagePostedE2ee` specs `reply_to: Option<EventRef>` and "the server must thread replies" — but the shipped log send path **drops replies entirely**: `MessageInput.tsx:283` "TODO(mesh): replies over the log need event-hash mapping; legacy replyTo is a numeric id, so drop it for now." E2EE channels are log-only, so replies are broken in them until the event-hash↔message-id mapping lands — work no sub-project owns. **Minimal change:** name the mapping work and put it in sub-project 3 (server derive: `reply_to: EventRef` → derived-row id) + 4 (client sends event-hash).

## F10. MEDIUM — Steward-liveness and no-history assumptions are calibrated for communities this owner doesn't run. PLAUSIBLE (usage), CONFIRMED (mechanics)

The spec's "acceptable for a community product (someone is around)" assumes overlap. The owner's real servers are tiny personal communities with long all-offline gaps. Concrete first-touch UX: a friend joins in the evening → "waiting for a member to grant keys" until *someone else* opens the app → then "messages before you joined are not available" → their first experience of the flagship private channel is an empty channel they spent hours locked out of. Note also, explicitly, in the spec: **self-hosting doesn't help** — the owner's always-on sidecar server is barred from stewarding by the no-server-keys constraint, which will surprise a self-hosting owner. **Minimal change:** no protocol change; (a) recommend answering open Q2 as "prioritize member-history-share," (b) make the join UX proactively honest ("keys arrive when a member comes online"), (c) nudge the steward loop to run in any open client even when the app is backgrounded.

## F11. MEDIUM — No story for MLS log growth, compaction, or update cadence; startup replay scales with total history. PLAUSIBLE

Every KeyPackage top-up, every commit (carrying KeyPackages), and every ratchet-tree-in-Welcome (~O(members) size; ~15-30 KB at 50 members) is a **permanent** log event; the server replays the entire log at every startup (`main.rs:113` `build_log_state`), and Rung 3 replicates it to every host forever. The spec flags Welcome *size* but not accumulation. It also never schedules MLS self-Update commits — without them, PCS quietly decays for long-quiet members (the healing the design is buying); with them, 50 members × K channels × cadence is unbounded control-chatter growth. **Minimal change:** decide the self-update cadence now (e.g., on-first-send-after-N-days, not timer-driven); state that consumed KeyPackages/delivered Welcomes are prunable at checkpoint boundaries (Rung 3/4 detail, but the *claim* belongs in this spec); add per-variant ingest size caps to sub-project 3's test matrix (spec mentions caps in Risks only).

## F12. LOW — Matrix row 2 mischaracterizes bot DMs as "already E2EE-shaped." CONFIRMED

Bot alert DMs are encrypted **on the server** with server-held bot secret keys (`bots.rs:474-512`: `get_bot_secret` → `encrypt_bot_dm` server-side; plaintext exists in server memory pre-seal). "Works unchanged" is true; "already E2EE-shaped" would fail the spec's own "server never holds keys" bar. Fix the rationale: "server-managed bots are server-trusted by definition; their DMs are server-encrypted, not E2E."

## F13. LOW — Matrix row 8 degrades a preview feature that doesn't exist. CONFIRMED

Push is count-only: `NotifyPending { count }` (`farder-notify/src/push.rs:28`) — no content, no channel name ever leaves for notify. The row's "previews become generic ('New message in #channel')" invents a richer current state, and its proposed generic text would actually *add* channel-name exposure to a service that today sees nothing. Correct the row to "already content-free; nothing changes," and drop open question 6 or reframe it as pure future work.

## F14. MEDIUM — Sub-project independence is real, but sub-4/5 verification as worded collides with this environment; and no sub-project owns the legacy-path lockdown. CONFIRMED (environment), CONFIRMED (scope gap)

Sub-1/2/3 are genuinely pure-Rust/single-process testable — good. Sub-4's observation test is worded as "capture the real send path's wire bytes" — the real path is the Tauri client, which cannot run in WSL (CLAUDE.md), and sub-5's steward races / ban-rekey-window tests need ≥2 concurrent clients. Both are single-machine testable **if** the MLS/steward logic lives in the client *crate* as plain library code driven by a headless two-protocol-client harness (the `tests/e2e_server.rs` pattern) — but the spec never commissions that harness, so sub-4/5 will either ship UNVERIFIED or fall back to the owner's manual multi-machine Windows testing. Separately: F1's legacy-path lockdown and F9's reply mapping appear in **no** sub-project — the matrix's "unavailable in E2EE channels" verdicts currently have no implementing owner. **Minimal change:** add the headless two-client harness as a named deliverable of sub-4; move steward logic into harness-drivable library code by construction; add the legacy-gate work to sub-3's checklist; owner-manual verification reduced to one final Windows smoke.

## F15. MEDIUM-LOW — Six serial protocol-touching drops maximize the owner's heaviest known workflow. CONFIRMED (workflow)

Sub-projects 2–6 each touch `farder-crypto`/`farder-protocol`, and each such change triggers the full known dance: workspace build + **separate client-crate rebuild** (project memory: `cargo build --workspace` does not build the Tauri crate) + WebView2 hard-reload on the owner's machine. **Minimal change:** land *all* new `EventPayload`/protocol variants and fetch surfaces in sub-2/3 (dormant until used), so sub-4/5/6 are behavior-only inside already-shipped types — one protocol upheaval instead of five.

## F16. LOW — Mis-post risk between classes: a lock icon alone is thin. PLAUSIBLE

Identical composers in adjacent channels; the failure that matters is typing something sensitive into plaintext #general *believing* it's the private channel. **Minimal change (client-only, theme-var compliant):** the E2EE composer gets a distinct affordance (placeholder "Encrypted message…" + a border color from `var(--xp-…)` in every theme per CLAUDE.md), and the *plaintext* channel creation notice ("bots and server can read this channel") does the other half.

---

## Summary verdict

The crypto-side shape (OpenMLS, log-as-DS, fold-as-authority, no escrow) is sound and honestly argued. What the spec gets wrong is **the codebase it thinks it's landing in**: enforcement is specced against the log path while the shipped product writes most content through seven legacy paths (F1); its moderation claim is contradicted by shipped resurrection behavior (F2); its compat claim covers the wrong serde direction while the owner runs a perpetually mixed fleet (F3); and its fresh-start story silently excludes every server and long-standing member that exists today (F4). None of these require redesigning the MLS mapping — they require one server-side write choke point, one tombstone mechanism, one protocol-versioning rule, and one migration decision, all cheap now and expensive after sub-project 3.
