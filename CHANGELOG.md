# Changelog

All notable changes to this project — bug fixes, features, and learnings.

## [Unreleased]

### Fixed
- (2026-05-03) Self-inflicted disconnect storm on client launch: React StrictMode opened two QUIC connections from the same identity, the server's identity-keyed `clients` map raced, and the first connection's unconditional cleanup evicted the second's entry — killing the live session and producing "failed to write request frame". Fixed with (a) module-level StrictMode guard on App init, (b) same-owner check before `clients.remove` on the server, (c) Tauri exit hook calling `procs.stop_all()` to stop server-process leaks across sessions, (d) reconnect catch now logs the real error.
- (2026-05-03) Channel creation debug: was investigating channel creation failure, debug logging added then removed
- (2026-05-03) Shell plugin config crash: Tauri v2 shell plugin uses `open` field not `scope` in tauri.conf.json. Sidecar permissions go in capabilities/default.json, not plugin config.
- (2026-05-03) Duplicate channel display: creating a channel showed it twice because the reducer blindly appended on CHANNEL_CREATED events. The server broadcasts the event to ALL clients including the creator, causing a double-add. Fixed by deduplicating on channel ID.
- (2026-05-03) Reaction 'me' flag was always undefined — bridge sends public_key not 'me' bool. Fixed by comparing event's public_key against own key in the event listener.

### Added
- (2026-05-03) "Create Thread" option in message right-click context menu

### Added
- (2026-05-03) Auto-claim owner on first connection to empty server — no setup token needed
- (2026-05-03) Server manager module with sidecar spawning and port selection
- (2026-05-03) Server setup UX redesign — two-path "Create a Server" / "Join a Server" flow (in progress)

### Changed
- (2026-05-03) First-run log messages updated: auto-claim is primary, setup token is fallback for headless deployments
