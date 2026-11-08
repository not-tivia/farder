# Edge strip

**What it does:** an opt-in, one-line (30 px) always-on-top bar docked to the top
or bottom of the primary monitor, for leaving Farder open beside a game or a
document. At rest it shows a chip per conversation that has unread messages; when
a message arrives it shows a one-line preview with a **Reply** button and a
one-line reply box. It REPLACES the notification pop-up while it is on.

**Files:** `client/src-tauri/src/edge_strip.rs` (window, settings, geometry,
commands), `client/src/components/EdgeStrip.tsx` (the strip's own page, loaded as
`index.html?strip=1` in the `strip` window), `client/src/lib/edgeStrip.ts` (pure
state: `StripModel`, `chipLabel`, `parseOpen`), `client/src/hooks/useServerEvents.ts`
(feeds it), `client/src/components/settings/EdgeStripSettings.tsx` (Settings ->
Edge strip), `.edge-strip*` classes in every theme's `theme.css`.

## Rules it follows (same as the pop-up, `notify-popup.md`)

- **Never takes the keyboard** except while its reply box is open
  (`strip_focus`); hiding resets it to unfocusable.
- **Privacy:** the main window builds the text with `buildNotificationText` using
  the person's "What a pop-up may show" setting. `hidden` folds every conversation
  into ONE anonymous "New messages" chip and gives no Reply button; `sender` shows
  only names, never channel names or words.
- **It never sends.** A reply is `notify:reply` -> the main window re-checks and
  sends (`quickReply.ts`), exactly as for the pop-up. Encrypted channels get no
  strip entry for the same reason they get no pop-up.
- **No network, text only in memory.**
- **Opaque window, not transparent**, so there is no see-through padding for
  Windows to draw an outline around (the pop-up's problem).

## Data flow (events)

| Event | From -> to | Payload |
|---|---|---|
| `strip:state` | main -> strip | `{ chips: StripChip[], preview: StripPreview \| null }` |
| `strip:ready` | strip -> main | `{}` (main re-sends the state) |
| `strip:open` | Rust (`strip_open`) -> main | `{ server_id, channel_id }`, validated by `parseOpen` |
| `strip:replied` | strip -> main | `{ server_id, channel_id }`; that chip is cleared |
| `strip:settings-changed` | Rust -> all | the saved `StripSettings` |
| `notify:reply` / `notify:reply-result` | strip <-> main | shared with the pop-up |

Chips clear when the main window gains focus (you have seen Farder). A preview
folds back to the chips after 10 s, holds while the mouse is over it or a reply is
open. The strip is hidden when it has nothing to show unless "Keep it on screen"
is ticked.

## Gotchas

- With "No pop-ups at all" the strip stays empty ("off means off"); the settings
  screen says so.
- Chips are per conversation, not per server (the server-level unread counter is
  separate and unchanged).
- Tests: `edge_strip::tests` (geometry, clamping), `client/tests/edgestrip.test.mjs`
  (counting, ordering, privacy folding, `parseOpen`). The window itself, focus
  behaviour and the look are UNVERIFIED on Windows (checklist Part 9).
