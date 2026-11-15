# Portability - Android and Apple (an audit, not a port)

Farder is built to run as a desktop app first. Nothing here has been built for
Android, iOS or macOS. This page lists what is tied to the desktop so a later port
knows where the work is, and the rules to follow so new code does not make it worse.

## What is shared and should port as is

- `crates/farder-crypto`, `farder-protocol`, `farder-e2ee-client`, `farder-mls`,
  `farder-history`, `farder-node`: plain Rust, no windowing, no OS calls. These are
  the privacy promise and are the part that must stay identical everywhere.
- The React/TypeScript front end (`client/src`): shared logic lives in `client/src/lib`
  (pure, tested with the node runner) and hooks. Tauri 2 supports Android and iOS
  with the same front end.

## Desktop-only today (in `client/src-tauri/src`)

| Area | Files | Why it does not carry over |
|---|---|---|
| Several windows (buddy list, chat windows, notification pop-up, edge strip, screenshare pop-out) | `buddy.rs`, `notify_popup.rs`, `edge_strip.rs`, `window_behavior.rs` | Phones show one app screen at a time. A phone port keeps the one-window layout; the small-window layout (Part 10 of the checklist) is the nearest thing. |
| Tray, start with the OS, single instance | `tray.rs`, `autostart.rs`, `tauri-plugin-single-instance` | No tray on phones. Phones use push notifications and background services instead. |
| Starting and stopping a local server process | `server_manager.rs` (spawn, find a free port, reap orphans with PowerShell on Windows) | A phone app cannot run a long-lived child server. On mobile a person joins servers hosted elsewhere (a friend's PC or a relay), and does not host. |
| Screen capture and sharing | `display.rs`, `display_wgc.rs`, `screenshare.rs`, `video_encoder.rs` (Windows Graphics Capture, openh264) | Mobile has its own capture APIs (MediaProjection, ReplayKit). |
| Per-app audio capture | `screen_audio.rs`, `screen_audio_wasapi.rs` | WASAPI is Windows only. |
| Audio devices | `audio_cpal.rs`, `audio.rs` | `cpal` supports Android and iOS, but permissions and routing differ. |
| Hotkeys, window position, click-through | parts of `window_behavior.rs` | No equivalent. |
| File dialogs | `rfd` | Mobile uses system pickers. |

## Rules for new code

1. Put decisions (what to show, what is allowed, what counts as unread) in `client/src/lib`
   as pure functions with tests, not inside a window or a component.
2. Anything that touches Windows, a second window or a child process goes behind
   `#[cfg(windows)]` / `#[cfg(desktop)]` or its own module, with a harmless fallback
   so the shared code still compiles elsewhere.
3. Do not put privacy logic in a place only the desktop reaches. Encryption, delete
   purging and consent checks belong in the shared crates or `lib`.
4. A feature that needs a second window must also work as a panel in the main window.
5. Avoid assuming a mouse, a hover or a keyboard. The click and tap path must work.

## Known risks for a mobile port

- The client crate links desktop-only crates (`windows-capture`, `wasapi`) under
  `cfg(windows)`, which is correct, but `rfd`, the tray feature and the sidecar
  (`externalBin`) in `tauri.conf.json` are unconditional and need a mobile config.
- The server is bundled as a sidecar binary for the host triple; mobile has none.
