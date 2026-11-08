# Skinned notification window (`notify_popup.rs` + `NotifyPopup.tsx`)

**What it is:** a small always-on-top, transparent, never-focused window at the
top-centre of the primary monitor that shows message notifications using the
ACTIVE THEME's CSS. It shows whether Farder is focused, minimised, or behind
another program.

**Why it exists instead of the OS toast:** a Windows toast is copied into the OS
notification history, which would put the plaintext of an end-to-end encrypted
message into a store Farder does not control. This window draws the text locally,
keeps it in memory only, and makes no network request.

## Flow

1. `useServerEvents` decides a message deserves a pop-up (`notifyIfWanted`):
   prefs allow it, it is not our own message, and either it is in a server other
   than the one on screen OR the app window is not focused (`appIsInBackground`).
2. `showNotification(title, body)` -> `show_notification` -> `notify_popup::push`
   emits the app event `notify:push` `{title, body}` to the `notify` window.
3. `NotifyPopup` (page `index.html?notify=1`, branch in `main.tsx`) keeps up to 4
   cards (3 if one has media), each auto-dismissed after 6 s (paused while
   hovered), and calls `notify_layout(count, measuredHeight)` so Rust sizes,
   positions and shows (or hides) the window.
   A card may carry `media_url`: a muted, looping preview of a direct video/image
   link, fetched through the relay-proxied path. The client sets it ONLY for a
   sender on the person's Autoplay list with pop-up previews on (`lib/autoplay.ts`).
4. Clicking a card calls `notify_activate`, which focuses the `main` window.

## Contracts to keep in step

- `CARD_H`, `GAP`, `CARD_W` in `notify_popup.rs` must match the `.notify-popup-*`
  CSS in EVERY theme file (a plain card is `min-height: CARD_H`). The window's
  height comes from the page's own measurement, clamped to `MAX_HEIGHT`.
- The window is created hidden at startup (`setup` in `main.rs`) so its page has
  loaded before the first message. `push` also builds it on demand (with a wait)
  as a fallback.
- The `notify` label must stay in `capabilities/default.json`'s `windows` list.

## Settings

`popupStyle` in `notifications.json`: `"skinned"` (default) or `"system"` (the OS
toast). Chosen in Settings -> Notifications -> Pop-up style.

## Verification status

Layout geometry is unit tested (`cargo test notify_popup`). The window itself
(transparency, no focus steal, click-through when empty, theme in the pop-up)
is UNVERIFIED until run on Windows.
