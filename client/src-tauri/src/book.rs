use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

const BOOK_DIR: &str = "book";
const FILES_DIR: &str = "files";
const INDEX_FILE: &str = "items.json";
const MAX_BOOK_BYTES: u64 = 2 * 1024 * 1024;
const ALLOWED_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

#[derive(Serialize, Deserialize, Clone)]
pub struct BookItem {
    pub id: String,
    pub name: String,
    pub ext: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub animated: bool,
    pub added_at: u64,
    pub source: String, // "upload" | "chat" | "favorites-migration"
    #[serde(default)]
    pub server_files: HashMap<String, u64>,
}

fn book_root() -> PathBuf {
    crate::commands::farder_data_dir_pub().join(BOOK_DIR)
}

fn files_dir() -> PathBuf {
    book_root().join(FILES_DIR)
}

fn index_path() -> PathBuf {
    book_root().join(INDEX_FILE)
}

fn ensure_dirs() -> Result<(), String> {
    std::fs::create_dir_all(files_dir())
        .map_err(|e| format!("create book dirs failed: {}", e))
}

fn load_index() -> Vec<BookItem> {
    std::fs::read_to_string(index_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<BookItem>>(&s).ok())
        .unwrap_or_default()
}

fn save_index(items: &[BookItem]) -> Result<(), String> {
    ensure_dirs()?;
    let pretty = serde_json::to_string_pretty(items).map_err(|e| e.to_string())?;
    std::fs::write(index_path(), pretty).map_err(|e| format!("write items.json failed: {}", e))
}

fn sanitize_name(input: &str) -> String {
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
    let trimmed: String = cleaned.trim_matches(|c| c == '_' || c == '-').chars().take(32).collect();
    trimmed
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn detect_animated(data: &[u8], ext: &str) -> bool {
    match ext {
        "gif" => true, // any GIF over 1 frame is animated; cheap conservative default
        "webp" => {
            if data.len() < 30 {
                return false;
            }
            let mut pos = 12;
            while pos + 8 <= data.len() {
                if &data[pos..pos + 4] == b"ANIM" {
                    return true;
                }
                let size = u32::from_le_bytes([
                    data[pos + 4],
                    data[pos + 5],
                    data[pos + 6],
                    data[pos + 7],
                ]) as usize;
                pos = pos.saturating_add(8).saturating_add(size).saturating_add(size & 1);
            }
            false
        }
        _ => false,
    }
}

#[tauri::command]
pub fn book_list_items() -> Vec<BookItem> {
    load_index()
}

#[tauri::command]
pub fn book_upload_item(source_path: String, name: Option<String>) -> Result<BookItem, String> {
    let src = std::path::Path::new(&source_path);
    let bytes = std::fs::read(src).map_err(|e| format!("read source failed: {}", e))?;
    if bytes.len() as u64 > MAX_BOOK_BYTES {
        return Err(format!(
            "image too large ({} bytes; max {} bytes)",
            bytes.len(),
            MAX_BOOK_BYTES
        ));
    }

    let ext = src
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .ok_or_else(|| "image has no extension".to_string())?;
    if !ALLOWED_EXTS.contains(&ext.as_str()) {
        return Err(format!(
            "unsupported extension '{}' (allowed: {})",
            ext,
            ALLOWED_EXTS.join(", ")
        ));
    }

    let (width, height) = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .map(|(w, h)| (Some(w), Some(h)))
        .unwrap_or((None, None));

    let id = uuid::Uuid::new_v4().to_string();
    let resolved_name = match name {
        Some(n) if !n.trim().is_empty() => sanitize_name(n.trim()),
        _ => {
            let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("emoji");
            sanitize_name(stem)
        }
    };
    if resolved_name.is_empty() {
        return Err("could not derive a valid name from the file".to_string());
    }

    let animated = detect_animated(&bytes, &ext);

    ensure_dirs()?;
    let target_path = files_dir().join(format!("{}.{}", id, ext));
    std::fs::write(&target_path, &bytes).map_err(|e| format!("write image failed: {}", e))?;

    let item = BookItem {
        id,
        name: resolved_name,
        ext,
        width,
        height,
        animated,
        added_at: now_secs(),
        source: "upload".to_string(),
        server_files: HashMap::new(),
    };

    let mut items = load_index();
    items.push(item.clone());
    save_index(&items)?;

    Ok(item)
}

#[tauri::command]
pub fn book_delete_item(id: String) -> Result<(), String> {
    let mut items = load_index();
    let pos = items.iter().position(|i| i.id == id).ok_or_else(|| format!("item not found: {}", id))?;
    let removed = items.remove(pos);
    save_index(&items)?;
    let file = files_dir().join(format!("{}.{}", removed.id, removed.ext));
    let _ = std::fs::remove_file(file);
    Ok(())
}

#[tauri::command]
pub fn book_rename_item(id: String, new_name: String) -> Result<BookItem, String> {
    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        return Err("name cannot be empty".to_string());
    }
    let sanitized = sanitize_name(trimmed);
    if sanitized.is_empty() {
        return Err("name became empty after sanitization".to_string());
    }
    let mut items = load_index();
    let item = items
        .iter_mut()
        .find(|i| i.id == id)
        .ok_or_else(|| format!("item not found: {}", id))?;
    item.name = sanitized;
    let snapshot = item.clone();
    save_index(&items)?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_tmp_data_dir() -> PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "farder-book-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::env::set_var("FARDER_DATA", &tmp);
        tmp
    }

    fn tiny_png() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89,
            0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54,
            0x78, 0x9C, 0x62, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x01,
            0x0D, 0x0A, 0x2D, 0xB4,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44,
            0xAE, 0x42, 0x60, 0x82,
        ]
    }

    #[test]
    fn upload_then_list_then_delete() {
        let tmp = fresh_tmp_data_dir();
        let src = tmp.join("test.png");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(&src, tiny_png()).unwrap();
        let item = book_upload_item(src.to_string_lossy().to_string(), Some("My Cat".to_string())).unwrap();
        assert_eq!(item.name, "my-cat");
        assert_eq!(item.ext, "png");
        assert_eq!(book_list_items().len(), 1);
        book_delete_item(item.id.clone()).unwrap();
        assert_eq!(book_list_items().len(), 0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rename_changes_name_in_index() {
        let tmp = fresh_tmp_data_dir();
        let src = tmp.join("test.png");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(&src, tiny_png()).unwrap();
        let item = book_upload_item(src.to_string_lossy().to_string(), Some("a".to_string())).unwrap();
        book_rename_item(item.id.clone(), "Renamed Thing".to_string()).unwrap();
        let items = book_list_items();
        assert_eq!(items[0].name, "renamed-thing");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rejects_unsupported_extension() {
        let tmp = fresh_tmp_data_dir();
        let src = tmp.join("test.exe");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(&src, vec![0u8; 100]).unwrap();
        let result = book_upload_item(src.to_string_lossy().to_string(), None);
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
