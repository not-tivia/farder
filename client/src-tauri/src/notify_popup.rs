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

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};

pub const LABEL: &str = "notify";

/// Logical pixels. Must match the card size in the theme CSS (`.notify-popup-card`).
/// A plain card is exactly `CARD_H` tall; one with a media preview is taller, so
/// the page MEASURES its content and reports the height (`notify_layout`).
pub const CARD_W: f64 = 380.0;
pub const CARD_H: f64 = 76.0;
pub const GAP: f64 = 8.0;
/// The tallest the window will ever be (logical px). The page caps its cards so
/// the content fits; this is the backstop if it ever does not.
pub const MAX_HEIGHT: f64 = 900.0;
/// Space between the top of the screen and the first card.
pub const TOP_MARGIN: f64 = 12.0;

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

/// Pure so it can be tested without a display. `content_h` is the logical height
/// of what the page is showing; `monitor_x`/`monitor_w` are the monitor's
/// PHYSICAL origin and width; `scale` is its device scale factor.
pub fn layout_for(content_h: f64, monitor_x: i32, monitor_y: i32, monitor_w: u32, scale: f64) -> Layout {
    let width = CARD_W + 2.0 * GAP;
    // Never smaller than one card (a zero/NaN report must not make a sliver),
    // never taller than the backstop.
    let min_h = CARD_H + 2.0 * GAP;
    let height = if content_h.is_finite() { content_h.clamp(min_h, MAX_HEIGHT) } else { min_h };
    let win_w_phys = (width * scale).round() as i32;
    let x = monitor_x + (monitor_w as i32 - win_w_phys) / 2;
    let y = monitor_y + (TOP_MARGIN * scale).round() as i32;
    Layout { x, y, width, height }
}

/// Where a reply typed into the pop-up should go. Set by the client ONLY when the
/// person allows replying from pop-ups and the pop-up is showing who it is from.
/// The pop-up merely carries it back; the main window re-checks it before sending.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ReplyTarget {
    pub server_id: String,
    pub channel_id: u64,
}

#[derive(Serialize, Clone)]
struct PushPayload {
    title: String,
    body: String,
    /// A direct video/image link the page may preview (only ever set for a
    /// sender the person trusts -- decided in the client, not here).
    media_url: Option<String>,
    reply: Option<ReplyTarget>,
}

/// Build the window if it does not exist. Hidden and unfocusable from birth so
/// it can never steal a keystroke from whatever the person is typing into.
pub fn ensure_window(app: &AppHandle) -> Result<bool, String> {
    if app.get_webview_window(LABEL).is_some() {
        return Ok(false);
    }
    let win = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html?notify=1".into()))
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
    #[cfg(windows)]
    hide_system_border(&win);
    let _ = &win;
    Ok(true)
}

/// Windows 11 draws a 1px accent-coloured line around even a borderless window,
/// which showed up as a pink rectangle 8px outside the card (the window is the
/// card plus transparent padding). Ask DWM for "no border". Best-effort: Windows
/// 10 does not know the setting and ignores it, which is fine.
#[cfg(windows)]
pub(crate) fn hide_system_border(win: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR};
    let Ok(h) = win.hwnd() else { return };
    // DWMWA_COLOR_NONE
    let none: u32 = 0xFFFF_FFFE;
    unsafe {
        let _ = DwmSetWindowAttribute(
            HWND(h.0 as _),
            DWMWA_BORDER_COLOR,
            &none as *const u32 as *const core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

/// Destroy the pop-up window, freeing its browser process. Safe if it is not there.
pub fn close_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.destroy();
    }
}

/// Hand a notification to the pop-up window.
pub fn push(
    app: &AppHandle,
    title: String,
    body: String,
    media_url: Option<String>,
    reply: Option<ReplyTarget>,
) -> Result<(), String> {
    let freshly_built = ensure_window(app)?;
    let app = app.clone();
    let payload = PushPayload { title, body, media_url, reply };
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

/// Called by the pop-up page whenever what it shows changes: `count` cards taking
/// `height` logical pixels in total (0 cards hides the window).
#[tauri::command]
pub fn notify_layout(app: AppHandle, count: u32, height: f64) -> Result<(), String> {
    let win = app
        .get_webview_window(LABEL)
        .ok_or_else(|| "notification window is not running".to_string())?;
    if count == 0 {
        // Nothing left to show: also drop back to never taking focus, so a reply
        // box that was open cannot leave the window grabbing keystrokes later.
        let _ = win.set_focusable(false);
        return win.hide().map_err(|e| e.to_string());
    }
    let monitor = app
        .primary_monitor()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no primary monitor".to_string())?;
    let l = layout_for(
        height,
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

/// The pop-up normally NEVER takes focus (it must not steal keystrokes from
/// whatever you are typing into). Typing a reply is the one time it must: the
/// page asks for focus when a reply box opens and gives it up again when it
/// closes, so the window is focusable only for as long as you are writing.
#[tauri::command]
pub fn notify_focus(app: AppHandle, focusable: bool) -> Result<(), String> {
    let win = app
        .get_webview_window(LABEL)
        .ok_or_else(|| "notification window is not running".to_string())?;
    win.set_focusable(focusable).map_err(|e| e.to_string())?;
    if focusable {
        win.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
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

    const ONE: f64 = CARD_H + 2.0 * GAP;

    #[test]
    fn a_reply_target_round_trips_the_shape_the_page_sends() {
        // The client sends snake_case keys; a rename on either side would silently
        // drop the Reply button, so pin the wire shape.
        let t: ReplyTarget = serde_json::from_str(r#"{"server_id":"s1","channel_id":7}"#).unwrap();
        assert_eq!(t, ReplyTarget { server_id: "s1".into(), channel_id: 7 });
        assert_eq!(serde_json::to_value(&t).unwrap(), serde_json::json!({"server_id":"s1","channel_id":7}));
        // A channel id cannot be negative or fractional.
        assert!(serde_json::from_str::<ReplyTarget>(r#"{"server_id":"s","channel_id":-1}"#).is_err());
        assert!(serde_json::from_str::<ReplyTarget>(r#"{"server_id":"s","channel_id":1.5}"#).is_err());
    }

    #[test]
    fn centres_on_the_monitor_at_scale_one() {
        let l = layout_for(ONE, 0, 0, 1920, 1.0);
        let w = (CARD_W + 2.0 * GAP) as i32;
        assert_eq!(l.x, (1920 - w) / 2);
        assert_eq!(l.y, TOP_MARGIN as i32);
    }

    #[test]
    fn centres_on_a_monitor_that_does_not_start_at_zero() {
        // Primary monitor placed to the right of another one.
        let l = layout_for(ONE, 2560, 0, 2560, 1.0);
        let w = (CARD_W + 2.0 * GAP) as i32;
        assert_eq!(l.x, 2560 + (2560 - w) / 2);
    }

    #[test]
    fn a_125_percent_display_still_centres_in_physical_pixels() {
        // The 125% display-scale gotcha: size is logical, position is physical.
        let l = layout_for(ONE, 0, 0, 2400, 1.25);
        let w_phys = ((CARD_W + 2.0 * GAP) * 1.25).round() as i32;
        assert_eq!(l.x, (2400 - w_phys) / 2);
        assert_eq!(l.y, (TOP_MARGIN * 1.25).round() as i32);
    }

    #[test]
    fn ultrawide_screens_centre_on_the_whole_width() {
        let l = layout_for(ONE, 0, 0, 5120, 1.0);
        let w = (CARD_W + 2.0 * GAP) as i32;
        assert_eq!(l.x, (5120 - w) / 2);
    }

    #[test]
    fn the_window_is_as_tall_as_the_page_says_within_limits() {
        // A page showing a media card reports a taller height and gets it.
        assert_eq!(layout_for(300.0, 0, 0, 1920, 1.0).height, 300.0);
        // Too small or nonsense never makes a sliver...
        assert_eq!(layout_for(0.0, 0, 0, 1920, 1.0).height, ONE);
        assert_eq!(layout_for(f64::NAN, 0, 0, 1920, 1.0).height, ONE);
        assert_eq!(layout_for(-50.0, 0, 0, 1920, 1.0).height, ONE);
        // ...and a runaway report is capped.
        assert_eq!(layout_for(1.0e9, 0, 0, 1920, 1.0).height, MAX_HEIGHT);
        assert_eq!(layout_for(f64::INFINITY, 0, 0, 1920, 1.0).height, ONE);
    }
}
