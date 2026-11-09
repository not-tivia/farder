//! The buddy list and its chat windows, in the style of MSN Messenger / Skype.
//!
//! - The BUDDY LIST (`buddy`) is one small window listing your people (direct
//!   messages) and channels, with an unread count beside each.
//! - Double-clicking an entry opens a CHAT WINDOW (`chat-<hash>-<channel>`): one
//!   small window per conversation, showing the recent messages and a box to reply.
//!
//! Like the notification pop-up and the edge strip, these windows only DRAW what
//! the main window pushes to them (`buddy:state`, `chat:state`) and only ASK it for
//! things (open, mark read, send a line via `notify:reply`). The main window owns
//! every conversation, so none of the rules about encryption or what may be sent
//! live here. They have normal OS title bars, take focus like any window, hold text
//! only in memory and make no network request.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub const BUDDY_LABEL: &str = "buddy";

/// Logical pixels.
pub const BUDDY_W: f64 = 250.0;
pub const BUDDY_H: f64 = 480.0;
pub const CHAT_W: f64 = 360.0;
pub const CHAT_H: f64 = 340.0;

const SETTINGS_KEY: &str = "buddy_list";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BuddySettings {
    /// Open the buddy list when Farder starts. Off by default: it is an extra window.
    pub open_at_start: bool,
}

impl Default for BuddySettings {
    fn default() -> Self {
        BuddySettings { open_at_start: false }
    }
}

pub fn load() -> BuddySettings {
    crate::commands::settings_get(SETTINGS_KEY)
        .and_then(|v| serde_json::from_value::<BuddySettings>(v).ok())
        .unwrap_or_default()
}

/// A stable 32-bit hash (FNV-1a). Not for security: it only turns a server id of
/// any length and characters into eight hex digits that are safe in a window label.
fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// The label of the chat window for a conversation: `chat-<8 hex>-<channel id>`.
/// The same conversation always gets the same label, so a second double-click
/// finds the window already open instead of making another.
pub fn chat_label(server_id: &str, channel_id: u64) -> String {
    format!("chat-{:08x}-{}", fnv1a(server_id), channel_id)
}

/// A window title the person sees: printable, single line, not absurdly long.
fn clean_title(raw: &str) -> String {
    let t: String = raw.chars().filter(|c| !c.is_control()).take(60).collect();
    let t = t.trim();
    if t.is_empty() { "Farder chat".to_string() } else { t.to_string() }
}

/// Build the buddy list window if it is not there; otherwise bring it forward.
pub fn open_buddy_window(app: &AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(BUDDY_LABEL) {
        let _ = w.unminimize();
        let _ = w.show();
        return w.set_focus().map_err(|e| e.to_string());
    }
    WebviewWindowBuilder::new(app, BUDDY_LABEL, WebviewUrl::App("index.html?buddy=1".into()))
        .title("Farder")
        .inner_size(BUDDY_W, BUDDY_H)
        .min_inner_size(190.0, 220.0)
        .resizable(true)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn get_buddy_settings() -> BuddySettings {
    load()
}

#[tauri::command]
pub fn set_buddy_settings(settings: BuddySettings) -> Result<BuddySettings, String> {
    let value = serde_json::to_value(&settings).map_err(|e| e.to_string())?;
    crate::commands::settings_set(SETTINGS_KEY, value)?;
    Ok(settings)
}

/// Open (or bring forward) the buddy list.
#[tauri::command]
pub fn open_buddy_list(app: AppHandle) -> Result<(), String> {
    open_buddy_window(&app)
}

/// Open (or bring forward) the chat window for one conversation. `title` only
/// names the window; what it shows comes from the main window, which checks that
/// the conversation exists.
#[tauri::command]
pub fn chat_open(app: AppHandle, server_id: String, channel_id: u64, title: String) -> Result<(), String> {
    if server_id.is_empty() || server_id.len() > 128 {
        return Err("bad server id".into());
    }
    let label = chat_label(&server_id, channel_id);
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.unminimize();
        let _ = w.show();
        return w.set_focus().map_err(|e| e.to_string());
    }
    // The ids ride in the page address and are read back by the page; encode them.
    let url = format!(
        "index.html?chat=1&s={}&c={}",
        urlencode(&server_id),
        channel_id
    );
    WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url.into()))
        .title(clean_title(&title))
        .inner_size(CHAT_W, CHAT_H)
        .min_inner_size(240.0, 200.0)
        .resizable(true)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Percent-encode everything except unreserved characters.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_conversation_always_gets_the_same_label() {
        assert_eq!(chat_label("srv-a", 7), chat_label("srv-a", 7));
        assert_ne!(chat_label("srv-a", 7), chat_label("srv-a", 8));
        assert_ne!(chat_label("srv-a", 7), chat_label("srv-b", 7));
    }

    #[test]
    fn a_label_is_only_ever_safe_characters_whatever_the_server_id() {
        // The page checks labels with /^chat-[0-9a-f]{8}-\d{1,15}$/ (buddy.ts);
        // pin the shape so the two cannot drift apart.
        for id in ["abc", "has spaces & symbols/../..", "日本語", &"x".repeat(128)] {
            let l = chat_label(id, 123456789);
            let parts: Vec<&str> = l.split('-').collect();
            assert_eq!(parts.len(), 3, "{l}");
            assert_eq!(parts[0], "chat");
            assert_eq!(parts[1].len(), 8);
            assert!(parts[1].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            assert!(parts[2].chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn the_hash_is_the_published_fnv1a_value() {
        // Known FNV-1a (32-bit) test vectors, so a "tidy-up" cannot change every label.
        assert_eq!(fnv1a(""), 0x811c_9dc5);
        assert_eq!(fnv1a("a"), 0xe40c_292c);
        assert_eq!(fnv1a("foobar"), 0xbf9c_f968);
    }

    #[test]
    fn titles_are_single_line_printable_and_bounded() {
        assert_eq!(clean_title("Mara"), "Mara");
        assert_eq!(clean_title("a\nb\u{7}c"), "abc");
        assert_eq!(clean_title("   "), "Farder chat");
        assert_eq!(clean_title(&"x".repeat(500)).chars().count(), 60);
    }

    #[test]
    fn ids_are_percent_encoded_for_the_page_address() {
        assert_eq!(urlencode("abc-123_x.y~"), "abc-123_x.y~");
        assert_eq!(urlencode("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
    }

    #[test]
    fn settings_default_to_off_and_round_trip() {
        let d = BuddySettings::default();
        assert!(!d.open_at_start);
        let back: BuddySettings = serde_json::from_value(serde_json::to_value(&d).unwrap()).unwrap();
        assert_eq!(back, d);
        assert!(serde_json::from_value::<BuddySettings>(serde_json::json!({})).is_err());
    }
}
