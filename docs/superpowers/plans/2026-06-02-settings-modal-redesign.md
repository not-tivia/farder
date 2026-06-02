# Settings Modal Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the cramped small-window-with-horizontal-tabs settings UI with a larger modal that has a left sidebar and a spacious, Discord-style content pane, applied to all four panels across all three themes — presentation only, no behavior changes.

**Architecture:** A new `SettingsModal` shell owns the modal chrome, a vertical sidebar nav, and the active-section state, and renders one of four panel components. Three tiny reusable primitives (`SettingsSection`, `RadioOption`, `KeybindRow`) plus a set of theme-variable-driven `.settings-*` CSS classes give every panel a consistent look. The existing `AppearanceSettings.tsx` (which today is both the shell and the Appearance content) is split: the shell moves to `SettingsModal`, and `AppearanceSettings` slims to just the Appearance panel.

**Tech Stack:** React + TypeScript (Vite), per-theme CSS files driven by `--xp-*` custom properties. No JS test runner — verification is `npx tsc --noEmit` plus a visual pass in the running app.

---

## Testing notes

- **No JS test runner.** Each task is verified by `cd /home/deez/farder/client && npx tsc --noEmit` (must be clean) and a visual check in the running app. This matches the project's established frontend workflow.
- **ASCII only in source strings.** Use plain ASCII in TSX string literals (e.g. `-` not an em-dash, `...` not an ellipsis char), consistent with the existing `VoiceSettings.tsx`.
- **Per `CLAUDE.md` verify-before-done:** the feature is not "done" until the final visual pass (Task 9) confirms it in all three themes in the running app.

## File structure

**Created:**
- `client/src/components/settings/SettingsModal.tsx` — the modal shell: overlay, titlebar, sidebar nav, active-section state, renders the active panel. Prop: `onClose`.
- `client/src/components/settings/SettingsSection.tsx` — `{ label?, children }` section wrapper (uppercase label + spacing).
- `client/src/components/settings/RadioOption.tsx` — `{ selected, label, description?, onSelect }` Discord-style radio + label + description.
- `client/src/components/settings/KeybindRow.tsx` — `{ label, keyLabel, capturing?, onRebind }` key-chip + Rebind button row.

**Modified:**
- `client/src/components/AppearanceSettings.tsx` — strip the shell (overlay/window/titlebar/tabbar) and the `onClose` prop; becomes the Appearance **panel** only, keeping all theme logic.
- `client/src/components/VoiceSettings.tsx` — rebuild with the three primitives.
- `client/src/components/GifSearchSettings.tsx` — wrap in `SettingsSection`s + shared classes.
- `client/src/components/TranslationSettingsTab.tsx` — wrap in `SettingsSection`s + shared classes.
- `client/src/components/ChannelSidebar.tsx` — render `<SettingsModal>` instead of `<AppearanceSettings>`; rename `showAppearance` -> `showSettings`.
- `client/src/themes/discord-dark/theme.css`, `client/src/themes/hello-kitty/theme.css`, `client/src/themes/xp-luna-blue/theme.css` — add the `.settings-*` classes; remove the obsolete `.settings-tabs` / `.settings-tab` / `.settings-tab-content` rules if unused.

---

# Phase 1 — Foundation: CSS + primitives

## Task 1: Add the `.settings-*` CSS to all three themes

**Files:**
- Modify: `client/src/themes/discord-dark/theme.css`
- Modify: `client/src/themes/hello-kitty/theme.css`
- Modify: `client/src/themes/xp-luna-blue/theme.css`

- [ ] **Step 1: Check whether the old tab classes are still referenced**

Run: `cd /home/deez/farder && grep -rn "settings-tab" client/src`
Expected: only matches inside the three `theme.css` files (no `.tsx` usage). If a `.tsx` still uses them, leave those CSS rules in place; otherwise they are dead and Step 3 removes them.

- [ ] **Step 2: Append the shared settings block to each theme file**

Append the **identical** block below to the end of each of the three theme CSS files. It is theme-agnostic: colors come from each theme's `--xp-*` variables (with fallbacks), so the same block adapts to blue/pink/dark.

```css
/* ── Settings modal (sidebar layout) ────────────────────── */
.modal-dialog.settings-modal {
  width: 760px;
  max-width: 94vw;
  height: 560px;
  max-height: 86vh;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.settings-layout { display: flex; flex: 1; min-height: 0; }
.settings-sidebar {
  width: 184px;
  flex-shrink: 0;
  background: var(--xp-window-bg, #ece9d8);
  border-right: 1px solid var(--xp-border, #888);
  padding: 12px 8px;
  overflow-y: auto;
}
.settings-nav-group-label {
  font-size: 10px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--xp-text-muted, #6b7280);
  padding: 6px 8px 4px;
}
.settings-nav-item {
  display: block;
  width: 100%;
  text-align: left;
  padding: 7px 10px;
  margin-bottom: 2px;
  border: none;
  border-radius: 4px;
  background: transparent;
  color: var(--xp-text-normal, #1a1a1a);
  font: inherit;
  cursor: pointer;
}
.settings-nav-item:hover { background: rgba(127, 127, 127, 0.14); }
.settings-nav-item.active {
  background: var(--xp-blue, #0058e6);
  color: #fff;
  font-weight: bold;
}
.settings-content {
  flex: 1;
  min-width: 0;
  overflow-y: auto;
  padding: 20px 24px;
  background: var(--xp-panel-bg, #fff);
  color: var(--xp-text-normal, #1a1a1a);
}
.settings-panel-title {
  margin: 0 0 18px;
  font-size: 19px;
  font-weight: 700;
  color: var(--xp-text-normal, #1a1a1a);
}
.settings-section { margin-bottom: 22px; }
.settings-section-label {
  font-size: 11px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--xp-text-muted, #6b7280);
  margin-bottom: 10px;
}
.settings-option {
  display: flex;
  gap: 10px;
  align-items: flex-start;
  width: 100%;
  text-align: left;
  padding: 10px 12px;
  margin-bottom: 6px;
  border: 1px solid transparent;
  border-radius: 6px;
  background: transparent;
  color: inherit;
  font: inherit;
  cursor: pointer;
}
.settings-option:hover { background: rgba(127, 127, 127, 0.10); }
.settings-option.selected {
  background: rgba(127, 127, 127, 0.16);
  border-color: var(--xp-blue, #0058e6);
}
.settings-option-radio {
  width: 16px;
  height: 16px;
  flex-shrink: 0;
  margin-top: 1px;
  border: 2px solid var(--xp-border, #80848e);
  border-radius: 50%;
  box-sizing: border-box;
}
.settings-option.selected .settings-option-radio {
  border-color: var(--xp-blue, #0058e6);
  background: radial-gradient(var(--xp-blue, #0058e6) 0 38%, transparent 42%);
}
.settings-option-label { display: block; font-weight: 600; font-size: 13px; }
.settings-option-desc {
  display: block;
  margin-top: 2px;
  font-size: 11px;
  line-height: 1.4;
  color: var(--xp-text-muted, #6b7280);
}
.settings-divider {
  height: 1px;
  margin: 18px 0;
  background: var(--xp-border, #d8d8d8);
}
.settings-keybind { display: flex; align-items: center; gap: 12px; }
.settings-keybind-label { flex: 1; font-size: 12px; }
.settings-kbd {
  display: inline-block;
  min-width: 26px;
  padding: 3px 9px;
  text-align: center;
  border: 1px solid var(--xp-border, #555);
  border-bottom-width: 2px;
  border-radius: 4px;
  background: var(--xp-window-bg, #f0f0f0);
  font-size: 12px;
  font-weight: 600;
}
.settings-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 9px 0;
}
.settings-btn {
  padding: 6px 14px;
  border: 1px solid var(--xp-border, #888);
  border-radius: 4px;
  background: var(--xp-panel-bg, #f0ece0);
  color: var(--xp-text-normal, #000);
  font: inherit;
  cursor: pointer;
}
.settings-help {
  margin: 4px 0 0;
  font-size: 11px;
  line-height: 1.4;
  color: var(--xp-text-muted, #6b7280);
}
.settings-error {
  color: #a00;
  background: #fff5f5;
  border: 1px solid #f3b8b8;
  padding: 8px;
  border-radius: 3px;
  margin-bottom: 10px;
}
```

- [ ] **Step 3: Remove the obsolete tab CSS (only if Step 1 found no `.tsx` usage)**

In each of the three theme files, delete the `.settings-tabs`, `.settings-tab`, `.settings-tab.active`, and `.settings-tab-content` rule blocks (in `xp-luna-blue/theme.css` these are under the `/* Settings Tabs */` comment near the end). Skip this step for any class still referenced by a `.tsx` file per Step 1.

- [ ] **Step 4: Verify the client still type-checks (CSS-only, sanity)**

Run: `cd /home/deez/farder/client && npx tsc --noEmit`
Expected: no output (clean).

- [ ] **Step 5: Commit**

```bash
cd /home/deez/farder && git add client/src/themes/*/theme.css && git commit -m "settings: add shared settings-modal CSS to all themes

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 2: `SettingsSection` primitive

**Files:**
- Create: `client/src/components/settings/SettingsSection.tsx`

- [ ] **Step 1: Create the component**

```tsx
import type { ReactNode } from "react";

interface Props {
  label?: string;
  children: ReactNode;
}

/** A labelled settings section: uppercase label + spaced content block. */
export default function SettingsSection({ label, children }: Props) {
  return (
    <div className="settings-section">
      {label && <div className="settings-section-label">{label}</div>}
      {children}
    </div>
  );
}
```

- [ ] **Step 2: Type-check**

Run: `cd /home/deez/farder/client && npx tsc --noEmit`
Expected: no output (clean).

- [ ] **Step 3: Commit**

```bash
cd /home/deez/farder && git add client/src/components/settings/SettingsSection.tsx && git commit -m "settings: add SettingsSection primitive

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 3: `RadioOption` primitive

**Files:**
- Create: `client/src/components/settings/RadioOption.tsx`

- [ ] **Step 1: Create the component**

```tsx
interface Props {
  selected: boolean;
  label: string;
  description?: string;
  onSelect: () => void;
}

/** Discord-style radio row: filled radio + bold label + description line. */
export default function RadioOption({ selected, label, description, onSelect }: Props) {
  return (
    <button
      type="button"
      className={`settings-option${selected ? " selected" : ""}`}
      aria-pressed={selected}
      onClick={onSelect}
    >
      <span className="settings-option-radio" />
      <span>
        <span className="settings-option-label">{label}</span>
        {description && <span className="settings-option-desc">{description}</span>}
      </span>
    </button>
  );
}
```

- [ ] **Step 2: Type-check**

Run: `cd /home/deez/farder/client && npx tsc --noEmit`
Expected: no output (clean).

- [ ] **Step 3: Commit**

```bash
cd /home/deez/farder && git add client/src/components/settings/RadioOption.tsx && git commit -m "settings: add RadioOption primitive

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 4: `KeybindRow` primitive

**Files:**
- Create: `client/src/components/settings/KeybindRow.tsx`

- [ ] **Step 1: Create the component**

```tsx
interface Props {
  label: string;
  keyLabel: string;
  capturing?: boolean;
  onRebind: () => void;
}

/** A row showing the current key as a chip with a Rebind button. */
export default function KeybindRow({ label, keyLabel, capturing, onRebind }: Props) {
  return (
    <div className="settings-keybind">
      <span className="settings-keybind-label">{label}</span>
      <kbd className="settings-kbd">{keyLabel}</kbd>
      <button type="button" className="settings-btn" onClick={onRebind}>
        {capturing ? "Press a key..." : "Rebind"}
      </button>
    </div>
  );
}
```

- [ ] **Step 2: Type-check**

Run: `cd /home/deez/farder/client && npx tsc --noEmit`
Expected: no output (clean).

- [ ] **Step 3: Commit**

```bash
cd /home/deez/farder && git add client/src/components/settings/KeybindRow.tsx && git commit -m "settings: add KeybindRow primitive

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

# Phase 2 — Shell swap (atomic)

## Task 5: Create `SettingsModal`, slim `AppearanceSettings` to a panel, rewire `ChannelSidebar`

These three changes are mutually dependent (the app only compiles with all three), so they are one commit.

**Files:**
- Create: `client/src/components/settings/SettingsModal.tsx`
- Modify: `client/src/components/AppearanceSettings.tsx`
- Modify: `client/src/components/ChannelSidebar.tsx:12,23,44,66`

- [ ] **Step 1: Create the shell `SettingsModal.tsx`**

Reuses the existing theme modal chrome (`.modal-overlay`, `.modal-dialog`, `.modal-titlebar`, `.modal-close`) plus the new `.settings-*` classes.

```tsx
import { useEffect, useState } from "react";
import AppearanceSettings from "../AppearanceSettings";
import GifSearchSettings from "../GifSearchSettings";
import { TranslationSettingsTab } from "../TranslationSettingsTab";
import VoiceSettings from "../VoiceSettings";

interface Props {
  onClose: () => void;
}

type SectionId = "appearance" | "gif" | "translation" | "voice";

const SECTIONS: { id: SectionId; label: string }[] = [
  { id: "appearance", label: "Appearance" },
  { id: "gif", label: "GIF Search" },
  { id: "translation", label: "Translation" },
  { id: "voice", label: "Voice" },
];

export default function SettingsModal({ onClose }: Props) {
  const [active, setActive] = useState<SectionId>("appearance");

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-dialog settings-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>Settings</span>
          <button className="modal-close" onClick={onClose} title="Close">
            &#10005;
          </button>
        </div>
        <div className="settings-layout">
          <nav className="settings-sidebar">
            <div className="settings-nav-group-label">Settings</div>
            {SECTIONS.map((s) => (
              <button
                key={s.id}
                className={`settings-nav-item${active === s.id ? " active" : ""}`}
                onClick={() => setActive(s.id)}
              >
                {s.label}
              </button>
            ))}
          </nav>
          <section className="settings-content">
            {active === "appearance" && <AppearanceSettings />}
            {active === "gif" && <GifSearchSettings />}
            {active === "translation" && <TranslationSettingsTab />}
            {active === "voice" && <VoiceSettings />}
          </section>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Slim `AppearanceSettings.tsx` to the Appearance panel**

Make these edits to `client/src/components/AppearanceSettings.tsx`:

1. Change the props to none:
   - Replace `interface Props {\n  onClose: () => void;\n}` with nothing (delete the interface), and change the signature `export default function AppearanceSettings({ onClose }: Props) {` to `export default function AppearanceSettings() {`.
2. Delete the `activeTab` state line:
   - Remove `const [activeTab, setActiveTab] = useState<"appearance" | "gif" | "translation" | "voice">("appearance");` (around line 82).
3. Remove the now-unused imports for sibling panels (they are rendered by `SettingsModal` now):
   - Delete `import GifSearchSettings from "./GifSearchSettings";`, `import { TranslationSettingsTab } from "./TranslationSettingsTab";`, and `import VoiceSettings from "./VoiceSettings";`. Keep `import CustomizeModal from "./CustomizeModal";`.
4. Replace the entire `return ( ... )` block. The new return drops the outer backdrop/window/titlebar/tabbar and keeps the existing Appearance content (the themes grid and footer that today live inside the `{activeTab === "appearance" && ( ... )}` branch) plus the `CustomizeModal`. New return:

```tsx
  return (
    <div className="settings-panel">
      <h2 className="settings-panel-title">Appearance</h2>
      {loading && <div>Loading themes...</div>}
      {error && <div className="settings-error">{error}</div>}
      {!loading && (
        <>
          {/* THEMES GRID: keep the existing
              <div style={{ display: "grid", gridTemplateColumns: ... }}> ... themes.map(...) ... </div>
              verbatim from the current activeTab === "appearance" block. */}

          {/* FOOTER ACTIONS: keep the existing footer
              <div style={{ marginTop: "auto", paddingTop: 10, ... }}> ... </div>
              verbatim, but change `marginTop: "auto"` to `marginTop: 16`
              (the old value relied on the removed flex body). */}
        </>
      )}
      {customizing && (
        <CustomizeModal
          themeId={customizing.themeId}
          initialName={customizing.name}
          onClose={() => {
            setCustomizing(null);
            refresh();
          }}
          onSaved={() => {
            refresh();
          }}
        />
      )}
    </div>
  );
```

   Move the existing themes-grid `<div style={{ display: "grid", ... }}>...</div>` and footer `<div style={{ marginTop: ..., ... }}>...</div>` markup (currently inside the `{activeTab === "appearance" && (<>...</>)}` branch, lines ~315-520) into the two marked spots verbatim, applying only the one `marginTop` change noted. Keep every logic function (`refresh`, `selectTheme`, `reorderThemes`, `deleteTheme`, `commitRename`, `startCustomizing`, `applyOrder`, `extractSwatch`) and all `useState`/`useEffect` hooks exactly as they are. The `chromeButton`/`closeButton` `CSSProperties` consts: `chromeButton` is still used by the footer buttons (keep it); `closeButton` is no longer used (delete it).

- [ ] **Step 3: Rewire `ChannelSidebar.tsx`**

Make these four edits to `client/src/components/ChannelSidebar.tsx`:
- Line 12: replace `import AppearanceSettings from "./AppearanceSettings";` with `import SettingsModal from "./settings/SettingsModal";`
- Line 23: replace `const [showAppearance, setShowAppearance] = useState(false);` with `const [showSettings, setShowSettings] = useState(false);`
- Line 44: replace `onClick={() => setShowAppearance(true)}` with `onClick={() => setShowSettings(true)}`
- Line 66: replace `{showAppearance && <AppearanceSettings onClose={() => setShowAppearance(false)} />}` with `{showSettings && <SettingsModal onClose={() => setShowSettings(false)} />}`

- [ ] **Step 4: Type-check**

Run: `cd /home/deez/farder/client && npx tsc --noEmit`
Expected: no output (clean). If it reports `closeButton` declared but never used, confirm you deleted the `closeButton` const in Step 2; if it reports an unused `CSSProperties` import, leave it only if `chromeButton` still uses it (it does).

- [ ] **Step 5: Commit**

```bash
cd /home/deez/farder && git add client/src/components/settings/SettingsModal.tsx client/src/components/AppearanceSettings.tsx client/src/components/ChannelSidebar.tsx && git commit -m "settings: new SettingsModal sidebar shell; AppearanceSettings becomes a panel

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

# Phase 3 — Restyle the remaining panels

## Task 6: `VoiceSettings` on the primitives

**Files:**
- Modify: `client/src/components/VoiceSettings.tsx`

- [ ] **Step 1: Replace the file with the primitive-based version**

```tsx
import { useEffect, useState } from "react";
import { getVoiceMode, setVoiceMode, getPttKey, setPttKey } from "../lib/tauri-bridge";
import SettingsSection from "./settings/SettingsSection";
import RadioOption from "./settings/RadioOption";
import KeybindRow from "./settings/KeybindRow";

export default function VoiceSettings() {
  const [mode, setMode] = useState<string>("OpenMic");
  const [pttKey, setPttKeyState] = useState<string>("Backquote");
  const [capturing, setCapturing] = useState(false);

  useEffect(() => {
    void getVoiceMode().then(setMode).catch(() => {});
    void getPttKey().then(setPttKeyState).catch(() => {});
  }, []);

  const chooseMode = (next: string) => {
    setMode(next);
    void setVoiceMode(next).catch(() => {});
  };

  useEffect(() => {
    if (!capturing) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      setPttKeyState(e.code);
      void setPttKey(e.code).catch(() => {});
      setCapturing(false);
    };
    window.addEventListener("keydown", onKey, { once: true });
    return () => window.removeEventListener("keydown", onKey);
  }, [capturing]);

  return (
    <div className="settings-panel">
      <h2 className="settings-panel-title">Voice</h2>
      <SettingsSection label="Microphone Mode">
        <RadioOption
          selected={mode === "OpenMic"}
          label="Open Mic"
          description="Your mic is always live - transmit whenever you speak."
          onSelect={() => chooseMode("OpenMic")}
        />
        <RadioOption
          selected={mode === "PushToTalk"}
          label="Push to Talk"
          description="Stay muted until you press your key."
          onSelect={() => chooseMode("PushToTalk")}
        />
      </SettingsSection>
      {mode === "PushToTalk" && (
        <>
          <div className="settings-divider" />
          <SettingsSection label="Push-to-Talk Keybind">
            <KeybindRow
              label="Current key"
              keyLabel={pttKey}
              capturing={capturing}
              onRebind={() => setCapturing(true)}
            />
          </SettingsSection>
        </>
      )}
    </div>
  );
}
```

- [ ] **Step 2: Type-check**

Run: `cd /home/deez/farder/client && npx tsc --noEmit`
Expected: no output (clean).

- [ ] **Step 3: Commit**

```bash
cd /home/deez/farder && git add client/src/components/VoiceSettings.tsx && git commit -m "settings: restyle Voice panel with shared primitives

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 7: `GifSearchSettings` on sections + shared classes

**Files:**
- Modify: `client/src/components/GifSearchSettings.tsx`

- [ ] **Step 1: Update imports and the returned markup**

Keep all hooks and the `update()` logic unchanged. Add `import SettingsSection from "./settings/SettingsSection";` at the top. Replace the loading branch and the `return ( ... )` block (the `row` `CSSProperties` const may be deleted — the `.settings-row` class replaces it):

```tsx
  if (!settings) {
    return (
      <div className="settings-panel">
        <h2 className="settings-panel-title">GIF Search</h2>
        <div>Loading...</div>
      </div>
    );
  }

  return (
    <div className="settings-panel">
      <h2 className="settings-panel-title">GIF Search</h2>
      {error && <div className="settings-error">{error}</div>}

      <SettingsSection>
        <div className="settings-row">
          <label htmlFor="gif-enabled">Enable Tenor GIF search</label>
          <input
            id="gif-enabled"
            type="checkbox"
            checked={settings.enabled}
            onChange={(e) => update({ enabled: e.target.checked })}
          />
        </div>
      </SettingsSection>

      {settings.enabled && (
        <>
          <p className="settings-help">
            Tenor is owned by Google. Searches are sent to Google's servers; your IP and search terms are visible to them.
          </p>

          <SettingsSection label="Content Filter">
            <div className="settings-row">
              <label htmlFor="gif-filter">Content filter</label>
              <select
                id="gif-filter"
                value={settings.content_filter}
                onChange={(e) => update({ content_filter: e.target.value as Settings["content_filter"] })}
                style={{ font: "inherit" }}
              >
                <option value="high">High (default)</option>
                <option value="medium">Medium</option>
                <option value="low">Low</option>
                <option value="off">Off</option>
              </select>
            </div>
          </SettingsSection>

          <SettingsSection label="Tenor API Key">
            <label htmlFor="gif-key" style={{ display: "block", marginBottom: 4 }}>
              Your Tenor API key (optional)
            </label>
            <input
              id="gif-key"
              type="text"
              placeholder="leave blank to use Farder's default"
              value={settings.user_api_key ?? ""}
              onChange={(e) => update({ user_api_key: e.target.value || null })}
              style={{ width: "100%", font: "inherit", boxSizing: "border-box" }}
            />
            <p className="settings-help">
              Setting your own key avoids sharing the default Farder quota.{" "}
              <a
                href={TENOR_DOCS_URL}
                onClick={(e) => {
                  e.preventDefault();
                  window.open(TENOR_DOCS_URL, "_blank");
                }}
                style={{ color: "var(--xp-blue, #0058E6)" }}
              >
                How to get a Tenor API key
              </a>
            </p>
          </SettingsSection>
        </>
      )}
    </div>
  );
```

If you removed all uses of the `row` const, delete its declaration and the now-unused `type CSSProperties` import.

- [ ] **Step 2: Type-check**

Run: `cd /home/deez/farder/client && npx tsc --noEmit`
Expected: no output (clean).

- [ ] **Step 3: Commit**

```bash
cd /home/deez/farder && git add client/src/components/GifSearchSettings.tsx && git commit -m "settings: restyle GIF Search panel with sections

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 8: `TranslationSettingsTab` on sections + shared classes

**Files:**
- Modify: `client/src/components/TranslationSettingsTab.tsx`

- [ ] **Step 1: Update imports and the returned markup**

Keep all hooks, `refresh()`, and handlers unchanged. Add `import SettingsSection from "./settings/SettingsSection";`. Replace the loading branch and the `return ( ... )` block:

```tsx
  if (!settings)
    return (
      <div className="settings-panel">
        <h2 className="settings-panel-title">Translation</h2>
        <div>Loading...</div>
      </div>
    );

  return (
    <div className="settings-panel">
      <h2 className="settings-panel-title">Translation</h2>

      <SettingsSection>
        <label style={{ display: "block", margin: "6px 0" }}>
          <input
            type="checkbox"
            checked={settings.enabled}
            onChange={async (e) => {
              const next = { ...settings, enabled: e.target.checked };
              await setTranslationSettings(next);
              setSettings(next);
            }}
          />
          {" "}Enable translation
        </label>
        <label style={{ display: "block", margin: "10px 0" }}>
          Default target language:{" "}
          <select
            value={settings.default_target}
            onChange={async (e) => {
              const next = { ...settings, default_target: e.target.value };
              await setTranslationSettings(next);
              setSettings(next);
            }}
          >
            {Object.keys(ISO_1_TO_3).map((iso) => (
              <option key={iso} value={iso}>{displayName(iso)}</option>
            ))}
          </select>
        </label>
      </SettingsSection>

      <SettingsSection label="Installed Languages">
        {installed.length === 0 && <p className="settings-help">No models installed yet.</p>}
        <ul style={{ margin: 0, paddingLeft: 18 }}>
          {installed.map((m) => (
            <li key={`${m.pair.src}-${m.pair.trg}`} style={{ margin: "6px 0" }}>
              {displayName(m.pair.src)} -&gt; {displayName(m.pair.trg)}
              {" "}({(m.disk_size_bytes / 1_000_000).toFixed(1)} MB)
              {" "}
              <button
                className="settings-btn"
                disabled={busy !== null}
                onClick={async () => {
                  if (!confirm(`Delete ${displayName(m.pair.src)}->${displayName(m.pair.trg)} model?`)) return;
                  setBusy("deleting");
                  try { await deleteModel(m.pair); await refresh(); }
                  finally { setBusy(null); }
                }}
              >Delete</button>
            </li>
          ))}
        </ul>

        <button className="settings-btn" style={{ marginTop: 8 }} onClick={() => setShowAdd(!showAdd)}>
          {showAdd ? "Hide available languages" : "+ Add language"}
        </button>

        {showAdd && (
          <ul style={{ maxHeight: 240, overflowY: "auto", border: "1px solid var(--xp-border, #ccc)", padding: 8, marginTop: 8, listStyle: "none" }}>
            {available
              .filter((p) => !installed.some((m) => m.pair.src === p.src && m.pair.trg === p.trg))
              .map((p) => (
                <li key={`${p.src}-${p.trg}`} style={{ margin: "4px 0" }}>
                  {displayName(p.src)} -&gt; {displayName(p.trg)}
                  {" "}({(p.size_bytes / 1_000_000).toFixed(1)} MB)
                  {" "}
                  <button
                    className="settings-btn"
                    disabled={busy !== null}
                    onClick={async () => {
                      setBusy(`downloading-${p.src}-${p.trg}`);
                      try { await downloadModel({ src: p.src, trg: p.trg }); await refresh(); }
                      catch (e) { alert(`Download failed: ${e}`); }
                      finally { setBusy(null); }
                    }}
                  >
                    {busy === `downloading-${p.src}-${p.trg}` ? "Downloading..." : "Download"}
                  </button>
                </li>
              ))}
          </ul>
        )}
      </SettingsSection>
    </div>
  );
```

- [ ] **Step 2: Type-check**

Run: `cd /home/deez/farder/client && npx tsc --noEmit`
Expected: no output (clean).

- [ ] **Step 3: Commit**

```bash
cd /home/deez/farder && git add client/src/components/TranslationSettingsTab.tsx && git commit -m "settings: restyle Translation panel with sections

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

# Phase 4 — Verify

## Task 9: Visual verification across all three themes (required acceptance)

**Files:** none (verification only).

- [ ] **Step 1: Final type-check**

Run: `cd /home/deez/farder/client && npx tsc --noEmit`
Expected: no output (clean).

- [ ] **Step 2: Run the app**

On the machine with a display + the Windows build: `cd client && npm run tauri dev` (set `$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"` first if Opus needs rebuilding). Open the Settings window.

- [ ] **Step 3: Verify in each of the three themes**

For xp-luna-blue, hello-kitty, and discord-dark (switch via the Appearance panel), confirm:
- The Settings window is the larger modal with a left sidebar and an active-item highlight.
- Clicking sidebar items swaps panels (Appearance / GIF Search / Translation / Voice).
- Each panel shows a page title, section labels, and proper spacing.
- Voice: the two mic-mode options show a bold label + description; selecting Push-to-Talk reveals a clean keybind row (key chip + Rebind, no overlap); Rebind captures a key.
- Text is readable in all three themes (watch xp-luna-blue, which lacks `--xp-text-normal` and relies on the CSS fallback).
- Existing behavior still works: theme switch/reorder/rename/customize/delete, GIF enable + filter + key, translation enable + target + model add/delete, voice mode + key persist across reopen.

- [ ] **Step 4: Record the result**

Note in the final message what was checked and what was observed (per `CLAUDE.md` verify-before-done). If anything looks wrong, fix it and re-verify before declaring done.

---

## Self-review

**Spec coverage:**
- Shell = bigger modal + sidebar -> Task 1 (CSS) + Task 5 (`SettingsModal`).
- Discord-style controls (title, section labels, radio+description, keybind chip) -> Tasks 2-4 (primitives) + Task 6 (Voice) + Tasks 7-8 (other panels).
- All four panels -> Tasks 5 (Appearance), 6 (Voice), 7 (GIF), 8 (Translation).
- All three themes -> Task 1 (CSS added to all three) + Task 9 (verified in all three).
- Split the 537-line `AppearanceSettings` -> Task 5.
- Presentation only / no behavior change -> Tasks preserve every handler; Task 9 verifies behavior.
- Out of scope (Notifications, server/channel dialogs) -> untouched by all tasks.
- Verification = tsc + visual -> tsc gate every task; Task 9 visual.

**Placeholder scan:** No TBD/TODO; the only "move verbatim" reference is the large Appearance themes-grid/footer markup, with exact source locations and the single `marginTop` change called out (re-pasting ~200 lines of unchanged JSX would add transcription risk).

**Type consistency:** `SectionId` union matches the four `SECTIONS` ids and the four `active === ...` branches. Primitive prop names (`selected`/`label`/`description`/`onSelect`; `label`/`keyLabel`/`capturing`/`onRebind`; `label`/`children`) match their call sites in Tasks 5-8. `SettingsModal` imports `TranslationSettingsTab` as a named export (matching its `export function`) and the others as default exports (matching theirs).
