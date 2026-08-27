# Mesh Rung 2 — Sub-project 4b: E2EE GUI — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** Make encrypted channels user-visible and usable in the Tauri client, wrapping the shipped `farder-e2ee-client` crate (sub-4a). The owner can create an encrypted channel, see it marked, send messages that seal, and read decrypted replies — with fail-closed rendering when something can't be verified.

**Verifiability (this is the honest constraint):** the client GUI cannot render from WSL, so 4b is verified by **compile + mechanical seam audit + reuse of 4a's already-harnessed logic**, NOT by a runtime run here. The owner's Windows run is the final gate and is called out explicitly.

**Spec:** `docs/superpowers/specs/2026-07-27-mesh-rung2-e2ee-design.md` (rev 2), sub-project 4, "Client changes" (lines 469-481) and the five UI states.

**Baseline:** `main` @ `6035d38` (4a merged+pushed). `cargo test --workspace` = 839 green. Client crate and `npx tsc --noEmit` both green at 4a.

---

## Architecture decisions (the shape of 4b)

**D1 — the `E2eeTransport` is implemented over the existing `bridge::send_request`, not new Tauri commands.** `bridge::send_request(state, server_id, ServerRequest)` (`client/src-tauri/src/bridge.rs:13-27`) already takes the whole request enum, so the transport impl builds the v2 variants (`FetchWelcomes`/`FetchMlsControl`/`FetchKeyPackages`/`FetchDeviceCerts`/`FetchHistoryV2`/`SubmitEvent`/`NegotiateProtocol`) and maps `ServerResponse::Error { reason }` → `TransportError::rejected`, unexpected → `transport(...)`. Zero new wire machinery for the transport itself. The 6 fetch surfaces and their request/response shapes are fixed at `crates/farder-protocol/src/server.rs:625-655` (requests) / `747-778` (responses).

**D2 — decryption is a per-message Tauri command, because the ratchet lives in Rust.** `MlsChannelGroup` + `FarderMlsStore` (and the consume-on-open ratchet) live in the Tauri process. So a sealed message arrives as `SealedMessage { channel_id, message: MessageInfoV2 }`, the frontend stores the sealed row, then invokes `decrypt_sealed_message` once per sealed message; the command loads the group, calls `receive_sealed`, and returns the plaintext envelope or an `undecryptable` marker. The ratchet constraint (a second `open_message` is impossible and destructive — 4a made `receive_sealed` take ciphertext by value) is satisfied because each ciphertext is handed to the command exactly once and the result is cached in frontend state. Decrypted content is NOT persisted to disk in 4b (see D4).

**D3 — `NegotiateProtocol` happens automatically in Rust on connect.** Negotiation is per-connection and cleared on disconnect, and absent it every v2 request returns `UPGRADE_REQUIRED` and v1 `GetServerInfo` silently omits sealed channels. So `connect_server`/`restart_local_servers` negotiate (client_version 2) right after a successful connect, and the frontend's `get_server_info` switches to a v2 path. This makes the client E2EE-aware on every connect with no frontend ceremony.

**D4 — no PIN-wrapped local store in 4b.** Decrypted content is held in frontend memory (a per-message map) for rendering; the PIN-wrapped local history store and client-side search are sub-7's scope. This keeps 4b tractable and is honest: an unlocked device already holds plaintext in memory; sub-7 is where it gets encrypted at rest. Search is deferred with it.

**D5 — channel class is immutable and set at creation, via a checkbox in the existing create form.** No class-change feature. The explainer copy (what "encrypted" means vs the plaintext caveat) lives in the create form.

---

## Authority note — the seams, from recon (cite these, don't re-derive)

1. **The client has zero v2 wiring today.** `client/src-tauri/Cargo.toml` depends only on `farder-crypto`+`farder-protocol` (lines 13,15) — NOT `farder-e2ee-client`/`farder-mls`. No `NegotiateProtocol`/`GetServerInfoV2`/`FetchHistoryV2` command exists. `ChannelInfo` (`client/src/lib/types.ts:1-12`) has no class field; `MessageInfo` (`types.ts:64-84`) has no `is_e2ee`/`sealed`/`event_hash`. `bridge.rs` `dispatch_event` (`:62-224`) drops every v2 `ServerEvent` into `_ => Ok(())` (`:222`).
2. **The server side is already shipped.** `ChannelInfoV2 { base, class }` / `MessageInfoV2 { base, is_e2ee, sealed, event_hash }` (`farder-protocol/src/server.rs:184-201`); `GetServerInfoV2` handler (`handlers.rs:1437-1478`), `FetchHistoryV2` (`:1480-1493`); the v2 events `SealedMessage`/`SealedMessageEdited`/`MessageTombstoned`/`MlsControlEvent`/`ChannelCreatedV2` (`server.rs:902-915`) are already emitted by 4a and gated by `event_requires_v2`.
3. **E2EE channels are created by a LOG event, not `CreateChannel`.** `create_e2ee_channel` in the crate (`channel.rs:281`) submits `ChannelCreated { class: E2ee }`; the legacy `create_channel` command (`commands.rs:1671`) is `ServerRequest::CreateChannel` and must NOT be used for encrypted channels.
4. **Send routing today:** `MessageInput.handleSend` (`MessageInput.tsx:285-301`) branches `submitEvent` (mesh log, `MessagePosted`) vs `sendMessage` (legacy) on `logServerId && !dm && !hasUncappableAttachment`. A sealed send needs a THIRD branch keyed on channel CLASS, with the legacy `fetchUrl`/emoji/DM paths excluded.
5. **The channel list has no class.** `ChannelSidebar.renderChannel` (`:474-498`) renders name at `:495`; the lock icon goes there and in `ChatPanel`'s `.channel-header` (`ChatPanel.tsx:122-127`).
6. **Fail-closed precedents:** deleted message (`Message.tsx:611-613`), redacted attachment (`:947-955`), widget-unavailable (`:439,469-470`). The "N messages could not be verified" marker is list-level (`ChatPanel.tsx:152-164`); the gated interstitial precedent is `AppShell.tsx:133-168` (pending-approval waiting screen).
7. **The untyped seam is the risk.** Every `invoke("X")` in `tauri-bridge.ts` must match a registered `#[tauri::command] fn X` in `main.rs:88-301` — no compiler checks it, and a voice-channel join shipped broken exactly this way. The seam audit is a task, not a step.
8. **Identity/device/chain already loadable in commands** (`commands.rs:4223-4233` pattern: `state.signing_key_bytes` → `Keypair`, `load_or_create_device_keypair()`, `state.device_chain_lock`, persisted `DeviceState` in `device.rs:49-109`). The crate's `Actor { device, identity, log_server_id }` + `ChainState` map 1:1 onto these.

---

## THE SPLIT — 4b-1 (plumbing, compile+audit verified) then 4b-2 (crypto flow, Windows-verified)

4b is large. It splits on the verifiability line:

- **4b-1 = make the client E2EE-AWARE without yet encrypting.** Deps, transport impl, negotiate-on-connect, v2 info/history, TS types, bridge+listener wiring, thread `class` through state, render the lock icon + "encrypted message" placeholders for sealed rows (decrypt not yet wired). Fully verifiable here by compile + seam audit + 4a's logic.
- **4b-2 = the crypto flow.** Create-with-class UI, KeyPackage publication, the steward (fetch/process incoming commits, confirm leaf), sealed send, decrypt command, the five states, composer affordance, theming. This is what the owner's Windows run exercises.

Both land on one branch, merged together at the end (or after 4b-1 if the owner wants an intermediate checkpoint).

---

## Tasks

### 4b-1 — foundation & awareness

- [ ] **T1. Deps + transport.** Add `farder-e2ee-client` + `farder-mls` to `client/src-tauri/Cargo.toml`. Implement `E2eeTransport` in a new `client/src-tauri/src/e2ee_transport.rs` over `bridge::send_request` (D1). Unit-test the `ServerResponse` → `TransportError` mapping (esp. `stale-epoch`).
- [ ] **T2. Negotiate-on-connect + v2 info/history.** `connect_server`/`restart_local_servers` send `NegotiateProtocol { client_version: 2 }` after connect (D3). Add `get_server_info_v2` and `fetch_history_v2` Tauri commands returning `ChannelInfoV2`/`MessageInfoV2`, registered + bridged. The frontend switches its connect-time info fetch to v2 and history to v2 for E2EE channels.
- [ ] **T3. TS types + state threading.** Add `ChannelInfoV2`/`MessageInfoV2`/`ChannelClass` to `types.ts`; thread `class` into `PerServerState.channels` (populated on CONNECTED/SERVER_REFRESHED/SERVER_ADDED); add `is_e2ee`/`sealed`/`event_hash` to the message model. Reducer actions unchanged where possible.
- [ ] **T4. Bridge + listeners.** Add `bridge.rs` `dispatch_event` arms for the five v2 events (`SealedMessage`, `SealedMessageEdited`, `MessageTombstoned`, `MlsControlEvent`, `ChannelCreatedV2`) → `server:sealed_message` etc.; add `useServerEvents.ts` handlers that fold them into state (sealed rows added, tombstoned removed, channel class updated).
- [ ] **T5. Class indicators + sealed placeholders.** Lock icon + class copy in `ChannelSidebar` and `ChatPanel` header; a sealed row renders an "encrypted message" placeholder (not empty content) until decrypt lands in 4b-2.
- [ ] **T6. SEAM AUDIT (a task with teeth).** A scripted cross-check (python) that extracts every `invoke("...")` string from `tauri-bridge.ts`, every `#[tauri::command] fn` + `generate_handler!` entry from `main.rs`, and every `listen("server:...")` from `useServerEvents.ts`, and asserts 1:1 consistency on all three. Plus `cargo build` (workspace AND the separate `client/src-tauri` crate) and `npx tsc --noEmit`. This is the untyped seam the project has broken before; it must be mechanically closed.

### 4b-2 — the crypto flow

- [ ] **T7. Create-with-class UI.** Checkbox "Encrypted" in `ServerSettingsDialog` create form + explainer copy; route to `create_e2ee_channel` (owner bootstraps, publishes own KeyPackage). A per-channel local state file (`mls_state.json` beside `device_state.json`) records `{generation, epoch, store_instance_hash, confirmed}` — the analog of `DeviceState` for the MLS group.
- [ ] **T8. KeyPackage publication + membership.** Members publish their KeyPackage (`publish_key_package`) when they join a server / first see an E2EE channel. On E2EE channel creation, the owner's client adds current server members: `fetch_key_packages` → `decode_key_package` → `add_member` → `MlsWelcome`.
- [ ] **T9. The steward (receive-side vertical).** On `MlsControlEvent` for a channel (and on channel open), fetch `fetch_mls_control` in order and `process_incoming_commit` each (Gate 1 + Gate 2 via a `build_cert_resolver` over the transport), then confirm our own leaf. A `LeafBindingFailure` is POISONED (per 4a finding F4) — surface the equivocation state, never continue. This is the piece that makes a member advance when someone else commits.
- [ ] **T10. Sealed send + decrypt commands.** `send_sealed_message` command (build envelope → `send_sealed` → on `stale-epoch` run the bounded `send_sealed_resync`); `decrypt_sealed_message` command (load group → `receive_sealed` → plaintext envelope or undecryptable). `MessageInput.handleSend` gains the sealed branch keyed on channel class, with legacy `fetchUrl`/emoji/DM paths excluded. The decrypt result is cached in frontend state (D2, D4).
- [ ] **T11. The five UI states + composer affordance.** Channel interstitials in `ChatPanel` for "waiting for keys" / "no history before you joined" / "channel needs a key refresh"; a non-dismissible banner for "channel state could not be confirmed"; the "N messages could not be verified" list marker; per-message fail-closed placeholder; the distinct encrypted-composer affordance (placeholder "Encrypted message…" + accent border).
- [ ] **T12. Theming ×3.** Every new class (lock icon, E2EE accent, interstitials, composer border, sealed placeholder) defined in all three `theme.css` files via `var(--xp-…)`; add `--xp-e2ee` (or reuse an existing accent) to all three `:root` blocks. No hard-coded colors.

---

## Gates (every task)

- `cargo build --workspace` — no new warnings.
- `cd client/src-tauri && cargo build` — the client crate builds SEPARATELY (workspace build does not cover it).
- `cd client && npx tsc --noEmit` — clean.
- `cargo test --workspace` stays ≥ 839 green (server-side code unchanged; any server edit is a red flag).
- `git ls-files --eol` after any scripted edit (CRLF has been destroyed before).

## Owner's Windows runbook (the part I cannot do)

After 4b-2, full rebuild incl. sidecar: `cargo build -p farder-server` → `copy-sidecar.ps1` from repo root → `npm run tauri dev` → Ctrl+Shift+R. Then, with two identities (the `$env:FARDER_DATA` second-instance trick from the mesh memory):
1. Create a server, create an **Encrypted** channel → lock icon shows; the plaintext-explainer appeared once.
2. Second identity joins the server → gets auto-added to the channel → "waiting for keys" resolves → both see the channel.
3. A posts in the encrypted channel → B sees it decrypted (not a placeholder); B replies → A decrypts.
4. Kill B's key state / simulate an undecryptable message → the fail-closed placeholder shows, never garbage.
5. Both confirm the encrypted composer is visually distinct (accent border + "Encrypted message…").
6. A posts in a PLAINTEXT channel → behaves exactly as before (no regression).

**The thing the runbook must catch that I can't:** the `invoke()`↔command seam, the actual decrypt rendering, and the theme CSS actually applying — the three surfaces no WSL gate can prove.

## Review discipline

The standing rule applies: verify every load-bearing guard by breaking it and watching its test fail, in a scratch worktree. In 4b the load-bearing items are the **seam audit** (break it by removing a registration and confirm it flags it) and the **decrypt-once** command contract (a second decrypt is structurally refused). A whole-branch review before merge, with the standard fix-review pass on any security-relevant fix.
