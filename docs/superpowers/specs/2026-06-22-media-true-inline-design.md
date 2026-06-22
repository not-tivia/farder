# Media Players: True-Inline (in-place float) + Volume — Design Spec

**Date:** 2026-06-22
**Status:** Approved (architecture chosen with owner during debugging), pending plan
**Revises:** the just-shipped float/dock system (`2026-06-21-media-float-dock`). Keeps the registry context + reducer; replaces the root-overlay rendering with true-inline in-place rendering.

## Why (root cause from the debugging session)

Owner live-tested the shipped float/dock and hit two bugs:
1. **"Popped out on play."** The docked player wasn't really inline — it was a separate `position: fixed` element overlaying the card via JS rect-tracking, which looked detached/mis-aligned.
2. **"Scroll restarts the video."** The media element sat at a *different child index* in the docked vs floating render branches; with no keys, React tore down and rebuilt the `<video>`/`<iframe>` on every docked↔floating transition → restart.

Root cause: the **root-mounted overlay + per-scroll rect-tracking** architecture is fragile (this was the flagged risk). Owner chose the robust fix: render the player **truly inline** in the card.

Verified enabling fact: **no ancestor of the chat message subtree uses `transform`/`filter`/`contain`/`will-change`/`perspective`** (only toast keyframes + a 2px indicator bar use transform — neither is a chat ancestor). So a `position: fixed` element *inside a message* is positioned relative to the viewport, not a mis-placed ancestor. This makes in-place float viable.

## The architecture (in-place, no DOM move, no reload)

A media player is **rendered once inside its chat card** (in `MediaSlot`, in normal DOM flow) and **never re-parented**. "Docked", "floating", and "minimized" are just CSS/position states of that same container:

- **Docked:** the container is in normal flow inside the card → genuinely inline, native scrolling, no overlay, no tracking. Fixes "popped out."
- **Floating:** the *same* container gets `position: fixed` at a saved anchor (right of chat) with drag chrome. No DOM move → the `<video>`/`<iframe>` is not reloaded → **neither video nor YouTube restarts.**
- **Minimized:** the same container becomes a fixed pill; the media element stays mounted but hidden (`display:none`) so restore doesn't reload.

The media element lives in a single stable wrapper across all three states, so it is **never remounted** → no restart on scroll/float/dock/minimize. (Fixes both bugs at the root; no handoff, no blob re-fetch, no keys-hack needed.)

**Trade-off (owner-accepted):** the floating element lives in its message's DOM subtree, so it **does not persist across server/channel switches** (navigating away unmounts the message → closes the player). Scrolling within the channel is fine. (Cross-channel persistence would require the root-mount + handoff approach; explicitly out of scope.)

## Per-kind float behavior

- **Video (Twitter/X, direct file):** inline by default; **auto-floats when its card scrolls out of view** and **auto-docks** when it returns (via IntersectionObserver), plus a manual pop-out button. Because it's the same element, these transitions never restart it.
- **YouTube/Spotify iframe:** inline by default after "Watch here" consent; **does NOT auto-float on scroll** (a manual pop-out button is available). Rationale: an iframe is the same element so floating doesn't reload it, but auto-floating on every scroll-past is visually noisy for an embedded player; manual pop-out is the deliberate action. (This also means an iframe simply scrolls off-screen normally while continuing to play audio.)

## Volume

- New players default to **15% volume** (`0.15`).
- The chosen volume is **remembered** client-side (`localStorage` `farder.mediaVolume`): when the user changes a `<video>`'s volume, persist it; new players start at the remembered value (or `0.15` if none).
- Applies to `<video>` only (a cross-origin YouTube/Spotify iframe's volume isn't script-controllable).

## Components (revision map)

- `client/src/context/MediaPlayersContext.tsx` — **KEEP** the registry + reducer (players, `openPlayer`/`closePlayer`/`focusPlayer`/`updatePlayer`/`setPlayerState`, cap `MAX_PLAYERS=4`, dedupe, z, anchor seeding, `hostVisible` for video docked↔float). **REMOVE** the now-unused `hosts` ref-map + `registerHost`/`unregisterHost` + the `orphan` action + rect-tracking surface (no overlay → no rect-tracking, no orphan-persist).
- `client/src/components/MediaPlayersLayer.tsx` — **DELETE** (no root rendering; players render in their slots) + remove its mount in `AppShell.tsx`.
- `client/src/components/MediaPlayer.tsx` — **REWRITE** for in-place: one media element in a stable wrapper; container is inline (docked) / `position: fixed` at `pos`/`size` (floating) / fixed pill (minimized, media hidden); pointer-capture drag (floating); CSS `resize` persisted to the anchor; opacity slider; dock/minimize/close/pop-out controls; `<video>` volume default 0.15 + persist. No `scroll`/`resize` rect-tracking.
- `client/src/components/MediaSlot.tsx` — renders: ▶ poster (idle, non-`manualTrigger`), else `<MediaPlayer player={...}>`; an IntersectionObserver that flips **video** docked↔float (`setHostVisible`); when its player is floating/minimized, render a placeholder reserving the card's space + a "playing in a floating player — dock" chip; on unmount, `closePlayer(id)` (no persistence). Drop the `registerHost`/rect wiring.
- `client/src/lib/mediaPrefs.ts` (new, or append to `floatAnchor.ts`) — `getMediaVolume(): number` (default 0.15, fail-safe) / `setMediaVolume(v: number)`.
- `client/src/components/VoiceSettings.tsx` — the "always float" toggle stays.
- Themes ×3 — adjust the `.mp-*` classes for in-place (docked container has no fixed positioning; floating does). Remove any now-dead overlay-only styles.
- Docs — update `frontend-media-players.md` (in-place model; no root layer; channel-switch caveat) + `relay-embed.md` if needed.

## Data flow

1. Card renders `MediaSlot` (▶ poster / "Watch here").
2. ▶ / consent → `openPlayer({kind, src, hostId, title, float})`. Inline (docked) unless "always float".
3. `MediaSlot` renders `<MediaPlayer>` inline. Video: IntersectionObserver flips docked↔float on scroll; iframe: stays inline (manual pop-out only).
4. Float = same container → `position: fixed` (no reload); drag/resize update + persist anchor; opacity slider; volume persisted.
5. Close, or navigate away (message unmounts) → player removed from registry.

## Error handling & edge cases

- **Video fails to load:** "Couldn't load video" + close (as today).
- **Cap 4:** `openPlayer` no-op + toast.
- **Minimized:** media hidden via `display:none`, stays mounted (no reload on restore).
- **Channel switch while floating:** the player closes (documented limitation).
- **Future fragility:** if a future change adds `transform`/`filter`/`contain` to a chat ancestor, `position: fixed` float positioning would break — add a code comment + doc note warning.
- **No message virtualization assumed:** the chat renders loaded messages without windowing, so scrolling within a channel doesn't unmount a floating player's host. (If virtualization is added later, revisit.)

## Testing

- **`tsc` clean.**
- Pure reducer + `mediaPrefs`/anchor helpers keep inline test-notes.
- **Runtime (owner, Windows — UNVERIFIED until run):** ▶ a tweet video → plays **inline in the card** (not popped out); **scroll within the channel → it does NOT restart**; scroll the card off → it floats (right of chat) still playing; scroll back → docks; pop-out/dock buttons work; **drag, release the mouse outside the window → ends cleanly**; volume starts at 15% and a changed volume is remembered next time; minimize→restore doesn't restart; YouTube "Watch here" plays inline and **pop-out doesn't restart it**; switching channels closes a floating player (expected); 5th player → toast.

## Out of scope

- Cross-channel/server persistence of floating players (needs root-mount + handoff).
- Auto-float-on-scroll for iframes.
- Snapping/tiling; PiP for still images; relay-proxied YouTube (separate v2).
