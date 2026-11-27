# Small window layout

**What it does:** an opt-in layout for leaving Farder open in a corner like an old
messenger (Skype / MSN): one narrow list of conversations (people first, then the
channels of the current server) and one chat. The server column, the full channel
sidebar, the member list and the DM pop-out panel are hidden, and the window is
shrunk (minimum 380 x 280, default 560 x 380). Chosen in Settings -> Window layout
or back with the "Full window" button at the bottom of the list. Remembered on
this device (`localStorage` key `farder.layout`); nothing is sent anywhere.

**Files:** `client/src/lib/layout.ts` (choice + the stylesheet),
`client/src/lib/layoutWindow.ts` (`changeLayout`, `initLayout`: also resizes the
window), `client/src/hooks/useLayout.ts`,
`client/src/components/CompactList.tsx` (the list),
`client/src/components/settings/LayoutSettings.tsx`, the one-line mount in
`AppShell.tsx`, `window_behavior::{plan_layout, set_window_layout}` in Rust.
`lib/selectChannel.ts` and `lib/selectServer.ts` are the open-a-channel and
switch-server logic, extracted from `ChannelSidebar` / `ServerStrip` so the list
and the full layout share one copy.

## How it works

- `<html data-layout="small">` plus one injected stylesheet. It only HIDES the
  wide-layout parts (every rule is scoped to the small layout, so the full layout
  is untouched; a test enforces that and that no literal colour appears).
- `CompactList` carries the classes `channel-sidebar compact-list`, and reuses
  `server-header`, `channel-item`, `channel-category`, `dm-item` so it takes each
  theme's own look without new per-theme CSS. The hide rule uses
  `.channel-sidebar:not(.compact-list)`.
- The full `ChannelSidebar` and `ServerStrip` STAY MOUNTED (hidden): the sidebar
  owns the "auto-select a channel when none is selected" effect and the voice
  controls, and the app breaks quietly without them.
- DMs open in the main chat (they are channels), not the pop-out panel.
- The window always starts at its default size, so `initLayout` re-applies a saved
  small layout (and its lower minimum) on every launch.

## Not in the small layout (go to the full window)

Voice channels (a "Leave voice" button appears if you are in a call), the member
list, channel/server settings (the gear opens YOUR settings), creating invites,
categories and drag-to-reorder.

## Gotchas

- Per-channel unread uses only messages already loaded in memory (same rule as the
  full sidebar); there are no counts, just the bold/dot style.
- The window comes back at 1200 x 800 when you return to the full layout, not at
  the size you had before.
- The chat header and message box were designed for a wide window; at 380 px they
  may be tight. UNVERIFIED on Windows (checklist Part 10).
- Tests: `client/tests/layout.test.mjs`, `window_behavior::tests` (plan_layout).
