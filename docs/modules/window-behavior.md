# Tray, close-to-tray and start-minimised (`window_behavior.rs`, `autostart.rs`, `tray.rs`)

**Why:** Farder is meant to be left running like a messenger -- messages arrive, a
pop-up shows, you reply from it -- so closing the window must not mean "stop".

## Behaviour
- **Close (X / title-bar close):** with `close_to_tray` ON (default) the window hides
  and the app keeps running. The first time, a one-off Farder pop-up says so
  ("Farder is still running"). Off: closing quits properly -- hosted servers are
  stopped and the whole process exits (including the hidden notification window,
  which would otherwise keep it alive with no UI).
- **Tray:** left-click opens Farder; right-click shows Open / Quit. Quit is explicit
  (`quit_app`). The tray now has the app icon (without one the entry can be invisible).
- **Start minimised:** the main window is created hidden (`tauri.conf.json`
  `visible: false`) and shown during setup unless starting in the tray, so a hidden
  start never flashes a window.
- **Start with Windows (opt-in):** one value in the user's own `Run` key whose data is
  `"<exe>" --minimized`. Turning it off deletes that value. Only in an installed
  build (a dev build's path would break at the next sign-in).

## Safety rules (tested)
- No tray => never start hidden, never hide on close (`decide_close`,
  `should_start_hidden`): there would be no way back in.
- A real quit is never blocked by the close handler.
- The sign-in entry only ever touches `HKCU\...\Run` value `Farder`, with the path
  quoted so spaces cannot split it (the stored command was round-tripped through
  Windows on a throwaway key).

## Known gap
There is no single-instance guard: launching Farder again while it sits in the tray
starts a second process instead of showing the first. (Adding the standard plugin is
a new dependency; deliberately not done without a decision.)

## Verification status
Unit-tested: the close/start decisions, the registry argument shapes. UNVERIFIED until
run on Windows: the tray icon appearing, left-click opening, hide-on-close, a hidden
start, the first-time hint, and that a hidden window really stops its timers.
