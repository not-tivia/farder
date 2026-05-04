# Client Themes — Design

**Status:** Approved 2026-05-03
**Scope:** Farder desktop client (Tauri + React)

## Goal

Let users switch the entire visual look of the client by selecting one of several bundled themes, or by dropping their own CSS theme into a folder on disk. Theming is a core differentiator versus Discord, where users resort to third-party modded clients (BetterDiscord, Vencord) for the same capability.

## Non-Goals (v1)

- Sharing or downloading themes from a marketplace.
- Sandboxing or sanitizing user-provided CSS (deferred — same trust model as installing any desktop extension).
- Per-server or per-identity theme selection. Theme is a per-machine preference.
- Bundled image/font assets in themes. CSS only for v1; if a theme references `url(...)`, it works the same way any CSS does.
- Refactoring the existing 2178-line `xp-theme.css` into a shared base + overrides. Themes for v1 are full-replacement stylesheets. Re-evaluate after 5+ themes exist.
- Cross-user profile CSS (the "MySpace profiles" feature) — separate concern, separate threat model, tracked separately.

## Architecture

### Theme as a folder

Each theme is a folder containing exactly two files:

```
<theme-id>/
  theme.css     — the full stylesheet (replaces, does not augment)
  theme.json    — metadata: { id, name, author, description }
```

`theme.json` schema:

```json
{
  "id": "discord-dark",
  "name": "Discord Modern Dark",
  "author": "Farder",
  "description": "Dark, dense, familiar."
}
```

The `id` field must match the folder name and is used as the persistence key.

### Two theme sources

**Built-in themes** live in the source tree at `client/src/themes/<id>/` and are compiled into the Rust binary via `include_str!` (see Implementation Notes). At runtime they are served from a static registry — no filesystem read.

**User themes** live in `~/.farder/themes/<id>/`. The folder is scanned at startup and on explicit refresh. If the folder doesn't exist, it's created empty.

A user theme with the same id as a built-in theme **overrides** the built-in. (This lets users hack on a copy without losing the bundled original — they edit a folder under their home dir and the picker prefers it.)

### Built-in themes shipped in v1

1. **`xp-luna-blue`** — Windows XP Luna Blue. The current `client/src/styles/xp-theme.css`, moved as-is to `client/src/themes/xp-luna-blue/theme.css`, plus a new `theme.json`.
2. **`discord-dark`** — Discord Modern Dark. New full stylesheet, authored from scratch using the same set of semantic class names the app already uses (`.channel-sidebar`, `.message-bubble`, etc).
3. **`hello-kitty`** — Hello Kitty Pink. Pink-and-white, soft, kawaii. New full stylesheet.

### Tauri commands

```rust
list_themes() -> Vec<ThemeMeta>
// Returns merged list of built-in + user themes (user wins on id collision).
// Each entry: { id, name, author, description, source: "builtin" | "user" }

load_theme_css(id: String) -> Result<String, String>
// Returns raw CSS string for the given id.

get_active_theme() -> Result<{ id: String, css: String }, String>
// Reads ~/.farder/settings.json for the saved id (default: "xp-luna-blue"),
// returns id + css in one call so the frontend can inject before first paint.

set_active_theme(id: String) -> Result<(), String>
// Persists the chosen id to ~/.farder/settings.json.

open_themes_folder() -> Result<(), String>
// Opens ~/.farder/themes/ in the OS file manager (creating the folder if missing).
// Uses tauri-plugin-shell.
```

### CSS injection

The current `import "./styles/xp-theme.css"` in `main.tsx` is removed. Instead:

1. Before React mounts, the entry script awaits `get_active_theme()`, inserts a `<style id="active-theme">` element into `<head>` with the returned CSS as its `textContent`, then renders. The `await` is at module top-level (or inside a tiny async bootstrapper) — the React tree only starts rendering once the style element is present, eliminating any flash of default styling.
2. Theme switching from the picker calls `load_theme_css(id)`, replaces `document.getElementById("active-theme").textContent` atomically, and persists via `set_active_theme(id)`.

Atomic `textContent` replacement = no flash, no double-application, no removed-then-added jank.

## Picker UX

### Entry point

A new gear/settings icon in the user pill area at the bottom of the channel sidebar (the strip that currently shows the user name + status dot). Clicking opens an `AppearanceSettings` modal in the same XP-window-chrome style as the existing `ChannelSettingsDialog` and `ServerSettingsDialog`.

If a top-level "Settings" dialog is later created with multiple sections, `AppearanceSettings` becomes one tab inside it. For v1 it stands alone.

### Layout

Modal contents, top to bottom:

1. **Title bar:** "Appearance"
2. **Grid of theme cards** (2 or 3 across, depending on width). Each card:
   - Theme name (prominent)
   - Author + description (small, muted)
   - **Swatch strip:** 4-5 colored blocks rendered by parsing the loaded CSS string for the theme's primary custom properties (`--*-bg`, `--*-blue`, `--*-accent`, etc — first 4-5 distinct colors found). Provides an at-a-glance preview without screenshotting.
   - Highlighted border on the currently active card.
   - Click → switches theme immediately (atomic CSS swap, persists).
3. **Footer row:**
   - "Open themes folder" button — calls `open_themes_folder()`.
   - Refresh icon — re-runs `list_themes()` and re-renders the grid (no app restart).
   - Inline warning text: *"Themes can load external resources. Only use themes from sources you trust."*

### Hot reload

Theme switching is instantaneous — the entire app re-styles as soon as a card is clicked. No restart, no reload. This falls out of the single-`<style>`-element injection model for free.

## Persistence

`~/.farder/settings.json` (a new file, created on first write):

```json
{ "theme": "discord-dark" }
```

If the file is missing, the key is missing, or the saved id resolves to no available theme (e.g. user deleted their custom theme folder), fall back to `"xp-luna-blue"` and do not error.

The same `settings.json` file can hold other future preferences (notification defaults, font size, etc) but only `theme` is used by this feature.

## Security (v1)

User themes are local files the user explicitly placed in their own home directory. Loading them is no more risky than running any desktop application — the user has filesystem access already. No sandboxing in v1.

The inline warning in the picker (*"Themes can load external resources. Only use themes from sources you trust."*) is the user-facing acknowledgment.

When a sharing mechanism appears in v2 (downloading themes from URLs, marketplace, etc), revisit:

- CSP that blocks external `url()` fetches (prevents IP grabbers via `background-image`)
- `url()` rewriting to proxy through the app or restrict to bundled assets
- CSS property allowlist (e.g. block `position: fixed` overlay phishing)

These mirror the concerns in the MySpace-style profile customization design and may be solved together.

## Out of Scope / Deferred

- Theme previews via screenshot (v1 uses swatch strip only).
- Per-element customization in-app (color picker → live edit). Today's UX is "swap whole stylesheet."
- Theme bundles with images/fonts. Themes can still reference any URL the CSS allows; we just don't bundle/distribute assets ourselves.
- Refactoring `xp-theme.css` into base + overrides. Reconsider when authoring overhead becomes painful.
- Marketplace, sharing, downloads, signing.

## Implementation Notes

- **Built-in themes via `include_str!`:** simplest path. The Rust side embeds each built-in theme's `theme.css` and `theme.json` at compile time, so there's no runtime dependency on bundled resource resolution. Adding a new built-in = add a folder, add an `include_str!` line in a registry.
- **Folder scanning:** `std::fs::read_dir(~/.farder/themes/)`, filter to subdirectories that contain both `theme.css` and `theme.json`, parse json. Skip and log entries that fail to parse — don't crash the picker.
- **Move the existing stylesheet:** `client/src/styles/xp-theme.css` → `client/src/themes/xp-luna-blue/theme.css` (verbatim) + new `theme.json`. Remove the import from `main.tsx`. Verify the app still looks identical with the new injection path before authoring the other two themes.
- **Discord-dark and Hello-Kitty content:** authored after the loader works end-to-end with XP. Each is a standalone task; the framework can ship with just XP if the others slip.

## Success Criteria

- Three themes appear in the picker on a fresh install.
- Switching themes is visually instant (no flicker, no restart).
- Closing and reopening the app restores the chosen theme without a flash of default styling.
- Dropping a valid theme folder into `~/.farder/themes/` and clicking refresh adds it to the picker.
- A user theme with the same id as a built-in overrides the built-in in the picker and when loaded.
- Selecting a missing/deleted theme does not crash; it silently falls back to XP Luna Blue.
