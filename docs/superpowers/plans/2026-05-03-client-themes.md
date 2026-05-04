# Client Themes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a user-loadable CSS theme system for the Farder client, with three built-in themes (XP Luna Blue, Discord Modern Dark, Hello Kitty Pink) and runtime hot-swapping via a settings picker.

**Architecture:** Each theme is a folder with `theme.css` + `theme.json`. Built-ins are compiled into the Rust binary via `include_str!`; user themes are scanned from `~/.farder/themes/`. The active theme's CSS is injected into a single `<style id="active-theme">` element before React mounts (no flash); switching themes atomically replaces that element's `textContent`. The picker is a modal launched from the user-footer area in the channel sidebar.

**Tech Stack:** Rust (Tauri v2), React + TypeScript, plain CSS, `tauri-plugin-shell` for opening the user folder.

**Spec:** `docs/superpowers/specs/2026-05-03-client-themes-design.md`

---

## File structure

**Rust (`client/src-tauri/src/`):**
- Create: `themes.rs` — built-in registry, user folder scan, all theme commands
- Modify: `commands.rs` — refactor `save_last_server` / `get_last_server` to read-modify-write `settings.json`
- Modify: `main.rs` — register theme commands; pre-init the theme folder

**TypeScript (`client/src/`):**
- Move: `styles/xp-theme.css` → `themes/xp-luna-blue/theme.css` (verbatim)
- Create: `themes/xp-luna-blue/theme.json`
- Create: `themes/discord-dark/theme.css` and `theme.json`
- Create: `themes/hello-kitty/theme.css` and `theme.json`
- Modify: `main.tsx` — drop CSS import; await `get_active_theme` and inject `<style>` before mount
- Modify: `lib/tauri-bridge.ts` — bindings for new commands
- Create: `components/AppearanceSettings.tsx` — picker modal
- Modify: `components/ChannelSidebar.tsx` — gear button next to existing `N` button in `UserFooter`

---

## Task 1: Refactor settings.json to be multi-key safe

**Why first:** the existing `save_last_server` overwrites the entire `settings.json` with `{"address": ...}`. If we add a `theme` key without fixing this, the next reconnect will wipe the theme preference. Fix the foundation before building on it.

**Files:**
- Modify: `client/src-tauri/src/commands.rs:225-242`

- [ ] **Step 1: Add read-modify-write helpers**

In `client/src-tauri/src/commands.rs`, replace the entire `save_last_server` / `get_last_server` block with:

```rust
// ---------------------------------------------------------------------------
// Settings commands
// ---------------------------------------------------------------------------

fn read_settings() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_settings(map: serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let value = serde_json::Value::Object(map);
    std::fs::write(settings_path(), value.to_string()).map_err(|e| e.to_string())
}

pub(crate) fn settings_get(key: &str) -> Option<serde_json::Value> {
    read_settings().get(key).cloned()
}

pub(crate) fn settings_set(key: &str, value: serde_json::Value) -> Result<(), String> {
    let mut map = read_settings();
    map.insert(key.to_string(), value);
    write_settings(map)
}

#[tauri::command]
pub fn save_last_server(address: String) -> Result<(), String> {
    settings_set("address", serde_json::Value::String(address))
}

#[tauri::command]
pub fn get_last_server() -> Option<String> {
    settings_get("address").and_then(|v| v.as_str().map(|s| s.to_string()))
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd client/src-tauri && cargo check`
Expected: `Finished` with no errors. The pre-existing `endpoint` dead-code warning is fine.

- [ ] **Step 3: Commit**

```bash
git add client/src-tauri/src/commands.rs
git commit -m "refactor(client): make settings.json multi-key safe via read-modify-write helpers"
```

---

## Task 2: Move XP theme into the new themes folder

This moves the existing stylesheet to its new home, untouched. We keep the import working until Task 6 swaps it out.

**Files:**
- Move: `client/src/styles/xp-theme.css` → `client/src/themes/xp-luna-blue/theme.css`
- Create: `client/src/themes/xp-luna-blue/theme.json`
- Modify: `client/src/main.tsx` (one-line import path change, temporary)

- [ ] **Step 1: Create the new directory and move the file**

```bash
mkdir -p client/src/themes/xp-luna-blue
git mv client/src/styles/xp-theme.css client/src/themes/xp-luna-blue/theme.css
```

- [ ] **Step 2: Create the metadata file**

`client/src/themes/xp-luna-blue/theme.json`:

```json
{
  "id": "xp-luna-blue",
  "name": "Windows XP — Luna Blue",
  "author": "Farder",
  "description": "The classic XP look. Blue title bars, Tahoma, Bliss-era window chrome."
}
```

- [ ] **Step 3: Update the import in main.tsx**

In `client/src/main.tsx`, change:

```ts
import "./styles/xp-theme.css";
```

to:

```ts
import "./themes/xp-luna-blue/theme.css";
```

(This is a temporary one-line move — Task 6 removes this import entirely and switches to runtime injection.)

- [ ] **Step 4: Verify the app still builds and looks identical**

Run: `cd client && pnpm tauri dev`
Expected: app launches and looks pixel-identical to before.
Stop the dev server with Ctrl+C after confirming.

- [ ] **Step 5: Commit**

```bash
git add client/src/themes/ client/src/main.tsx
git commit -m "refactor(client): move xp-theme.css into themes/xp-luna-blue/ with metadata"
```

---

## Task 3: Create the themes Rust module skeleton

Sets up `themes.rs` with types, the built-in registry (just XP for now), and stub commands. No frontend changes yet.

**Files:**
- Create: `client/src-tauri/src/themes.rs`
- Modify: `client/src-tauri/src/main.rs` — declare module and register all 5 commands

- [ ] **Step 1: Create themes.rs**

`client/src-tauri/src/themes.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const DEFAULT_THEME_ID: &str = "xp-luna-blue";

// Built-in themes: CSS + metadata embedded at compile time.
// Add a new built-in by creating client/src/themes/<id>/{theme.css,theme.json}
// and adding an entry to BUILTIN_THEMES below.
struct BuiltinTheme {
    id: &'static str,
    css: &'static str,
    meta_json: &'static str,
}

const BUILTIN_THEMES: &[BuiltinTheme] = &[
    BuiltinTheme {
        id: "xp-luna-blue",
        css: include_str!("../../src/themes/xp-luna-blue/theme.css"),
        meta_json: include_str!("../../src/themes/xp-luna-blue/theme.json"),
    },
];

#[derive(Serialize, Deserialize, Clone)]
pub struct ThemeMeta {
    pub id: String,
    pub name: String,
    pub author: String,
    pub description: String,
    pub source: String, // "builtin" | "user"
}

#[derive(Deserialize)]
struct RawMeta {
    id: String,
    name: String,
    author: String,
    description: String,
}

fn parse_meta(raw_json: &str, source: &str) -> Option<ThemeMeta> {
    let raw: RawMeta = serde_json::from_str(raw_json).ok()?;
    Some(ThemeMeta {
        id: raw.id,
        name: raw.name,
        author: raw.author,
        description: raw.description,
        source: source.to_string(),
    })
}

fn user_themes_dir() -> PathBuf {
    crate::commands::farder_data_dir_pub().join("themes")
}

fn ensure_user_themes_dir() {
    let _ = std::fs::create_dir_all(user_themes_dir());
}

fn scan_user_themes() -> Vec<(ThemeMeta, String)> {
    ensure_user_themes_dir();
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(user_themes_dir()) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let css_path = path.join("theme.css");
        let meta_path = path.join("theme.json");
        let css = match std::fs::read_to_string(&css_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let raw = match std::fs::read_to_string(&meta_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Some(meta) = parse_meta(&raw, "user") {
            out.push((meta, css));
        } else {
            eprintln!("[themes] skipping {:?}: invalid theme.json", path);
        }
    }
    out
}

fn all_themes() -> Vec<(ThemeMeta, String)> {
    // User themes win on id collision — start with built-ins, then user themes overwrite by id.
    let mut by_id: std::collections::HashMap<String, (ThemeMeta, String)> =
        std::collections::HashMap::new();
    for b in BUILTIN_THEMES {
        if let Some(meta) = parse_meta(b.meta_json, "builtin") {
            by_id.insert(meta.id.clone(), (meta, b.css.to_string()));
        }
    }
    for (meta, css) in scan_user_themes() {
        by_id.insert(meta.id.clone(), (meta, css));
    }
    let mut out: Vec<_> = by_id.into_values().collect();
    out.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    out
}

#[tauri::command]
pub fn list_themes() -> Vec<ThemeMeta> {
    all_themes().into_iter().map(|(m, _)| m).collect()
}

#[tauri::command]
pub fn load_theme_css(id: String) -> Result<String, String> {
    all_themes()
        .into_iter()
        .find(|(m, _)| m.id == id)
        .map(|(_, css)| css)
        .ok_or_else(|| format!("theme not found: {}", id))
}

#[derive(Serialize)]
pub struct ActiveTheme {
    pub id: String,
    pub css: String,
}

#[tauri::command]
pub fn get_active_theme() -> Result<ActiveTheme, String> {
    let saved_id = crate::commands::settings_get("theme")
        .and_then(|v| v.as_str().map(|s| s.to_string()));

    // Try the saved id first; fall back to default if missing or unresolvable.
    let themes = all_themes();
    let chosen = saved_id
        .as_deref()
        .and_then(|id| themes.iter().find(|(m, _)| m.id == id))
        .or_else(|| themes.iter().find(|(m, _)| m.id == DEFAULT_THEME_ID))
        .ok_or_else(|| "no themes available".to_string())?;

    Ok(ActiveTheme {
        id: chosen.0.id.clone(),
        css: chosen.1.clone(),
    })
}

#[tauri::command]
pub fn set_active_theme(id: String) -> Result<(), String> {
    crate::commands::settings_set("theme", serde_json::Value::String(id))
}

#[tauri::command]
pub fn open_themes_folder(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_shell::ShellExt;
    ensure_user_themes_dir();
    let path = user_themes_dir();
    app.shell()
        .open(path.to_string_lossy().to_string(), None)
        .map_err(|e| e.to_string())
}
```

- [ ] **Step 2: Expose `farder_data_dir` for the new module**

The existing `farder_data_dir` in `commands.rs:18` is private. Add a public re-export at the bottom of `commands.rs`:

```rust
// Public re-export so other modules can resolve paths under ~/.farder/.
pub fn farder_data_dir_pub() -> std::path::PathBuf {
    farder_data_dir()
}
```

(We expose a wrapper instead of changing `farder_data_dir`'s visibility to keep the change surgical.)

- [ ] **Step 3: Register the module and commands in main.rs**

In `client/src-tauri/src/main.rs`, after the existing `mod tray;` line, add:

```rust
mod themes;
```

And in the `tauri::generate_handler![...]` list (after `commands::restart_local_servers,`), add:

```rust
            themes::list_themes,
            themes::load_theme_css,
            themes::get_active_theme,
            themes::set_active_theme,
            themes::open_themes_folder,
```

- [ ] **Step 4: Verify it compiles**

Run: `cd client/src-tauri && cargo check`
Expected: `Finished` with no errors. (Pre-existing `endpoint` dead-code warning is fine.)

- [ ] **Step 5: Commit**

```bash
git add client/src-tauri/src/themes.rs client/src-tauri/src/main.rs client/src-tauri/src/commands.rs
git commit -m "feat(client): theme module with built-in registry and Tauri commands"
```

---

## Task 4: Add a unit test for `parse_meta` and `all_themes`

Catch malformed `theme.json` files and verify the user-overrides-builtin behavior.

**Files:**
- Modify: `client/src-tauri/src/themes.rs` — add `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing tests**

Append to `client/src-tauri/src/themes.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_meta_accepts_valid_json() {
        let raw = r#"{"id":"x","name":"X","author":"A","description":"D"}"#;
        let meta = parse_meta(raw, "builtin").expect("should parse");
        assert_eq!(meta.id, "x");
        assert_eq!(meta.name, "X");
        assert_eq!(meta.source, "builtin");
    }

    #[test]
    fn parse_meta_rejects_invalid_json() {
        assert!(parse_meta("not json", "user").is_none());
        assert!(parse_meta(r#"{"id":"x"}"#, "user").is_none()); // missing fields
    }

    #[test]
    fn builtin_xp_luna_blue_is_registered() {
        let themes = list_themes();
        assert!(
            themes.iter().any(|t| t.id == "xp-luna-blue"),
            "xp-luna-blue must be in the built-in registry"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cd client/src-tauri && cargo test --lib themes::tests`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add client/src-tauri/src/themes.rs
git commit -m "test(client): unit tests for theme metadata parsing and registry"
```

---

## Task 5: Add TypeScript bindings to tauri-bridge.ts

**Files:**
- Modify: `client/src/lib/tauri-bridge.ts`

- [ ] **Step 1: Add the bindings**

Append to `client/src/lib/tauri-bridge.ts`:

```ts
// ---------------------------------------------------------------------------
// Themes
// ---------------------------------------------------------------------------

export interface ThemeMeta {
  id: string;
  name: string;
  author: string;
  description: string;
  source: "builtin" | "user";
}

export interface ActiveTheme {
  id: string;
  css: string;
}

export async function listThemes(): Promise<ThemeMeta[]> {
  return invoke<ThemeMeta[]>("list_themes");
}

export async function loadThemeCss(id: string): Promise<string> {
  return invoke<string>("load_theme_css", { id });
}

export async function getActiveTheme(): Promise<ActiveTheme> {
  return invoke<ActiveTheme>("get_active_theme");
}

export async function setActiveTheme(id: string): Promise<void> {
  return invoke<void>("set_active_theme", { id });
}

export async function openThemesFolder(): Promise<void> {
  return invoke<void>("open_themes_folder");
}
```

- [ ] **Step 2: Verify TypeScript compiles**

Run: `cd client && pnpm tsc --noEmit`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add client/src/lib/tauri-bridge.ts
git commit -m "feat(client): tauri-bridge bindings for theme commands"
```

---

## Task 6: Pre-mount theme injection in main.tsx

Replace the static CSS import with runtime injection so the active theme is applied before React renders.

**Files:**
- Modify: `client/src/main.tsx`

- [ ] **Step 1: Replace main.tsx**

Replace the entire contents of `client/src/main.tsx` with:

```tsx
import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { getActiveTheme } from "./lib/tauri-bridge";

async function bootstrap() {
  // Inject the active theme's CSS before React mounts so there's no
  // flash of default styling. If the IPC fails (shouldn't happen in
  // production), we still render — an unstyled app is better than a
  // blank window.
  try {
    const { id, css } = await getActiveTheme();
    const style = document.createElement("style");
    style.id = "active-theme";
    style.textContent = css;
    document.head.appendChild(style);
    document.documentElement.dataset.theme = id;
  } catch (e) {
    console.error("[bootstrap] failed to load theme:", e);
  }

  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

bootstrap();
```

- [ ] **Step 2: Verify the app launches and looks the same**

Run: `cd client && pnpm tauri dev`
Expected:
- App launches.
- Visually identical to Task 2's result (XP Luna Blue).
- Browser DevTools → Elements: a `<style id="active-theme">` element exists in `<head>` with the XP CSS as text.
- `<html>` has `data-theme="xp-luna-blue"`.
- No `[bootstrap]` errors in the console.

Stop the dev server with Ctrl+C.

- [ ] **Step 3: Commit**

```bash
git add client/src/main.tsx
git commit -m "feat(client): inject active theme CSS before React mount"
```

---

## Task 7: Build the AppearanceSettings picker modal

The picker is a single self-contained component. Uses inline styles for its own chrome (so it works against any theme), but pulls accent colors from CSS variables when available.

**Files:**
- Create: `client/src/components/AppearanceSettings.tsx`

- [ ] **Step 1: Create the component**

`client/src/components/AppearanceSettings.tsx`:

```tsx
import { useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";

interface Props {
  onClose: () => void;
}

// Pull a few representative colors out of a theme's CSS for the swatch strip.
// Looks for `--<prefix>-bg`, `--<prefix>-blue`, `--<prefix>-accent`, etc, and
// returns up to 5 distinct color values in declaration order.
function extractSwatch(css: string): string[] {
  const colors: string[] = [];
  const seen = new Set<string>();
  const re = /--[\w-]+:\s*(#[0-9a-fA-F]{3,8}|rgb[a]?\([^)]+\)|hsl[a]?\([^)]+\))\s*;/g;
  let match: RegExpExecArray | null;
  while ((match = re.exec(css)) !== null && colors.length < 5) {
    const c = match[1].trim();
    if (!seen.has(c)) {
      seen.add(c);
      colors.push(c);
    }
  }
  return colors;
}

export default function AppearanceSettings({ onClose }: Props) {
  const [themes, setThemes] = useState<api.ThemeMeta[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [swatchByCss, setSwatchByCss] = useState<Record<string, string[]>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    setLoading(true);
    setError(null);
    try {
      const list = await api.listThemes();
      setThemes(list);
      const active = await api.getActiveTheme();
      setActiveId(active.id);
      // Load CSS for each to compute swatches. Cheap — already in memory on Rust side.
      const swatches: Record<string, string[]> = {};
      for (const t of list) {
        try {
          const css = await api.loadThemeCss(t.id);
          swatches[t.id] = extractSwatch(css);
        } catch {
          swatches[t.id] = [];
        }
      }
      setSwatchByCss(swatches);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    refresh();
  }, []);

  async function selectTheme(id: string) {
    try {
      const css = await api.loadThemeCss(id);
      const styleEl = document.getElementById("active-theme");
      if (styleEl) styleEl.textContent = css;
      document.documentElement.dataset.theme = id;
      await api.setActiveTheme(id);
      setActiveId(id);
    } catch (e) {
      console.error("[appearance] failed to switch theme:", e);
      setError(String(e));
    }
  }

  return (
    <div
      className="appearance-settings-backdrop"
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.4)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 1000,
      }}
      onClick={onClose}
    >
      <div
        className="appearance-settings"
        style={{
          background: "var(--xp-window-bg, #ECE9D8)",
          color: "#000",
          border: "2px solid var(--xp-blue-dark, #003C74)",
          borderRadius: 6,
          width: 560,
          maxHeight: "80vh",
          display: "flex",
          flexDirection: "column",
          fontFamily: "var(--xp-font, Tahoma, sans-serif)",
          fontSize: "var(--xp-font-size, 11px)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          style={{
            background: "linear-gradient(to right, var(--xp-blue, #0058E6), var(--xp-blue-light, #3389FF))",
            color: "#fff",
            padding: "4px 8px",
            fontWeight: "bold",
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
          }}
        >
          <span>Appearance</span>
          <button
            onClick={onClose}
            style={{ background: "transparent", color: "#fff", border: "1px solid #fff", padding: "0 6px", cursor: "pointer" }}
            title="Close"
          >
            ✕
          </button>
        </div>

        <div style={{ padding: 12, overflow: "auto", flex: 1 }}>
          {loading && <div>Loading themes…</div>}
          {error && <div style={{ color: "#a00" }}>Error: {error}</div>}
          {!loading && !error && (
            <>
              <div
                style={{
                  display: "grid",
                  gridTemplateColumns: "repeat(auto-fill, minmax(220px, 1fr))",
                  gap: 10,
                }}
              >
                {themes.map((t) => {
                  const isActive = t.id === activeId;
                  const swatch = swatchByCss[t.id] ?? [];
                  return (
                    <button
                      key={t.id}
                      onClick={() => selectTheme(t.id)}
                      style={{
                        textAlign: "left",
                        padding: 10,
                        border: isActive ? "2px solid var(--xp-blue, #0058E6)" : "1px solid #aca899",
                        background: "#fff",
                        cursor: "pointer",
                        display: "flex",
                        flexDirection: "column",
                        gap: 6,
                      }}
                    >
                      <div style={{ fontWeight: "bold" }}>{t.name}</div>
                      <div style={{ fontSize: 10, color: "#555" }}>
                        {t.author} · {t.source}
                      </div>
                      <div style={{ fontSize: 10, color: "#555" }}>{t.description}</div>
                      <div style={{ display: "flex", gap: 2, marginTop: 4 }}>
                        {swatch.map((c, i) => (
                          <div
                            key={i}
                            style={{
                              width: 24,
                              height: 16,
                              background: c,
                              border: "1px solid #888",
                            }}
                          />
                        ))}
                      </div>
                    </button>
                  );
                })}
              </div>

              <div
                style={{
                  marginTop: 14,
                  display: "flex",
                  gap: 8,
                  alignItems: "center",
                  flexWrap: "wrap",
                }}
              >
                <button onClick={() => api.openThemesFolder().catch((e) => setError(String(e)))}>
                  Open themes folder
                </button>
                <button onClick={refresh} title="Re-scan ~/.farder/themes/">
                  Refresh
                </button>
                <span style={{ fontSize: 10, color: "#666", flex: 1, minWidth: 200 }}>
                  Themes can load external resources. Only use themes from sources you trust.
                </span>
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify TypeScript compiles**

Run: `cd client && pnpm tsc --noEmit`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add client/src/components/AppearanceSettings.tsx
git commit -m "feat(client): AppearanceSettings picker modal with swatch preview"
```

---

## Task 8: Wire the gear button into the user footer

Add a gear icon next to the existing `N` (Notification Settings) button in `UserFooter`. Opens the AppearanceSettings modal.

**Files:**
- Modify: `client/src/components/ChannelSidebar.tsx:11` (add import) and `:13-57` (add state + button + render)

- [ ] **Step 1: Add the AppearanceSettings import**

In `client/src/components/ChannelSidebar.tsx`, add to the existing imports near line 11:

```tsx
import AppearanceSettings from "./AppearanceSettings";
```

- [ ] **Step 2: Add state and the gear button to UserFooter**

In the `UserFooter` function (currently lines 13-57), make these changes:

Add a new state hook alongside the existing `showNotifSettings`:

```tsx
  const [showAppearance, setShowAppearance] = useState(false);
```

In the JSX, just before the existing `<button ...>N</button>` element, insert a sibling button:

```tsx
        <button
          className="server-invite-btn"
          onClick={() => setShowAppearance(true)}
          title="Appearance"
          style={{ fontSize: 10, marginRight: 4 }}
        >⚙</button>
```

Then, alongside the existing conditional render `{showNotifSettings && <NotificationSettings ... />}`, add:

```tsx
      {showAppearance && <AppearanceSettings onClose={() => setShowAppearance(false)} />}
```

- [ ] **Step 3: Verify it builds and the picker opens**

Run: `cd client && pnpm tauri dev`
Expected:
- App launches.
- Bottom-left of channel sidebar shows: user name · ⚙ button · N button.
- Click ⚙ → AppearanceSettings modal opens with one card: "Windows XP — Luna Blue".
- Card shows author, description, and a swatch strip of XP colors.
- Card has the active border (it's the only theme).
- "Open themes folder" button opens `~/.farder/themes/` in your file manager.
- "Refresh" button re-runs the scan (no visible change yet — no user themes installed).
- Click ✕ or backdrop → modal closes.

Stop the dev server with Ctrl+C.

- [ ] **Step 4: Commit**

```bash
git add client/src/components/ChannelSidebar.tsx
git commit -m "feat(client): gear button in user footer launches Appearance settings"
```

---

## Task 9: Author the Discord Modern Dark theme

A new full-replacement stylesheet authored to match Discord's dark mode aesthetic, using the same selectors as `xp-luna-blue/theme.css` (which IS the public class-name contract).

**Authoring workflow:**

1. Copy `client/src/themes/xp-luna-blue/theme.css` to `client/src/themes/discord-dark/theme.css`.
2. Replace the `:root` variable block with Discord-flavored values (see palette below).
3. Walk through the file top-to-bottom, replacing color/border/background values to match the dark aesthetic. Layout and selectors stay the same — only visual properties change.
4. Verify by switching to it in the picker and visually inspecting every screen (channels, chat, members, modals, scrollbars).

**Discord-dark palette (approximate, adjust to taste):**

```
--bg-primary:      #313338   (main chat background)
--bg-secondary:    #2B2D31   (channel sidebar background)
--bg-tertiary:     #1E1F22   (server strip / deeper chrome)
--bg-floating:     #232428   (popovers, dropdowns)
--text-normal:     #DBDEE1
--text-muted:      #949BA4
--text-link:       #00A8FC
--interactive:     #B5BAC1   (icons in their default state)
--interactive-hover: #DBDEE1
--brand:           #5865F2   (Discord blurple — accents, primary buttons)
--brand-hover:     #4752C4
--border:          #1E1F22
--scrollbar-thumb: #1A1B1E
--scrollbar-track: #2B2D31
--mention:         #5865F2
font-family:       "gg sans", "Helvetica Neue", Helvetica, Arial, sans-serif
font-size:         14px
```

(These can be tuned during authoring. The point is: dark, dense, blurple-accented.)

**Files:**
- Create: `client/src/themes/discord-dark/theme.css`
- Create: `client/src/themes/discord-dark/theme.json`
- Modify: `client/src-tauri/src/themes.rs` — register the new built-in

- [ ] **Step 1: Seed the new theme.css from XP**

```bash
mkdir -p client/src/themes/discord-dark
cp client/src/themes/xp-luna-blue/theme.css client/src/themes/discord-dark/theme.css
```

- [ ] **Step 2: Replace the `:root` variable block**

Open `client/src/themes/discord-dark/theme.css`, replace the existing `:root { ... }` block at the top with the Discord palette above (using the new variable names). Keep the rest of the file untouched for now.

- [ ] **Step 3: Walk the file, swap visual properties**

Search-and-replace through the rest of the file:
- All `var(--xp-blue)` and similar → corresponding Discord variable.
- Hardcoded color literals (`#fff`, `#000`, `#aca899`, etc) → appropriate dark-mode values.
- `font-family: var(--xp-font, Tahoma, sans-serif)` → Discord's font stack.
- `border-radius: 6px` style XP chrome → flatter Discord chrome (`border-radius: 4px` or 0).
- Keep all class selectors as-is.

This is real authoring work — budget an hour, not 5 minutes. Stay focused: **selectors are the contract; visual properties are the freedom.**

- [ ] **Step 4: Create theme.json**

`client/src/themes/discord-dark/theme.json`:

```json
{
  "id": "discord-dark",
  "name": "Discord Modern Dark",
  "author": "Farder",
  "description": "Dark, dense, blurple. The familiar Discord look."
}
```

- [ ] **Step 5: Register in the built-in array**

In `client/src-tauri/src/themes.rs`, extend `BUILTIN_THEMES`:

```rust
const BUILTIN_THEMES: &[BuiltinTheme] = &[
    BuiltinTheme {
        id: "xp-luna-blue",
        css: include_str!("../../src/themes/xp-luna-blue/theme.css"),
        meta_json: include_str!("../../src/themes/xp-luna-blue/theme.json"),
    },
    BuiltinTheme {
        id: "discord-dark",
        css: include_str!("../../src/themes/discord-dark/theme.css"),
        meta_json: include_str!("../../src/themes/discord-dark/theme.json"),
    },
];
```

- [ ] **Step 6: Verify and visually inspect**

Run: `cd client && pnpm tauri dev`
Expected:
- Picker now shows two cards: XP Luna Blue and Discord Modern Dark.
- Click Discord Modern Dark → entire app re-styles instantly.
- Walk through each screen: channel sidebar, chat, members, message hover, reaction picker, modals, context menu. Nothing should be unreadable (white text on white bg, etc).
- Switch back to XP Luna Blue. Switches back cleanly.
- Close and reopen the app. The last-selected theme persists.

Fix any unreadable areas by editing the relevant selectors in `discord-dark/theme.css`. Iterate.

- [ ] **Step 7: Commit**

```bash
git add client/src/themes/discord-dark/ client/src-tauri/src/themes.rs
git commit -m "feat(client): Discord Modern Dark theme"
```

---

## Task 10: Author the Hello Kitty Pink theme

Same workflow as Task 9. New full stylesheet, same selectors, pink-and-white kawaii aesthetic.

**Hello Kitty palette (approximate):**

```
--bg-primary:      #FFF5F8   (warm off-white main background)
--bg-secondary:    #FFE4ED   (sidebar pink wash)
--bg-tertiary:     #FFC2D4   (accents, deeper chrome)
--bg-floating:     #FFFFFF
--text-normal:     #4A2937   (deep raspberry text — readable on pink)
--text-muted:      #A87687
--text-link:       #E91E63
--accent-pink:     #FF6FAB   (Hello Kitty signature pink)
--accent-pink-dark:#E91E63
--border:          #FFB3CC
--scrollbar-thumb: #FF6FAB
--scrollbar-track: #FFE4ED
--bow-red:         #E63946   (Hello Kitty's bow — used sparingly for active/notification accents)
font-family:       "Comic Neue", "Comic Sans MS", "M PLUS Rounded 1c", sans-serif
font-size:         12px
border-radius:     12px       (rounded everything for the soft kawaii feel)
```

**Files:**
- Create: `client/src/themes/hello-kitty/theme.css`
- Create: `client/src/themes/hello-kitty/theme.json`
- Modify: `client/src-tauri/src/themes.rs` — register the new built-in

- [ ] **Step 1: Seed from XP**

```bash
mkdir -p client/src/themes/hello-kitty
cp client/src/themes/xp-luna-blue/theme.css client/src/themes/hello-kitty/theme.css
```

- [ ] **Step 2: Replace `:root` and walk the file**

Same process as Task 9: swap variables to the Hello Kitty palette, replace hardcoded color literals, soften corners with rounded radii, swap fonts. Stay focused on visual properties; do not touch selectors.

- [ ] **Step 3: Create theme.json**

`client/src/themes/hello-kitty/theme.json`:

```json
{
  "id": "hello-kitty",
  "name": "Hello Kitty Pink",
  "author": "Farder",
  "description": "Soft pink and white, rounded, kawaii. For when the office vibes need to be gentler."
}
```

- [ ] **Step 4: Register in the built-in array**

In `client/src-tauri/src/themes.rs`, extend `BUILTIN_THEMES`:

```rust
    BuiltinTheme {
        id: "hello-kitty",
        css: include_str!("../../src/themes/hello-kitty/theme.css"),
        meta_json: include_str!("../../src/themes/hello-kitty/theme.json"),
    },
```

(Add as a third element of the array.)

- [ ] **Step 5: Verify and visually inspect**

Run: `cd client && pnpm tauri dev`
Expected:
- Picker shows three cards: XP Luna Blue, Discord Modern Dark, Hello Kitty Pink.
- Switch to Hello Kitty → app re-styles to pink/white.
- Walk every screen, fix any unreadable areas.

- [ ] **Step 6: Commit**

```bash
git add client/src/themes/hello-kitty/ client/src-tauri/src/themes.rs
git commit -m "feat(client): Hello Kitty Pink theme"
```

---

## Task 11: End-to-end verification + CHANGELOG

Final acceptance pass against the spec's success criteria.

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Verify each success criterion from the spec**

Run: `cd client && pnpm tauri dev`

Walk through and confirm:

- [ ] Three themes appear in the picker on a fresh install.
- [ ] Switching themes is visually instant (no flicker, no restart).
- [ ] Closing and reopening the app restores the chosen theme without a flash of default styling.
- [ ] Drop a tiny test theme into `~/.farder/themes/test/` (one `theme.css` with a single rule like `body { background: lime !important }` and a `theme.json` `{"id":"test","name":"Test","author":"You","description":"x"}`), click Refresh, confirm it appears in the picker and applies when selected.
- [ ] Create a `~/.farder/themes/xp-luna-blue/` folder with a tweaked CSS (e.g. red title bars), refresh, confirm the picker shows ONE XP entry (the user override) and selecting it uses the user's CSS.
- [ ] Edit `~/.farder/settings.json` to set `"theme": "nonexistent"`, restart the app, confirm it falls back to XP Luna Blue without errors.

If any criterion fails, return to the relevant earlier task to fix.

- [ ] **Step 2: Add the CHANGELOG entry**

In `CHANGELOG.md`, under `### Added`, add:

```
- (2026-05-03) User-loadable CSS themes: ships with three built-ins (Windows XP Luna Blue, Discord Modern Dark, Hello Kitty Pink) plus a folder at `~/.farder/themes/<id>/` for user-authored themes (each is a `theme.css` + `theme.json`). Switching is hot — no restart, no flash. Picker lives under the new ⚙ button in the user footer.
```

- [ ] **Step 3: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs: changelog entry for client themes feature"
```

---

## Self-review notes

**Spec coverage:**
- Folder layout, file format → Task 3
- Two theme sources (built-in + user) → Task 3 (`scan_user_themes`, `all_themes`)
- User overrides built-in by id → Task 3 (`all_themes` insertion order)
- Three built-in themes → Tasks 2, 9, 10
- All five Tauri commands → Task 3
- CSS injection (single `<style>`, atomic swap, await before mount) → Tasks 6, 7
- Picker layout + footer + warning text → Task 7
- Entry point in user pill area → Task 8
- Persistence in settings.json → Tasks 1, 3
- Fallback when saved id is missing → Task 3 (`get_active_theme`)
- Built-ins via `include_str!` → Task 3
- Move `xp-theme.css` verbatim first, verify, then change loader → Tasks 2, 6

**Type/name consistency:** `ThemeMeta`, `ActiveTheme`, `ThemeMeta.source`, `id`/`name`/`author`/`description` are referenced consistently across tasks 3, 5, 7. Tauri command names match between Rust (`#[tauri::command]`) and TS bindings.

**No placeholders:** every code step contains complete code; CSS authoring tasks (9, 10) explicitly note the creative-work scope and provide concrete palettes + workflow.
