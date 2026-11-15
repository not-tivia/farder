# Feature matrix - what is actually verified

One row per feature. Status is only ever one of:

- **VERIFIED** - run on the real path and the result observed (evidence column says how).
- **UNVERIFIED** - built, compiles, unit tests may pass, but nobody has seen it work.
- **BROKEN** - seen failing.

"Real path" means the two-client self-test (`scripts/selftest/`): two separate
Windows copies of the app, each with its own throwaway identity, talking to each
other, driven through their real windows. Unit tests do not count.

Goal this serves: a chat app friends can use locally, later offered as a free
alternative to the big clients, and kept portable to Android and Apple
(see `docs/modules/portability.md`).

Last updated: 2026-11-15.

## Test tiers

| Tier | What it is | Last result |
|---|---|---|
| T1 Rust workspace | `cargo test --workspace --no-fail-fast` | 1010 pass, 0 fail (2026-11-15); the `farder-mls` store tests used to fail in parallel (shared at-rest key) and now hold a lock, fixed 2026-11-14 |
| T2 Client crate | `cd client/src-tauri && cargo test -- --test-threads=1` | 250 pass, 0 fail (2026-11-15) |
| T3 Frontend | `client/scripts/run-unit-tests.sh` + `npx tsc --noEmit` | 91 pass, tsc clean |
| T4 Audits | `scripts/seam_audit.py`, `reachability_audit.py`, `doc_audit.py` | all three pass (2026-11-15) |
| T5 Real path | two Windows instances, driven over WebView2 DevTools: `scripts/selftest/run-selftest.ps1` | 2026-11-15: 17 scenarios, 70/70 checks PASS in one run on the latest code (core 9, dm 7, buddy 5, edit 3, direct 4, unread 4, popup 3, attach 2, garble 2, admin-garble 5, powers 4, voice 3, strip 2, themes 6, relaunch 6, direct-relaunch 3, tray 2); includes the app-as-library change; relay and direct servers |

## Core chat (what friends will use)

| Feature | Status | Evidence / note |
|---|---|---|
| Create identity, PIN gate | VERIFIED | selftest core + relaunch (unlock with PIN) |
| Create a server | VERIFIED | selftest core (relay-hosted default) |
| Create an invite link | VERIFIED (relay and direct) | selftest core; the first-run Translation dialog covers the app until skipped, which may be what chris hit on 2026-11-09. |
| Join from an invite (second identity) | VERIFIED (relay) | selftest core, incl. the "Join it?" confirmation |
| Send / receive text, both directions | VERIFIED | selftest core |
| Direct messages (encrypted) | VERIFIED | selftest dm: open from member list, send, recipient gets the conversation and reads it, reply from a small chat window. Found and fixed 3 bugs on the way (server announced every DM to the whole server; recipient never saw the DM; DM history opened from the list showed ciphertext) |
| Encrypted channels | UNVERIFIED | server-side e2e tests pass (T1); GUI path unseen |
| Quit and relaunch: still joined | VERIFIED (relay and direct) | selftest relaunch + direct-relaunch: B still in the direct server with no re-invite, chat works again. This was chris's reported bug (fixed 8ab9a69, now confirmed on the real path) |
| Message edit / delete (own message; other person sees both) | VERIFIED | selftest edit. Also fixed: edits/deletes on a server not on screen were ignored and the local readable copy of a deleted message was kept |
| Second server, and a direct (non-relayed) server | VERIFIED | selftest direct: create, invite (points at 127.0.0.1), B joins, chat both ways |
| Attachments (picture: stage by drop, send, recipient loads it) | VERIFIED | selftest attach (drop event emulated; the native file-picker dialog itself is not drivable) |
| Voice channels: create, both join, rosters agree, leave | VERIFIED (signalling only) | selftest voice. Found and fixed: new servers gave members no CONNECT/SPEAK, so only the owner could join voice. NOT covered: microphone, speakers, audio quality, mute/deafen, screenshare (need hardware and a person) |

## Windows added on `features-skins-notify-garble`

| Feature | Status | Evidence / note |
|---|---|---|
| Themes (6) apply, text readable, nothing overflows | VERIFIED | selftest themes (screenshots in the results folder). Found and fixed: avatar letters nearly invisible in Candy and Hello Kitty. NOT judged by a machine: whether they look good, and gradient sidebars (contrast not measurable) |
| Close to tray (app survives closing its window, receives messages, pop-up shows) | VERIFIED | selftest tray (the pop-up part with focus simulated). Tray icon/menu clicks, start in tray, start with Windows: UNVERIFIED (Parts 1-2) |
| Pop-up window fits its content (short, long, reply box open; no outline frame) | VERIFIED | selftest popup-size. Found and fixed: the page inherited the main window's frame (border + shadow) and squashed the card, cutting off its bottom |
| Notification pop-up (message from another server: shows it, Reply from the pop-up reaches the sender) | VERIFIED | selftest popup. Background pop-up (focus simulated by overriding the one "has focus" answer) also VERIFIED. NOT automatable: pop-up position/centring, keyboard focus after use: checklist Part 3, chris to eyeball. keyboard stops working in the main window after pop-up testing, pop-up not centred first try, Reply button possibly clipped |
| Autoplay | UNVERIFIED | Part 4 |
| Garbler (own "speak as": B receives garbled words; off restores normal) | VERIFIED | selftest garble |
| Admin garble (moderation) and consent garble | VERIFIED | selftest admin-garble: author keeps own words, others see garbled; stop works; a consensual garble is refused without consent and works after. Delegated powers (grant/revoke "invite" and "garble", enforced on the other side): VERIFIED by selftest powers via the commands; the on-screen powers dialogs themselves not clicked |
| Sound | UNVERIFIED | Part 6 |
| Message density | UNVERIFIED | Part 8 |
| Unread markers (dot on another server, highlight on a conversation) | VERIFIED | selftest unread |
| Edge strip (turn on, window appears, shows an incoming message) | VERIFIED | selftest strip. Found and fixed: turning it on hung the app on Windows (window built inside a synchronous command; now async). NOT checked by machine: docking position, always-visible, reply from the strip, keyboard focus (Part 9) |
| Small window layout | UNVERIFIED | Part 10 |
| Buddy list shows people and channels | VERIFIED | chris 2026-11-09; selftest buddy 2026-11-14 (channels; DM rows not yet) |
| Chat window opens, shows messages | VERIFIED (by chris, screenshot 2026-11-09) | for the server that opened; others fixed in 28fe65f, chris reports "they work visually" |
| Chat window: sending updates the window, live lines from others appear | VERIFIED (active server) | selftest buddy: typed line shows in the window and reaches B; B's line appears live. Non-active server not yet tested |
| Chat window: typing box visible | VERIFIED | selftest buddy (box inside the window) |
| Chat window: DMs | VERIFIED | selftest dm |
| Live messages / unread for channels not on screen | VERIFIED | selftest buddy (was broken: app only subscribed to the viewed channel; fixed by useSubscriptions). Unread BADGE rendering itself not asserted yet |
| Display name shows right after joining | VERIFIED | selftest dm (was "Anonymous" until reconnect; fixed) |

## Hosting on Ubuntu

| Item | Status | Evidence |
|---|---|---|
| Server runs headless on Ubuntu, prints a setup token | VERIFIED | ran on Ubuntu 24.04 (2026-11-15); 1010 tests pass there |
| Windows client claims a Linux-hosted server and chats | VERIFIED | selftest ubuntu (4 checks), direct connection |
| Runs as a systemd service, over the internet, behind the relay, long uptime | UNVERIFIED | unit file written (`deploy/server/farder-server.service`), never installed |

See `docs/hosting-on-ubuntu.md`.

## Portability (Android / Apple)

| Item | Status | Evidence |
|---|---|---|
| Shared Rust crates + the app crate type-check for Android (NDK) | VERIFIED | `scripts\portability_check_android.ps1 -Client`, 2026-11-14 |
| Android APK builds | VERIFIED | `scripts\android_apk.ps1` produced app-arm64-debug.apk (2026-11-15) |
| Android app installs and runs | UNVERIFIED | never installed anywhere; needs an emulator or phone the owner agrees to use (see portability.md) |
| iOS | UNVERIFIED | pure crates type-check; the rest needs a Mac and the Apple SDK |

See `docs/modules/portability.md`.
