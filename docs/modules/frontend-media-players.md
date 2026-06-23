# Inline-First Media Players (`MediaPlayersContext` / `MediaPlayer` / `MediaSlot`)

Video / YouTube / Spotify embeds play **inline** in the chat card by default and
can **float** (draggable mini-player) via a pop-out button or by auto-detaching
when their card scrolls out of view; they **auto-dock** back when the card
returns. Client-only.

## Core idea

A media element (`<video>` / `<iframe>`) reloads if moved in the DOM, so each
player owns its media element for its entire lifetime and **never re-parents it**.
Players render **inside their `MediaSlot`** (the in-card placeholder), not in a
separate root layer. The single `<video>` or `<iframe>` lives in `.mp-media-wrap`
and is present in every player state — dock, float, and minimize never unmount or
move it.

State is just CSS positioning toggled on the same container element:

- **Docked** — `position: absolute; inset: 0` filling the slot's aspect-ratio box.
  Looks native/inline; scrolls with the message like any chat attachment.
- **Floating** — the **same** container element toggled to `position: fixed`.
  No DOM move; media does not reload. The player becomes a draggable pip anywhere
  on screen.
- **Minimized** — a fixed pill; `.mp-media-wrap` is `display: none` (media stays
  mounted, does not reload).

## Pieces

- `client/src/context/MediaPlayersContext.tsx` — `MediaPlayersProvider` (wraps
  `<AppInner/>`), `useMediaPlayers()`, the pure `mediaPlayersReducer`. Exposes:
  `players`, `openPlayer`, `closePlayer`, `focusPlayer`, `updatePlayer`,
  `setPlayerState`, `setHostVisible`. `MAX_PLAYERS = 4`.
- `client/src/components/MediaPlayer.tsx` — the player container rendered inside
  its `MediaSlot`. Handles drag (pointer capture), opacity, dock/float/minimize/close
  controls, and CSS-transition state switches. Video via `useProxiedMedia`; iframe
  is the sandboxed embed (`referrerPolicy="origin"`).
- `client/src/components/MediaSlot.tsx` — in-card placeholder: reserves space,
  renders its own `MediaPlayer` child, runs an IntersectionObserver (docked ↔
  floating on scroll), shows ▶ or a "dock it back" chip while floating.
  `manualTrigger` suppresses ▶ for consent-gated iframe embeds. Closes its player
  on unmount (channel/server switch).
- `client/src/lib/mediaPrefs.ts` — `farder.mediaVolume` (default `0.15`, `<video>`
  only; remembered in `localStorage`). Iframe volume is not script-controllable.
- `client/src/lib/floatAnchor.ts` — remembered float position/size
  (`farder.floatAnchor`) + the "always float" pref (`farder.alwaysFloat`).

## Transitions

- **Scroll card out of view** → `MediaSlot`'s IntersectionObserver auto-floats the
  player (`autoFloated = true`). Applies to `<video>` only; `<iframe>`
  (YouTube/Spotify) does **not** auto-float — manual pop-out only.
- **Scroll card back into view** → auto-docks (if `autoFloated`).
- **Pop-out button** → float (not auto-docking).
- **"Always float" setting** → opens ▶ directly floating.
- **Channel/server switch** → `MediaSlot` unmounts → player closes. Floating
  players **do not persist** across server/channel navigation; they live with
  their message.

## Privacy

Video plays relay-proxied bytes (`useProxiedMedia`). The YouTube/Spotify iframe
is created only after the existing "Watch here" consent and keeps
`referrerPolicy="origin"` + sandbox.

## Limitations and warnings

- **No cross-navigation persistence.** Floating players are closed when their
  `MediaSlot` unmounts (channel/server switch). This is by design given the
  in-place model.
- **CSS stacking context caveat.** `position: fixed` float positioning is
  relative to the viewport only when no ancestor has a `transform`, `filter`, or
  `contain` property. Adding any of those to a chat ancestor (message list,
  channel panel, etc.) will break float positioning and must be avoided.
- **Max 4 concurrent players** (`MAX_PLAYERS = 4`). A toast is shown when the
  cap is hit.

## Verification

UNVERIFIED at runtime until the owner's Windows run (WSL has no display). Key
things to verify: video plays inline in the card; scroll out → floats without
restarting; scroll back → docks; volume starts at 15% and is remembered; minimize
→ restore does not restart; YouTube plays inline after "Watch here" consent;
channel switch closes a floating player; 5th player shows toast.
