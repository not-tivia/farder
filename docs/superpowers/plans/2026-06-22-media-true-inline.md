# Media Players: True-Inline (in-place float) + Volume — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render each media player as one element inside its chat card that toggles between inline (docked) and `position: fixed` (floating/minimized) WITHOUT moving in the DOM — so video/YouTube never reload on dock/float/scroll — and default `<video>` volume to a remembered 15%.

**Architecture:** The player element lives in `MediaSlot` (inside the message). Docked = `position:absolute; inset:0` filling the slot's aspect box (looks inline, native scroll). Floating = the same element becomes `position:fixed` at a saved anchor. Minimized = fixed pill with the media `display:none` (stays mounted). No root overlay layer, no rect-tracking. The existing registry/reducer is kept (minus the now-unused host ref-map + orphan).

**Tech Stack:** React 18 + TypeScript, Tauri, existing `useProxiedMedia`, per-theme CSS.

## Global Constraints

- **Client-only.** No Rust/relay/protocol/Tauri-command changes. State = React + `localStorage`.
- **Never re-parent / never conditionally unmount the media element.** It is rendered once per player, always inside `MediaPlayer`'s media-wrap, in all states (minimized hides it via `display:none`, never unmounts). This is what prevents reload-on-transition.
- **iframe attributes exact (no regression):** `sandbox="allow-scripts allow-same-origin allow-presentation allow-popups"`, `referrerPolicy="origin"`, `allow="autoplay; encrypted-media; fullscreen; picture-in-picture"`, `loading="lazy"`, `allowFullScreen`.
- **No JS test runner.** Tests = `cd client && npx tsc --noEmit` clean + pure reducer/helpers carry inline test-notes.
- **Per-kind float:** VIDEO auto-floats on scroll-out / auto-docks on scroll-in (IntersectionObserver) + manual pop-out; IFRAME does NOT auto-float (manual pop-out only).
- **Volume:** `<video>` defaults to 0.15, remembered in `localStorage` `farder.mediaVolume`; iframe volume is not script-controllable (skip).
- **Float anchor right-of-chat** (`farder.floatAnchor`), cap `MAX_PLAYERS=4`, z-base 300 — unchanged from the existing context.
- **Theming:** all 3 themes, no hard-coded colors except `#000` (letterbox) / `#fff`-on-`--xp-blue` / `rgba(0,0,0,…)` shadows; `var(--xp-text-normal, var(--xp-text-secondary))` fallback.
- **Spec:** `docs/superpowers/specs/2026-06-22-media-true-inline-design.md`.

## Ordering rationale
Consumers (`MediaPlayer`, `MediaSlot`) are rewritten to STOP using `hosts`/`registerHost`/`unregisterHost` BEFORE the context is trimmed (Task 4), so every task is tsc-clean (unused context exports compile fine until removed).

---

### Task 1: `mediaPrefs.ts` — remembered media volume

**Files:**
- Create: `client/src/lib/mediaPrefs.ts`

**Interfaces:**
- Produces: `getMediaVolume(): number`, `setMediaVolume(v: number): void`.

- [ ] **Step 1: Write the module**

Create `client/src/lib/mediaPrefs.ts`:

```ts
// Remembered <video> volume. Defaults to 15%; fails safe to 0.15 on any error.
const KEY = "farder.mediaVolume";
const DEFAULT = 0.15;

/**
 * Read the remembered volume (0..1), or 0.15.
 * Test-notes (verified by inspection):
 *   - nothing saved      → 0.15
 *   - saved "0.4"        → 0.4
 *   - saved "5" (out of range) → 0.15
 *   - saved "abc" / throws → 0.15
 */
export function getMediaVolume(): number {
  try {
    const v = parseFloat(localStorage.getItem(KEY) ?? "");
    return v >= 0 && v <= 1 ? v : DEFAULT;
  } catch { return DEFAULT; }
}

/** Persist the volume (0..1); ignores out-of-range + storage errors. */
export function setMediaVolume(v: number): void {
  try { if (v >= 0 && v <= 1) localStorage.setItem(KEY, String(v)); } catch { /* ignore */ }
}
```

- [ ] **Step 2: Type-check** — `cd client && npx tsc --noEmit` → clean.
- [ ] **Step 3: Trace the 4 test-notes by hand.** Fix any mismatch.
- [ ] **Step 4: Commit**

```bash
git add client/src/lib/mediaPrefs.ts
git commit -m "feat(media-players): remembered <video> volume (default 15%)"
```

---

### Task 2: Rewrite `MediaPlayer` for in-place rendering + volume

**Files:**
- Modify (full replace): `client/src/components/MediaPlayer.tsx`

**Interfaces:**
- Consumes: `useMediaPlayers` (`focusPlayer`, `updatePlayer`, `setPlayerState`, `closePlayer`), `MediaPlayerInfo` from context; `useProxiedMedia`; `setFloatAnchor` from `../lib/floatAnchor`; `getMediaVolume`/`setMediaVolume` from `../lib/mediaPrefs`. (Does NOT use `hosts` anymore.)
- Produces: `export default function MediaPlayer({ player }: { player: MediaPlayerInfo })`.

The media element is rendered ONCE in `.mp-media-wrap` and never moved/unmounted. Container class/position is the only thing that changes per state: docked = `.mp-docked` (CSS `position:absolute; inset:0` filling the slot box), floating = `.mp-float` (inline `position:fixed` at pos/size), minimized = `.mp-mini` (inline `position:fixed` pill, media hidden).

- [ ] **Step 1: Replace the file**

```tsx
import { useEffect, useRef } from "react";
import { useProxiedMedia } from "../hooks/useProxiedMedia";
import { setFloatAnchor } from "../lib/floatAnchor";
import { getMediaVolume, setMediaVolume } from "../lib/mediaPrefs";
import { useMediaPlayers, type MediaPlayerInfo } from "../context/MediaPlayersContext";

export default function MediaPlayer({ player }: { player: MediaPlayerInfo }) {
  const { focusPlayer, updatePlayer, setPlayerState, closePlayer } = useMediaPlayers();
  const rootRef = useRef<HTMLDivElement>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const dragRef = useRef<{ dx: number; dy: number; raf: number } | null>(null);

  // Video bytes via the relay; kept loaded in ALL states (incl. minimized) so the
  // element never reloads. iframe needs no proxy.
  const videoUrl = useProxiedMedia(player.kind === "video" ? player.src : null, player.kind === "video");

  // Apply remembered volume once the <video> + src are ready; persist on change.
  useEffect(() => {
    if (player.kind === "video" && videoRef.current) videoRef.current.volume = getMediaVolume();
  }, [videoUrl, player.kind]);
  const onVolume = () => { if (videoRef.current) setMediaVolume(videoRef.current.volume); };

  // Drag (floating/minimized) via pointer capture — release outside the window still ends it.
  const startDrag = (e: React.PointerEvent) => {
    if ((e.target as HTMLElement).closest("button, input")) return;
    const root = rootRef.current; if (!root) return;
    const rect = root.getBoundingClientRect();
    dragRef.current = { dx: e.clientX - rect.left, dy: e.clientY - rect.top, raf: 0 };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  };
  const onDragMove = (e: React.PointerEvent) => {
    const d = dragRef.current; if (!d) return;
    const x = e.clientX - d.dx, y = e.clientY - d.dy;
    if (!d.raf) d.raf = requestAnimationFrame(() => { d.raf = 0; updatePlayer(player.id, { pos: { x, y } }); });
  };
  const endDrag = (e: React.PointerEvent) => {
    const d = dragRef.current; if (!d) return;
    if (d.raf) cancelAnimationFrame(d.raf);
    dragRef.current = null;
    try { (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId); } catch { /* ignore */ }
    setFloatAnchor({ x: player.pos.x, y: player.pos.y, w: player.size.w, h: player.size.h });
  };

  // Persist size after a CSS resize (floating only).
  useEffect(() => {
    if (player.state !== "floating") return;
    const el = rootRef.current; if (!el) return;
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

  // The media element — rendered ONCE; never repositioned in the DOM.
  const media = player.kind === "video"
    ? (videoUrl
        ? <video ref={videoRef} className="mp-media" src={videoUrl} controls autoPlay onVolumeChange={onVolume} />
        : <div className="mp-state">Couldn&rsquo;t load video</div>)
    : <iframe className="mp-media" src={player.src} title={player.title}
        sandbox="allow-scripts allow-same-origin allow-presentation allow-popups"
        referrerPolicy="origin" allow="autoplay; encrypted-media; fullscreen; picture-in-picture"
        loading="lazy" allowFullScreen />;

  const floating = player.state === "floating";
  const minimized = player.state === "minimized";
  const cls = minimized ? "mp-mini" : floating ? "mp-float" : "mp-docked";
  const style: React.CSSProperties | undefined = floating
    ? { left: player.pos.x, top: player.pos.y, width: player.size.w, height: player.size.h, opacity: player.opacity, zIndex: player.z }
    : minimized
      ? { left: player.pos.x, top: player.pos.y, zIndex: player.z }
      : { zIndex: player.z }; // docked: position/inset come from .mp-docked CSS

  return (
    <div ref={rootRef} className={cls} style={style} onMouseDown={() => focusPlayer(player.id)}>
      {floating && (
        <div className="mp-head" onPointerDown={startDrag} onPointerMove={onDragMove} onPointerUp={endDrag} onPointerCancel={endDrag}>
          <span className="mp-title">{player.title}</span>
          <input className="mp-opacity" type="range" min={0.2} max={1} step={0.05} value={player.opacity}
                 title="Opacity" onChange={(e) => updatePlayer(player.id, { opacity: Number(e.target.value) })} />
          {player.hostId && <button className="mp-btn" title="Dock back into chat" onClick={() => setPlayerState(player.id, "docked")}>&#x21F2;</button>}
          <button className="mp-btn" title="Minimize" onClick={() => setPlayerState(player.id, "minimized")}>&#x2013;</button>
          <button className="mp-btn" title="Close" onClick={() => closePlayer(player.id)}>&#x2715;</button>
        </div>
      )}
      {minimized && (
        <div className="mp-mini-bar" onPointerDown={startDrag} onPointerMove={onDragMove} onPointerUp={endDrag} onPointerCancel={endDrag}>
          <span className="mp-title">{player.title}</span>
          <button className="mp-btn" title="Restore" onClick={() => setPlayerState(player.id, "floating")}>&#x25A2;</button>
          <button className="mp-btn" title="Close" onClick={() => closePlayer(player.id)}>&#x2715;</button>
        </div>
      )}
      {!floating && !minimized && (
        <button className="mp-pop" title="Pop out" onClick={() => setPlayerState(player.id, "floating")}>&#x2197;</button>
      )}
      <div className="mp-media-wrap" style={minimized ? { display: "none" } : undefined}>
        {media}
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Type-check** — `cd client && npx tsc --noEmit` → clean (the context still exports `hosts` etc.; this file simply no longer imports them).
- [ ] **Step 3: Self-check** — the `<video>`/`<iframe>` lives only inside `.mp-media-wrap` and is rendered in every state (minimized just hides the wrap), so it never unmounts; pointer-capture drag releases on up+cancel; no `scroll`/`resize` rect-tracking remains; iframe attributes exact; volume applied + persisted.
- [ ] **Step 4: Commit**

```bash
git add client/src/components/MediaPlayer.tsx
git commit -m "feat(media-players): in-place MediaPlayer (docked=inline, float=fixed, no reload) + volume"
```

---

### Task 3: Rewrite `MediaSlot` to render the player in-place; delete the root layer

**Files:**
- Modify (full replace): `client/src/components/MediaSlot.tsx`
- Modify: `client/src/components/AppShell.tsx` (remove the `MediaPlayersLayer` import + element)
- Delete: `client/src/components/MediaPlayersLayer.tsx`

**Interfaces:**
- Consumes: `useMediaPlayers` (`players`, `setHostVisible`, `openPlayer`, `setPlayerState`, `closePlayer`); `getAlwaysFloat`; `MediaPlayer`.
- Produces: `export default function MediaSlot(props: { hostId; kind; src; title; thumbUrl?; aspect?; manualTrigger? })` (same prop shape as today).

- [ ] **Step 1: Replace `MediaSlot.tsx`**

```tsx
import { useEffect, useRef } from "react";
import { useMediaPlayers, type PlayerKind } from "../context/MediaPlayersContext";
import { getAlwaysFloat } from "../lib/floatAnchor";
import MediaPlayer from "./MediaPlayer";

export default function MediaSlot({
  hostId, kind, src, title, thumbUrl, aspect = 0.5625, manualTrigger = false,
}: { hostId: string; kind: PlayerKind; src: string; title: string; thumbUrl?: string | null; aspect?: number; manualTrigger?: boolean }) {
  const { players, setHostVisible, openPlayer, setPlayerState, closePlayer } = useMediaPlayers();
  const ref = useRef<HTMLDivElement>(null);
  const player = players.find((p) => p.hostId === hostId);

  // Keep the current player id in a ref so the unmount cleanup can close it.
  const playerIdRef = useRef<string | null>(null);
  playerIdRef.current = player?.id ?? null;

  // VIDEO auto-floats when its card scrolls out of view (and docks back). IFRAME
  // does not auto-float (manual pop-out only), so only observe for video.
  useEffect(() => {
    if (kind !== "video") return;
    const el = ref.current; if (!el) return;
    const io = new IntersectionObserver(
      (entries) => { for (const e of entries) setHostVisible(hostId, e.isIntersecting); },
      { threshold: 0.5 },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [hostId, kind]); // eslint-disable-line react-hooks/exhaustive-deps

  // On unmount (e.g. switching server/channel) close this slot's player — floating
  // players live with their message and do not persist across navigation.
  useEffect(() => {
    return () => { if (playerIdRef.current) closePlayer(playerIdRef.current); };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div ref={ref} className="media-slot" style={{ position: "relative", width: "100%", maxWidth: 480, marginTop: 6 }}>
      <div style={{ paddingTop: `${aspect * 100}%` }} />
      {!player && !manualTrigger && (
        <button className="media-slot-poster" onClick={() => openPlayer({ kind, src, hostId, title, float: getAlwaysFloat() })}>
          {thumbUrl && <img className="media-slot-thumb" src={thumbUrl} alt={title} />}
          <span className="media-slot-play">&#9654; Play</span>
        </button>
      )}
      {player && <MediaPlayer player={player} />}
      {player && player.state !== "docked" && (
        <button className="media-slot-chip" onClick={() => setPlayerState(player.id, "docked")}>
          &#9654; Playing in a floating player &mdash; dock it back
        </button>
      )}
    </div>
  );
}
```

- [ ] **Step 2: Remove `MediaPlayersLayer` from `AppShell.tsx`**

Delete the import line `import MediaPlayersLayer from "./MediaPlayersLayer";` and the `<MediaPlayersLayer />` element (next to `<ScreenShareStage voice={voice} />`).

- [ ] **Step 3: Delete the layer**

```bash
git rm client/src/components/MediaPlayersLayer.tsx
```

- [ ] **Step 4: Type-check + grep** — `cd client && npx tsc --noEmit` → clean. Then `grep -rn "MediaPlayersLayer" client/src` → no matches. Put the grep result in your report.
- [ ] **Step 5: Commit**

```bash
git add client/src/components/MediaSlot.tsx client/src/components/AppShell.tsx
git rm client/src/components/MediaPlayersLayer.tsx
git commit -m "feat(media-players): render player in-place via MediaSlot; remove root layer"
```

---

### Task 4: Trim the context (remove host ref-map + orphan)

**Files:**
- Modify: `client/src/context/MediaPlayersContext.tsx`

**Interfaces:**
- Produces: `useMediaPlayers()` now returns `{ players, openPlayer, closePlayer, focusPlayer, updatePlayer, setPlayerState, setHostVisible }` (no `hosts`/`registerHost`/`unregisterHost`); reducer no longer has the `orphan` action.

- [ ] **Step 1: Confirm nothing uses the removed surface**

Run: `grep -rn "registerHost\|unregisterHost\|hosts\.current\|orphan" client/src`
Expected: matches ONLY inside `client/src/context/MediaPlayersContext.tsx` itself (its own definitions, which you remove in step 2) — NO other file should reference them (Tasks 2–3 removed all consumers). If a consumer outside the context still references them, STOP and report. Put the result in your report.

- [ ] **Step 2: Edit `MediaPlayersContext.tsx`**

Remove, with surgical edits:
- The `MutableRefObject` import (if it was added) — change the react import back to just what's used (`createContext, useContext, useReducer, useRef, useEffect, useCallback, ReactNode`). (`useRef` may still be used for `stateRef`; keep it.)
- The `hosts` field from `CtxValue`, the `hosts = useRef(new Map())` line in the provider, the `registerHost`/`unregisterHost` `useCallback`s, and `hosts`/`registerHost`/`unregisterHost` from the provider's context value object.
- The `orphan` action from the `Action` union, its `case "orphan":` in the reducer, and the `orphan` line in its test-notes.

Keep everything else (`open`/`close`/`focus`/`update`/`setState`/`hostVisible`, cap, dedupe, z, anchor seeding, the `stateRef` toast guard).

- [ ] **Step 3: Type-check + grep** — `cd client && npx tsc --noEmit` → clean. `grep -rn "registerHost\|unregisterHost\|orphan\|hosts" client/src/context/MediaPlayersContext.tsx` → no matches. Put results in report.
- [ ] **Step 4: Re-trace the remaining reducer test-notes** (open/dedupe/cap/close/focus/update/setState/hostVisible) by hand; confirm still accurate after removing orphan.
- [ ] **Step 5: Commit**

```bash
git add client/src/context/MediaPlayersContext.tsx
git commit -m "refactor(media-players): drop host ref-map + orphan (no overlay/persistence)"
```

---

### Task 5: Theming for in-place players (all three themes)

**Files:**
- Modify: `client/src/themes/discord-dark/theme.css`
- Modify: `client/src/themes/hello-kitty/theme.css`
- Modify: `client/src/themes/xp-luna-blue/theme.css`

**Interfaces:** styling for `.mp-docked`, `.mp-float`, `.mp-mini`, `.mp-mini-bar`, `.mp-head`, `.mp-title`, `.mp-opacity`, `.mp-btn`, `.mp-pop`, `.mp-media-wrap`, `.mp-media`, `.mp-state` (the `.media-slot*` classes already exist from the prior feature — keep them).

- [ ] **Step 1: Replace the existing `.mp-*` block in each theme**

In each theme file, find the existing `/* --- Inline-first media players ... --- */` block (the `.mp-docked`/`.mp-float`/etc. rules added previously) and REPLACE it with this updated block (docked is now `position:absolute; inset:0`; adds `.mp-media-wrap` and `.mp-mini-bar`):

```css
/* --- In-place media players (docked = absolute-fill slot, float = fixed) --- */
.mp-docked { position: absolute; inset: 0; overflow: hidden; border-radius: 4px; background: #000; }
.mp-float { position: fixed; display: flex; flex-direction: column; min-width: 220px; min-height: 140px; resize: both; overflow: hidden; border: 1px solid var(--xp-border); border-radius: 8px; background: var(--xp-panel-bg); box-shadow: 0 14px 44px rgba(0,0,0,0.55); }
.mp-mini { position: fixed; display: flex; align-items: center; gap: 6px; padding: 5px 10px; max-width: 220px; border-radius: 20px; background: var(--xp-panel-bg); border: 1px solid var(--xp-border); box-shadow: 0 6px 18px rgba(0,0,0,0.4); }
.mp-mini-bar { display: flex; align-items: center; gap: 6px; cursor: move; user-select: none; font-size: 12px; color: var(--xp-text-normal, var(--xp-text-secondary)); }
.mp-head { display: flex; align-items: center; gap: 8px; padding: 5px 8px; font-size: 12px; color: var(--xp-text-normal, var(--xp-text-secondary)); cursor: move; user-select: none; }
.mp-title { flex: 1; font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.mp-opacity { width: 80px; accent-color: var(--xp-blue); }
.mp-btn { background: none; border: none; color: var(--xp-text-normal, var(--xp-text-secondary)); cursor: pointer; font-size: 14px; line-height: 1; }
.mp-pop { position: absolute; top: 6px; right: 6px; z-index: 1; padding: 2px 6px; font-size: 12px; line-height: 1; cursor: pointer; border: 1px solid var(--xp-border); border-radius: 4px; background: var(--xp-panel-bg); color: var(--xp-text-normal, var(--xp-text-secondary)); }
.mp-media-wrap { flex: 1; min-height: 0; width: 100%; height: 100%; }
.mp-media { width: 100%; height: 100%; border: 0; background: #000; object-fit: contain; display: block; }
.mp-state { width: 100%; height: 100%; display: flex; align-items: center; justify-content: center; background: #000; color: var(--xp-text-secondary); font-size: 0.9em; }
```

(If a theme's old block also defined the now-removed `.mp-media` differently, this replaces it. The `.media-slot`, `.media-slot-poster/-thumb/-play/-chip`, `.embed-watch-btn`, `.embed-open-external`, `.embed-player-note` rules stay as-is.)

- [ ] **Step 2: Verify** — `grep -l "mp-media-wrap" client/src/themes/*/theme.css` lists all three; scan added lines for disallowed colors (only `var(--xp-…)`, `#000`, `#fff`, `rgba(0,0,0,…)`). Put grep output in report.
- [ ] **Step 3: Commit**

```bash
git add client/src/themes/discord-dark/theme.css client/src/themes/hello-kitty/theme.css client/src/themes/xp-luna-blue/theme.css
git commit -m "feat(media-players): theme in-place docked/float/mini players in all three themes"
```

---

### Task 6: Docs

**Files:**
- Modify: `docs/modules/frontend-media-players.md`

**Interfaces:** none.

- [ ] **Step 1: Update the module doc**

Rewrite the "Core idea" / pieces / transitions sections of `docs/modules/frontend-media-players.md` to describe the in-place model:
- Players render inside `MediaSlot` (in the chat card), not a root layer. Docked = `position:absolute; inset:0` over the slot's aspect box (inline); floating = the SAME element toggled to `position:fixed`; minimized = fixed pill with media `display:none`. The media element never re-parents/unmounts → no reload on dock/float/scroll/minimize.
- `MediaPlayersLayer` and the host ref-map / rect-tracking are GONE.
- Video auto-floats on scroll (IntersectionObserver); iframe is manual pop-out only.
- Volume: `farder.mediaVolume` (default 0.15), `<video>` only (`lib/mediaPrefs.ts`).
- Limitation: floating players do NOT persist across server/channel switch (they live with their message). Add the warning: a future `transform`/`filter`/`contain` on a chat ancestor would break `position:fixed` float positioning.

Also confirm the doc no longer references `MediaPlayersLayer`, `hosts`/`registerHost`, or rect-tracking.

- [ ] **Step 2: Commit**

```bash
git add docs/modules/frontend-media-players.md
git commit -m "docs(media-players): in-place model (no root layer/tracking) + volume + caveats"
```

---

## Final verification (before declaring done in code)

- [ ] `cd client && npx tsc --noEmit` clean.
- [ ] `grep -rn "MediaPlayersLayer\|registerHost\|unregisterHost\|orphan\|hosts\.current" client/src` → no matches.
- [ ] `grep -l "mp-media-wrap" client/src/themes/*/theme.css` → all three themes.
- [ ] `grep -rn "invoke(" client/src/lib/mediaPrefs.ts client/src/components/MediaPlayer.tsx client/src/components/MediaSlot.tsx` → no new Tauri commands.
- [ ] `grep -n 'referrerPolicy' client/src/components/MediaPlayer.tsx` → `"origin"`.
- [ ] The `<video>`/`<iframe>` is rendered in exactly one place (`.mp-media-wrap`) and is present in every player state (minimized hides the wrap, doesn't unmount it) — confirm by reading `MediaPlayer.tsx`.
- [ ] Spec coverage walk: in-place no-reload (Task 2) ✓; docked=inline absolute-fill (Tasks 2/5) ✓; float=fixed same element (Tasks 2/5) ✓; video auto-float + iframe manual-only (Task 3) ✓; volume 15% remembered (Tasks 1/2) ✓; close-on-nav (Task 3) ✓; context trim (Task 4) ✓; theming ×3 (Task 5) ✓; docs (Task 6) ✓.
- [ ] **Runtime (owner, Windows — UNVERIFIED until run):** ▶ a tweet video → plays INLINE in the card (not popped out); scroll within the channel → does NOT restart; scroll the card off → floats (right of chat), still playing; scroll back → docks; pop-out/dock buttons; drag + release the mouse OUTSIDE the window → ends cleanly; volume starts at 15%, a changed volume is remembered next time; minimize→restore doesn't restart; YouTube "Watch here" plays inline and pop-out doesn't restart it; switching channels closes a floating player (expected); 5th player → toast.
```

