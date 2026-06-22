# Floating Picture-in-Picture Video Player — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace inline auto-playing video embeds with a compact poster (▶ Play) that opens the video in a floating, draggable, resizable, opacity-adjustable in-app picture-in-picture pane — up to 4 at once — that persists across channel/server navigation.

**Architecture:** Entirely client-side. A new React context (`PipManager`) holds the list of open panes via a pure reducer; a `PipLayer` mounted at `AppShell` (the same overlay level as `ScreenShareStage`) renders one `PipPane` per open pane. Each pane streams the *same* relay-proxied bytes the inline player already used (`useProxiedMedia` → blob URL → `<video>`). `LinkEmbed` stops rendering inline video and instead renders a poster whose Play button calls `openPip`. No relay, protocol, or Tauri-command changes.

**Tech Stack:** React 18 + TypeScript, Tauri, existing `useProxiedMedia` hook, the existing `toast` window-event helper, per-theme CSS (`client/src/themes/*/theme.css`).

## Global Constraints

- **Client-only.** No changes to Rust crates, the relay, the protocol, or any `#[tauri::command]` / `generate_handler!` list. The relay's existing `ProxyMedia`/`fetch_media` and the `get_proxied_media` bridge already serve the bytes.
- **Privacy unchanged.** PiP renders only the relay-proxied bytes the embed already fetched. No new external connections.
- **No new colors.** Every color/border/background in new CSS must come from a `var(--xp-…)` variable; the only accepted hard-coded color is `#fff`-on-accent matching `.link-embed-play`/`.xp-button` (already an established exception). New classes MUST be styled in ALL THREE themes: `discord-dark`, `hello-kitty`, `xp-luna-blue`. `xp-luna-blue` does not define `--xp-text-normal`, so text color must use `var(--xp-text-normal, var(--xp-text-secondary))`.
- **No JS test runner exists in this repo.** "Tests" for TypeScript are: (a) `cd client && npx tsc --noEmit` is clean, and (b) the `PipManager` reducer is written as a **pure function** with inline test-notes verified by inspection — mirroring the existing `detectEmbedUrls` test-notes pattern in `client/src/lib/linkEmbed.ts`. Runtime behavior is UNVERIFIED until the owner's Windows run (WSL has no display).
- **Cap:** `MAX_PIPS = 4`. **Opacity range:** 0.2–1.0. **Z-order base:** panes sit above `ScreenShareStage` (which is `z-index: 200`), so PiP z starts at 300.
- **Spec:** `docs/superpowers/specs/2026-06-21-embed-video-pip-design.md`.
- **Docs-in-same-commit discipline** (CLAUDE.md): new context/components are documented in `docs/modules/` and the `LinkEmbed` behavior change noted in `docs/modules/relay-embed.md`.

---

### Task 1: `PipManager` context + pure reducer, wired into the app

**Files:**
- Create: `client/src/context/PipContext.tsx`
- Modify: `client/src/App.tsx` (wrap `<AppInner />` in `<PipProvider>`)

**Interfaces:**
- Consumes: `toast` from `client/src/lib/toast.ts` (`toast.info(message: string)`).
- Produces (relied on by Tasks 2, 3, 4):
  - `interface PipPaneState { id: string; mediaUrl: string; title: string; pos: { x: number; y: number }; size: { w: number; h: number }; opacity: number; minimized: boolean; z: number }`
  - `type PipPatch = Partial<Pick<PipPaneState, "pos" | "size" | "opacity" | "minimized">>`
  - `interface PipOpenInput { mediaUrl: string; title?: string; mime?: string }`
  - `function pipReducer(state: PipState, action: PipAction): PipState` (exported, pure)
  - `const MAX_PIPS = 4`, `const initialPipState: PipState`
  - `function PipProvider({ children }: { children: ReactNode })`
  - `function usePip(): { panes: PipPaneState[]; openPip: (input: PipOpenInput) => void; closePip: (id: string) => void; focusPip: (id: string) => void; updatePip: (id: string, patch: PipPatch) => void }`

- [ ] **Step 1: Write the pure reducer + context with inline test-notes**

Create `client/src/context/PipContext.tsx`:

```tsx
import { createContext, useContext, useReducer, useRef, useEffect, useCallback, ReactNode } from "react";
import { toast } from "../lib/toast";

export const MAX_PIPS = 4;
const BASE_Z = 300; // above ScreenShareStage (z-index: 200)

export interface PipPaneState {
  id: string;
  mediaUrl: string;
  title: string;
  pos: { x: number; y: number };
  size: { w: number; h: number };
  opacity: number;
  minimized: boolean;
  z: number;
}

export type PipPatch = Partial<Pick<PipPaneState, "pos" | "size" | "opacity" | "minimized">>;
export interface PipOpenInput { mediaUrl: string; title?: string; mime?: string }

interface PipState { panes: PipPaneState[]; nextZ: number; nextId: number }

type PipAction =
  | { type: "open"; input: PipOpenInput }
  | { type: "close"; id: string }
  | { type: "focus"; id: string }
  | { type: "update"; id: string; patch: PipPatch };

export const initialPipState: PipState = { panes: [], nextZ: BASE_Z, nextId: 1 };

/**
 * Pure reducer — the testable core of the PiP manager. No side effects (the
 * over-cap toast lives in the provider). Dedupe + cap are enforced here so the
 * state is always correct even if the provider's pre-check is racy.
 *
 * Test-notes (manually verified by inspection):
 *   - open into empty state            → 1 pane, id "pip-1", z 300, nextZ 301, nextId 2
 *   - open a 2nd distinct mediaUrl     → 2 panes, 2nd z 301, cascade pos offset
 *   - open a mediaUrl already present  → no new pane; existing pane gets top z + minimized=false
 *   - open a 5th distinct mediaUrl     → state unchanged (cap of 4)
 *   - close an id                      → that pane removed; others untouched
 *   - focus an id                      → that pane gets nextZ; nextZ increments
 *   - update {opacity}                 → only that pane's opacity changes
 */
export function pipReducer(state: PipState, action: PipAction): PipState {
  switch (action.type) {
    case "open": {
      const existing = state.panes.find((p) => p.mediaUrl === action.input.mediaUrl);
      if (existing) {
        return {
          ...state,
          nextZ: state.nextZ + 1,
          panes: state.panes.map((p) =>
            p.id === existing.id ? { ...p, z: state.nextZ, minimized: false } : p,
          ),
        };
      }
      if (state.panes.length >= MAX_PIPS) return state; // cap (toast in provider)
      const n = state.panes.length;
      const pane: PipPaneState = {
        id: `pip-${state.nextId}`,
        mediaUrl: action.input.mediaUrl,
        title: action.input.title ?? "Video",
        pos: { x: 80 + ((n * 28) % 220), y: 80 + ((n * 28) % 220) },
        size: { w: 360, h: 240 },
        opacity: 1,
        minimized: false,
        z: state.nextZ,
      };
      return { panes: [...state.panes, pane], nextZ: state.nextZ + 1, nextId: state.nextId + 1 };
    }
    case "close":
      return { ...state, panes: state.panes.filter((p) => p.id !== action.id) };
    case "focus":
      return {
        ...state,
        nextZ: state.nextZ + 1,
        panes: state.panes.map((p) => (p.id === action.id ? { ...p, z: state.nextZ } : p)),
      };
    case "update":
      return {
        ...state,
        panes: state.panes.map((p) => (p.id === action.id ? { ...p, ...action.patch } : p)),
      };
    default:
      return state;
  }
}

interface PipContextValue {
  panes: PipPaneState[];
  openPip: (input: PipOpenInput) => void;
  closePip: (id: string) => void;
  focusPip: (id: string) => void;
  updatePip: (id: string, patch: PipPatch) => void;
}

const PipContext = createContext<PipContextValue | null>(null);

export function PipProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(pipReducer, initialPipState);
  // Ref mirrors the latest state so openPip can decide whether to toast WITHOUT
  // doing a side effect inside the reducer (which StrictMode double-invokes).
  const stateRef = useRef(state);
  useEffect(() => { stateRef.current = state; }, [state]);

  const openPip = useCallback((input: PipOpenInput) => {
    const s = stateRef.current;
    const isDup = s.panes.some((p) => p.mediaUrl === input.mediaUrl);
    if (!isDup && s.panes.length >= MAX_PIPS) {
      toast.info("Close a video to open another");
      return;
    }
    dispatch({ type: "open", input });
  }, []);

  const closePip = useCallback((id: string) => dispatch({ type: "close", id }), []);
  const focusPip = useCallback((id: string) => dispatch({ type: "focus", id }), []);
  const updatePip = useCallback((id: string, patch: PipPatch) => dispatch({ type: "update", id, patch }), []);

  return (
    <PipContext.Provider value={{ panes: state.panes, openPip, closePip, focusPip, updatePip }}>
      {children}
    </PipContext.Provider>
  );
}

export function usePip(): PipContextValue {
  const ctx = useContext(PipContext);
  if (!ctx) throw new Error("usePip must be used within a PipProvider");
  return ctx;
}
```

- [ ] **Step 2: Wrap the app in `PipProvider`**

In `client/src/App.tsx`, add the import near the other imports (it imports `AppProvider` from `./context/ServerContext` at line 3):

```tsx
import { PipProvider } from "./context/PipContext";
```

Then change the `App` component (currently lines ~168-175) from:

```tsx
export default function App() {
  return (
    <AppProvider>
      <AppInner />
      <ToastContainer />
    </AppProvider>
  );
}
```

to:

```tsx
export default function App() {
  return (
    <AppProvider>
      <PipProvider>
        <AppInner />
      </PipProvider>
      <ToastContainer />
    </AppProvider>
  );
}
```

(`AppInner` renders `<AppShell />`, which contains both the future `PipLayer` and the `LinkEmbed` consumers, so wrapping `AppInner` puts every `usePip()` caller inside the provider. `ToastContainer` stays outside — `toast` works via window events.)

- [ ] **Step 3: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean (no errors). `PipContext.tsx` compiles; `App.tsx` still compiles with the new wrapper.

- [ ] **Step 4: Re-read the reducer against its test-notes**

Read the test-notes block and trace each line through `pipReducer` by hand. Confirm: empty→open gives `id: "pip-1"`/`z: 300`; duplicate open re-focuses + un-minimizes without adding; the 5th distinct open returns the same object reference (cap). Fix any mismatch.

- [ ] **Step 5: Commit**

```bash
git add client/src/context/PipContext.tsx client/src/App.tsx
git commit -m "feat(pip): PipManager context + pure reducer, wrap app in PipProvider"
```

---

### Task 2: `PipPane` component (drag / resize / opacity / minimize / close + video)

**Files:**
- Create: `client/src/components/PipPane.tsx`

**Interfaces:**
- Consumes: `PipPaneState`, `PipPatch` from `client/src/context/PipContext`; `useProxiedMedia(url: string | null, enabled: boolean): string | null` from `client/src/hooks/useProxiedMedia`.
- Produces (relied on by Task 3): `export default function PipPane(props: { pane: PipPaneState; onClose: (id: string) => void; onFocus: (id: string) => void; onUpdate: (id: string, patch: PipPatch) => void })`.

- [ ] **Step 1: Write the component**

Create `client/src/components/PipPane.tsx`. The drag logic mirrors the proven `startDrag` pattern in `client/src/components/ScreenShareStage.tsx` (window `mousemove`/`mouseup`, writing position back through the manager). Resize uses CSS `resize: both` plus a `ResizeObserver` that writes the new dimensions back so they survive re-renders (panes re-render on z-order changes; an uncontrolled CSS size would otherwise snap back to the inline width/height).

```tsx
import { useEffect, useRef } from "react";
import { useProxiedMedia } from "../hooks/useProxiedMedia";
import type { PipPaneState, PipPatch } from "../context/PipContext";

interface Props {
  pane: PipPaneState;
  onClose: (id: string) => void;
  onFocus: (id: string) => void;
  onUpdate: (id: string, patch: PipPatch) => void;
}

export default function PipPane({ pane, onClose, onFocus, onUpdate }: Props) {
  const paneRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ dx: number; dy: number } | null>(null);

  // Stream the relay-proxied bytes → blob URL (same path the inline player used).
  // Don't fetch while minimized; useProxiedMedia revokes the blob URL on cleanup.
  const mediaUrl = useProxiedMedia(pane.mediaUrl, !pane.minimized);

  // Persist user CSS `resize: both` dimensions back into state.
  useEffect(() => {
    const el = paneRef.current;
    if (!el || pane.minimized) return;
    const ro = new ResizeObserver(() => {
      const w = el.offsetWidth, h = el.offsetHeight;
      if (w && h && (w !== pane.size.w || h !== pane.size.h)) onUpdate(pane.id, { size: { w, h } });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [pane.id, pane.minimized]); // eslint-disable-line react-hooks/exhaustive-deps

  // Drag by the header (ignore clicks on a button/slider).
  const startDrag = (e: React.MouseEvent) => {
    if ((e.target as HTMLElement).closest("button, input")) return;
    const el = paneRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    dragRef.current = { dx: e.clientX - rect.left, dy: e.clientY - rect.top };
    const onMove = (ev: MouseEvent) => {
      if (!dragRef.current) return;
      onUpdate(pane.id, { pos: { x: ev.clientX - dragRef.current.dx, y: ev.clientY - dragRef.current.dy } });
    };
    const onUp = () => {
      dragRef.current = null;
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  if (pane.minimized) {
    return (
      <div
        className="pip-pane-mini"
        style={{ left: pane.pos.x, top: pane.pos.y, zIndex: pane.z }}
        onMouseDown={(e) => { onFocus(pane.id); startDrag(e); }}
      >
        <span className="pip-pane-title">{pane.title}</span>
        <button className="pip-pane-min" title="Restore" onClick={() => onUpdate(pane.id, { minimized: false })}>&#x25A2;</button>
        <button className="pip-pane-close" title="Close" onClick={() => onClose(pane.id)}>&#x2715;</button>
      </div>
    );
  }

  return (
    <div
      ref={paneRef}
      className="pip-pane"
      style={{ left: pane.pos.x, top: pane.pos.y, width: pane.size.w, height: pane.size.h, opacity: pane.opacity, zIndex: pane.z }}
      onMouseDown={() => onFocus(pane.id)}
    >
      <div className="pip-pane-head" onMouseDown={startDrag}>
        <span className="pip-pane-title">{pane.title}</span>
        <input
          className="pip-pane-opacity" type="range" min={0.2} max={1} step={0.05} value={pane.opacity}
          title="Opacity"
          onChange={(e) => onUpdate(pane.id, { opacity: Number(e.target.value) })}
        />
        <button className="pip-pane-min" title="Minimize" onClick={() => onUpdate(pane.id, { minimized: true })}>&#x2013;</button>
        <button className="pip-pane-close" title="Close" onClick={() => onClose(pane.id)}>&#x2715;</button>
      </div>
      {mediaUrl
        ? <video className="pip-pane-video" src={mediaUrl} controls autoPlay />
        : <div className="pip-pane-state">Couldn&rsquo;t load video</div>}
    </div>
  );
}
```

- [ ] **Step 2: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean. (The classes have no CSS yet — that's Task 5; the component still compiles and renders, just unstyled.)

- [ ] **Step 3: Commit**

```bash
git add client/src/components/PipPane.tsx
git commit -m "feat(pip): PipPane floating video pane (drag/resize/opacity/minimize/close)"
```

---

### Task 3: `PipLayer` and mount it in `AppShell`

**Files:**
- Create: `client/src/components/PipLayer.tsx`
- Modify: `client/src/components/AppShell.tsx` (add `<PipLayer />` next to `<ScreenShareStage />`)

**Interfaces:**
- Consumes: `usePip()` from `client/src/context/PipContext`; `PipPane` from `client/src/components/PipPane`.
- Produces: `export default function PipLayer()` (renders all open panes; the only mount point for PiP UI).

- [ ] **Step 1: Write `PipLayer`**

Create `client/src/components/PipLayer.tsx`:

```tsx
import { usePip } from "../context/PipContext";
import PipPane from "./PipPane";

// Renders every open PiP pane. Mounted once in AppShell (overlay level) so panes
// float above all views and persist across channel/server navigation.
export default function PipLayer() {
  const { panes, closePip, focusPip, updatePip } = usePip();
  if (panes.length === 0) return null;
  return (
    <>
      {panes.map((p) => (
        <PipPane key={p.id} pane={p} onClose={closePip} onFocus={focusPip} onUpdate={updatePip} />
      ))}
    </>
  );
}
```

- [ ] **Step 2: Mount it in `AppShell`**

In `client/src/components/AppShell.tsx`, add the import next to the `ScreenShareStage` import (line 5):

```tsx
import PipLayer from "./PipLayer";
```

Then in the render (next to `<ScreenShareStage voice={voice} />` at line 132), add `<PipLayer />` directly after it:

```tsx
        <ScreenShareStage voice={voice} />
        <PipLayer />
```

- [ ] **Step 3: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add client/src/components/PipLayer.tsx client/src/components/AppShell.tsx
git commit -m "feat(pip): PipLayer rendering open panes, mounted at AppShell overlay level"
```

---

### Task 4: `LinkEmbed` — inline video becomes a compact poster that opens a PiP

**Files:**
- Modify: `client/src/components/LinkEmbed.tsx`

**Interfaces:**
- Consumes: `usePip()` → `openPip(input: { mediaUrl; title?; mime? })` from `client/src/context/PipContext`.
- Produces: no new exports (behavior change only).

- [ ] **Step 1: Rewrite `LinkEmbed.tsx`**

Two changes: (1) the video bytes are **no longer fetched on card display** — `mediaBlob` is gated to inline *images* only; (2) a new `thumbBlob` gate also covers the video poster thumbnail; (3) the inline `<video>` branch is replaced by a clickable poster whose Play button calls `openPip` with the inline media URL. Image and non-inline (YouTube/Spotify) branches are unchanged. Replace the whole file with:

```tsx
import { useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { useLinkEmbed } from "../hooks/useLinkEmbed";
import { useProxiedMedia } from "../hooks/useProxiedMedia";
import { usePip } from "../context/PipContext";

export default function LinkEmbed({ url, dataSaver }: { url: string; dataSaver: boolean }) {
  // Data-saver: don't auto-load; show a chip that loads on click.
  const [loaded, setLoaded] = useState(!dataSaver);
  const state = useLinkEmbed(url, loaded);
  const { openPip } = usePip();

  // Derive embed properties safely (embed may be null until state is "ok").
  const e = state.status === "ok" ? state.embed : null;
  const inlineMedia = e?.media?.playable_inline ? e.media : null;
  const isVideo = !!inlineMedia?.mime.startsWith("video/");

  // IMPORTANT: both useProxiedMedia calls are hoisted here, before any early
  // return, so hook call order is stable across all render paths (rules of hooks).
  // The `enabled` flag gates the actual fetch — no work is done when not needed.
  //
  // Playable VIDEO no longer fetches its bytes on the card (it streams when the
  // PiP opens); only inline IMAGES fetch their media here.
  const mediaBlob = useProxiedMedia(
    inlineMedia?.url ?? null,
    loaded && state.status === "ok" && !!inlineMedia && !isVideo,
  );
  // Thumbnail: for non-inline providers (YouTube/Spotify) AND for the video poster.
  const thumbBlob = useProxiedMedia(
    e?.thumbnail ?? null,
    loaded && state.status === "ok" && !!e?.thumbnail && (!inlineMedia || isVideo),
  );

  // --- early returns (all hooks already called above) ---

  if (!loaded) {
    return (
      <button className="link-embed-chip" onClick={() => setLoaded(true)}>
        Load preview
      </button>
    );
  }
  if (state.status === "loading") {
    return <div className="link-embed link-embed-state">Loading preview&hellip;</div>;
  }
  if (state.status !== "ok" || !e) {
    // unsupported / unavailable: render nothing extra
    return null;
  }

  const openVideoPip = () => {
    if (!inlineMedia) return;
    openPip({ mediaUrl: inlineMedia.url, title: e.author ?? e.title ?? "Video", mime: inlineMedia.mime });
  };

  return (
    <div className={`link-embed link-embed--${e.provider}`}>
      {e.author && <div className="link-embed-author">{e.author}</div>}
      {e.title && <div className="link-embed-title">{e.title}</div>}
      {e.description && <div className="link-embed-desc">{e.description}</div>}

      {inlineMedia && isVideo && (
        <div className="link-embed-poster" onClick={openVideoPip}>
          {thumbBlob
            ? <img className="link-embed-thumb" src={thumbBlob} alt={e.title ?? ""} />
            : <div className="link-embed-poster-blank" />}
          <button
            className="link-embed-poster-play"
            onClick={(ev) => { ev.stopPropagation(); openVideoPip(); }}
          >
            &#9654; Play
          </button>
        </div>
      )}
      {inlineMedia && !isVideo && mediaBlob && (
        <img className="link-embed-image" src={mediaBlob} alt={e.title ?? ""} />
      )}
      {!inlineMedia && (thumbBlob || e.kind === "Video" || e.kind === "Audio") && (
        <div className="link-embed-thumb-wrap">
          {/* Thumbnail when it resolved; the Play/Open button renders regardless
              so a failed/absent thumbnail never hides the action (RC#2). */}
          {thumbBlob && <img className="link-embed-thumb" src={thumbBlob} alt={e.title ?? ""} />}
          {(e.kind === "Video" || e.kind === "Audio") && (
            <button
              className="link-embed-play"
              onClick={() => { void openExternal(e.url); }}
            >
              &#9654; {e.kind === "Video" ? "Play" : "Open"}
            </button>
          )}
        </div>
      )}
      {e.duration_secs != null && (
        <div className="link-embed-duration">{formatDuration(e.duration_secs)}</div>
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

- [ ] **Step 2: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean. (`usePip()` resolves because `LinkEmbed` renders under `AppShell`, which is inside `PipProvider` from Task 1.)

- [ ] **Step 3: Confirm no inline `<video>` remains in `LinkEmbed`**

Run: `grep -n "link-embed-video\|<video" client/src/components/LinkEmbed.tsx`
Expected: no matches (the inline player is gone; video now opens in a PiP). The `.link-embed-video` CSS class becomes unused but is left in the themes (harmless; removing it is out of scope).

- [ ] **Step 4: Commit**

```bash
git add client/src/components/LinkEmbed.tsx
git commit -m "feat(pip): LinkEmbed renders a poster for video embeds, opens PiP on Play"
```

---

### Task 5: Theme the PiP panes and the compact poster in all three themes

**Files:**
- Modify: `client/src/themes/discord-dark/theme.css`
- Modify: `client/src/themes/hello-kitty/theme.css`
- Modify: `client/src/themes/xp-luna-blue/theme.css`

**Interfaces:**
- Consumes: theme CSS variables (`--xp-border`, `--xp-panel-bg`, `--xp-text-normal`, `--xp-text-secondary`, `--xp-blue`, `--xp-sidebar`).
- Produces: styling for `.pip-pane`, `.pip-pane-head`, `.pip-pane-title`, `.pip-pane-opacity`, `.pip-pane-min`, `.pip-pane-close`, `.pip-pane-video`, `.pip-pane-state`, `.pip-pane-mini`, `.link-embed-poster`, `.link-embed-poster-blank`, `.link-embed-poster-play`.

- [ ] **Step 1: Add the CSS block to EACH theme file**

Append the **same** block to all three theme files (it is variable-driven, so it adapts per theme). Place it near the existing `.screen-stage`/`.link-embed` rules. The block:

```css
/* --- Picture-in-picture floating video panes --- */
.pip-pane {
  position: fixed; z-index: 300; display: flex; flex-direction: column;
  min-width: 220px; min-height: 140px; resize: both; overflow: hidden;
  border: 1px solid var(--xp-border); border-radius: 8px;
  background: var(--xp-panel-bg); box-shadow: 0 14px 44px rgba(0,0,0,0.55);
}
.pip-pane-head {
  display: flex; align-items: center; gap: 8px; padding: 5px 8px; font-size: 12px;
  color: var(--xp-text-normal, var(--xp-text-secondary)); cursor: move; user-select: none;
}
.pip-pane-title { flex: 1; font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.pip-pane-opacity { width: 80px; accent-color: var(--xp-blue); }
.pip-pane-min, .pip-pane-close {
  background: none; border: none; color: var(--xp-text-normal, var(--xp-text-secondary));
  cursor: pointer; font-size: 14px; line-height: 1;
}
.pip-pane-video { flex: 1; min-height: 0; width: 100%; background: #000; object-fit: contain; display: block; }
.pip-pane-state {
  flex: 1; display: flex; align-items: center; justify-content: center;
  background: #000; color: var(--xp-text-secondary); font-size: 0.9em;
}
.pip-pane-mini {
  position: fixed; z-index: 300; display: flex; align-items: center; gap: 6px;
  padding: 5px 10px; max-width: 220px; border-radius: 20px;
  background: var(--xp-panel-bg); border: 1px solid var(--xp-border);
  color: var(--xp-text-normal, var(--xp-text-secondary)); font-size: 12px;
  cursor: move; user-select: none; box-shadow: 0 6px 18px rgba(0,0,0,0.4);
}
/* Compact poster shown in chat instead of an inline video player */
.link-embed-poster { position: relative; display: inline-block; cursor: pointer; margin-top: 6px; }
.link-embed-poster-blank {
  width: 320px; max-width: 100%; height: 180px; border-radius: 4px;
  background: var(--xp-sidebar, #000);
}
.link-embed-poster-play {
  position: absolute; inset: 0; margin: auto; width: fit-content; height: fit-content;
  padding: 6px 12px; background: var(--xp-blue); color: #fff; border: none;
  border-radius: 4px; cursor: pointer;
}
```

- [ ] **Step 2: Verify all three themes define the new classes**

Run: `grep -l "pip-pane" client/src/themes/*/theme.css && grep -l "link-embed-poster" client/src/themes/*/theme.css`
Expected: both greps list all three theme files (`discord-dark`, `hello-kitty`, `xp-luna-blue`).

- [ ] **Step 3: Confirm no hard-coded colors slipped in**

Visually scan the block you added in each file. Every color is either `var(--xp-…)` or one of the accepted `#000` (video letterbox / shadow) and `#fff` (text on the `--xp-blue` accent button, matching `.link-embed-play`). No other literal colors.

- [ ] **Step 4: Commit**

```bash
git add client/src/themes/discord-dark/theme.css client/src/themes/hello-kitty/theme.css client/src/themes/xp-luna-blue/theme.css
git commit -m "feat(pip): theme PiP panes + compact video poster in all three themes"
```

---

### Task 6: Documentation

**Files:**
- Create: `client/../docs/modules/frontend-pip.md` (i.e. `docs/modules/frontend-pip.md`)
- Modify: `docs/modules/relay-embed.md` (note the LinkEmbed inline-video → poster+PiP behavior change)

**Interfaces:** none (docs only).

- [ ] **Step 1: Write the module doc**

Create `docs/modules/frontend-pip.md` following the one-feature-doc pattern of `docs/modules/frontend-toast.md`. It must let a junior dev understand the feature without reading the implementation:

```markdown
# Floating Picture-in-Picture Video (`PipManager` / `PipPane` / `PipLayer`)

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
```

- [ ] **Step 2: Note the behavior change in the embed doc**

In `docs/modules/relay-embed.md`, find where the client `LinkEmbed` rendering of playable video is described and add a sentence (adapt wording to the surrounding text):

```markdown
**Update (2026-06-21):** Playable *video* embeds no longer auto-play inline.
`LinkEmbed` now renders a compact poster (thumbnail + ▶ Play); clicking Play
opens the video in a floating in-app picture-in-picture pane (see
`docs/modules/frontend-pip.md`). The video bytes are fetched only when the PiP
opens (not on card display). Inline images and YouTube/Spotify external-open
buttons are unchanged.
```

- [ ] **Step 3: Commit**

```bash
git add docs/modules/frontend-pip.md docs/modules/relay-embed.md
git commit -m "docs(pip): document PiP context/components + LinkEmbed video behavior change"
```

---

## Final verification (before declaring the feature done in code)

- [ ] `cd client && npx tsc --noEmit` is clean.
- [ ] `grep -l "pip-pane" client/src/themes/*/theme.css` lists all three theme files; same for `link-embed-poster`.
- [ ] `grep -rn "invoke(" client/src/components/PipPane.tsx client/src/components/PipLayer.tsx client/src/context/PipContext.tsx` → no new Tauri commands introduced (client-only feature; the seam is untouched).
- [ ] Spec coverage walk: poster trigger (Task 4) ✓; relay-proxied-video-only scope (Task 4 gates on `playable_inline` + `video/` mime) ✓; multiple PiPs capped at 4 + toast (Task 1) ✓; in-app overlay with opacity (Task 2/5) ✓; per-pane controls drag/resize/opacity/minimize/close + bring-to-front (Task 2) ✓; persistence at AppShell level (Task 3) ✓; privacy unchanged (Tasks 2/4) ✓; error/dedupe/cap edge cases (Tasks 1/2) ✓; theming ×3 (Task 5) ✓; docs (Task 6) ✓.
- [ ] **Runtime (owner, Windows — UNVERIFIED until run, WSL has no display):** rebuild the client (frontend-only change, no sidecar rebuild needed); in a chat with a tweet-video / direct-video message, the card shows a poster with ▶ Play; click → a floating pane plays the video; drag by the header, resize from the corner, drag the opacity slider (chat readable behind it), minimize to a pill and restore; open a second video → two panes, clicking one brings it to front; open a 5th → toast "Close a video to open another"; switch channel/server → panes persist; ✕ closes and frees it.
```

