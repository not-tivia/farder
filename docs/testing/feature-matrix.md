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

Last updated: 2026-11-14 (first real-path run recorded; relay-hosted servers only).

## Test tiers

| Tier | What it is | Last result |
|---|---|---|
| T1 Rust workspace | `cargo test --workspace --no-fail-fast` | green; the `farder-mls` store tests used to fail in parallel (shared at-rest key) and now hold a lock, fixed 2026-11-14 |
| T2 Client crate | `cd client/src-tauri && cargo test -- --test-threads=1` | last run green (buddy 6 tests) |
| T3 Frontend | `client/scripts/run-unit-tests.sh` + `npx tsc --noEmit` | 91 pass, tsc clean |
| T4 Audits | `scripts/seam_audit.py`, `reachability_audit.py`, `doc_audit.py` | not re-run this session |
| T5 Real path | two Windows instances, driven over WebView2 DevTools: `scripts/selftest/run-selftest.ps1` | 2026-11-14: 13 scenarios, 55/55 checks PASS in one run (core 9, dm 7, buddy 5, edit 3, direct 4, unread 4, popup 2, attach 2, garble 2, strip 2, themes 6, relaunch 6, direct-relaunch 3); relay and direct servers |

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
| Voice, screenshare | UNVERIFIED | needs audio/capture hardware paths; not automatable here |

## Windows added on `features-skins-notify-garble`

| Feature | Status | Evidence / note |
|---|---|---|
| Themes (6) apply, text readable, nothing overflows | VERIFIED | selftest themes (screenshots in the results folder). Found and fixed: avatar letters nearly invisible in Candy and Hello Kitty. NOT judged by a machine: whether they look good, and gradient sidebars (contrast not measurable) |
| Tray, start in tray, start with Windows | UNVERIFIED | checklist Parts 1-2 |
| Notification pop-up (message from another server: shows it, Reply from the pop-up reaches the sender) | VERIFIED | selftest popup. NOT automatable: pop-up when Farder is in the BACKGROUND (the test driver forces "has focus"), position/centring, keyboard focus after use: still checklist Part 3, chris to eyeball. keyboard stops working in the main window after pop-up testing, pop-up not centred first try, Reply button possibly clipped |
| Autoplay | UNVERIFIED | Part 4 |
| Garbler (own "speak as": B receives garbled words; off restores normal) | VERIFIED | selftest garble. Admin-imposed garble, consent flow and powers: UNVERIFIED (Part 5; same-account issue) |
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

## Portability (Android / Apple)

See `docs/modules/portability.md`. Not a feature to test yet; an audit of what is desktop-only.
