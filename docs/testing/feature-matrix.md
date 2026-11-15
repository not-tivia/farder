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
| T1 Rust workspace | `cargo test --workspace --no-fail-fast` | 1 flake: `farder-mls` store tests (2) fail in parallel, pass with `--test-threads=1` (a shared at-rest key leaks between tests; test isolation, not product) |
| T2 Client crate | `cd client/src-tauri && cargo test -- --test-threads=1` | last run green (buddy 6 tests) |
| T3 Frontend | `client/scripts/run-unit-tests.sh` + `npx tsc --noEmit` | 91 pass, tsc clean |
| T4 Audits | `scripts/seam_audit.py`, `reachability_audit.py`, `doc_audit.py` | not re-run this session |
| T5 Real path | two Windows instances, driven over WebView2 DevTools: `scripts/selftest/run-selftest.ps1` | 2026-11-14: core 9/9 and relaunch 6/6 PASS (relay-hosted server, build 5ec95826) |

## Core chat (what friends will use)

| Feature | Status | Evidence / note |
|---|---|---|
| Create identity, PIN gate | VERIFIED | selftest core + relaunch (unlock with PIN) |
| Create a server | VERIFIED | selftest core (relay-hosted default) |
| Create an invite link | VERIFIED (relay) | selftest core; the first-run Translation dialog covers the app until skipped, which may be what chris hit on 2026-11-09. Local-only server not tested |
| Join from an invite (second identity) | VERIFIED (relay) | selftest core, incl. the "Join it?" confirmation |
| Send / receive text, both directions | VERIFIED | selftest core |
| Direct messages (encrypted) | UNVERIFIED | |
| Encrypted channels | UNVERIFIED | server-side e2e tests pass (T1); GUI path unseen |
| Quit and relaunch: still joined | VERIFIED (relay) | selftest relaunch; local-only server not tested |
| Message edit / delete | UNVERIFIED | edits/deletes for a non-active server are ignored by the main window (known gap) |
| Attachments, voice, screenshare | UNVERIFIED | |

## Windows added on `features-skins-notify-garble`

| Feature | Status | Evidence / note |
|---|---|---|
| Themes (6) | UNVERIFIED | |
| Tray, start in tray, start with Windows | UNVERIFIED | checklist Parts 1-2 |
| Notification pop-up | UNVERIFIED | checklist Part 3; keyboard stops working in the main window after pop-up testing, pop-up not centred first try, Reply button possibly clipped |
| Autoplay | UNVERIFIED | Part 4 |
| Garbler and powers | UNVERIFIED | Part 5; same-account issue |
| Sound | UNVERIFIED | Part 6 |
| Message density | UNVERIFIED | Part 8 |
| Edge strip | UNVERIFIED | Part 9 |
| Small window layout | UNVERIFIED | Part 10 |
| Buddy list shows people and channels | VERIFIED (by chris, screenshot 2026-11-09) | |
| Chat window opens, shows messages | VERIFIED (by chris, screenshot 2026-11-09) | for the server that opened; others fixed in 28fe65f, chris reports "they work visually" |
| Chat window: sending updates the window | UNVERIFIED | c3c4c75; relies on the server echoing your own message back |
| Chat window: DMs | UNVERIFIED | chris: "they work visually" after 28fe65f |

## Portability (Android / Apple)

See `docs/modules/portability.md`. Not a feature to test yet; an audit of what is desktop-only.
