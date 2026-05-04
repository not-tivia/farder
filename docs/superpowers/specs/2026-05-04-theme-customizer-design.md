# Theme Customizer — Design

**Status:** Approved 2026-05-04
**Scope:** Farder desktop client (Tauri + React). Extension of the themes feature shipped 2026-05-04.
**Predecessor spec:** `docs/superpowers/specs/2026-05-03-client-themes-design.md`

## Goal

Let users build their own visual theme directly from the app — pick colors and drop images on regions of the interface and have it persisted as a real theme they can keep, swap to, and (eventually) share. Compete with Discord by making the customization that BetterDiscord/Vencord users want available natively, with an interaction model so simple a non-technical user can do it.

## Non-Goals (v1)

- Sharing or marketplace flow. Users can hand-share their `~/.farder/themes/<id>/` folder; built-in export/upload is a separate feature.
- Per-region hover/active state customization beyond the few hover targets baked into the 12 regions.
- Animations / transitions.
- Reverse-engineering existing hand-edited `theme.css` files into the region model. Customizer edits stack as overrides on top of whatever's in the fork.
- Cross-region "apply to all" actions. Each region is independent in v1.
- Editing a built-in theme in place (built-ins are compiled in; the customizer always forks first).

## Two phases

The full feature ships in two phases. Phase 1 builds the foundation; Phase 2 adds the live drag-drop layer on top, reusing Phase 1's data model and theme writer.

### Phase 1 — "Customize" modal

Click-to-edit form, opened from a "Customize" button on each theme card in the existing Appearance picker.

### Phase 2 — Live edit mode

Floating palette + drag-drop onto the live UI, opened from an "Edit Live" button inside the Phase 1 modal. Same data model, same persistence, same undo/redo.

## The 12 regions

A curated, named bundle of CSS selectors. Each region is one row in the customizer with at most three knobs: background (color OR image+fit), text color, and an implicit "clear to base" per knob.

| # | Region | Selectors targeted |
|---|---|---|
| 1 | Main background | `body`, `#root`, `.app-shell`, `.chat-panel` background |
| 2 | Channel sidebar | `.channel-sidebar` background; channel item text via region's text color |
| 3 | Server strip | `.server-strip` background |
| 4 | Member sidebar | `.member-sidebar` background; member name text |
| 5 | Title bars | `.title-bar`, modal headers, `.connect-dialog-titlebar` background + text |
| 6 | Message bubble | `.message`, `.message-content` background + body text |
| 7 | Message hover | `.message:hover` background only |
| 8 | Buttons | `.xp-button` background + text |
| 9 | Input field | `.message-input`, `input[type="text"]`, `textarea` background + text |
| 10 | Modal background | `.connect-screen`, `.modal`, dialog body backgrounds |
| 11 | Scrollbars | `::-webkit-scrollbar-thumb` color |
| 12 | Accent (active/selected) | `--xp-blue` value (drives active channel border, send button highlight, mention color, etc) |

The region map lives in TypeScript as a constant `REGIONS: { id, label, selectors: { background?: string[], text?: string[], accent?: string[] } }[]`. Used by both the Phase 1 form and the Phase 2 hit-detection.

## Data model

```ts
interface CustomizerSession {
  themeId: string;          // user theme being edited
  baseThemeId: string;      // what was forked from
  regions: Map<RegionId, RegionState>;
  history: Map<RegionId, RegionState>[];  // snapshots for undo
  historyIndex: number;
  dirty: boolean;
}

interface RegionState {
  bgColor?: string;          // any CSS color string
  bgImage?: {
    path: string;            // relative path within theme folder, e.g. "./assets/dog.jpg"
    fit: "stretch" | "tile" | "center" | "cover";
  };
  textColor?: string;
}
```

`bgColor` and `bgImage` are mutually exclusive at the UI level (setting one clears the other) but stored independently so undo/redo can switch between them.

## Live preview mechanism

The bootstrap (`main.tsx`) already injects `<style id="active-theme">` with the current theme's CSS. The customizer adds a sibling element `<style id="active-theme-overrides">` immediately after it. Every change regenerates the override CSS string and replaces that element's `textContent` atomically. On save, the overrides are merged into the user theme's `theme.css` on disk, the override element is removed, and `active-theme`'s content is updated to the new merged CSS.

Override CSS structure:
```css
/* === Customizer overrides — generated, edit with the customizer === */
:root {
  --customizer-channel-sidebar-bg: <user value>;
  --customizer-channel-sidebar-text: <user value>;
  /* ... */
}
.channel-sidebar { background: var(--customizer-channel-sidebar-bg); color: var(--customizer-channel-sidebar-text); }
/* ... per-region rules ... */
```

Why route through CSS variables instead of inlining values into the rules: undo/redo only swaps `:root` values rather than regenerating selector blocks per change. Faster, simpler.

## Phase 1 UX

**Entry point.** "Customize" button on each theme card in the Appearance picker. Clicking it opens a small naming dialog (pre-filled `<base name> (Custom)`), then forks: copies the base theme's CSS to a fresh `~/.farder/themes/<new-id>/theme.css`, writes a `theme.json` with `baseThemeId` set, then opens the customizer modal scoped to that new theme.

**Modal layout.**

Header row: theme name (renameable) · Undo · Redo · Save · Close (close prompts on dirty).

Body: 12 region rows, each:

```
[ Region name ]   [bg color swatch ×]   [Pick image… ▾fit]   text: [color swatch ×]
```

- **Color swatch (small filled square):** click → popover with theme-extracted swatch strip + "more colors" button (HTML5 color picker). Live preview on change.
- **Pick image…:** opens file dialog. Image is copied to `~/.farder/themes/<theme-id>/assets/<sanitized-filename>` and applied via `url('./assets/<sanitized-filename>')`. Adjacent fit dropdown (stretch / tile / center / cover) updates `background-size` and `background-repeat`.
- **× clear:** reverts that one knob to the base theme's value (removes the override).

**Live preview.** Every edit visible immediately. Modal is draggable so the user can move it off screen regions they want to inspect.

**Save.** Writes the merged base+overrides CSS to the user theme's `theme.css`, clears the override `<style>` element, marks session clean.

**Close.** If dirty, prompt: "Discard unsaved changes?". The forked theme folder remains either way (created at fork time) — closing without saving leaves it as the original copy.

**Image limits.** Warn if a single image exceeds 5MB ("This image is large and may slow the app down"). Hard reject above 25MB ("Image is too large to use as a background").

## Phase 2 UX (Live edit mode)

**Entry.** "Edit Live" button in the Phase 1 modal header. Modal collapses to a small floating toolbar.

**Floating toolbar contents.**
- Theme-extracted color swatches strip
- Recently-used colors row (fills as you go, persisted within the session)
- "+" → OS color picker for arbitrary new colors
- "Image" button → drawer of thumbnails for images already in the theme's `assets/`. Each is draggable.
- Undo / Redo / Save / Exit (returns to Phase 1 modal).

**Drag-drop interaction.**
- Hover a swatch → cursor becomes paint-bucket. As you drag over the live app, the region under the cursor highlights (2px outline in accent color + faint tinted overlay) and a small label floats near the cursor with the region name ("Channel sidebar").
- Drop on a region → applies as background. Released without dropping on a region → no-op.
- **Hold Shift while dropping** → applies to text color instead of background.
- Image thumbnails: same drop interaction. After drop, a tiny popover at the drop site lets the user switch fit without going back to the toolbar.
- **OS-level drag from file manager**: dropping an image file onto a region in the live app auto-copies the image into the theme's `assets/` and applies it.

**Highlights.** Only one region highlights at a time — the one under the cursor. No clutter from showing all 12.

**Exit.** Esc or Exit button → returns to Phase 1 modal with all changes still in the undo stack. Save from there (or from the toolbar's Save).

**Keyboard shortcuts (both phases):** Ctrl+Z undo · Ctrl+Y redo · Ctrl+S save.

## Onboarding & instructions

Drag-drop is non-obvious. We bake in three layers of guidance:

1. **One-time intro overlays.** First click of "Customize" → walkthrough overlay: "Pick a region, change its color or drop an image, hit Save when you're done." First click of "Edit Live" → "Drag a color onto any part of the app to paint it. Hold Shift to color the text. Drag an image for a background. Esc to exit." Both dismissable, both re-openable from a "?" icon in the respective header. Dismissal state stored in `~/.farder/settings.json` (e.g. `customizerIntroDismissed: true`).
2. **Tooltips on every control** in both toolbars — including the Shift hint ("Hold Shift while dropping a color to change text color instead") and keyboard shortcuts ("Undo (Ctrl+Z)").
3. **Empty-state hint** in the Phase 1 modal when no regions have been edited yet: "Tip: click any color or image to start. Use Undo if you change your mind."

## Persistence

**On fork** (when "Customize" is clicked on a theme):
- Create `~/.farder/themes/<new-id>/`
- Copy base CSS to `theme.css`
- Write `theme.json` with the new id, name, author "you", description, and `baseThemeId`
- The customizer immediately operates on this new theme; it appears in the picker after refresh.

**On save** (Phase 1 or Phase 2):
- Generate the merged CSS: base CSS (unchanged) + an overrides block at the bottom delimited by a clearly marked comment.
- Write to `~/.farder/themes/<id>/theme.css`.
- Write any new image assets to `~/.farder/themes/<id>/assets/`.
- Clear the in-memory override `<style>` element; reload the active theme's `<style id="active-theme">` content from the new merged CSS.
- Mark session clean (Save button disables until next change).

**Image asset naming.** When an image is added, the original filename is sanitized (`[^a-zA-Z0-9._-]` → `_`) and prefixed with a short timestamp/hash so two `dog.jpg` files don't collide. Stored as `assets/<timestamp>-<sanitized>`.

**Hand-edits coexist.** If a user later opens the theme's CSS in a text editor and adds rules outside the overrides block, the customizer's next save preserves them — the overrides block is the only thing the customizer rewrites.

## Undo / Redo

**Scope:** per-session. The history is the array of `Map<RegionId, RegionState>` snapshots. Each user action that mutates a region pushes a new snapshot at `historyIndex+1` (truncating any redo branch). Undo decrements the index and reapplies; redo increments. Unbounded for the session (12 regions × few knobs each = small memory footprint even with hundreds of edits).

**Crash safety.** On customizer open, write a single `theme.css.bak` snapshot of the file's pre-customizer state. If the app crashes mid-customize, the user can manually rename the .bak back. Removed on successful save or explicit discard.

**Cleared on:** session close (modal exit), explicit discard, or Save (history collapses to a single "saved" baseline that future undos don't cross).

## Implementation notes (non-binding, for the planner)

- **New Tauri commands** likely needed: `fork_theme(base_id, new_id, name) -> Result<>`, `save_user_theme(id, css, asset_files) -> Result<>`, `add_theme_asset(theme_id, file_path) -> Result<relative_path>`, `delete_user_theme(id) -> Result<>`. The existing `list_themes` / `load_theme_css` / `get_active_theme` / `set_active_theme` don't need changes — the user theme created by the customizer is just another folder in `~/.farder/themes/`.
- **The customizer modal is one large component** but the region rows can be a small re-used `<RegionRow>` component to keep the file readable.
- **The Phase 2 hit-detection** uses `document.elementsFromPoint(x, y)` while dragging, mapped backward through the region map's selectors to identify which region the cursor is over. Throttled to ~60fps via `requestAnimationFrame`.
- **Drag-drop in browsers is finicky for non-list elements.** Plan to spend extra time on Phase 2's interaction polish — visual feedback during drag, robust drop-target detection, and graceful no-op on misses.

## Success criteria

- A non-technical user can fork Discord Dark, give it a name, change the channel sidebar to a custom photo, save, and see it appear in the picker as a usable theme — without reading any docs.
- The base built-in themes are never modified (verified: edit then revert via picker → built-in still pristine).
- Undo/redo correctly walks back through ≥10 edits without state corruption.
- Crash mid-customize → re-opening the app shows the theme as it was before customize started (via .bak restore on detected dirty state — UX TBD if .bak found, but "your previous edit was interrupted" prompt is one option).
- Phase 2 drag-drop works reliably on common region drops (sidebar, chat area, member list); dropping on overlapping regions resolves to the topmost.
- Onboarding overlays appear once, dismiss, do not re-appear.
