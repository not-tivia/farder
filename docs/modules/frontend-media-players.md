# Inline-First Media Players (`MediaPlayersContext` / `MediaPlayer` / `MediaPlayersLayer` / `MediaSlot`)

Video / YouTube / Spotify embeds play **inline** in the chat card by default and
can **float** (draggable mini-player) via a pop-out button or by auto-detaching
when their card scrolls out of view; they **auto-dock** back when the card
returns. Client-only.

## Core idea
A media element (`<video>` / `<iframe>`) reloads if moved in the DOM, so every
player is mounted **once at the app root** (`MediaPlayersLayer` in `AppShell`)
and never re-parented. "Docked" and "floating" are two ways of positioning that
same `position: fixed` element.

## Pieces
- `client/src/context/MediaPlayersContext.tsx` — `MediaPlayersProvider` (wraps
  `<AppInner/>`), `useMediaPlayers()`, the pure `mediaPlayersReducer`, and a
  `hosts` ref-map (hostId → slot element). State per player: id, kind, src,
  hostId, state (docked/floating/minimized), autoFloated, pos, size, opacity, z.
  `MAX_PLAYERS = 4`.
- `client/src/components/MediaPlayer.tsx` — the root element. Docked: tracks its
  host slot's rect on scroll/resize (rAF) so it overlays the card. Floating:
  header (drag via **pointer capture**, opacity, dock, minimize, close), CSS
  `resize`. Video via `useProxiedMedia`; iframe is the sandboxed embed.
- `client/src/components/MediaPlayersLayer.tsx` — renders all players once.
- `client/src/components/MediaSlot.tsx` — in-card placeholder: reserves space,
  registers its element, runs an IntersectionObserver (docked↔floating), shows ▶
  (or a "dock it back" chip while floating). `manualTrigger` suppresses the ▶ for
  consent-gated iframe embeds (the parent's "Watch here" triggers those).
- `client/src/lib/floatAnchor.ts` — remembered float position/size
  (`farder.floatAnchor`) + the "always float" pref (`farder.alwaysFloat`).

## Transitions
Scroll card out of view → float (autoFloated); scroll back → dock. Pop-out → float
(not auto-docking). Host card unmounts (channel switch) → orphan to floating
(persists). "Always float" setting opens ▶ directly floating.

## Privacy
Video plays relay-proxied bytes (`useProxiedMedia`). The YouTube/Spotify iframe
is created only after the existing "Watch here" consent and keeps
`referrerPolicy="origin"` + sandbox.

## Verification
UNVERIFIED at runtime until the owner's Windows run (WSL has no display). The
docked scroll-tracking smoothness is the thing to verify first.
