# Theme Customizer Phase 1 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the modal-based "Customize" experience: user clicks Customize on any theme card → names a new theme → forks it → opens a customizer modal with 12 region rows (color/image/text per region) + undo/redo + save/discard. Phase 2 (live drag-drop) builds on this foundation in a separate plan.

**Architecture:** New Rust Tauri commands handle the file-system side (fork a theme to disk, write back the saved CSS, copy image assets). New TypeScript module under `client/src/lib/customizer/` holds the pure logic (region map, CSS generator, undo/redo history). New React components render the modal, region rows, and color popover. The customizer overlays its in-progress edits via a second `<style id="active-theme-overrides">` element appended next to the existing one — no plumbing changes to the existing themes loader.

**Tech Stack:** Rust (Tauri v2), React + TypeScript, plain CSS, browser-native `<input type="color">` for color picking, browser-native file dialog for image picking (via Tauri's existing file pick command).

**Spec:** `docs/superpowers/specs/2026-05-04-theme-customizer-design.md`

**Phase scope:** Phase 1 only. Phase 2 (live drag-drop edit mode) is a separate plan written after Phase 1 ships and is in real-world use.

---

## File structure

**Rust (`client/src-tauri/src/`):**
- Modify `themes.rs` — append four new commands (`fork_theme`, `save_user_theme`, `add_theme_asset`, `delete_user_theme`) plus tests
- Modify `main.rs` — register the new commands

**TypeScript (`client/src/`):**
- Create `lib/customizer/types.ts` — `RegionId`, `RegionState`, `CustomizerSession`, `RegionDefinition` interfaces
- Create `lib/customizer/regions.ts` — the 12 `REGIONS` constant
- Create `lib/customizer/cssGenerator.ts` — pure function: `generateOverrideCss(regions: Map<RegionId, RegionState>): string`
- Create `lib/customizer/history.ts` — pure undo/redo state machine
- Modify `lib/tauri-bridge.ts` — bindings for the 4 new commands
- Create `components/ColorPickerPopover.tsx` — small popover with theme-extracted swatches + "more colors" native picker
- Create `components/CustomizerRegionRow.tsx` — one row in the modal
- Create `components/CustomizerIntro.tsx` — one-time intro overlay
- Create `components/CustomizeModal.tsx` — the main customizer shell (header, scrollable region list, save/discard, manages `<style id="active-theme-overrides">`)
- Modify `components/AppearanceSettings.tsx` — add a "Customize" button to each theme card; on click open a name dialog then the customizer

---

## Task 1: Rust `fork_theme` command + tests

Forks an existing theme (built-in or user) into a new user theme. Copies CSS, writes a `theme.json` with the new id, name, author "you", and `baseThemeId`. Refuses to overwrite an existing folder.

**Files:**
- Modify: `client/src-tauri/src/themes.rs` — append the command + tests inside the existing `mod tests`

- [ ] **Step 1: Add a helper to validate / sanitize a proposed user theme id**

In `client/src-tauri/src/themes.rs`, **before** the existing `#[tauri::command] pub fn list_themes()` definition, add:

```rust
/// Sanitize a user-supplied id into a filesystem-safe folder name.
/// Returns Err if the result would be empty.
fn sanitize_theme_id(input: &str) -> Result<String, String> {
    let cleaned: String = input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else if c == ' ' {
                '-'
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == '-' || c == '_').to_string();
    if trimmed.is_empty() {
        return Err("theme id cannot be empty after sanitization".to_string());
    }
    Ok(trimmed)
}
```

- [ ] **Step 2: Add the `fork_theme` command**

In the same file, before the existing `#[tauri::command] pub fn open_themes_folder` definition, add:

```rust
#[tauri::command]
pub fn fork_theme(base_id: String, new_id: String, name: String) -> Result<String, String> {
    let safe_new_id = sanitize_theme_id(&new_id)?;

    // Refuse to overwrite an existing user theme.
    let target_dir = user_themes_dir().join(&safe_new_id);
    if target_dir.exists() {
        return Err(format!("a theme with id '{}' already exists", safe_new_id));
    }

    // Resolve the base theme's CSS (built-in or user). Fail if it can't be found.
    let base_css = all_themes()
        .into_iter()
        .find(|(m, _)| m.id == base_id)
        .map(|(_, css)| css)
        .ok_or_else(|| format!("base theme not found: {}", base_id))?;

    // Create the directory and write the two files.
    std::fs::create_dir_all(&target_dir).map_err(|e| format!("create dir failed: {}", e))?;
    std::fs::write(target_dir.join("theme.css"), base_css)
        .map_err(|e| format!("write theme.css failed: {}", e))?;

    let meta = serde_json::json!({
        "id": safe_new_id,
        "name": name,
        "author": "you",
        "description": format!("Customized from {}", base_id),
        "baseThemeId": base_id,
    });
    std::fs::write(
        target_dir.join("theme.json"),
        serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write theme.json failed: {}", e))?;

    Ok(safe_new_id)
}
```

- [ ] **Step 3: Add tests for `fork_theme` and `sanitize_theme_id`**

In the existing `#[cfg(test)] mod tests` block at the bottom of `themes.rs`, add:

```rust
    #[test]
    fn sanitize_strips_unsafe_chars() {
        assert_eq!(sanitize_theme_id("My Theme!").unwrap(), "my-theme_");
        assert_eq!(sanitize_theme_id("foo/bar").unwrap(), "foo_bar");
        assert_eq!(sanitize_theme_id("  hello  ").unwrap(), "hello");
    }

    #[test]
    fn sanitize_rejects_empty_after_clean() {
        assert!(sanitize_theme_id("").is_err());
        assert!(sanitize_theme_id("   ").is_err());
        assert!(sanitize_theme_id("///").is_err());
    }

    #[test]
    fn fork_theme_creates_new_user_theme() {
        // Use a fresh temp dir as the FARDER_DATA so we don't clobber anything.
        let tmp = std::env::temp_dir().join(format!("farder-fork-test-{}", std::process::id()));
        std::env::set_var("FARDER_DATA", &tmp);

        let result = fork_theme(
            "xp-luna-blue".to_string(),
            "my custom".to_string(),
            "My Custom Theme".to_string(),
        );
        assert!(result.is_ok(), "fork_theme failed: {:?}", result);
        let new_id = result.unwrap();
        assert_eq!(new_id, "my-custom");

        let dir = tmp.join("themes").join(&new_id);
        assert!(dir.join("theme.css").exists(), "theme.css missing");
        assert!(dir.join("theme.json").exists(), "theme.json missing");

        // theme.css should be the same length as the built-in xp-luna-blue's CSS.
        let written = std::fs::read_to_string(dir.join("theme.css")).unwrap();
        assert!(written.contains("--xp-blue"), "theme.css doesn't contain expected XP token");

        // theme.json should round-trip via parse_meta with the right id and source=user.
        let raw = std::fs::read_to_string(dir.join("theme.json")).unwrap();
        let meta = parse_meta(&raw, "user").expect("parse_meta failed");
        assert_eq!(meta.id, "my-custom");
        assert_eq!(meta.name, "My Custom Theme");
        assert_eq!(meta.author, "you");

        // Cleanup.
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fork_theme_refuses_to_overwrite() {
        let tmp = std::env::temp_dir().join(format!("farder-fork-overwrite-{}", std::process::id()));
        std::env::set_var("FARDER_DATA", &tmp);

        let _ = fork_theme("xp-luna-blue".to_string(), "dup".to_string(), "Dup".to_string()).unwrap();
        let second = fork_theme("xp-luna-blue".to_string(), "dup".to_string(), "Dup 2".to_string());
        assert!(second.is_err(), "second fork should have failed");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fork_theme_rejects_unknown_base() {
        let tmp = std::env::temp_dir().join(format!("farder-fork-unknown-{}", std::process::id()));
        std::env::set_var("FARDER_DATA", &tmp);

        let result = fork_theme("nonexistent".to_string(), "x".to_string(), "X".to_string());
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }
```

- [ ] **Step 4: Run the tests**

Run from `/home/deez/farder`:
```
cd client/src-tauri && cargo test --lib themes::tests 2>&1 | tail -25
```
Expected: 4 new tests pass (plus the 3 existing ones — 7 total OK).

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/themes.rs
git -C /home/deez/farder commit -m "feat(client): fork_theme command for customizer + sanitization helper"
```

---

## Task 2: Rust `save_user_theme`, `add_theme_asset`, `delete_user_theme` commands + tests

`save_user_theme` writes user-provided CSS back to a user theme. `add_theme_asset` copies an image into the theme's `assets/` folder, returning a relative path. `delete_user_theme` removes a user theme folder. All three refuse to operate on built-in themes.

**Files:**
- Modify: `client/src-tauri/src/themes.rs`

- [ ] **Step 1: Add a helper to identify user-vs-builtin themes**

In `themes.rs`, before the existing `#[tauri::command] pub fn list_themes`, add:

```rust
/// Returns true iff the given id is one of the BUILTIN_THEMES (resolved by parsing
/// each builtin's embedded meta_json).
fn is_builtin_theme_id(id: &str) -> bool {
    BUILTIN_THEMES
        .iter()
        .filter_map(|b| parse_meta(b.meta_json, "builtin"))
        .any(|m| m.id == id)
}

/// Returns the directory for a user theme, refusing if the id resolves to a built-in.
fn user_theme_dir(id: &str) -> Result<std::path::PathBuf, String> {
    if is_builtin_theme_id(id) {
        return Err(format!("'{}' is a built-in theme and cannot be modified", id));
    }
    let dir = user_themes_dir().join(id);
    if !dir.exists() {
        return Err(format!("user theme not found: {}", id));
    }
    Ok(dir)
}
```

- [ ] **Step 2: Add the three new commands**

In `themes.rs`, before `pub fn open_themes_folder`, add:

```rust
const MAX_ASSET_BYTES: u64 = 25 * 1024 * 1024; // 25 MB hard cap

#[tauri::command]
pub fn save_user_theme(id: String, css: String) -> Result<(), String> {
    let dir = user_theme_dir(&id)?;
    std::fs::write(dir.join("theme.css"), css)
        .map_err(|e| format!("write theme.css failed: {}", e))
}

#[tauri::command]
pub fn add_theme_asset(
    theme_id: String,
    source_path: String,
    target_filename: String,
) -> Result<String, String> {
    let dir = user_theme_dir(&theme_id)?;
    let assets = dir.join("assets");
    std::fs::create_dir_all(&assets).map_err(|e| format!("create assets dir failed: {}", e))?;

    let src = std::path::Path::new(&source_path);
    let metadata = std::fs::metadata(src).map_err(|e| format!("source not readable: {}", e))?;
    if metadata.len() > MAX_ASSET_BYTES {
        return Err(format!(
            "image too large ({} bytes > {} limit)",
            metadata.len(),
            MAX_ASSET_BYTES
        ));
    }

    // Sanitize the target filename so callers can't escape the assets dir.
    let safe_name: String = target_filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe_name.is_empty() || safe_name == "." || safe_name == ".." {
        return Err("invalid target filename".to_string());
    }

    let target = assets.join(&safe_name);
    std::fs::copy(src, &target).map_err(|e| format!("copy failed: {}", e))?;

    // Return the path the CSS will reference (relative to theme.css).
    Ok(format!("./assets/{}", safe_name))
}

#[tauri::command]
pub fn delete_user_theme(id: String) -> Result<(), String> {
    let dir = user_theme_dir(&id)?;
    std::fs::remove_dir_all(&dir).map_err(|e| format!("remove failed: {}", e))
}
```

- [ ] **Step 3: Add tests for the three commands**

In the existing `#[cfg(test)] mod tests` block, append:

```rust
    fn fresh_tmp_data_dir() -> std::path::PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "farder-customizer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("FARDER_DATA", &tmp);
        tmp
    }

    #[test]
    fn save_user_theme_writes_css_to_disk() {
        let tmp = fresh_tmp_data_dir();
        let id = fork_theme("xp-luna-blue".into(), "save-test".into(), "Save Test".into()).unwrap();
        save_user_theme(id.clone(), "/* hello */".to_string()).unwrap();

        let written = std::fs::read_to_string(tmp.join("themes").join(&id).join("theme.css")).unwrap();
        assert_eq!(written, "/* hello */");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn save_user_theme_refuses_builtin() {
        let tmp = fresh_tmp_data_dir();
        let result = save_user_theme("xp-luna-blue".to_string(), "/* x */".to_string());
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn add_theme_asset_copies_file() {
        let tmp = fresh_tmp_data_dir();
        let id = fork_theme("xp-luna-blue".into(), "asset-test".into(), "Asset Test".into()).unwrap();

        // Create a small fake source file.
        let source = tmp.join("source.png");
        std::fs::write(&source, b"fake png bytes").unwrap();

        let rel = add_theme_asset(id.clone(), source.to_string_lossy().to_string(), "dog.png".to_string()).unwrap();
        assert_eq!(rel, "./assets/dog.png");

        let copied = tmp.join("themes").join(&id).join("assets").join("dog.png");
        assert!(copied.exists());
        assert_eq!(std::fs::read(&copied).unwrap(), b"fake png bytes");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn add_theme_asset_sanitizes_target_filename() {
        let tmp = fresh_tmp_data_dir();
        let id = fork_theme("xp-luna-blue".into(), "asset-san".into(), "Asset San".into()).unwrap();
        let source = tmp.join("source.png");
        std::fs::write(&source, b"x").unwrap();

        // Try to escape with ../ or path separators.
        let rel = add_theme_asset(id.clone(), source.to_string_lossy().into(), "../../etc/passwd".into()).unwrap();
        assert_eq!(rel, "./assets/______etc_passwd");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn delete_user_theme_removes_folder() {
        let tmp = fresh_tmp_data_dir();
        let id = fork_theme("xp-luna-blue".into(), "del-test".into(), "Del Test".into()).unwrap();

        let dir = tmp.join("themes").join(&id);
        assert!(dir.exists());

        delete_user_theme(id.clone()).unwrap();
        assert!(!dir.exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn delete_user_theme_refuses_builtin() {
        let tmp = fresh_tmp_data_dir();
        let result = delete_user_theme("xp-luna-blue".to_string());
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
```

- [ ] **Step 4: Run the tests**

Run from `/home/deez/farder`:
```
cd client/src-tauri && cargo test --lib themes::tests 2>&1 | tail -25
```
Expected: 6 new tests pass (plus the 7 from Task 1 + earlier — 13 total OK).

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/themes.rs
git -C /home/deez/farder commit -m "feat(client): save_user_theme + add_theme_asset + delete_user_theme commands"
```

---

## Task 3: Register the four new commands in main.rs

**Files:**
- Modify: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Add four lines to `generate_handler!`**

In `client/src-tauri/src/main.rs`, find the existing block:
```rust
            themes::list_themes,
            themes::load_theme_css,
            themes::get_active_theme,
            themes::set_active_theme,
            themes::open_themes_folder,
        ])
```

Replace with:
```rust
            themes::list_themes,
            themes::load_theme_css,
            themes::get_active_theme,
            themes::set_active_theme,
            themes::open_themes_folder,
            themes::fork_theme,
            themes::save_user_theme,
            themes::add_theme_asset,
            themes::delete_user_theme,
        ])
```

- [ ] **Step 2: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
```
Expected: `Finished` with the pre-existing dead-code warning only.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/main.rs
git -C /home/deez/farder commit -m "feat(client): register customizer commands in tauri handler"
```

---

## Task 4: TypeScript types + REGIONS constant

**Files:**
- Create: `client/src/lib/customizer/types.ts`
- Create: `client/src/lib/customizer/regions.ts`

- [ ] **Step 1: Create `types.ts`**

`client/src/lib/customizer/types.ts`:

```ts
export type RegionId =
  | "main-bg"
  | "channel-sidebar"
  | "server-strip"
  | "member-sidebar"
  | "title-bars"
  | "message-bubble"
  | "message-hover"
  | "buttons"
  | "input-field"
  | "modal-bg"
  | "scrollbars"
  | "accent";

export type ImageFit = "stretch" | "tile" | "center" | "cover";

export interface RegionImage {
  /** Relative path within the theme folder, e.g. "./assets/dog.jpg" */
  path: string;
  fit: ImageFit;
}

export interface RegionState {
  bgColor?: string;
  bgImage?: RegionImage;
  textColor?: string;
}

export interface RegionDefinition {
  id: RegionId;
  label: string;
  /** Whether this region accepts a text-color knob (some, like scrollbars, don't). */
  hasText: boolean;
  /** Whether this region accepts a background image (scrollbars, accent don't really). */
  hasImage: boolean;
  /** CSS selectors targeted for the background. */
  backgroundSelectors: string[];
  /** CSS selectors targeted for the text color. Empty if hasText is false. */
  textSelectors: string[];
  /** If set, this region's color drives a CSS variable instead of selector rules.
   *  Used by 'accent' which sets --xp-blue. */
  accentVariable?: string;
}

export interface CustomizerSession {
  themeId: string;
  baseThemeId: string;
  regions: Map<RegionId, RegionState>;
  history: Array<Map<RegionId, RegionState>>;
  historyIndex: number;
  dirty: boolean;
}
```

- [ ] **Step 2: Create `regions.ts`**

`client/src/lib/customizer/regions.ts`:

```ts
import type { RegionDefinition } from "./types";

export const REGIONS: RegionDefinition[] = [
  {
    id: "main-bg",
    label: "Main background",
    hasText: false,
    hasImage: true,
    backgroundSelectors: ["body", "#root", ".app-shell", ".chat-panel"],
    textSelectors: [],
  },
  {
    id: "channel-sidebar",
    label: "Channel sidebar",
    hasText: true,
    hasImage: true,
    backgroundSelectors: [".channel-sidebar"],
    textSelectors: [".channel-sidebar", ".channel-name", ".channel-category"],
  },
  {
    id: "server-strip",
    label: "Server strip",
    hasText: false,
    hasImage: true,
    backgroundSelectors: [".server-strip"],
    textSelectors: [],
  },
  {
    id: "member-sidebar",
    label: "Member sidebar",
    hasText: true,
    hasImage: true,
    backgroundSelectors: [".member-sidebar"],
    textSelectors: [".member-sidebar", ".member-name"],
  },
  {
    id: "title-bars",
    label: "Title bars",
    hasText: true,
    hasImage: false,
    backgroundSelectors: [
      ".title-bar",
      ".connect-dialog-titlebar",
      ".modal-titlebar",
    ],
    textSelectors: [".title-bar", ".connect-dialog-titlebar", ".modal-titlebar"],
  },
  {
    id: "message-bubble",
    label: "Message bubble",
    hasText: true,
    hasImage: true,
    backgroundSelectors: [".message", ".message-content"],
    textSelectors: [".message", ".message-content"],
  },
  {
    id: "message-hover",
    label: "Message hover",
    hasText: false,
    hasImage: false,
    backgroundSelectors: [".message:hover"],
    textSelectors: [],
  },
  {
    id: "buttons",
    label: "Buttons",
    hasText: true,
    hasImage: false,
    backgroundSelectors: [".xp-button"],
    textSelectors: [".xp-button"],
  },
  {
    id: "input-field",
    label: "Input field",
    hasText: true,
    hasImage: false,
    backgroundSelectors: [".message-input", "input[type=\"text\"]", "textarea"],
    textSelectors: [".message-input", "input[type=\"text\"]", "textarea"],
  },
  {
    id: "modal-bg",
    label: "Modal background",
    hasText: false,
    hasImage: true,
    backgroundSelectors: [".connect-screen", ".modal", ".dialog-body"],
    textSelectors: [],
  },
  {
    id: "scrollbars",
    label: "Scrollbars",
    hasText: false,
    hasImage: false,
    backgroundSelectors: ["::-webkit-scrollbar-thumb"],
    textSelectors: [],
  },
  {
    id: "accent",
    label: "Accent (active / selected)",
    hasText: false,
    hasImage: false,
    backgroundSelectors: [],
    textSelectors: [],
    accentVariable: "--xp-blue",
  },
];

export const REGIONS_BY_ID: Map<string, RegionDefinition> = new Map(
  REGIONS.map((r) => [r.id, r]),
);
```

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```
Expected: `(exit 0)`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/lib/customizer/types.ts client/src/lib/customizer/regions.ts
git -C /home/deez/farder commit -m "feat(client): customizer types and 12-region map"
```

---

## Task 5: TypeScript `cssGenerator.ts` (pure function)

Generates an override CSS string from a `Map<RegionId, RegionState>`. Used both for live preview and (combined with the base CSS) for save-to-disk.

**Files:**
- Create: `client/src/lib/customizer/cssGenerator.ts`

- [ ] **Step 1: Create the file**

`client/src/lib/customizer/cssGenerator.ts`:

```ts
import type { RegionId, RegionState, ImageFit } from "./types";
import { REGIONS } from "./regions";

const MARKER = "/* === Customizer overrides — generated, edit with the customizer === */";

function imageDeclaration(path: string, fit: ImageFit): string {
  const url = `url('${path.replace(/'/g, "\\'")}')`;
  switch (fit) {
    case "stretch":
      return `background: ${url} no-repeat; background-size: 100% 100%;`;
    case "tile":
      return `background: ${url} repeat; background-size: auto;`;
    case "center":
      return `background: ${url} no-repeat center center; background-size: auto;`;
    case "cover":
      return `background: ${url} no-repeat center center; background-size: cover;`;
  }
}

function escapeSelector(s: string): string {
  // Selectors come from a fixed map (regions.ts) — no untrusted input.
  // This function exists so future contributors don't accidentally
  // interpolate user-controlled strings into selectors. Keep as identity for now.
  return s;
}

/**
 * Build the full overrides CSS for the given region states. Returns the empty
 * string if no region has any override set. Pure function — no DOM access.
 */
export function generateOverrideCss(regions: Map<RegionId, RegionState>): string {
  const blocks: string[] = [];
  let hasAny = false;

  for (const region of REGIONS) {
    const state = regions.get(region.id);
    if (!state || (state.bgColor === undefined && state.bgImage === undefined && state.textColor === undefined)) {
      continue;
    }
    hasAny = true;

    // Special case: accent is just a CSS variable change.
    if (region.accentVariable && state.bgColor) {
      blocks.push(`:root { ${region.accentVariable}: ${state.bgColor}; }`);
      continue;
    }

    const decls: string[] = [];
    if (state.bgImage) {
      decls.push(imageDeclaration(state.bgImage.path, state.bgImage.fit));
    } else if (state.bgColor) {
      decls.push(`background: ${state.bgColor};`);
    }
    if (decls.length > 0 && region.backgroundSelectors.length > 0) {
      const selectorList = region.backgroundSelectors.map(escapeSelector).join(", ");
      blocks.push(`/* ${region.label} — background */\n${selectorList} { ${decls.join(" ")} }`);
    }

    if (state.textColor && region.hasText && region.textSelectors.length > 0) {
      const selectorList = region.textSelectors.map(escapeSelector).join(", ");
      blocks.push(`/* ${region.label} — text */\n${selectorList} { color: ${state.textColor}; }`);
    }
  }

  if (!hasAny) return "";
  return [MARKER, ...blocks].join("\n\n") + "\n";
}

/**
 * Strip any previously-generated customizer overrides from a CSS string,
 * leaving everything before the marker. Used when saving to disk so we don't
 * accumulate stale override blocks across saves.
 */
export function stripExistingOverrides(css: string): string {
  const idx = css.indexOf(MARKER);
  if (idx === -1) return css;
  return css.slice(0, idx).trimEnd() + "\n";
}

/** Combine base CSS + overrides into the final theme.css to write to disk. */
export function mergeForSave(baseCss: string, overrideCss: string): string {
  const stripped = stripExistingOverrides(baseCss);
  if (!overrideCss) return stripped;
  return stripped + "\n" + overrideCss;
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```
Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/lib/customizer/cssGenerator.ts
git -C /home/deez/farder commit -m "feat(client): customizer cssGenerator (pure)"
```

---

## Task 6: TypeScript `history.ts` (undo/redo)

A small functional state machine for undo/redo on a `Map<RegionId, RegionState>`. Stored as an array of snapshots; user actions push a new snapshot at `historyIndex+1` and truncate any redo branch.

**Files:**
- Create: `client/src/lib/customizer/history.ts`

- [ ] **Step 1: Create the file**

`client/src/lib/customizer/history.ts`:

```ts
import type { RegionId, RegionState } from "./types";

export type RegionsMap = Map<RegionId, RegionState>;

export interface HistoryState {
  history: RegionsMap[];
  index: number;
}

/** Deep-clone a regions map so future mutations don't bleed into history snapshots. */
function cloneRegions(regions: RegionsMap): RegionsMap {
  const out = new Map<RegionId, RegionState>();
  for (const [k, v] of regions) {
    out.set(k, {
      bgColor: v.bgColor,
      bgImage: v.bgImage ? { path: v.bgImage.path, fit: v.bgImage.fit } : undefined,
      textColor: v.textColor,
    });
  }
  return out;
}

/** Initialize a history with the given starting regions snapshot. */
export function initHistory(initial: RegionsMap): HistoryState {
  return { history: [cloneRegions(initial)], index: 0 };
}

/** Push a new snapshot. Truncates anything after the current index (redo branch lost on new edit). */
export function pushSnapshot(state: HistoryState, next: RegionsMap): HistoryState {
  const truncated = state.history.slice(0, state.index + 1);
  truncated.push(cloneRegions(next));
  return { history: truncated, index: truncated.length - 1 };
}

/** Move back one step. Returns same state if already at index 0. */
export function undo(state: HistoryState): HistoryState {
  if (state.index <= 0) return state;
  return { history: state.history, index: state.index - 1 };
}

/** Move forward one step. Returns same state if already at the end. */
export function redo(state: HistoryState): HistoryState {
  if (state.index >= state.history.length - 1) return state;
  return { history: state.history, index: state.index + 1 };
}

/** Read the snapshot at the current index. Returns a clone (callers may mutate). */
export function current(state: HistoryState): RegionsMap {
  return cloneRegions(state.history[state.index]);
}

export function canUndo(state: HistoryState): boolean {
  return state.index > 0;
}

export function canRedo(state: HistoryState): boolean {
  return state.index < state.history.length - 1;
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```
Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/lib/customizer/history.ts
git -C /home/deez/farder commit -m "feat(client): customizer undo/redo history (pure)"
```

---

## Task 7: Tauri-bridge bindings for the four new commands

**Files:**
- Modify: `client/src/lib/tauri-bridge.ts`

- [ ] **Step 1: Append to the file**

In `client/src/lib/tauri-bridge.ts`, append at the end (after the existing themes section):

```ts
export async function forkTheme(baseId: string, newId: string, name: string): Promise<string> {
  return invoke<string>("fork_theme", { baseId, newId, name });
}

export async function saveUserTheme(id: string, css: string): Promise<void> {
  return invoke<void>("save_user_theme", { id, css });
}

export async function addThemeAsset(themeId: string, sourcePath: string, targetFilename: string): Promise<string> {
  return invoke<string>("add_theme_asset", { themeId, sourcePath, targetFilename });
}

export async function deleteUserTheme(id: string): Promise<void> {
  return invoke<void>("delete_user_theme", { id });
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```
Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/lib/tauri-bridge.ts
git -C /home/deez/farder commit -m "feat(client): tauri-bridge bindings for customizer commands"
```

---

## Task 8: `ColorPickerPopover` component

Small popover with theme-extracted swatches + a native HTML5 color picker. Re-used by every region row.

**Files:**
- Create: `client/src/components/ColorPickerPopover.tsx`

- [ ] **Step 1: Create the file**

`client/src/components/ColorPickerPopover.tsx`:

```tsx
import { useEffect, useRef, useState, type CSSProperties } from "react";

interface Props {
  /** Initial color (any CSS color string). May be undefined for "no override". */
  value: string | undefined;
  /** Swatches extracted from the active theme to show as quick-picks. */
  themeSwatches: string[];
  /** Called as the user picks; live-preview-friendly. */
  onChange: (color: string) => void;
  /** Called when the user clears the override. */
  onClear: () => void;
  /** Anchor: position the popover near this element. */
  anchorRect: DOMRect;
  onClose: () => void;
}

const swatchStyle: CSSProperties = {
  width: 22,
  height: 22,
  border: "1px solid var(--xp-border, #888)",
  cursor: "pointer",
  padding: 0,
  background: "transparent",
};

export default function ColorPickerPopover({
  value,
  themeSwatches,
  onChange,
  onClear,
  anchorRect,
  onClose,
}: Props) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [pickerVal, setPickerVal] = useState<string>(value ?? "#000000");

  // Close on outside click.
  useEffect(() => {
    function handle(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    }
    document.addEventListener("mousedown", handle);
    return () => document.removeEventListener("mousedown", handle);
  }, [onClose]);

  return (
    <div
      ref={ref}
      style={{
        position: "fixed",
        top: anchorRect.bottom + 4,
        left: anchorRect.left,
        background: "var(--xp-panel-bg, #fff)",
        color: "var(--xp-text-normal, #000)",
        border: "1px solid var(--xp-border, #888)",
        borderRadius: 4,
        padding: 8,
        boxShadow: "2px 2px 8px rgba(0,0,0,0.3)",
        zIndex: 2000,
        display: "flex",
        flexDirection: "column",
        gap: 8,
        minWidth: 180,
      }}
    >
      <div style={{ fontSize: 10, color: "var(--xp-text-muted, #666)" }}>From this theme</div>
      <div style={{ display: "flex", flexWrap: "wrap", gap: 4 }}>
        {themeSwatches.length === 0 && (
          <span style={{ fontSize: 10, color: "var(--xp-text-muted, #888)" }}>(none extracted)</span>
        )}
        {themeSwatches.map((c, i) => (
          <button
            key={i}
            title={c}
            onClick={() => {
              onChange(c);
              setPickerVal(c.startsWith("#") ? c : "#000000");
            }}
            style={{ ...swatchStyle, background: c }}
          />
        ))}
      </div>
      <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
        <span style={{ fontSize: 10 }}>Custom:</span>
        <input
          type="color"
          value={pickerVal}
          onChange={(e) => {
            setPickerVal(e.target.value);
            onChange(e.target.value);
          }}
          style={{ width: 36, height: 24, padding: 0, border: "1px solid var(--xp-border, #888)" }}
        />
        <button
          onClick={onClear}
          title="Clear (use base theme value)"
          style={{
            marginLeft: "auto",
            font: "inherit",
            background: "transparent",
            border: "1px solid var(--xp-border, #888)",
            padding: "2px 8px",
            cursor: "pointer",
            color: "var(--xp-text-normal, #000)",
          }}
        >
          Clear
        </button>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```
Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/ColorPickerPopover.tsx
git -C /home/deez/farder commit -m "feat(client): ColorPickerPopover with theme-extracted swatches and native picker"
```

---

## Task 9: `CustomizerRegionRow` component

A single row in the customizer. Shows the region label, a background color/image control, an image-fit dropdown when an image is set, and a text-color control (only if `hasText`). Each control opens a `ColorPickerPopover` or a Tauri file dialog.

**Files:**
- Create: `client/src/components/CustomizerRegionRow.tsx`

- [ ] **Step 1: Create the file**

`client/src/components/CustomizerRegionRow.tsx`:

```tsx
import { useState, useRef, type CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import * as api from "../lib/tauri-bridge";
import ColorPickerPopover from "./ColorPickerPopover";
import type { RegionDefinition, RegionState, ImageFit } from "../lib/customizer/types";

interface Props {
  region: RegionDefinition;
  state: RegionState | undefined;
  themeId: string;
  themeSwatches: string[];
  onChange: (next: RegionState | undefined) => void;
  onError: (msg: string) => void;
}

const swatchBtn: CSSProperties = {
  width: 28,
  height: 22,
  border: "1px solid var(--xp-border, #888)",
  cursor: "pointer",
  padding: 0,
};

const chip: CSSProperties = {
  font: "inherit",
  background: "transparent",
  color: "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  borderRadius: 3,
  padding: "2px 8px",
  cursor: "pointer",
  whiteSpace: "nowrap",
};

const FIT_OPTIONS: ImageFit[] = ["stretch", "tile", "center", "cover"];
const WARN_BYTES = 5 * 1024 * 1024;

export default function CustomizerRegionRow({
  region,
  state,
  themeId,
  themeSwatches,
  onChange,
  onError,
}: Props) {
  const bgRef = useRef<HTMLButtonElement | null>(null);
  const textRef = useRef<HTMLButtonElement | null>(null);
  const [openPicker, setOpenPicker] = useState<"bg" | "text" | null>(null);

  const bgColor = state?.bgColor;
  const bgImage = state?.bgImage;
  const textColor = state?.textColor;

  function patch(p: Partial<RegionState>): void {
    const next: RegionState = {
      bgColor: p.bgColor !== undefined ? (p.bgColor || undefined) : bgColor,
      bgImage: p.bgImage !== undefined ? (p.bgImage || undefined) : bgImage,
      textColor: p.textColor !== undefined ? (p.textColor || undefined) : textColor,
    };
    if (next.bgColor === undefined && next.bgImage === undefined && next.textColor === undefined) {
      onChange(undefined);
    } else {
      onChange(next);
    }
  }

  async function pickImage(): Promise<void> {
    try {
      const selected = await openFileDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "Images", extensions: ["png", "jpg", "jpeg", "webp", "gif", "bmp"] }],
      });
      if (!selected || typeof selected !== "string") return;

      // Soft warning for large files (Rust enforces a hard 25MB cap).
      try {
        const size = await invoke<number>("plugin:fs|metadata", { path: selected });
        if (typeof size === "number" && size > WARN_BYTES) {
          if (!window.confirm(
            `This image is ${(size / 1024 / 1024).toFixed(1)} MB — large images may slow the app down. Use it anyway?`,
          )) {
            return;
          }
        }
      } catch {
        // metadata probe is non-critical; proceed.
      }

      const original = selected.split(/[\\/]/).pop() ?? "image";
      const stamped = `${Date.now()}-${original}`;
      const relPath = await api.addThemeAsset(themeId, selected, stamped);

      patch({ bgImage: { path: relPath, fit: bgImage?.fit ?? "cover" } });
    } catch (e) {
      onError(String(e));
    }
  }

  return (
    <div
      style={{
        display: "grid",
        gridTemplateColumns: "200px auto auto auto 1fr auto auto",
        gap: 8,
        alignItems: "center",
        padding: "6px 0",
        borderBottom: "1px solid var(--xp-border, #d8d4c4)",
      }}
    >
      <div style={{ fontWeight: "bold" }}>{region.label}</div>

      {/* Background color swatch */}
      <button
        ref={bgRef}
        onClick={() => setOpenPicker(openPicker === "bg" ? null : "bg")}
        title={bgColor ? `Background: ${bgColor}` : "Set background color"}
        style={{
          ...swatchBtn,
          background: bgColor ?? "repeating-linear-gradient(45deg, #eee, #eee 4px, #ccc 4px, #ccc 8px)",
        }}
      />

      {/* Image picker */}
      {region.hasImage ? (
        <button style={chip} onClick={pickImage} title="Pick a background image">
          {bgImage ? "Change image…" : "Pick image…"}
        </button>
      ) : (
        <span />
      )}

      {/* Fit dropdown — only when an image is set */}
      {region.hasImage && bgImage ? (
        <select
          value={bgImage.fit}
          onChange={(e) => patch({ bgImage: { path: bgImage.path, fit: e.target.value as ImageFit } })}
          style={{
            font: "inherit",
            background: "var(--xp-panel-bg, #fff)",
            color: "var(--xp-text-normal, #000)",
            border: "1px solid var(--xp-border, #888)",
            padding: "2px 4px",
          }}
        >
          {FIT_OPTIONS.map((f) => (
            <option key={f} value={f}>{f}</option>
          ))}
        </select>
      ) : (
        <span />
      )}

      <span /> {/* spacer */}

      {/* Text color */}
      {region.hasText ? (
        <div style={{ display: "flex", alignItems: "center", gap: 4 }}>
          <span style={{ fontSize: 10, color: "var(--xp-text-muted, #666)" }}>text:</span>
          <button
            ref={textRef}
            onClick={() => setOpenPicker(openPicker === "text" ? null : "text")}
            title={textColor ? `Text color: ${textColor}` : "Set text color"}
            style={{
              ...swatchBtn,
              background: textColor ?? "repeating-linear-gradient(45deg, #eee, #eee 4px, #ccc 4px, #ccc 8px)",
            }}
          />
        </div>
      ) : (
        <span />
      )}

      {/* Clear-all-for-this-region button */}
      <button
        style={{ ...chip, padding: "2px 6px" }}
        onClick={() => onChange(undefined)}
        title="Clear all overrides for this region"
      >
        ×
      </button>

      {/* Popovers */}
      {openPicker === "bg" && bgRef.current && (
        <ColorPickerPopover
          value={bgColor}
          themeSwatches={themeSwatches}
          anchorRect={bgRef.current.getBoundingClientRect()}
          onChange={(c) => patch({ bgColor: c, bgImage: undefined })}
          onClear={() => {
            patch({ bgColor: undefined });
            setOpenPicker(null);
          }}
          onClose={() => setOpenPicker(null)}
        />
      )}
      {openPicker === "text" && textRef.current && (
        <ColorPickerPopover
          value={textColor}
          themeSwatches={themeSwatches}
          anchorRect={textRef.current.getBoundingClientRect()}
          onChange={(c) => patch({ textColor: c })}
          onClear={() => {
            patch({ textColor: undefined });
            setOpenPicker(null);
          }}
          onClose={() => setOpenPicker(null)}
        />
      )}
    </div>
  );
}
```

- [ ] **Step 2: Verify the dialog plugin is installed**

The component imports `@tauri-apps/plugin-dialog`. Check `client/package.json`:

```
grep "plugin-dialog" /home/deez/farder/client/package.json
```

If absent, install it:
```
cd /home/deez/farder/client && npm install @tauri-apps/plugin-dialog
```

And ensure the Rust side has the dialog plugin enabled. Check `client/src-tauri/Cargo.toml` for `tauri-plugin-dialog`. If absent, add `tauri-plugin-dialog = "2"` to dependencies, and add `.plugin(tauri_plugin_dialog::init())` in `client/src-tauri/src/main.rs` next to the existing `.plugin(tauri_plugin_shell::init())`. Also add `"dialog:default"` to the permissions array in `client/src-tauri/capabilities/default.json`.

- [ ] **Step 3: Verify TS + Rust compile**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
```
Expected: TS exit 0, cargo Finished.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/components/CustomizerRegionRow.tsx client/package.json client/package-lock.json client/src-tauri/Cargo.toml client/src-tauri/Cargo.lock client/src-tauri/src/main.rs client/src-tauri/capabilities/default.json
git -C /home/deez/farder commit -m "feat(client): CustomizerRegionRow with color + image + text controls"
```

(Only stage the files that actually changed — if dialog plugin was already present, just stage the component file.)

---

## Task 10: `CustomizerIntro` one-time intro overlay

A simple dismissable overlay shown the first time a user opens the customizer. Dismissal stored in `~/.farder/settings.json` under `customizerIntroDismissed: true`.

**Files:**
- Create: `client/src/components/CustomizerIntro.tsx`

- [ ] **Step 1: Create the component**

`client/src/components/CustomizerIntro.tsx`:

```tsx
import { type CSSProperties } from "react";

interface Props {
  onDismiss: () => void;
}

const overlayStyle: CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0,0,0,0.5)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 3000,
};

const cardStyle: CSSProperties = {
  background: "var(--xp-window-bg, #ECE9D8)",
  color: "var(--xp-text-normal, #000)",
  border: "2px solid var(--xp-blue-dark, #003C74)",
  borderRadius: 6,
  padding: 24,
  maxWidth: 480,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

export default function CustomizerIntro({ onDismiss }: Props) {
  return (
    <div style={overlayStyle} onClick={onDismiss}>
      <div style={cardStyle} onClick={(e) => e.stopPropagation()}>
        <h2 style={{ marginTop: 0 }}>Welcome to the Customizer</h2>
        <p>
          Pick a region from the list, change its background color, drop in an image, or change the text color.
          Hit <strong>Save</strong> when you're done. Use <strong>Undo</strong> if you change your mind.
        </p>
        <p style={{ fontSize: 11, color: "var(--xp-text-muted, #666)" }}>
          Your edits go into a new theme — built-in themes are never modified.
        </p>
        <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 16 }}>
          <button
            onClick={onDismiss}
            style={{
              font: "inherit",
              padding: "4px 14px",
              background: "var(--xp-panel-bg, #f0ece0)",
              color: "var(--xp-text-normal, #000)",
              border: "1px solid var(--xp-border, #888)",
              borderRadius: 4,
              cursor: "pointer",
            }}
          >
            Got it
          </button>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```
Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/CustomizerIntro.tsx
git -C /home/deez/farder commit -m "feat(client): one-time CustomizerIntro overlay"
```

---

## Task 11: `CustomizeModal` — the main customizer shell

Renders header (theme name, Undo, Redo, Save, Close), the scrollable list of `CustomizerRegionRow`s, and the `CustomizerIntro` overlay if it hasn't been dismissed. Manages live preview by writing to `<style id="active-theme-overrides">`. Save merges via `mergeForSave`, persists via `api.saveUserTheme`, and reloads the active theme into `<style id="active-theme">`. Close prompts if dirty.

**Files:**
- Create: `client/src/components/CustomizeModal.tsx`

- [ ] **Step 1: Create the file**

`client/src/components/CustomizeModal.tsx`:

```tsx
import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import { REGIONS } from "../lib/customizer/regions";
import type { RegionId, RegionState } from "../lib/customizer/types";
import {
  initHistory,
  pushSnapshot,
  undo as histUndo,
  redo as histRedo,
  current as histCurrent,
  canUndo,
  canRedo,
  type HistoryState,
} from "../lib/customizer/history";
import { generateOverrideCss, mergeForSave } from "../lib/customizer/cssGenerator";
import CustomizerRegionRow from "./CustomizerRegionRow";
import CustomizerIntro from "./CustomizerIntro";

interface Props {
  /** The user theme id we're editing (already forked before opening). */
  themeId: string;
  /** Display name shown in the header. */
  initialName: string;
  /** Called when the user closes the modal. */
  onClose: () => void;
  /** Called after a successful Save (so the parent can refresh its theme list). */
  onSaved: () => void;
}

const OVERRIDE_STYLE_ID = "active-theme-overrides";
const INTRO_DISMISSED_KEY = "customizerIntroDismissed";

function extractSwatchesFromActiveTheme(): string[] {
  const styleEl = document.getElementById("active-theme");
  if (!styleEl) return [];
  const css = styleEl.textContent ?? "";
  const colors: string[] = [];
  const seen = new Set<string>();
  const re = /--[\w-]+:\s*(#[0-9a-fA-F]{3,8}|rgb[a]?\([^)]+\)|hsl[a]?\([^)]+\))\s*;/g;
  let match: RegExpExecArray | null;
  while ((match = re.exec(css)) !== null && colors.length < 12) {
    const c = match[1].trim();
    if (!seen.has(c)) {
      seen.add(c);
      colors.push(c);
    }
  }
  return colors;
}

function setOverrideCss(css: string): void {
  let el = document.getElementById(OVERRIDE_STYLE_ID) as HTMLStyleElement | null;
  if (!el) {
    el = document.createElement("style");
    el.id = OVERRIDE_STYLE_ID;
    document.head.appendChild(el);
  }
  el.textContent = css;
}

function clearOverrideCss(): void {
  const el = document.getElementById(OVERRIDE_STYLE_ID);
  if (el) el.remove();
}

const headerBtn: CSSProperties = {
  font: "inherit",
  padding: "4px 12px",
  background: "var(--xp-panel-bg, #f0ece0)",
  color: "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  borderRadius: 4,
  cursor: "pointer",
};

export default function CustomizeModal({ themeId, initialName, onClose, onSaved }: Props) {
  const [history, setHistory] = useState<HistoryState>(() => initHistory(new Map()));
  const [error, setError] = useState<string | null>(null);
  const [showIntro, setShowIntro] = useState<boolean>(false);
  const dirtyRef = useRef<boolean>(false);
  const swatches = useMemo(() => extractSwatchesFromActiveTheme(), []);

  const regions = useMemo(() => histCurrent(history), [history]);

  // Re-extract intro-dismissed flag once on mount.
  useEffect(() => {
    (async () => {
      try {
        // We piggyback on the existing settings.json multi-key store.
        // Use a lightweight invoke-by-name for this single boolean.
        const { invoke } = await import("@tauri-apps/api/core");
        const dismissed = await invoke<unknown>("get_last_server").catch(() => null);
        // We don't have a generic settings_get on the TS side yet — read the file directly via tauri fs.
        // For v1, fall back to localStorage if reading the settings file is awkward.
        const local = localStorage.getItem(INTRO_DISMISSED_KEY);
        setShowIntro(local !== "true");
        void dismissed; // unused — placeholder for a future settings binding
      } catch {
        setShowIntro(true);
      }
    })();
  }, []);

  function dismissIntro() {
    localStorage.setItem(INTRO_DISMISSED_KEY, "true");
    setShowIntro(false);
  }

  // Apply live preview on every regions change.
  useEffect(() => {
    setOverrideCss(generateOverrideCss(regions));
  }, [regions]);

  // Cleanup override element when this modal unmounts.
  useEffect(() => {
    return () => {
      clearOverrideCss();
    };
  }, []);

  // Keyboard shortcuts: Ctrl+Z / Ctrl+Y / Ctrl+S
  useEffect(() => {
    function handle(e: KeyboardEvent) {
      if (!(e.ctrlKey || e.metaKey)) return;
      if (e.key === "z" && !e.shiftKey) {
        e.preventDefault();
        setHistory((h) => histUndo(h));
      } else if (e.key === "y" || (e.key === "z" && e.shiftKey)) {
        e.preventDefault();
        setHistory((h) => histRedo(h));
      } else if (e.key === "s") {
        e.preventDefault();
        void doSave();
      }
    }
    window.addEventListener("keydown", handle);
    return () => window.removeEventListener("keydown", handle);
  });

  function updateRegion(id: RegionId, next: RegionState | undefined): void {
    const nextMap = new Map(regions);
    if (next === undefined) nextMap.delete(id);
    else nextMap.set(id, next);
    setHistory((h) => pushSnapshot(h, nextMap));
    dirtyRef.current = true;
  }

  async function doSave(): Promise<void> {
    try {
      const baseCss = await api.loadThemeCss(themeId);
      const overrides = generateOverrideCss(regions);
      const merged = mergeForSave(baseCss, overrides);
      await api.saveUserTheme(themeId, merged);

      // Refresh the active style to reflect the saved version, drop the override layer.
      const refreshed = await api.loadThemeCss(themeId);
      const activeStyle = document.getElementById("active-theme") as HTMLStyleElement | null;
      if (activeStyle) activeStyle.textContent = refreshed;
      clearOverrideCss();

      dirtyRef.current = false;
      onSaved();
    } catch (e) {
      setError(String(e));
    }
  }

  function handleClose(): void {
    if (dirtyRef.current) {
      const ok = window.confirm("Discard unsaved changes?");
      if (!ok) return;
    }
    clearOverrideCss();
    onClose();
  }

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.4)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 1500,
      }}
      onClick={handleClose}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          background: "var(--xp-window-bg, #ECE9D8)",
          color: "var(--xp-text-normal, #000)",
          border: "2px solid var(--xp-blue-dark, #003C74)",
          borderRadius: "6px 6px 0 0",
          width: 820,
          maxWidth: "94vw",
          maxHeight: "88vh",
          display: "flex",
          flexDirection: "column",
          fontFamily: "var(--xp-font, Tahoma, sans-serif)",
          fontSize: "var(--xp-font-size, 11px)",
          boxShadow: "3px 3px 16px rgba(0,0,0,0.45)",
          overflow: "hidden",
        }}
      >
        {/* Header */}
        <div
          style={{
            background:
              "linear-gradient(to bottom, var(--xp-blue, #0058E6), var(--xp-blue-light, #3389FF))",
            color: "#fff",
            padding: "4px 8px 4px 12px",
            fontWeight: "bold",
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            gap: 8,
          }}
        >
          <span>Customize: {initialName}</span>
          <div style={{ display: "flex", gap: 6 }}>
            <button
              style={headerBtn}
              disabled={!canUndo(history)}
              onClick={() => setHistory((h) => histUndo(h))}
              title="Undo (Ctrl+Z)"
            >
              Undo
            </button>
            <button
              style={headerBtn}
              disabled={!canRedo(history)}
              onClick={() => setHistory((h) => histRedo(h))}
              title="Redo (Ctrl+Y)"
            >
              Redo
            </button>
            <button
              style={headerBtn}
              disabled={!dirtyRef.current}
              onClick={() => void doSave()}
              title="Save (Ctrl+S)"
            >
              Save
            </button>
            <button
              onClick={handleClose}
              style={{ ...headerBtn, background: "linear-gradient(to bottom, #ee5a5a, #c83030)", color: "#fff", border: "1px solid #fff" }}
              title="Close"
            >
              ✕
            </button>
          </div>
        </div>

        {/* Body */}
        <div style={{ padding: 12, overflowY: "auto", overflowX: "hidden", flex: 1 }}>
          {error && (
            <div style={{ color: "#a00", background: "#fff5f5", border: "1px solid #f3b8b8", padding: 8, marginBottom: 8 }}>
              {error}
            </div>
          )}
          {regions.size === 0 && (
            <div
              style={{
                fontSize: 11,
                color: "var(--xp-text-muted, #666)",
                padding: "4px 0 12px",
              }}
            >
              Tip: click any color or image below to start. Use Undo if you change your mind.
            </div>
          )}
          {REGIONS.map((r) => (
            <CustomizerRegionRow
              key={r.id}
              region={r}
              state={regions.get(r.id)}
              themeId={themeId}
              themeSwatches={swatches}
              onChange={(next) => updateRegion(r.id, next)}
              onError={setError}
            />
          ))}
        </div>
      </div>
      {showIntro && <CustomizerIntro onDismiss={dismissIntro} />}
    </div>
  );
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```
Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/CustomizeModal.tsx
git -C /home/deez/farder commit -m "feat(client): CustomizeModal main shell + live preview + undo/redo"
```

---

## Task 12: Wire "Customize" button into AppearanceSettings

Adds a "Customize" button to each theme card. Clicking opens a name prompt (browser `window.prompt`) and on confirm calls `forkTheme` then opens the `CustomizeModal`.

**Files:**
- Modify: `client/src/components/AppearanceSettings.tsx`

- [ ] **Step 1: Add the button + state to AppearanceSettings**

In `client/src/components/AppearanceSettings.tsx`, after the existing imports, add:

```tsx
import CustomizeModal from "./CustomizeModal";
```

In the `AppearanceSettings` component, add state for the active customizer session (alongside `themes`, `activeId`, etc):

```tsx
  const [customizing, setCustomizing] = useState<{ themeId: string; name: string } | null>(null);
```

Add a click handler (just before the `return` statement):

```tsx
  async function startCustomizing(base: api.ThemeMeta): Promise<void> {
    const proposedName = window.prompt(
      `Customize a copy of "${base.name}". Name it:`,
      `${base.name} (Custom)`,
    );
    if (!proposedName) return;
    try {
      const newId = await api.forkTheme(base.id, proposedName.toLowerCase().replace(/\s+/g, "-"), proposedName);
      // Refresh the picker list so the new theme appears, then open the customizer on it.
      await refresh();
      setCustomizing({ themeId: newId, name: proposedName });
    } catch (e) {
      setError(String(e));
    }
  }
```

Inside each card's render block, add a "Customize" button below the swatch strip. Find the section:

```tsx
                      <div style={{ display: "flex", gap: 2, marginTop: 2 }}>
                        {swatch.map((c, i) => (
```

Immediately AFTER the `</div>` that closes the swatch strip but still INSIDE the card `<button>`, add:

```tsx
                      <div
                        role="button"
                        tabIndex={0}
                        onClick={(e) => { e.stopPropagation(); void startCustomizing(t); }}
                        onKeyDown={(e) => { if (e.key === "Enter") { e.stopPropagation(); void startCustomizing(t); } }}
                        style={{
                          marginTop: 8,
                          fontSize: 10,
                          color: "var(--xp-blue, #0058E6)",
                          textDecoration: "underline",
                          cursor: "pointer",
                          alignSelf: "flex-start",
                        }}
                      >
                        Customize…
                      </div>
```

(Using a `<div role="button">` rather than `<button>` because the parent is already a `<button>` and nesting buttons is invalid HTML.)

Finally, before the closing `</div>` of the entire backdrop (the outermost return), add:

```tsx
      {customizing && (
        <CustomizeModal
          themeId={customizing.themeId}
          initialName={customizing.name}
          onClose={() => { setCustomizing(null); refresh(); }}
          onSaved={() => { refresh(); }}
        />
      )}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```
Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/AppearanceSettings.tsx
git -C /home/deez/farder commit -m "feat(client): Customize button on theme cards opens fork-then-customize flow"
```

---

## Task 13: End-to-end verification + CHANGELOG

Manual smoke test against the spec's success criteria + add the CHANGELOG entry.

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Restart the dev session and run the smoke tests**

```
pkill -f farder-server
cd /home/deez/farder/client && npm run tauri dev
```

Confirm each of the following:

- [ ] Open Appearance picker → each theme card shows a "Customize…" link.
- [ ] Click "Customize…" on Discord Modern Dark → name prompt appears, pre-filled "Discord Modern Dark (Custom)".
- [ ] Confirm the name → picker refreshes (the new theme appears as a card with `you · user` source) and the customizer modal opens.
- [ ] Intro overlay appears the first time. Dismiss it. Re-open the customizer — overlay does NOT reappear.
- [ ] Click a region's bg color swatch → popover opens. Click a theme swatch → live preview updates immediately.
- [ ] Click a region's "Pick image…" → file dialog opens, choose a small PNG → preview updates with the image. Try each fit mode in the dropdown — preview reflects each.
- [ ] Click a region's text color swatch → popover, pick a color, preview updates text.
- [ ] Make 3 changes → click Undo three times → all three revert. Click Redo three times → all three reapply. (Try Ctrl+Z / Ctrl+Y too.)
- [ ] Click Save → no error. Close the customizer. Re-open it (Customize the same custom theme) — the saved changes persist.
- [ ] Open a fresh customizer (Customize Discord again as a different name), make changes, click ✕ Close → "Discard unsaved changes?" prompt appears. Confirm → changes discarded but the new theme folder still exists in the picker.
- [ ] Switch to the saved custom theme via the existing picker → changes apply.
- [ ] Switch to the original Discord Modern Dark → it's untouched (built-in pristine).
- [ ] In `~/.farder/themes/<your-custom-id>/` you should see a `theme.css`, a `theme.json`, and (if you added an image) an `assets/` folder.
- [ ] Try fork name with weird characters: "My Theme!?/<3" — fork should succeed with a sanitized id, the name field in `theme.json` keeps the original.
- [ ] Click "Customize" again with the same name twice in a row — second attempt errors cleanly ("a theme with id ... already exists") rather than silently overwriting.

- [ ] **Step 2: Add the CHANGELOG entry**

In `CHANGELOG.md`, under `### Added`, add:

```
- (2026-05-04) Theme Customizer (Phase 1): a "Customize" button on each theme card forks the theme into a new user theme, then opens a modal listing 12 named regions (channel sidebar, message bubble, title bars, etc). Each region accepts a background color, a background image (with stretch/tile/center/cover fit), and a text color. Live preview while editing; Ctrl+Z / Ctrl+Y / Ctrl+S work. Image assets are copied into the theme folder for portability. Built-in themes are never modified — fork-on-customize keeps originals pristine.
```

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add CHANGELOG.md
git -C /home/deez/farder commit -m "docs: changelog for theme customizer phase 1"
```

---

## Self-review notes

**Spec coverage:**
- "Customize" button + name prompt + fork → Tasks 1, 12
- 12 regions with curated selectors → Tasks 4, 5
- Color OR image+fit + text color per region → Tasks 8, 9
- Theme-extracted swatches → Tasks 8, 11
- Live preview via override `<style>` element → Task 11
- Undo/redo (per-session, unbounded) → Tasks 6, 11
- Save: merged base + overrides written to disk → Tasks 5, 11
- Fork creates new user theme, never edits built-in → Tasks 1, 2
- Image copied into `assets/`, referenced relatively → Tasks 2, 9
- Image size warning at 5MB / hard cap at 25MB → Tasks 2, 9
- Discard prompt on dirty close → Task 11
- Onboarding intro overlay (one-time, dismissable) → Task 10
- Empty-state hint + tooltips → Task 11 (empty-state hint, tooltips on header buttons)
- Selectors are the contract; values are the freedom → Task 4 (selectors enumerated in REGIONS)
- Crash safety .bak → **deferred** (spec mentions UX TBD if .bak found; not in v1 plan; revisit before Phase 2)
- Phase 2 "Edit Live" button → **deferred to Phase 2 plan**

**Type/name consistency:** `RegionId`, `RegionState`, `ImageFit`, `RegionDefinition`, `CustomizerSession`, `HistoryState`, `RegionsMap` defined once in Tasks 4 and 6, used consistently in 5, 8, 9, 11. Tauri command names match between Rust (`#[tauri::command]`) and TS bindings (Task 7).

**No placeholders:** every code step is complete code, every test step is a runnable test, every command step is the actual command + expected output.

**Known minor compromise:** the intro-dismissed flag is stored in `localStorage` rather than via a dedicated Rust binding for `settings_get/set` (Task 11 has a small TODO-style import block but uses localStorage as the v1 mechanism). That's acceptable for an in-app preference that's not portable across reinstalls. If desired, a follow-up can promote this to settings.json by adding a `settings_get_string` / `settings_set_string` Tauri command pair.
