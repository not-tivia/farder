# Floating Picture-in-Picture Video (`PipContext` / `PipPane` / `PipLayer`)

In-app, draggable, opacity-adjustable floating video players. A playable video
embed in chat renders a compact poster (▶ Play); clicking it opens a floating
pane that streams the same relay-proxied bytes. Client-only — no relay,
protocol, or Tauri-command changes.

## Pieces

- `client/src/context/PipContext.tsx`
  - `PipProvider` — wraps `<AppInner />` in `App.tsx`; holds the open-pane list.
  - `usePip()` → `{ panes, openPip, closePip, focusPip, updatePip }`.
  - `openPip({ mediaUrl, title?, mime? })` — dedupes by `mediaUrl` (re-focuses an
    existing pane instead of duplicating), enforces a cap of `MAX_PIPS` (4; a
    `toast.info` when exceeded), assigns a cascade position, opacity 1, top `z`.
  - `pipReducer` — the pure, inspection-tested core (dedupe, cap, focus z-bump,
    patch). Has inline test-notes.
- `client/src/components/PipPane.tsx` — one floating pane: header (title, opacity
  slider, minimize, close), `<video controls autoPlay>` fed by
  `useProxiedMedia(mediaUrl)`, drag-by-header (ScreenShareStage pattern),
  CSS `resize: both` persisted via a `ResizeObserver`, bring-to-front on click,
  minimized pill, "Couldn't load video" fallback.
- `client/src/components/PipLayer.tsx` — renders one `PipPane` per open pane;
  mounted once in `AppShell` next to `ScreenShareStage` so panes float above all
  views and persist across channel/server navigation.

## Caps / constants

- `MAX_PIPS = 4`; opacity range 0.2–1.0; z-order base 300 (above `ScreenShareStage`'s 200).

## Privacy

PiP plays only the relay-proxied bytes the embed already fetched (`useProxiedMedia`
→ `get_proxied_media`). No new external connections. Blob URLs are revoked on
pane close by `useProxiedMedia`'s cleanup.

## Verification status

UNVERIFIED at runtime (needs a display; WSL can't). `tsc` clean and the reducer
test-notes hold by inspection. Owner Windows check: click ▶ on a tweet video →
floating pane plays; drag/resize/opacity work; chat readable behind it; open a
second video → two panes, click toggles front; switch channels → panes persist;
✕ closes and frees it.

## Theming

`.pip-pane*` and `.link-embed-poster*` classes are defined in all three themes
(`client/src/themes/*/theme.css`), variable-driven.
