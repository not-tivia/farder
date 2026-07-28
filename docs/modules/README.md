# Module docs index

Per-module documentation for Farder. Start with the top-level
[`ARCHITECTURE.md`](../../ARCHITECTURE.md) for the system map, then read the
module relevant to what you're touching. New docs use
[`_TEMPLATE.md`](_TEMPLATE.md). Keeping these current is required — see the
"Documentation discipline" section in [`CLAUDE.md`](../../CLAUDE.md).

## Tauri client backend (`client/src-tauri/src/`)
- [`tauri-commands.md`](tauri-commands.md) — every `#[tauri::command]`, grouped by area, + the `invoke()` seam.
- [`tauri-bridge.md`](tauri-bridge.md) — the server-event → UI-event bus (`bridge.rs`) and `send_request`.
- [`tauri-voice.md`](tauri-voice.md) — the local voice engine (`VoiceController`, audio pipeline, cpal device layer).

## Server (`crates/farder-server/`)
- [`server-handlers.md`](server-handlers.md) — request dispatch, per-request permissions + DB effects + event broadcasts.
- [`server-connection.md`](server-connection.md) — QUIC connection lifecycle, broadcast, voice media relay, DB schema.
- [`server-permissions.md`](server-permissions.md) — auth + the permission model (flags, role resolution, overrides).
- [`server-widgets.md`](server-widgets.md) — interactive message widgets: polls, giveaways, and the shared sweeper (`polls.rs`, `giveaways.rs`, `widgets.rs`).

## Shared crates
- [`protocol.md`](protocol.md) — the wire contract: `ServerRequest` / `ServerResponse` / `ServerEvent` catalogs + shared structs.
- [`crypto.md`](crypto.md) — identity (Ed25519), DM E2EE (X25519 + AES-GCM), voice media key wrapping.

## Frontend (`client/src/`)
- [`frontend-bridge.md`](frontend-bridge.md) — the typed `invoke()` wrappers (`tauri-bridge.ts`) + shared TS types.
- [`frontend-state.md`](frontend-state.md) — `ServerContext` reducer, `useServerEvents`, `useVoice`.
- [`frontend-toast.md`](frontend-toast.md) — app-wide toast notifications (`toast.error/success/info`).
