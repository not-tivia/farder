//! The skinned notification window.
//!
//! A small always-on-top, transparent, never-focused window at the top-centre of
//! the primary monitor. It renders the app's own React view (`?notify=1`, see
//! `NotifyPopup.tsx`) with the ACTIVE THEME's CSS, so a pop-up looks like the
//! app whichever theme is running, and it shows even when Farder is minimised or
//! another program has focus.
//!
//! Why not the OS toast: a Windows toast lands in the notification-centre
//! history, which means the plaintext of an end-to-end encrypted message would
//! be copied out of the app into a store Farder does not control. This window is
//! drawn locally, holds the text only in memory, and makes no network request.
//!
//! The window is created once at startup, hidden, so its JavaScript is loaded by
//! the time the first message arrives. The page tells us how many cards it is
//! showing (`notify_layout`) and we size/position/show/hide the window to fit,
//! which also means the empty transparent area never sits over other programs
//! and eats their clicks.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};

pub const LABEL: &str = "notify";

/// Logical pixels. Must match the card size in the theme CSS (`.notify-popup-card`).
pub const CARD_W: f64 = 380.0;
pub const CARD_H: f64 = 76.0;
pub const GAP: f64 = 8.0;
/// Space between the top of the screen and the first card.
pub const TOP_MARGIN: f64 = 12.0;
/// More than this and the oldest card is dropped by the page; the window never
/// grows past it.
pub const MAX_CARDS: u32 = 4;

/// Where the window goes and how big it is, for `count` cards on a monitor.
#[derive(Debug, PartialEq)]
pub struct Layout {
    /// Physical pixels.
    pub x: i32,
    pub y: i32,
    /// Logical pixels.
    pub width: f64,
    pub height: f64,
}

/// Pure so it can be tested without a display. `monitor_x`/`monitor_w` are the
/// monitor's PHYSICAL origin and width; `scale` is its device scale factor.
pub fn layout_for(count: u32, monitor_x: i32, monitor_y: i32, monitor_w: u32, scale: f64) -> Layout {
    let n = count.clamp(1, MAX_CARDS) as f64;
    let width = CARD_W + 2.0 * GAP;
    let height = n * CARD_H + (n + 1.0) * GAP;
    let win_w_phys = (width * scale).round() as i32;
    let x = monitor_x + (monitor_w as i32 - win_w_phys) / 2;
    let y = monitor_y + (TOP_MARGIN * scale).round() as i32;
    Layout { x, y, width, height }
}

#[derive(Serialize, Clone)]
struct PushPayload {
    title: String,
    body: String,
}

/// Build the window if it does not exist. Hidden and unfocusable from birth so
/// it can never steal a keystroke from whatever the person is typing into.
pub fn ensure_window(app: &AppHandle) -> Result<bool, String> {
    if app.get_webview_window(LABEL).is_some() {
        return Ok(false);
    }
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html?notify=1".into()))
        .title("Farder notification")
        .inner_size(CARD_W + 2.0 * GAP, CARD_H + 2.0 * GAP)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focusable(false)
        .focused(false)
        .visible(false)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// Hand a notification to the pop-up window.
pub fn push(app: &AppHandle, title: String, body: String) -> Result<(), String> {
    let freshly_built = ensure_window(app)?;
    let app = app.clone();
    let payload = PushPayload { title, body };
    if freshly_built {
        // Its page has not loaded yet; an event sent now would be lost. Only
        // happens if startup creation failed, so a short wait is enough.
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(900));
            let _ = app.emit_to(LABEL, "notify:push", payload);
        });
        Ok(())
    } else {
        app.emit_to(LABEL, "notify:push", payload).map_err(|e| e.to_string())
    }
}

/// Called by the pop-up page whenever its card count changes.
#[tauri::command]
pub fn notify_layout(app: AppHandle, count: u32) -> Result<(), String> {
    let win = app
        .get_webview_window(LABEL)
        .ok_or_else(|| "notification window is not running".to_string())?;
    if count == 0 {
        return win.hide().map_err(|e| e.to_string());
    }
    let monitor = app
        .primary_monitor()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no primary monitor".to_string())?;
    let l = layout_for(
        count,
        monitor.position().x,
        monitor.position().y,
        monitor.size().width,
        monitor.scale_factor(),
    );
    win.set_size(tauri::LogicalSize::new(l.width, l.height)).map_err(|e| e.to_string())?;
    win.set_position(PhysicalPosition::new(l.x, l.y)).map_err(|e| e.to_string())?;
    win.set_ignore_cursor_events(false).map_err(|e| e.to_string())?;
    // `focusable(false)` means this shows without activating.
    win.show().map_err(|e| e.to_string())
}

/// A card was clicked: bring the real app forward.
#[tauri::command]
pub fn notify_activate(app: AppHandle) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    let _ = main.unminimize();
    let _ = main.show();
    main.set_focus().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centres_on_the_monitor_at_scale_one() {
        let l = layout_for(1, 0, 0, 1920, 1.0);
        let w = (CARD_W + 2.0 * GAP) as i32;
        assert_eq!(l.x, (1920 - w) / 2);
        assert_eq!(l.y, TOP_MARGIN as i32);
    }

    #[test]
    fn centres_on_a_monitor_that_does_not_start_at_zero() {
        // Primary monitor placed to the right of another one.
        let l = layout_for(1, 2560, 0, 2560, 1.0);
        let w = (CARD_W + 2.0 * GAP) as i32;
        assert_eq!(l.x, 2560 + (2560 - w) / 2);
    }

    #[test]
    fn a_125_percent_display_still_centres_in_physical_pixels() {
        // The 125% display-scale gotcha: size is logical, position is physical.
        let l = layout_for(1, 0, 0, 2400, 1.25);
        let w_phys = ((CARD_W + 2.0 * GAP) * 1.25).round() as i32;
        assert_eq!(l.x, (2400 - w_phys) / 2);
        assert_eq!(l.y, (TOP_MARGIN * 1.25).round() as i32);
    }

    #[test]
    fn ultrawide_screens_centre_on_the_whole_width() {
        let l = layout_for(1, 0, 0, 5120, 1.0);
        let w = (CARD_W + 2.0 * GAP) as i32;
        assert_eq!(l.x, (5120 - w) / 2);
    }

    #[test]
    fn height_grows_with_the_card_count_and_is_capped() {
        let one = layout_for(1, 0, 0, 1920, 1.0).height;
        let two = layout_for(2, 0, 0, 1920, 1.0).height;
        assert_eq!(two - one, CARD_H + GAP);
        let capped = layout_for(MAX_CARDS, 0, 0, 1920, 1.0).height;
        assert_eq!(layout_for(99, 0, 0, 1920, 1.0).height, capped);
    }

    #[test]
    fn zero_cards_never_produces_a_zero_height_window() {
        // The page hides the window for 0; this guards the arithmetic anyway.
        assert!(layout_for(0, 0, 0, 1920, 1.0).height >= CARD_H);
    }
}
