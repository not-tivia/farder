//! How the main window behaves when you close it, start the app, or sign in.
//!
//! Farder is meant to be left running like a messenger: close the window and it
//! keeps going from the system tray (so messages still arrive and pop up), and it
//! can start minimised, optionally when you log in to Windows. Quit is explicit,
//! from the tray menu.
//!
//! Safety rules that matter:
//!  - The app only starts hidden if the tray icon really exists; otherwise there
//!    would be no way to open it.
//!  - Closing never leaves an invisible half-alive process: when the setting is
//!    off, closing quits properly (stopping hosted servers first).
//!  - "Start with Windows" is only available in an installed build; a development
//!    build's path would break at the next login.

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Manager, Window};

/// Set once a real quit has begun, so the close handler lets it through.
static QUITTING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum CloseAction {
    /// Hide the window; the app keeps running from the tray.
    HideToTray,
    /// Really quit.
    Quit,
}

/// What closing the main window should do. Pure so it can be tested.
pub fn decide_close(close_to_tray: bool, quitting: bool, tray_available: bool) -> CloseAction {
    // Hiding to a tray that does not exist would strand the app.
    if close_to_tray && !quitting && tray_available { CloseAction::HideToTray } else { CloseAction::Quit }
}

/// Whether the app should start hidden in the tray: asked for by the setting or by
/// the `--minimized` flag (what the login entry passes), and only if there is a tray.
pub fn should_start_hidden(setting: bool, args: &[String], tray_available: bool) -> bool {
    tray_available && (setting || args.iter().any(|a| a == "--minimized"))
}

/// What to do when Farder is started a second time while it is already running.
#[derive(Debug, PartialEq, Eq)]
pub struct SecondLaunch {
    /// Bring the running window forward.
    pub show_window: bool,
    /// A `farder://` link the second launch was started with, to hand to the
    /// running copy (a link click starts a new process with the URL as an argument,
    /// so without forwarding it would be thrown away).
    pub deep_link: Option<String>,
}

/// Decide what a second launch does. Pure so it can be tested.
///
/// A launch from the sign-in entry passes `--minimized`; if Farder is already
/// running it must NOT throw the window open (the person did nothing). Anything
/// else -- the shortcut, or a link -- shows it.
pub fn plan_second_launch(args: &[String]) -> SecondLaunch {
    let deep_link = args.iter().skip(1).find(|a| a.starts_with("farder://")).cloned();
    let only_minimized = args.iter().skip(1).any(|a| a == "--minimized");
    SecondLaunch { show_window: deep_link.is_some() || !only_minimized, deep_link }
}

/// Run in the FIRST copy when a second one is started (and exits).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn on_second_launch(app: &AppHandle, args: &[String]) {
    let plan = plan_second_launch(args);
    if plan.show_window {
        show_main(app);
    }
    if let Some(url) = plan.deep_link {
        let _ = tauri::Emitter::emit(app, "deep-link", url);
    }
}

static TRAY_AVAILABLE: AtomicBool = AtomicBool::new(false);

pub fn set_tray_available(ok: bool) {
    TRAY_AVAILABLE.store(ok, Ordering::Release);
}

pub fn tray_available() -> bool {
    TRAY_AVAILABLE.load(Ordering::Acquire)
}

pub fn close_to_tray_setting() -> bool {
    // On by default: leaving a messenger running is the point.
    crate::commands::settings_get("close_to_tray").and_then(|v| v.as_bool()).unwrap_or(true)
}

pub fn start_minimized_setting() -> bool {
    crate::commands::settings_get("start_minimized").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Bring the main window forward from wherever it is (hidden, minimised, behind).
pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Quit for real: stop hosted servers, then exit the whole process (including the
/// hidden notification window, which would otherwise keep it alive).
pub fn quit_app(app: &AppHandle) {
    QUITTING.store(true, Ordering::Release);
    if let Some(procs) = app.try_state::<crate::server_manager::ServerProcesses>() {
        procs.stop_all();
    }
    app.exit(0);
}

/// The main window's close handler.
pub fn handle_main_close(window: &Window, api: &tauri::CloseRequestApi) {
    let app = window.app_handle();
    match decide_close(close_to_tray_setting(), QUITTING.load(Ordering::Acquire), tray_available()) {
        CloseAction::HideToTray => {
            api.prevent_close();
            let _ = window.hide();
            tell_once_it_is_still_running(app);
        }
        CloseAction::Quit => {
            api.prevent_close(); // quit_app exits explicitly, after stopping servers
            quit_app(app);
        }
    }
}

/// The first time the window hides to the tray, say so (once, ever) -- otherwise
/// "I closed it and it is still running" is a surprise. Uses Farder's own pop-up
/// only; it never falls back to an OS notification for this.
fn tell_once_it_is_still_running(app: &AppHandle) {
    if crate::commands::settings_get("tray_hint_shown").and_then(|v| v.as_bool()).unwrap_or(false) {
        return;
    }
    if crate::commands::notification_popup_style() != "skinned" {
        return;
    }
    if crate::notify_popup::push(
        app,
        "Farder is still running".to_string(),
        "It lives in the tray so messages still reach you. Click the tray icon to open it, or choose Quit there. You can change this in Settings.".to_string(),
        None,
        None,
    )
    .is_ok()
    {
        let _ = crate::commands::settings_set("tray_hint_shown", serde_json::json!(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_hides_to_the_tray_only_when_asked_and_possible() {
        assert_eq!(decide_close(true, false, true), CloseAction::HideToTray);
        assert_eq!(decide_close(false, false, true), CloseAction::Quit, "setting off: closing quits");
        assert_eq!(decide_close(true, true, true), CloseAction::Quit, "a real quit is never blocked");
        assert_eq!(decide_close(true, false, false), CloseAction::Quit, "no tray: never strand the app");
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_second_launch_shows_the_running_window() {
        let p = plan_second_launch(&args(&["farder-client.exe"]));
        assert_eq!(p, SecondLaunch { show_window: true, deep_link: None });
    }

    #[test]
    fn a_second_launch_from_the_sign_in_entry_does_not_throw_the_window_open() {
        let p = plan_second_launch(&args(&["farder-client.exe", "--minimized"]));
        assert_eq!(p, SecondLaunch { show_window: false, deep_link: None });
    }

    #[test]
    fn a_link_is_forwarded_to_the_running_copy_and_shows_it_even_if_minimized() {
        let p = plan_second_launch(&args(&["farder-client.exe", "--minimized", "farder://join/abc"]));
        assert_eq!(p.deep_link.as_deref(), Some("farder://join/abc"));
        assert!(p.show_window, "someone clicked a link: they want to see it");
        // The program name (argv[0]) is never mistaken for a flag or a link.
        let q = plan_second_launch(&args(&["farder://not-a-link-it-is-argv0"]));
        assert_eq!(q.deep_link, None);
    }

    #[test]
    fn only_farder_links_are_forwarded() {
        for bad in ["https://evil.example/", "file:///etc/passwd", "farder:/x", "FARDER://x"] {
            assert_eq!(plan_second_launch(&args(&["f.exe", bad])).deep_link, None, "{bad}");
        }
    }

    #[test]
    fn starting_hidden_needs_a_tray_whatever_asked_for_it() {
        let none: Vec<String> = vec![];
        let flag = vec!["farder".to_string(), "--minimized".to_string()];
        assert!(should_start_hidden(true, &none, true));
        assert!(should_start_hidden(false, &flag, true), "the login entry passes --minimized");
        assert!(!should_start_hidden(false, &none, true), "by default the window shows");
        assert!(!should_start_hidden(true, &flag, false), "no tray: the window must show");
        assert!(!should_start_hidden(false, &["--minimizedx".to_string()], true), "the flag is matched exactly");
    }
}
