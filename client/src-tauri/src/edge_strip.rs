//! The edge strip: a one-line, always-on-top bar docked to the top or bottom of
//! the screen, for leaving Farder open beside a game or a document.
//!
//! At rest it shows who has written to you; when a message arrives it shows a
//! one-line preview with a reply box. It is the same idea as the notification
//! pop-up (`notify_popup.rs`) and follows the same rules:
//! - it never takes keyboard focus, except for the moment you open its reply box;
//! - it holds the text only in memory and makes no network request;
//! - it never sends anything itself. A reply is only an ask (`notify:reply`) that
//!   the main window checks and sends, exactly as for the pop-up.
//!
//! The main window owns the state (which conversations have unread messages, what
//! the preview says, and how much of it the person's privacy setting lets through)
//! and pushes it as `strip:state`; this window just draws it. The page reports
//! whether it has anything to show (`strip_layout`) and the window is placed and
//! shown or hidden to match, so an empty strip never sits over other programs.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};

pub const LABEL: &str = "strip";

/// Logical pixels. Must match `.edge-strip` in the theme CSS.
pub const HEIGHT: f64 = 30.0;
/// Gap between the strip and the screen edge it is docked to.
pub const MARGIN: f64 = 6.0;
pub const MIN_WIDTH: u32 = 280;
pub const MAX_WIDTH: u32 = 900;
pub const DEFAULT_WIDTH: u32 = 460;

const SETTINGS_KEY: &str = "edge_strip";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StripSettings {
    /// Off by default: it is an opt-in extra window.
    pub enabled: bool,
    /// "top" or "bottom".
    pub dock: String,
    /// "left", "center" or "right".
    pub align: String,
    /// Logical pixels, MIN_WIDTH..=MAX_WIDTH.
    pub width: u32,
    /// Keep the strip on screen even with nothing unread (otherwise it only
    /// appears when there is something to show).
    pub always_visible: bool,
}

impl Default for StripSettings {
    fn default() -> Self {
        StripSettings { enabled: false, dock: "top".into(), align: "center".into(), width: DEFAULT_WIDTH, always_visible: false }
    }
}

/// Clamp whatever was stored (or sent by the page) into values the layout code
/// can trust. Unknown words fall back to the defaults.
pub fn normalize(s: StripSettings) -> StripSettings {
    StripSettings {
        enabled: s.enabled,
        dock: if s.dock == "bottom" { "bottom".into() } else { "top".into() },
        align: match s.align.as_str() {
            "left" => "left".into(),
            "right" => "right".into(),
            _ => "center".into(),
        },
        width: s.width.clamp(MIN_WIDTH, MAX_WIDTH),
        always_visible: s.always_visible,
    }
}

pub fn load() -> StripSettings {
    crate::commands::settings_get(SETTINGS_KEY)
        .and_then(|v| serde_json::from_value::<StripSettings>(v).ok())
        .map(normalize)
        .unwrap_or_default()
}

#[derive(Debug, PartialEq)]
pub struct Layout {
    /// Physical pixels.
    pub x: i32,
    pub y: i32,
    /// Logical pixels.
    pub width: f64,
    pub height: f64,
}

/// Where the strip goes. Pure so it can be tested without a display.
/// `work_*` is the monitor's WORK AREA in physical pixels (the screen minus the
/// taskbar), so a bottom strip sits above the taskbar, not under it.
pub fn layout_for(s: &StripSettings, work_x: i32, work_y: i32, work_w: u32, work_h: u32, scale: f64) -> Layout {
    let s = normalize(s.clone());
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let margin = (MARGIN * scale).round() as i32;
    // Never wider than the screen it is on.
    let fit = ((work_w as f64 / scale) - 2.0 * MARGIN).max(1.0);
    let width = (s.width as f64).min(fit);
    let w_phys = (width * scale).round() as i32;
    let h_phys = (HEIGHT * scale).round() as i32;
    let x = match s.align.as_str() {
        "left" => work_x + margin,
        "right" => work_x + work_w as i32 - w_phys - margin,
        _ => work_x + (work_w as i32 - w_phys) / 2,
    };
    let y = if s.dock == "bottom" { work_y + work_h as i32 - h_phys - margin } else { work_y + margin };
    Layout { x, y, width, height: HEIGHT }
}

/// Build the window if it does not exist. Hidden and unfocusable from birth so it
/// can never steal a keystroke. Opaque (not transparent): the strip has no
/// see-through padding, so there is nothing for Windows to outline.
pub fn ensure_window(app: &AppHandle) -> Result<bool, String> {
    if app.get_webview_window(LABEL).is_some() {
        return Ok(false);
    }
    let s = load();
    let win = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html?strip=1".into()))
        .title("Farder strip")
        .inner_size(s.width as f64, HEIGHT)
        .decorations(false)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focusable(false)
        .focused(false)
        .visible(false)
        .build()
        .map_err(|e| e.to_string())?;
    #[cfg(windows)]
    crate::notify_popup::hide_system_border(&win);
    let _ = &win;
    Ok(true)
}

/// Destroy the window, freeing its browser process. Safe if it is not there.
pub fn close_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.destroy();
    }
}

/// The strip settings, for the settings screen and the strip's own page.
#[tauri::command]
pub fn get_edge_strip() -> StripSettings {
    load()
}

/// Save the settings, create or destroy the window to match, and tell the strip's
/// page (`strip:settings-changed`) so it re-places itself.
#[tauri::command]
pub fn set_edge_strip(app: AppHandle, settings: StripSettings) -> Result<StripSettings, String> {
    let s = normalize(settings);
    let value = serde_json::to_value(&s).map_err(|e| e.to_string())?;
    crate::commands::settings_set(SETTINGS_KEY, value)?;
    if s.enabled {
        ensure_window(&app)?;
    } else {
        close_window(&app);
    }
    let _ = app.emit("strip:settings-changed", &s);
    Ok(s)
}

/// The page says whether it has anything to show; place the window and show or
/// hide it. Called whenever the page's content changes.
#[tauri::command]
pub fn strip_layout(app: AppHandle, has_content: bool) -> Result<(), String> {
    let win = app
        .get_webview_window(LABEL)
        .ok_or_else(|| "strip window is not running".to_string())?;
    let s = load();
    if !s.enabled || (!has_content && !s.always_visible) {
        // Nothing to show: also drop back to never taking focus, so a reply box
        // that was open cannot leave the window grabbing keystrokes later.
        let _ = win.set_focusable(false);
        return win.hide().map_err(|e| e.to_string());
    }
    let monitor = app
        .primary_monitor()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no primary monitor".to_string())?;
    let work = monitor.work_area();
    let l = layout_for(&s, work.position.x, work.position.y, work.size.width, work.size.height, monitor.scale_factor());
    win.set_size(tauri::LogicalSize::new(l.width, l.height)).map_err(|e| e.to_string())?;
    win.set_position(PhysicalPosition::new(l.x, l.y)).map_err(|e| e.to_string())?;
    // `focusable(false)` means this shows without activating.
    win.show().map_err(|e| e.to_string())
}

/// The strip normally NEVER takes focus. Typing a reply is the one time it must:
/// the page asks for focus when the reply box opens and gives it up again after.
#[tauri::command]
pub fn strip_focus(app: AppHandle, focusable: bool) -> Result<(), String> {
    let win = app
        .get_webview_window(LABEL)
        .ok_or_else(|| "strip window is not running".to_string())?;
    win.set_focusable(focusable).map_err(|e| e.to_string())?;
    if focusable {
        win.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(Serialize, Clone)]
struct OpenPayload {
    server_id: Option<String>,
    channel_id: Option<u64>,
}

/// A chip or the preview was clicked: bring the real app forward and ask it to
/// show that conversation (`strip:open`, handled in `useServerEvents.ts`). With no
/// target it just opens the app.
#[tauri::command]
pub fn strip_open(app: AppHandle, server_id: Option<String>, channel_id: Option<u64>) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    let _ = main.unminimize();
    let _ = main.show();
    let _ = app.emit_to("main", "strip:open", OpenPayload { server_id, channel_id });
    main.set_focus().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(dock: &str, align: &str, width: u32) -> StripSettings {
        StripSettings { enabled: true, dock: dock.into(), align: align.into(), width, always_visible: false }
    }

    #[test]
    fn top_centre_on_a_plain_monitor() {
        let l = layout_for(&s("top", "center", 460), 0, 0, 1920, 1040, 1.0);
        assert_eq!(l.x, (1920 - 460) / 2);
        assert_eq!(l.y, MARGIN as i32);
        assert_eq!((l.width, l.height), (460.0, HEIGHT));
    }

    #[test]
    fn bottom_sits_above_the_taskbar_because_the_work_area_excludes_it() {
        // 1080p screen, 40px taskbar: the work area is 1040 tall.
        let l = layout_for(&s("bottom", "center", 460), 0, 0, 1920, 1040, 1.0);
        assert_eq!(l.y, 1040 - HEIGHT as i32 - MARGIN as i32);
        // A taskbar at the top moves the work area down; the strip follows it.
        let l = layout_for(&s("top", "center", 460), 0, 40, 1920, 1040, 1.0);
        assert_eq!(l.y, 40 + MARGIN as i32);
    }

    #[test]
    fn left_and_right_hug_their_corner() {
        let left = layout_for(&s("top", "left", 400), 0, 0, 1920, 1040, 1.0);
        assert_eq!(left.x, MARGIN as i32);
        let right = layout_for(&s("top", "right", 400), 0, 0, 1920, 1040, 1.0);
        assert_eq!(right.x, 1920 - 400 - MARGIN as i32);
    }

    #[test]
    fn a_monitor_that_does_not_start_at_zero() {
        let l = layout_for(&s("top", "center", 460), 2560, 0, 2560, 1400, 1.0);
        assert_eq!(l.x, 2560 + (2560 - 460) / 2);
    }

    #[test]
    fn a_125_percent_display_places_in_physical_pixels() {
        // Size is logical, position is physical: the 125% display-scale gotcha.
        let l = layout_for(&s("bottom", "right", 460), 0, 0, 2400, 1300, 1.25);
        let w = (460.0f64 * 1.25).round() as i32;
        let h = (HEIGHT * 1.25).round() as i32;
        let m = (MARGIN * 1.25).round() as i32;
        assert_eq!(l.x, 2400 - w - m);
        assert_eq!(l.y, 1300 - h - m);
    }

    #[test]
    fn never_wider_than_the_screen() {
        // 300 wide screen, 900 wide request: it shrinks to fit with a margin each side.
        let l = layout_for(&s("top", "center", 900), 0, 0, 300, 600, 1.0);
        assert_eq!(l.width, 300.0 - 2.0 * MARGIN);
        assert!(l.x >= 0);
    }

    #[test]
    fn nonsense_settings_are_pulled_back_into_range() {
        let n = normalize(StripSettings { enabled: true, dock: "sideways".into(), align: "diagonal".into(), width: 5, always_visible: true });
        assert_eq!((n.dock.as_str(), n.align.as_str(), n.width), ("top", "center", MIN_WIDTH));
        let n = normalize(StripSettings { enabled: true, dock: "bottom".into(), align: "left".into(), width: 100_000, always_visible: false });
        assert_eq!((n.dock.as_str(), n.align.as_str(), n.width), ("bottom", "left", MAX_WIDTH));
        // A bad scale factor must not produce NaN positions.
        let l = layout_for(&s("top", "center", 460), 0, 0, 1920, 1040, f64::NAN);
        assert_eq!(l.x, (1920 - 460) / 2);
    }

    #[test]
    fn the_stored_shape_round_trips_and_defaults_are_off() {
        let d = StripSettings::default();
        assert!(!d.enabled && !d.always_visible);
        let back: StripSettings = serde_json::from_value(serde_json::to_value(&d).unwrap()).unwrap();
        assert_eq!(back, d);
        // A stored object missing a field is rejected, so `load` falls back to the default.
        assert!(serde_json::from_value::<StripSettings>(serde_json::json!({"enabled": true})).is_err());
    }
}
