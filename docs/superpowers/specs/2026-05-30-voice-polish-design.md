# Voice Polish — Design

**Date:** 2026-05-30
**Status:** Approved (design); implementation plan to follow.
**Scope:** Three self-contained voice-UI features — push-to-talk (tap-to-toggle), per-peer volume, and a connection-quality meter. Auto-reconnect is intentionally a **separate** follow-up spec (it rewires the core connection lifecycle and carries different risk).

## Background

The voice subsystem currently supports join/leave, mute, deafen, speaking indicators, and peer mute/deafen broadcast (shipped in the 2026-05-28 voice-ui work). The control bar (`client/src/components/VoiceControlBar.tsx`), the `useVoice` hook (`client/src/hooks/useVoice.ts`), the participant list in `client/src/components/ChannelSidebar.tsx`, and the Rust `VoiceController` (`client/src-tauri/src/voice/mod.rs`) are all in place.

Three pieces of groundwork already exist and make these features cheap:

- **PTT gate:** `client/src-tauri/src/voice/gate.rs` defines `GateMode::{Open, Vad, Ptt(Arc<AtomicBool>)}` and the send task honors it — but the pipeline hardcodes `GateMode::Open` at `client/src-tauri/src/voice/mod.rs` (~line 281).
- **Mixer mix point:** `client/src-tauri/src/voice/mixer.rs` `mix_one_frame()` sums each peer's PCM ring with equal weight — the single place to apply per-peer gain.
- **QUIC stats:** the client holds its `quinn::Connection` in `ServerConnection` (`client/src-tauri/src/state.rs`); `connection.stats()` exposes RTT and packet counts but is never read.

**Hardware constraint:** the dev box is WSL2 with no audio devices, so the engine runs on a mock audio backend. All *logic and UI* is verifiable here (unit tests + typecheck); a live two-party audio test requires real hardware and is out of scope for automated verification.

## Conventions followed

- New `settings.json` fields use `#[serde(default)]` for backward compatibility (project convention — see prior CHANGELOG entries).
- Unicode emoji/icons in UI go through existing rendering paths; no non-ASCII in source where the project avoids it.
- Frontend has no JS test runner; frontend verification is `npx tsc --noEmit` from `client/`. Rust uses full unit tests (TDD).
- Commit each feature independently.

---

## Feature 1: Push-to-talk (tap-to-toggle)

### Behavior

- A new **Voice** tab in the existing tabbed Settings modal exposes a mic-mode choice: **Open Mic** (current default — always transmitting unless muted) or **Push-to-Talk**.
- In PTT mode the mic gate starts **closed** (not transmitting). Tapping the bound key **toggles** transmission on; tapping again toggles it off. (Tap-to-toggle, not hold.)
- The voice control bar shows transmit state explicitly: a live indicator (e.g. red dot "Transmitting") vs. off (e.g. grey "Mic off — tap <key> to talk"), including the bound key hint.
- **Mute precedence:** mute is absolute. When muted, toggling transmit on has no audible effect until unmuted. PTT gate and mute are independent flags; the send path already accepts both.
- **Scope:** in-app only for v1 — the key is captured while the Farder window is focused. OS-global hotkeys (transmit while another window is focused) are deferred to a later enhancement; for a *toggle* this is a minor limitation.
- **Default key:** `` ` `` (backtick), rebindable in the Voice tab via a key-capture control.

### Data model

`settings.json` gains (all `#[serde(default)]`):

- `voice_mode: VoiceMode` — enum `{ OpenMic, PushToTalk }`, default `OpenMic`.
- `ptt_key: String` — a normalized key identifier (e.g. browser `KeyboardEvent.code` like `Backquote`), default `"Backquote"`.

### Components & flow

- **Rust gate plumbing:** the pipeline spawn (`voice/mod.rs` `AudioPipelineFactory::spawn`) currently builds `GateMode::Open`. Change it to accept a gate decision from the `VoiceController`. The controller owns a `transmit: Arc<AtomicBool>` and chooses:
  - Open Mic mode → `GateMode::Open` (transmit flag ignored).
  - PTT mode → `GateMode::Ptt(transmit.clone())`, with `transmit` initialized `false` on join.
- **Tauri command:** `voice_set_transmitting(transmitting: bool)` (or `voice_toggle_transmit() -> bool` returning the new state) flips the atomic and emits an updated `VoiceState` so the UI reflects it. Mode is read from settings at join time.
- **`useVoice` hook:** expose `transmitting: boolean` (from `VoiceState`) and `toggleTransmit()`.
- **Keybind listener:** a window-level `keydown` listener (active only while in a call AND mode is PTT) matches `event.code` against `ptt_key` and calls `toggleTransmit()`. Ignores repeats and typing in text inputs.
- **Settings Voice tab:** radio for mode + a key-capture button that records the next keypress as `ptt_key`.

### Edge cases

- Switching mode mid-call: if a future need arises, mode is read at join; v1 reads mode on join and does not hot-swap the gate mid-call (changing mode takes effect on next join). Documented limitation; keeps the gate immutable for the call's lifetime.
- Deafen auto-mutes (existing behavior) — unchanged; PTT layers on top of the existing mute flag.

### Tests (Rust)

- Gate already unit-tested (`gate.rs`, `send.rs`). Add: controller selects `Ptt` gate when settings mode is PTT and `Open` otherwise; `voice_set_transmitting` flips the atomic and the gate reports open/closed accordingly.

---

## Feature 2: Per-peer volume

### Behavior

- Right-click a participant in the voice list → a **Volume** control (slider) in the context menu, matching the existing `MemberContextMenu` pattern.
- Range **0%–200%**, default **100%**. 0% locally silences that person only (distinct from their self-mute).
- **Persistence:** keyed by the peer's public key (hex) in `settings.json`, so a volume set for someone persists across calls, rejoins, and restarts.

### Data model

`settings.json` gains (`#[serde(default)]`):

- `peer_volumes: HashMap<String, f32>` — map of `pubkey_hex` → gain (1.0 = 100%). Absent entry = 1.0.

### Components & flow

- **Mixer gain:** `mix_one_frame()` in `mixer.rs` applies a per-peer gain when accumulating: `acc[i] += frame[i] * gain`. Each peer ring carries (or is paired with) a shared gain value the mixer reads each frame.
- **Gain storage at runtime:** peer rings are keyed by `SessionId` (`PeerRings` in mixer). Each registered ring gains an associated atomic gain (e.g. `Arc<AtomicU32>` holding `f32::to_bits`), defaulting from the persisted `peer_volumes[pubkey]` at peer-join time (the controller knows `session → pubkey` via `peer_keys`).
- **Tauri command:** `voice_set_peer_volume(pubkey_hex: String, volume: f32)` — clamps to `[0.0, 2.0]`, updates the persisted map, and updates the live atomic for the matching current session (if that peer is currently in the call). No-op on the live side if the peer is not currently present, but the persisted value still applies on their next join.
- **`useVoice` / UI:** expose current per-peer volume (from persisted settings) and `setPeerVolume(pubkey, v)`. The voice participant context menu renders the slider; dragging calls the command (debounced is optional — direct is fine given it is cheap and local).

### Tests (Rust)

- `mix_one_frame` applies gain: a peer at 0.0 contributes nothing; at 2.0 doubles its samples (pre-clip); default 1.0 unchanged.
- `voice_set_peer_volume` clamps out-of-range values and updates the persisted map.

---

## Feature 3: Connection-quality meter

### Behavior

- A small **signal-bars icon** on the voice control bar, colored by tier. Hover shows exact numbers: `Ping: <rtt> ms · Loss: <loss>%`.
- Reflects the client's QUIC link to the server (the same path voice datagrams ride). Sampled about once per second **while in a voice call** (poller starts on join, stops on leave).

### Tiers (display-only thresholds)

- 🟢 **Good:** `rtt < 100 ms` AND `loss < 2%`
- 🟡 **Fair:** `rtt < 250 ms` OR `loss < 8%` (and not Good)
- 🔴 **Poor:** otherwise

`loss` is computed from Quinn path stats as `lost_packets / max(1, sent_packets)` over the connection (a smoothed/cumulative estimate is acceptable for v1; a windowed delta is a possible refinement, noted but not required).

### Components & flow

- **Stats poller (Rust):** a task started on voice join and aborted on leave that, every ~1s, reads `connection.stats()` from the active `ServerConnection`, extracts smoothed RTT (ms) and a loss fraction, and emits a Tauri event `voice://connection-quality` with `{ rtt_ms: f64, loss_pct: f64 }`.
  - The poller needs access to the current server's `quinn::Connection`. It reads from the same `ServerConnection` the call is bound to.
- **Tier classification:** a pure function (Rust or TS) mapping `(rtt_ms, loss_pct) -> tier`. Implement in TS for the UI; optionally mirror in Rust if convenient — single source of truth in the UI is acceptable since it is display-only.
- **`useVoice` / UI:** the hook subscribes to `voice://connection-quality`, holds the latest `{ rtt_ms, loss_pct }`, and the control bar renders the bars + tooltip. The icon only appears while in a call.

### Tests

- Tier classification (TS pure function or Rust): boundary cases at 100 ms / 250 ms and 2% / 8% map to the documented tiers. Verified via `tsc` for type-correctness; logic boundaries covered by a small pure-function test if a Rust mirror is implemented (no JS test runner available).

---

## File structure

**Created:**
- `client/src/components/VoiceSettings.tsx` (or a tab section) — Voice settings tab: mic mode radio + PTT key capture.
- Possibly `client/src/lib/connectionQuality.ts` — pure tier classifier + types.

**Modified (Rust):**
- `client/src-tauri/src/voice/mod.rs` — controller owns `transmit: Arc<AtomicBool>`; gate selection by mode; per-peer gain wiring at peer join; start/stop stats poller on join/leave; new event emit.
- `client/src-tauri/src/voice/mixer.rs` — per-peer gain in `mix_one_frame`; ring registration carries a gain atomic.
- `client/src-tauri/src/commands.rs` — `voice_set_transmitting` (or `voice_toggle_transmit`), `voice_set_peer_volume`; register commands.
- Settings module (the Rust side that owns `settings.json` de/serialization) — add `voice_mode`, `ptt_key`, `peer_volumes` fields with `#[serde(default)]`, plus getters/setters as needed.
- `client/src-tauri/src/state.rs` / wherever the stats poller reaches the `Connection`.

**Modified (Frontend):**
- `client/src/hooks/useVoice.ts` — expose `transmitting`, `toggleTransmit`, peer-volume getter/setter, connection-quality state; subscribe to the new event.
- `client/src/components/VoiceControlBar.tsx` — transmit indicator + key hint; signal-bars + tooltip.
- `client/src/components/ChannelSidebar.tsx` — participant right-click → volume slider; PTT keydown listener wiring (or a dedicated hook).
- `client/src/lib/tauri-bridge.ts` — new command bindings + types (`transmitting`, quality payload, volume).
- The Settings modal container — add the Voice tab.
- Theme CSS (`discord-dark`, `hello-kitty`, `xp-luna-blue`) — styles for transmit indicator, signal bars, volume slider, consistent with existing voice styles.

## Out of scope (this spec)

- **Auto-reconnect** — separate follow-up spec.
- OS-global PTT hotkey (works when Farder unfocused).
- Mic/speaker device pickers.
- Hold-to-talk and voice-activity (VAD) modes — only Open Mic and tap-to-toggle PTT ship here.
- Per-peer or per-participant *quality* (only the local link to the server is measured).
- Windowed/rate-based loss metric (cumulative estimate is acceptable for v1).

## Verification

- `cd /home/deez/farder && cargo test --workspace` — green, including new gate/mixer/volume tests.
- `cd /home/deez/farder/client/src-tauri && cargo test voice::` — green.
- `cd /home/deez/farder/client && npx tsc --noEmit` — clean.
- Manual (where hardware allows): Voice tab toggles mode; PTT key toggles transmit indicator; right-click volume slider persists; quality bars render with plausible numbers. Live two-party audio test deferred to real hardware.
