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

## What has been checked (2026-11-14)

**Android: the whole Rust side type-checks.** `scripts\portability_check_android.ps1`
(Windows, uses the NDK Android Studio installed) runs `cargo check --target
aarch64-linux-android` and passes for `farder-protocol`, `farder-crypto`,
`farder-history`, `farder-e2ee-client`, `farder-mls` and, with `-Client`, the Tauri
app crate `farder-client` itself (including the C libraries SQLite and Opus, built with
the NDK through `scripts/android-toolchain.cmake`).

What it took, and what it means for the next steps:

- Native file dialogs (`rfd`) are desktop-only: now behind `cfg`, with an error on
  phones where `pick_file` / `save_recovery_image` need a system-picker plugin.
- Everything that builds or drives extra windows or the tray (`buddy`, `edge_strip`,
  `notify_popup`, `window_behavior`, `tray`, `open_themes_folder`) has a real version
  for desktop and a small stub for phones (`#[cfg(any(target_os = "android",
  target_os = "ios"))]`), so the command names stay registered and the seam audit
  stays at zero.
- `client/src-tauri/tauri.android.conf.json` / `tauri.ios.conf.json` drop the bundled
  server on phones.

**What is still not done for Android:** type-checking is not a build. Nobody has
produced an APK, linked the native libraries, or run it. Still open: the mobile entry
point (`tauri android init`), a system file-picker plugin, microphone/capture
permissions, push notifications instead of pop-ups, and the one-window layout as the
default on phones (the small-window layout, checklist Part 10, is the starting point).

**iOS:** `farder-protocol` and `farder-crypto` type-check for `aarch64-apple-ios`
(`scripts/portability_check.sh`). The SQLite crates and the app need the Apple SDK,
which exists only on a Mac, so iOS cannot be checked from this machine. The same
`cfg` split above applies to iOS, and the platform stubs already cover it.

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
