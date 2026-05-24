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
    pub model: String,
    pub vocab: String,
    pub lex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationSettings {
    pub enabled: bool,
    pub default_target: String,
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

fn validate_pair(pair: &LangPair) -> Result<(), String> {
    fn ok(s: &str) -> bool {
        let len = s.len();
        (2..=8).contains(&len) && s.chars().all(|c| c.is_ascii_lowercase())
    }
    if !ok(&pair.src) || !ok(&pair.trg) {
        return Err(format!("invalid language pair: {}-{}", pair.src, pair.trg));
    }
    Ok(())
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

const REGISTRY_URL: &str = "https://storage.googleapis.com/moz-fx-translations-data--303e-prod-translations-data/db/models.json";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryEntry {
    pub source_language: String,
    pub target_language: String,
    pub architecture: String,
    pub release_status: Option<String>,
    pub files: RegistryFiles,
    #[serde(default)]
    pub model_statistics: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryFiles {
    pub model: RegistryFile,
    pub vocab: RegistryFile,
    pub lexical_shortlist: RegistryFile,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryFile {
    pub path: String,
    #[serde(default)]
    pub uncompressed_size: Option<u64>,
    #[serde(default)]
    pub uncompressed_hash: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryRoot {
    base_url: String,
    models: std::collections::HashMap<String, Vec<RegistryEntry>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AvailablePair {
    pub src: String,
    pub trg: String,
    pub size_bytes: u64,
    pub display_name: String,
}

async fn fetch_registry() -> Result<RegistryRoot, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(REGISTRY_URL).send().await.map_err(|e| e.to_string())?;
    let text = resp.text().await.map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| format!("registry parse: {e}"))
}

/// Pick the best registry entry for a pair: prefer `releaseStatus == "Release"`,
/// then prefer `architecture == "base-memory"`. Fall back to first available.
fn pick_entry(entries: &[RegistryEntry]) -> Option<&RegistryEntry> {
    let released: Vec<&RegistryEntry> = entries
        .iter()
        .filter(|e| e.release_status.as_deref() == Some("Release"))
        .collect();
    if released.is_empty() {
        return entries.first();
    }
    released
        .iter()
        .find(|e| e.architecture == "base-memory")
        .copied()
        .or_else(|| released.first().copied())
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

    validate_pair(&pair)?;
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
        std::fs::remove_dir_all(&tmp_dir).map_err(|e| format!("remove stale tmp: {e}"))?;
    }
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("create tmp dir: {e}"))?;

    // Helper: clean up the staging dir before propagating any error.
    let cleanup = |e: String| -> String {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        e
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| cleanup(e.to_string()))?;

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
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| cleanup(format!("fetch {slot}: {e}")))?;
        let compressed = resp
            .bytes()
            .await
            .map_err(|e| cleanup(format!("read {slot}: {e}")))?;
        emit("decompressing", cumulative, Some(slot));
        let mut decoder = flate2::read::GzDecoder::new(&compressed[..]);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .map_err(|e| cleanup(format!("gunzip {slot}: {e}")))?;

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
        let mut f = std::fs::File::create(tmp_dir.join(slot))
            .map_err(|e| cleanup(format!("create {slot}: {e}")))?;
        f.write_all(&out)
            .map_err(|e| cleanup(format!("write {slot}: {e}")))?;
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
        .map_err(|e| cleanup(format!("write meta.json: {e}")))?;

    // Atomic rename
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| cleanup(e.to_string()))?;
    }
    std::fs::rename(&tmp_dir, &dir).map_err(|e| cleanup(format!("rename tmp -> final: {e}")))?;

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
            Err(e) => {
                eprintln!("[translation] skipping {}: {}", path.display(), e);
                continue;
            }
        };
        let meta: serde_json::Value = match serde_json::from_slice(&meta_bytes) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[translation] skipping {}: invalid meta.json: {}", path.display(), e);
                continue;
            }
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
    validate_pair(&pair)?;
    let dir = pair_dir(&pair);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn get_model_paths(pair: LangPair) -> Result<ModelPaths, String> {
    validate_pair(&pair)?;
    let dir = pair_dir(&pair);
    if !dir.exists() {
        return Err(format!("model not present: {}-{}", pair.src, pair.trg));
    }
    let to_path = |p: std::path::PathBuf| -> String {
        // absolute path; TS resolves via `convertFileSrc` on the renderer side.
        p.to_string_lossy().to_string()
    };
    Ok(ModelPaths {
        model: to_path(dir.join("model.bin")),
        vocab: to_path(dir.join("vocab.spm")),
        lex: to_path(dir.join("lex.bin")),
    })
}

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
        assert_eq!(picked.architecture, "base");
    }

    // NOTE: This test mutates FARDER_DATA process-globally. New tests in this
    // module that depend on the real ~/.farder path must not run concurrently
    // with this one. Cargo runs tests in parallel within a binary; if collisions
    // appear, consider serial_test or factoring list_local_models to take an
    // explicit base dir.
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

    #[test]
    fn validate_pair_rejects_traversal_and_garbage() {
        let bad_inputs = [
            ("..", "en"),
            ("../foo", "en"),
            ("en", "/etc"),
            ("EN", "es"),       // uppercase rejected
            ("e1", "es"),       // digits rejected
            ("", "es"),         // empty rejected
            ("toolongtoolong", "es"),
        ];
        for (s, t) in bad_inputs {
            let p = LangPair { src: s.into(), trg: t.into() };
            assert!(validate_pair(&p).is_err(), "should reject {s:?}/{t:?}");
        }
        let good = LangPair { src: "en".into(), trg: "es".into() };
        assert!(validate_pair(&good).is_ok());
        let good3 = LangPair { src: "ces".into(), trg: "eng".into() };  // ISO 639-3 also allowed
        assert!(validate_pair(&good3).is_ok());
    }
}
