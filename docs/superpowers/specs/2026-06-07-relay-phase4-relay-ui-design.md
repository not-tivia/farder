# Relay Phase 4 — Relay UX Polish — Design Spec

**Date:** 2026-06-07
**Status:** Approved (design); ready to plan
**Parent design:** `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md` (Phase 4)
**Depends on:** Phases 1–3b (relay end-to-end + invite links) — all merged.

## Problem

The relay feature works end-to-end (join a relayed server via an invite link), but
two rough edges remain in the join experience:
1. **Voice** is refused over a relay (Phase 3a returns an error), but the UI still
   presents voice channels as joinable — the user only finds out by clicking and
   getting an error.
2. **Invite links auto-join** (Phase 3b) with no confirmation — clicking a
   `farder.gg/join/...` link silently connects to and joins a server.

## Goal

Polish the relay *join* experience: disable voice on relayed servers in the UI
(honest before the click), and add a "Join this server?" confirmation before a
deep-link invite connects.

## Scope decision (decomposition)

The originally-envisioned third piece — **creating a relayed server in the app** —
is **deferred**. It is heavy (reworks the local-server spawn/probe/auto-claim flow,
which assumes a direct localhost server) and effectively blocked: a relay-only
server has no localhost listener, so the owner would connect via the relay, needing
a relay address + the relay's cert fingerprint — and the **hosted default relay is
not deployed**, so a non-technical user has no relay to point at (a technical user
would use the CLI `--relay` flag). It will be revisited once the default relay is
deployed. **This phase is pieces 2 + 3 only.**

## Decisions (settled)

| Decision | Choice |
|----------|--------|
| Voice on relayed servers | **Disabled with a toast** ("Voice isn't available over a relay yet") + a greyed/disabled style on voice channels — NOT hidden entirely (the channels are part of the server structure). |
| Invite confirm | A generic **"Join this server?" (Join / Cancel)** modal before connecting (a relay server's name is opaque until connected). Applies to both relay and direct invite links. |
| Surfacing `relayed` | Add `relayed: bool` to `ConnectResult`; thread into the frontend `PerServerState`. |

## Architecture

### Surfacing `relayed` to the frontend

- `ConnectResult` (`client/src-tauri/src/commands.rs:44`) gains `pub relayed: bool`.
  `connect_server` already computes the `relayed` flag in its connect branch
  (Phase 3a) — set it on the returned `ConnectResult`. Direct/local paths set
  `false`. Any other `ConnectResult`/connect-payload construction (e.g.
  `create_local_server`'s JSON payload) sets `relayed: false`.
- TS `ConnectResult` type (`client/src/lib/types.ts`) gains `relayed?: boolean`.
- `PerServerState` (`client/src/context/ServerContext.tsx:5`) gains `relayed: boolean`;
  the `SERVER_ADDED` reducer copies it from the payload (defaulting `false` when
  absent, for backward-compatible payloads).

### Piece 2 — voice disabled on relayed servers

In `ChannelSidebar.tsx` (the voice-channel join is at `:378`,
`await api.joinVoice(serverId, ch.id)`):
- Read the active server's `relayed` from `PerServerState`.
- When `relayed`, the voice-channel row is rendered with a disabled/greyed style,
  and its click handler shows `toast` "Voice isn't available over a relay yet"
  instead of calling `joinVoice`. (The backend already refuses it; this makes the
  UI honest before the click.)
- Text channels and everything else are unaffected.

### Piece 3 — "Join this server?" confirm on invite links

In `App.tsx`, the pending-invite effect (Phase 3b) currently connects immediately.
Change it to:
- When `unlocked && pendingInvite`, instead of connecting, set a `joinConfirm`
  state holding the invite URL (and clear `pendingInvite`).
- Render a confirm modal when `joinConfirm` is set: generic copy ("You've been
  invited to a Farder server. Join?"), **Join** and **Cancel** buttons (reuse the
  existing modal/dialog styling, e.g. the `modal-overlay`/`modal-dialog` classes).
- **Join** → run the existing connect + dispatch sequence (the Phase 3b logic),
  then clear `joinConfirm`. **Cancel** → clear `joinConfirm`, do nothing.
- A small `JoinConfirmModal` component (or inline in App) — keep it focused.

## File structure

- `client/src-tauri/src/commands.rs` — `ConnectResult.relayed` + set it in
  `connect_server` (and `relayed: false` in `create_local_server`'s payload).
- `client/src/lib/types.ts` — `ConnectResult.relayed?: boolean`.
- `client/src/context/ServerContext.tsx` — `PerServerState.relayed` + `SERVER_ADDED`.
- `client/src/components/ChannelSidebar.tsx` — voice-disable on relayed.
- `client/src/components/JoinConfirmModal.tsx` *(new, or inline)* — the confirm modal.
- `client/src/App.tsx` — route pending invite through the confirm modal.

## Error handling

- A relayed server with `relayed` missing from an older payload → treated as
  `false` (voice works as before; no crash). Acceptable — `connect_server` always
  sets it going forward.
- Connect failure after confirming Join → the existing error path (toast/log) from
  the Phase 3b connect sequence.
- Cancel → no connection attempted; the invite is dropped (the user can re-click
  the link).

## Testing

- **Rust (headless):** assert `connect_server`'s `ConnectResult.relayed` is `true`
  for a relay-link address and `false` for a direct address. (If a full
  connect_server test is impractical without a server, at minimum a focused test of
  the relayed-flag plumbing, or rely on the existing relay integration coverage +
  a compile-time guarantee that the field is set in both branches.)
- **Frontend:** `npx tsc --noEmit`. The voice-disabled styling/toast and the
  confirm modal are visual/runtime behavior — **GUI-verified on the Windows build**
  (cannot render in WSL), flagged UNVERIFIED here, like prior phases.

## Out of scope / deferred

- **Creating a relayed server in the app** (piece 1) — deferred until the hosted
  default relay is deployed (its prerequisite).
- Voice over relay — later phase.
- Deploying the hosted default relay — ops.
