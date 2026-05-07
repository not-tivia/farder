# Translation v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Right-click any chat message → "Translate" → translated text appears below the original. All translation runs on the user's device via Bergamot WASM. No chat content ever leaves the device.

**Architecture:** Bergamot WASM in the Tauri WebView (no native build deps). Rust side handles file storage + Mozilla CDN downloads + settings persistence. TS side handles language detection (franc), engine instantiation, store, and UI. Models cached in `~/.farder/translation-models/<src>-<trg>/` and served to the WebView via Tauri's asset protocol.

**Tech Stack:** Rust + Tauri 2 (existing), `@browsermt/bergamot-translator` 0.4.9 (WASM), `franc-min` 6.x (language detection), Mozilla's public GCS bucket for model files, `flate2` for gzip decompression, existing `reqwest` + `sha2` for fetch + integrity.

**Spec:** `docs/superpowers/specs/2026-05-07-translation-v1-design.md`

---

## File structure

**Created (Rust):**
- `client/src-tauri/src/translation.rs` — types, commands, file ops, registry fetch

**Modified (Rust):**
- `client/src-tauri/src/main.rs` — `mod translation;` + register commands
- `client/src-tauri/Cargo.toml` — add `flate2`

**Created (TS):**
- `client/src/lib/translation/api.ts` — Tauri command bindings
- `client/src/lib/translation/types.ts` — shared types
- `client/src/lib/translation/lang.ts` — ISO 639-3 ↔ 1 mapping + display names
- `client/src/lib/translation/detect.ts` — franc wrapper
- `client/src/lib/translation/engine.ts` — Bergamot WASM + Translator pool
- `client/src/lib/translation/models.ts` — Mozilla registry fetcher
- `client/src/lib/translation/store.ts` — per-message translation state
- `client/src/components/TranslatedRow.tsx` — UI row below message
- `client/src/components/TranslationSettingsTab.tsx` — Settings tab content
- `client/src/components/TranslationFirstRunModal.tsx` — first-run picker
- `client/src/components/TranslationDownloadDialog.tsx` — privacy disclosure + progress

**Modified (TS):**
- `client/package.json` — add `@browsermt/bergamot-translator`, `franc-min`, `iso-639-3`
- `client/src/components/Message.tsx` — context menu item + render TranslatedRow
- `client/src/components/AppearanceSettings.tsx` — add "translation" to tab list

---

## Phase 1: Rust foundation

## Task 1: translation.rs scaffold + types + settings

**Files:**
- Create: `client/src-tauri/src/translation.rs`
- Modify: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Create translation.rs with types and stub commands**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LangPair {
    pub src: String,
    pub trg: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModel {
    pub pair: LangPair,
    pub disk_size_bytes: u64,
    pub downloaded_at: u64,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPaths {
    pub model: String,  // converted asset URL
    pub vocab: String,
    pub lex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationSettings {
    pub enabled: bool,
    pub default_target: String,  // ISO 639-1
    pub seen_first_run: bool,
}

impl Default for TranslationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            default_target: "en".to_string(),
            seen_first_run: false,
        }
    }
}

fn translation_models_dir() -> std::path::PathBuf {
    crate::commands::farder_data_dir_pub().join("translation-models")
}

fn pair_dir(pair: &LangPair) -> std::path::PathBuf {
    translation_models_dir().join(format!("{}-{}", pair.src, pair.trg))
}

#[tauri::command]
pub fn get_translation_settings() -> TranslationSettings {
    let enabled = crate::commands::settings_get("translation_enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let default_target = crate::commands::settings_get("translation_default_target")
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "en".to_string());
    let seen_first_run = crate::commands::settings_get("translation_seen_first_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    TranslationSettings { enabled, default_target, seen_first_run }
}

#[tauri::command]
pub fn set_translation_settings(settings: TranslationSettings) -> Result<(), String> {
    crate::commands::settings_set("translation_enabled", serde_json::Value::Bool(settings.enabled))?;
    crate::commands::settings_set(
        "translation_default_target",
        serde_json::Value::String(settings.default_target),
    )?;
    crate::commands::settings_set(
        "translation_seen_first_run",
        serde_json::Value::Bool(settings.seen_first_run),
    )?;
    Ok(())
}
```

- [ ] **Step 2: Wire module + register settings commands in main.rs**

In `client/src-tauri/src/main.rs`, after `mod voice;` (or wherever modules are listed), add:

```rust
mod translation;
```

Inside `tauri::Builder::default() ... .invoke_handler(tauri::generate_handler![ ... ])`, add the two new commands at the bottom of the list (alongside `tenor::*` entries):

```rust
            translation::get_translation_settings,
            translation::set_translation_settings,
```

- [ ] **Step 3: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -10
```

Expected: `Finished`. Warnings about unused fns are fine.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/translation.rs client/src-tauri/src/main.rs
git -C /home/deez/farder commit -m "feat(client): translation module scaffold + settings commands"
```

---

## Task 2: Mozilla registry fetcher

**Files:**
- Modify: `client/src-tauri/src/translation.rs`

- [ ] **Step 1: Add registry types**

Append to `translation.rs`:

```rust
const REGISTRY_URL: &str = "https://storage.googleapis.com/moz-fx-translations-data--303e-prod-translations-data/db/models.json";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistryEntry {
    pub source_language: String,
    pub target_language: String,
    pub architecture: String,
    pub release_status: Option<String>,
    pub files: RegistryFiles,
    pub model_statistics: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistryFiles {
    pub model: RegistryFile,
    pub vocab: RegistryFile,
    pub lexical_shortlist: RegistryFile,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistryFile {
    pub path: String,
    #[serde(default)]
    pub uncompressed_size: Option<u64>,
    #[serde(default)]
    pub uncompressed_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AvailablePair {
    pub src: String,
    pub trg: String,
    pub size_bytes: u64,
    pub display_name: String,  // "Spanish ↔ English" — built TS-side; Rust just emits src/trg
}
```

- [ ] **Step 2: Add fetch_registry helper + list_available_pairs command**

```rust
#[derive(Debug, Clone, Deserialize)]
struct RegistryRoot {
    base_url: String,
    models: std::collections::HashMap<String, Vec<RegistryEntry>>,
}

async fn fetch_registry() -> Result<RegistryRoot, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(REGISTRY_URL).send().await.map_err(|e| e.to_string())?;
    let text = resp.text().await.map_err(|e| e.to_string())?;
    // Mozilla's JSON uses camelCase keys; serde with rename_all handles it.
    serde_json::from_str(&text).map_err(|e| format!("registry parse: {e}"))
}

/// Return the registry entry for a pair preferring `releaseStatus == "Release"` and
/// `architecture == "base-memory"` when available. Falls back to first present entry.
fn pick_entry<'a>(entries: &'a [RegistryEntry]) -> Option<&'a RegistryEntry> {
    let released: Vec<&RegistryEntry> = entries
        .iter()
        .filter(|e| e.release_status.as_deref() == Some("Release"))
        .collect();
    let pool: &[&RegistryEntry] = if released.is_empty() {
        return entries.first();
    } else {
        &released
    };
    pool.iter().find(|e| e.architecture == "base-memory").copied()
        .or_else(|| pool.first().copied())
}

#[tauri::command]
pub async fn list_available_pairs() -> Result<Vec<AvailablePair>, String> {
    let registry = fetch_registry().await?;
    let mut out = Vec::new();
    for (key, entries) in &registry.models {
        let parts: Vec<&str> = key.split('-').collect();
        if parts.len() != 2 { continue; }
        let Some(entry) = pick_entry(entries) else { continue };
        let size = entry.files.model.uncompressed_size.unwrap_or(0)
            + entry.files.vocab.uncompressed_size.unwrap_or(0)
            + entry.files.lexical_shortlist.uncompressed_size.unwrap_or(0);
        out.push(AvailablePair {
            src: parts[0].to_string(),
            trg: parts[1].to_string(),
            size_bytes: size,
            display_name: format!("{} → {}", parts[0], parts[1]),
        });
    }
    out.sort_by(|a, b| (a.src.clone(), a.trg.clone()).cmp(&(b.src.clone(), b.trg.clone())));
    Ok(out)
}
```

Add `#[serde(rename_all = "camelCase")]` to `RegistryEntry`, `RegistryFiles`, `RegistryFile`, and `RegistryRoot` so Mozilla's `sourceLanguage`/`baseUrl`/etc. parse correctly:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryEntry { ... }

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryFiles { ... }

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryFile { ... }

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryRoot { ... }
```

- [ ] **Step 3: Add unit test for pick_entry**

At the bottom of `translation.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn entry(arch: &str, status: Option<&str>) -> RegistryEntry {
        RegistryEntry {
            source_language: "en".into(),
            target_language: "es".into(),
            architecture: arch.into(),
            release_status: status.map(String::from),
            files: RegistryFiles {
                model: RegistryFile { path: "m".into(), uncompressed_size: None, uncompressed_hash: None },
                vocab: RegistryFile { path: "v".into(), uncompressed_size: None, uncompressed_hash: None },
                lexical_shortlist: RegistryFile { path: "l".into(), uncompressed_size: None, uncompressed_hash: None },
            },
            model_statistics: None,
        }
    }

    #[test]
    fn pick_entry_prefers_released_base_memory() {
        let v = vec![
            entry("base", Some("Release")),
            entry("base-memory", Some("Release")),
            entry("base-memory", None),
        ];
        let picked = pick_entry(&v).unwrap();
        assert_eq!(picked.architecture, "base-memory");
        assert_eq!(picked.release_status.as_deref(), Some("Release"));
    }

    #[test]
    fn pick_entry_falls_back_when_none_released() {
        let v = vec![entry("base", None), entry("base-memory", None)];
        let picked = pick_entry(&v).unwrap();
        // Falls through to first
        assert_eq!(picked.architecture, "base");
    }
}
```

- [ ] **Step 4: Register list_available_pairs in main.rs**

Add to the invoke_handler list:

```rust
            translation::list_available_pairs,
```

- [ ] **Step 5: Run tests**

```
cd /home/deez/farder/client/src-tauri && cargo test translation::tests 2>&1 | tail -10
```

Expected: 2 passed.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/translation.rs client/src-tauri/src/main.rs
git -C /home/deez/farder commit -m "feat(client): translation registry fetcher + pick_entry tests"
```

---

## Task 3: download_model command (atomic write + sha256 verify)

**Files:**
- Modify: `client/src-tauri/src/translation.rs`
- Modify: `client/src-tauri/Cargo.toml`

- [ ] **Step 1: Add flate2 dep**

In `client/src-tauri/Cargo.toml`, alongside `sha2 = "0.10"`:

```toml
flate2 = "1"
```

- [ ] **Step 2: Add download_model command**

Append to `translation.rs`:

```rust
use std::io::{Read, Write};

#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub pair: LangPair,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub stage: String,  // "downloading" | "decompressing" | "verifying" | "saving" | "done" | "error"
    pub message: Option<String>,
}

#[tauri::command]
pub async fn download_model(
    pair: LangPair,
    app: tauri::AppHandle,
) -> Result<LocalModel, String> {
    use sha2::{Digest, Sha256};
    use tauri::Emitter;

    let registry = fetch_registry().await?;
    let key = format!("{}-{}", pair.src, pair.trg);
    let entries = registry
        .models
        .get(&key)
        .ok_or_else(|| format!("no models for {key}"))?;
    let entry = pick_entry(entries).ok_or_else(|| format!("no usable entry for {key}"))?;

    // Compute total size for progress reporting (sum of compressed sizes is unknown ahead of HEAD;
    // use uncompressed as a UX-acceptable approximation).
    let approx_total = entry.files.model.uncompressed_size.unwrap_or(0)
        + entry.files.vocab.uncompressed_size.unwrap_or(0)
        + entry.files.lexical_shortlist.uncompressed_size.unwrap_or(0);

    let dir = pair_dir(&pair);
    let tmp_dir = dir.with_extension("tmp");
    if tmp_dir.exists() {
        std::fs::remove_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| e.to_string())?;

    let emit = |stage: &str, done: u64, msg: Option<&str>| {
        let _ = app.emit(
            "translation:progress",
            DownloadProgress {
                pair: pair.clone(),
                bytes_done: done,
                bytes_total: approx_total,
                stage: stage.to_string(),
                message: msg.map(String::from),
            },
        );
    };

    let mut cumulative = 0u64;
    for (slot, file_meta) in [
        ("model.bin", &entry.files.model),
        ("vocab.spm", &entry.files.vocab),
        ("lex.bin", &entry.files.lexical_shortlist),
    ] {
        let url = format!("{}/{}", registry.base_url, file_meta.path);
        emit("downloading", cumulative, Some(slot));
        let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
        let compressed = resp.bytes().await.map_err(|e| e.to_string())?;
        emit("decompressing", cumulative, Some(slot));
        let mut decoder = flate2::read::GzDecoder::new(&compressed[..]);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).map_err(|e| format!("gunzip {slot}: {e}"))?;

        if let Some(expected_hash) = file_meta.uncompressed_hash.as_deref() {
            emit("verifying", cumulative, Some(slot));
            let mut hasher = Sha256::new();
            hasher.update(&out);
            let got = hex::encode(hasher.finalize());
            if got != expected_hash.to_lowercase() {
                let _ = std::fs::remove_dir_all(&tmp_dir);
                return Err(format!("sha256 mismatch on {slot}"));
            }
        }

        emit("saving", cumulative, Some(slot));
        let mut f = std::fs::File::create(tmp_dir.join(slot)).map_err(|e| e.to_string())?;
        f.write_all(&out).map_err(|e| e.to_string())?;
        cumulative += out.len() as u64;
    }

    // Write meta.json
    let meta = serde_json::json!({
        "version": format!("{}-{}", entry.architecture, entry.release_status.as_deref().unwrap_or("?")),
        "downloaded_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs()).unwrap_or(0),
        "src": pair.src,
        "trg": pair.trg,
    });
    std::fs::write(tmp_dir.join("meta.json"), serde_json::to_vec_pretty(&meta).unwrap())
        .map_err(|e| e.to_string())?;

    // Atomic rename
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp_dir, &dir).map_err(|e| e.to_string())?;

    emit("done", cumulative, None);
    Ok(LocalModel {
        pair,
        disk_size_bytes: cumulative,
        downloaded_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        version: format!("{}-{}", entry.architecture, entry.release_status.as_deref().unwrap_or("?")),
    })
}
```

- [ ] **Step 3: Register download_model in main.rs**

```rust
            translation::download_model,
```

- [ ] **Step 4: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
```

Expected: `Finished`. (No automated test — atomic-rename behavior is verified manually.)

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/translation.rs client/src-tauri/Cargo.toml client/src-tauri/Cargo.lock
git -C /home/deez/farder commit -m "feat(client): translation download_model with sha256 + atomic rename"
```

---

## Task 4: list_local_models / delete_model / get_model_paths

**Files:**
- Modify: `client/src-tauri/src/translation.rs`
- Modify: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Add the three commands**

Append to `translation.rs`:

```rust
#[tauri::command]
pub fn list_local_models() -> Result<Vec<LocalModel>, String> {
    let dir = translation_models_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_dir() { continue; }
        let meta_path = path.join("meta.json");
        if !meta_path.exists() { continue; }
        let meta_bytes = match std::fs::read(&meta_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let meta: serde_json::Value = match serde_json::from_slice(&meta_bytes) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let src = meta.get("src").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let trg = meta.get("trg").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let version = meta.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let downloaded_at = meta.get("downloaded_at").and_then(|v| v.as_u64()).unwrap_or(0);
        if src.is_empty() || trg.is_empty() { continue; }
        // Sum file sizes for the three model files
        let mut size = 0u64;
        for slot in ["model.bin", "vocab.spm", "lex.bin"] {
            if let Ok(meta) = std::fs::metadata(path.join(slot)) {
                size += meta.len();
            }
        }
        out.push(LocalModel {
            pair: LangPair { src, trg },
            disk_size_bytes: size,
            downloaded_at,
            version,
        });
    }
    out.sort_by(|a, b| (a.pair.src.clone(), a.pair.trg.clone())
        .cmp(&(b.pair.src.clone(), b.pair.trg.clone())));
    Ok(out)
}

#[tauri::command]
pub fn delete_model(pair: LangPair) -> Result<(), String> {
    let dir = pair_dir(&pair);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn get_model_paths(pair: LangPair) -> Result<ModelPaths, String> {
    let dir = pair_dir(&pair);
    if !dir.exists() {
        return Err(format!("model not present: {}-{}", pair.src, pair.trg));
    }
    let to_url = |p: std::path::PathBuf| -> String {
        // Tauri 2: use file:// URLs which the WebView can fetch via convertFileSrc
        // on the TS side. Here we just return the absolute path; TS resolves via
        // @tauri-apps/api/core convertFileSrc.
        p.to_string_lossy().to_string()
    };
    Ok(ModelPaths {
        model: to_url(dir.join("model.bin")),
        vocab: to_url(dir.join("vocab.spm")),
        lex: to_url(dir.join("lex.bin")),
    })
}
```

- [ ] **Step 2: Add unit tests for list + delete**

Add to the existing `mod tests` block:

```rust
    #[test]
    fn list_local_models_skips_invalid_dirs() {
        // Set up a temp models dir
        let tmp = std::env::temp_dir().join(format!("farder-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("translation-models").join("en-es")).unwrap();
        std::fs::create_dir_all(tmp.join("translation-models").join("not-a-pair-no-meta")).unwrap();

        let valid_meta = serde_json::json!({
            "src": "en", "trg": "es", "version": "test", "downloaded_at": 1
        });
        std::fs::write(
            tmp.join("translation-models").join("en-es").join("meta.json"),
            serde_json::to_vec(&valid_meta).unwrap(),
        ).unwrap();

        // Override data dir for this scope by temporarily setting FARDER_DATA env var.
        // (commands::farder_data_dir reads FARDER_DATA — see the existing helper.)
        let prev = std::env::var("FARDER_DATA").ok();
        std::env::set_var("FARDER_DATA", &tmp);
        let result = list_local_models().unwrap();
        if let Some(p) = prev { std::env::set_var("FARDER_DATA", p); }
        else { std::env::remove_var("FARDER_DATA"); }

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].pair.src, "en");
        assert_eq!(result[0].pair.trg, "es");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn delete_model_idempotent() {
        // Deleting a non-existent pair should succeed.
        let pair = LangPair { src: "xx".into(), trg: "yy".into() };
        delete_model(pair).expect("idempotent delete");
    }
```

(Note: `farder_data_dir_pub` should respect `FARDER_DATA` env var per existing convention — verify by reading `commands::farder_data_dir_pub` source. If it doesn't, the test still works on a clean dev env but may pollute `~/.farder/translation-models/`. Skip the env-var override and use a fresh dir directly if the helper doesn't read the env.)

- [ ] **Step 3: Register the three commands in main.rs**

```rust
            translation::list_local_models,
            translation::delete_model,
            translation::get_model_paths,
```

- [ ] **Step 4: Run tests**

```
cd /home/deez/farder/client/src-tauri && cargo test translation::tests 2>&1 | tail -10
```

Expected: 4 passed.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/translation.rs client/src-tauri/src/main.rs
git -C /home/deez/farder commit -m "feat(client): list_local_models + delete_model + get_model_paths"
```

---

## Phase 2: TS engine library

## Task 5: TS dependencies + IPC bindings + types

**Files:**
- Modify: `client/package.json`
- Create: `client/src/lib/translation/api.ts`
- Create: `client/src/lib/translation/types.ts`
- Create: `client/src/lib/translation/lang.ts`

- [ ] **Step 1: Add npm deps**

```
cd /home/deez/farder/client && npm install --save @browsermt/bergamot-translator@0.4.9 franc-min@^6 iso-639-3@^3
```

- [ ] **Step 2: Create types.ts**

```ts
// client/src/lib/translation/types.ts

export interface LangPair {
  src: string;
  trg: string;
}

export interface LocalModel {
  pair: LangPair;
  disk_size_bytes: number;
  downloaded_at: number;
  version: string;
}

export interface ModelPaths {
  model: string;
  vocab: string;
  lex: string;
}

export interface AvailablePair {
  src: string;
  trg: string;
  size_bytes: number;
  display_name: string;
}

export interface TranslationSettings {
  enabled: boolean;
  default_target: string;
  seen_first_run: boolean;
}

export interface DownloadProgress {
  pair: LangPair;
  bytes_done: number;
  bytes_total: number;
  stage: "downloading" | "decompressing" | "verifying" | "saving" | "done" | "error";
  message?: string | null;
}

export type TranslationStatus =
  | { kind: "idle" }
  | { kind: "detecting" }
  | { kind: "loading-model"; src: string; trg: string }
  | { kind: "downloading-model"; src: string; trg: string; progress: DownloadProgress }
  | { kind: "translating"; src: string; trg: string }
  | { kind: "done"; text: string; src: string; trg: string }
  | { kind: "already-in-target"; lang: string }
  | { kind: "low-confidence"; suggested?: string }
  | { kind: "error"; reason: string };
```

- [ ] **Step 3: Create api.ts (IPC bindings)**

```ts
// client/src/lib/translation/api.ts

import { invoke } from "@tauri-apps/api/core";
import type {
  AvailablePair,
  LangPair,
  LocalModel,
  ModelPaths,
  TranslationSettings,
} from "./types";

export async function getTranslationSettings(): Promise<TranslationSettings> {
  return invoke<TranslationSettings>("get_translation_settings");
}

export async function setTranslationSettings(settings: TranslationSettings): Promise<void> {
  return invoke<void>("set_translation_settings", { settings });
}

export async function listAvailablePairs(): Promise<AvailablePair[]> {
  return invoke<AvailablePair[]>("list_available_pairs");
}

export async function listLocalModels(): Promise<LocalModel[]> {
  return invoke<LocalModel[]>("list_local_models");
}

export async function downloadModel(pair: LangPair): Promise<LocalModel> {
  return invoke<LocalModel>("download_model", { pair });
}

export async function deleteModel(pair: LangPair): Promise<void> {
  return invoke<void>("delete_model", { pair });
}

export async function getModelPaths(pair: LangPair): Promise<ModelPaths> {
  return invoke<ModelPaths>("get_model_paths", { pair });
}
```

- [ ] **Step 4: Create lang.ts (ISO mapping + display names)**

```ts
// client/src/lib/translation/lang.ts

// Minimal map covering the languages we expect to see — the most common chat languages.
// Bergamot uses ISO 639-1 codes; franc returns 639-3. Map between them here.

export const ISO_3_TO_1: Record<string, string> = {
  eng: "en", spa: "es", zho: "zh", cmn: "zh",
  fra: "fr", deu: "de", por: "pt", rus: "ru",
  ita: "it", jpn: "ja", kor: "ko", ara: "ar",
  nld: "nl", pol: "pl", ukr: "uk", tur: "tr",
  vie: "vi", tha: "th", hin: "hi", ben: "bn",
  fas: "fa", swe: "sv", nor: "no", dan: "da",
  fin: "fi", ces: "cs", hun: "hu", ron: "ro",
  ell: "el", heb: "he", ind: "id", msa: "ms",
};

export const ISO_1_TO_3: Record<string, string> = Object.fromEntries(
  Object.entries(ISO_3_TO_1).map(([three, one]) => [one, three])
);

export const DISPLAY_NAMES: Record<string, string> = {
  en: "English", es: "Spanish", zh: "Chinese", fr: "French",
  de: "German", pt: "Portuguese", ru: "Russian", it: "Italian",
  ja: "Japanese", ko: "Korean", ar: "Arabic", nl: "Dutch",
  pl: "Polish", uk: "Ukrainian", tr: "Turkish", vi: "Vietnamese",
  th: "Thai", hi: "Hindi", bn: "Bengali", fa: "Persian",
  sv: "Swedish", no: "Norwegian", da: "Danish", fi: "Finnish",
  cs: "Czech", hu: "Hungarian", ro: "Romanian", el: "Greek",
  he: "Hebrew", id: "Indonesian", ms: "Malay",
};

export function displayName(iso1: string): string {
  return DISPLAY_NAMES[iso1] ?? iso1.toUpperCase();
}

export function iso3to1(iso3: string): string | null {
  return ISO_3_TO_1[iso3] ?? null;
}
```

- [ ] **Step 5: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add client/package.json client/package-lock.json client/src/lib/translation/
git -C /home/deez/farder commit -m "feat(client): translation TS deps + types + IPC bindings + lang mapping"
```

---

## Task 6: detect.ts (franc wrapper)

**Files:**
- Create: `client/src/lib/translation/detect.ts`

- [ ] **Step 1: Implement detect**

```ts
// client/src/lib/translation/detect.ts

import { franc } from "franc-min";
import { iso3to1 } from "./lang";

export interface DetectResult {
  iso1: string | null;
  confidence: number;
}

/**
 * Detect the source language of a chat message.
 *
 * franc-min returns ISO 639-3 codes plus a confidence score. We map back to
 * ISO 639-1 (Bergamot's identifiers); if no map exists or confidence is low,
 * iso1 is null (caller should surface a "pick source language" UI).
 *
 * Short messages (< 10 chars) are inherently low-confidence; we cap returned
 * confidence at 0.4 in that range to force the manual picker.
 */
export function detect(text: string): DetectResult {
  const trimmed = text.trim();
  if (trimmed.length === 0) return { iso1: null, confidence: 0 };

  const iso3 = franc(trimmed, { minLength: 1 });
  if (iso3 === "und") return { iso1: null, confidence: 0 };

  const iso1 = iso3to1(iso3);
  // franc doesn't expose a per-call confidence in the simple API; use length-based heuristic.
  // For < 10 chars, accuracy is poor — cap confidence so callers fall back.
  const lenConfidence = trimmed.length < 10 ? 0.3 : trimmed.length < 30 ? 0.7 : 0.9;
  return { iso1, confidence: lenConfidence };
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/lib/translation/detect.ts
git -C /home/deez/farder commit -m "feat(client): translation detect.ts (franc wrapper)"
```

---

## Task 7: engine.ts (Bergamot WASM + Translator pool)

**Files:**
- Create: `client/src/lib/translation/engine.ts`

- [ ] **Step 1: Implement engine**

```ts
// client/src/lib/translation/engine.ts

import { convertFileSrc } from "@tauri-apps/api/core";
import {
  getModelPaths,
  listLocalModels,
  downloadModel,
} from "./api";
import type { LangPair } from "./types";

// @browsermt/bergamot-translator exposes a default export that loads the WASM module.
// The exact API (loadModel, translate) depends on the package version (0.4.9).
// This wrapper isolates the Bergamot surface.

interface BergamotTranslator {
  translate(text: string): Promise<string>;
  free(): void;
}

interface BergamotEngine {
  loadModel(opts: {
    src: string;
    trg: string;
    modelUrl: string;
    vocabUrl: string;
    lexUrl: string;
  }): Promise<BergamotTranslator>;
}

let enginePromise: Promise<BergamotEngine> | null = null;

async function loadEngine(): Promise<BergamotEngine> {
  if (!enginePromise) {
    enginePromise = (async () => {
      // The npm package exports a factory; we create a single instance for the session.
      // Refer to @browsermt/bergamot-translator README for the exact init API.
      const mod = await import("@browsermt/bergamot-translator");
      // The package's default export is the WASM module factory.
      // Newer wrappers may expose `.createTranslator()` / `.translateBatch()` etc.
      // This is intentionally a thin adapter that we can refine when running
      // against the real package.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const factory = (mod as any).default ?? mod;
      const engine = await factory();
      return engine as BergamotEngine;
    })();
  }
  return enginePromise;
}

const translatorPool = new Map<string, BergamotTranslator>();

function pairKey(pair: LangPair): string {
  return `${pair.src}-${pair.trg}`;
}

export async function ensureModel(
  pair: LangPair,
  onNotPresent: () => Promise<void>,  // called if model needs download (UI confirms)
): Promise<void> {
  const local = await listLocalModels();
  const present = local.some((m) => m.pair.src === pair.src && m.pair.trg === pair.trg);
  if (!present) {
    await onNotPresent();  // UI shows confirm + download progress
    await downloadModel(pair);
  }
}

export async function getOrCreateTranslator(pair: LangPair): Promise<BergamotTranslator> {
  const key = pairKey(pair);
  const cached = translatorPool.get(key);
  if (cached) return cached;

  const engine = await loadEngine();
  const paths = await getModelPaths(pair);
  const translator = await engine.loadModel({
    src: pair.src,
    trg: pair.trg,
    modelUrl: convertFileSrc(paths.model),
    vocabUrl: convertFileSrc(paths.vocab),
    lexUrl: convertFileSrc(paths.lex),
  });
  translatorPool.set(key, translator);
  return translator;
}

export async function translate(text: string, pair: LangPair): Promise<string> {
  const translator = await getOrCreateTranslator(pair);
  return translator.translate(text);
}

export function clearPool(): void {
  for (const t of translatorPool.values()) {
    t.free();
  }
  translatorPool.clear();
  enginePromise = null;
}
```

(Implementer note: `@browsermt/bergamot-translator` v0.4.9's exact API may diverge from the `BergamotEngine` interface above. Adjust the adapter to match the real package — the contract is "given three file URLs, return an object that exposes `translate(text) -> Promise<string>`". Read the package's README and tests for the precise factory shape; the package ships a `module/` directory with TypeScript-friendly examples. If the real API requires structured config blobs instead of URLs, fetch the file URLs via `fetch()` and pass the bytes.)

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors. (Some `any` casts in the bergamot adapter are intentional at this stage.)

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/lib/translation/engine.ts
git -C /home/deez/farder commit -m "feat(client): translation engine.ts (Bergamot WASM adapter + translator pool)"
```

---

## Task 8: store.ts (per-message translation state)

**Files:**
- Create: `client/src/lib/translation/store.ts`

- [ ] **Step 1: Implement store**

```ts
// client/src/lib/translation/store.ts

import { detect } from "./detect";
import { ensureModel, translate } from "./engine";
import type { TranslationStatus } from "./types";

type Listener = (state: Map<string, TranslationStatus>) => void;

const state = new Map<string, TranslationStatus>();
const listeners = new Set<Listener>();

function emit(): void {
  const snapshot = new Map(state);
  for (const l of listeners) l(snapshot);
}

export function subscribe(l: Listener): () => void {
  listeners.add(l);
  l(new Map(state));
  return () => { listeners.delete(l); };
}

export function getStatus(messageId: string): TranslationStatus {
  return state.get(messageId) ?? { kind: "idle" };
}

export function dismiss(messageId: string): void {
  state.delete(messageId);
  emit();
}

export interface TranslateOptions {
  messageId: string;
  content: string;
  defaultTarget: string;
  /**
   * Called when a model needs to be downloaded. The UI must show a privacy
   * disclosure dialog and either resolve (user accepted) or throw (user cancelled).
   */
  confirmDownload: (pair: { src: string; trg: string }) => Promise<void>;
}

export async function translateMessage(opts: TranslateOptions): Promise<void> {
  const { messageId, content, defaultTarget, confirmDownload } = opts;
  state.set(messageId, { kind: "detecting" });
  emit();

  const det = detect(content);
  if (det.iso1 === null || det.confidence < 0.5) {
    state.set(messageId, { kind: "low-confidence", suggested: det.iso1 ?? undefined });
    emit();
    return;
  }
  if (det.iso1 === defaultTarget) {
    state.set(messageId, { kind: "already-in-target", lang: defaultTarget });
    emit();
    return;
  }

  const pair = { src: det.iso1, trg: defaultTarget };
  state.set(messageId, { kind: "loading-model", src: pair.src, trg: pair.trg });
  emit();

  try {
    await ensureModel(pair, async () => {
      await confirmDownload(pair);
      // Note: actual progress events are emitted by the Rust side via `translation:progress`
      // and consumed by the UI. The store doesn't subscribe to them directly; the
      // download dialog component does and updates its own progress UI.
    });
    state.set(messageId, { kind: "translating", src: pair.src, trg: pair.trg });
    emit();
    const text = await translate(content, pair);
    state.set(messageId, { kind: "done", text, src: pair.src, trg: pair.trg });
    emit();
  } catch (err) {
    state.set(messageId, {
      kind: "error",
      reason: err instanceof Error ? err.message : String(err),
    });
    emit();
  }
}

/** Called when picking a source language manually (low-confidence path). */
export async function translateMessageWithSource(
  opts: TranslateOptions & { src: string },
): Promise<void> {
  const { messageId, content, src, defaultTarget, confirmDownload } = opts;
  if (src === defaultTarget) {
    state.set(messageId, { kind: "already-in-target", lang: defaultTarget });
    emit();
    return;
  }
  const pair = { src, trg: defaultTarget };
  state.set(messageId, { kind: "loading-model", src, trg: defaultTarget });
  emit();
  try {
    await ensureModel(pair, async () => { await confirmDownload(pair); });
    state.set(messageId, { kind: "translating", src, trg: defaultTarget });
    emit();
    const text = await translate(content, pair);
    state.set(messageId, { kind: "done", text, src, trg: defaultTarget });
    emit();
  } catch (err) {
    state.set(messageId, {
      kind: "error",
      reason: err instanceof Error ? err.message : String(err),
    });
    emit();
  }
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/lib/translation/store.ts
git -C /home/deez/farder commit -m "feat(client): translation store.ts (per-message state machine)"
```

---

## Phase 3: UI

## Task 9: TranslatedRow component

**Files:**
- Create: `client/src/components/TranslatedRow.tsx`

- [ ] **Step 1: Implement TranslatedRow**

```tsx
// client/src/components/TranslatedRow.tsx

import { useEffect, useState } from "react";
import { subscribe, dismiss, translateMessageWithSource } from "../lib/translation/store";
import { displayName } from "../lib/translation/lang";
import type { TranslationStatus } from "../lib/translation/types";

interface Props {
  messageId: string;
  content: string;
  defaultTarget: string;
  confirmDownload: (pair: { src: string; trg: string }) => Promise<void>;
}

export function TranslatedRow({ messageId, content, defaultTarget, confirmDownload }: Props) {
  const [status, setStatus] = useState<TranslationStatus>({ kind: "idle" });

  useEffect(() => {
    const unsub = subscribe((m) => {
      setStatus(m.get(messageId) ?? { kind: "idle" });
    });
    return unsub;
  }, [messageId]);

  if (status.kind === "idle") return null;

  return (
    <div
      className="translated-row"
      style={{
        marginTop: 4,
        padding: "4px 8px",
        fontSize: "0.9em",
        color: "var(--text-muted, #888)",
        borderLeft: "2px solid var(--accent, #888)",
        display: "flex",
        alignItems: "flex-start",
        gap: 8,
      }}
    >
      <div style={{ flex: 1 }}>
        {status.kind === "detecting" && <span>Detecting language…</span>}
        {status.kind === "loading-model" && (
          <span>Loading {displayName(status.src)} → {displayName(status.trg)} model…</span>
        )}
        {status.kind === "downloading-model" && (
          <span>
            Downloading {displayName(status.src)} model ({Math.round(status.progress.bytes_done / 1_000_000)}{" "}
            of {Math.round(status.progress.bytes_total / 1_000_000)} MB)…
          </span>
        )}
        {status.kind === "translating" && <span>Translating…</span>}
        {status.kind === "done" && (
          <>
            <div>{status.text}</div>
            <div style={{ fontSize: "0.8em", marginTop: 2 }}>
              ↳ Translated from {displayName(status.src)}
            </div>
          </>
        )}
        {status.kind === "already-in-target" && (
          <span>Already in {displayName(status.lang)}</span>
        )}
        {status.kind === "low-confidence" && (
          <span>
            Couldn't detect language.{" "}
            <button
              onClick={() => {
                const src = prompt("Source language code (en, es, zh, …)?", status.suggested ?? "en");
                if (src) {
                  translateMessageWithSource({
                    messageId,
                    content,
                    src,
                    defaultTarget,
                    confirmDownload,
                  });
                }
              }}
            >
              Pick source…
            </button>
          </span>
        )}
        {status.kind === "error" && (
          <span style={{ color: "var(--error, #c44)" }}>Translation failed: {status.reason}</span>
        )}
      </div>
      <button
        onClick={() => dismiss(messageId)}
        style={{ background: "none", border: "none", cursor: "pointer", color: "inherit" }}
        title="Dismiss translation"
      >
        ×
      </button>
    </div>
  );
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/TranslatedRow.tsx
git -C /home/deez/farder commit -m "feat(client): TranslatedRow component"
```

---

## Task 10: TranslationDownloadDialog (privacy disclosure + progress)

**Files:**
- Create: `client/src/components/TranslationDownloadDialog.tsx`

- [ ] **Step 1: Implement download dialog**

```tsx
// client/src/components/TranslationDownloadDialog.tsx

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { displayName } from "../lib/translation/lang";
import type { DownloadProgress } from "../lib/translation/types";

interface Props {
  pair: { src: string; trg: string };
  onConfirm: () => void;
  onCancel: () => void;
  inProgress: boolean;  // true once user accepted; show progress instead of confirm
}

export function TranslationDownloadDialog({ pair, onConfirm, onCancel, inProgress }: Props) {
  const [progress, setProgress] = useState<DownloadProgress | null>(null);

  useEffect(() => {
    if (!inProgress) return;
    const unlistenP = listen<DownloadProgress>("translation:progress", (evt) => {
      const p = evt.payload;
      if (p.pair.src === pair.src && p.pair.trg === pair.trg) {
        setProgress(p);
      }
    });
    return () => { unlistenP.then((u) => u()); };
  }, [inProgress, pair.src, pair.trg]);

  return (
    <div className="modal-overlay" style={overlayStyle}>
      <div className="modal" style={modalStyle}>
        <h3>Download translation model?</h3>
        <p>
          Downloading the {displayName(pair.src)} ↔ {displayName(pair.trg)} model from
          Mozilla's servers (storage.googleapis.com).
        </p>
        <p style={{ fontSize: "0.9em", color: "var(--text-muted, #888)" }}>
          Mozilla will see your IP address and the language pair you're requesting.
        </p>
        <p style={{ fontSize: "0.9em", color: "var(--text-muted, #888)" }}>
          Once downloaded, all translation runs entirely on your device — no chat
          content is ever sent anywhere.
        </p>
        {inProgress && progress && (
          <div style={{ margin: "12px 0" }}>
            <div>{progress.stage}: {progress.message ?? ""}</div>
            <progress
              value={progress.bytes_done}
              max={progress.bytes_total || 1}
              style={{ width: "100%" }}
            />
          </div>
        )}
        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", marginTop: 16 }}>
          <button onClick={onCancel} disabled={inProgress}>Cancel</button>
          <button onClick={onConfirm} disabled={inProgress}>Download</button>
        </div>
      </div>
    </div>
  );
}

const overlayStyle: React.CSSProperties = {
  position: "fixed", inset: 0,
  background: "rgba(0,0,0,0.5)",
  display: "flex", alignItems: "center", justifyContent: "center",
  zIndex: 1000,
};

const modalStyle: React.CSSProperties = {
  background: "var(--bg-elevated, #fff)",
  color: "var(--text, #000)",
  padding: 20,
  borderRadius: 8,
  maxWidth: 480,
  width: "90%",
};
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/TranslationDownloadDialog.tsx
git -C /home/deez/farder commit -m "feat(client): TranslationDownloadDialog (privacy + progress)"
```

---

## Task 11: Wire Translate into Message.tsx context menu

**Files:**
- Modify: `client/src/components/Message.tsx`

- [ ] **Step 1: Add Translate menu entry + state**

Find the existing context-menu block in `Message.tsx` (around line 465 — `setMenu({ x: e.clientX, y: e.clientY })`). Identify the menu's render function (the JSX block that lists Reply/Edit/Delete/Create Thread).

Add at the top of the component:

```tsx
import { TranslatedRow } from "./TranslatedRow";
import { TranslationDownloadDialog } from "./TranslationDownloadDialog";
import { translateMessage } from "../lib/translation/store";
import { getTranslationSettings } from "../lib/translation/api";
```

Add to the component's state:

```tsx
const [pendingDownload, setPendingDownload] = useState<{
  pair: { src: string; trg: string };
  resolve: () => void;
  reject: (reason: unknown) => void;
  inProgress: boolean;
} | null>(null);
```

In the menu's JSX (alongside existing `Reply` / `Edit` / `Delete` / `Create Thread` items), add a new menu item:

```tsx
<div
  className="menu-item"
  onClick={async () => {
    setMenu(null);
    const settings = await getTranslationSettings();
    if (!settings.enabled) return;
    await translateMessage({
      messageId: String(props.message.id),
      content: props.message.content,
      defaultTarget: settings.default_target,
      confirmDownload: async (pair) =>
        new Promise<void>((resolve, reject) => {
          setPendingDownload({ pair, resolve, reject, inProgress: false });
        }),
    });
  }}
>
  Translate
</div>
```

(Match the existing item's class/styling — copy the styling from the Reply or Create Thread item, do NOT use `className="menu-item"` literally if the file doesn't use that class.)

- [ ] **Step 2: Render TranslatedRow below the message bubble**

Find the JSX that renders the message text/content (look for where `props.message.content` is rendered). Immediately below that element, add:

```tsx
<TranslatedRow
  messageId={String(props.message.id)}
  content={props.message.content}
  defaultTarget={"en" /* read from settings cache; see step 3 */}
  confirmDownload={async (pair) =>
    new Promise<void>((resolve, reject) => {
      setPendingDownload({ pair, resolve, reject, inProgress: false });
    })
  }
/>
```

- [ ] **Step 3: Cache translation settings to avoid per-render IPC**

At the top of the component (or in a top-level App component if state should be global):

```tsx
const [translationSettings, setTranslationSettings] = useState<{ enabled: boolean; default_target: string } | null>(null);

useEffect(() => {
  getTranslationSettings().then((s) =>
    setTranslationSettings({ enabled: s.enabled, default_target: s.default_target })
  );
}, []);
```

Then use `translationSettings?.default_target ?? "en"` instead of the hardcoded `"en"` in step 2. Hide the menu item if `!translationSettings?.enabled`.

(For implementer: if Message.tsx renders many messages and per-message state churn is undesirable, lift the settings cache to a parent component or React context. For v1 a per-message useEffect is acceptable.)

- [ ] **Step 4: Render the download dialog when pendingDownload is set**

Below the message JSX (before the component's closing tag), add:

```tsx
{pendingDownload && (
  <TranslationDownloadDialog
    pair={pendingDownload.pair}
    inProgress={pendingDownload.inProgress}
    onCancel={() => {
      pendingDownload.reject(new Error("user cancelled"));
      setPendingDownload(null);
    }}
    onConfirm={() => {
      setPendingDownload({ ...pendingDownload, inProgress: true });
      pendingDownload.resolve();
      // Dialog stays mounted to show progress; cleared when status moves past "loading-model".
      // Simplest: clear after 30s timeout, or watch the store. For v1, rely on the parent
      // re-rendering from store transitions and dismissing the modal in a follow-up effect.
    }}
  />
)}

{/* Auto-dismiss the dialog when translation reaches "translating" or beyond. */}
{pendingDownload && pendingDownload.inProgress && (
  <DismissDialogWhenDone messageId={String(props.message.id)} onDone={() => setPendingDownload(null)} />
)}
```

Define the helper component below:

Add the import at the top of `Message.tsx`:

```tsx
import { subscribe as subscribeTranslation } from "../lib/translation/store";
```

```tsx
function DismissDialogWhenDone({ messageId, onDone }: { messageId: string; onDone: () => void }) {
  useEffect(() => {
    let active = true;
    const unsub = subscribeTranslation((m) => {
      if (!active) return;
      const s = m.get(messageId);
      if (s && (s.kind === "translating" || s.kind === "done" || s.kind === "error")) {
        onDone();
      }
    });
    return () => { active = false; unsub(); };
  }, [messageId, onDone]);
  return null;
}
```

(This dismissal pattern is functional but a bit awkward — consider lifting the dialog to a global App-level slot in a follow-up. For v1 it's acceptable.)

- [ ] **Step 5: Verify TS compiles + run dev**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -15
```

Expected: no errors. Visual smoke is in Task 16.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add client/src/components/Message.tsx
git -C /home/deez/farder commit -m "feat(client): right-click Translate + TranslatedRow + download dialog wiring"
```

---

## Task 12: Translation tab in Settings modal

**Files:**
- Create: `client/src/components/TranslationSettingsTab.tsx`
- Modify: `client/src/components/AppearanceSettings.tsx`

- [ ] **Step 1: Implement TranslationSettingsTab**

```tsx
// client/src/components/TranslationSettingsTab.tsx

import { useEffect, useState } from "react";
import {
  getTranslationSettings,
  setTranslationSettings,
  listLocalModels,
  listAvailablePairs,
  deleteModel,
  downloadModel,
} from "../lib/translation/api";
import { displayName, ISO_1_TO_3 } from "../lib/translation/lang";
import type { LocalModel, AvailablePair, TranslationSettings } from "../lib/translation/types";

export function TranslationSettingsTab() {
  const [settings, setSettings] = useState<TranslationSettings | null>(null);
  const [installed, setInstalled] = useState<LocalModel[]>([]);
  const [available, setAvailable] = useState<AvailablePair[]>([]);
  const [showAdd, setShowAdd] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);

  async function refresh() {
    setSettings(await getTranslationSettings());
    setInstalled(await listLocalModels());
    if (showAdd) setAvailable(await listAvailablePairs());
  }

  useEffect(() => { refresh(); }, [showAdd]);

  if (!settings) return <div>Loading…</div>;

  return (
    <div style={{ padding: 16 }}>
      <h2>Translation</h2>

      <label style={{ display: "block", margin: "12px 0" }}>
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

      <label style={{ display: "block", margin: "12px 0" }}>
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

      <h3 style={{ marginTop: 24 }}>Installed languages</h3>
      {installed.length === 0 && <p>No models installed yet.</p>}
      <ul>
        {installed.map((m) => (
          <li key={`${m.pair.src}-${m.pair.trg}`} style={{ margin: "6px 0" }}>
            {displayName(m.pair.src)} → {displayName(m.pair.trg)}
            {" "}({(m.disk_size_bytes / 1_000_000).toFixed(1)} MB)
            {" "}
            <button
              disabled={busy !== null}
              onClick={async () => {
                if (!confirm(`Delete ${displayName(m.pair.src)}→${displayName(m.pair.trg)} model?`)) return;
                setBusy("deleting");
                try { await deleteModel(m.pair); await refresh(); }
                finally { setBusy(null); }
              }}
            >Delete</button>
          </li>
        ))}
      </ul>

      <button onClick={() => setShowAdd(!showAdd)}>
        {showAdd ? "Hide available languages" : "+ Add language"}
      </button>

      {showAdd && (
        <ul style={{ maxHeight: 240, overflowY: "auto", border: "1px solid var(--border, #ccc)", padding: 8, marginTop: 8 }}>
          {available
            .filter((p) =>
              !installed.some((m) => m.pair.src === p.src && m.pair.trg === p.trg)
            )
            .map((p) => (
              <li key={`${p.src}-${p.trg}`} style={{ margin: "4px 0" }}>
                {displayName(p.src)} → {displayName(p.trg)}
                {" "}({(p.size_bytes / 1_000_000).toFixed(1)} MB)
                {" "}
                <button
                  disabled={busy !== null}
                  onClick={async () => {
                    setBusy(`downloading-${p.src}-${p.trg}`);
                    try { await downloadModel({ src: p.src, trg: p.trg }); await refresh(); }
                    catch (e) { alert(`Download failed: ${e}`); }
                    finally { setBusy(null); }
                  }}
                >
                  {busy === `downloading-${p.src}-${p.trg}` ? "Downloading…" : "Download"}
                </button>
              </li>
            ))}
        </ul>
      )}
    </div>
  );
}
```

- [ ] **Step 2: Add the tab to AppearanceSettings.tsx**

In `client/src/components/AppearanceSettings.tsx`:

Find:
```tsx
const [activeTab, setActiveTab] = useState<"appearance" | "gif">("appearance");
```

Replace with:
```tsx
const [activeTab, setActiveTab] = useState<"appearance" | "gif" | "translation">("appearance");
```

Find:
```tsx
{(["appearance", "gif"] as const).map((tab) => (
```

Replace with:
```tsx
{(["appearance", "gif", "translation"] as const).map((tab) => (
```

Find the tab-label ternary (around line 277 — `tab === "appearance" ? "Appearance" : "GIF Search"`).

Replace with:
```tsx
{tab === "appearance" ? "Appearance" : tab === "gif" ? "GIF Search" : "Translation"}
```

At the top of `AppearanceSettings.tsx`, add:
```tsx
import { TranslationSettingsTab } from "./TranslationSettingsTab";
```

Find the conditional rendering (`activeTab === "appearance" && (...)`); add a new sibling block:

```tsx
{activeTab === "translation" && <TranslationSettingsTab />}
```

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/components/TranslationSettingsTab.tsx client/src/components/AppearanceSettings.tsx
git -C /home/deez/farder commit -m "feat(client): Translation tab in Settings modal"
```

---

## Task 13: First-run modal

**Files:**
- Create: `client/src/components/TranslationFirstRunModal.tsx`
- Modify: a top-level App or layout component (likely `client/src/App.tsx`) to mount it

- [ ] **Step 1: Implement TranslationFirstRunModal**

```tsx
// client/src/components/TranslationFirstRunModal.tsx

import { useEffect, useState } from "react";
import {
  getTranslationSettings,
  setTranslationSettings,
  downloadModel,
} from "../lib/translation/api";
import { displayName } from "../lib/translation/lang";

const DEFAULT_OFFER = ["en", "es", "zh"];

export function TranslationFirstRunModal() {
  const [show, setShow] = useState(false);
  const [picks, setPicks] = useState<Set<string>>(new Set());
  const [downloading, setDownloading] = useState<string | null>(null);

  useEffect(() => {
    getTranslationSettings().then((s) => {
      if (!s.seen_first_run) setShow(true);
    });
  }, []);

  async function dismiss(): Promise<void> {
    const s = await getTranslationSettings();
    await setTranslationSettings({ ...s, seen_first_run: true });
    setShow(false);
  }

  async function downloadSelected(): Promise<void> {
    // Build pairs: each picked language ↔ defaultTarget. Default target is the first
    // picked or "en". For v1, we treat the first picked as the user's primary language.
    const target = Array.from(picks)[0] ?? "en";
    const others = Array.from(picks).filter((p) => p !== target);

    const s = await getTranslationSettings();
    await setTranslationSettings({ ...s, default_target: target });

    for (const other of others) {
      // Both directions
      for (const pair of [{ src: other, trg: target }, { src: target, trg: other }]) {
        setDownloading(`${pair.src}-${pair.trg}`);
        try { await downloadModel(pair); }
        catch (e) { console.error("download failed", pair, e); }
      }
    }
    setDownloading(null);
    await dismiss();
  }

  if (!show) return null;

  return (
    <div className="modal-overlay" style={{
      position: "fixed", inset: 0, background: "rgba(0,0,0,0.5)",
      display: "flex", alignItems: "center", justifyContent: "center", zIndex: 1100,
    }}>
      <div className="modal" style={{
        background: "var(--bg-elevated, #fff)", color: "var(--text, #000)",
        padding: 20, borderRadius: 8, maxWidth: 480, width: "90%",
      }}>
        <h2>Translation</h2>
        <p>
          Farder can translate messages between languages, on your device — no
          chat content is ever sent anywhere.
        </p>
        <p>
          Pick the languages you want to translate to and from. Each is ~50 MB
          and is downloaded from Mozilla's servers.
        </p>
        {DEFAULT_OFFER.map((iso) => (
          <label key={iso} style={{ display: "block", margin: "6px 0" }}>
            <input
              type="checkbox"
              checked={picks.has(iso)}
              onChange={(e) => {
                const next = new Set(picks);
                if (e.target.checked) next.add(iso); else next.delete(iso);
                setPicks(next);
              }}
            />
            {" "}{displayName(iso)}
          </label>
        ))}
        {downloading && (
          <p style={{ fontSize: "0.9em", color: "var(--text-muted, #888)" }}>
            Downloading {downloading}…
          </p>
        )}
        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", marginTop: 16 }}>
          <button onClick={dismiss} disabled={downloading !== null}>
            Skip — I don't need translation right now
          </button>
          <button onClick={downloadSelected} disabled={picks.size === 0 || downloading !== null}>
            Download selected
          </button>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Mount it in the App**

Find the top-level layout component (probably `client/src/App.tsx` or `client/src/components/AppShell.tsx`). Add the import:

```tsx
import { TranslationFirstRunModal } from "./components/TranslationFirstRunModal";
```

(Adjust import path to match the file's location.)

Add `<TranslationFirstRunModal />` somewhere inside the JSX render — at the top level so it overlays everything when shown:

```tsx
return (
  <>
    <TranslationFirstRunModal />
    {/* …existing layout… */}
  </>
);
```

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/components/TranslationFirstRunModal.tsx client/src/App.tsx
git -C /home/deez/farder commit -m "feat(client): translation first-run modal"
```

(Adjust commit's file list to match where the modal got mounted — App.tsx vs AppShell.tsx vs whatever the existing pattern is.)

---

## Phase 4: Polish + smoke test

## Task 14: Smoke test + CHANGELOG

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Run cargo + tsc + smoke**

```
cd /home/deez/farder && cargo check -p farder-client 2>&1 | tail -5
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5
```

Expected: `Finished` and no TS errors.

- [ ] **Step 2: Run the client + manual smoke (Alice + Bob)**

In one terminal:
```
cd /home/deez/farder/client && npm run tauri dev
```

In another (after `cargo build --release` in `client/src-tauri`):
```
FARDER_DATA=/tmp/farder-bob /home/deez/farder/client/src-tauri/target/release/farder-client
```

Manual checklist:
- [ ] First-run modal appears on first Alice launch. Pick English + Spanish + Chinese → models download → modal closes.
- [ ] Bob sends a Spanish message in a shared channel. Alice right-clicks → Translate → English appears below within ~1s.
- [ ] Settings → Translation tab shows the 3 installed pairs (× 2 directions). Delete one → re-translate triggers re-download dialog with privacy copy.
- [ ] Bob sends an English message. Alice translates → "Already in English" muted line.
- [ ] Disable translation in Settings → right-click menu no longer shows Translate.
- [ ] Toggle re-enable → menu item returns.
- [ ] Translate a 1-word message ("hi") → "Couldn't detect language" with manual picker.

- [ ] **Step 3: Update CHANGELOG.md**

Add a new entry under `### Added` (top of the section, just below the existing 2026-05-07 voice phase 2 entry):

```markdown
- (2026-05-07) Message translation v1: right-click any chat message → "Translate" → translated text appears below the original. Runs entirely on the user's device via Bergamot WASM (the same engine Firefox Translate uses); no chat content ever leaves the device. Source language is auto-detected (`franc-min`); models are fetched on demand from Mozilla's public CDN with an explicit privacy disclosure that the model download exposes the user's IP and language pair to Mozilla. First-run modal lets the user pick which of English/Spanish/Chinese to download (or skip entirely — zero footprint). Settings → Translation tab manages installed languages with delete + add. Defers per-channel auto-translate and server-side translation to v1.5.
```

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add CHANGELOG.md
git -C /home/deez/farder commit -m "docs: changelog entry for translation v1"
```

---

## Self-review notes

- Spec coverage: every section of the spec maps to a task — Rust commands (Tasks 1-4), TS engine library (Tasks 5-8), UI components (Tasks 9-13), smoke + CHANGELOG (Task 14). Auto-translate and server-side are explicitly out of scope per spec; no tasks for them.
- The `BergamotEngine` interface in Task 7 is a documented adapter point — the implementer is told to refine against the real package's API. This is the one part of the plan most likely to require adjustment during implementation.
- The download-dialog auto-dismiss in Task 11 step 4 is intentionally pragmatic ("admittedly ugly"). Cleanup is flagged as follow-up rather than gating v1.
- macOS is explicitly out of scope per the spec; nothing in the plan depends on macOS-specific behavior.
- Test coverage is Rust-side unit tests + manual smoke on the TS side, matching the existing project convention (no Vitest infra exists).
