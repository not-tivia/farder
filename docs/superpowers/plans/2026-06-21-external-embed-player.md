# In-App External Embed Player (opt-in iframe) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users watch YouTube/Spotify embeds inside Farder via an opt-in, per-provider-consented, sandboxed inline iframe — privacy-by-default (nothing external loads until the user clicks "Watch here" and, the first time, accepts a warning).

**Architecture:** Entirely client-side. A new pure helper module parses the provider/id from the embed URL and builds the official embed src, and stores per-provider consent in `localStorage`. `LinkEmbed` gains a "Watch here" flow that (after consent) swaps the thumbnail for a sandboxed `<iframe>`; a consent modal handles the first-time warning; Settings gets revoke toggles. No relay, protocol, or Tauri-command changes.

**Tech Stack:** React 18 + TypeScript, Tauri (`@tauri-apps/plugin-shell` for external open — already imported in LinkEmbed), per-theme CSS.

## Global Constraints

- **Client-only.** No changes to Rust crates, the relay, the protocol, or any `#[tauri::command]` / `generate_handler!` list. Consent lives in `localStorage`, not in any Tauri-backed setting.
- **Privacy-by-default.** On render, YouTube/Spotify cards show only relay-proxied metadata (title/thumbnail) — no connection to Google/Spotify. The iframe loads ONLY after an explicit "Watch here" click (and first-time consent). Collapsing the player unmounts the iframe.
- **Consent fails closed.** `getEmbedConsent` returns `false` on any storage error (worst case: the user is asked again — never that something loads without consent).
- **Hardening (exact values):** YouTube embeds use host `https://www.youtube-nocookie.com/embed/<id>`. The iframe MUST set `sandbox="allow-scripts allow-same-origin allow-presentation"`, `referrerPolicy="no-referrer"`, `allow="encrypted-media; fullscreen; picture-in-picture"`, `loading="lazy"`, `allowFullScreen`.
- **Prefer existing classes (CLAUDE.md).** Reuse `modal-overlay`/`modal-dialog`/`modal-titlebar`/`modal-close`/`modal-body`/`connect-actions`/`xp-button`/`settings-row`/`settings-help`/`link-embed-play`/`link-embed-thumb`/`link-embed-thumb-wrap` rather than inventing equivalents. New CSS classes must be added to ALL THREE themes (`discord-dark`, `hello-kitty`, `xp-luna-blue`), variable-driven, no hard-coded colors except `#000` (iframe letterbox) and `#fff`-on-`--xp-blue`. `xp-luna-blue` lacks `--xp-text-normal` → use `var(--xp-text-normal, var(--xp-text-secondary))`.
- **No JS test runner exists.** "Tests" = `cd client && npx tsc --noEmit` clean, plus pure helpers carry inline test-notes verified by inspection (mirroring `detectEmbedUrls` in `client/src/lib/linkEmbed.ts`). Do not add a test runner.
- **Spec:** `docs/superpowers/specs/2026-06-21-external-embed-player-design.md`.
- **Spotify in-app = 30s preview** (the webview isn't logged in); surface a "30-second preview" note. Full track stays "Open externally".

## File Structure

- `client/src/lib/embedPlayer.ts` (new) — pure: provider/id parsing + embed src builder + `localStorage` consent store.
- `client/src/components/EmbedConsentModal.tsx` (new) — first-time warning modal (reuses existing modal classes).
- `client/src/components/LinkEmbed.tsx` (modify) — "Watch here" flow + sandboxed iframe + "Open externally" secondary action for YouTube/Spotify.
- `client/src/components/VoiceSettings.tsx` (modify) — two revoke toggles in the existing "Privacy & Data" section.
- `client/src/themes/{discord-dark,hello-kitty,xp-luna-blue}/theme.css` (modify) — the new player-container classes.
- `docs/modules/relay-embed.md` (modify) — document the opt-in player + consent keys.

---

### Task 1: `embedPlayer.ts` — provider/id parsing + consent store

**Files:**
- Create: `client/src/lib/embedPlayer.ts`

**Interfaces:**
- Consumes: nothing (pure; uses the global `URL` and `localStorage`).
- Produces (used by Tasks 2, 3, 4):
  - `type EmbedProvider = "youtube" | "spotify"`
  - `function buildEmbedPlayerSrc(url: string): { provider: EmbedProvider; src: string } | null`
  - `function getEmbedConsent(p: EmbedProvider): boolean`
  - `function setEmbedConsent(p: EmbedProvider, allowed: boolean): void`
  - `function providerLabel(p: EmbedProvider): string`

- [ ] **Step 1: Write the module with inline test-notes**

Create `client/src/lib/embedPlayer.ts`:

```ts
// Opt-in in-app players for YouTube/Spotify embeds. Pure helpers + a tiny
// localStorage-backed consent store. No React, no Tauri — keeps the feature
// seam-free (see CLAUDE.md on the untyped Tauri seam). Consent defaults to
// DENIED and fails closed on any storage error (privacy-safe).

export type EmbedProvider = "youtube" | "spotify";

const CONSENT_KEY: Record<EmbedProvider, string> = {
  youtube: "farder.embedConsent.youtube",
  spotify: "farder.embedConsent.spotify",
};

export function providerLabel(p: EmbedProvider): string {
  return p === "youtube" ? "YouTube" : "Spotify";
}

function ytSrc(id: string): string {
  return `https://www.youtube-nocookie.com/embed/${encodeURIComponent(id)}`;
}

/**
 * Detect the provider from the URL host and build the official embed-player src,
 * or return null if the host is unsupported or the id can't be parsed.
 *
 * Test-notes (verified by inspection):
 *   - "https://youtu.be/abc123"                      -> {youtube, https://www.youtube-nocookie.com/embed/abc123}
 *   - "https://www.youtube.com/watch?v=abc123&t=10"  -> {youtube, .../embed/abc123}
 *   - "https://youtube.com/shorts/abc123"            -> {youtube, .../embed/abc123}
 *   - "https://open.spotify.com/track/xyz789?si=1"   -> {spotify, https://open.spotify.com/embed/track/xyz789}
 *   - "https://open.spotify.com/intl-de/album/xyz"   -> {spotify, .../embed/album/xyz}  (locale prefix tolerated)
 *   - "https://example.com/x"                         -> null  (unsupported host)
 *   - "not a url"                                      -> null  (URL ctor throws, caught)
 *   - "https://www.youtube.com/feed/subscriptions"    -> null  (no video id)
 */
export function buildEmbedPlayerSrc(url: string): { provider: EmbedProvider; src: string } | null {
  let u: URL;
  try { u = new URL(url); } catch { return null; }
  const host = u.host.toLowerCase();

  if (host === "youtu.be") {
    const id = u.pathname.split("/").filter(Boolean)[0];
    return id ? { provider: "youtube", src: ytSrc(id) } : null;
  }
  if (host === "youtube.com" || host === "www.youtube.com" || host === "m.youtube.com") {
    const v = u.searchParams.get("v");
    if (v) return { provider: "youtube", src: ytSrc(v) };
    const parts = u.pathname.split("/").filter(Boolean); // ["shorts","abc"] | ["embed","abc"]
    if ((parts[0] === "shorts" || parts[0] === "embed") && parts[1]) {
      return { provider: "youtube", src: ytSrc(parts[1]) };
    }
    return null;
  }

  if (host === "open.spotify.com") {
    const parts = u.pathname.split("/").filter(Boolean); // ["track","xyz"] | ["intl-de","track","xyz"]
    const allowed = ["track", "album", "playlist", "episode", "show"];
    const ti = parts.findIndex((s) => allowed.includes(s));
    if (ti >= 0 && parts[ti + 1]) {
      return { provider: "spotify", src: `https://open.spotify.com/embed/${parts[ti]}/${encodeURIComponent(parts[ti + 1])}` };
    }
    return null;
  }

  return null;
}

/** Read per-provider consent. Fails CLOSED (returns false) on any error. */
export function getEmbedConsent(p: EmbedProvider): boolean {
  try { return localStorage.getItem(CONSENT_KEY[p]) === "1"; } catch { return false; }
}

/** Persist per-provider consent. Swallows storage errors. */
export function setEmbedConsent(p: EmbedProvider, allowed: boolean): void {
  try {
    if (allowed) localStorage.setItem(CONSENT_KEY[p], "1");
    else localStorage.removeItem(CONSENT_KEY[p]);
  } catch { /* ignore */ }
}
```

- [ ] **Step 2: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean (no errors).

- [ ] **Step 3: Trace the test-notes by hand**

Read each test-note line and trace it through `buildEmbedPlayerSrc`. Confirm: `youtu.be/abc123` → embed/abc123; `watch?v=abc123&t=10` → embed/abc123 (query stripped); `intl-de/album/xyz` → embed/album/xyz (findIndex tolerates the locale prefix); `feed/subscriptions` → null (no `v`, parts[0] not shorts/embed); `"not a url"` → null (caught). Fix any mismatch.

- [ ] **Step 4: Commit**

```bash
git add client/src/lib/embedPlayer.ts
git commit -m "feat(embed-player): URL->embed-src parser + localStorage consent store"
```

---

### Task 2: `EmbedConsentModal` — first-time warning

**Files:**
- Create: `client/src/components/EmbedConsentModal.tsx`

**Interfaces:**
- Consumes: `providerLabel`, `EmbedProvider` from `client/src/lib/embedPlayer`.
- Produces (used by Task 3): `export default function EmbedConsentModal(props: { provider: EmbedProvider; onConfirm: (alwaysAllow: boolean) => void; onCancel: () => void })`.

- [ ] **Step 1: Write the component**

Reuses the existing modal classes (`modal-overlay`/`modal-dialog`/`modal-titlebar`/`modal-close`/`modal-body`/`connect-actions`/`xp-button`) and `settings-row` — all already styled in every theme (see `JoinConfirmModal.tsx`). No new CSS. Create `client/src/components/EmbedConsentModal.tsx`:

```tsx
import { useState } from "react";
import { providerLabel, type EmbedProvider } from "../lib/embedPlayer";

export default function EmbedConsentModal({
  provider,
  onConfirm,
  onCancel,
}: {
  provider: EmbedProvider;
  onConfirm: (alwaysAllow: boolean) => void;
  onCancel: () => void;
}) {
  const [alwaysAllow, setAlwaysAllow] = useState(false);
  const label = providerLabel(provider);
  return (
    <div className="modal-overlay" onClick={onCancel}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>Watch in Farder?</span>
          <button className="modal-close" onClick={onCancel}>X</button>
        </div>
        <div className="modal-body">
          <p>
            Playing this connects you directly to <strong>{label}</strong> and
            shares your IP address and viewing data with them. Farder can&apos;t
            hide this while the video plays.
          </p>
          <label className="settings-row">
            <input
              type="checkbox"
              checked={alwaysAllow}
              onChange={(e) => setAlwaysAllow(e.target.checked)}
            />
            Always allow {label} embeds
          </label>
          <div className="connect-actions">
            <button className="xp-button" onClick={() => onConfirm(alwaysAllow)}>Watch</button>
            <button className="xp-button" onClick={onCancel}>Cancel</button>
          </div>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add client/src/components/EmbedConsentModal.tsx
git commit -m "feat(embed-player): EmbedConsentModal first-time privacy warning"
```

---

### Task 3: `LinkEmbed` — "Watch here" flow + sandboxed iframe

**Files:**
- Modify: `client/src/components/LinkEmbed.tsx`

**Interfaces:**
- Consumes: `buildEmbedPlayerSrc`, `getEmbedConsent`, `setEmbedConsent`, `providerLabel` from `client/src/lib/embedPlayer`; `EmbedConsentModal` from `client/src/components/EmbedConsentModal`.
- Produces: no new exports (behavior change only).

- [ ] **Step 1: Add imports**

At the top of `client/src/components/LinkEmbed.tsx`, after the existing imports (the last is `import { usePip } from "../context/PipContext";`), add:

```tsx
import EmbedConsentModal from "./EmbedConsentModal";
import { buildEmbedPlayerSrc, getEmbedConsent, setEmbedConsent, providerLabel } from "../lib/embedPlayer";
```

- [ ] **Step 2: Hoist the new state (before the early returns)**

Immediately after the existing `const { openPip } = usePip();` line, add the two state hooks (kept above all early returns to preserve rules-of-hooks):

```tsx
  const [watching, setWatching] = useState(false);
  const [showConsent, setShowConsent] = useState(false);
```

- [ ] **Step 3: Compute the player + add the watch handler (after the `if (state.status !== "ok" || !e) return null;` guard)**

Right after the existing `const openVideoPip = () => { ... };` block, add:

```tsx
  // YouTube/Spotify get an opt-in in-app iframe player; null for any other URL.
  const player = buildEmbedPlayerSrc(e.url);
  const watchHere = () => {
    if (!player) return;
    if (getEmbedConsent(player.provider)) setWatching(true);
    else setShowConsent(true);
  };
```

- [ ] **Step 4: Replace the non-inline (YouTube/Spotify) render branch**

Find this existing block:

```tsx
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
```

Replace it with the player-aware version (the `player` case offers Watch-here + Open-externally; the non-player case is the original behavior unchanged):

```tsx
      {!inlineMedia && player && (
        <div className="link-embed-player-wrap">
          {watching ? (
            <div className="embed-player">
              <button className="embed-player-close" title="Stop watching" onClick={() => setWatching(false)}>&#x2715;</button>
              <iframe
                className="embed-player-frame"
                style={{ height: player.provider === "spotify" ? 152 : 270 }}
                src={player.src}
                title={e.title ?? providerLabel(player.provider)}
                sandbox="allow-scripts allow-same-origin allow-presentation"
                referrerPolicy="no-referrer"
                allow="encrypted-media; fullscreen; picture-in-picture"
                loading="lazy"
                allowFullScreen
              />
              {player.provider === "spotify" && (
                <div className="embed-player-note">30-second preview in Farder &mdash; open externally for the full track.</div>
              )}
            </div>
          ) : (
            <div className="link-embed-thumb-wrap">
              {thumbBlob && <img className="link-embed-thumb" src={thumbBlob} alt={e.title ?? ""} />}
              <button className="link-embed-play" onClick={watchHere}>&#9654; Watch here</button>
            </div>
          )}
          <button className="embed-open-external" onClick={() => { void openExternal(e.url); }}>Open externally &#8599;</button>
        </div>
      )}
      {!inlineMedia && !player && (thumbBlob || e.kind === "Video" || e.kind === "Audio") && (
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
```

- [ ] **Step 5: Render the consent modal**

Just before the final closing `</div>` of the returned `link-embed` container (after the `{e.duration_secs != null && (...)}` block), add:

```tsx
      {showConsent && player && (
        <EmbedConsentModal
          provider={player.provider}
          onConfirm={(always) => {
            if (always) setEmbedConsent(player.provider, true);
            setShowConsent(false);
            setWatching(true);
          }}
          onCancel={() => setShowConsent(false)}
        />
      )}
```

- [ ] **Step 6: Type-check + confirm the iframe hardening is present**

Run: `cd client && npx tsc --noEmit`
Expected: clean.

Run: `grep -n 'sandbox=\|youtube-nocookie\|referrerPolicy\|<iframe' client/src/components/LinkEmbed.tsx`
Expected: the `<iframe` is present with `sandbox="allow-scripts allow-same-origin allow-presentation"` and `referrerPolicy="no-referrer"`; `youtube-nocookie` appears via the imported builder (it won't be in this file — it's in embedPlayer.ts; that's expected). Put the grep output in your report.

- [ ] **Step 7: Commit**

```bash
git add client/src/components/LinkEmbed.tsx
git commit -m "feat(embed-player): LinkEmbed Watch-here flow + sandboxed iframe for YouTube/Spotify"
```

---

### Task 4: Settings revoke toggles in `VoiceSettings`

**Files:**
- Modify: `client/src/components/VoiceSettings.tsx`

**Interfaces:**
- Consumes: `getEmbedConsent`, `setEmbedConsent` from `client/src/lib/embedPlayer`.
- Produces: nothing (UI only).

- [ ] **Step 1: Add the import**

In `client/src/components/VoiceSettings.tsx`, add near the other `../lib/...` imports:

```tsx
import { getEmbedConsent, setEmbedConsent } from "../lib/embedPlayer";
```

- [ ] **Step 2: Add state + load-on-mount**

With the other `useState` declarations (near `const [dataSaverEmbeds, setDataSaverEmbedsState] = useState<boolean>(false);`), add:

```tsx
  const [ytEmbeds, setYtEmbeds] = useState<boolean>(false);
  const [spotifyEmbeds, setSpotifyEmbeds] = useState<boolean>(false);
```

Add a mount effect (place it next to the existing settings-loading `useEffect` near the top of the component body — it must be unconditional, like the others):

```tsx
  useEffect(() => {
    setYtEmbeds(getEmbedConsent("youtube"));
    setSpotifyEmbeds(getEmbedConsent("spotify"));
  }, []);
```

- [ ] **Step 3: Add the change handlers**

Near `const chooseDataSaverEmbeds = (enabled: boolean) => { ... };`, add:

```tsx
  const chooseYtEmbeds = (enabled: boolean) => {
    setYtEmbeds(enabled);
    setEmbedConsent("youtube", enabled);
  };
  const chooseSpotifyEmbeds = (enabled: boolean) => {
    setSpotifyEmbeds(enabled);
    setEmbedConsent("spotify", enabled);
  };
```

- [ ] **Step 4: Add the toggles to the "Privacy & Data" section**

In the `<SettingsSection label="Privacy &amp; Data">` block, after the existing data-saver `<label className="settings-row">…</label>` and its `<p className="settings-help">…</p>`, add (still inside that `SettingsSection`):

```tsx
        <label className="settings-row">
          <input
            type="checkbox"
            checked={ytEmbeds}
            onChange={(e) => chooseYtEmbeds(e.target.checked)}
          />
          Allow YouTube embeds (sends your IP to YouTube when you watch)
        </label>
        <label className="settings-row">
          <input
            type="checkbox"
            checked={spotifyEmbeds}
            onChange={(e) => chooseSpotifyEmbeds(e.target.checked)}
          />
          Allow Spotify embeds (sends your IP to Spotify when you watch)
        </label>
        <p className="settings-help">
          When off, the first time you click &ldquo;Watch here&rdquo; on a YouTube or
          Spotify card Farder asks before connecting. Turn on to skip that prompt for
          that provider. You can turn it back off here at any time.
        </p>
```

- [ ] **Step 5: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add client/src/components/VoiceSettings.tsx
git commit -m "feat(embed-player): per-provider embed consent revoke toggles in Settings"
```

---

### Task 5: Theme the player container in all three themes

**Files:**
- Modify: `client/src/themes/discord-dark/theme.css`
- Modify: `client/src/themes/hello-kitty/theme.css`
- Modify: `client/src/themes/xp-luna-blue/theme.css`

**Interfaces:**
- Consumes: theme CSS variables (`--xp-border`, `--xp-panel-bg`, `--xp-text-normal`, `--xp-text-secondary`, `--xp-blue`).
- Produces: styling for `.link-embed-player-wrap`, `.embed-player`, `.embed-player-frame`, `.embed-player-close`, `.embed-player-note`, `.embed-open-external`. (The "Watch here" button reuses `.link-embed-play`; the modal reuses existing `modal-*`/`settings-row` classes — no new CSS for those.)

- [ ] **Step 1: Append the same block to EACH theme file**

Add this block near the existing `.link-embed` rules in all three files (it is variable-driven, so it adapts per theme). The iframe's height is set inline per provider in `LinkEmbed.tsx` (layout); CSS handles the visual chrome:

```css
/* --- Opt-in in-app external embed player (YouTube/Spotify iframe) --- */
.link-embed-player-wrap { display: flex; flex-direction: column; gap: 4px; margin-top: 6px; align-items: flex-start; }
.embed-player { position: relative; width: 100%; max-width: 480px; }
.embed-player-frame { width: 100%; border: 0; display: block; border-radius: 4px; background: #000; }
.embed-player-close {
  position: absolute; top: 4px; right: 4px; z-index: 1;
  padding: 2px 6px; font-size: 12px; line-height: 1; cursor: pointer;
  border: 1px solid var(--xp-border); border-radius: 4px;
  background: var(--xp-panel-bg); color: var(--xp-text-normal, var(--xp-text-secondary));
}
.embed-player-note { color: var(--xp-text-secondary); font-size: 0.8em; margin-top: 2px; }
.embed-open-external {
  background: none; border: none; padding: 0; cursor: pointer;
  color: var(--xp-blue); font-size: 0.85em;
}
```

- [ ] **Step 2: Verify all three themes define the classes**

Run: `grep -l "embed-player-frame" client/src/themes/*/theme.css`
Expected: lists all three theme files. Put the output in your report.

- [ ] **Step 3: Confirm no disallowed hard-coded colors**

Scan the block in each file: every color is `var(--xp-…)` except `#000` (iframe letterbox background) — which is the accepted exception. No other literal colors.

- [ ] **Step 4: Commit**

```bash
git add client/src/themes/discord-dark/theme.css client/src/themes/hello-kitty/theme.css client/src/themes/xp-luna-blue/theme.css
git commit -m "feat(embed-player): theme the in-app embed player container in all three themes"
```

---

### Task 6: Documentation

**Files:**
- Modify: `docs/modules/relay-embed.md`

**Interfaces:** none (docs only).

- [ ] **Step 1: Add the feature note**

In `docs/modules/relay-embed.md`, find the section describing the client `LinkEmbed` rendering of YouTube/Spotify (near where the PiP/embed behavior is described) and add:

```markdown
**Update (2026-06-21) — opt-in in-app player:** YouTube and Spotify cards now
offer a **"Watch here"** button alongside "Open externally". Clicking it loads a
**sandboxed iframe** inline (YouTube via `youtube-nocookie.com`; Spotify via
`open.spotify.com/embed`, which is a 30s preview in-app). Privacy-by-default:
nothing external loads until the click, and the FIRST click per provider shows a
warning (`EmbedConsentModal`) with an "always allow" checkbox. Consent is stored
client-side in `localStorage` keys `farder.embedConsent.youtube` /
`farder.embedConsent.spotify` (no Tauri/relay involvement) and is revocable in
Settings → Privacy & Data. URL→embed-src parsing and the consent store live in
`client/src/lib/embedPlayer.ts`. The relay-proxied (Invidious-style) path that
would hide the viewer's IP from the provider is explicitly deferred (possible v2).
```

- [ ] **Step 2: Read it back in context**

Read the edited section of `docs/modules/relay-embed.md` and confirm the note fits cleanly (not dropped mid-sentence) and the facts match Tasks 1-5 (the `localStorage` keys, the `youtube-nocookie.com` host, the iframe sandbox).

- [ ] **Step 3: Commit**

```bash
git add docs/modules/relay-embed.md
git commit -m "docs(embed-player): document opt-in in-app YouTube/Spotify player + consent keys"
```

---

## Final verification (before declaring the feature done in code)

- [ ] `cd client && npx tsc --noEmit` is clean.
- [ ] `grep -l "embed-player-frame" client/src/themes/*/theme.css` lists all three theme files.
- [ ] `grep -rn "invoke(" client/src/lib/embedPlayer.ts client/src/components/EmbedConsentModal.tsx` → no new Tauri commands (client-only; the seam is untouched).
- [ ] `grep -n 'sandbox=\|referrerPolicy=\|allow=\|loading=' client/src/components/LinkEmbed.tsx` → the iframe carries all four hardening attributes.
- [ ] Spec coverage walk: Watch-here inline player (Task 3) ✓; both YouTube + Spotify (Task 1 parser + Task 3 render, Spotify note) ✓; privacy-by-default + warn-once-per-provider + remembered + revocable (Tasks 1/2/3/4) ✓; youtube-nocookie + sandbox/referrer hardening (Tasks 1/3) ✓; consent in localStorage, client-only (Task 1) ✓; graceful fallback when unparseable (Task 3 `!player` branch) ✓; theming ×3 (Task 5) ✓; docs (Task 6) ✓.
- [ ] **Runtime (owner, Windows — UNVERIFIED until run, WSL has no display):** rebuild the client (frontend-only, no sidecar). Post a YouTube link → card shows thumbnail + "Watch here" + "Open externally" → first click shows the warning + "always allow" → Watch → video plays inline → ✕ stops it → a second YouTube card now plays on one click. Toggle "Allow YouTube embeds" off in Settings → Privacy & Data → next card asks again. Post a Spotify track → 30s preview plays inline with the note. Post a non-YouTube/Spotify link → only "Open externally" shows (no "Watch here"). Default render of all cards makes no external connection until clicked.
```

