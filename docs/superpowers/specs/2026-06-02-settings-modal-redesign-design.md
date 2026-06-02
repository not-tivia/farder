# Settings Modal Redesign — Design Spec

**Date:** 2026-06-02
**Status:** Approved (brainstorming), ready for implementation plan

## Goal

Make the Farder user-settings window look professional and organized — closer
to Discord's settings — instead of the current cramped, partly-unstyled small
window. This is a **presentation-only** redesign: no settings behavior changes.

## Current state

The settings window is `client/src/components/AppearanceSettings.tsx` (~537
lines). Despite its name it is the whole settings modal: it owns the modal
shell, an inline-styled **horizontal tab bar** (Appearance / GIF Search /
Translation / Voice), and the inline Appearance-tab content, and it renders the
other three panels as child components:

- `client/src/components/GifSearchSettings.tsx` (~105 lines)
- `client/src/components/TranslationSettingsTab.tsx` (~118 lines)
- `client/src/components/VoiceSettings.tsx` (~62 lines)

It is rendered from `client/src/components/ChannelSidebar.tsx` (around line 66):
`{showAppearance && <AppearanceSettings onClose={() => setShowAppearance(false)} />}`.

Problems:
- **Shell:** a small floating window with horizontal tabs; little room, weak
  hierarchy.
- **Panels:** mixed inline styles; some are unstyled and cramped. The Voice
  panel is the worst — its two radios are jammed onto one line and the "Rebind"
  button overlaps the key text.
- **Naming/size:** `AppearanceSettings` is really the settings shell *and* the
  Appearance content in one 537-line file.

Theme CSS lives per-theme in `client/src/themes/{discord-dark,hello-kitty,
xp-luna-blue}/theme.css` and already defines `.modal-*`, `.settings-tabs`, and
`.settings-tab`. Shared CSS variables exist across all three themes:
`--xp-panel-bg`, `--xp-blue`, `--xp-border`, `--xp-window-bg` are defined in all
three; `--xp-text-normal` is defined in discord-dark and hello-kitty but **not**
xp-luna-blue (so new CSS must supply a fallback).

## Approved design decisions (from brainstorming)

1. **Shell layout = "B": bigger centered modal + vertical sidebar.** Not
   full-window. Keeps Farder's "window" feel but enlarged, with a left sidebar
   nav and a roomy content pane on the right.
2. **Control style = Discord-like.** Per-panel page title, uppercase section
   labels, radio options that pair a **bold label with a description line**,
   clean dividers, a proper keybind chip + spaced button, generous spacing.
3. **Scope = all four panels** (Appearance, GIF Search, Translation, Voice).
4. **Themes = all three.** Structure/spacing shared; colors from each theme's
   variables. xp-luna-blue stays blue, hello-kitty pink, discord-dark dark.

## Architecture

### Shell: `SettingsModal`

New `client/src/components/settings/SettingsModal.tsx` becomes the shell. It:

- Renders the modal overlay + dialog (reusing existing `.modal-overlay` /
  `.modal-dialog` / `.modal-titlebar` / `.modal-close` conventions), sized
  larger than today.
- Holds the active-section state (replacing `AppearanceSettings`'s `activeTab`).
- Renders a left **vertical sidebar** (`<nav>`): a "Settings" group label and the
  four nav items, with an `active` highlight on the current one.
- Renders the active panel in the right content pane.
- Preserves existing dismissal (Escape key, overlay/close-button click) and the
  `onClose` prop.

`AppearanceSettings.tsx` is slimmed to **only the Appearance panel content**
(the theme list / customization UI extracted from the current inline body); it
no longer owns the shell or tabs.

`ChannelSidebar.tsx` renders `<SettingsModal .../>` instead of
`<AppearanceSettings .../>`. The trigger state `showAppearance` is renamed to
`showSettings` for clarity (mechanical rename).

### Reusable panel primitives

New `client/src/components/settings/` directory holds small, single-purpose,
theme-aware building blocks so all four panels are visually consistent:

- `SettingsSection.tsx` — `{ label?: string; children: ReactNode }`. Renders an
  uppercase section label and spaced content block.
- `RadioOption.tsx` — `{ selected: boolean; label: string; description?: string;
  onSelect: () => void }`. The Discord-style radio + bold label + description,
  with a selected state (accent border/background).
- `KeybindRow.tsx` — `{ label: string; keyLabel: string; capturing?: boolean;
  onRebind: () => void }`. A row with a label, the current key shown as a chip,
  and a spaced Rebind button; shows a "Press a key…" state while capturing.

These have no settings logic — panels pass data/handlers in. Sliders, toggles,
and dropdowns reuse shared CSS classes (below) rather than new components unless
a panel needs one; if a toggle is needed in more than one panel, add a
`Toggle.tsx` primitive of the same shape.

### Panels (rebuilt on the shell + primitives)

- **Voice** (`VoiceSettings.tsx`): `SettingsSection "Microphone Mode"` with two
  `RadioOption`s (Open Mic / Push to Talk, each with a description), then a
  `SettingsSection "Push-to-Talk Keybind"` with a `KeybindRow` (shown only in
  PTT mode, matching current behavior). Same `getVoiceMode`/`setVoiceMode`/
  `getPttKey`/`setPttKey` wiring as today.
- **Appearance** (`AppearanceSettings.tsx`): the extracted theme/customization
  content, reorganized into `SettingsSection`s with the shared row classes.
- **GIF Search** (`GifSearchSettings.tsx`) and **Translation**
  (`TranslationSettingsTab.tsx`): wrap their existing controls in
  `SettingsSection`s and the shared row/control classes. No logic change.

### Theming

New settings CSS is defined **once** (shared structural rules) using theme
variables with fallbacks, so it adapts to every theme:

- Colors: `var(--xp-panel-bg, #fff)`, `var(--xp-blue, #0058E6)`,
  `var(--xp-border, #888)`, `var(--xp-window-bg, #ECE9D8)`, and
  `var(--xp-text-normal, #1a1a1a)` (the fallback matters for xp-luna-blue).
- Layout/spacing: fixed values (theme-independent) so structure is consistent.

Following the existing per-theme convention, the new classes are added to each
theme file (they can fine-tune where a theme needs a distinct look, e.g. XP's
beveled buttons), but the shared variable-driven definitions mean each theme
mostly inherits the same clean layout. The obsolete `.settings-tabs` /
`.settings-tab` rules are removed.

New / updated class names (consistent across themes):

- `.settings-modal` — enlarged dialog
- `.settings-layout` — flex row (sidebar + content)
- `.settings-sidebar`, `.settings-nav-group-label`, `.settings-nav-item`,
  `.settings-nav-item.active`
- `.settings-content` — scrollable padded right pane
- `.settings-panel-title` — per-panel page title
- `.settings-section`, `.settings-section-label`
- `.settings-option`, `.settings-option.selected`, `.settings-option-radio`,
  `.settings-option-label`, `.settings-option-desc`
- `.settings-divider`
- `.settings-keybind`, `.settings-kbd`
- `.settings-row` (generic label + control), plus toggle/slider helpers as needed

## Data flow

Unchanged. Each panel still reads/writes its settings through the same
`tauri-bridge` calls it uses today (theme commands, GIF settings, translation
settings, `get/set_voice_mode`, `get/set_ptt_key`). The shell only decides which
panel is visible.

## Out of scope

- The separate dialogs — Notifications (`NotificationSettings`), Server settings
  (`ServerSettingsDialog`), Channel settings (`ChannelSettingsDialog`),
  `CustomizeModal` — are **not** folded into this sidebar now.
- No new settings, no behavior changes, no changes to what any control does.

## Testing / verification

The frontend has no JS test runner, so:

- **Type-check:** `cd client && npx tsc --noEmit` must be clean.
- **Visual verification (required by `CLAUDE.md` verify-before-done):** run the
  app (`npm run tauri dev`) and confirm, in **each of the three themes**:
  - The settings window is the larger modal with a working left sidebar and an
    active-item highlight.
  - Switching sidebar items swaps panels correctly.
  - Each panel renders with section labels, spacing, and the new control style;
    the Voice radios show label + description and the keybind row is clean (no
    overlap).
  - Theme colors are correct (blue / pink / dark) and text is readable in all
    three (watch the xp-luna-blue `--xp-text-normal` fallback).
  - All controls still function (theme switch, GIF key, translation settings,
    voice mode + key rebind).
