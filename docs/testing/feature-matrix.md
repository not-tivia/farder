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

Last updated: 2026-11-26.

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
| Encrypted channels (two online apps, text) | BROKEN after the first exchange | `ubuntu-suite.sh e2ee` (2026-11-26). WORKS: owner makes it in Server Settings, both apps show the lock, A and B each read the other's first message, and no file the server wrote holds the words (a normal-channel line IS found, so the search can see). BROKEN: right after B's first message B authors an `MlsCommit` the server ACCEPTS (event log), A's steward then reports `processed: 0` and stays one epoch behind, and from then on no message opens in either direction. Pre-existing: reproduced with today's early key-package publish switched off. Fixed on the way: pre-Welcome message cached as "couldn't decrypt" forever; pre-join message looked like tampering; friend missed the first message. |
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

## Recovery owner (cold key that can take the server back)

| Item | Status | Evidence |
|---|---|---|
| Log rules: name a key, claim, cancel, finish; owner key cannot cancel; recovery key cannot be banned | VERIFIED | 8 unit tests in `recovery_owner_tests`, 4 deliberate breaks caught |
| Server: waiting period on the server's clock, owner read per request, audit trail | VERIFIED | handler test (break caught) + real run |
| App: make key (24 words once), see countdown, start/finish takeover, ownership moves, old owner refused while still connected | VERIFIED | selftest recovery, 7 checks (Windows clients, Ubuntu server, 30 s wait) |
| Banner for every member + nudge for owners without a key; valid-but-wrong phrase refused | VERIFIED | selftest recovery, 9 checks |
| Host-side owner reset (`--reset-owner`): old messages kept, new owner has powers, old owner refused incl. after rejoin and restart | VERIFIED | selftest `ubuntu-suite.sh reset`, 11 checks; 3 unit tests |
| Rotate the recovery key in the app; old words stop working | VERIFIED | selftest recovery, 10 checks |
| Bulk-damage brake: 5 destructive changes per account per 10 min, then "slow down" + audit entry | VERIFIED | 3 unit tests (break caught); `ubuntu-suite.sh brake`, 3 checks on the real stack |
| Takeover pop-up for every member (in-app toast, once per takeover) | VERIFIED | selftest recovery, 10 checks |
| "Reset by its host, rejoin" message for members left out by a host reset | VERIFIED | selftest `ubuntu-suite.sh reset` + unit test |
| Cancellable waiting period for destructive acts; OS-level notification for a takeover; a dedicated reset screen | NOT BUILT | see docs/recovery-owner-design.md |

## Hosting on Ubuntu

| Item | Status | Evidence |
|---|---|---|
| Server runs headless on Ubuntu, prints a setup token | VERIFIED | ran on Ubuntu 24.04 (2026-11-15); 1010 tests pass there |
| Windows client claims a Linux-hosted server and chats | VERIFIED | selftest ubuntu (4 checks), direct connection |
| Backup (automatic, rotated), restore, wipe-and-restore on a Linux server with two clients reconnecting | VERIFIED | 7 unit tests (3 deliberate breaks caught) + real run (selftest ubuntu + ubuntu-restored; server DB read directly) |
| Chat box recovers after the server restarts (request fails at once, 60s limit) | VERIFIED | found by the restore run (box froze disabled); fixed; ubuntu-restored passes |
| systemd: starts, setup token in the journal, auto-restarts after kill -9, stops cleanly (as a USER service, the unit's own settings) | VERIFIED | run 2026-11-15 on Ubuntu 24.04 (WSL) |
| systemd system install with `User=farder`, over the internet, long uptime | UNVERIFIED | needs sudo / a real host; never installed |

See `docs/hosting-on-ubuntu.md`.

## Portability (Android / Apple)

| Item | Status | Evidence |
|---|---|---|
| Shared Rust crates + the app crate type-check for Android (NDK) | VERIFIED | `scripts\portability_check_android.ps1 -Client`, 2026-11-14 |
| Android APK builds | VERIFIED | `scripts\android_apk.ps1` produced app-arm64-debug.apk (2026-11-15) |
| Android app installs and runs | UNVERIFIED | never installed anywhere; needs an emulator or phone the owner agrees to use (see portability.md) |
| iOS | UNVERIFIED | pure crates type-check; the rest needs a Mac and the Apple SDK |

See `docs/modules/portability.md`.

## Browser guest links (docs/web-guest-join-design.md)

| Feature | Status | Evidence |
|---|---|---|
| Server: guest invite type, owner switch (off by default), expiry, permission cap | VERIFIED (unit) | 2 tests, cap break caught |
| Gateway: WebSocket to the real server, guest joins with its own key, limited | VERIFIED | `cargo test -p farder-gateway`, 6 tests incl. real server; pin break caught |
| Refusal reasons reach a refused login (expired link etc.) | VERIFIED | same real-server test |
| Browser guest page: WebAssembly crypto, joins a server with a signed history with its OWN key, reads, writes, live messages, text-not-HTML, refresh keeps the guest, wrong code refused | VERIFIED | `bash scripts/selftest/guest-suite.sh`: real Edge, real server, via gateway, 9 checks |
| A guest cannot use the signed history for anything but join/post | VERIFIED (unit) | test + deliberate break caught |
| Caddy routes `/<name>/t` and `/ws/*` over real TLS | UNVERIFIED | written in `deploy/website/Caddyfile`, never run |
| Owner's in-app screen for guest links: switch, make a link (duration, web name), copy; link then works in a real browser; both apps see the guest and their messages | VERIFIED | `ubuntu-suite.sh guestlink`: 2 + 9 + 2 checks (Windows apps, Ubuntu server, gateway, real Edge) |
| Guest page on a phone browser | UNVERIFIED | responsive CSS only |

## Mesh steps 1-2: a verified copy on another machine (docs/mesh-hosting-status.md)

| Feature | Status | Evidence |
|---|---|---|
| `FetchLog` (owner/replica only, paged, with genesis) | VERIFIED (unit) | 2 tests |
| Owner allows replicas; verified database snapshots in pieces; replica cannot make replicas | VERIFIED (unit) | handler test + snapshot tests |
| `farder-replica` against the real server: refused until allowed, verified copy with data, bad copy rejected, revoke stops copies | VERIFIED | `cargo test -p farder-replica` |
| Attachments (files) copied, checked against their hash, redacted ones removed; the read-only copy serves them | VERIFIED (real server, no app) | `cargo test -p farder-replica` (2026-11-26); not yet clicked in the app on a copy |
| Owner allows a copy machine from the app (Server Settings > Backup copy, matched by its short key) | VERIFIED | `ubuntu-suite.sh failover` (2026-11-26) |
| `farder-replica --invite <the app's join link>` (address taken from the link) | VERIFIED | same suite: exit 3 before allowed, exit 0 + copy after; unit tests for the link forms |
| Read-only copy keeps the host's server name (name saved in the database) | VERIFIED | `cargo test -p farder-replica` (proven to fail without the fix) + suite |
| Host killed (`kill -9`): the app moves to the backup address, shows the banner, reads history FROM the copy, sending refused with the plain reason | VERIFIED | same suite: on the copy after 36 s; route + history checked through the connection, not the screen; nothing written to either database |
| Host back: the app leaves the copy by itself and sends to the host | VERIFIED | same suite: banner gone after 11 s, A sees B's message |
| Promotion (a copy becomes the host) | NOT BUILT | |
