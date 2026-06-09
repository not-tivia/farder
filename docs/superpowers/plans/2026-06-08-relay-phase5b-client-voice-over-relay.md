# Relay Phase 5b-client — Voice Over Relay (client half) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the Tauri client send and receive voice over its relay connection by removing the four blockers (no datagrams on the relay endpoint, recv loop skipped for relayed, `voice_join` refusal, UI gate).

**Architecture:** Pure plumbing. Over the relay the client sees raw media frames identical to direct mode (the relay strips/adds the routing handle), so the existing send path, recv loop, and dispatcher work unchanged — we just stop disabling them for relayed connections and enable datagrams on the pinned relay endpoint.

**Tech Stack:** Rust (Tauri 2, quinn 0.11), TypeScript/React.

**Spec:** `docs/superpowers/specs/2026-06-08-relay-phase5b-client-voice-over-relay-design.md`

---

## Context for the implementer

- **This ships UNVERIFIED for runtime/audio behavior** — no audio, no display (WSL), no deployed relay here. The ONLY gates are: the client crate compiles (`cargo build`), the frontend type-checks (`npx tsc --noEmit`), and the Tauri seam stays aligned (no command changes). Do NOT claim voice works; the changes are mechanical and verified only at compile/type level.
- There is **no testable logic** in these changes (a config addition, a guard removal, an error-branch removal, a UI-gate removal), so there are **no new unit tests** — this is intentional and stated in the spec. Verification is compile + type-check + a seam grep.
- Work from `/home/deez/farder`. The client Rust crate builds with `cd client/src-tauri && cargo build`. The frontend type-checks with `cd client && npx tsc --noEmit`.
- WSL note: `npm run tauri dev` / running the app is NOT possible here (no display/audio) — do not attempt it.

---

## File structure

- `client/src-tauri/src/tls.rs` — enable datagrams on `make_pinned_relay_endpoint`.
- `client/src-tauri/src/commands.rs` — spawn the voice recv loop for relayed connections; drop the `voice_join` relay refusal.
- `client/src/components/ChannelSidebar.tsx` — remove the relayed-voice UI gate.
- `docs/modules/client-relay.md` — update the now-stale "voice not available over relay" notes.

---

## Task 1: Client Rust — enable datagrams, recv loop for relayed, drop the refusal

**Files:**
- Modify: `client/src-tauri/src/tls.rs:127-128` (`make_pinned_relay_endpoint`)
- Modify: `client/src-tauri/src/commands.rs:422-440` (recv loop guard)
- Modify: `client/src-tauri/src/commands.rs:2199-2202` (`voice_join` refusal)

- [ ] **Step 1: Enable datagrams on the pinned relay endpoint**

In `client/src-tauri/src/tls.rs`, in `make_pinned_relay_endpoint`, after the existing
`transport.keep_alive_interval(...)` line (line 128), add the two datagram lines so it
matches `make_client_endpoint`:

```rust
    transport.keep_alive_interval(Some(std::time::Duration::from_secs(15)));
    // Voice (QUIC datagrams) over the relay (Phase 5b-client).
    transport.datagram_receive_buffer_size(Some(1 << 20));
    transport.datagram_send_buffer_size(1 << 20);
```

- [ ] **Step 2: Spawn the voice datagram recv loop for relayed connections too**

In `client/src-tauri/src/commands.rs`, replace the `if !relayed { ... }`-guarded recv
loop (lines 422-440) with an unconditional block (same loop body, updated comment):

```rust
    let media_dispatcher = std::sync::Arc::new(crate::voice::MediaInboundDispatcher::default());
    // Voice datagrams flow over BOTH direct and relayed connections: the relay
    // strips the routing handle before delivering, so the client sees raw frames
    // identical to direct mode (Phase 5b-client).
    {
        let dispatcher_for_loop = media_dispatcher.clone();
        let conn_for_loop = conn.clone();
        tokio::spawn(async move {
            loop {
                match conn_for_loop.read_datagram().await {
                    Ok(bytes) => dispatcher_for_loop.dispatch(bytes).await,
                    Err(quinn::ConnectionError::ApplicationClosed { .. })
                    | Err(quinn::ConnectionError::ConnectionClosed { .. })
                    | Err(quinn::ConnectionError::LocallyClosed)
                    | Err(quinn::ConnectionError::TimedOut) => break,
                    Err(_) => break,
                }
            }
        });
    }
```

(The bare `{ }` block keeps `dispatcher_for_loop`/`conn_for_loop` scoped, mirroring the
old structure. `media_dispatcher` is still moved into `ServerConnection` below — unchanged.)

- [ ] **Step 3: Drop the `voice_join` relay refusal**

In `client/src-tauri/src/commands.rs`, in `voice_join` (lines 2199-2202), remove the
relay refusal. `server_conn` was only used by that refusal, so replace the binding with a
plain existence check (keeps the "unknown server id" error path):

Replace:
```rust
    let server_conn = state.get_server(&server_id)?;
    if server_conn.relayed {
        return Err("voice is not available over a relay yet".to_string());
    }
    // Apply the saved mic sensitivity before the send task spawns.
```
with:
```rust
    // Validate the server exists (the QuinnServerSession adapter looks it up too).
    state.get_server(&server_id)?;
    // Apply the saved mic sensitivity before the send task spawns.
```

- [ ] **Step 4: Build the client crate (compile gate)**

Run: `cd client/src-tauri && cargo build`
Expected: compiles. No `unused variable: server_conn` warning (the binding was removed),
and no new warnings from these changes. If `cargo build` reports the relay endpoint or
recv-loop edits as type errors, fix them to match the existing `make_client_endpoint` /
direct recv-loop patterns.

- [ ] **Step 5: Confirm the Tauri seam is unchanged (no drift)**

Run: `grep -n "voice_join" client/src-tauri/src/main.rs`
Expected: `voice_join` still appears in the `generate_handler!` list (we did not add or
rename any command, so the seam is intact). This is a confirmation, not a change.

- [ ] **Step 6: Commit**

```bash
git add client/src-tauri/src/tls.rs client/src-tauri/src/commands.rs
git commit -m "Client: enable voice datagrams over relay; recv loop + voice_join for relayed (Phase 5b-client)"
```

---

## Task 2: Frontend — remove the relayed-voice UI gate

**Files:**
- Modify: `client/src/components/ChannelSidebar.tsx:364-375` (voice channel render)

- [ ] **Step 1: Remove the gate**

In `client/src/components/ChannelSidebar.tsx`, in `renderVoiceChannel`, remove the
`style` and `title` relayed-conditionals and the `if (activeServer?.relayed) { ... }`
short-circuit. The `<div>` and `onClick` become:

```tsx
        <div
          data-drag-id={ch.id}
          data-drag-type="channel"
          className={`channel-item voice-channel${isInThisChannel ? " active" : ""}${dragOverId === ch.id ? " drag-over" : ""}`}
          onClick={async () => {
            if (!serverId) return;
            if (isInThisChannel) {
              await leaveVoiceChannel(ch.id);
            } else {
```

(Delete exactly: the `style={activeServer?.relayed ? ... }` line, the
`title={activeServer?.relayed ? ... }` line, and the four-line
`if (activeServer?.relayed) { toast.error(...); return; }` block. Leave the rest of the
`onClick` body — the `isInThisChannel` branch and the join logic — untouched.)

- [ ] **Step 2: Remove the `toast` import if it is now unused**

Check whether `toast` is still referenced elsewhere in `ChannelSidebar.tsx`:
Run: `grep -n "toast" client/src/components/ChannelSidebar.tsx`
- If the only remaining hits are the `import`, remove the now-unused `toast` import line
  (tsc with `noUnusedLocals` would otherwise error).
- If `toast` is used elsewhere in the file, leave the import.

- [ ] **Step 3: Type-check the frontend (gate)**

Run: `cd client && npx tsc --noEmit`
Expected: passes with no errors. (If it flags `activeServer` or `toast` as unused, resolve
by removing the now-dead reference/import; do not silence with casts.)

- [ ] **Step 4: Commit**

```bash
git add client/src/components/ChannelSidebar.tsx
git commit -m "Client UI: allow voice channels on relayed servers (Phase 5b-client)"
```

---

## Task 3: Docs + final gates

**Files:**
- Modify: `docs/modules/client-relay.md` (the stale voice notes)

- [ ] **Step 1: Update `docs/modules/client-relay.md`**

This doc currently states (Phase 3a) that voice is refused over relay and the datagram
recv loop is skipped for relayed connections. Find the "Relay-mode behaviour" / "Voice"
section that says something like:
`**Voice:** `voice_join` returns "voice is not available over a relay yet" for a relayed connection ... The datagram recv loop is not spawned for relayed connections.`
and replace it with a note that voice now works over the relay (Phase 5b): the pinned
relay endpoint enables datagrams, the voice datagram recv loop runs for relayed
connections (the client sees raw frames; the relay strips/adds the routing handle), and
`voice_join` no longer refuses relayed servers. Also update the "Relay UX (Phase 4)" voice-
disabled bullet and the "Trust / limits" `Voice over relay is deferred.` line to say voice
over relay is now implemented (server + client), with **real-audio end-to-end still
UNVERIFIED until a Windows + deployed-relay run**. Make surgical edits; don't rewrite
unrelated parts.

- [ ] **Step 2: Final gates**

Run both and confirm green:
- `cd /home/deez/farder/client/src-tauri && cargo build` — client crate compiles.
- `cd /home/deez/farder/client && npx tsc --noEmit` — frontend types pass.

(Optional, if quick: `cargo test --workspace` from the repo root to confirm nothing else
regressed — these changes don't touch server/relay/protocol, so it should be unaffected.)

- [ ] **Step 3: Commit**

```bash
git add docs/modules/client-relay.md
git commit -m "Docs: client voice-over-relay enabled (Phase 5b-client)"
```

---

## Final verification

- [ ] `cd client/src-tauri && cargo build` — green.
- [ ] `cd client && npx tsc --noEmit` — green.
- [ ] `grep -n "voice_join" client/src-tauri/src/main.rs` — still registered (seam intact).
- [ ] Spec coverage: datagrams on relay endpoint (T1S1); recv loop for relayed (T1S2); `voice_join` refusal removed (T1S3); UI gate removed (T2); docs (T3).
- [ ] **UNVERIFIED, by design:** real audio + datagrams-actually-flow + "member A hears member B over the relay" — the user's Windows + deployed-relay run. State this plainly; do not imply voice-over-relay is proven.

After all tasks: use **superpowers:finishing-a-development-branch** to complete the work.
```

