# Widget UX v2 — custom durations, shareable links, active-widgets bar — design

**Date:** 2026-07-27
**Status:** design (awaiting owner review)
**Context:** owner-requested follow-on after runtime-verifying the shipped interactive widgets ([[2026-07-27-poll-command-design]] `/poll` + [[2026-07-27-giveaway-command-design]] `/giveaway`). Three quality-of-life features on top of the shipped substrate: (1) **custom durations** in both builder modals (client-only), (2) **shareable widget links** — a copy-link control on every widget card plus rendering `farder://widget/...` links in message text as live, interactive widget cards (no new server surface), (3) an **active-widgets bar** — a chip strip under the channel header listing that channel's open widgets, each chip expanding to the full interactive widget (one new read request: `ListActiveWidgets`).

## Problem

Runtime verification surfaced three gaps:

1. The builder modals offer seven fixed durations (30m…7d) while the server accepts anything from **1m to 30d** (`polls.rs:14-15`, `giveaways.rs:16-19`, token regex `^(\d{1,4})(m|h|d)$`). "45 minutes" or "2 weeks" requires abandoning the builder and hand-typing the pipe syntax.
2. There is no way to *point at* a running poll or giveaway from elsewhere — another channel, a DM ("go vote in #general"). Invite links already do exactly this for servers (detection → embedded card → action); widgets have nothing.
3. A widget scrolls out of view minutes after posting. There is no "what's live in this channel right now" affordance; members must scroll back to find the poll they meant to vote on.

## What already exists (reused)

- **Server widget substrate (shipped):** `polls.rs` / `giveaways.rs` modules with `PollInfo` / `GiveawayInfo` (protocol `server.rs:245` / `:265` — both carry `channel_id`), `GetPoll` / `GetGiveaway` read requests, `PollUpdated` / `GiveawayUpdated` events to `EventTarget::Subscribers(channel_id)`, the shared `widgets::spawn_widget_sweeper`, the shared `widget_limiter` (10/10s).
- **The opaque-visibility helper:** `handlers::widget_channel_visible(conn, member, channel_id, is_owner)` (handlers.rs:367) — DM → `is_dm_participant`, else `resolve_member_perms_pub` + `VIEW_CHANNEL`, channel-gone → `false`. Every shipped widget handler funnels visibility failures into the same opaque "poll/giveaway not found" error. This spec reuses it verbatim.
- **Default-deny membership gate:** `request_requires_membership` (handlers.rs:393) is an allow-list of exactly four bootstrap requests; **any new variant is membership-gated automatically** by not being listed.
- **Invite-link mechanism (the template for widget links):** `INVITE_REGEX` / `INVITE_SPLIT_REGEX` / `isInviteLink()` (Message.tsx:31/:100/:102), the invite-embeds IIFE (Message.tsx:529-546, dedupe + cap 3 + `<InviteEmbed>`), the inline `.invite-link-pill` replacement in `renderContent()` (Message.tsx:135-157), and the clipboard idiom `copyInviteLink()` = `navigator.clipboard?.writeText(link)` + `toast.success(...)` (Message.tsx:106-111; same `navigator.clipboard.writeText` idiom in InviteDialog.tsx:34-43 — **no Tauri clipboard plugin anywhere**, and this spec adds none).
- **Widget components + state:** `PollWidget.tsx` / `GiveawayWidget.tsx` (props `{serverId, pollId|giveawayId, onUnavailable?}`), per-server slices `PerServerState.polls/giveaways` (ServerContext.tsx:27-30), reducer cases `POLL_UPDATED`/`POLL_STATE`/`POLL_MY_VOTE` + giveaway twins (ServerContext.tsx:400-431), mount-fetch state recovery via `api.getPoll`/`api.getGiveaway` behind a `fetchedRef` guard (PollWidget.tsx:50-59, GiveawayWidget.tsx:58-71).
- **Builder modals:** `PollBuilderModal.tsx` (`DURATIONS` :12-20 incl. "No end time"), `GiveawayBuilderModal.tsx` (`DURATIONS` :5-12, required, default "1h"); both submit through `api.runCommand` — the server parse is the single source of truth.
- **Header slot:** ChatPanel.tsx — `.channel-header` closes at :149, `.message-list` opens at :150; a bar slots between them as a sibling inside `.chat-panel`.
- **Click-anchored overlays:** the `useClickAnchoredPosition` hook (project memory: handles the 125% display-scale gotcha) for the chip dropdown.
- **Subscription semantics (drives the refetch discipline):** `Subscribe` is replace-all per client (connection.rs:877-906); `broadcast_event` sends `Subscribers(channel_id)` events **only** to clients currently subscribed to that channel (connection.rs:1372-1425). The client subscribes to current channel + DM panel + thread (AppShell.tsx:66-80). Therefore a widget card rendered in channel B for a widget living in channel A receives **no** live `PollUpdated`/`GiveawayUpdated` events.

## Goals

1. Both builders can express any server-legal duration (1m–30d) via a "Custom…" entry — presets stay; poll keeps "No end time"; giveaway still requires a duration. Client-only; zero server changes.
2. Every widget card has a copy-link control producing `farder://widget/poll/<channel_id>/<poll_id>` or `farder://widget/giveaway/<channel_id>/<giveaway_id>`; pasting such a link into any message renders the **same interactive widget component** below the text, live and votable/enterable from wherever it's viewed — with correct freshness even outside the origin channel.
3. A chip strip under the channel header shows that channel's open widgets (icon + truncated question/prize + time-left); clicking a chip opens an anchored dropdown containing the full interactive widget; chips appear/disappear live.
4. No visibility leak: a widget link or channel id must never reveal the existence or content of a channel the viewer cannot see.

## Non-goals

- **Cross-SERVER widget links** — a link is only meaningful inside the server whose messages contain it; there is no global widget namespace.
- **Pinning / manual reordering** of chips in the active bar (server order: oldest-first, that's it).
- **Unread/attention badges** on chips (no "new votes since you looked" tracking).
- **Server-side link unfurl / OpenGraph-style preview** for widget links — the client renders from its own state via existing reads.
- **Custom durations beyond the server's 1m–30d bounds** — no server parse changes; the builder clamps client-side to what `parse_poll_args` / `parse_giveaway_duration` already accept.
- **Active bar in ThreadPanel** — threads early-return before the ChatPanel header (ChatPanel.tsx:105); v1 scopes the bar to the main channel view (which includes DMs viewed in ChatPanel).

## Design

### Feature 1 — Custom durations (client-only)

**`PollBuilderModal.tsx`:** `DURATIONS` gains a final entry `{ label: "Custom…", value: "custom" }` (after "7d", before nothing). **`GiveawayBuilderModal.tsx`:** same entry appended; default stays `"1h"`.

When the duration `<select>` value is `"custom"`, a row appears below it (inline flex layout — CLAUDE.md permits inline styles for layout):

- `<input type="number" min={1} max={9999} step={1}>` with class `.connect-input` (inline `width: 80`), amount `n`.
- `<select>` with class `.connect-input`: `minutes` / `hours` / `days` → unit char `m` / `h` / `d`.

**Token construction:** `${n}${unit}` — e.g. `45m`, `36h`, `14d`. This is exactly the shape the server's duration regex `^(\d{1,4})(m|h|d)$` accepts (both `polls.rs` and `giveaways.rs`), so the max input of 9999 mirrors the 4-digit cap.

**Client-side validation (mirrors server bounds, never replaces them):** `n` must be a positive integer 1–9999, and computed seconds (`m`×60 / `h`×3600 / `d`×86400) must lie in **[60, 2 592 000]** (1m–30d, `MIN_DURATION_SECS`/`MAX_DURATION_SECS`). Violation → inline `.error-text` line "Duration must be between 1 minute and 30 days" and submit is blocked (same pattern as the modals' existing validation). Legal-but-large combos work naturally (9999m ≈ 6.9d passes; 999h fails the 30d bound with the clear error rather than a server round-trip).

**Unchanged:** poll's `""` = "No end time" option; the poll builder's trailing-duration-token collision guard (PollBuilderModal.tsx:70); args assembly (`q | opt | opt [| token]` and `${token} ${prize}`); the server parsers; `MessageInput`. No new CSS classes (reuses `.connect-input` / `.connect-label` / `.error-text`; layout is inline flex).

### Feature 2 — Shareable widget links

**Link format (exact):**

```
farder://widget/poll/<channel_id>/<poll_id>
farder://widget/giveaway/<channel_id>/<giveaway_id>
```

Both ids are decimal integers as the server reports them (`PollInfo.channel_id: u64`, `id: i64`). The `channel_id` in the link is **client-side display/consistency data only — never trusted, never sent to the server**; the server request is keyed by widget id alone, exactly as today.

**Copy control:** a small icon button (🔗, `title="Copy widget link"`) on each widget card — PollWidget footer row, GiveawayWidget meta row — class `.widget-copy-link`. Click builds the link from the info already in the slice (`farder://widget/poll/${poll.channel_id}/${poll.id}`) and runs the app's established clipboard idiom: `navigator.clipboard?.writeText(link)` + `toast.success("Widget link copied")` — a direct copy of `copyInviteLink` (Message.tsx:106-111). No Tauri plugin, no new permission.

**Detection (new, `client/src/components/Message.tsx`, mirroring the invite mechanism):**

```ts
const WIDGET_LINK_REGEX = /farder:\/\/widget\/(poll|giveaway)\/(\d+)\/(\d+)/gi;
```

with `isWidgetLink(s)` and a capturing-split variant, mirroring `INVITE_REGEX` / `INVITE_SPLIT_REGEX` / `isInviteLink` (Message.tsx:31/:100/:102). `parseWidgetLink(link)` returns `{ kind: "poll" | "giveaway", channelId: number, widgetId: number } | null` (null when either id fails `Number.isSafeInteger` — treated as malformed).

**Collision with invite detection (must-fix, called out explicitly):** `INVITE_REGEX`'s second alternative `farder:\/\/[^\s]+` **already matches** `farder://widget/...`, so without a guard a widget link would render as a bogus invite pill and `parseInviteLink` would misparse it down its direct `addr/code` branch into a garbage join card. Fix: `isInviteLink()` returns `false` for strings matching the `farder://widget/` prefix; the invite-embeds IIFE (Message.tsx:529) filters widget links out of its `match(INVITE_REGEX)` results before `parseInviteLink`; and in `renderContent()`'s split map, the widget check runs **first** — a widget-link token renders as a `.widget-link-pill` (📊 "Poll link" / 🎉 "Giveaway link"; click = copy the link, same behavior as the invite pill).

**Linked card render:** alongside the invite-embeds IIFE, a sibling block runs `message.content.match(WIDGET_LINK_REGEX)`, parses each, **dedupes by (kind, channelId, widgetId), caps at 3** (same discipline as invite embeds), and renders:

```tsx
<div className="linked-widget-embeds">
  {links.map(l => <LinkedWidgetCard key={...} serverId={serverId} link={l}
      messageChannelId={message.channel_id} />)}
</div>
```

**`LinkedWidgetCard.tsx`** (new component):

1. On mount, fetches via the existing reads: `api.getPoll(serverId, widgetId)` / `api.getGiveaway(...)` → dispatch `POLL_STATE` / `GIVEAWAY_STATE` (the shipped state-recovery path, same per-server slice — no new state shape).
2. **Consistency check:** the fetched info's `channel_id` must equal the link's `channelId`. Mismatch (stale/forged/cross-server-pasted link whose id happens to exist here) → render the unavailable card. This, plus visibility, is the cross-server-collision mitigation: a link pasted into a different server resolves against *that* server's ids and will either 404 or fail the channel match.
3. **Fetch error → compact unavailable card** `.linked-widget-unavailable`: "Poll not available" / "Giveaway not available" (muted, card-family styling). Because `GetPoll`/`GetGiveaway` return the opaque "poll/giveaway not found" for *both* nonexistent ids and channels the viewer can't see (handlers.rs:2306-2318 idiom via `widget_channel_visible`), the card is identical in every failure case — the link **cannot** function as an existence or content oracle.
4. On success → renders the **same** `<PollWidget>` / `<GiveawayWidget>` component, fully interactive (vote/enter/close/etc. all work — the interaction handlers only care about the widget id + the caller's own visibility).

**No new server surface for this feature — stated explicitly:** `GetPoll`/`GetGiveaway` already exist, already gate visibility through `widget_channel_visible`, and already answer opaquely. Feature 2 is entirely client-side.

**Live-update caveat and refetch discipline (from the subscription semantics above):** `PollUpdated`/`GiveawayUpdated` broadcasts reach **origin-channel subscribers only**, and subscribing is replace-all — so a linked card rendered in another channel (or a DM) goes silently stale. Therefore `PollWidget`/`GiveawayWidget` gain one optional prop:

```ts
refetch?: "mount" | "interval"
```

- `undefined` (default — shipped behavior unchanged): fetch only when absent from the slice (`fetchedRef` guard).
- `"mount"`: always fetch once on mount (refreshes counts **and** the per-viewer `myVote`/`myEntered`, which broadcasts never carry).
- `"interval"`: `"mount"` **plus** refetch after each own successful interaction ack **plus** a 20-second `setInterval` calling `getPoll`/`getGiveaway` → `POLL_STATE`/`GIVEAWAY_STATE` while mounted, cleared on unmount.

`LinkedWidgetCard` computes `sameChannel = link.channelId === messageChannelId` (the channel the containing message lives in — which the viewer is by definition subscribed to while reading it, including DM-panel and thread channels per AppShell.tsx:66-80) and passes `refetch={sameChannel ? "mount" : "interval"}`. Same-channel linked cards get live events anyway; cross-channel/DM cards stay ≤20s stale and self-correct instantly after the viewer's own interactions.

**Edge cases:**

- **Malformed link** (`farder://widget/poll/abc/1`, missing segment, unknown kind): the regex simply doesn't match → plain text, no pill, no card.
- **Cross-server id collision:** mitigated by the channel-id match (step 2) + visibility (step 3); worst case is the unavailable card.
- **Link inside a DM:** works — the card fetch is ordinary `GetPoll`, and `widget_channel_visible` on the *widget's* channel decides per viewer. A DM participant who can't see the origin channel gets the unavailable card; the other may get the live widget. The DM channel itself is subscribed while open, so `sameChannel` logic holds (a link to a widget in that same DM is same-channel).
- **Widget deleted/closed mid-view:** closed states render normally (widgets already handle `closed`/`ended`/`cancelled`); a deleted card's poll is closed by the delete hook, so the linked card shows the closed/final state; a fetch that now errors flips to the unavailable card on the next interval tick.

### Feature 3 — Active-widgets bar

#### Server: `ListActiveWidgets`

**Protocol (`crates/farder-protocol/src/server.rs`, appended variants — MessagePack externally-tagged enums, so append-only is the compat rule):**

```rust
// ServerRequest
ListActiveWidgets { channel_id: u64 },

// ServerResponse
ActiveWidgets {
    polls: Vec<PollInfo>,        // OPEN polls of that channel only
    giveaways: Vec<GiveawayInfo>,// OPEN giveaways of that channel only
},
```

After landing: `cargo build --workspace` **plus** `cd client/src-tauri && cargo build` (the non-workspace client crate — the MemberApproved-class regression; project memory `reference_farder_client_crate_build`).

**Handler (`handlers.rs`, sync arm beside the widget arms):**

1. Membership: **not** added to the `request_requires_membership` allow-list → default-deny gates it automatically. Actor = authenticated connection key; `channel_id` is the only client-supplied field.
2. Visibility: `if !widget_channel_visible(conn, member, channel_id, is_owner)? { return err("channel not found"); }` — the **same opaque string** for a nonexistent channel and a channel the caller can't see (the helper already returns `false` for channel-gone), so channel ids are not an existence oracle. No timeout gate (read).
3. Query via two new module fns (unit-testable without handlers):
   - `polls::list_open_in_channel(conn, channel_id, now, limit) -> Result<Vec<PollRow>>` — `WHERE channel_id = ?1 AND closed_at IS NULL AND (closes_at IS NULL OR closes_at > ?now) ORDER BY id ASC LIMIT ?limit` (the `closes_at > now` half excludes due-but-unswept polls, matching the VotePoll closed-check exactness).
   - `giveaways::list_open_in_channel(conn, channel_id, limit) -> Result<Vec<GiveawayRow>>` — `WHERE channel_id = ?1 AND status = 'open' ORDER BY id ASC LIMIT ?limit`. (Belt-and-braces note: a due-but-unswept giveaway may appear for ≤15s; its chip's Enter is still correctly rejected by the handler's `ends_at` check, and the chip drops on the sweeper's `GiveawayUpdated`.)
4. **Ordering + cap:** each list is oldest-first (`id ASC` = creation order, both `AUTOINCREMENT`); fetch each with `LIMIT 20`, then merge-sort by `created_at` ascending and truncate to **20 combined**.
5. `build_info` each → `ok(ServerResponse::ActiveWidgets { polls, giveaways })`. **No per-viewer fields** (`my_vote`/`my_entered` stay exclusive to `GetPoll`/`GetGiveaway`, unchanged rule).
6. **Rate limit: none** — same class as `GetPoll` (cheap bounded read, one per channel switch); `widget_limiter` continues to bound only mutations. **No `.await` anywhere in the arm** — it is a sync `handle_request` arm like every widget handler; the DB guard never crosses an await. **Broadcasts: none** — this feature adds zero events; `PollUpdated`/`GiveawayUpdated` keep their `Subscribers(origin channel)` targeting untouched.

#### Client plumbing

- **Tauri command (`client/src-tauri/src/commands.rs` + `generate_handler!` in `main.rs` — the untyped seam, CLAUDE.md checklist):** `list_active_widgets(server_id, channel_id) -> Result<ActiveWidgets, String>` with `#[derive(Serialize)] struct ActiveWidgets { polls: Vec<PollInfo>, giveaways: Vec<GiveawayInfo> }` ← `ServerResponse::ActiveWidgets`; standard 3-arm mapping.
- **Bridge (`tauri-bridge.ts`):** `listActiveWidgets(serverId, channelId): Promise<{ polls: PollInfo[]; giveaways: GiveawayInfo[] }>`.
- **Reducer (`ServerContext.tsx`):** `PerServerState` gains `activeWidgets: { channelId: number; polls: number[]; giveaways: number[] } | null` (initialized `null` — **ids only**; the infos are upserted into the existing `polls`/`giveaways` slices so chips share one source of truth with the widgets). New action:
  - `ACTIVE_WIDGETS { serverId, payload: { channelId: number; polls: PollInfo[]; giveaways: GiveawayInfo[] } }` — replaces `activeWidgets` with the id lists and upserts every info with `POLL_UPDATED`/`GIVEAWAY_UPDATED` semantics (preserving any existing `myVote`/`myEntered`).
  - The existing `POLL_UPDATED` / `GIVEAWAY_UPDATED` cases are **extended** to maintain the bar: when `activeWidgets` is set and `payload.channel_id === activeWidgets.channelId` — widget now open and id missing → append (a widget created live while viewing gets its chip with no refetch); widget now closed/ended/cancelled → remove its id. Client keeps the 20-cap.
  - No listener changes: the current channel **is** subscribed, so these events already arrive live (useServerEvents.ts:454-466 active-server filter is satisfied by definition).

#### Bar UI — `ActiveWidgetsBar.tsx` (new component)

Rendered in `ChatPanel.tsx` between `</div>` of `.channel-header` (:149) and `.message-list` (:150), whenever a channel (including a DM) is selected.

- **Fetch:** `useEffect` on `[serverId, connected, currentChannelId]` → `api.listActiveWidgets(serverId, currentChannelId)` → dispatch `ACTIVE_WIDGETS`; on error, dispatch with empty lists (bar hides; opaque errors are not surfaced — the member simply has no bar, consistent with not seeing the channel's widgets at all). Covers channel switch **and** reconnect (`connected` flip), mirroring the subscribe effect (AppShell.tsx:66-80, :99-101).
- **Render:** `null` when both lists are empty (zero layout cost for widget-free channels). Otherwise `<div className="active-widgets-bar">` — horizontal, `overflow-x: auto` — one `<button className="widget-chip">` per id, in server order (polls and giveaways interleaved by the merged order the ids arrived in): icon (📊 / 🎉) + question/prize truncated to ~24 chars with CSS ellipsis + `<span className="widget-chip-time">` time-left ("2h 10m"; "no end" for untimed polls; "ending…" when past due but unswept). Time-left re-derived on a 30s interval (the PollWidget footer idiom).
- **Dropdown:** clicking a chip toggles an anchored panel `<div className="widget-chip-dropdown">` positioned with `useClickAnchoredPosition` (project memory: the 125% display-scale gotcha) containing the full `<PollWidget refetch="mount">` / `<GiveawayWidget refetch="mount">` — `"mount"` (not `"interval"`) because the bar's channel is the subscribed channel, so live events keep it fresh; the mount fetch exists to pull the per-viewer `myVote`/`myEntered` that `ACTIVE_WIDGETS` upserts don't carry. One dropdown open at a time; outside-click or Esc closes; the dropdown unmounts on close (no hidden mounted widgets).
- **Chip lifecycle:** closes/ends/cancels arrive as `PollUpdated`/`GiveawayUpdated` (manual close, sweeper, delete-hook cascade all broadcast) → reducer removes the id → chip disappears live; if its dropdown was open, the widget inside shows the final state and the panel stays until dismissed (no rug-pull mid-read).

#### CSS (theme rule)

New classes — each added to **all** `client/src/themes/*/theme.css` files (`xp-luna-blue`, `discord-dark`, `hello-kitty`), colors exclusively via `var(--xp-…)`, modeled on the existing card/chip families (`.link-embed`, `.invite-link-pill`):

`.widget-copy-link`, `.widget-link-pill`, `.linked-widget-embeds`, `.linked-widget-unavailable`, `.active-widgets-bar`, `.widget-chip`, `.widget-chip-time`, `.widget-chip-dropdown`.

Done-check: `grep -l "widget-chip" client/src/themes/*/theme.css` (and the other classes) lists every theme. Feature 1 adds **no** classes.

## Security (explicit verification points)

- **Default-deny membership:** `ListActiveWidgets` is not added to the `request_requires_membership` allow-list (handlers.rs:393-401), so it is membership-gated automatically like every widget request; the actor is always the authenticated connection key, and `channel_id` is the only client-supplied field.
- **Opaque visibility, no oracle:** `ListActiveWidgets` reuses `handlers::widget_channel_visible` (handlers.rs:367 — DM-participant / `VIEW_CHANNEL`, channel-gone → false) and returns the identical `err("channel not found")` for missing and invisible channels. Widget links add **no** server surface at all: they resolve through the shipped `GetPoll`/`GetGiveaway`, whose visibility failures already collapse into the opaque "poll/giveaway not found" — so neither a pasted link nor a probed channel id can confirm the existence or reveal the content of a channel the viewer cannot see. The link's embedded `channel_id` is never sent to the server.
- **Lock discipline:** the new handler arm is synchronous; no DB `Mutex` is held across any `.await` (nothing in any of the three features touches the sweeper/broadcast paths).
- **Broadcasts unchanged:** zero new events; `PollUpdated`/`GiveawayUpdated` keep `EventTarget::Subscribers(origin channel_id)`. The linked-card staleness this causes is handled client-side by the refetch discipline, not by widening any broadcast.
- **Rate limits:** reads stay unlimited like `GetPoll` (`ListActiveWidgets` is one bounded query per channel switch; the linked-card 20s interval is a `GetPoll`-class read, bounded by the 3-embeds-per-message cap). All mutations keep the shared `widget_limiter` (10/10s) untouched.
- **Client-only features add no trust surface:** custom durations only construct a token the server re-validates from scratch; widget-link JSON/ids are parsed with `Number.isSafeInteger` guards and never fed anywhere but the existing typed bridge calls.

## Testing

- **Server — module fns (unit, in-memory conn):** `polls::list_open_in_channel` returns only open polls of that channel (closed excluded; **past-`closes_at`-but-unswept excluded**; other channels excluded; untimed included), `id ASC` order, respects limit; `giveaways::list_open_in_channel` same for `status='open'`.
- **Server — handler (handlers.rs `mod tests` fixtures — `setup()`/`add_member`/`make_channel`/`fake_state`):** `ListActiveWidgets` happy path returns both kinds oldest-first; combined cap at 20 (create 25, assert 20 by `created_at`); member without `VIEW_CHANNEL` → `err("channel not found")`; **nonexistent channel returns the byte-identical error string** (oracle test); DM: participant gets the DM's widgets, non-participant gets "channel not found"; response carries no per-viewer fields.
- **Client:** `cd client && npx tsc --noEmit`; invoke-name ↔ `generate_handler!` audit for `list_active_widgets`; theme grep for every new class across all theme files; `cd client/src-tauri && cargo build` after the protocol change (workspace build alone is NOT sufficient).
- **Detection (reasoned + runtime-verified):** widget links must not match the invite path — verify a message containing both an invite link and a widget link renders one join card and one widget card, no cross-contamination.
- Refetch timing (20s interval), clipboard, dropdown anchoring, and bar live-updates are runtime-verified (below), not unit-tested.

## Owner runtime verification (server changed → sidecar rebuild; two clients ideal)

1. **Custom durations:** Poll builder → duration "Custom…" → 45 + minutes → poll posts with a 45m countdown. Try 45 + days → inline "between 1 minute and 30 days" error, no submit. Giveaway builder same with 90 + minutes; "No end time" still present for polls only.
2. **Copy link:** click 🔗 on a live poll card → toast; paste into the same channel → text pill + a second live card below; voting on either updates both instantly (same slice + same channel events).
3. **Cross-channel link:** paste a #general poll link into #random → interactive card renders; vote from #random (works); have the second client vote in #general → the #random card catches up within ~20s (interval refetch). Your own vote reflects immediately.
4. **Visibility:** paste a link to a widget in a channel the second account cannot see → that account sees only the compact "Poll not available" card; confirm the error reveals nothing else. Paste a mangled link (`farder://widget/poll/x/1`) → renders as plain text.
5. **DM link:** DM a giveaway link to the second account → they can enter from the DM card if they can see the origin channel; count updates on the origin card.
6. **Invite regression:** paste a server invite link → join card still renders as before (widget detection didn't swallow it).
7. **Active bar:** open a channel with two live widgets → two chips with countdowns under the header; click one → dropdown with the full widget; vote from the dropdown → counts move on the in-channel card too. `/poll` a new poll while watching → its chip appears live. Close/cancel a widget (or wait out its timer, or delete its card message) → chip disappears on both clients; channel with no widgets shows no bar.
8. **Themes:** flip through all themes → pills, cards, chips, dropdown all styled (no raw/unstyled elements).

## Decomposition (for the plan — exactly 4 build tasks)

1. **T1 — Custom durations (client-only).** "Custom…" entry + amount/unit inputs + token construction + 1m–30d client clamp with `.error-text` in `PollBuilderModal.tsx` and `GiveawayBuilderModal.tsx`; poll keeps "No end time", giveaway keeps required duration. `npx tsc --noEmit`. No server, no protocol, no CSS.
2. **T2 — Server `ListActiveWidgets` + protocol + tests.** Appended `ServerRequest::ListActiveWidgets` / `ServerResponse::ActiveWidgets`; `polls::list_open_in_channel` + `giveaways::list_open_in_channel`; the sync handler arm (default-deny membership, `widget_channel_visible`, opaque "channel not found", merge/cap 20, no limiter); module + handler + oracle tests; client-crate rebuild.
3. **T3 — Linked widget cards.** `.widget-copy-link` control on both widgets (clipboard idiom); `WIDGET_LINK_REGEX` + `parseWidgetLink` + `isInviteLink`/embeds-IIFE exclusion guard + `.widget-link-pill` in `Message.tsx`; `LinkedWidgetCard.tsx` (fetch → channel-id consistency check → widget | unavailable card); the `refetch?: "mount" | "interval"` prop on `PollWidget`/`GiveawayWidget` (mount + post-interaction + 20s interval, cleared on unmount); CSS for the four T3 classes in all themes.
4. **T4 — Active bar UI.** Tauri command + `generate_handler!` + bridge fn; `activeWidgets` slice + `ACTIVE_WIDGETS` action + `POLL_UPDATED`/`GIVEAWAY_UPDATED` maintenance; `ActiveWidgetsBar.tsx` (fetch effect, chips, 30s time-left tick, `useClickAnchoredPosition` dropdown hosting `refetch="mount"` widgets); ChatPanel slot; CSS for the four T4 classes in all themes; docs (`tauri-commands.md`, `tauri-bridge.md`, `frontend-context.md`).

## Carry-forward / known limitations

- Linked cards outside the origin channel are up to ~20s stale between interval ticks (documented, self-correcting); a future targeted "widget subscriptions" event channel could remove the polling.
- The active bar shows open widgets only; a "recently ended" view or a server-wide widgets moderation list remains future work (pairs with the shipped entrant/voter-list carry-forwards).
- Chip labels expose the question/prize only to members who can already see the channel (bar is per-channel behind visibility) — nothing new leaks, but a future roll-up across channels must re-apply per-channel visibility.
- Old client binaries cannot decode frames containing the appended protocol variants (existing project-wide property of every protocol addition; client+server ship together).
- Widget links are plain text to old clients (no pill, no card) — degraded but coherent, same class as the widget-fallback content.
