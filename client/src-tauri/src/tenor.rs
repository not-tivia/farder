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
