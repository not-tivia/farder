# Relay Phase 5b-client — Voice Over Relay (client half) — Design Spec

**Date:** 2026-06-08
**Status:** Approved (design); ready to plan
**Parent design:** `docs/superpowers/specs/2026-06-07-relay-phase5b-server-voice-over-relay-design.md`
(the "Out of scope (Phase 5b-client)" section — this spec is that deferred half).
**Builds on:** Phase 5b-server (merged) — relay routes voice datagrams by handle;
server demuxes incoming by source handle and fans out tagged by recipient handle.

## Problem

Voice now works over the relay on the **server** side, but the **client** still
refuses voice on relayed servers and cannot carry voice datagrams over its relay
connection. Four things block it: the pinned relay endpoint doesn't enable QUIC
datagrams; the voice datagram recv loop is skipped for relayed connections; the
`voice_join` command hard-refuses relayed servers; and the UI greys voice channels
on relayed servers. This phase removes all four blockers so voice flows
end-to-end over the relay.

## Key fact that makes this trivial

Over the relay, the client sends and receives **raw media frames, byte-identical to
direct mode**. The relay's route path **strips** the 4-byte handle prefix before
delivering a datagram to the client, and the relay's forward path **adds** the
handle when forwarding the client's outgoing datagram to the server. So the client
never sees a handle — its send path (`send_datagram(frame)`) and its recv/dispatch
path (read datagram -> dispatch by the session_id at frame bytes 12..28) are
unchanged. The **same** recv loop and send path work for relayed connections; there
is no client-side handle logic to add.

## Scope — exactly four changes

1. **`client/src-tauri/src/tls.rs` `make_pinned_relay_endpoint`** — add, after the
   existing `keep_alive_interval` line, the two datagram-buffer lines that
   `make_client_endpoint` already has:
   ```rust
   transport.datagram_receive_buffer_size(Some(1 << 20));
   transport.datagram_send_buffer_size(1 << 20);
   ```
2. **`client/src-tauri/src/commands.rs` (~line 425)** — the voice datagram recv loop
   is spawned only `if !relayed`. Remove that guard so the loop runs for **all**
   connections (relayed included). The loop body is unchanged (it reads
   `conn.read_datagram()` and dispatches to `MediaInboundDispatcher`). Update the
   stale comment ("Relayed connections carry no datagrams...") accordingly.
3. **`client/src-tauri/src/commands.rs` (~lines 2200-2201)** — delete the `voice_join`
   refusal `if server_conn.relayed { return Err("voice is not available over a relay
   yet".to_string()); }`. The rest of `voice_join` is unchanged. No Tauri-seam change
   (the command is already registered in `generate_handler!`).
4. **`client/src/components/ChannelSidebar.tsx` (~lines 368-374)** — remove the Phase-4
   relayed-voice gate: the `opacity/cursor` greying, the `title` tooltip, and the
   `if (activeServer?.relayed) { toast.error(...); return; }` short-circuit, so voice
   channels are clickable on relayed servers. Leave the normal (non-relayed) voice
   join behavior intact.

Nothing else changes. The send path (`voice_bridge.rs` `send_datagram` on the
connection), the dispatcher, and the stream-key offer (rides the bridged control
stream) all already work for relayed connections.

## Verification reality (CLAUDE.md verify-before-done)

**This phase ships UNVERIFIED for all runtime/audio behavior.** This environment has
no audio, no display (WSL), and no deployed relay — there is nothing to run against.
The actual behavior (datagrams flow over the relay; member A hears member B) is
**only** provable when the user runs two clients on Windows against a **deployed**
relay. That is the accepted trade-off (the user opted to land the code now).

**Headless checks that ARE green here (the only gates):**
- `cd client/src-tauri && cargo build` — the client crate compiles.
- `cd client && npx tsc --noEmit` — the frontend type-checks.
- Tauri seam: `voice_join` (and the other voice commands) remain in
  `client/src-tauri/src/main.rs` `generate_handler!` — unchanged, so no
  `invoke("...")`/command/handler drift is introduced.

No new unit/integration tests: each change is a config addition, a one-line guard
removal, an error-branch removal, or a UI-gate removal — none has
independently-testable logic without the full Tauri connect + audio stack, which
can't run here.

## Out of scope / deferred

- **Real-audio / two-client end-to-end verification** — the user's Windows +
  deployed-relay run. Until then, voice-over-relay is code-complete but UNVERIFIED.
- Any voice-quality tuning over the relay (jitter, loss handling) — revisit only if
  the real run surfaces problems.
