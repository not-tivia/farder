# Data Saver Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in "Data Saver" mode that holds large images behind a click-to-load gate, defers link previews, and freezes animated avatars — controlled by a master switch plus per-type sub-toggles and a configurable image size threshold.

**Architecture:** Pure client-side. One localStorage-backed settings store (`lib/dataSaver.ts`) is exposed through a React context (`DataSaverContext`) so every component reads it synchronously (no per-message IPC). Gating is applied at the existing render points: `AttachmentDisplay` (images), `LinkEmbed` (embeds), `MemberAvatar` (avatars). A one-time migration seeds the store from the legacy Rust `data_saver_embeds` setting.

**Tech Stack:** React 18 + TypeScript, Tauri (no Rust changes here), localStorage, Canvas 2D (first-frame avatar freeze).

## Global Constraints

- **Client-only.** No Rust, protocol, relay, or Tauri-command changes. Feature hot-reloads; no sidecar rebuild. (The legacy Rust `data_saver_embeds` command is read once by the migration and otherwise left dormant — do NOT remove it.)
- **Single source of truth:** all Data Saver state lives in `localStorage["farder.dataSaver"]`, read/written only through `lib/dataSaver.ts` + `DataSaverContext`. No `getDataSaverEmbeds()` IPC anywhere except the one-time migration inside `DataSaverProvider`.
- **No JS test runner exists** (per `CLAUDE.md`). The discipline is: write small pure functions with `Test-notes (verified by inspection)` comments (mirror `lib/mediaPrefs.ts`), and gate every task on `cd client && npx tsc --noEmit` being clean. There are no `*.test.ts` files to add.
- **Theming:** reuse existing themed classes (`.link-embed-chip` for load buttons, `.attachment-image`, `.avatar-img`, `.settings-row`, `.settings-help`). Use inline styles only for **layout** (sizing, fl/indent, opacity). Do NOT introduce a new `className` that lacks CSS, and do NOT hard-code colors. This feature should add **zero** new theme CSS.
- **Defaults:** `enabled` defaults OFF. When the master is on, sub-toggles default on and `thresholdMB` defaults to 1.

---

## File Structure

- **Create** `client/src/lib/dataSaver.ts` — settings type, defaults, localStorage get/set, and pure helpers (`thresholdBytes`, `imageIsGated`, `isAnimatedDataUrl`, `hasDataSaver`).
- **Create** `client/src/context/DataSaverContext.tsx` — `DataSaverProvider` (state + persistence + one-time migration) and `useDataSaver()` hook.
- **Modify** `client/src/App.tsx` — mount `DataSaverProvider`.
- **Modify** `client/src/components/VoiceSettings.tsx` — replace the single embeds checkbox with the master + sub-toggles + threshold control; drop the now-unused legacy state/imports.
- **Modify** `client/src/components/LinkEmbed.tsx` + `client/src/components/Message.tsx` — read `clickToLoadEmbeds` from context, drop the `dataSaver` prop and the per-message IPC.
- **Modify** `client/src/components/Message.tsx` (`AttachmentDisplay`) — image size gating.
- **Modify** `client/src/components/MemberAvatar.tsx` — freeze animated avatars to a first-frame canvas.

---

## Task 1: Data Saver store + context + provider

**Files:**
- Create: `client/src/lib/dataSaver.ts`
- Create: `client/src/context/DataSaverContext.tsx`
- Modify: `client/src/App.tsx` (provider nesting at lines ~171-176; import near line 4)

**Interfaces:**
- Produces:
  - `interface DataSaverSettings { enabled: boolean; gateImages: boolean; clickToLoadEmbeds: boolean; freezeAvatars: boolean; thresholdMB: number }`
  - `DATA_SAVER_DEFAULTS: DataSaverSettings`
  - `getDataSaver(): DataSaverSettings`, `setDataSaver(s): void`, `hasDataSaver(): boolean`
  - `thresholdBytes(s): number`, `imageIsGated(s, sizeBytes): boolean`, `isAnimatedDataUrl(url): boolean`
  - `DataSaverProvider({children})`, `useDataSaver(): { settings: DataSaverSettings; update(patch: Partial<DataSaverSettings>): void }`
- Consumes: `getDataSaverEmbeds` from `client/src/lib/tauri-bridge.ts` (migration only).

- [ ] **Step 1: Create the settings store**

Create `client/src/lib/dataSaver.ts`:

```ts
// Data Saver settings: a single localStorage-backed store, read through
// DataSaverContext. Client-only; fails safe to defaults on any error.
const KEY = "farder.dataSaver";

export interface DataSaverSettings {
  enabled: boolean;            // master switch
  gateImages: boolean;         // images over threshold -> click-to-load
  clickToLoadEmbeds: boolean;  // link previews -> "Load preview"
  freezeAvatars: boolean;      // animated avatars -> still first frame
  thresholdMB: number;         // size cutoff for images, in MB
}

export const DATA_SAVER_DEFAULTS: DataSaverSettings = {
  enabled: false,
  gateImages: true,
  clickToLoadEmbeds: true,
  freezeAvatars: true,
  thresholdMB: 1,
};

/**
 * Read settings, filling any missing keys from defaults.
 * Test-notes (verified by inspection):
 *   - nothing saved          -> DATA_SAVER_DEFAULTS
 *   - partial {enabled:true} -> defaults merged, enabled:true
 *   - invalid JSON / throws  -> DATA_SAVER_DEFAULTS
 */
export function getDataSaver(): DataSaverSettings {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...DATA_SAVER_DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<DataSaverSettings>;
    return { ...DATA_SAVER_DEFAULTS, ...parsed };
  } catch {
    return { ...DATA_SAVER_DEFAULTS };
  }
}

/** Persist settings; ignores storage errors. */
export function setDataSaver(s: DataSaverSettings): void {
  try { localStorage.setItem(KEY, JSON.stringify(s)); } catch { /* ignore */ }
}

/** True once anything has been saved (gates the one-time migration). */
export function hasDataSaver(): boolean {
  try { return localStorage.getItem(KEY) != null; } catch { return false; }
}

export function thresholdBytes(s: DataSaverSettings): number {
  return Math.max(0, s.thresholdMB) * 1024 * 1024;
}

/**
 * True when an image of sizeBytes should be held behind a click-to-load gate.
 * Test-notes (verified by inspection), threshold 1 MB:
 *   - disabled                 -> false
 *   - enabled, gateImages off  -> false
 *   - enabled, 500 KB          -> false
 *   - enabled, 4 MB            -> true
 */
export function imageIsGated(s: DataSaverSettings, sizeBytes: number): boolean {
  return s.enabled && s.gateImages && sizeBytes > thresholdBytes(s);
}

/**
 * True for animated-image data URLs we should freeze.
 * Test-notes (verified by inspection):
 *   - "data:image/gif;base64,..."  -> true
 *   - "data:image/webp;base64,..." -> true
 *   - "data:image/png;base64,..."  -> false
 *   - null/undefined               -> false
 */
export function isAnimatedDataUrl(url: string | null | undefined): boolean {
  if (!url) return false;
  return /^data:image\/(gif|apng|webp)/i.test(url);
}
```

- [ ] **Step 2: Create the context + provider**

Create `client/src/context/DataSaverContext.tsx`:

```tsx
import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import {
  type DataSaverSettings,
  DATA_SAVER_DEFAULTS,
  getDataSaver,
  setDataSaver,
  hasDataSaver,
} from "../lib/dataSaver";
import { getDataSaverEmbeds } from "../lib/tauri-bridge";

interface DataSaverCtx {
  settings: DataSaverSettings;
  update: (patch: Partial<DataSaverSettings>) => void;
}

const Ctx = createContext<DataSaverCtx>({
  settings: DATA_SAVER_DEFAULTS,
  update: () => {},
});

export function DataSaverProvider({ children }: { children: ReactNode }) {
  const [settings, setSettings] = useState<DataSaverSettings>(() => getDataSaver());

  // One-time migration from the legacy Rust `data_saver_embeds` setting:
  // only runs when nothing is stored locally yet.
  useEffect(() => {
    if (hasDataSaver()) return;
    getDataSaverEmbeds()
      .then((on) => {
        const seeded = { ...DATA_SAVER_DEFAULTS, enabled: on, clickToLoadEmbeds: on };
        setDataSaver(seeded);
        setSettings(seeded);
      })
      .catch(() => { /* defaults stand */ });
  }, []);

  const update = (patch: Partial<DataSaverSettings>) => {
    setSettings((prev) => {
      const next = { ...prev, ...patch };
      setDataSaver(next);
      return next;
    });
  };

  return <Ctx.Provider value={{ settings, update }}>{children}</Ctx.Provider>;
}

export function useDataSaver(): DataSaverCtx {
  return useContext(Ctx);
}
```

- [ ] **Step 3: Mount the provider in `App.tsx`**

Add the import after the `MediaPlayersProvider` import (currently `client/src/App.tsx:4`):

```tsx
import { DataSaverProvider } from "./context/DataSaverContext";
```

Wrap `<AppInner />` (currently `client/src/App.tsx:171-176`) so it reads:

```tsx
    <AppProvider>
      <MediaPlayersProvider>
        <DataSaverProvider>
          <AppInner />
        </DataSaverProvider>
      </MediaPlayersProvider>
    </AppProvider>
```

- [ ] **Step 4: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: no output (clean). Resolve any type errors before committing.

- [ ] **Step 5: Commit**

```bash
git add client/src/lib/dataSaver.ts client/src/context/DataSaverContext.tsx client/src/App.tsx
git commit -m "feat(data-saver): settings store + context + provider with legacy migration"
```

---

## Task 2: Settings UI (master + sub-toggles + threshold)

**Files:**
- Modify: `client/src/components/VoiceSettings.tsx` (imports ~19-20; state ~62; handler ~121-124; Privacy & Data block ~300-313)

**Interfaces:**
- Consumes: `useDataSaver()` from Task 1.

- [ ] **Step 1: Wire the context, remove the legacy embeds state**

In `client/src/components/VoiceSettings.tsx`:

1. Remove the two now-unused bridge imports `getDataSaverEmbeds,` and `setDataSaverEmbeds,` (currently lines ~19-20). (They remain exported from the bridge for the migration; VoiceSettings no longer uses them.)
2. Add at the top of the imports:

```tsx
import { useDataSaver } from "../context/DataSaverContext";
```

3. Remove the legacy state line (currently ~62):

```tsx
const [dataSaverEmbeds, setDataSaverEmbedsState] = useState<boolean>(false);
```

4. Remove the loader for it (the `void getDataSaverEmbeds().then(setDataSaverEmbedsState)...` line, currently ~77).
5. Remove the `chooseDataSaverEmbeds` handler (currently ~121-124).
6. Inside the component body (near the other `const [...]` hooks), add:

```tsx
const { settings: ds, update: updateDs } = useDataSaver();
```

- [ ] **Step 2: Replace the control markup**

Replace the legacy block (currently `client/src/components/VoiceSettings.tsx:300-313` — the `<label>` with `checked={dataSaverEmbeds}` and its following `<p className="settings-help">`) with:

```tsx
        <label className="settings-row">
          <input
            type="checkbox"
            checked={ds.enabled}
            onChange={(e) => updateDs({ enabled: e.target.checked })}
          />
          Data Saver
        </label>
        <div style={{ marginLeft: 22, opacity: ds.enabled ? 1 : 0.5 }}>
          <label className="settings-row">
            <input
              type="checkbox"
              checked={ds.gateImages}
              disabled={!ds.enabled}
              onChange={(e) => updateDs({ gateImages: e.target.checked })}
            />
            Don&rsquo;t auto-load large images
          </label>
          <label className="settings-row">
            <input
              type="checkbox"
              checked={ds.clickToLoadEmbeds}
              disabled={!ds.enabled}
              onChange={(e) => updateDs({ clickToLoadEmbeds: e.target.checked })}
            />
            Click-to-load link previews
          </label>
          <label className="settings-row">
            <input
              type="checkbox"
              checked={ds.freezeAvatars}
              disabled={!ds.enabled}
              onChange={(e) => updateDs({ freezeAvatars: e.target.checked })}
            />
            Freeze animated avatars
          </label>
          <label className="settings-row">
            Auto-load media up to&nbsp;
            <input
              type="number"
              min={0}
              step={0.5}
              value={ds.thresholdMB}
              disabled={!ds.enabled || !ds.gateImages}
              onChange={(e) => updateDs({ thresholdMB: Math.max(0, parseFloat(e.target.value) || 0) })}
              style={{ width: 56 }}
            />
            &nbsp;MB
          </label>
        </div>
        <p className="settings-help">
          When on, large images show a &ldquo;Load image&rdquo; button instead of
          downloading automatically, link previews wait for a click, and animated
          avatars are shown as a still frame. Small files load normally.
        </p>
```

(The `ytEmbeds`/`spotifyEmbeds`/`alwaysFloat`/presence rows that follow are unrelated — leave them unchanged.)

- [ ] **Step 3: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean. In particular confirm no "declared but never read" errors for the removed `dataSaverEmbeds`/`getDataSaverEmbeds`/`setDataSaverEmbeds`.

- [ ] **Step 4: Commit**

```bash
git add client/src/components/VoiceSettings.tsx
git commit -m "feat(data-saver): master + sub-toggle + threshold settings UI"
```

---

## Task 3: Rewire embeds to the context (drop per-message IPC)

**Files:**
- Modify: `client/src/components/LinkEmbed.tsx` (signature line 11; `useState` line 12)
- Modify: `client/src/components/Message.tsx` (import line 20; state lines ~183-187; call site line ~494)

**Interfaces:**
- Consumes: `useDataSaver()` from Task 1.

- [ ] **Step 1: `LinkEmbed` reads context, drops the prop**

In `client/src/components/LinkEmbed.tsx`, add to the imports:

```tsx
import { useDataSaver } from "../context/DataSaverContext";
```

Change the signature + first line (currently lines 11-12) from:

```tsx
export default function LinkEmbed({ url, dataSaver }: { url: string; dataSaver: boolean }) {
  const [loaded, setLoaded] = useState(!dataSaver);
```

to:

```tsx
export default function LinkEmbed({ url }: { url: string }) {
  const { settings } = useDataSaver();
  // Captured once on mount (matches prior behavior); newly-rendered embeds
  // pick up a toggled setting, already-mounted ones keep their state.
  const [loaded, setLoaded] = useState(!settings.clickToLoadEmbeds);
```

Leave the rest of `LinkEmbed` unchanged.

- [ ] **Step 2: `Message` drops the IPC + prop**

In `client/src/components/Message.tsx`:

1. Remove the import (currently line 20):

```tsx
import { getDataSaverEmbeds } from "../lib/tauri-bridge";
```

2. Remove the state + effect (currently lines ~183-187):

```tsx
const [dataSaver, setDataSaver] = useState(false);

useEffect(() => {
  getDataSaverEmbeds().then(setDataSaver).catch(() => {});
}, []);
```

3. Change the call site (currently line ~494) from:

```tsx
{urls.map((u, i) => <LinkEmbed key={i} url={u} dataSaver={dataSaver} />)}
```

to:

```tsx
{urls.map((u, i) => <LinkEmbed key={i} url={u} />)}
```

- [ ] **Step 3: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean. Confirm no leftover references to `dataSaver` or `getDataSaverEmbeds` in `Message.tsx` (`grep -n "dataSaver\|getDataSaverEmbeds" client/src/components/Message.tsx` should return nothing).

- [ ] **Step 4: Commit**

```bash
git add client/src/components/LinkEmbed.tsx client/src/components/Message.tsx
git commit -m "feat(data-saver): embeds read context, drop per-message IPC"
```

---

## Task 4: Image size gating in `AttachmentDisplay`

**Files:**
- Modify: `client/src/components/Message.tsx` (`AttachmentDisplay`, lines ~637-730; `formatSize` already exists at line 67)

**Interfaces:**
- Consumes: `useDataSaver()` and `imageIsGated()` from Task 1; `imageCache` (module-level `Map<number,string>` at `Message.tsx:74`); `formatSize(bytes): string` (`Message.tsx:67`).

- [ ] **Step 1: Add imports**

In `client/src/components/Message.tsx` imports, add (if not already present from Task 3):

```tsx
import { useDataSaver } from "../context/DataSaverContext";
import { imageIsGated } from "../lib/dataSaver";
```

- [ ] **Step 2: Compute the gate + guard auto-download**

Inside `AttachmentDisplay`, just after the existing `isImage`/`isAudio` consts (currently ~652-653), add:

```tsx
const { settings: ds } = useDataSaver();
const [userLoaded, setUserLoaded] = useState(false);
const gated =
  isImage &&
  !userLoaded &&
  !imageCache.has(attachment.file_id) &&
  imageIsGated(ds, attachment.size);
```

Then update the download `useEffect` (currently ~655-669) so it does NOT fetch while gated. Change its body's first guard and deps:

```tsx
  useEffect(() => {
    if (!isImage && !isAudio) return;
    if (gated) return;
    const cached = imageCache.get(attachment.file_id);
    if (cached) {
      setImageUrl(cached);
      return;
    }
    setLoading(true);
    api.downloadFile(serverId, attachment.file_id).then((r) => {
      if (r.data_url) {
        imageCache.set(attachment.file_id, r.data_url);
        setImageUrl(r.data_url);
      }
    }).catch(() => {}).finally(() => setLoading(false));
  }, [attachment.file_id, isImage, isAudio, serverId, gated]);
```

(Adding `gated` to the deps means clicking "Load" — which sets `userLoaded` → flips `gated` to false — re-runs the effect and downloads.)

- [ ] **Step 3: Render the load-gate placeholder**

Add this block at the TOP of `AttachmentDisplay`'s return section — immediately before the existing `if ((isImage || isAudio) && loading) {` line (currently ~702):

```tsx
  if (isImage && gated) {
    // Reserve the image's real footprint so loading doesn't shift layout,
    // capped to the same 400x300 max the loaded <img> uses.
    const maxW = 400, maxH = 300;
    let w = attachment.width ?? 0;
    let h = attachment.height ?? 0;
    if (w > 0 && h > 0) {
      const scale = Math.min(1, maxW / w, maxH / h);
      w = Math.round(w * scale);
      h = Math.round(h * scale);
    } else {
      w = 200; h = 150;
    }
    return (
      <div
        className="attachment-image"
        style={{ width: w, height: h, display: "flex", alignItems: "center", justifyContent: "center" }}
      >
        <button className="link-embed-chip" onClick={() => setUserLoaded(true)}>
          &#11015; Load image ({formatSize(attachment.size)})
        </button>
      </div>
    );
  }
```

- [ ] **Step 4: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add client/src/components/Message.tsx
git commit -m "feat(data-saver): click-to-load gate for large images"
```

---

## Task 5: Freeze animated avatars

**Files:**
- Modify: `client/src/components/MemberAvatar.tsx` (full rewrite of the 20-line component)

**Interfaces:**
- Consumes: `useDataSaver()` and `isAnimatedDataUrl()` from Task 1.

- [ ] **Step 1: Rewrite `MemberAvatar` with a frozen-canvas branch**

Replace the entire contents of `client/src/components/MemberAvatar.tsx` with:

```tsx
import { useEffect, useRef, useState } from "react";
import { useMemberProfile } from "../hooks/useMemberProfile";
import { useDataSaver } from "../context/DataSaverContext";
import { isAnimatedDataUrl } from "../lib/dataSaver";

interface Props {
  serverId: string;
  publicKey?: string;            // omit when unknown -> always letter fallback
  profileHash?: string | null;
  name: string;
  className: string;             // keeps each site's existing class (member-avatar-mini, message-avatar, ...)
}

export default function MemberAvatar({ serverId, publicKey, profileHash, name, className }: Props) {
  const { avatarUrl } = useMemberProfile(serverId, publicKey ?? "", publicKey ? profileHash : null);
  const { settings } = useDataSaver();
  const freeze = settings.enabled && settings.freezeAvatars && isAnimatedDataUrl(avatarUrl);

  return (
    <span className={className}>
      {!avatarUrl
        ? (name || "?").charAt(0).toUpperCase()
        : freeze
          ? <FrozenAvatar src={avatarUrl} />
          : <img className="avatar-img" src={avatarUrl} alt="" />}
    </span>
  );
}

// Draws the first frame of an animated image into a canvas so it stops moving.
// Pure render-time: the bytes are already downloaded/cached by profile-sync.
function FrozenAvatar({ src }: { src: string }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    setFailed(false);
    const img = new Image();
    img.onload = () => {
      const c = canvasRef.current;
      if (!c) return;
      const w = img.naturalWidth || 64;
      const h = img.naturalHeight || 64;
      c.width = w;
      c.height = h;
      const ctx = c.getContext("2d");
      if (!ctx) { setFailed(true); return; }
      try { ctx.drawImage(img, 0, 0, w, h); } catch { setFailed(true); }
    };
    img.onerror = () => setFailed(true);
    img.src = src;
    return () => { img.onload = null; img.onerror = null; };
  }, [src]);

  // Fall back to the (animated) image rather than a blank avatar on any failure.
  if (failed) return <img className="avatar-img" src={src} alt="" />;
  return <canvas ref={canvasRef} className="avatar-img" />;
}
```

(The canvas reuses the existing `.avatar-img` class so it inherits each site's size/border-radius — no new CSS.)

- [ ] **Step 2: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add client/src/components/MemberAvatar.tsx
git commit -m "feat(data-saver): freeze animated avatars to a still first frame"
```

---

## Final verification (manual — owner, client-only)

After all tasks, the owner verifies on Windows with `git pull` + `Ctrl+Shift+R` (no sidecar rebuild):

1. Master off → everything auto-loads exactly as before.
2. Master on: a large image (> threshold) shows `⬇ Load image (X MB)` at the right size; clicking loads it in place. A small image still auto-loads.
3. Change the threshold (e.g. to 0.1) → a previously-auto image now shows the button; raise it back → auto-loads.
4. Link previews show "Load preview"; clicking loads.
5. An animated-GIF avatar stops animating but stays visible; turning the toggle off re-animates it (after a re-render).
6. Settings persist across an app restart; a prior `data_saver_embeds=on` carried over (enabled + click-to-load embeds on) on first run.

Confirm zero new theme CSS was needed: `grep -rl "ds-suboption\|attachment-load-gate" client/src/themes/` returns nothing (we reused `.link-embed-chip` / `.attachment-image` / `.avatar-img` and inline layout).
