# Floating Picture-in-Picture Video Player — Design Spec

**Date:** 2026-06-21
**Status:** Approved (brainstorm), pending implementation plan
**Builds on:** the rich external embeds feature (`LinkEmbed`, `useProxiedMedia`,
the relay media proxy) and the screenshare stage's floating-pane UX
(`ScreenShareStage` — drag/minimize machinery).

## Problem

Playable video embeds (Twitter/X video, direct video files) currently auto-play
*inline* in the message list. Even capped at 300px tall they crowd the chat, and
you can't keep watching while scrolling/reading other messages. The owner wants
a **floating, draggable, opacity-adjustable picture-in-picture player** so a
video can float over the UI without covering the chat you're reading — and wants
to be able to open **several at once**.

## Product decisions (owner)

- **Trigger:** inline embeds for playable video become a **compact poster**
  (thumbnail + ▶ Play); clicking Play opens a floating **PiP** pane and streams
  the bytes there. No auto-playing video inline anymore (cleaner chat, lazy
  bandwidth).
- **Scope of sources:** **relay-proxied playable video only** — Twitter/X video
  and direct video files (embeds where `media.playable_inline` is true and the
  mime is video). YouTube and Spotify keep their external-open buttons. Images
  stay inline (no PiP for stills).
- **Multiple simultaneous PiPs** (owner chose this over single-pane), capped at
  **4** open panes (a toast when exceeded).
- **In-app floating overlays**, not detached OS windows — the see-through
  **opacity** requirement only makes sense for an in-app overlay. (Detach-to-OS
  -window is the screenshare stage's mode and is explicitly out of scope here.)
- **Per-pane controls:** native video controls (play/pause/seek/volume) + drag
  (by header) + resize + opacity slider + minimize (collapse to a pill) + close.
  Clicking a pane brings it to the front.
- **Persistence:** panes live at the `AppShell` overlay level, so they persist
  while you switch channels/servers.
- **Privacy:** unchanged — PiP renders the *same* relay-proxied bytes the inline
  player already used. No new external connections, no new backend.

## Architecture

Entirely **client-side**. No relay, protocol, or Tauri-command changes — the
relay's existing `ProxyMedia`/`fetch_media` already serves the video bytes, and
`useProxiedMedia` already turns them into a blob URL.

```
LinkEmbed (compact poster, ▶ Play)
   └─ openPip(embed)  ──►  PipManager (context, mounted in AppShell)
                              holds [{ id, title, mediaUrl, pos, size, opacity, z }]
   PipLayer (renders in AppShell, above all views)
      └─ PipPane × N   ──►  useProxiedMedia(mediaUrl) → <video controls>
```

## Components

### A. `PipManager` (new — `client/src/context/PipContext.tsx`)
- React context exposing the open-pane list and actions:
  - `openPip(input: { mediaUrl: string; title?: string; mime?: string }): void`
    — dedupes by `mediaUrl` (if already open, brings that pane to front instead
    of adding a duplicate); enforces the **cap of 4** (no-op + a toast when at
    cap); assigns a fresh `id`, an initial cascade position/size, opacity 1, and
    the top `z`.
  - `closePip(id: string): void` — removes the pane.
  - `focusPip(id: string): void` — assigns it the top `z` (bring-to-front).
  - `updatePip(id, patch: Partial<{ pos; size; opacity; minimized }>): void`.
- Holds state: `panes: PipPaneState[]` and a monotonic `nextZ` counter.
- `PipPaneState = { id: string; mediaUrl: string; title: string; pos: {x:number;y:number}; size: {w:number;h:number}; opacity: number; minimized: boolean; z: number }`.
- `const MAX_PIPS = 4`.

### B. `PipPane` (new — `client/src/components/PipPane.tsx`)
- Props: `pane: PipPaneState` + the manager actions it needs.
- Renders a floating `div.pip-pane` positioned at `pane.pos`, sized `pane.size`,
  `style={{ opacity, zIndex: z }}`, containing:
  - a **header** (drag handle): title, opacity `<input type="range">` (0.2–1.0),
    minimize button, close button.
  - a `<video controls autoPlay>` whose `src` is `useProxiedMedia(pane.mediaUrl, true)`
    (the blob URL); a "couldn't load" state if it resolves null.
  - **resize:** CSS `resize: both` on the pane element, with a `ResizeObserver`
    that writes the new dimensions back via `updatePip({ size })` so the size
    survives re-renders (panes re-render on z-order changes; an uncontrolled CSS
    size would otherwise reset).
- `onMouseDown` on the pane calls `focusPip(id)` (bring-to-front).
- Drag logic reuses the proven pattern from `ScreenShareStage` (`startDrag` →
  window mousemove/mouseup, write back via `updatePip({pos})`).
- Minimized state collapses to a small pill (mirror `screen-stage-mini`) showing
  the title + a restore button.

### C. `PipLayer` (new — `client/src/components/PipLayer.tsx`)
- Subscribes to `PipManager`; renders one `PipPane` per open pane.
- Mounted once in `AppShell` (the same overlay level as `ScreenShareStage`), so
  panes float above all views and persist across navigation.

### D. `LinkEmbed` change (`client/src/components/LinkEmbed.tsx`)
- For a playable **video** embed (`inlineMedia` with a `video/*` mime), STOP
  rendering the inline `<video>`. Instead render a **compact poster**: the
  proxied thumbnail (`useProxiedMedia(e.thumbnail)`) or a neutral placeholder,
  with a ▶ Play overlay button that calls `openPip({ mediaUrl: inlineMedia.url,
  title: e.author ?? e.title ?? "Video", mime: inlineMedia.mime })`.
- Playable **image** media (e.g. a tweet photo) keeps rendering inline as today.
- Non-inline providers (YouTube/Spotify) keep the existing thumbnail + external
  open button (unchanged).
- The inline video bytes are no longer fetched on card display (the poster only
  needs the small thumbnail); the video streams when the PiP opens.

### E. Theming
- New classes — `.pip-pane`, `.pip-pane-head`, `.pip-pane-title`, `.pip-pane-video`,
  `.pip-pane-opacity`, `.pip-pane-min`, `.pip-pane-close`, `.pip-pane-mini`,
  `.pip-pane-state`, plus the compact-poster classes (`.link-embed-poster`,
  `.link-embed-poster-play`) — styled in ALL THREE themes
  (`discord-dark`, `hello-kitty`, `xp-luna-blue`) using `var(--xp-…)` variables,
  never hard-coded colors (the one accepted exception is `#fff`-on-accent matching
  `.xp-button`, as already used by `.link-embed-play`).

## Data flow

1. A message contains a Twitter/direct-video URL → `LinkEmbed` resolves the
   embed (existing path) → renders a compact poster (thumbnail + ▶).
2. Click ▶ → `openPip({ mediaUrl, title, mime })`.
3. `PipManager` dedupes/caps, adds a `PipPaneState`, bumps `z`.
4. `PipLayer` renders a `PipPane`; `useProxiedMedia(mediaUrl)` fetches the bytes
   via the relay → blob URL → `<video>` plays.
5. Drag/resize/opacity/minimize update the pane via `updatePip`; close removes it
   (decoder torn down, blob URL revoked by `useProxiedMedia` cleanup).

## Error handling & edge cases

- **Media fails to load** (`useProxiedMedia` → null): the pane shows a compact
  "Couldn't load video" state with a close button (not a dead black rectangle).
- **Duplicate open:** `openPip` dedupes by `mediaUrl` and focuses the existing
  pane instead of stacking a second copy.
- **Cap reached:** `openPip` at 4 panes is a no-op plus a toast ("Close a video
  to open another").
- **Blob-URL lifecycle:** unchanged from today — `useProxiedMedia` revokes the
  object URL on unmount, so closing a pane frees memory.
- **Persistence vs teardown:** panes persist across channel/server switches
  (they live in `AppShell`); they are NOT auto-closed on navigation. Closing is
  explicit (the ✕). (Open question deferred: whether to auto-close panes on full
  app lock/logout — default: leave them, they're harmless and re-fetch on demand.)

## Testing

- **`tsc` clean.**
- **`PipManager` unit-testable logic** (pure reducer/helpers): add a pane; dedupe
  by `mediaUrl` (second open focuses, doesn't duplicate); cap at 4 (5th is a
  no-op); close removes; `focusPip` assigns the top `z`. (No JS test runner in
  the repo today — structure the manager so its reducer is a pure function that a
  reviewer can verify by inspection, and add inline test-notes as other client
  libs do; if a runner is added later these become real tests.)
- **Runtime verification (Windows, per verify-before-done):** click ▶ on a tweet
  video → a floating pane plays; drag it, resize it, drag the opacity slider
  (chat readable behind it); open a second video → two panes, click toggles
  front; switch channels → panes persist; ✕ closes and frees it. UNVERIFIED
  until that run (WSL has no display).

## Out of scope (explicit)

- Detach-to-OS-window for PiP (that's the screenshare stage's mode; conflicts
  with see-through opacity).
- PiP for YouTube/Spotify (external open) or for still images (stay inline).
- An in-app mini-browser / real-site players (rejected — breaks the privacy
  model; see the embeds discussion).
- Snapping/tiling layout for multiple panes (free-floating only in v1).
- Persisting pane positions across app restarts.

## Documentation (same-commit discipline)

- New React components/context → the frontend module docs
  (`docs/modules/frontend-*.md`) catalog `PipManager`/`PipPane`/`PipLayer`.
- Note the `LinkEmbed` behavior change (inline video → poster + PiP) in the
  embed/voice-video docs where embeds are described.
- No `tauri-commands.md` / protocol / relay doc changes (client-only feature).
