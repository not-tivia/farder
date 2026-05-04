use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const DEFAULT_THEME_ID: &str = "xp-luna-blue";

// Built-in themes: CSS + metadata embedded at compile time.
// Add a new built-in by creating client/src/themes/<id>/{theme.css,theme.json}
// and adding an entry to BUILTIN_THEMES below. The id comes from theme.json.
struct BuiltinTheme {
    css: &'static str,
    meta_json: &'static str,
}

const BUILTIN_THEMES: &[BuiltinTheme] = &[
    BuiltinTheme {
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
    // TODO: migrate to tauri-plugin-opener when we add it as a dep — Shell::open
    // is deprecated in tauri-plugin-shell v2 in favor of the dedicated opener plugin.
    #[allow(deprecated)]
    use tauri_plugin_shell::ShellExt;
    ensure_user_themes_dir();
    let path = user_themes_dir();
    #[allow(deprecated)]
    app.shell()
        .open(path.to_string_lossy().to_string(), None)
        .map_err(|e| e.to_string())
}

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
