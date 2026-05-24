# Message Search v1 — Design

**Status:** Drafted 2026-05-23
**Scope:** Farder client (Tauri + React). No server changes — reuses the existing `ServerRequest::Search` protocol arm and the `search_messages` Tauri command. UI-only feature.

## Goal

Press `Ctrl+K` (`Cmd+K` on macOS) → centered search overlay opens → type a query → results stream in within ~300 ms. Hovering or arrow-keying a result shows a preview of the message in context (5 messages before, the match, 5 after). Clicking or pressing Enter jumps to that message in its channel, briefly highlighting it.

The backend search command (`search_messages(serverId, query, channelId?, limit?)`) already exists and returns `Vec<MessageInfo>`. This spec is entirely client-side.

## Non-Goals

- **DM search.** `search_messages` is server-scoped; DMs live in a separate channel space. Search within DMs is a v1.5 follow-up.
- **Filter chips** (`from:user`, `in:channel`, `before:date`). v1 is plain text only. Easy to add later — the chips would compose into the existing `query` field or a new structured request.
- **Cross-server search.** v1 searches the currently-active server. The overlay closes if you switch servers.
- **Full-text ranking / fuzzy match.** v1 leans on whatever ranking the server already does. No client-side reordering.
- **Search history.** No persisted "recent searches" list in v1.
- **Highlight matched terms inside snippets.** v1 shows the message text as-is. Term highlighting in the snippet is a nice-to-have.

## Architecture

```
┌────────────────────── Tauri WebView (renderer) ──────────────────────┐
│                                                                       │
│  AppShell                                                             │
│    └─ global Ctrl+K / Cmd+K listener → opens overlay                  │
│    └─ <MessageSearchOverlay />  (mounted, conditional render)         │
│                                                                       │
│  client/src/components/MessageSearchOverlay.tsx                       │
│    ├─ search input (auto-focus, debounced 300ms)                      │
│    ├─ left pane: results list (selectable, ↑/↓ keyboard)              │
│    └─ right pane: preview (5 before + match + 5 after)                │
│                                                                       │
│  client/src/hooks/useMessageContext.ts                                │
│    └─ fetches window of messages around a given (channelId, msgId);   │
│       caches per-msgId for the search session; AbortController on     │
│       stale fetches when selection changes quickly                    │
│                                                                       │
│  client/src/context/ServerContext.tsx  (modified)                     │
│    ├─ new state field: highlightMessageId: number | null              │
│    └─ new action: HIGHLIGHT_MESSAGE { messageId, ttlMs }              │
│                                                                       │
│  client/src/components/ChatPanel.tsx  (modified)                      │
│    ├─ when highlightMessageId changes, scrollIntoView on that <div>   │
│    └─ applies .search-highlight class for ~1.2s, then clears          │
│                                                                       │
└───────────────────────────────────────────────────────────────────────┘
                                  │
                                  │ Tauri IPC
                                  ▼
            (existing) search_messages, fetch_history commands
```

### Why a centered overlay (not a sidebar or title-bar box)

- Doesn't steal screen real-estate when not in use
- Familiar pattern (Discord, Slack, VS Code)
- Two-column layout fits comfortably in 800×600 and scales up
- Closes on Esc / outside-click without disturbing the channel view

### Why hover-previews + click-to-jump

- Cheap glance ≠ expensive context switch. Most "where did X say Y" queries are answered by the preview alone.
- Click is the explicit commit; user opts into the channel switch.
- Matches the user's stated preference (vs. Discord-style "click jumps immediately").

## UX

### Layout

```
┌──────────────────────────────────────────────────────────────────┐
│  Search messages in <Server Name>                          [×]   │
├──────────────────────────────────────────────────────────────────┤
│  [ search query…                                               ] │
├──────────────┬───────────────────────────────────────────────────┤
│              │                                                   │
│  alice       │  alice       11 min ago   in #general            │
│  in #general │  > so I was reading about Bergamot               │
│  > thanks…   │                                                  │
│              │  bob         10 min ago   in #general            │
│  bob         │  > oh nice — got a link?                          │
│  in #random  │                                                   │
│  > sure thing│  alice        9 min ago   in #general            │
│              │  > yeah here: https://browser.mt/                 │
│  alice       │                                                   │
│  in #random  │  …  (10 messages of context above the match)      │
│  > anyway…   │                                                   │
│              │  bob          4 min ago   in #general  ◀ MATCH   │
│              │  > thanks for the link ◀ highlighted              │
│              │                                                   │
│              │  (no "after" context in v1 — see fetch note)      │
└──────────────┴───────────────────────────────────────────────────┘
```

- **Left pane** (~320 px wide): scrollable list of results. Each row shows: avatar (small), display name, channel name, timestamp, single-line message snippet (truncated with ellipsis). The selected row has an accent background.
- **Right pane** (fills remaining width): renders the context window (see "About fetching context around a message" below — v1 ships 10 before + the match, no after) using the existing `<Message>` component in a slim variant. The match has a subtle highlighted background and a small "MATCH" pill in the corner.
- Header shows the server name (search is server-scoped, makes scope obvious).

### Interactions

| Action | Effect |
|---|---|
| Ctrl+K / Cmd+K | Opens overlay; if already open, refocuses the input |
| Esc | Closes overlay, returns focus to whatever had it before |
| Click overlay backdrop | Closes overlay |
| Type in input | Debounced 300 ms → fires `searchMessages(serverId, query, undefined, 50)` |
| ↑ / ↓ | Move selection in results list (wraps top↔bottom) |
| Hover a result | After 200 ms, sets selection to that result |
| Selection change | Triggers preview fetch (debounced 200 ms; AbortController cancels stale fetches) |
| Click result row | Commits jump |
| Enter (with selection) | Commits jump |
| Commit jump | Closes overlay; dispatches `SELECT_CHANNEL` then `HIGHLIGHT_MESSAGE { messageId, ttlMs: 1200 }`; ChatPanel scrolls the matched message into view and applies the highlight class for 1.2 s |

### Empty / loading / error states

| State | Display |
|---|---|
| Overlay just opened, empty query | Left: "Type to search messages." Right: empty. |
| Query non-empty, searching | Left: spinner + "Searching…". Right: previous preview if any, else empty. |
| Zero results | Left: "No messages match \"…\"". Right: empty. |
| Search failed | Left: red "Search failed — \<reason\>. Press Enter to retry." Right: empty. |
| Result selected, preview loading | Right: skeleton of 11 message rows. |
| Preview fetch failed | Right: "Couldn't load context — \<reason\>". Match still highlighted in left pane. |
| Match channel is one the user can't currently see (e.g., role overrides changed) | Left row is dimmed; click shows an error toast "You don't have access to that channel anymore." No channel switch. |

## Data flow

1. User presses `Ctrl+K` → `AppShell` sets `searchOpen = true` → `<MessageSearchOverlay />` mounts and auto-focuses the input.
2. User types — input state updates immediately. After 300 ms of inactivity, fires:
   ```ts
   const results = await searchMessages(serverId, query.trim(), undefined, 50);
   ```
3. Results render in the left pane. Default selection: index 0 if results non-empty.
4. Selection change (hover after 200 ms / arrow key) → after 200 ms debounce, calls `useMessageContext({ channelId, messageId })`:
   - Hits an in-overlay-session `Map<messageId, ContextWindow>` cache.
   - On cache miss: `fetchHistory(serverId, channelId, beforeId: messageId + 1, limit: 11)` — backend returns up to 11 messages ending at the match. (If the backend returns fewer than 11 because the match is near the channel's head, the preview is rendered with whatever was returned, padded with "(top of channel)" if applicable.)
   - Stores in cache.
   - Cancels via AbortController if selection changes mid-flight.
5. Right pane renders the context with the matched message styled distinctly (subtle background tint, bold border-left).
6. On commit (click or Enter):
   - Dispatch `SELECT_CHANNEL { channelId }` (already wired; ChatPanel listens and re-fetches history if needed).
   - Dispatch `HIGHLIGHT_MESSAGE { messageId, ttlMs: 1200 }`.
   - Set `searchOpen = false`.
7. ChatPanel `useEffect` watches `highlightMessageId`:
   - On change to a non-null id, `document.getElementById(\`msg-${id}\`)?.scrollIntoView({ block: "center", behavior: "smooth" })`.
   - The matched `<Message>` element applies `className="message search-highlight"`; the CSS keyframes do an orange-tinted background that fades to transparent over 1.2 s.
   - After ttlMs, a setTimeout dispatches `HIGHLIGHT_MESSAGE { messageId: null }`.

### About fetching context around a message

The existing `fetch_history` command takes `(serverId, channelId, beforeId?, limit)`. To get 5 before + match + 5 after, we have two options:

- **Option A (chosen for v1):** one call to `fetch_history(channelId, beforeId: matchId + 1, limit: 11)`. Backend returns messages strictly *before* `beforeId` ordered newest-first; we get the match itself plus up to 10 immediately preceding messages. That gives us the match + 10 before — NOT 5 before + 5 after.
- **Option B:** two calls — `before` and `after` — and stitch. Requires `fetch_history_after` which doesn't currently exist on the server.

**Decision:** Ship Option A in v1 (10 before + match, no after). It's a known compromise; "5 before + 5 after" requires a small server-side addition (an `after_id` field in `ServerRequest::FetchHistory` or a new request type) which lives outside this UI-only spec.

This compromise is called out in the spec's testing section so the reviewer knows the visual layout in the design ("5 before + match + 5 after") differs from what v1 actually renders. v1.5 adds the bidirectional fetch.

## Component / file inventory

**Created:**
- `client/src/components/MessageSearchOverlay.tsx` — the modal + dual-pane UI.
- `client/src/hooks/useMessageContext.ts` — fetch + cache for context windows.
- (No new CSS file.) Overlay styling uses inline `style={{...}}` consistent with existing components (e.g., `TranslationDownloadDialog.tsx`); the `.search-highlight` keyframes go into the existing `client/src/index.css` (or whichever stylesheet currently holds global keyframes).

**Modified:**
- `client/src/components/AppShell.tsx` — register the global Ctrl+K / Cmd+K listener; mount the overlay when open. Renders `null` if not connected to a server.
- `client/src/context/ServerContext.tsx` — add `highlightMessageId: number | null` to state; add `HIGHLIGHT_MESSAGE` reducer action; the existing `SELECT_CHANNEL` action does NOT clear highlight (so it survives the channel switch).
- `client/src/components/ChatPanel.tsx` — `useEffect` on `highlightMessageId` that scrolls the matched message into view; clears the highlight after `ttlMs` via a follow-up dispatch.
- `client/src/components/Message.tsx` — render a stable `id={`msg-${message.id}`}` attribute on the outer wrapper, and apply `search-highlight` class when `message.id === highlightMessageId` (props passed down from ChatPanel).
- `client/src/index.css` (or theme CSS) — add the `.search-highlight` keyframes.

## Keyboard shortcut handling

The existing codebase has multiple `addEventListener("keydown", …)` patterns. To avoid stomping (e.g., the user typing `K` in a chat input shouldn't toggle search), the global listener:

- Uses `window.addEventListener("keydown", handler, { capture: true })` so it fires before component handlers.
- Skips when the event target is an `<input>`, `<textarea>`, or has `contenteditable="true"` AND the modifier (Ctrl/Cmd) is not held. With the modifier held, the shortcut fires regardless of target (so you can open search while typing).
- Detects platform via `navigator.platform.startsWith("Mac")` for Cmd vs Ctrl. (Tauri builds run on real OSes, so this works.)

## Error handling

| Failure | Behavior |
|---|---|
| `searchMessages` rejects | Left pane shows red "Search failed — \<reason\>. Press Enter to retry." Selection cleared. |
| `fetchHistory` for preview rejects | Right pane shows error message; left pane unaffected, user can pick a different result. |
| Selected result's channel was deleted between search and click | Backend returns 404-equivalent in fetch_history; preview shows "Channel no longer available." Click → error toast, no jump. |
| User hits Enter when no results visible | No-op. |
| User clicks a result while preview for it is still loading | Commit happens anyway — the preview was for the user's own benefit; jump uses just `channelId` + `messageId` from the result row. |
| User opens search overlay, then disconnects from server | Existing connection-lost UI takes over; overlay closes when AppShell unmounts. |

## Testing

### Manual smoke

- Open overlay (Ctrl+K), type a known string from a recent message → result appears within 1 s.
- Arrow up/down through 3+ results, watch preview refresh.
- Hover → preview after 200 ms.
- Esc closes; previous focus restored.
- Click a result → channel switches, message scrolls into view, briefly highlights orange ~1.2 s.
- Search with no matches → "No messages match" copy.
- Disconnect mid-search → overlay closes cleanly.
- Search a string only in a channel you've never opened → after jumping, history loads and the matched message scrolls into view (depends on `fetchHistory` resolving with the message in the returned window).
- Open chat input, type "key", verify K does NOT open the overlay. Then hit Ctrl+K from inside the input — should still open.

### What's NOT smoke-tested in v1

- Concurrency under fast-hover sweeps (>10 results/sec). The 200 ms debounce + AbortController is intended to handle this; sweep-test by holding ↓ for a second across a long results list.
- Behavior when `highlightMessageId` is set for a message in a channel the user just lost access to (probably: `Message` never renders, highlight silently expires). Verify with a manual role-removal test or defer.

### Unit-testable surface (deferred)

The codebase has no TS test infrastructure (no Vitest, no Jest). Pure-function helpers that could earn tests later:
- `useMessageContext` cache eviction
- The Ctrl+K platform detection / input-focus skip logic

If TS testing is set up later, these are easy targets.

## Performance considerations

- **Search debounce:** 300 ms after last keystroke. Prevents request storms while typing.
- **Selection debounce:** 200 ms. Hovering through a tall results list won't fire 30 preview fetches.
- **AbortController:** every stale preview fetch is cancelled when selection changes. Prevents UI flicker if the second fetch resolves before the first.
- **Cache:** preview windows cached per `messageId` for the duration of the search session. Cleared when the overlay closes.
- **Result limit:** 50. Server-side `search_messages` has a default of 20; bumping to 50 still keeps the response small (~10 KB at typical message sizes).

## Migration / rollout

- No data migration.
- No protocol changes.
- No persistent storage changes.
- Backward-compatible: feature is purely additive. A user on an older client connecting to a server that supports `Search` (all current servers do) sees no change. A new client connecting to a server that doesn't support `Search` (none in production) would surface "Search failed" — acceptable.

## Future work (v1.5+)

- **`fetch_history_after`** or `range_around` server request — enables the true "5 before + 5 after" preview window. ~30 lines on the server.
- **DM search** — extend `search_messages` to accept a DM channel id; mostly a backend change.
- **Filter chips** — `from:user`, `in:#channel`, `before:date`. Client parses query, structures the request.
- **Term highlighting in snippets** — wrap matched substrings in the left-pane snippets with `<mark>`.
- **Recent searches** — last 10 queries persisted in `~/.farder/settings.json`, shown when input is empty.
- **Cross-server "search everywhere"** — fan out to all connected servers, group results by server.
