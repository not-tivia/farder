# Inline-First Media Playback with Float/Dock — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make video / YouTube / Spotify embeds play **inline by default** in the chat card, with the player able to **float** (draggable mini-player) via an explicit pop-out or by auto-detaching when its card scrolls out of view, and **auto-dock** back — all without ever reloading the media (so a YouTube iframe floats without restarting), with a remembered float position/size and an opt-in "always float" setting.

**Architecture:** Every active media player is mounted **once at the app root** (a `MediaPlayersLayer` in `AppShell`) and never re-parented — "docked" and "floating" are two ways of positioning that same `position: fixed` element. A `MediaSlot` placeholder in each chat card reserves space, registers its element, and (via IntersectionObserver) drives docked↔floating transitions; while docked, the root player tracks the slot's on-screen rect on scroll/resize (rAF-coalesced). This replaces the old PiP system (`PipContext`/`PipPane`/`PipLayer`).

**Tech Stack:** React 18 + TypeScript, Tauri, existing `useProxiedMedia` hook, the existing `toast` helper, per-theme CSS.

## Global Constraints

- **Client-only.** No Rust/relay/protocol/Tauri-command changes. Player state + float anchor live in React state / `localStorage`.
- **Never re-parent a player element.** Moving a `<video>`/`<iframe>` in the DOM reloads it; the whole design exists to avoid that. Each player is mounted once at the root and positioned via CSS.
- **No JS test runner.** "Tests" = `cd client && npx tsc --noEmit` clean + pure reducer/helpers carry inline test-notes verified by inspection (mirroring `detectEmbedUrls` in `client/src/lib/linkEmbed.ts`). Do not add a test runner.
- **Privacy unchanged.** Video plays relay-proxied bytes via `useProxiedMedia` (no new external connections). The YouTube/Spotify iframe is created only after the existing "Watch here" consent (`EmbedConsentModal`); it keeps `referrerPolicy="origin"`, `sandbox="allow-scripts allow-same-origin allow-presentation allow-popups"`, `allow="autoplay; encrypted-media; fullscreen; picture-in-picture"`, `loading="lazy"`, `allowFullScreen` (the shipped values — do not regress to `no-referrer`).
- **Theming:** new classes in ALL THREE themes (`discord-dark`, `hello-kitty`, `xp-luna-blue`), variable-driven, no hard-coded colors except `#000` (video letterbox) / `#fff`-on-`--xp-blue`. `xp-luna-blue` lacks `--xp-text-normal` → use `var(--xp-text-normal, var(--xp-text-secondary))`.
- **Constants:** `MAX_PLAYERS = 4` (toast beyond); z-base `300` (above ScreenShareStage's 200); float-anchor `localStorage` key `farder.floatAnchor`; default float anchor = right of chat.
- **Spec:** `docs/superpowers/specs/2026-06-21-media-float-dock-design.md`.

## Shared types (used across tasks — defined in Task 2, repeated here for reference)

```ts
export type PlayerKind = "video" | "iframe";
export type PlayerVisualState = "docked" | "floating" | "minimized";
export interface MediaPlayerInfo {
  id: string;
  kind: PlayerKind;
  src: string;              // video: relay media URL; iframe: embed src
  title: string;
  hostId: string | null;    // the MediaSlot this belongs to; null once orphaned
  state: PlayerVisualState;
  autoFloated: boolean;     // true only when floated BY SCROLL (so it can auto-dock back)
  pos: { x: number; y: number };
  size: { w: number; h: number };
  opacity: number;
  z: number;
}
export interface OpenPlayerInput { kind: PlayerKind; src: string; hostId: string; title?: string; float?: boolean }
export type PlayerPatch = Partial<Pick<MediaPlayerInfo, "pos" | "size" | "opacity">>;
```

## File Structure

- `client/src/lib/floatAnchor.ts` (new) — `localStorage` float anchor get/set.
- `client/src/context/MediaPlayersContext.tsx` (new; replaces `PipContext.tsx`) — registry reducer + provider + `useMediaPlayers()` + host element ref-map.
- `client/src/components/MediaPlayer.tsx` (new; replaces `PipPane.tsx`) — the single root-mounted player (video/iframe; docked rect-tracking; floating chrome; pointer-capture drag; resize).
- `client/src/components/MediaPlayersLayer.tsx` (new; replaces `PipLayer.tsx`) — mounts all players in `AppShell`.
- `client/src/components/MediaSlot.tsx` (new) — in-card placeholder (▶ / "playing in floating player" chip; registers host; IntersectionObserver).
- `client/src/components/LinkEmbed.tsx` (modify) — video + YouTube/Spotify branches use `MediaSlot` and register players.
- `client/src/components/AppShell.tsx` (modify) — mount `MediaPlayersLayer`.
- `client/src/App.tsx` (modify) — wrap in `MediaPlayersProvider`.
- `client/src/components/VoiceSettings.tsx` (modify) — "Always float" toggle.
- `client/src/themes/*/theme.css` (modify) — new classes.
- `docs/modules/frontend-pip.md` → rename/replace with `frontend-media-players.md`; `docs/modules/relay-embed.md` (modify).
- Delete: `PipContext.tsx`, `PipPane.tsx`, `PipLayer.tsx` (in the tasks that remove their last usage).

---

### Task 1: `floatAnchor.ts` — remembered float position/size

**Files:**
- Create: `client/src/lib/floatAnchor.ts`

**Interfaces:**
- Produces: `interface FloatAnchor { x: number; y: number; w: number; h: number }`, `function getFloatAnchor(): FloatAnchor`, `function setFloatAnchor(a: FloatAnchor): void`, `const DEFAULT_ANCHOR_FALLBACK` (used only if window is unavailable).

- [ ] **Step 1: Write the module with test-notes**

Create `client/src/lib/floatAnchor.ts`:

```ts
// Remembered floating-player placement, persisted client-side. Default anchors
// to the right of the chat column so a floating player doesn't cover messages.
// Fails safe (returns the default) on any storage error.

export interface FloatAnchor { x: number; y: number; w: number; h: number }

const KEY = "farder.floatAnchor";
const SIZE = { w: 360, h: 240 };

/** Default = upper-right area of the viewport (right of the chat column). */
function defaultAnchor(): FloatAnchor {
  const vw = typeof window !== "undefined" ? window.innerWidth : 1280;
  return { x: Math.max(16, vw - SIZE.w - 32), y: 88, w: SIZE.w, h: SIZE.h };
}

/**
 * Read the saved anchor, or the right-of-chat default.
 * Test-notes (verified by inspection):
 *   - nothing saved            → defaultAnchor() (x near right edge, y 88)
 *   - saved {x,y,w,h} valid    → that object
 *   - saved malformed/partial  → defaultAnchor() (validation rejects it)
 *   - localStorage throws       → defaultAnchor() (caught)
 */
export function getFloatAnchor(): FloatAnchor {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return defaultAnchor();
    const a = JSON.parse(raw) as Partial<FloatAnchor>;
    if (
      typeof a.x === "number" && typeof a.y === "number" &&
      typeof a.w === "number" && typeof a.h === "number" &&
      a.w > 80 && a.h > 60
    ) return { x: a.x, y: a.y, w: a.w, h: a.h };
    return defaultAnchor();
  } catch { return defaultAnchor(); }
}

/** Persist the anchor; swallows storage errors. */
export function setFloatAnchor(a: FloatAnchor): void {
  try { localStorage.setItem(KEY, JSON.stringify(a)); } catch { /* ignore */ }
}
```

- [ ] **Step 2: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 3: Trace test-notes by hand** — confirm the four cases. Fix any mismatch.

- [ ] **Step 4: Commit**

```bash
git add client/src/lib/floatAnchor.ts
git commit -m "feat(media-players): remembered float anchor (localStorage, fail-safe default)"
```

---

### Task 2: `MediaPlayersContext` — registry, reducer, provider, host ref-map

**Files:**
- Create: `client/src/context/MediaPlayersContext.tsx`
- Modify: `client/src/App.tsx` (swap `PipProvider` → `MediaPlayersProvider`)

**Interfaces:**
- Consumes: `getFloatAnchor` from `client/src/lib/floatAnchor`; `toast` from `client/src/lib/toast`.
- Produces (used by Tasks 3–6): the shared types above plus
  `useMediaPlayers()` returning
  `{ players: MediaPlayerInfo[]; hosts: React.MutableRefObject<Map<string, HTMLElement>>; openPlayer(input): void; closePlayer(id): void; focusPlayer(id): void; updatePlayer(id, patch): void; setPlayerState(id, state: PlayerVisualState): void; registerHost(hostId, el): void; unregisterHost(hostId): void; setHostVisible(hostId, visible: boolean): void }`,
  and `mediaPlayersReducer` (exported, pure).

- [ ] **Step 1: Write the context module with a pure reducer + test-notes**

Create `client/src/context/MediaPlayersContext.tsx`:

```tsx
import { createContext, useContext, useReducer, useRef, useEffect, useCallback, ReactNode } from "react";
import { toast } from "../lib/toast";
import { getFloatAnchor } from "../lib/floatAnchor";

export const MAX_PLAYERS = 4;
const BASE_Z = 300; // above ScreenShareStage (z-index: 200)

export type PlayerKind = "video" | "iframe";
export type PlayerVisualState = "docked" | "floating" | "minimized";

export interface MediaPlayerInfo {
  id: string;
  kind: PlayerKind;
  src: string;
  title: string;
  hostId: string | null;
  state: PlayerVisualState;
  autoFloated: boolean;
  pos: { x: number; y: number };
  size: { w: number; h: number };
  opacity: number;
  z: number;
}

export interface OpenPlayerInput { kind: PlayerKind; src: string; hostId: string; title?: string; float?: boolean }
export type PlayerPatch = Partial<Pick<MediaPlayerInfo, "pos" | "size" | "opacity">>;

interface State { players: MediaPlayerInfo[]; nextZ: number; nextId: number }

type Action =
  | { type: "open"; input: OpenPlayerInput; anchor: { x: number; y: number; w: number; h: number } }
  | { type: "close"; id: string }
  | { type: "focus"; id: string }
  | { type: "update"; id: string; patch: PlayerPatch }
  | { type: "setState"; id: string; state: PlayerVisualState; autoFloated?: boolean }
  | { type: "hostVisible"; hostId: string; visible: boolean }
  | { type: "orphan"; hostId: string };

export const initialState: State = { players: [], nextZ: BASE_Z, nextId: 1 };

/**
 * Pure reducer — the testable core. No side effects (the over-cap toast lives in
 * the provider). Cap + dedupe are enforced here as a backstop.
 *
 * Test-notes (verified by inspection):
 *   - open (float=false) into empty → 1 player, id "mp-1", state "docked", z 300, nextId 2
 *   - open float=true               → state "floating", pos/size from anchor
 *   - open dup (same kind+src+hostId)→ no new player; existing focused (top z)
 *   - open 5th distinct             → unchanged (cap 4)
 *   - close id                      → removed
 *   - focus id                      → top z
 *   - update {pos}                  → only that player's pos
 *   - setState id "floating"        → that player floating (autoFloated as given, default false)
 *   - hostVisible(host,false) when docked → that player floating + autoFloated=true
 *   - hostVisible(host,true) when floating&autoFloated → docked + autoFloated=false
 *   - hostVisible(host,true) when floating&!autoFloated (popped out) → unchanged
 *   - orphan(host) → that player's hostId=null, state floating (if was docked), autoFloated=false
 */
export function mediaPlayersReducer(state: State, action: Action): State {
  switch (action.type) {
    case "open": {
      const { kind, src, hostId } = action.input;
      const existing = state.players.find((p) => p.kind === kind && p.src === src && p.hostId === hostId);
      if (existing) {
        return { ...state, nextZ: state.nextZ + 1, players: state.players.map((p) => p.id === existing.id ? { ...p, z: state.nextZ } : p) };
      }
      if (state.players.length >= MAX_PLAYERS) return state;
      const floating = !!action.input.float;
      const n = state.players.length;
      const a = action.anchor;
      const p: MediaPlayerInfo = {
        id: `mp-${state.nextId}`,
        kind, src,
        title: action.input.title ?? (kind === "video" ? "Video" : "Player"),
        hostId,
        state: floating ? "floating" : "docked",
        autoFloated: false,
        pos: { x: a.x + ((n * 28) % 140), y: a.y + ((n * 28) % 140) },
        size: { w: a.w, h: a.h },
        opacity: 1,
        z: state.nextZ,
      };
      return { players: [...state.players, p], nextZ: state.nextZ + 1, nextId: state.nextId + 1 };
    }
    case "close":
      return { ...state, players: state.players.filter((p) => p.id !== action.id) };
    case "focus":
      return { ...state, nextZ: state.nextZ + 1, players: state.players.map((p) => p.id === action.id ? { ...p, z: state.nextZ } : p) };
    case "update":
      return { ...state, players: state.players.map((p) => p.id === action.id ? { ...p, ...action.patch } : p) };
    case "setState":
      return { ...state, players: state.players.map((p) => p.id === action.id ? { ...p, state: action.state, autoFloated: action.autoFloated ?? false } : p) };
    case "hostVisible":
      return {
        ...state,
        players: state.players.map((p) => {
          if (p.hostId !== action.hostId || p.state === "minimized") return p;
          if (!action.visible && p.state === "docked") return { ...p, state: "floating", autoFloated: true };
          if (action.visible && p.state === "floating" && p.autoFloated) return { ...p, state: "docked", autoFloated: false };
          return p;
        }),
      };
    case "orphan":
      return {
        ...state,
        players: state.players.map((p) => p.hostId === action.hostId
          ? { ...p, hostId: null, autoFloated: false, state: p.state === "docked" ? "floating" : p.state }
          : p),
      };
    default:
      return state;
  }
}

interface CtxValue {
  players: MediaPlayerInfo[];
  hosts: React.MutableRefObject<Map<string, HTMLElement>>;
  openPlayer: (input: OpenPlayerInput) => void;
  closePlayer: (id: string) => void;
  focusPlayer: (id: string) => void;
  updatePlayer: (id: string, patch: PlayerPatch) => void;
  setPlayerState: (id: string, state: PlayerVisualState) => void;
  registerHost: (hostId: string, el: HTMLElement) => void;
  unregisterHost: (hostId: string) => void;
  setHostVisible: (hostId: string, visible: boolean) => void;
}

const Ctx = createContext<CtxValue | null>(null);

export function MediaPlayersProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(mediaPlayersReducer, initialState);
  const stateRef = useRef(state);
  useEffect(() => { stateRef.current = state; }, [state]);
  const hosts = useRef<Map<string, HTMLElement>>(new Map());

  const openPlayer = useCallback((input: OpenPlayerInput) => {
    const s = stateRef.current;
    const dup = s.players.some((p) => p.kind === input.kind && p.src === input.src && p.hostId === input.hostId);
    if (!dup && s.players.length >= MAX_PLAYERS) { toast.info("Close a player to open another"); return; }
    dispatch({ type: "open", input, anchor: getFloatAnchor() });
  }, []);
  const closePlayer = useCallback((id: string) => dispatch({ type: "close", id }), []);
  const focusPlayer = useCallback((id: string) => dispatch({ type: "focus", id }), []);
  const updatePlayer = useCallback((id: string, patch: PlayerPatch) => dispatch({ type: "update", id, patch }), []);
  const setPlayerState = useCallback((id: string, st: PlayerVisualState) => dispatch({ type: "setState", id, state: st }), []);
  const registerHost = useCallback((hostId: string, el: HTMLElement) => { hosts.current.set(hostId, el); }, []);
  const unregisterHost = useCallback((hostId: string) => { hosts.current.delete(hostId); dispatch({ type: "orphan", hostId }); }, []);
  const setHostVisible = useCallback((hostId: string, visible: boolean) => dispatch({ type: "hostVisible", hostId, visible }), []);

  return (
    <Ctx.Provider value={{ players: state.players, hosts, openPlayer, closePlayer, focusPlayer, updatePlayer, setPlayerState, registerHost, unregisterHost, setHostVisible }}>
      {children}
    </Ctx.Provider>
  );
}

export function useMediaPlayers(): CtxValue {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useMediaPlayers must be used within a MediaPlayersProvider");
  return ctx;
}
```

- [ ] **Step 2: Swap the provider in `App.tsx`**

In `client/src/App.tsx`, replace the `PipProvider` import line:
```tsx
import { PipProvider } from "./context/PipContext";
```
with:
```tsx
import { MediaPlayersProvider } from "./context/MediaPlayersContext";
```
and replace the `<PipProvider>` wrapper around `<AppInner />` with `<MediaPlayersProvider>`:
```tsx
    <AppProvider>
      <MediaPlayersProvider>
        <AppInner />
      </MediaPlayersProvider>
      <ToastContainer />
    </AppProvider>
```

- [ ] **Step 3: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: errors ONLY in `PipLayer.tsx`/`PipPane.tsx`/`LinkEmbed.tsx` that still import `usePip` (they're replaced in later tasks). The new file + App.tsx must compile. If errors appear outside those three files, fix them. (Note: `PipContext.tsx` still exists, so `usePip` still resolves for now — there should be no errors yet. Confirm.)

- [ ] **Step 4: Trace the reducer test-notes by hand.** Confirm each, especially the `hostVisible` and `orphan` transitions. Fix any mismatch.

- [ ] **Step 5: Commit**

```bash
git add client/src/context/MediaPlayersContext.tsx client/src/App.tsx
git commit -m "feat(media-players): registry context + reducer (docked/floating/orphan), wrap app"
```

---

### Task 3: `MediaPlayer` — the root-mounted player element

**Files:**
- Create: `client/src/components/MediaPlayer.tsx`

**Interfaces:**
- Consumes: `MediaPlayerInfo`, `useMediaPlayers` from `client/src/context/MediaPlayersContext`; `useProxiedMedia` from `client/src/hooks/useProxiedMedia`; `setFloatAnchor` from `client/src/lib/floatAnchor`.
- Produces (used by Task 4): `export default function MediaPlayer({ player }: { player: MediaPlayerInfo })`.

This is the hardest task. The element is `position: fixed`. When **docked**, it tracks its host slot's rect on scroll/resize (rAF-coalesced) and shows just the media + a small pop-out button. When **floating**, it shows full chrome (drag header, opacity, dock, minimize, close) at `pos`/`size`. **Minimized** is a pill. Drag uses pointer capture (fixes the runaway-drag bug).

- [ ] **Step 1: Write the component**

Create `client/src/components/MediaPlayer.tsx`:

```tsx
import { useEffect, useRef } from "react";
import { useProxiedMedia } from "../hooks/useProxiedMedia";
import { setFloatAnchor } from "../lib/floatAnchor";
import { useMediaPlayers, type MediaPlayerInfo } from "../context/MediaPlayersContext";

export default function MediaPlayer({ player }: { player: MediaPlayerInfo }) {
  const { hosts, focusPlayer, updatePlayer, setPlayerState, closePlayer } = useMediaPlayers();
  const rootRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ dx: number; dy: number; raf: number } | null>(null);

  // Video bytes via the relay (iframe needs no proxy). Don't fetch while minimized.
  const videoUrl = useProxiedMedia(
    player.kind === "video" ? player.src : null,
    player.kind === "video" && player.state !== "minimized",
  );

  // DOCKED: track the host slot's rect so the fixed element overlays it (looks inline).
  useEffect(() => {
    if (player.state !== "docked") return;
    let raf = 0;
    const place = () => {
      raf = 0;
      const host = player.hostId ? hosts.current.get(player.hostId) : null;
      const root = rootRef.current;
      if (!host || !root) return;
      const r = host.getBoundingClientRect();
      root.style.transform = `translate(${r.left}px, ${r.top}px)`;
      root.style.width = `${r.width}px`;
      root.style.height = `${r.height}px`;
    };
    const schedule = () => { if (!raf) raf = requestAnimationFrame(place); };
    place();
    window.addEventListener("scroll", schedule, true); // capture: catch the .message-list scroll
    window.addEventListener("resize", schedule);
    return () => {
      window.removeEventListener("scroll", schedule, true);
      window.removeEventListener("resize", schedule);
      if (raf) cancelAnimationFrame(raf);
    };
  }, [player.state, player.hostId, hosts]);

  // FLOATING/ minimized drag via pointer capture (release outside the window still ends it).
  const startDrag = (e: React.PointerEvent) => {
    if ((e.target as HTMLElement).closest("button, input")) return;
    const root = rootRef.current;
    if (!root) return;
    const rect = root.getBoundingClientRect();
    dragRef.current = { dx: e.clientX - rect.left, dy: e.clientY - rect.top, raf: 0 };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  };
  const onDragMove = (e: React.PointerEvent) => {
    const d = dragRef.current;
    if (!d) return;
    const x = e.clientX - d.dx, y = e.clientY - d.dy;
    if (!d.raf) d.raf = requestAnimationFrame(() => { d.raf = 0; updatePlayer(player.id, { pos: { x, y } }); });
  };
  const endDrag = (e: React.PointerEvent) => {
    const d = dragRef.current;
    if (!d) return;
    if (d.raf) cancelAnimationFrame(d.raf);
    dragRef.current = null;
    try { (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId); } catch { /* ignore */ }
    setFloatAnchor({ x: player.pos.x, y: player.pos.y, w: player.size.w, h: player.size.h });
  };

  // Persist size after a CSS resize (floating only).
  useEffect(() => {
    if (player.state !== "floating") return;
    const el = rootRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      const w = el.offsetWidth, h = el.offsetHeight;
      if (w && h && (w !== player.size.w || h !== player.size.h)) {
        updatePlayer(player.id, { size: { w, h } });
        setFloatAnchor({ x: player.pos.x, y: player.pos.y, w, h });
      }
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [player.state, player.id]); // eslint-disable-line react-hooks/exhaustive-deps

  const media = player.kind === "video"
    ? (videoUrl
        ? <video className="mp-media" src={videoUrl} controls autoPlay />
        : <div className="mp-state">Couldn&rsquo;t load video</div>)
    : <iframe
        className="mp-media"
        src={player.src}
        title={player.title}
        sandbox="allow-scripts allow-same-origin allow-presentation allow-popups"
        referrerPolicy="origin"
        allow="autoplay; encrypted-media; fullscreen; picture-in-picture"
        loading="lazy"
        allowFullScreen
      />;

  // MINIMIZED: pill
  if (player.state === "minimized") {
    return (
      <div className="mp-mini" style={{ left: player.pos.x, top: player.pos.y, zIndex: player.z }}
           onPointerDown={(e) => { focusPlayer(player.id); startDrag(e); }} onPointerMove={onDragMove} onPointerUp={endDrag} onPointerCancel={endDrag}>
        <span className="mp-title">{player.title}</span>
        <button className="mp-btn" title="Restore" onClick={() => setPlayerState(player.id, "floating")}>&#x25A2;</button>
        <button className="mp-btn" title="Close" onClick={() => closePlayer(player.id)}>&#x2715;</button>
      </div>
    );
  }

  // DOCKED: fixed element overlaying the slot; minimal chrome (pop-out only).
  if (player.state === "docked") {
    return (
      <div ref={rootRef} className="mp-docked" style={{ position: "fixed", left: 0, top: 0, zIndex: player.z }} onMouseDown={() => focusPlayer(player.id)}>
        {media}
        <button className="mp-pop" title="Pop out" onClick={() => setPlayerState(player.id, "floating")}>&#x2197;</button>
      </div>
    );
  }

  // FLOATING: full chrome.
  return (
    <div ref={rootRef} className="mp-float"
         style={{ left: player.pos.x, top: player.pos.y, width: player.size.w, height: player.size.h, opacity: player.opacity, zIndex: player.z }}
         onMouseDown={() => focusPlayer(player.id)}>
      <div className="mp-head" onPointerDown={startDrag} onPointerMove={onDragMove} onPointerUp={endDrag} onPointerCancel={endDrag}>
        <span className="mp-title">{player.title}</span>
        <input className="mp-opacity" type="range" min={0.2} max={1} step={0.05} value={player.opacity}
               title="Opacity" onChange={(e) => updatePlayer(player.id, { opacity: Number(e.target.value) })} />
        {player.hostId && <button className="mp-btn" title="Dock back into chat" onClick={() => setPlayerState(player.id, "docked")}>&#x21F2;</button>}
        <button className="mp-btn" title="Minimize" onClick={() => setPlayerState(player.id, "minimized")}>&#x2013;</button>
        <button className="mp-btn" title="Close" onClick={() => closePlayer(player.id)}>&#x2715;</button>
      </div>
      {media}
    </div>
  );
}
```

- [ ] **Step 2: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: **clean.** The old PiP files still resolve against the still-present `PipContext.tsx`, so there are no errors; `MediaPlayer.tsx` compiles. (Classes are styled in Task 8 — unstyled-for-now is expected and doesn't affect tsc.)

- [ ] **Step 3: Self-check the drag + tracking** — confirm: pointer capture set on `startDrag` and released on `endDrag`/cancel; `onPointerUp`/`onPointerCancel` both call `endDrag`; the docked effect adds/removes scroll(capture)+resize listeners and cancels its rAF on cleanup; `useProxiedMedia` gated off when minimized.

- [ ] **Step 4: Commit**

```bash
git add client/src/components/MediaPlayer.tsx
git commit -m "feat(media-players): MediaPlayer root element (docked tracking, floating chrome, pointer-capture drag)"
```

---

### Task 4: `MediaPlayersLayer` + mount in AppShell; delete old PipLayer

**Files:**
- Create: `client/src/components/MediaPlayersLayer.tsx`
- Modify: `client/src/components/AppShell.tsx`
- Delete: `client/src/components/PipLayer.tsx`

**Interfaces:**
- Consumes: `useMediaPlayers`; `MediaPlayer`.
- Produces: `export default function MediaPlayersLayer()`.

- [ ] **Step 1: Write the layer**

Create `client/src/components/MediaPlayersLayer.tsx`:

```tsx
import { useMediaPlayers } from "../context/MediaPlayersContext";
import MediaPlayer from "./MediaPlayer";

// Renders every active media player once, at the app-root overlay level, so a
// player is never re-parented (no reload) and floating players persist across
// channel/server navigation.
export default function MediaPlayersLayer() {
  const { players } = useMediaPlayers();
  if (players.length === 0) return null;
  return <>{players.map((p) => <MediaPlayer key={p.id} player={p} />)}</>;
}
```

- [ ] **Step 2: Swap the mount in `AppShell.tsx`**

Replace the import `import PipLayer from "./PipLayer";` with `import MediaPlayersLayer from "./MediaPlayersLayer";`, and the element `<PipLayer />` with `<MediaPlayersLayer />` (it sits next to `<ScreenShareStage voice={voice} />`).

- [ ] **Step 3: Delete the old layer**

```bash
git rm client/src/components/PipLayer.tsx
```

- [ ] **Step 4: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: **clean.** `PipPane.tsx` is now orphaned (imported by nothing) but still compiles against the still-present `PipContext.tsx`; `LinkEmbed.tsx` still uses the existing `PipContext`. No reference to `PipLayer` remains.

- [ ] **Step 5: Commit**

```bash
git add client/src/components/MediaPlayersLayer.tsx client/src/components/AppShell.tsx
git commit -m "feat(media-players): MediaPlayersLayer mounted at AppShell; remove PipLayer"
```

---

### Task 5: `MediaSlot` — in-card placeholder, host registration, visibility observer

**Files:**
- Create: `client/src/components/MediaSlot.tsx`

**Interfaces:**
- Consumes: `useMediaPlayers` (`registerHost`, `unregisterHost`, `setHostVisible`, `openPlayer`, `players`).
- Produces (used by Task 6): `export default function MediaSlot(props: { hostId: string; kind: PlayerKind; src: string; title: string; thumbUrl?: string | null; aspect?: number })`.

The slot reserves space, shows a ▶ poster when idle, registers its element, runs an IntersectionObserver that calls `setHostVisible`, and shows a "playing in a floating player — dock" chip when its player is detached.

- [ ] **Step 1: Write the component**

Create `client/src/components/MediaSlot.tsx`:

```tsx
import { useEffect, useRef } from "react";
import { useMediaPlayers, type PlayerKind } from "../context/MediaPlayersContext";

export default function MediaSlot({
  hostId, kind, src, title, thumbUrl, aspect = 0.5625,
}: { hostId: string; kind: PlayerKind; src: string; title: string; thumbUrl?: string | null; aspect?: number }) {
  const { players, registerHost, unregisterHost, setHostVisible, openPlayer, setPlayerState } = useMediaPlayers();
  const ref = useRef<HTMLDivElement>(null);

  const player = players.find((p) => p.hostId === hostId);
  const docked = player?.state === "docked";

  // Register this slot's element + observe visibility for docked<->floating.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    registerHost(hostId, el);
    const io = new IntersectionObserver(
      (entries) => { for (const e of entries) setHostVisible(hostId, e.isIntersecting); },
      { threshold: 0.5 },
    );
    io.observe(el);
    return () => { io.disconnect(); unregisterHost(hostId); };
  }, [hostId]); // eslint-disable-line react-hooks/exhaustive-deps

  // The slot reserves the media's space (so the docked player overlays it and the
  // layout doesn't jump when it floats). Use a padding-top aspect box.
  return (
    <div ref={ref} className="media-slot" style={{ position: "relative", width: "100%", maxWidth: 480, marginTop: 6 }}>
      <div style={{ paddingTop: `${aspect * 100}%` }} />
      {!player && (
        <button className="media-slot-poster" onClick={() => openPlayer({ kind, src, hostId, title })}>
          {thumbUrl && <img className="media-slot-thumb" src={thumbUrl} alt={title} />}
          <span className="media-slot-play">&#9654; Play</span>
        </button>
      )}
      {player && !docked && (
        <button className="media-slot-chip" onClick={() => setPlayerState(player.id, "docked")}>
          &#9654; Playing in a floating player &mdash; dock it back
        </button>
      )}
      {/* When docked, the root-level MediaPlayer overlays this slot; nothing rendered here. */}
    </div>
  );
}
```

- [ ] **Step 2: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: **clean** (old PiP files still resolve against the present `PipContext`). `MediaSlot.tsx` compiles.

- [ ] **Step 3: Self-check** — the IntersectionObserver cleanup disconnects AND `unregisterHost` (which orphans the player to floating); `openPlayer` is called only from the idle poster; the aspect box reserves height.

- [ ] **Step 4: Commit**

```bash
git add client/src/components/MediaSlot.tsx
git commit -m "feat(media-players): MediaSlot placeholder + host registration + visibility observer"
```

---

### Task 6: `LinkEmbed` — route video + YouTube/Spotify through MediaSlot; delete PipPane

**Files:**
- Modify: `client/src/components/LinkEmbed.tsx`
- Delete: `client/src/components/PipPane.tsx`

**Interfaces:**
- Consumes: `MediaSlot`; `buildEmbedPlayerSrc`, `getEmbedConsent`, `setEmbedConsent`, `providerLabel` (existing, unchanged); `EmbedConsentModal` (existing); `useProxiedMedia` (still used for the inline IMAGE and the video poster thumbnail).
- Produces: no new exports.

The current `LinkEmbed` has: an inline-image branch (keep), a video branch that opened a PiP (replace with `MediaSlot`), and a YouTube/Spotify branch that rendered the iframe inline after consent (replace: register an `iframe` player via `MediaSlot` on consent). Remove the `usePip`/`openPip` usage and the inline `<iframe>`.

- [ ] **Step 1: Replace `LinkEmbed.tsx`**

Replace the whole file with:

```tsx
import { useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { useLinkEmbed } from "../hooks/useLinkEmbed";
import { useProxiedMedia } from "../hooks/useProxiedMedia";
import { useMediaPlayers } from "../context/MediaPlayersContext";
import MediaSlot from "./MediaSlot";
import EmbedConsentModal from "./EmbedConsentModal";
import { buildEmbedPlayerSrc, getEmbedConsent, setEmbedConsent } from "../lib/embedPlayer";

export default function LinkEmbed({ url, dataSaver }: { url: string; dataSaver: boolean }) {
  const [loaded, setLoaded] = useState(!dataSaver);
  const state = useLinkEmbed(url, loaded);
  const { openPlayer } = useMediaPlayers();
  const [showConsent, setShowConsent] = useState(false);
  // Stable per-embed host id so the slot and player line up across re-renders.
  const [hostId] = useState(() => `embed-${Math.random().toString(36).slice(2)}`);

  const e = state.status === "ok" ? state.embed : null;
  const inlineMedia = e?.media?.playable_inline ? e.media : null;
  const isVideo = !!inlineMedia?.mime.startsWith("video/");

  // Inline IMAGE bytes (unchanged). Video no longer fetches on the card — it
  // streams when the player opens (inside MediaPlayer via useProxiedMedia).
  const imageBlob = useProxiedMedia(
    inlineMedia?.url ?? null,
    loaded && state.status === "ok" && !!inlineMedia && !isVideo,
  );
  // Thumbnail for the video / iframe poster.
  const thumbBlob = useProxiedMedia(
    e?.thumbnail ?? null,
    loaded && state.status === "ok" && !!e?.thumbnail && (!inlineMedia || isVideo),
  );

  if (!loaded) {
    return <button className="link-embed-chip" onClick={() => setLoaded(true)}>Load preview</button>;
  }
  if (state.status === "loading") {
    return <div className="link-embed link-embed-state">Loading preview&hellip;</div>;
  }
  if (state.status !== "ok" || !e) return null;

  const player = buildEmbedPlayerSrc(e.url);

  const watchHere = () => {
    if (!player) return;
    if (getEmbedConsent(player.provider)) openPlayer({ kind: "iframe", src: player.src, hostId, title: e.author ?? e.title ?? "Video" });
    else setShowConsent(true);
  };

  return (
    <div className={`link-embed link-embed--${e.provider}`}>
      {e.author && <div className="link-embed-author">{e.author}</div>}
      {e.title && <div className="link-embed-title">{e.title}</div>}
      {e.description && <div className="link-embed-desc">{e.description}</div>}

      {/* Inline image (unchanged) */}
      {inlineMedia && !isVideo && imageBlob && (
        <img className="link-embed-image" src={imageBlob} alt={e.title ?? ""} />
      )}

      {/* Playable VIDEO (Twitter/X video, direct file) → inline-first player */}
      {inlineMedia && isVideo && (
        <MediaSlot hostId={hostId} kind="video" src={inlineMedia.url} title={e.author ?? e.title ?? "Video"} thumbUrl={thumbBlob} />
      )}

      {/* YouTube/Spotify → "Watch here" opens an iframe player (after consent) */}
      {!inlineMedia && player && (
        <div className="link-embed-player-wrap">
          <MediaSlot hostId={hostId} kind="iframe" src={player.src} title={e.author ?? e.title ?? "Video"} thumbUrl={thumbBlob} aspect={player.provider === "spotify" ? 0.32 : 0.5625} />
          <div className="link-embed-slot-actions">
            <button className="embed-watch-btn" onClick={watchHere}>&#9654; Watch here</button>
            <button className="embed-open-external" onClick={() => { void openExternal(e.url); }}>Open externally &#8599;</button>
          </div>
          {player.provider === "spotify" && (
            <div className="embed-player-note">30-second preview in Farder &mdash; open externally for the full track.</div>
          )}
        </div>
      )}

      {/* Non-inline, non-YouTube/Spotify (e.g. reddit video) → external open (unchanged) */}
      {!inlineMedia && !player && (thumbBlob || e.kind === "Video" || e.kind === "Audio") && (
        <div className="link-embed-thumb-wrap">
          {thumbBlob && <img className="link-embed-thumb" src={thumbBlob} alt={e.title ?? ""} />}
          {(e.kind === "Video" || e.kind === "Audio") && (
            <button className="link-embed-play" onClick={() => { void openExternal(e.url); }}>
              &#9654; {e.kind === "Video" ? "Play" : "Open"}
            </button>
          )}
        </div>
      )}

      {e.duration_secs != null && <div className="link-embed-duration">{formatDuration(e.duration_secs)}</div>}

      {showConsent && player && (
        <EmbedConsentModal
          provider={player.provider}
          onConfirm={(always) => {
            if (always) setEmbedConsent(player.provider, true);
            setShowConsent(false);
            openPlayer({ kind: "iframe", src: player.src, hostId, title: e.author ?? e.title ?? "Video" });
          }}
          onCancel={() => setShowConsent(false)}
        />
      )}
    </div>
  );
}

function formatDuration(s: number): string {
  const m = Math.floor(s / 60);
  const sec = String(s % 60).padStart(2, "0");
  return `${m}:${sec}`;
}
```

Note: for the **video** slot, clicking ▶ in `MediaSlot` calls `openPlayer({ kind: "video", ... })` directly (no consent). For the **iframe** slot, the ▶ poster inside `MediaSlot` would also call `openPlayer` directly — but YouTube/Spotify must go through consent first. To keep consent, the iframe slot's poster ▶ is NOT used; the consent-gated **"Watch here"** button beside it is the trigger. That means the iframe `MediaSlot` should not show its own ▶ poster. Handle this by passing the iframe slot a flag:

In `MediaSlot.tsx`, add an optional prop `manualTrigger?: boolean` (default false); when true, the slot does NOT render its own ▶ poster (the parent supplies the trigger). Update the `MediaSlot` props/interface accordingly and gate the poster: `{!player && !manualTrigger && (<button className="media-slot-poster" ...>)}`. Then in `LinkEmbed`, pass `manualTrigger` on the iframe `MediaSlot`. (Apply this small `MediaSlot` change as part of this task.)

- [ ] **Step 2: Apply the `MediaSlot` `manualTrigger` tweak**

In `client/src/components/MediaSlot.tsx`: add `manualTrigger?: boolean` to the props type, and change the idle-poster condition from `{!player && (` to `{!player && !manualTrigger && (`. In `LinkEmbed.tsx` (above), the iframe `MediaSlot` is rendered with `manualTrigger` — update that JSX to include the prop: `<MediaSlot hostId={hostId} kind="iframe" src={player.src} title={...} thumbUrl={thumbBlob} aspect={...} manualTrigger />`.

- [ ] **Step 3: Delete `PipPane.tsx`**

```bash
git rm client/src/components/PipPane.tsx
```

- [ ] **Step 4: Type-check + confirm old PiP is gone**

Run: `cd client && npx tsc --noEmit`
Expected: **clean** (no more `usePip`/PiP references anywhere).

Run: `grep -rn "usePip\|PipContext\|PipPane\|PipLayer\|openPip" client/src` 
Expected: only `client/src/context/PipContext.tsx` itself (deleted in Task 9) — no other references. Put the output in your report.

- [ ] **Step 5: Commit**

```bash
git add client/src/components/LinkEmbed.tsx client/src/components/MediaSlot.tsx
git rm client/src/components/PipPane.tsx
git commit -m "feat(media-players): LinkEmbed uses MediaSlot for video + YouTube/Spotify; remove PipPane"
```

---

### Task 7: "Always float" setting

**Files:**
- Modify: `client/src/components/VoiceSettings.tsx`
- Modify: `client/src/lib/floatAnchor.ts` (add the preference helpers) **or** a tiny new helper — see step 1.

**Interfaces:**
- Produces: `getAlwaysFloat(): boolean`, `setAlwaysFloat(v: boolean): void` in `client/src/lib/floatAnchor.ts`; consumed by `MediaSlot`/`LinkEmbed` open calls and the Settings toggle.

- [ ] **Step 1: Add the preference helpers to `floatAnchor.ts`**

Append to `client/src/lib/floatAnchor.ts`:

```ts
const ALWAYS_KEY = "farder.alwaysFloat";
/** Whether ▶ should open players directly floating. Default false; fail-safe false. */
export function getAlwaysFloat(): boolean {
  try { return localStorage.getItem(ALWAYS_KEY) === "1"; } catch { return false; }
}
export function setAlwaysFloat(v: boolean): void {
  try { if (v) localStorage.setItem(ALWAYS_KEY, "1"); else localStorage.removeItem(ALWAYS_KEY); } catch { /* ignore */ }
}
```

- [ ] **Step 2: Honor the preference when opening**

In `client/src/components/MediaSlot.tsx`, import `getAlwaysFloat` and pass `float: getAlwaysFloat()` in its `openPlayer` call:
```tsx
onClick={() => openPlayer({ kind, src, hostId, title, float: getAlwaysFloat() })}
```
In `client/src/components/LinkEmbed.tsx`, add `float: getAlwaysFloat()` to BOTH `openPlayer` calls (the consent-granted `watchHere` and the modal `onConfirm`), importing `getAlwaysFloat` from `../lib/floatAnchor`.

- [ ] **Step 3: Add the toggle in `VoiceSettings.tsx`**

Import `getAlwaysFloat, setAlwaysFloat` from `../lib/floatAnchor`. Add state `const [alwaysFloat, setAlwaysFloatState] = useState<boolean>(false);` with the other `useState`s, and seed it in a mount effect (next to the other settings-loading effects):
```tsx
  useEffect(() => { setAlwaysFloatState(getAlwaysFloat()); }, []);
```
Add a handler:
```tsx
  const chooseAlwaysFloat = (v: boolean) => { setAlwaysFloatState(v); setAlwaysFloat(v); };
```
Add to the "Privacy & Data" `SettingsSection` (after the embed-consent toggles):
```tsx
        <label className="settings-row">
          <input type="checkbox" checked={alwaysFloat} onChange={(e) => chooseAlwaysFloat(e.target.checked)} />
          Always play videos in a floating player (instead of inline)
        </label>
```

- [ ] **Step 4: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add client/src/lib/floatAnchor.ts client/src/components/MediaSlot.tsx client/src/components/LinkEmbed.tsx client/src/components/VoiceSettings.tsx
git commit -m "feat(media-players): 'always float' setting (default off)"
```

---

### Task 8: Theming (all three themes)

**Files:**
- Modify: `client/src/themes/discord-dark/theme.css`
- Modify: `client/src/themes/hello-kitty/theme.css`
- Modify: `client/src/themes/xp-luna-blue/theme.css`

**Interfaces:**
- Produces styling for: `.media-slot`, `.media-slot-poster`, `.media-slot-thumb`, `.media-slot-play`, `.media-slot-chip`, `.link-embed-slot-actions`, `.embed-watch-btn`, `.embed-open-external`, `.embed-player-note`, `.mp-docked`, `.mp-float`, `.mp-head`, `.mp-title`, `.mp-opacity`, `.mp-btn`, `.mp-pop`, `.mp-media`, `.mp-state`, `.mp-mini`.

- [ ] **Step 1: Append the same block to EACH theme file**

Add near the existing `.link-embed`/`.screen-stage` rules in all three files:

```css
/* --- Inline-first media players (docked + floating) --- */
.media-slot { background: #000; border-radius: 4px; overflow: hidden; }
.media-slot-poster { position: absolute; inset: 0; width: 100%; height: 100%; border: 0; padding: 0; cursor: pointer; background: #000; }
.media-slot-thumb { width: 100%; height: 100%; object-fit: contain; display: block; }
.media-slot-play { position: absolute; inset: 0; margin: auto; width: fit-content; height: fit-content; padding: 6px 12px; background: var(--xp-blue); color: #fff; border-radius: 4px; }
.media-slot-chip { position: absolute; inset: 0; width: 100%; height: 100%; border: 0; cursor: pointer; background: var(--xp-panel-bg); color: var(--xp-text-normal, var(--xp-text-secondary)); font-size: 0.85em; }
.link-embed-slot-actions { display: flex; gap: 8px; align-items: center; margin-top: 4px; }
.embed-watch-btn { padding: 4px 10px; background: var(--xp-blue); color: #fff; border: none; border-radius: 4px; cursor: pointer; font-size: 0.85em; }
.embed-open-external { background: none; border: none; padding: 0; cursor: pointer; color: var(--xp-blue); font-size: 0.85em; }
.embed-player-note { color: var(--xp-text-secondary); font-size: 0.8em; margin-top: 2px; }

.mp-docked { overflow: hidden; border-radius: 4px; }
.mp-float { position: fixed; display: flex; flex-direction: column; min-width: 220px; min-height: 140px; resize: both; overflow: hidden; border: 1px solid var(--xp-border); border-radius: 8px; background: var(--xp-panel-bg); box-shadow: 0 14px 44px rgba(0,0,0,0.55); }
.mp-head { display: flex; align-items: center; gap: 8px; padding: 5px 8px; font-size: 12px; color: var(--xp-text-normal, var(--xp-text-secondary)); cursor: move; user-select: none; }
.mp-title { flex: 1; font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.mp-opacity { width: 80px; accent-color: var(--xp-blue); }
.mp-btn { background: none; border: none; color: var(--xp-text-normal, var(--xp-text-secondary)); cursor: pointer; font-size: 14px; line-height: 1; }
.mp-pop { position: absolute; top: 6px; right: 6px; z-index: 1; padding: 2px 6px; font-size: 12px; line-height: 1; cursor: pointer; border: 1px solid var(--xp-border); border-radius: 4px; background: var(--xp-panel-bg); color: var(--xp-text-normal, var(--xp-text-secondary)); }
.mp-media { width: 100%; height: 100%; flex: 1; min-height: 0; border: 0; background: #000; object-fit: contain; display: block; }
.mp-state { width: 100%; height: 100%; display: flex; align-items: center; justify-content: center; background: #000; color: var(--xp-text-secondary); font-size: 0.9em; }
.mp-mini { position: fixed; display: flex; align-items: center; gap: 6px; padding: 5px 10px; max-width: 220px; border-radius: 20px; background: var(--xp-panel-bg); border: 1px solid var(--xp-border); color: var(--xp-text-normal, var(--xp-text-secondary)); font-size: 12px; cursor: move; user-select: none; box-shadow: 0 6px 18px rgba(0,0,0,0.4); }
```

- [ ] **Step 2: Verify all three themes + colors**

Run: `grep -l "mp-float" client/src/themes/*/theme.css` → lists all three. Put output in report.
Scan the block in each file: only `var(--xp-…)`, `#000` (letterbox), `#fff` (on `--xp-blue`), and `rgba(0,0,0,…)` shadows. No other literals.

- [ ] **Step 3: Commit**

```bash
git add client/src/themes/discord-dark/theme.css client/src/themes/hello-kitty/theme.css client/src/themes/xp-luna-blue/theme.css
git commit -m "feat(media-players): theme docked/floating players + slot in all three themes"
```

---

### Task 9: Docs + delete old PipContext

**Files:**
- Delete: `client/src/context/PipContext.tsx`
- Delete: `docs/modules/frontend-pip.md`
- Create: `docs/modules/frontend-media-players.md`
- Modify: `docs/modules/relay-embed.md`

**Interfaces:** none.

- [ ] **Step 1: Delete the now-unused PiP context and its doc**

Run: `grep -rn "PipContext\|usePip" client/src` → expect NO matches (Task 6 removed the last usage). Then:
```bash
git rm client/src/context/PipContext.tsx docs/modules/frontend-pip.md
```

- [ ] **Step 2: Write the new module doc**

Create `docs/modules/frontend-media-players.md`:

```markdown
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
```

- [ ] **Step 3: Update `relay-embed.md`**

In `docs/modules/relay-embed.md`, update the LinkEmbed media note: playable video and the YouTube/Spotify "Watch here" player now render via `MediaSlot` + the root `MediaPlayersLayer` (inline-first, float/dock — see `frontend-media-players.md`); the old PiP poster→pane flow is gone. Keep the existing consent/`youtube-nocookie`/`referrerPolicy="origin"` facts.

- [ ] **Step 4: Commit**

```bash
git add docs/modules/frontend-media-players.md docs/modules/relay-embed.md
git rm client/src/context/PipContext.tsx docs/modules/frontend-pip.md
git commit -m "docs(media-players): module doc + relay-embed update; remove old PiP context/doc"
```

---

## Final verification (before declaring done in code)

- [ ] `cd client && npx tsc --noEmit` clean.
- [ ] `grep -rn "usePip\|PipContext\|PipPane\|PipLayer\|openPip" client/src` → no matches.
- [ ] `grep -l "mp-float" client/src/themes/*/theme.css` → all three themes.
- [ ] `grep -rn "invoke(" client/src/context/MediaPlayersContext.tsx client/src/components/MediaPlayer.tsx client/src/components/MediaSlot.tsx client/src/components/MediaPlayersLayer.tsx` → no new Tauri commands.
- [ ] `grep -n 'referrerPolicy' client/src/components/MediaPlayer.tsx` → `"origin"` (NOT `no-referrer`).
- [ ] Spec coverage walk: inline-default (MediaSlot ▶ → docked) ✓; pop-out + scroll-detach + auto-dock (MediaPlayer + MediaSlot observer + reducer hostVisible/orphan) ✓; no-reload (root mount, never re-parented) ✓; video + iframe ✓; remembered pos/size (floatAnchor on drag/resize end) ✓; default right-of-chat ✓; always-float setting ✓; pointer-capture drag fix ✓; cap 4 + toast ✓; theming ×3 ✓; docs ✓.
- [ ] **Runtime (owner, Windows — UNVERIFIED until run; WSL has no display). VERIFY THE DOCKED SCROLL-TRACKING SMOOTHNESS FIRST** (the flagged risk): ▶ a tweet video → plays inline in the card; scroll down slowly and fast → the player follows smoothly, then floats to the right of chat; scroll back → docks into the card; pop-out button floats on demand; **drag a floating player and release the mouse outside the window → drag ends cleanly (no runaway)**; resize, then open another video → opens at the saved position/size; switch channels while floating → keeps playing; YouTube "Watch here" → floats/docks without restarting; turn on "Always float" → ▶ opens floating; 5th player → toast. If docked tracking lags badly, apply the spec's snappier fallback.
```

