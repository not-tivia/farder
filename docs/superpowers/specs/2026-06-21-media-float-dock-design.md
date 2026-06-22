# Inline-First Media Playback with Float/Dock — Design Spec

**Date:** 2026-06-21
**Status:** Approved (brainstorm), pending implementation plan
**Replaces:** the current PiP feature (`PipContext`/`PipPane`/`PipLayer` + the
`LinkEmbed` video poster→PiP flow) and extends the in-app embed player
(`EmbedConsentModal` + the YouTube/Spotify iframe in `LinkEmbed`).

## Problem

The shipped PiP feature made the floating frame the *default* action: clicking ▶
on a video opened a floating pane immediately. Owner feedback after live testing:

1. **PiP shouldn't be the default.** Video should play **inline** in the chat
   card. The floating frame should be a *transition* — entered by an explicit
   pop-out or by scrolling the playing card out of view — not the first thing
   that happens. Unless the user opts into "always float" in settings.
2. **The drag is clunky/buggy** — the pane keeps following the cursor after you
   release the mouse outside the window (the drag never ends).
3. The behavior should be consistent for **YouTube/Spotify** too (the in-app
   iframe player), and a floating video should sit **to the right of the chat**
   so it doesn't cover messages.

## Product decisions (owner, locked 2026-06-21)

- **Inline by default.** ▶ plays the media in the chat card. No autoplay.
- **Float is a transition**, entered by: (a) a **pop-out** button on the inline
  player, or (b) **auto-detach when the playing card scrolls out of view**.
- **Auto-dock both ways.** Scroll the card back into view → the player docks back
  into the card. Stray floating players don't linger.
- **Applies to all media:** Twitter/X video, direct video files, AND the
  YouTube/Spotify iframe player. The iframe must float **without restarting**.
- **Floating placement:** defaults to the **right of the chat column**;
  **remembers the position and size** the user last dragged/resized to
  (persisted client-side), used as the anchor next time. Opacity stays
  per-session.
- **Setting "Always play media in a floating player"** (default **OFF**): when
  on, ▶ opens directly as a floating player (the previous behavior).
- **Drag fixed** via pointer capture.
- Up to **4** floating players at once (toast beyond), as today.

## The core constraint that drives the architecture

Moving a `<video>` or `<iframe>` to a different place in the DOM **reloads it**
(restarts from 0; for a cross-origin YouTube iframe we also can't read/restore
the playback position). React portals don't help — changing a portal's container
is still a DOM move. Therefore, to "never restart," **a media player element must
never change DOM parent** for its whole life.

Combined with "floating players persist across channel/server navigation" (you
pop a video out, then browse other channels while it keeps playing), the player
**cannot live inside a message** (messages unmount on navigation). So:

> **Every active media player is mounted once, at the app root, and never moved.
> "Docked" and "floating" are just two ways the same root-level element is
> positioned.**

## Architecture

```
MediaPlayersProvider (context at app root — evolves PipContext)
   tracks: [{ id, kind: "video"|"iframe", src, hostId, state, pos, size, opacity, z }]
        hostId = the chat card slot this player belongs to (null once orphaned)
        state  = "docked" | "floating" | "minimized"

MediaPlayersLayer (rendered once in AppShell — evolves PipLayer)
   renders one MediaPlayer per active player, position: fixed, via portal-free
   root mount. Positioned by state:
     docked   → transform-tracked over its host card's on-screen slot
     floating → at the saved/dragged anchor (right-of-chat default)

LinkEmbed (in each message)
   renders a PLACEHOLDER slot (reserves the media's size) + ▶ Play.
   ▶ registers a player with the provider (hostId = this slot's id).
   The placeholder is the docking target: an IntersectionObserver + a
   ResizeObserver report its on-screen rect and visibility to the provider.
```

### Docking mechanics (the tricky part — flagged risk)

- The placeholder (a plain `<div>` in the card) holds the media's space so the
  message layout is correct whether the player is docked or floating.
- While **docked**, the root-level player is positioned to overlay the
  placeholder's rect, updated on scroll/resize via `requestAnimationFrame`
  (using `transform: translate()` + width/height for GPU-friendly updates). The
  player visually appears inline even though it lives at the root.
- An **IntersectionObserver** on the placeholder drives the transition: when the
  placeholder leaves the viewport while the player is docked-and-playing → set
  state `floating`; when it re-enters → set `docked`.
- **Risk + mitigation:** per-frame transform tracking of a docked player during
  fast scroll can lag. Mitigation: `transform`-based positioning + rAF
  throttling + only tracking players that are currently docked (floating ones
  don't track). This is the one genuine technical risk; if tracking proves janky
  in the Windows run, the fallback is to dock only when the card is near-centered
  and float otherwise (snappier, less continuous). The plan will implement the
  rAF-tracked version first and the owner verifies smoothness.
- **Orphaning:** if a docked player's host card unmounts (channel/server switch,
  message deleted) the placeholder's observer fires a disconnect → the provider
  sets the player `floating` (hostId = null). Because the element already lives
  at the root, this is a pure state change — **no reload** — so "switch channel
  while watching inline → it becomes a floating player" falls out naturally and
  satisfies persist-across-navigation.

### Floating mechanics

- `position: fixed` at `pos`, sized `size`, `opacity`, `z` (bring-to-front on
  click). Default `pos` = right of the chat column (right-aligned, below the top
  bar); overridden by the saved anchor.
- Chrome: header (drag handle), opacity slider, **dock button** (return to the
  card if its host still exists, else just a label), minimize (pill), close.
- **Drag = pointer capture.** `onPointerDown` on the header calls
  `setPointerCapture(e.pointerId)`; `pointermove` updates `pos` via rAF;
  `pointerup`/`pointercancel` releases. Because the element captures the pointer,
  releasing **outside the window** still ends the drag — fixing the current
  "keeps following the cursor" bug. Buttons/sliders are excluded from drag start.
- **Resize:** CSS `resize: both` persisted via `ResizeObserver` (as today).
- **Persistence:** on drag-end and resize-end, save `{ pos, size }` to
  `localStorage` key `farder.floatAnchor`. New floating players open at that
  anchor (cascaded if multiple are open). Read fails-safe to the right-of-chat
  default. Opacity is not persisted.

### Player element

- `kind: "video"` → `<video controls autoPlay>` fed by `useProxiedMedia(src)`
  (relay-proxied bytes, as today — privacy unchanged).
- `kind: "iframe"` → the YouTube/Spotify embed iframe (same sandboxed,
  `referrerPolicy="origin"` element shipped in the embed-player fix). Consent is
  unchanged: the iframe is only created after the user clicks "Watch here" and
  consent is satisfied (the `EmbedConsentModal` flow stays in `LinkEmbed`; once
  satisfied it registers an `iframe` player instead of rendering the iframe in
  the card directly).

## Components (refactor map)

- `client/src/context/MediaPlayersContext.tsx` (rename/evolve `PipContext.tsx`)
  — registry + actions: `openPlayer({kind, src, hostId, title})`,
  `closePlayer(id)`, `setState(id, "docked"|"floating"|"minimized")`,
  `focus(id)`, `update(id, patch)`, `reportHostRect(hostId, rect|null)`. Pure
  reducer core with inline test-notes (cap 4, dedupe by src+hostId, z-bump,
  anchor seeding).
- `client/src/components/MediaPlayer.tsx` (evolve `PipPane.tsx`) — the single
  root-mounted element; renders `<video>` or `<iframe>` by `kind`; docked vs
  floating positioning; pointer-capture drag; resize; chrome.
- `client/src/components/MediaPlayersLayer.tsx` (evolve `PipLayer.tsx`) — mounts
  all players once in `AppShell`.
- `client/src/components/MediaSlot.tsx` (new, used by `LinkEmbed`) — the in-card
  placeholder: reserves size, registers the host, drives the
  Intersection/Resize observers, shows ▶ when idle and a "playing in floating
  player — dock" affordance when its player is detached.
- `client/src/components/LinkEmbed.tsx` — video branch and the YouTube/Spotify
  "Watch here" branch both render a `MediaSlot` + register a player on play
  (instead of an inline `<video>` / inline `<iframe>` / the old PiP poster).
- `client/src/components/VoiceSettings.tsx` — add the "Always play media in a
  floating player" toggle (default off) in the Privacy & Data section.
- `client/src/lib/floatAnchor.ts` (new) — `getFloatAnchor()` / `setFloatAnchor()`
  over `localStorage` (`farder.floatAnchor`), fail-safe default = right of chat.
- `client/src/lib/embedPlayer.ts` — unchanged (still builds the iframe src +
  consent).
- Themes ×3 + docs.

## Data flow

1. Card renders a `MediaSlot` (placeholder + ▶). No media loaded yet.
2. ▶ (video) or "Watch here" + consent (iframe) → `openPlayer(...)` with
   `hostId` = the slot. Setting "always float" decides initial state
   (`docked` normally, `floating` if on).
3. `MediaPlayersLayer` renders the player at root; docked → tracks the slot.
4. Scroll slot out of view → `floating`; back into view → `docked`. Pop-out
   button → `floating`; dock button → `docked` (if host alive).
5. Host card unmounts → player goes `floating` (persists).
6. Drag/resize a floating player → saves `{pos,size}` anchor; close → unmount
   (revokes the video blob URL; drops the iframe → ends its connection).

## Error handling & edge cases

- **Video fails to load** (`useProxiedMedia` null): the player shows a compact
  "Couldn't load video" with a close button.
- **Cap reached (4):** `openPlayer` is a no-op + toast ("Close a player to open
  another").
- **Dedupe:** opening the same `src` from the same host re-focuses the existing
  player rather than duplicating.
- **iframe + restart:** never re-parented, so floating/docking never reloads it
  (the whole point of the root-mount architecture).
- **`localStorage` blocked:** `getFloatAnchor` returns the right-of-chat default;
  `setFloatAnchor` swallows errors.
- **Some YouTube videos still show "Watch on YouTube":** owner-side embedding
  disabled by the video owner — not fixable; the external-open link remains.

## Testing

- **`tsc` clean.**
- **Pure reducer + `floatAnchor.ts`** carry inline test-notes (cap, dedupe,
  z-bump, anchor get/set round-trip, fail-safe default) — no JS test runner in
  the repo.
- **Runtime (Windows, per verify-before-done — UNVERIFIED until then; WSL has no
  display):** ▶ a tweet video → plays inline in the card; scroll down → it floats
  to the right of chat and keeps playing; scroll back → docks into the card;
  pop-out button floats it on demand; drag it and **release the mouse outside the
  window** → drag ends cleanly (no runaway); resize + reopen another video →
  opens at the saved position/size; switch channels while floating → keeps
  playing; open a YouTube "Watch here" → same float/dock without restarting;
  turn on "Always float" → ▶ opens floating directly; 5th player → toast.
  Confirm docked scroll-tracking is smooth.

## Out of scope (explicit)

- Snapping/tiling layouts; multiple-monitor/OS-detached windows (that's the
  screenshare stage's mode).
- Persisting players across full app restart (only the anchor persists).
- PiP for still images.
- Relay-proxied (Invidious-style) YouTube — still a separate possible v2.

## Documentation (same-commit discipline)

- Replace `docs/modules/frontend-pip.md` with a doc for the new
  context/components (or rename to `frontend-media-players.md`).
- Update `docs/modules/relay-embed.md` (the LinkEmbed media rendering now goes
  through `MediaSlot` + the root player layer; inline-default; float/dock).
- Client-only — no `tauri-commands.md` / protocol / relay changes.
