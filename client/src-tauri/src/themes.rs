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
    BuiltinTheme {
        css: include_str!("../../src/themes/discord-dark/theme.css"),
        meta_json: include_str!("../../src/themes/discord-dark/theme.json"),
    },
    BuiltinTheme {
        css: include_str!("../../src/themes/hello-kitty/theme.css"),
        meta_json: include_str!("../../src/themes/hello-kitty/theme.json"),
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

/// Sanitize a user-supplied id into a filesystem-safe folder name.
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
    let trimmed = cleaned.trim_matches('-').to_string();
    if trimmed.is_empty() || !trimmed.chars().any(|c| c.is_ascii_alphanumeric()) {
        return Err("theme id cannot be empty after sanitization".to_string());
    }
    Ok(trimmed)
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
pub fn fork_theme(base_id: String, new_id: String, name: String) -> Result<String, String> {
    let safe_new_id = sanitize_theme_id(&new_id)?;

    let target_dir = user_themes_dir().join(&safe_new_id);
    if target_dir.exists() {
        return Err(format!("a theme with id '{}' already exists", safe_new_id));
    }

    let base_css = all_themes()
        .into_iter()
        .find(|(m, _)| m.id == base_id)
        .map(|(_, css)| css)
        .ok_or_else(|| format!("base theme not found: {}", base_id))?;

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

#[tauri::command]
pub fn open_themes_folder(_app: tauri::AppHandle) -> Result<(), String> {
    use std::process::Command;
    ensure_user_themes_dir();
    let path = user_themes_dir();

    // Spawn the platform's native file-browser opener. We avoid
    // tauri-plugin-shell::Shell::open both because it's deprecated and
    // because on WSL it'd try to invoke xdg-open, which usually isn't
    // installed — silently failing.
    #[cfg(target_os = "linux")]
    {
        if is_wsl() {
            // WSL: use Windows Explorer with the \\wsl.localhost\... path it understands.
            let win_path = wsl_to_windows_path(&path)?;
            // explorer.exe returns exit code 1 even on success; spawn-and-forget.
            return Command::new("explorer.exe")
                .arg(win_path)
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("explorer.exe failed: {}", e));
        }
        return Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("xdg-open failed: {}", e));
    }
    #[cfg(target_os = "macos")]
    {
        return Command::new("open")
            .arg(&path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("open failed: {}", e));
    }
    #[cfg(target_os = "windows")]
    {
        return Command::new("explorer")
            .arg(&path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("explorer failed: {}", e));
    }
}

#[cfg(target_os = "linux")]
fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/version")
        .map(|s| s.to_lowercase().contains("microsoft"))
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn wsl_to_windows_path(path: &std::path::Path) -> Result<String, String> {
    let output = std::process::Command::new("wslpath")
        .arg("-w")
        .arg(path)
        .output()
        .map_err(|e| format!("wslpath failed: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "wslpath returned non-zero: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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
        assert!(dir.join("theme.css").exists());
        assert!(dir.join("theme.json").exists());

        let written = std::fs::read_to_string(dir.join("theme.css")).unwrap();
        assert!(written.contains("--xp-blue"));

        let raw = std::fs::read_to_string(dir.join("theme.json")).unwrap();
        let meta = parse_meta(&raw, "user").expect("parse_meta failed");
        assert_eq!(meta.id, "my-custom");
        assert_eq!(meta.name, "My Custom Theme");
        assert_eq!(meta.author, "you");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fork_theme_refuses_to_overwrite() {
        let tmp = std::env::temp_dir().join(format!("farder-fork-overwrite-{}", std::process::id()));
        std::env::set_var("FARDER_DATA", &tmp);

        let _ = fork_theme("xp-luna-blue".to_string(), "dup".to_string(), "Dup".to_string()).unwrap();
        let second = fork_theme("xp-luna-blue".to_string(), "dup".to_string(), "Dup 2".to_string());
        assert!(second.is_err());

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
}
