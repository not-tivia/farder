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
}
