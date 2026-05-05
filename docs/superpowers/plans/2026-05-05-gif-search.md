# GIF Search (Tenor) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tenor GIF search picker in the message input — opt-in for privacy, embedded API key with bring-your-own-key override, click-to-send via the existing fetch_url + send_message flow, hover-save to the Reaction Book. Settings panel adds a new "GIF Search" tab to the existing AppearanceSettings (renamed Settings).

**Architecture:** New Tauri Rust module `tenor.rs` proxies all Tenor v2 API calls (key never in renderer). Settings persist under `~/.farder/settings.json` (`gif_search_*` keys, multi-key-safe via existing settings helpers). Sent GIFs round-trip through the existing server-side `fetch_url` so receivers see a normal server-hosted attachment — Tenor URLs never leak to other clients. No protocol changes.

**Tech Stack:** Rust (Tauri v2 + new `reqwest` client dep), React + TypeScript. Reuses Reaction Book Phase 1's `book_save_from_url` for the hover-save action.

**Spec:** `docs/superpowers/specs/2026-05-05-gif-search-design.md`

---

## File structure

**New Rust:**
- Create: `client/src-tauri/src/tenor.rs` — Tenor API client + 4 commands + types + key resolution
- Modify: `client/src-tauri/Cargo.toml` — add `reqwest` with json + rustls-tls features
- Modify: `client/src-tauri/src/main.rs` — declare module + register 4 commands

**New TS:**
- Create: `client/src/lib/gifSearch.ts` — typed wrappers for the 4 Tenor commands + types
- Create: `client/src/components/GifSearchOptIn.tsx` — first-click privacy modal
- Create: `client/src/components/GifPicker.tsx` — search + grid + hover-save
- Create: `client/src/components/GifSearchSettings.tsx` — settings tab content

**Modified TS:**
- Modify: `client/src/components/AppearanceSettings.tsx` — add tab structure (Appearance + GIF Search), rename title to "Settings"
- Modify: `client/src/components/MessageInput.tsx` — add 🎬 button + state branching on enabled

---

## Task 1: Add `reqwest` dep to client crate

**Files:**
- Modify: `client/src-tauri/Cargo.toml`

- [ ] **Step 1: Add reqwest to dependencies**

In `client/src-tauri/Cargo.toml`, under `[dependencies]`, add:

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
```

The `rustls-tls` feature avoids depending on system OpenSSL, matching what `tauri` itself uses elsewhere in the workspace.

- [ ] **Step 2: Verify cargo can resolve**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
```

Expected: `Finished` (with the pre-existing dead_code warning only). The dep download may take a minute on first build.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src-tauri/Cargo.toml client/src-tauri/Cargo.lock
git -C /home/deez/farder commit -m "feat(client): add reqwest dep for Tenor API client"
```

---

## Task 2: Create `tenor.rs` module

The whole Tenor module: types, settings helpers, key resolution, search and trending HTTP calls.

**Files:**
- Create: `client/src-tauri/src/tenor.rs`

- [ ] **Step 1: Write the file**

`client/src-tauri/src/tenor.rs`:

```rust
use serde::{Deserialize, Serialize};

const TENOR_BASE: &str = "https://tenor.googleapis.com/v2";

// EMBEDDED DEFAULT KEY — replace with a real Google Cloud Tenor API key before
// shipping to users. For local development, generate one at
// https://developers.google.com/tenor/guides/quickstart and paste it here.
// Users can override via Settings → GIF Search → "Your Tenor API key".
const TENOR_DEFAULT_KEY: &str = "REPLACE_WITH_REAL_KEY";

#[derive(Serialize, Deserialize, Clone)]
pub struct GifSearchSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_filter")]
    pub content_filter: String,
    #[serde(default)]
    pub user_api_key: Option<String>,
}

fn default_filter() -> String { "high".to_string() }

impl Default for GifSearchSettings {
    fn default() -> Self {
        Self { enabled: false, content_filter: "high".to_string(), user_api_key: None }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TenorGif {
    pub id: String,
    pub title: String,
    pub preview_url: String,
    pub full_url: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize)]
pub struct TenorSearchResult {
    pub gifs: Vec<TenorGif>,
    pub next: Option<String>,
}

// ---------------------------------------------------------------------------
// Settings persistence (read from ~/.farder/settings.json via existing helpers)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_gif_search_settings() -> GifSearchSettings {
    let enabled = crate::commands::settings_get("gif_search_enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let content_filter = crate::commands::settings_get("gif_search_content_filter")
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(default_filter);
    let user_api_key = crate::commands::settings_get("gif_search_user_key")
        .and_then(|v| v.as_str().map(String::from))
        .filter(|s| !s.is_empty());
    GifSearchSettings { enabled, content_filter, user_api_key }
}

#[tauri::command]
pub fn set_gif_search_settings(settings: GifSearchSettings) -> Result<(), String> {
    crate::commands::settings_set(
        "gif_search_enabled",
        serde_json::Value::Bool(settings.enabled),
    )?;
    crate::commands::settings_set(
        "gif_search_content_filter",
        serde_json::Value::String(settings.content_filter),
    )?;
    crate::commands::settings_set(
        "gif_search_user_key",
        match settings.user_api_key {
            Some(k) if !k.is_empty() => serde_json::Value::String(k),
            _ => serde_json::Value::Null,
        },
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tenor API client
// ---------------------------------------------------------------------------

fn resolve_key(settings: &GifSearchSettings) -> Result<String, String> {
    if let Some(k) = &settings.user_api_key {
        if !k.trim().is_empty() {
            return Ok(k.clone());
        }
    }
    if TENOR_DEFAULT_KEY == "REPLACE_WITH_REAL_KEY" {
        return Err("No Tenor API key configured. Set one in Settings → GIF Search.".to_string());
    }
    Ok(TENOR_DEFAULT_KEY.to_string())
}

async fn call_tenor(url: &str) -> Result<serde_json::Value, String> {
    let resp = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Could not reach Tenor: {}", e))?;
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Invalid Tenor response: {}", e))?;
    if !status.is_success() {
        if status.as_u16() == 429 || status.as_u16() == 403 {
            return Err("GIF search is over quota. Try setting your own API key in Settings → GIF Search.".to_string());
        }
        if status.as_u16() == 401 {
            return Err("Your API key was rejected. Check it in Settings → GIF Search.".to_string());
        }
        return Err(format!("Tenor error {}: {}", status, body));
    }
    Ok(body)
}

fn parse_results(body: serde_json::Value) -> TenorSearchResult {
    let next = body.get("next").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from);
    let gifs = body
        .get("results")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let id = item.get("id")?.as_str()?.to_string();
                    let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let formats = item.get("media_formats")?;
                    let tinygif = formats.get("tinygif")?;
                    let preview_url = tinygif.get("url")?.as_str()?.to_string();
                    let dims = tinygif.get("dims").and_then(|v| v.as_array());
                    let (width, height) = match dims {
                        Some(d) if d.len() == 2 => (
                            d[0].as_u64().unwrap_or(0) as u32,
                            d[1].as_u64().unwrap_or(0) as u32,
                        ),
                        _ => (0, 0),
                    };
                    let full_url = formats
                        .get("gif")
                        .and_then(|v| v.get("url"))
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .unwrap_or_else(|| preview_url.clone());
                    Some(TenorGif { id, title, preview_url, full_url, width, height })
                })
                .collect()
        })
        .unwrap_or_default();
    TenorSearchResult { gifs, next }
}

#[tauri::command]
pub async fn tenor_search(query: String, pos: Option<String>) -> Result<TenorSearchResult, String> {
    let settings = get_gif_search_settings();
    if !settings.enabled {
        return Err("GIF search is not enabled".to_string());
    }
    let key = resolve_key(&settings)?;
    let mut url = format!(
        "{}/search?key={}&q={}&contentfilter={}&media_filter=tinygif,gif&limit=24",
        TENOR_BASE,
        urlencoding::encode(&key),
        urlencoding::encode(&query),
        urlencoding::encode(&settings.content_filter),
    );
    if let Some(p) = pos {
        url.push_str(&format!("&pos={}", urlencoding::encode(&p)));
    }
    let body = call_tenor(&url).await?;
    Ok(parse_results(body))
}

#[tauri::command]
pub async fn tenor_trending(pos: Option<String>) -> Result<TenorSearchResult, String> {
    let settings = get_gif_search_settings();
    if !settings.enabled {
        return Err("GIF search is not enabled".to_string());
    }
    let key = resolve_key(&settings)?;
    let mut url = format!(
        "{}/featured?key={}&contentfilter={}&media_filter=tinygif,gif&limit=24",
        TENOR_BASE,
        urlencoding::encode(&key),
        urlencoding::encode(&settings.content_filter),
    );
    if let Some(p) = pos {
        url.push_str(&format!("&pos={}", urlencoding::encode(&p)));
    }
    let body = call_tenor(&url).await?;
    Ok(parse_results(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_results_empty() {
        let body = serde_json::json!({"results": []});
        let r = parse_results(body);
        assert_eq!(r.gifs.len(), 0);
        assert!(r.next.is_none());
    }

    #[test]
    fn parse_results_with_next() {
        let body = serde_json::json!({
            "results": [{
                "id": "abc",
                "title": "Test GIF",
                "media_formats": {
                    "tinygif": { "url": "https://example.com/tiny.gif", "dims": [200, 200] },
                    "gif": { "url": "https://example.com/full.gif", "dims": [400, 400] }
                }
            }],
            "next": "cursor123"
        });
        let r = parse_results(body);
        assert_eq!(r.gifs.len(), 1);
        assert_eq!(r.gifs[0].id, "abc");
        assert_eq!(r.gifs[0].preview_url, "https://example.com/tiny.gif");
        assert_eq!(r.gifs[0].full_url, "https://example.com/full.gif");
        assert_eq!(r.gifs[0].width, 200);
        assert_eq!(r.next.as_deref(), Some("cursor123"));
    }

    #[test]
    fn parse_results_falls_back_when_no_full_gif() {
        let body = serde_json::json!({
            "results": [{
                "id": "x",
                "title": "",
                "media_formats": {
                    "tinygif": { "url": "https://example.com/tiny.gif", "dims": [100, 100] }
                }
            }]
        });
        let r = parse_results(body);
        assert_eq!(r.gifs.len(), 1);
        assert_eq!(r.gifs[0].full_url, r.gifs[0].preview_url);
    }
}
```

NOTE: this uses `urlencoding` crate. If not already a dep, add it to Cargo.toml: `urlencoding = "2"`. (It's a small no-deps crate.)

- [ ] **Step 2: Add urlencoding dep if not present**

```
grep '^urlencoding' /home/deez/farder/client/src-tauri/Cargo.toml
```

If missing, add to `[dependencies]`:
```toml
urlencoding = "2"
```

- [ ] **Step 3: Run tests**

```
cd /home/deez/farder/client/src-tauri && cargo test --lib tenor::tests 2>&1 | tail -10
```

Expected: 3 tests pass.

- [ ] **Step 4: Verify full server suite still compiles**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
```

Expected: `Finished`. The module is declared in main.rs in Task 3 — for now it'll be unused-warning-only.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/Cargo.toml client/src-tauri/Cargo.lock client/src-tauri/src/tenor.rs
git -C /home/deez/farder commit -m "feat(client): tenor.rs module with search/trending/settings + tests"
```

---

## Task 3: Register Tenor commands in main.rs

**Files:**
- Modify: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Add module declaration + register commands**

In `client/src-tauri/src/main.rs`:

Near the existing `mod book;` line, add:
```rust
mod tenor;
```

Inside the `tauri::generate_handler![ ... ]` block, near the other book/settings commands, add:
```rust
            tenor::tenor_search,
            tenor::tenor_trending,
            tenor::get_gif_search_settings,
            tenor::set_gif_search_settings,
```

- [ ] **Step 2: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
```

Expected: `Finished`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/main.rs
git -C /home/deez/farder commit -m "feat(client): register tenor commands"
```

---

## Task 4: TS bridge bindings + types

**Files:**
- Create: `client/src/lib/gifSearch.ts`

- [ ] **Step 1: Create the file**

`client/src/lib/gifSearch.ts`:

```ts
import { invoke } from "@tauri-apps/api/core";

export interface TenorGif {
  id: string;
  title: string;
  preview_url: string;
  full_url: string;
  width: number;
  height: number;
}

export interface TenorSearchResult {
  gifs: TenorGif[];
  next: string | null;
}

export interface GifSearchSettings {
  enabled: boolean;
  content_filter: "high" | "medium" | "low" | "off";
  user_api_key: string | null;
}

export async function tenorSearch(query: string, pos?: string): Promise<TenorSearchResult> {
  return invoke<TenorSearchResult>("tenor_search", { query, pos: pos ?? null });
}

export async function tenorTrending(pos?: string): Promise<TenorSearchResult> {
  return invoke<TenorSearchResult>("tenor_trending", { pos: pos ?? null });
}

export async function getGifSearchSettings(): Promise<GifSearchSettings> {
  return invoke<GifSearchSettings>("get_gif_search_settings");
}

export async function setGifSearchSettings(settings: GifSearchSettings): Promise<void> {
  return invoke<void>("set_gif_search_settings", { settings });
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/lib/gifSearch.ts
git -C /home/deez/farder commit -m "feat(client): TS bindings for Tenor commands"
```

---

## Task 5: GifSearchOptIn modal

Small first-click privacy modal.

**Files:**
- Create: `client/src/components/GifSearchOptIn.tsx`

- [ ] **Step 1: Create the file**

`client/src/components/GifSearchOptIn.tsx`:

```tsx
import { type CSSProperties } from "react";

interface Props {
  onCancel: () => void;
  onEnable: () => void;
}

const overlay: CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0,0,0,0.4)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 2400,
};

const card: CSSProperties = {
  background: "var(--xp-window-bg, #ECE9D8)",
  color: "var(--xp-text-normal, #000)",
  border: "2px solid var(--xp-blue-dark, #003C74)",
  padding: 20,
  width: 420,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

export default function GifSearchOptIn({ onCancel, onEnable }: Props) {
  return (
    <div style={overlay} onClick={onCancel}>
      <div style={card} onClick={(e) => e.stopPropagation()}>
        <h3 style={{ marginTop: 0 }}>Enable GIF search?</h3>
        <p style={{ fontSize: 11, lineHeight: 1.5 }}>
          GIF search uses Tenor (owned by Google). When enabled, Tenor will see your search terms and your IP address. NSFW content is filtered out by default.
        </p>
        <p style={{ fontSize: 11, color: "var(--xp-text-muted, #666)" }}>
          You can disable this anytime in Settings → GIF Search.
        </p>
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 6, marginTop: 16 }}>
          <button onClick={onCancel} style={{ font: "inherit", padding: "4px 12px" }}>
            Cancel
          </button>
          <button
            onClick={onEnable}
            style={{
              font: "inherit",
              padding: "4px 12px",
              background: "var(--xp-blue, #0058E6)",
              color: "#fff",
              border: "1px solid var(--xp-blue-dark, #003C74)",
            }}
          >
            Enable
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

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/GifSearchOptIn.tsx
git -C /home/deez/farder commit -m "feat(client): GifSearchOptIn privacy modal"
```

---

## Task 6: GifPicker component

The big component: search input + grid + pagination + hover-save overlay.

**Files:**
- Create: `client/src/components/GifPicker.tsx`

- [ ] **Step 1: Create the file**

`client/src/components/GifPicker.tsx`:

```tsx
import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import * as gifApi from "../lib/gifSearch";
import * as bookApi from "../lib/book/client";
import type { TenorGif, GifSearchSettings } from "../lib/gifSearch";

interface Props {
  serverId: string;
  channelId: number;
  onClose: () => void;
}

const popover: CSSProperties = {
  position: "absolute",
  bottom: "calc(100% + 4px)",
  left: 0,
  background: "var(--xp-panel-bg, #fff)",
  color: "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  boxShadow: "2px 2px 8px rgba(0,0,0,0.3)",
  padding: 8,
  width: 360,
  maxHeight: 420,
  zIndex: 1100,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
  fontSize: "var(--xp-font-size, 11px)",
  display: "flex",
  flexDirection: "column",
  gap: 6,
};

function GifTile({
  gif,
  onSend,
  onSave,
}: {
  gif: TenorGif;
  onSend: (g: TenorGif) => void;
  onSave: (g: TenorGif) => void;
}) {
  const [hover, setHover] = useState(false);
  return (
    <div
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      style={{
        position: "relative",
        width: "calc(50% - 4px)",
        cursor: "pointer",
        aspectRatio: gif.width && gif.height ? `${gif.width} / ${gif.height}` : "1 / 1",
      }}
      onClick={() => onSend(gif)}
      title={gif.title}
    >
      <img
        src={gif.preview_url}
        alt={gif.title}
        style={{ width: "100%", height: "100%", objectFit: "cover", border: "1px solid var(--xp-border, #888)" }}
      />
      {hover && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onSave(gif);
          }}
          title="Save to book"
          style={{
            position: "absolute",
            top: 4,
            right: 4,
            background: "rgba(0,0,0,0.7)",
            color: "#fff",
            border: "none",
            borderRadius: 4,
            padding: "2px 6px",
            cursor: "pointer",
            fontSize: 14,
          }}
        >
          📚
        </button>
      )}
    </div>
  );
}

export default function GifPicker({ serverId, channelId, onClose }: Props) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [search, setSearch] = useState("");
  const [debouncedSearch, setDebouncedSearch] = useState("");
  const [results, setResults] = useState<TenorGif[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [settings, setSettings] = useState<GifSearchSettings | null>(null);
  const reqSeqRef = useRef(0);

  // Initial settings load + close-on-outside / Esc
  useEffect(() => {
    gifApi.getGifSearchSettings().then(setSettings).catch(() => {});
    function handleMouse(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    }
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("mousedown", handleMouse);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleMouse);
      document.removeEventListener("keydown", handleKey);
    };
  }, [onClose]);

  // Debounce search input.
  useEffect(() => {
    const t = setTimeout(() => setDebouncedSearch(search), 300);
    return () => clearTimeout(t);
  }, [search]);

  // Fetch on debouncedSearch change. Empty → trending.
  useEffect(() => {
    const seq = ++reqSeqRef.current;
    setLoading(true);
    setError(null);
    const fetcher = debouncedSearch.trim()
      ? gifApi.tenorSearch(debouncedSearch.trim())
      : gifApi.tenorTrending();
    fetcher
      .then((r) => {
        if (seq !== reqSeqRef.current) return; // stale
        setResults(r.gifs);
        setNext(r.next);
      })
      .catch((e) => {
        if (seq !== reqSeqRef.current) return;
        setError(String(e));
        setResults([]);
        setNext(null);
      })
      .finally(() => {
        if (seq === reqSeqRef.current) setLoading(false);
      });
  }, [debouncedSearch]);

  async function loadMore() {
    if (!next || loading) return;
    const seq = ++reqSeqRef.current;
    setLoading(true);
    try {
      const more = debouncedSearch.trim()
        ? await gifApi.tenorSearch(debouncedSearch.trim(), next)
        : await gifApi.tenorTrending(next);
      if (seq !== reqSeqRef.current) return;
      setResults((prev) => [...prev, ...more.gifs]);
      setNext(more.next);
    } catch (e) {
      if (seq === reqSeqRef.current) setError(String(e));
    } finally {
      if (seq === reqSeqRef.current) setLoading(false);
    }
  }

  function handleScroll(e: React.UIEvent<HTMLDivElement>) {
    const el = e.currentTarget;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 50) {
      void loadMore();
    }
  }

  async function send(gif: TenorGif) {
    try {
      const fileId = await api.fetchUrl(serverId, gif.full_url, channelId);
      await api.sendMessage(serverId, channelId, "", undefined, [fileId]);
      onClose();
    } catch (e) {
      setError(String(e));
    }
  }

  async function save(gif: TenorGif) {
    try {
      const fileId = await api.fetchUrl(serverId, gif.full_url, channelId);
      const safeName = gif.title.replace(/[^a-zA-Z0-9]+/g, "-").toLowerCase().slice(0, 32) || "tenor-gif";
      await bookApi.bookSaveFromUrl(serverId, fileId, safeName);
    } catch (e) {
      setError(String(e));
    }
  }

  const showNsfwWarning = settings && settings.content_filter !== "high";

  return (
    <div ref={ref} style={popover}>
      <input
        autoFocus
        placeholder="Search Tenor…"
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        style={{ font: "inherit", padding: "2px 6px" }}
      />
      {showNsfwWarning && (
        <div style={{ fontSize: 10, color: "#a60", background: "#fff8e1", padding: "2px 6px", border: "1px solid #e0c060" }}>
          Content filter is set to "{settings?.content_filter}". Adult content may appear.
        </div>
      )}
      {error && <div style={{ color: "#a00", fontSize: 10 }}>{error}</div>}
      <div
        onScroll={handleScroll}
        style={{ overflowY: "auto", display: "flex", flexWrap: "wrap", gap: 4 }}
      >
        {loading && results.length === 0 && (
          <div style={{ width: "100%", textAlign: "center", padding: 16, color: "var(--xp-text-muted, #666)" }}>
            Loading…
          </div>
        )}
        {!loading && !error && results.length === 0 && (
          <div style={{ width: "100%", textAlign: "center", padding: 16, color: "var(--xp-text-muted, #666)" }}>
            {debouncedSearch.trim() ? "No results." : "Try a search to find GIFs."}
          </div>
        )}
        {results.map((g) => (
          <GifTile key={g.id} gif={g} onSend={send} onSave={save} />
        ))}
        {loading && results.length > 0 && (
          <div style={{ width: "100%", textAlign: "center", padding: 8, color: "var(--xp-text-muted, #666)" }}>
            Loading more…
          </div>
        )}
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
git -C /home/deez/farder add client/src/components/GifPicker.tsx
git -C /home/deez/farder commit -m "feat(client): GifPicker — search + trending + grid + pagination + hover-save"
```

---

## Task 7: GifSearchSettings tab content

**Files:**
- Create: `client/src/components/GifSearchSettings.tsx`

- [ ] **Step 1: Create the file**

`client/src/components/GifSearchSettings.tsx`:

```tsx
import { useEffect, useState, type CSSProperties } from "react";
import * as gifApi from "../lib/gifSearch";
import type { GifSearchSettings as Settings } from "../lib/gifSearch";

const row: CSSProperties = {
  display: "flex",
  alignItems: "center",
  justifyContent: "space-between",
  padding: "8px 0",
  gap: 12,
};

const TENOR_DOCS_URL = "https://developers.google.com/tenor/guides/quickstart";

export default function GifSearchSettings() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    gifApi.getGifSearchSettings().then(setSettings).catch((e) => setError(String(e)));
  }, []);

  function update(patch: Partial<Settings>) {
    if (!settings) return;
    const next: Settings = { ...settings, ...patch };
    if (patch.content_filter === "off" && settings.content_filter !== "off") {
      if (!window.confirm("Content filter off — adult content may appear in your searches. Are you sure?")) {
        return;
      }
    }
    setSettings(next);
    gifApi.setGifSearchSettings(next).catch((e) => setError(String(e)));
  }

  if (!settings) {
    return <div style={{ padding: 12 }}>Loading…</div>;
  }

  return (
    <div style={{ padding: 12 }}>
      <h3 style={{ marginTop: 0 }}>GIF Search</h3>
      {error && <div style={{ color: "#a00", marginBottom: 8 }}>{error}</div>}

      <div style={row}>
        <label htmlFor="gif-enabled">Enable Tenor GIF search</label>
        <input
          id="gif-enabled"
          type="checkbox"
          checked={settings.enabled}
          onChange={(e) => update({ enabled: e.target.checked })}
        />
      </div>

      {settings.enabled && (
        <>
          <p style={{ fontSize: 11, color: "var(--xp-text-muted, #666)", marginTop: 4 }}>
            Tenor is owned by Google. Searches are sent to Google's servers; your IP and search terms are visible to them.
          </p>

          <div style={row}>
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

          <div style={{ marginTop: 12 }}>
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
            <p style={{ fontSize: 10, color: "var(--xp-text-muted, #666)", marginTop: 4 }}>
              Setting your own key avoids sharing the default Farder quota.{" "}
              <a
                href={TENOR_DOCS_URL}
                onClick={(e) => {
                  e.preventDefault();
                  // Use window.open as a fallback; Tauri intercepts external links.
                  window.open(TENOR_DOCS_URL, "_blank");
                }}
                style={{ color: "var(--xp-blue, #0058E6)" }}
              >
                How to get a Tenor API key
              </a>
            </p>
          </div>
        </>
      )}
    </div>
  );
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/GifSearchSettings.tsx
git -C /home/deez/farder commit -m "feat(client): GifSearchSettings tab content"
```

---

## Task 8: Add tab structure to AppearanceSettings + plug in GIF Search tab

**Files:**
- Modify: `client/src/components/AppearanceSettings.tsx`

- [ ] **Step 1: Read the current AppearanceSettings.tsx**

In `client/src/components/AppearanceSettings.tsx`, get a feel for the existing structure: the modal title is "Appearance", the body shows themes. We'll wrap that body in a tab structure.

- [ ] **Step 2: Add the tab system**

At the top of the file, add:

```tsx
import GifSearchSettings from "./GifSearchSettings";
```

Inside the component, add a tab state:

```tsx
const [activeTab, setActiveTab] = useState<"appearance" | "gif">("appearance");
```

In the JSX, find the title row (with "Appearance" + close button). Change "Appearance" to "Settings".

Below the title row, add a tab bar:

```tsx
<div style={{ display: "flex", borderBottom: "1px solid var(--xp-border, #888)", padding: "0 4px", flexShrink: 0 }}>
  {(["appearance", "gif"] as const).map((tab) => (
    <button
      key={tab}
      onClick={() => setActiveTab(tab)}
      style={{
        font: "inherit",
        padding: "6px 12px",
        background: activeTab === tab ? "var(--xp-panel-bg, #fff)" : "transparent",
        color: activeTab === tab ? "var(--xp-blue, #0058E6)" : "inherit",
        border: "none",
        borderBottom: activeTab === tab ? "2px solid var(--xp-blue, #0058E6)" : "2px solid transparent",
        cursor: "pointer",
      }}
    >
      {tab === "appearance" ? "Appearance" : "GIF Search"}
    </button>
  ))}
</div>
```

Wrap the existing body (the themes grid + footer) in a conditional:

```tsx
{activeTab === "appearance" && (
  <>
    {/* existing themes-grid + footer JSX */}
  </>
)}
{activeTab === "gif" && <GifSearchSettings />}
```

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`. If you see errors about a missing `<>...</>` fragment around the themes content, just wrap it.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/components/AppearanceSettings.tsx
git -C /home/deez/farder commit -m "feat(client): AppearanceSettings becomes tabbed Settings (Appearance + GIF Search)"
```

---

## Task 9: Wire 🎬 button into MessageInput + opt-in flow

**Files:**
- Modify: `client/src/components/MessageInput.tsx`

- [ ] **Step 1: Add imports + state**

In `client/src/components/MessageInput.tsx`, add to imports:

```tsx
import * as gifApi from "../lib/gifSearch";
import GifPicker from "./GifPicker";
import GifSearchOptIn from "./GifSearchOptIn";
```

Inside the component, add state alongside other show* flags:

```tsx
const [showGifPicker, setShowGifPicker] = useState(false);
const [showGifOptIn, setShowGifOptIn] = useState(false);
```

- [ ] **Step 2: Add the click handler**

Below the existing handlers:

```tsx
async function handleGifButtonClick() {
  try {
    const settings = await gifApi.getGifSearchSettings();
    if (settings.enabled) {
      setShowGifPicker((s) => !s);
    } else {
      setShowGifOptIn(true);
    }
  } catch (e) {
    setError(String(e));
  }
}

async function handleGifOptInEnable() {
  try {
    const current = await gifApi.getGifSearchSettings();
    await gifApi.setGifSearchSettings({ ...current, enabled: true });
    setShowGifOptIn(false);
    setShowGifPicker(true);
  } catch (e) {
    setError(String(e));
  }
}
```

- [ ] **Step 3: Add the 🎬 button + picker rendering**

In the message-input-row JSX, after the existing 🎁 SendStickerPicker block, add:

```tsx
<div style={{ position: "relative" }}>
  <button
    className="xp-button attach-btn"
    onClick={handleGifButtonClick}
    disabled={sending}
    title="GIF Search"
  >
    🎬
  </button>
  {showGifPicker && (
    <GifPicker
      serverId={serverId}
      channelId={channelId}
      onClose={() => setShowGifPicker(false)}
    />
  )}
</div>
```

At the END of the component's JSX (alongside the existing `{showBook && ...}` and `{autocompleteQuery !== null && ...}`), add:

```tsx
{showGifOptIn && (
  <GifSearchOptIn
    onCancel={() => setShowGifOptIn(false)}
    onEnable={handleGifOptInEnable}
  />
)}
```

- [ ] **Step 4: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src/components/MessageInput.tsx
git -C /home/deez/farder commit -m "feat(client): wire 🎬 button into MessageInput with opt-in modal flow"
```

---

## Task 10: Smoke test + CHANGELOG

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Restart dev session and walk smoke tests**

Replace `REPLACE_WITH_REAL_KEY` in `client/src-tauri/src/tenor.rs` with a real Tenor API key first (get one from https://developers.google.com/tenor/guides/quickstart) — without it the picker will return "No Tenor API key configured".

```
pkill -f farder-server
cd /home/deez/farder/client && npm run tauri dev
```

Confirm:

- [ ] 🎬 button visible in message input next to 🎁.
- [ ] First click → opt-in modal with privacy warning. Cancel → modal closes, picker doesn't open.
- [ ] Click again → opt-in modal again. Click Enable → setting flips, modal closes, picker opens with trending GIFs.
- [ ] Type a search → results refresh after ~300ms.
- [ ] Click a result → message sends as a normal image attachment. Picker closes.
- [ ] Hover a result → 📚 overlay button appears. Click → GIF saved to your book (visible in BookBrowser).
- [ ] Scroll to bottom of grid → "Loading more…" appears, more results append.
- [ ] Open Settings (⚙ in user footer) → tabs say "Appearance" and "GIF Search". Default tab is Appearance.
- [ ] Click GIF Search tab → toggle, content filter dropdown, BYO key input visible.
- [ ] Toggle off → save persists; reopening picker shows opt-in modal again.
- [ ] Set content filter to "off" → confirm prompt. Confirm → picker shows the warning banner.
- [ ] Set an invalid API key → next picker open shows "Your API key was rejected" message.
- [ ] Sent GIFs visible to recipients as normal image attachments (no Tenor URL leak).

- [ ] **Step 2: Add CHANGELOG entry**

In `CHANGELOG.md`, under the most recent `### Added` block, add:

```
- (2026-05-05) GIF search via Tenor: 🎬 button in the message input opens a search picker (trending by default, debounced search-as-you-type). Click a result to send as a normal image attachment (Tenor URL never leaks to other clients — fetches via the existing server-side fetch_url path). Hover a result to save it directly to your Reaction Book. Opt-in for privacy: first click shows a warning that Tenor (Google) sees your search terms and IP. NSFW filter on by default, user-adjustable. Bring-your-own Tenor API key supported in Settings → GIF Search to avoid sharing the default Farder quota. AppearanceSettings is now a tabbed "Settings" modal (Appearance + GIF Search).
```

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add CHANGELOG.md
git -C /home/deez/farder commit -m "docs: changelog for GIF search (Tenor)"
```

---

## Self-review notes

**Spec coverage:**
- Tauri-side proxy (key never in renderer) → Task 2
- Two query commands + two settings commands → Tasks 2, 3
- Settings stored in settings.json via existing helpers → Task 2 (uses `crate::commands::settings_get` / `settings_set`)
- Embedded default key + bring-your-own override → Task 2 (resolve_key)
- Privacy gate: button always visible, first-click modal → Tasks 5, 9
- Picker UI with search + trending + pagination + hover-save → Task 6
- NSFW handling: default high + user toggle + warning banner → Tasks 2, 6, 7
- Send via existing fetch_url → Task 6 (`api.fetchUrl(serverId, full_url, channelId)`)
- Save via existing book_save_from_url → Task 6
- Settings panel as tabs in AppearanceSettings → Task 8
- All edge cases (no network / quota / invalid key / NSFW off) → Tasks 2, 6
- Backwards compat (no protocol changes) → no protocol changes throughout
- Render warning when filter is below high → Task 6 (`showNsfwWarning`)

**Type/name consistency:** `TenorGif`, `TenorSearchResult`, `GifSearchSettings` defined in Task 2, mirrored in TS Task 4, used in Tasks 6, 7, 9. Tauri command names match between Rust (Task 2) and TS bindings (Task 4).

**No placeholders:** every code step has runnable code. The only "find existing pattern" reference is in Task 8 for AppearanceSettings's existing body — necessary because the existing JSX shape varies and the implementer must align with established conventions.

**Known compromise:**
- `TENOR_DEFAULT_KEY` ships as a literal string in source. For v1 the dev (you) replaces the placeholder before testing; production-grade rotation is a follow-up.
- The save-to-book flow uploads the GIF to the current server before saving locally — costs one server attachment but reuses existing infra (deferred follow-up: a direct-from-URL save without server round-trip).
- No automated tests for the React components — the codebase has no JS test infra (consistent with prior sessions).
