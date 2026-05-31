# Voice Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship three self-contained voice-UX features — tap-to-toggle push-to-talk, per-peer volume, and a connection-quality meter — exactly as described in `docs/superpowers/specs/2026-05-30-voice-polish-design.md`, with auto-reconnect out of scope.

**Architecture:** The Rust `VoiceController` (`client/src-tauri/src/voice/mod.rs`) owns a new `transmit: Arc<AtomicBool>` and selects the send-path `GateMode` at join from the persisted `voice_mode` setting; per-peer gain is an `Arc<AtomicU32>` (holding `f32::to_bits`) stored alongside each `PeerPcmRing` and applied in `mixer::mix_one_frame`; a per-call stats-poller task reads `quinn::Connection::stats()` and emits `voice://connection-quality`. New Tauri commands (`voice_toggle_transmit`, `voice_set_peer_volume`) bridge the controller to the React `useVoice` hook, which feeds the control bar, the participant context-menu slider, the PTT keydown listener, and a new Voice settings tab.

**Tech Stack:** Rust (Tauri `src-tauri`), React + TypeScript (Vite), Tauri 2 command/event bridge.

---

## Testing notes

- **Rust = full unit tests, TDD.** New logic (gate selection, per-peer gain in the mixer, volume clamp/persist, the stats→tier numbers) is covered by `cargo test`. The controller is exercised through the existing `mod controller_tests` harness in `client/src-tauri/src/voice/mod.rs` (a `FakeServerSession` + capturing `MockEmitter` + `NoopPipelineFactory`); extend that harness for new controller tests. The mixer has a `mod tests` with `make_ring_with_sine` / `run_for_n_frames` helpers — mirror that style for gain tests.
- **Cargo workspace root** is `/home/deez/farder`. The client crate lives at `/home/deez/farder/client/src-tauri`.
  - Run all Rust tests: `cd /home/deez/farder && cargo test --workspace`
  - Run just the voice module: `cd /home/deez/farder/client/src-tauri && cargo test voice::`
- **Frontend has NO JS test runner.** Frontend changes are verified by type-checking only:
  - `cd /home/deez/farder/client && npx tsc --noEmit` — must be clean.
  - UI-only tasks (no Rust unit test possible) say so explicitly and use `npx tsc --noEmit` as their "test".
- **Hardware constraint:** the dev box is WSL2 with no audio devices, so the engine runs on a mock audio backend. All logic + UI is verifiable here; a live two-party audio test needs real hardware and is out of automated scope.
- **No non-ASCII in source** where the project avoids it. UI strings that need a separator use the ASCII middot substitute `·` only inside JSX/TSX text where the existing voice UI already renders unicode through React (Twemoji/text); if in doubt, use the word "and"/"/". Tooltip text in this plan uses `·` inside a TS template string, which is data (not identifiers) and renders fine.

## File structure

**Created:**
- `client/src/lib/connectionQuality.ts` — pure `classifyQuality(rttMs, lossPct) -> 'good' | 'fair' | 'poor'` tier classifier + the `ConnectionQuality` payload type. Single source of truth for the UI thresholds.
- `client/src/components/VoiceSettings.tsx` — the Voice settings tab: mic-mode radio (Open Mic / Push-to-Talk) + a PTT key-capture control. Reads/writes the new settings via `tauri-bridge`.

**Modified (Rust):**
- `client/src-tauri/src/voice/gate.rs` — no signature change; (already has `GateMode::{Open,Vad,Ptt}` + `pass`). Used as-is.
- `client/src-tauri/src/voice/mixer.rs` — `PeerRings` value type becomes a `(Arc<PeerPcmRing>, Arc<AtomicU32>)` gain pair; `mix_one_frame` multiplies each sample by the decoded gain.
- `client/src-tauri/src/voice/mod.rs` — `VoiceState` gains `transmitting: bool`; `Inner` gains `transmit: Arc<AtomicBool>` + `voice_mode`/`ptt`-resolution at join; `PipelineParams` gains a `gate: GateMode` field; `AudioPipelineFactory::spawn` uses `params.gate` instead of hardcoded `GateMode::Open`; `ActiveCall` gains a `quality_poller: Option<JoinHandle<()>>`; new methods `set_transmitting`, `toggle_transmit`, `set_peer_volume`; `on_peer_track_enabled` seeds per-peer gain from a persisted-volumes lookup injected at join.
- `client/src-tauri/src/commands.rs` — the voice Tauri commands live here (NOT in `voice_bridge.rs`). Existing `voice_join`/`voice_leave`/`voice_set_mute`/`voice_set_deafen`/`voice_get_state` are `#[tauri::command]`s at ~lines 2000-2045 taking `controller: tauri::State<'_, Arc<crate::voice::VoiceController>>`; `voice_join` constructs `crate::voice_bridge::QuinnServerSession::new(state, server_id)` then calls `controller.join(...)`. Add new commands `voice_toggle_transmit`, `voice_set_peer_volume` here, plus the voice settings get/set commands.
- `client/src-tauri/src/voice_bridge.rs` — holds only `QuinnServerSession` (the `ServerSession` impl); no commands. Unchanged unless a helper is needed; `QuinnServerSession::new(state, server_id)` is the constructor `voice_join` uses.
- `client/src-tauri/src/main.rs` — register the new commands in the `tauri::generate_handler!` list (line ~60) as `commands::voice_toggle_transmit`, `commands::voice_set_peer_volume`, `commands::get_voice_mode`, etc., next to the existing `commands::voice_get_state` entry (line 146).
- `client/src-tauri/src/state.rs` — no struct change required (`ServerConnection.connection: quinn::Connection` already exposed); the poller reads `connection.stats()` from the active server's `ServerConnection`.

**Modified (Frontend):**
- `client/src/lib/tauri-bridge.ts` — bindings + types: `voiceToggleTransmit()`, `voiceSetPeerVolume(pubkeyHex, volume)`, the `transmitting` field on the `VoiceState` type, `ConnectionQualityPayload`, and settings get/set for `voice_mode` / `ptt_key` / `peer_volumes`.
- `client/src/hooks/useVoice.ts` — expose `transmitting`, `toggleTransmit()`, `setPeerVolume(pubkey, v)`, a `peerVolume(pubkey)` getter, and `connectionQuality: { rttMs, lossPct } | null`; subscribe to `voice://connection-quality` using the StrictMode-safe `safePush` listener pattern.
- `client/src/components/VoiceControlBar.tsx` — PTT transmit indicator + key hint (only when mode is PTT), and a signal-bars quality icon with a hover tooltip (`vcb-*` classes).
- `client/src/components/VoiceParticipantContextMenu.tsx` — wire the already-declared `onSetVolume?` / `currentVolume?` props to a 0–200% slider.
- `client/src/components/ChannelSidebar.tsx` — right-click participant -> context menu volume; window-level PTT keydown listener (active only while in a call AND mode = PTT).
- `client/src/components/SettingsModal.tsx` — add a **Voice** tab rendering `VoiceSettings`.
- `client/src/themes/{discord-dark,hello-kitty,xp-luna-blue}/theme.css` — styles for the transmit indicator, signal bars, and volume slider.

---

# Phase 1 — Push-to-talk (tap-to-toggle)

End-of-phase commit(s): one commit covering the Rust gate plumbing + command + tests, then one commit covering the frontend wiring + Voice settings tab. Feature is independently committable and shippable.

## Task 1.1 — Add `transmitting` to `VoiceState` and `transmit` atomic to the controller

**Files**
- Modify: `client/src-tauri/src/voice/mod.rs`
- Test: `client/src-tauri/src/voice/mod.rs` (`mod controller_tests`)

Steps:

- [ ] 1. Write a failing test in `controller_tests` asserting the default state serializes `transmitting: false`. Add this test function inside `mod controller_tests` (after `leave_with_no_active_call_is_idempotent`):

```rust
    #[tokio::test]
    async fn fresh_controller_reports_not_transmitting() {
        let (ctrl, _emitter) = make_controller();
        let st = ctrl.state().await;
        assert!(!st.transmitting, "fresh controller must not be transmitting");
    }
```

- [ ] 2. Run it, expect FAIL (compile error: no field `transmitting`):

```
cd /home/deez/farder/client/src-tauri && cargo test voice::controller_tests::fresh_controller_reports_not_transmitting
```
Expected: `error[E0560]: struct \`VoiceState\` has no field named \`transmitting\`` (or "no field `transmitting`").

- [ ] 3. Add the field to `VoiceState`. Change the struct (around line 28):

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoiceState {
    pub channel_id: Option<ChannelId>,
    pub muted: bool,
    pub deafened: bool,
    pub transmitting: bool,
    pub peers: Vec<VoicePeer>,
}
```

- [ ] 4. Add the `transmit` atomic to `Inner` (struct around line 311). Change:

```rust
struct Inner {
    state: VoiceState,
    muted: Arc<AtomicBool>,
    deafened: Arc<AtomicBool>,
    transmit: Arc<AtomicBool>,
    pre_deafen_muted: bool,
    /// Set while a call is active.
    active: Option<ActiveCall>,
}
```

- [ ] 5. Initialize both new fields in `with_runtime` (around line 365). Change the `Inner { ... }` initializer:

```rust
            inner: Arc::new(Mutex::new(Inner {
                state: VoiceState {
                    channel_id: None,
                    muted: false,
                    deafened: false,
                    transmitting: false,
                    peers: vec![],
                },
                muted: Arc::new(AtomicBool::new(false)),
                deafened: Arc::new(AtomicBool::new(false)),
                transmit: Arc::new(AtomicBool::new(false)),
                pre_deafen_muted: false,
                active: None,
            })),
```

- [ ] 6. In `leave`, reset `transmitting`/`transmit` in the Phase-3 commit block (the `inner.state = VoiceState { ... }` reconstruction around line 525). Change that block:

```rust
        let snap = {
            let mut inner = self.inner.lock().await;
            inner.state = VoiceState {
                channel_id: None,
                muted: false,
                deafened: false,
                transmitting: false,
                peers: vec![],
            };
            inner.muted.store(false, Ordering::Release);
            inner.deafened.store(false, Ordering::Release);
            inner.transmit.store(false, Ordering::Release);
            inner.pre_deafen_muted = false;
            inner.state.clone()
        };
```

- [ ] 7. Run, expect PASS:

```
cd /home/deez/farder/client/src-tauri && cargo test voice::controller_tests::fresh_controller_reports_not_transmitting
```
Expected: `test result: ok. 1 passed`.

- [ ] 8. Run the whole voice module to confirm nothing else broke:

```
cd /home/deez/farder/client/src-tauri && cargo test voice::
```
Expected: all existing voice tests still pass (`test result: ok`).

## Task 1.2 — Thread a `gate: GateMode` through `PipelineParams` and `AudioPipelineFactory::spawn`

**Files**
- Modify: `client/src-tauri/src/voice/mod.rs`
- Test: covered indirectly (controller tests use `NoopPipelineFactory`); compile is the gate. Add a focused unit test on a tiny helper instead.

Steps:

- [ ] 1. Write a failing test for a new free function `gate_for_mode` that maps a mode + transmit flag to a `GateMode`. Add to `mod controller_tests`:

```rust
    #[test]
    fn gate_for_mode_picks_ptt_only_when_push_to_talk() {
        let flag = Arc::new(AtomicBool::new(false));
        // Open mic -> Open (always passes), transmit flag ignored.
        let g_open = super::gate_for_mode(super::VoiceMode::OpenMic, flag.clone());
        assert!(g_open.pass(&[0.0; 960]));
        // PTT -> Ptt(flag); closed blocks, open passes.
        let g_ptt = super::gate_for_mode(super::VoiceMode::PushToTalk, flag.clone());
        assert!(!g_ptt.pass(&[0.0; 960]));
        flag.store(true, Ordering::Release);
        assert!(g_ptt.pass(&[0.0; 960]));
    }
```

- [ ] 2. Run, expect FAIL (no `VoiceMode`, no `gate_for_mode`):

```
cd /home/deez/farder/client/src-tauri && cargo test voice::controller_tests::gate_for_mode_picks_ptt_only_when_push_to_talk
```
Expected: `error[E0433]` / `cannot find ... VoiceMode` / `gate_for_mode`.

- [ ] 3. Define `VoiceMode` and `gate_for_mode` near the top of `voice/mod.rs`, right after the `VoiceState` struct (after line 34):

```rust
/// Mic transmission mode, read from settings at join time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceMode {
    OpenMic,
    PushToTalk,
}

impl Default for VoiceMode {
    fn default() -> Self {
        VoiceMode::OpenMic
    }
}

/// Choose the send-path gate for a mode. PTT uses the shared transmit flag;
/// Open Mic ignores it and always passes.
pub fn gate_for_mode(mode: VoiceMode, transmit: Arc<AtomicBool>) -> crate::voice::gate::GateMode {
    match mode {
        VoiceMode::OpenMic => crate::voice::gate::GateMode::Open,
        VoiceMode::PushToTalk => crate::voice::gate::GateMode::Ptt(transmit),
    }
}
```

Note: `AtomicBool` and `Ordering` are already imported at line 302 (`use std::sync::atomic::{AtomicBool, Ordering};`). `Arc` is imported at line 42.

- [ ] 4. Add a `gate` field to `PipelineParams` (struct around line 197). Change:

```rust
pub struct PipelineParams {
    pub session_id: SessionId,
    pub stream_key: [u8; 32],
    pub speaker_pk: [u8; 32],
    pub peer_rings: crate::voice::mixer::PeerRings,
    pub muted: Arc<std::sync::atomic::AtomicBool>,
    pub gate: crate::voice::gate::GateMode,
    pub local_speaking_tx: tokio::sync::watch::Sender<bool>,
    pub datagram_sink: Box<dyn Fn(Bytes) + Send + Sync + 'static>,
}
```

- [ ] 5. Use `params.gate` in `AudioPipelineFactory::spawn` instead of the hardcoded `GateMode::Open`. In `spawn` (around line 281) replace the `gate:` line and capture the gate before the closure. Change the send-task setup block (lines ~268-291):

```rust
        // Send.
        let send_aec = aec_ref;
        let session_id = params.session_id;
        let stream_key = params.stream_key;
        let speaker_pk = params.speaker_pk;
        let muted = params.muted;
        let gate = params.gate;
        let speak_tx = params.local_speaking_tx;
        let datagram_sink = params.datagram_sink;
        tokio::task::spawn_blocking(move || {
            crate::voice::send::run(
                crate::voice::send::SendTaskConfig {
                    pcm_rx,
                    apm: crate::voice::apm::AudioProcessor::new(),
                    gate,
                    session_id,
                    stream_key,
                    speaker_pk,
                    aec_ref: send_aec,
                    datagram_sink,
                },
                muted,
                speak_tx,
            );
        });
```

- [ ] 6. Fix the `join` call site that builds `PipelineParams` (around line 445) to pass a gate. For this task pass `GateMode::Open` (mode resolution lands in Task 1.4). Change the `spawn(PipelineParams { ... })` to include the new field; add `gate` right after `muted`:

```rust
        let muted_flag = self.inner.lock().await.muted.clone();
        let transmit_flag = self.inner.lock().await.transmit.clone();
        let (speak_tx, mut speak_rx) = tokio::sync::watch::channel(false);
        let server_for_sink = server.clone();
        let pipeline = self.pipeline_factory.spawn(PipelineParams {
            session_id,
            stream_key,
            speaker_pk: my_pk_bytes,
            peer_rings: peer_rings.clone(),
            muted: muted_flag,
            gate: gate_for_mode(VoiceMode::OpenMic, transmit_flag),
            local_speaking_tx: speak_tx,
            datagram_sink: Box::new(move |b: Bytes| {
                let _ = server_for_sink.send_datagram(b);
            }),
        })?;
```

- [ ] 7. Fix the `NoopPipelineFactory::spawn` test impl — it ignores params, so no change needed; but the controller-test `setup_joined_call` / `join` now passes the extra field, which compiles. Run the focused test, expect PASS:

```
cd /home/deez/farder/client/src-tauri && cargo test voice::controller_tests::gate_for_mode_picks_ptt_only_when_push_to_talk
```
Expected: `test result: ok. 1 passed`.

- [ ] 8. Run the whole voice module, expect PASS:

```
cd /home/deez/farder/client/src-tauri && cargo test voice::
```
Expected: `test result: ok`.

## Task 1.3 — `set_transmitting` / `toggle_transmit` on the controller

**Files**
- Modify: `client/src-tauri/src/voice/mod.rs`
- Test: `client/src-tauri/src/voice/mod.rs` (`mod controller_tests`)

Steps:

- [ ] 1. Write a failing test asserting toggle flips the atomic + state + emits. Add to `controller_tests`:

```rust
    #[tokio::test]
    async fn toggle_transmit_flips_state_and_emits() {
        let (ctrl, server, emitter) = setup_joined_call().await;
        let _ = server; // server relay not needed for transmit
        let before = emitter.count("voice://state-changed");

        let now_on = ctrl.toggle_transmit().await;
        assert!(now_on, "first toggle turns transmit on");
        assert!(ctrl.state().await.transmitting);

        let now_off = ctrl.toggle_transmit().await;
        assert!(!now_off, "second toggle turns transmit off");
        assert!(!ctrl.state().await.transmitting);

        assert_eq!(
            emitter.count("voice://state-changed"),
            before + 2,
            "each toggle re-emits state"
        );
    }
```

- [ ] 2. Run, expect FAIL (no `toggle_transmit`):

```
cd /home/deez/farder/client/src-tauri && cargo test voice::controller_tests::toggle_transmit_flips_state_and_emits
```
Expected: `no method named \`toggle_transmit\``.

- [ ] 3. Add the methods to `impl VoiceController`, right after `set_deafen` (after line 584):

```rust
    /// Set the PTT transmit flag explicitly and re-emit state.
    pub async fn set_transmitting(&self, transmitting: bool) -> Result<(), String> {
        let snap = {
            let mut inner = self.inner.lock().await;
            inner.transmit.store(transmitting, Ordering::Release);
            inner.state.transmitting = transmitting;
            inner.state.clone()
        };
        self.emitter
            .emit("voice://state-changed", serde_json::to_value(&snap).unwrap_or_default());
        Ok(())
    }

    /// Toggle the PTT transmit flag and return the new value.
    pub async fn toggle_transmit(&self) -> bool {
        let new_val = {
            let inner = self.inner.lock().await;
            !inner.transmit.load(Ordering::Acquire)
        };
        let _ = self.set_transmitting(new_val).await;
        new_val
    }
```

- [ ] 4. Run, expect PASS:

```
cd /home/deez/farder/client/src-tauri && cargo test voice::controller_tests::toggle_transmit_flips_state_and_emits
```
Expected: `test result: ok. 1 passed`.

- [ ] 5. Run full voice module, expect PASS:

```
cd /home/deez/farder/client/src-tauri && cargo test voice::
```
Expected: `test result: ok`.

## Task 1.4 — Read `voice_mode` from settings at join; build the gate accordingly

**Files**
- Modify: `client/src-tauri/src/voice/mod.rs`
- Test: `client/src-tauri/src/voice/mod.rs` (`mod controller_tests`)

Decision (matches spec): mode is read **at join**, no mid-call hot-swap. To keep the controller testable without touching disk, inject the mode + a persisted-volumes snapshot into `join` via a small `JoinConfig`. The Tauri command layer (Task 2.x) reads settings and fills this in; tests pass it directly.

Steps:

- [ ] 1. Write a failing test that a PTT-mode join produces a closed gate until toggled. Because the `NoopPipelineFactory` ignores the gate, assert on observable controller state instead: after a PTT join, `state.transmitting` starts false and `toggle_transmit` flips it (already covered), AND a new accessor `current_gate_is_ptt()` reports the chosen mode. Add to `controller_tests`:

```rust
    #[tokio::test]
    async fn join_with_ptt_mode_selects_ptt_gate() {
        let (ctrl, _emitter) = make_controller();
        let server = FakeServerSession::new();
        ctrl.join_with_config(
            7,
            server.clone(),
            super::JoinConfig {
                mode: super::VoiceMode::PushToTalk,
                peer_volumes: std::collections::HashMap::new(),
            },
        )
        .await
        .unwrap();
        assert!(ctrl.current_gate_is_ptt().await, "PTT mode selects Ptt gate");
        ctrl.leave().await.unwrap();
    }

    #[tokio::test]
    async fn join_with_open_mic_mode_selects_open_gate() {
        let (ctrl, _emitter) = make_controller();
        let server = FakeServerSession::new();
        ctrl.join_with_config(
            7,
            server.clone(),
            super::JoinConfig {
                mode: super::VoiceMode::OpenMic,
                peer_volumes: std::collections::HashMap::new(),
            },
        )
        .await
        .unwrap();
        assert!(!ctrl.current_gate_is_ptt().await, "Open Mic selects Open gate");
        ctrl.leave().await.unwrap();
    }
```

- [ ] 2. Run, expect FAIL (no `join_with_config`, `JoinConfig`, `current_gate_is_ptt`):

```
cd /home/deez/farder/client/src-tauri && cargo test voice::controller_tests::join_with_ptt_mode_selects_ptt_gate
```
Expected: `cannot find ... JoinConfig` / `no method named join_with_config`.

- [ ] 3. Define `JoinConfig` near `VoiceMode` (after the `gate_for_mode` fn added in Task 1.2):

```rust
/// Per-join configuration resolved from settings by the command layer.
/// Kept separate from `join`'s args so tests can inject it directly.
#[derive(Clone, Debug, Default)]
pub struct JoinConfig {
    pub mode: VoiceMode,
    /// pubkey hex -> gain (1.0 = 100%). Absent peer = 1.0.
    pub peer_volumes: std::collections::HashMap<String, f32>,
}
```

- [ ] 4. Add a `mode` and `peer_volumes` field to `ActiveCall` so peer-join (Phase 2) can seed gains, and a `gate_is_ptt` cached bool for the test accessor. Change `ActiveCall` (struct around line 320):

```rust
struct ActiveCall {
    server: Arc<dyn ServerSession>,
    pipeline: Option<Box<dyn VoicePipelineHandle>>,
    peer_rings: crate::voice::mixer::PeerRings,
    peers: HashMap<SessionId, PeerEntry>,
    peer_keys: HashMap<SessionId, ([u8; 32], PublicKey)>,
    peer_status: HashMap<SessionId, (bool, bool)>,
    /// pubkey hex -> gain, snapshotted from settings at join. Seeds new peers.
    peer_volumes: HashMap<String, f32>,
    gate_is_ptt: bool,
}
```

- [ ] 5. Refactor `join` to delegate to `join_with_config`. Replace the existing `pub async fn join(...)` signature/body opener (lines 401-405) so `join` forwards with default config, and rename the body to `join_with_config`. Concretely, change the method header from:

```rust
    pub async fn join(
        &self,
        channel_id: u64,
        server: Arc<dyn ServerSession>,
    ) -> Result<(), String> {
```
to:

```rust
    /// Back-compat join used by existing tests: Open Mic, no saved volumes.
    pub async fn join(
        &self,
        channel_id: u64,
        server: Arc<dyn ServerSession>,
    ) -> Result<(), String> {
        self.join_with_config(channel_id, server, JoinConfig::default()).await
    }

    pub async fn join_with_config(
        &self,
        channel_id: u64,
        server: Arc<dyn ServerSession>,
        config: JoinConfig,
    ) -> Result<(), String> {
```
(Keep the rest of the original `join` body as the body of `join_with_config`.)

- [ ] 6. In `join_with_config`, build the gate from `config.mode` and seed `ActiveCall`. Replace the `gate_for_mode(VoiceMode::OpenMic, transmit_flag)` from Task 1.2 step 6 with `gate_for_mode(config.mode, transmit_flag)`, and update the `inner.active = Some(ActiveCall { ... })` commit block (around line 476) to populate the new fields:

```rust
            inner.active = Some(ActiveCall {
                server,
                pipeline: Some(pipeline),
                peer_rings,
                peers: HashMap::new(),
                peer_keys: HashMap::new(),
                peer_status: HashMap::new(),
                peer_volumes: config.peer_volumes.clone(),
                gate_is_ptt: matches!(config.mode, VoiceMode::PushToTalk),
            });
```

- [ ] 7. Add the test accessor in `impl VoiceController` (next to `state`):

```rust
    #[cfg(test)]
    pub async fn current_gate_is_ptt(&self) -> bool {
        self.inner
            .lock()
            .await
            .active
            .as_ref()
            .map(|c| c.gate_is_ptt)
            .unwrap_or(false)
    }
```

- [ ] 8. Run, expect PASS (both new join tests):

```
cd /home/deez/farder/client/src-tauri && cargo test voice::controller_tests::join_with
```
Expected: `test result: ok. 2 passed`.

- [ ] 9. Run full workspace to confirm the `join` refactor didn't break callers:

```
cd /home/deez/farder && cargo test --workspace
```
Expected: `test result: ok` across crates.

## Task 1.5 — Settings helpers + Tauri command `voice_toggle_transmit`

**Files**
- Modify: `client/src-tauri/src/commands.rs` (the `voice_*` commands + settings helpers all live here)
- Modify: `client/src-tauri/src/main.rs` (register commands)
- Test: settings round-trip is plain serde_json; covered by a small unit test in `commands.rs`; the command itself is verified by compile + `cargo test --workspace`.

Note: there is **no `settings.rs`** in this crate. `~/.farder/settings.json` is a flat JSON object managed by `read_settings`/`write_settings` + the `pub(crate) settings_get(key)` / `settings_set(key, value)` helpers in `commands.rs` (lines 230-252). New settings are top-level keys: `voice_mode` (string `"OpenMic"`/`"PushToTalk"`), `ptt_key` (string, default `"Backquote"`), `peer_volumes` (object: hex -> number).

Steps:

- [ ] 1. Write a failing unit test for the settings accessors. Add a `#[cfg(test)] mod voice_settings_tests` at the end of `commands.rs`:

```rust
#[cfg(test)]
mod voice_settings_tests {
    use super::*;

    #[test]
    fn voice_mode_defaults_to_open_mic() {
        // Point FARDER_DATA at a temp dir so we read a fresh settings.json.
        let tmp = std::env::temp_dir().join(format!("farder-test-{}", std::process::id()));
        std::env::set_var("FARDER_DATA", &tmp);
        let _ = std::fs::remove_file(settings_path());
        assert_eq!(read_voice_mode(), "OpenMic");
        assert_eq!(read_ptt_key(), "Backquote");
    }
}
```

- [ ] 2. Run, expect FAIL (no `read_voice_mode`):

```
cd /home/deez/farder/client/src-tauri && cargo test voice_settings_tests
```
Expected: `cannot find function \`read_voice_mode\``.

- [ ] 3. Add settings accessor helpers + their Tauri commands in `commands.rs`, in the "Settings commands" section (after `get_last_server`, ~line 262):

```rust
// ---------------------------------------------------------------------------
// Voice settings (mic mode, PTT key, per-peer volumes)
// ---------------------------------------------------------------------------

pub(crate) fn read_voice_mode() -> String {
    settings_get("voice_mode")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "OpenMic".to_string())
}

pub(crate) fn read_ptt_key() -> String {
    settings_get("ptt_key")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "Backquote".to_string())
}

pub(crate) fn read_peer_volumes() -> std::collections::HashMap<String, f32> {
    settings_get("peer_volumes")
        .and_then(|v| serde_json::from_value::<std::collections::HashMap<String, f32>>(v).ok())
        .unwrap_or_default()
}

#[tauri::command]
pub fn get_voice_mode() -> String {
    read_voice_mode()
}

#[tauri::command]
pub fn set_voice_mode(mode: String) -> Result<(), String> {
    // Accept only the two known values; default unknown to OpenMic.
    let normalized = if mode == "PushToTalk" { "PushToTalk" } else { "OpenMic" };
    settings_set("voice_mode", serde_json::Value::String(normalized.to_string()))
}

#[tauri::command]
pub fn get_ptt_key() -> String {
    read_ptt_key()
}

#[tauri::command]
pub fn set_ptt_key(key: String) -> Result<(), String> {
    settings_set("ptt_key", serde_json::Value::String(key))
}

#[tauri::command]
pub fn get_peer_volumes() -> std::collections::HashMap<String, f32> {
    read_peer_volumes()
}
```

- [ ] 4. Run, expect PASS:

```
cd /home/deez/farder/client/src-tauri && cargo test voice_settings_tests
```
Expected: `test result: ok. 1 passed`.

- [ ] 5. Add the `voice_toggle_transmit` command in `commands.rs`, in the "Voice controller commands" section (after `voice_get_state`, ~line 2043), mirroring the existing `voice_set_mute` pattern (each `#[tauri::command]` takes `voice: State<'_, Arc<crate::voice::VoiceController>>`):

```rust
#[tauri::command]
pub async fn voice_toggle_transmit(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
) -> Result<bool, String> {
    Ok(voice.toggle_transmit().await)
}
```

- [ ] 6. Update `voice_join` (commands.rs ~line 1999) to read settings and call `join_with_config` instead of `join`. Replace the body's `voice.join(...)` call. The existing function already has `voice: State<...>` and `state: State<'_, Arc<AppState>>` params and builds `session`. Change the tail from:

```rust
    let session = crate::voice_bridge::QuinnServerSession::new(
        Arc::clone(&state),
        server_id,
    )?;
    voice
        .join(channel_id, Arc::new(session) as Arc<dyn crate::voice::ServerSession>)
        .await
}
```
to:

```rust
    let session = crate::voice_bridge::QuinnServerSession::new(
        Arc::clone(&state),
        server_id.clone(),
    )?;
    let config = crate::voice::JoinConfig {
        mode: if read_voice_mode() == "PushToTalk" {
            crate::voice::VoiceMode::PushToTalk
        } else {
            crate::voice::VoiceMode::OpenMic
        },
        peer_volumes: read_peer_volumes(),
        connection: None, // poller connection wired in Task 3.2
    };
    voice
        .join_with_config(
            channel_id,
            Arc::new(session) as Arc<dyn crate::voice::ServerSession>,
            config,
        )
        .await
}
```
(Note `server_id` becomes `server_id.clone()` since it is also needed for `state.get_server` in Task 3.2. `read_voice_mode`/`read_peer_volumes` are in the same module, called unqualified. `connection` stays `None` here; Task 3.2 sets it.)

- [ ] 7. Register the new commands in `main.rs`'s `tauri::generate_handler!` list, using the same `commands::` path style as the existing `commands::voice_get_state` entry (line 146). Add after it:

```rust
            commands::voice_toggle_transmit,
            commands::get_voice_mode,
            commands::set_voice_mode,
            commands::get_ptt_key,
            commands::set_ptt_key,
            commands::get_peer_volumes,
```

- [ ] 8. Compile the whole crate, expect success:

```
cd /home/deez/farder && cargo test --workspace
```
Expected: builds clean, `test result: ok`.

- [ ] 9. **Commit Phase 1 Rust:**

```
cd /home/deez/farder && git add -A && git commit -m "voice: PTT tap-to-toggle gate + transmit command + settings

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 1.6 — Frontend: `tauri-bridge` bindings + `transmitting` on VoiceState type

**Files**
- Modify: `client/src/lib/tauri-bridge.ts`
- Test: `npx tsc --noEmit` (no JS runner).

Steps:

- [ ] 1. Add the `transmitting` field to the TS `VoiceState` interface (in `tauri-bridge.ts`, ~lines 375-382). It currently reads `channel_id`, `muted`, `deafened`, `peers`. Add `transmitting: boolean;` so it becomes:

```ts
export interface VoiceState {
  /// Present iff a call is active. 16-byte channel identifier serialized as
  /// a Vec<u8> (raw bytes), matching the controller's `ChannelId` type.
  channel_id: number[] | null;
  muted: boolean;
  deafened: boolean;
  transmitting: boolean;
  peers: VoicePeer[];
}
```

- [ ] 2. Add the command bindings + the connection-quality payload type, following the existing `async function` + `invoke` style used by `voiceSetMute` (the file uses `import { invoke } from "@tauri-apps/api/core"` at line 1; existing bindings use `async function` with double-quoted command names). Add near the other `voice*` bindings (~line 408):

```ts
export interface ConnectionQualityPayload {
  rtt_ms: number;
  loss_pct: number;
}

export async function voiceToggleTransmit(): Promise<boolean> {
  return invoke<boolean>("voice_toggle_transmit");
}

export async function voiceSetPeerVolume(pubkeyHex: string, volume: number): Promise<void> {
  return invoke<void>("voice_set_peer_volume", { pubkeyHex, volume });
}

export async function getVoiceMode(): Promise<string> {
  return invoke<string>("get_voice_mode");
}

export async function setVoiceMode(mode: string): Promise<void> {
  return invoke<void>("set_voice_mode", { mode });
}

export async function getPttKey(): Promise<string> {
  return invoke<string>("get_ptt_key");
}

export async function setPttKey(key: string): Promise<void> {
  return invoke<void>("set_ptt_key", { key });
}

export async function getPeerVolumes(): Promise<Record<string, number>> {
  return invoke<Record<string, number>>("get_peer_volumes");
}
```
(Tauri maps camelCase JS keys to snake_case command args, so `pubkeyHex` -> `pubkey_hex` and `volume` -> `volume` match the Rust `voice_set_peer_volume(pubkey_hex, volume)` args in Task 2.2 — same convention as the existing `voiceSetMute`/`updateChannel` bindings.)

- [ ] 3. Type-check, expect clean:

```
cd /home/deez/farder/client && npx tsc --noEmit
```
Expected: no output (success). This is a UI-only/type task — verified by `tsc`, not a unit test, because there is no JS test runner.

## Task 1.7 — Frontend: `useVoice` exposes `transmitting` + `toggleTransmit`

**Files**
- Modify: `client/src/hooks/useVoice.ts`
- Test: `npx tsc --noEmit`.

Steps:

- [ ] 1. The hook imports the bridge as `import * as api from "../lib/tauri-bridge"`, so call `api.voiceToggleTransmit()` (no new import needed). Add a `transmitting` boolean state, set it in `applyState`, expose it + a `toggleTransmit` callback.

  - Add to the `UseVoice` interface (after `deafened: boolean;`): `transmitting: boolean;` and (after `setDeafen`): `toggleTransmit: () => Promise<void>;`.
  - Add state: `const [transmitting, setTransmitting] = useState(false);`.
  - In `applyState` (after `setDeafenedState(s.deafened);`): `setTransmitting(s.transmitting);` and in the `if (!n.inCall)` block also reset `setTransmitting(false);`.
  - Add the callback near the other `useCallback`s:

```ts
  const toggleTransmit = useCallback(async () => {
    await api.voiceToggleTransmit();
    // State refreshes via the existing voice://state-changed listener.
  }, []);
```
  - Add `transmitting` and `toggleTransmit` to the returned object on the final `return { ... }` line.

- [ ] 2. Type-check, expect clean (UI-only, no JS runner):

```
cd /home/deez/farder/client && npx tsc --noEmit
```
Expected: no output.

## Task 1.8 — Frontend: control-bar transmit indicator + Voice settings tab + PTT keydown

**Files**
- Modify: `client/src/components/VoiceControlBar.tsx`
- Create: `client/src/components/VoiceSettings.tsx`
- Modify: `client/src/components/SettingsModal.tsx`
- Modify: `client/src/components/ChannelSidebar.tsx`
- Modify: `client/src/themes/{discord-dark,hello-kitty,xp-luna-blue}/theme.css`
- Test: `npx tsc --noEmit` (UI-only).

Steps:

- [ ] 1. In `VoiceControlBar.tsx`, when the active mic mode is PTT, render a transmit indicator + key hint using the hook's `transmitting`. Read the mode via `getVoiceMode()` (in a `useEffect`) and `getPttKey()`. Add (using existing `vcb-*` class style):

```tsx
{micMode === 'PushToTalk' && (
  <button
    type="button"
    className={transmitting ? 'vcb-ptt vcb-ptt-on' : 'vcb-ptt vcb-ptt-off'}
    onClick={() => { void toggleTransmit(); }}
    title={transmitting ? 'Transmitting' : `Mic off — tap ${pttKey} to talk`}
  >
    <span className="vcb-ptt-dot" />
    {transmitting ? 'Transmitting' : `Tap ${pttKey} to talk`}
  </button>
)}
```
where `transmitting` and `toggleTransmit` come from `useVoice()`, and `micMode`/`pttKey` are local state loaded from `getVoiceMode()`/`getPttKey()`.

- [ ] 2. Create `client/src/components/VoiceSettings.tsx` with a mic-mode radio + a key-capture button:

```tsx
import { useEffect, useState } from 'react';
import { getVoiceMode, setVoiceMode, getPttKey, setPttKey } from '../lib/tauri-bridge';

export function VoiceSettings() {
  const [mode, setMode] = useState<string>('OpenMic');
  const [pttKey, setPttKeyState] = useState<string>('Backquote');
  const [capturing, setCapturing] = useState(false);

  useEffect(() => {
    void getVoiceMode().then(setMode);
    void getPttKey().then(setPttKeyState);
  }, []);

  const chooseMode = (next: string) => {
    setMode(next);
    void setVoiceMode(next);
  };

  useEffect(() => {
    if (!capturing) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      setPttKeyState(e.code);
      void setPttKey(e.code);
      setCapturing(false);
    };
    window.addEventListener('keydown', onKey, { once: true });
    return () => window.removeEventListener('keydown', onKey);
  }, [capturing]);

  return (
    <div className="voice-settings">
      <h3>Microphone mode</h3>
      <label>
        <input
          type="radio"
          name="voice-mode"
          checked={mode === 'OpenMic'}
          onChange={() => chooseMode('OpenMic')}
        />
        Open Mic
      </label>
      <label>
        <input
          type="radio"
          name="voice-mode"
          checked={mode === 'PushToTalk'}
          onChange={() => chooseMode('PushToTalk')}
        />
        Push-to-Talk
      </label>
      {mode === 'PushToTalk' && (
        <div className="voice-settings-ptt-key">
          <span>Key: {pttKey}</span>
          <button type="button" onClick={() => setCapturing(true)}>
            {capturing ? 'Press a key…' : 'Rebind'}
          </button>
        </div>
      )}
    </div>
  );
}
```

- [ ] 3. In `SettingsModal.tsx`, add a "Voice" tab that renders `<VoiceSettings />`. Follow the exact tab pattern the file already uses for the Appearance tab (`AppearanceSettings`): add a tab id/label entry and a conditional render branch. Import `VoiceSettings` from `./VoiceSettings`.

- [ ] 4. In `ChannelSidebar.tsx` (voice region ~lines 295-365), add a window-level PTT keydown listener active only while in a call AND mode = PTT. Add an effect:

```tsx
useEffect(() => {
  if (!inCall || micMode !== 'PushToTalk') return;
  const onKey = (e: KeyboardEvent) => {
    if (e.repeat) return;
    const t = e.target as HTMLElement | null;
    const tag = t?.tagName;
    if (tag === 'INPUT' || tag === 'TEXTAREA' || t?.isContentEditable) return;
    if (e.code === pttKey) {
      e.preventDefault();
      void toggleTransmit();
    }
  };
  window.addEventListener('keydown', onKey);
  return () => window.removeEventListener('keydown', onKey);
}, [inCall, micMode, pttKey, toggleTransmit]);
```
where `inCall` is derived from `useVoice()` state (`channel_id != null`), `toggleTransmit` from `useVoice()`, and `micMode`/`pttKey` are loaded via `getVoiceMode()`/`getPttKey()` (re-read when the settings modal closes; for v1 a load on mount is acceptable per the "read at join / on next open" spec note).

- [ ] 5. Add CSS to all three theme files. In each of `client/src/themes/discord-dark/theme.css`, `client/src/themes/hello-kitty/theme.css`, `client/src/themes/xp-luna-blue/theme.css` add (tuned per theme's palette, consistent with existing `.vcb-*`/`.voice-*` rules):

```css
.vcb-ptt { display: inline-flex; align-items: center; gap: 6px; cursor: pointer; }
.vcb-ptt-dot { width: 8px; height: 8px; border-radius: 50%; background: #888; }
.vcb-ptt-on .vcb-ptt-dot { background: #e23b3b; box-shadow: 0 0 6px #e23b3b; }
.vcb-ptt-off { opacity: 0.7; }
.voice-settings label { display: block; margin: 4px 0; }
.voice-settings-ptt-key { margin-top: 8px; display: flex; gap: 8px; align-items: center; }
```

- [ ] 6. Type-check, expect clean (UI-only; no JS runner):

```
cd /home/deez/farder/client && npx tsc --noEmit
```
Expected: no output.

- [ ] 7. **Commit Phase 1 frontend:**

```
cd /home/deez/farder && git add -A && git commit -m "voice: PTT UI — control-bar indicator, Voice settings tab, keydown

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

# Phase 2 — Per-peer volume

End-of-phase commit(s): one commit for the Rust mixer-gain + command + tests, one for the frontend slider wiring. Independently committable after Phase 1.

## Task 2.1 — Mixer applies per-peer gain in `mix_one_frame`

**Files**
- Modify: `client/src-tauri/src/voice/mixer.rs`
- Test: `client/src-tauri/src/voice/mixer.rs` (`mod tests`)

Decision (matches spec): `PeerRings` value becomes `(Arc<PeerPcmRing>, Arc<AtomicU32>)` where the `AtomicU32` holds `f32::to_bits(gain)`. `mix_one_frame` multiplies each sample by the decoded gain before summing.

Steps:

- [ ] 1. Write failing tests for gain. Add to `mixer.rs`'s `mod tests` a helper + tests (mirroring `make_ring_with_sine`/`run_for_n_frames`):

```rust
    fn gain_bits(g: f32) -> Arc<std::sync::atomic::AtomicU32> {
        Arc::new(std::sync::atomic::AtomicU32::new(g.to_bits()))
    }

    #[test]
    fn zero_gain_silences_peer() {
        let peer_rings: PeerRings = Default::default();
        peer_rings
            .lock()
            .unwrap()
            .insert([1u8; 16], (make_ring_with_sine(440.0, 5), gain_bits(0.0)));
        let mixed = mix_one_frame(&peer_rings);
        assert!(mixed.iter().all(|&s| s == 0.0), "gain 0.0 must silence the peer");
    }

    #[test]
    fn double_gain_doubles_pre_clip_amplitude() {
        // Compare a single peer at gain 1.0 vs 2.0 on a low-amplitude sine
        // (well inside the soft-clip linear-ish region) sample-by-sample.
        let unity: PeerRings = Default::default();
        unity
            .lock()
            .unwrap()
            .insert([1u8; 16], (make_ring_with_sine(440.0, 5), gain_bits(1.0)));
        let doubled: PeerRings = Default::default();
        doubled
            .lock()
            .unwrap()
            .insert([1u8; 16], (make_ring_with_sine(440.0, 5), gain_bits(2.0)));
        let a = mix_one_frame(&unity);
        let b = mix_one_frame(&doubled);
        // For at least one clearly non-zero sample, doubled magnitude > unity.
        let idx = a.iter().position(|&s| s.abs() > 0.05).expect("some signal");
        assert!(b[idx].abs() > a[idx].abs(), "gain 2.0 must increase amplitude");
    }
```

- [ ] 2. Run, expect FAIL (tuple type mismatch / `mix_one_frame` signature):

```
cd /home/deez/farder/client/src-tauri && cargo test voice::mixer::tests::zero_gain_silences_peer
```
Expected: a type error on the `insert(... (ring, gain))` because `PeerRings` currently holds `Arc<PeerPcmRing>` not a tuple.

- [ ] 3. Change `PeerRings` to carry a gain atomic (line 21):

```rust
/// Shared registry of active peer rings paired with a per-peer gain atomic
/// (holds `f32::to_bits`). VoiceController inserts/removes as TrackEnabled /
/// TrackDisabled events arrive; the command layer updates the gain live.
pub type PeerRings = Arc<Mutex<HashMap<SessionId, (Arc<PeerPcmRing>, Arc<std::sync::atomic::AtomicU32>)>>>;
```

- [ ] 4. Apply gain in `mix_one_frame` (replace the loop body around lines 45-55):

```rust
fn mix_one_frame(peer_rings: &PeerRings) -> Vec<f32> {
    let rings = peer_rings.lock().expect("peer_rings poisoned");
    let mut acc = vec![0.0f32; OPUS_FRAME_SAMPLES_MONO];
    for (ring, gain_bits) in rings.values() {
        let gain = f32::from_bits(gain_bits.load(std::sync::atomic::Ordering::Acquire));
        let frame = ring.pop_frame();
        for (i, s) in frame.iter().enumerate() {
            if i < acc.len() {
                acc[i] += *s * gain;
            }
        }
    }
    for s in acc.iter_mut() {
        *s = soft_clip(*s);
    }
    acc
}
```

- [ ] 5. Update the three existing mixer tests that insert bare rings (`single_peer_passes_audible_signal_through`, `two_peers_sum_stays_within_soft_clip_bounds`) to insert tuples with unity gain. For each `.insert([N u8; 16], make_ring_with_sine(...))` change to `.insert([N u8; 16], (make_ring_with_sine(...), gain_bits(1.0)))`. (`empty_registry_emits_silence` has no inserts — leave it.)

- [ ] 6. Run, expect PASS:

```
cd /home/deez/farder/client/src-tauri && cargo test voice::mixer::
```
Expected: `test result: ok` (all mixer tests including the two new gain tests).

## Task 2.2 — Controller seeds per-peer gain at peer-join; `set_peer_volume` command

**Files**
- Modify: `client/src-tauri/src/voice/mod.rs`
- Modify: `client/src-tauri/src/voice_bridge.rs`
- Modify: `client/src-tauri/src/commands.rs` (persist into `peer_volumes`)
- Modify: `client/src-tauri/src/main.rs` (register command)
- Test: `client/src-tauri/src/voice/mod.rs` (`mod controller_tests`)

Steps:

- [ ] 1. Write a failing controller test: registering a peer creates a ring whose gain reflects the persisted `peer_volumes`, and `set_peer_volume` clamps + updates the live gain. Add to `controller_tests` a helper to register a peer and read its live gain:

```rust
    async fn live_gain_for(ctrl: &VoiceController, sid: &SessionId) -> Option<f32> {
        let inner = ctrl.inner.lock().await;
        inner.active.as_ref().and_then(|c| {
            c.peer_rings
                .lock()
                .ok()
                .and_then(|r| r.get(sid).map(|(_, g)| f32::from_bits(g.load(Ordering::Acquire))))
        })
    }

    #[tokio::test]
    async fn peer_join_seeds_gain_from_saved_volume_and_set_peer_volume_clamps() {
        let (ctrl, _emitter) = make_controller();
        let server = FakeServerSession::new();
        let pk = PublicKey::from_bytes([6u8; 32]);
        let hex = pk.to_string(); // controller keys peer_volumes by this exact string
        let mut vols = std::collections::HashMap::new();
        vols.insert(hex.clone(), 0.5f32);
        ctrl.join_with_config(
            3,
            server.clone(),
            super::JoinConfig { mode: super::VoiceMode::OpenMic, peer_volumes: vols },
        )
        .await
        .unwrap();

        let sid: SessionId = [9u8; 16];
        // Register the peer (offer key then TrackEnabled), reusing the existing
        // round-trip helper pattern.
        let peer_kp = Keypair::generate();
        let our_kp = server.my_keypair();
        let key = farder_crypto::media::derive_stream_key();
        let wrapped = farder_crypto::media::wrap_stream_key_for_peer(
            &key,
            peer_kp.signing_key_bytes(),
            our_kp.public_key().as_bytes(),
        )
        .unwrap();
        ctrl.on_stream_key_offer(sid, peer_kp.public_key(), wrapped).await;
        ctrl.on_peer_track_enabled(sid, pk.clone(), TrackKind::Audio).await;

        assert_eq!(live_gain_for(&ctrl, &sid).await, Some(0.5), "seeded from saved volume");

        // Over-range clamps to 2.0; live gain updates for the present peer.
        ctrl.set_peer_volume(hex.clone(), 5.0).await.unwrap();
        assert_eq!(live_gain_for(&ctrl, &sid).await, Some(2.0), "clamped to 2.0");

        // Negative clamps to 0.0.
        ctrl.set_peer_volume(hex.clone(), -1.0).await.unwrap();
        assert_eq!(live_gain_for(&ctrl, &sid).await, Some(0.0), "clamped to 0.0");

        ctrl.leave().await.unwrap();
    }
```

- [ ] 2. Run, expect FAIL (no `set_peer_volume`; ring insert is still a tuple now from Task 2.1 so `on_peer_track_enabled` won't compile against the new `PeerRings` until updated):

```
cd /home/deez/farder/client/src-tauri && cargo test voice::controller_tests::peer_join_seeds_gain_from_saved_volume_and_set_peer_volume_clamps
```
Expected: compile errors (`no method named set_peer_volume`, and tuple mismatch in `on_peer_track_enabled`).

- [ ] 3. Update `on_peer_track_enabled` to insert a `(ring, gain)` tuple seeded from `peer_volumes[pubkey_hex]`. In the ring-insert block (around lines 647-651) change:

```rust
        let ring = Arc::new(PeerPcmRing::new(10));
        let pubkey_hex = peer_pubkey.to_string();
        let seed_gain = call
            .peer_volumes
            .get(&pubkey_hex)
            .copied()
            .unwrap_or(1.0)
            .clamp(0.0, 2.0);
        let gain = Arc::new(std::sync::atomic::AtomicU32::new(seed_gain.to_bits()));
        call.peer_rings
            .lock()
            .expect("peer_rings poisoned")
            .insert(session_id, (ring.clone(), gain));
```

- [ ] 4. Add `set_peer_volume` to `impl VoiceController` (after `toggle_transmit`):

```rust
    /// Clamp + persist a per-peer volume (keyed by pubkey hex) and, if that
    /// peer is currently in the call, update its live mixer gain. Persisting
    /// is delegated to the caller-supplied closure so the controller stays
    /// independent of the settings file in tests.
    pub async fn set_peer_volume(&self, pubkey_hex: String, volume: f32) -> Result<(), String> {
        let clamped = volume.clamp(0.0, 2.0);
        let inner = self.inner.lock().await;
        if let Some(call) = inner.active.as_ref() {
            let rings = call.peer_rings.lock().expect("peer_rings poisoned");
            for (sid, (_, gain)) in rings.iter() {
                // Match the peer by pubkey via the peers map.
                if let Some(entry) = call.peers.get(sid) {
                    if entry.pubkey.to_string() == pubkey_hex {
                        gain.store(clamped.to_bits(), Ordering::Release);
                    }
                }
            }
        }
        Ok(())
    }
```

- [ ] 5. Make `inner` accessible to tests — it already is, since `controller_tests` is a child module of the same file (`super::` access to private fields works). No visibility change needed.

- [ ] 6. Run, expect PASS:

```
cd /home/deez/farder/client/src-tauri && cargo test voice::controller_tests::peer_join_seeds_gain_from_saved_volume_and_set_peer_volume_clamps
```
Expected: `test result: ok. 1 passed`.

- [ ] 7. Add the persistence command in `commands.rs` (voice controller commands section, after `voice_toggle_transmit`) that clamps, persists to settings, and updates the live controller:

```rust
#[tauri::command]
pub async fn voice_set_peer_volume(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
    pubkey_hex: String,
    volume: f32,
) -> Result<(), String> {
    let clamped = volume.clamp(0.0, 2.0);
    persist_peer_volume(&pubkey_hex, clamped)?;
    voice.set_peer_volume(pubkey_hex, clamped).await
}
```

- [ ] 8. Add the `persist_peer_volume` helper to `commands.rs` (in the voice-settings section from Task 1.5; same module as the command so it is called unqualified):

```rust
pub(crate) fn persist_peer_volume(pubkey_hex: &str, volume: f32) -> Result<(), String> {
    let mut map = read_peer_volumes();
    map.insert(pubkey_hex.to_string(), volume.clamp(0.0, 2.0));
    let value = serde_json::to_value(map).map_err(|e| e.to_string())?;
    settings_set("peer_volumes", value)
}
```

- [ ] 9. Add a unit test for `persist_peer_volume` clamping, in `voice_settings_tests` (commands.rs):

```rust
    #[test]
    fn persist_peer_volume_clamps_and_roundtrips() {
        let tmp = std::env::temp_dir().join(format!("farder-vol-{}", std::process::id()));
        std::env::set_var("FARDER_DATA", &tmp);
        let _ = std::fs::remove_file(settings_path());
        persist_peer_volume("deadbeef", 9.0).unwrap();
        assert_eq!(read_peer_volumes().get("deadbeef"), Some(&2.0));
        persist_peer_volume("deadbeef", -3.0).unwrap();
        assert_eq!(read_peer_volumes().get("deadbeef"), Some(&0.0));
    }
```

- [ ] 10. Register `voice_set_peer_volume` in `main.rs`'s `generate_handler!` list (next to `commands::voice_toggle_transmit`):

```rust
            commands::voice_set_peer_volume,
```

- [ ] 11. Run the full workspace, expect PASS:

```
cd /home/deez/farder && cargo test --workspace
```
Expected: `test result: ok`.

- [ ] 12. **Commit Phase 2 Rust:**

```
cd /home/deez/farder && git add -A && git commit -m "voice: per-peer mixer gain + set_peer_volume command + persistence

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 2.3 — Frontend: volume slider in the participant context menu

**Files**
- Modify: `client/src/hooks/useVoice.ts`
- Modify: `client/src/components/VoiceParticipantContextMenu.tsx`
- Modify: `client/src/components/ChannelSidebar.tsx`
- Modify: theme CSS (slider)
- Test: `npx tsc --noEmit` (UI-only).

Steps:

- [ ] 1. In `useVoice.ts` (bridge imported as `api`), load `peerVolumes` once into state, expose `peerVolume(pubkey)` (saved value or 1.0) and `setPeerVolume(pubkey, v)`.

  - Add to the `UseVoice` interface: `peerVolume: (pubkey: string) => number;` and `setPeerVolume: (pubkey: string, v: number) => Promise<void>;`.
  - Add state + loader + callbacks in the hook body:

```ts
  const [peerVolumes, setPeerVolumes] = useState<Record<string, number>>({});
  useEffect(() => { void api.getPeerVolumes().then(setPeerVolumes).catch(() => {}); }, []);
  const peerVolume = useCallback(
    (pubkey: string) => peerVolumes[pubkey] ?? 1.0,
    [peerVolumes],
  );
  const setPeerVolume = useCallback(async (pubkey: string, v: number) => {
    const clamped = Math.max(0, Math.min(2, v));
    setPeerVolumes((prev) => ({ ...prev, [pubkey]: clamped }));
    await api.voiceSetPeerVolume(pubkey, clamped);
  }, []);
```
  - Add `peerVolume`, `setPeerVolume` to the returned object.

- [ ] 2. In `VoiceParticipantContextMenu.tsx`, render a 0–200% slider wired to the already-declared `onSetVolume?` / `currentVolume?` props:

```tsx
{onSetVolume && (
  <div className="voice-volume-slider">
    <label>Volume: {Math.round((currentVolume ?? 1) * 100)}%</label>
    <input
      type="range"
      min={0}
      max={200}
      step={5}
      value={Math.round((currentVolume ?? 1) * 100)}
      onChange={(e) => onSetVolume(Number(e.target.value) / 100)}
    />
  </div>
)}
```

- [ ] 3. In `ChannelSidebar.tsx` (voice participant list ~lines 295-365), pass `currentVolume={peerVolume(participantPubkeyHex)}` and `onSetVolume={(v) => setPeerVolume(participantPubkeyHex, v)}` to the `VoiceParticipantContextMenu` instance, using the same `pubkey` hex the participant row already has (the same string the Rust controller keys by, i.e. `PublicKey.to_string()` — confirm the participant's pubkey field matches that representation; the existing peer-speaking handler already maps `pubkey.to_string()`).

- [ ] 4. Add slider CSS to all three theme files:

```css
.voice-volume-slider { padding: 6px 10px; display: flex; flex-direction: column; gap: 4px; }
.voice-volume-slider input[type="range"] { width: 100%; }
```

- [ ] 5. Type-check, expect clean (UI-only; no JS runner):

```
cd /home/deez/farder/client && npx tsc --noEmit
```
Expected: no output.

- [ ] 6. **Commit Phase 2 frontend:**

```
cd /home/deez/farder && git add -A && git commit -m "voice: per-peer volume slider in participant context menu

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

# Phase 3 — Connection-quality meter

End-of-phase commit(s): one commit for the Rust poller + event, one for the TS classifier + control-bar bars. Independently committable after Phases 1-2.

## Task 3.1 — TS tier classifier (pure function) — created first so the UI/Rust agree

**Files**
- Create: `client/src/lib/connectionQuality.ts`
- Test: `npx tsc --noEmit` (UI-only; pure function, no JS runner — boundaries asserted by a Rust mirror in Task 3.3 and by `tsc` types here).

Steps:

- [ ] 1. Create `client/src/lib/connectionQuality.ts` with the classifier + types exactly per the spec thresholds (Good = rtt<100 AND loss<2; Fair = rtt<250 OR loss<8, and not Good; Poor otherwise). Note `loss_pct` is a percentage (0-100):

```ts
export type QualityTier = 'good' | 'fair' | 'poor';

export interface ConnectionQuality {
  rttMs: number;
  lossPct: number;
}

/**
 * Display-only tier classifier. Thresholds from the voice-polish spec:
 *   Good: rtt < 100 ms AND loss < 2%
 *   Fair: rtt < 250 ms OR loss < 8%  (and not Good)
 *   Poor: otherwise
 */
export function classifyQuality(rttMs: number, lossPct: number): QualityTier {
  if (rttMs < 100 && lossPct < 2) return 'good';
  if (rttMs < 250 || lossPct < 8) return 'fair';
  return 'poor';
}
```

- [ ] 2. Type-check, expect clean (UI-only; pure function — verified by `tsc`, no JS runner available):

```
cd /home/deez/farder/client && npx tsc --noEmit
```
Expected: no output.

## Task 3.2 — Rust stats poller on the controller; emit `voice://connection-quality`

**Files**
- Modify: `client/src-tauri/src/voice/mod.rs`
- Modify: `client/src-tauri/src/commands.rs` (`voice_join` supplies the `quinn::Connection` to the controller at join)
- Test: `client/src-tauri/src/voice/mod.rs` (`mod controller_tests`) — assert via a pure stats→payload helper unit test (the live poll needs a real connection, which the WSL2/mock env lacks).

Decision (matches spec): add a pure helper `quality_from_stats(rtt: Duration, lost: u64, sent: u64) -> (f64, f64)` that the poller and tests share; loss = `lost / max(1, sent) * 100`. The poller task is owned by `ActiveCall.quality_poller` and aborted in `leave`. The `quinn::Connection` is injected through `JoinConfig` (added as an `Option`) so the controller stays decoupled and tests pass `None` (no poller spawned).

`quinn` is **0.11.9** in `Cargo.lock`. `Connection::stats() -> quinn::ConnectionStats`; the smoothed RTT and packet counters live on `ConnectionStats.path` (`PathStats`): `path.rtt: std::time::Duration`, `path.lost_packets: u64`, `path.sent_packets: u64`. The poller reads `connection.stats().path`.

Steps:

- [ ] 1. Write a failing pure-helper test. Add to `controller_tests`:

```rust
    #[test]
    fn quality_from_stats_computes_ms_and_loss_pct() {
        use std::time::Duration;
        // 50 ms rtt, 3 lost of 100 sent => 50.0 ms, 3.0%.
        let (rtt_ms, loss_pct) = super::quality_from_stats(Duration::from_millis(50), 3, 100);
        assert!((rtt_ms - 50.0).abs() < 1e-6);
        assert!((loss_pct - 3.0).abs() < 1e-6);
        // Zero sent must not divide-by-zero: max(1, sent).
        let (_, loss0) = super::quality_from_stats(Duration::from_millis(10), 0, 0);
        assert_eq!(loss0, 0.0);
    }
```

- [ ] 2. Run, expect FAIL (no `quality_from_stats`):

```
cd /home/deez/farder/client/src-tauri && cargo test voice::controller_tests::quality_from_stats_computes_ms_and_loss_pct
```
Expected: `cannot find function quality_from_stats`.

- [ ] 3. Add the pure helper near `gate_for_mode`:

```rust
/// Convert raw QUIC path stats into (rtt_ms, loss_pct). Cumulative estimate;
/// loss = lost / max(1, sent) * 100. Display-only.
pub fn quality_from_stats(rtt: std::time::Duration, lost: u64, sent: u64) -> (f64, f64) {
    let rtt_ms = rtt.as_secs_f64() * 1000.0;
    let loss_pct = (lost as f64 / sent.max(1) as f64) * 100.0;
    (rtt_ms, loss_pct)
}
```

- [ ] 4. Add `quality_poller: Option<JoinHandle<()>>` to `ActiveCall` (it already has access to `JoinHandle` via the `use tokio::task::JoinHandle;` at line 303). Add the field:

```rust
    quality_poller: Option<JoinHandle<()>>,
```

- [ ] 5. Extend `JoinConfig` with an optional connection for the poller:

```rust
#[derive(Clone, Default)]
pub struct JoinConfig {
    pub mode: VoiceMode,
    pub peer_volumes: std::collections::HashMap<String, f32>,
    pub connection: Option<quinn::Connection>,
}
```
(Remove the earlier `#[derive(Debug)]` if `quinn::Connection` is not `Debug`; keep `Clone, Default` — `quinn::Connection` is `Clone` and `Option<_>` defaults to `None`.) Add `use quinn` is unnecessary; reference the fully-qualified `quinn::Connection`.

- [ ] 6. In `join_with_config`, spawn the poller when a connection is present, and store its handle. After the speaking-event forwarder (after line 470, before the state-commit block) add:

```rust
        // Connection-quality poller (only when a real connection is supplied).
        let quality_poller = config.connection.as_ref().map(|conn| {
            let conn = conn.clone();
            let emitter = self.emitter.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
                loop {
                    tick.tick().await;
                    let p = conn.stats().path;
                    let (rtt_ms, loss_pct) =
                        quality_from_stats(p.rtt, p.lost_packets, p.sent_packets);
                    emitter.emit(
                        "voice://connection-quality",
                        serde_json::json!({ "rtt_ms": rtt_ms, "loss_pct": loss_pct }),
                    );
                }
            })
        });
```

- [ ] 7. Store the handle in the `ActiveCall` initializer:

```rust
                quality_poller,
```

- [ ] 8. Abort the poller in `leave`'s teardown phase (Phase-2 block, after stopping the pipeline, around line 509):

```rust
            if let Some(h) = call.quality_poller.take() {
                h.abort();
            }
```

- [ ] 9. Update the back-compat `join` (and all `controller_tests` callers using `JoinConfig { ... }`) — `JoinConfig::default()` now also defaults `connection: None`, so `join` is unchanged; the Task 1.4/2.2 tests construct `JoinConfig { mode, peer_volumes }` which no longer compiles because of the new field. Add `connection: None,` to those literal constructions (in `join_with_ptt_mode_selects_ptt_gate`, `join_with_open_mic_mode_selects_open_gate`, and `peer_join_seeds_gain_from_saved_volume_and_set_peer_volume_clamps`).

- [ ] 10. Run, expect PASS:

```
cd /home/deez/farder/client/src-tauri && cargo test voice::controller_tests::quality_from_stats_computes_ms_and_loss_pct
```
Expected: `test result: ok. 1 passed`.

- [ ] 11. In `commands.rs` `voice_join`, populate `JoinConfig.connection` from the active server's `ServerConnection`. `voice_join` already has `state: State<'_, Arc<AppState>>` and `server_id`; fetch the connection and set the field. Update the body (building on Task 1.5 step 6) so it reads:

```rust
    let server_conn = state.get_server(&server_id)?;
    let session = crate::voice_bridge::QuinnServerSession::new(
        Arc::clone(&state),
        server_id.clone(),
    )?;
    let config = crate::voice::JoinConfig {
        mode: if read_voice_mode() == "PushToTalk" {
            crate::voice::VoiceMode::PushToTalk
        } else {
            crate::voice::VoiceMode::OpenMic
        },
        peer_volumes: read_peer_volumes(),
        connection: Some(server_conn.connection.clone()),
    };
    voice
        .join_with_config(
            channel_id,
            Arc::new(session) as Arc<dyn crate::voice::ServerSession>,
            config,
        )
        .await
}
```
(`state.get_server` returns `Result<Arc<ServerConnection>, String>`, so `?` works directly with the command's `Result<_, String>`. `server_conn.connection` is the `quinn::Connection`, which is `Clone`.)

- [ ] 12. Run full workspace, expect PASS:

```
cd /home/deez/farder && cargo test --workspace
```
Expected: `test result: ok`.

- [ ] 13. **Commit Phase 3 Rust:**

```
cd /home/deez/farder && git add -A && git commit -m "voice: connection-quality poller emits voice://connection-quality

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 3.3 — Frontend: subscribe to quality event + render signal bars

**Files**
- Modify: `client/src/hooks/useVoice.ts`
- Modify: `client/src/components/VoiceControlBar.tsx`
- Modify: theme CSS (signal bars)
- Test: `npx tsc --noEmit` (UI-only).

Steps:

- [ ] 1. In `useVoice.ts`, subscribe to `voice://connection-quality` inside the **existing** `useEffect` that already sets up the other listeners (the one using `safePush`, where `safePush(u: () => void)` pushes the unlisten fn). Add a new `listen(...).then(safePush)` line alongside the others so it shares the StrictMode-safe cleanup.

  - Add to the `UseVoice` interface: `connectionQuality: { rttMs: number; lossPct: number } | null;`.
  - Add state: `const [connectionQuality, setConnectionQuality] = useState<{ rttMs: number; lossPct: number } | null>(null);`.
  - Clear it when leaving: in `applyState`, inside the existing `if (!n.inCall) { ... }` block add `setConnectionQuality(null);`.
  - Inside the existing listener `useEffect`, add (next to the `voice://peer-speaking` registration):

```ts
    listen<api.ConnectionQualityPayload>("voice://connection-quality", (e) =>
      setConnectionQuality({ rttMs: e.payload.rtt_ms, lossPct: e.payload.loss_pct })).then(safePush);
```
  - Add `connectionQuality` to the returned object. (`listen` is already imported; `api.ConnectionQualityPayload` comes from the `import * as api` already in the file.)

- [ ] 2. In `VoiceControlBar.tsx`, render the signal-bars icon only while in a call, colored by tier, with a tooltip. Import `classifyQuality` from `../lib/connectionQuality`. Add:

```tsx
{inCall && connectionQuality && (() => {
  const tier = classifyQuality(connectionQuality.rttMs, connectionQuality.lossPct);
  const ping = Math.round(connectionQuality.rttMs);
  const loss = connectionQuality.lossPct.toFixed(1);
  return (
    <span
      className={`vcb-signal vcb-signal-${tier}`}
      title={`Ping: ${ping} ms · Loss: ${loss}%`}
    >
      <i className="vcb-bar vcb-bar-1" />
      <i className="vcb-bar vcb-bar-2" />
      <i className="vcb-bar vcb-bar-3" />
    </span>
  );
})()}
```
where `connectionQuality` and `inCall` come from `useVoice()`.

- [ ] 3. Add signal-bars CSS to all three theme files (colors tuned per theme; green/yellow/red tiers):

```css
.vcb-signal { display: inline-flex; align-items: flex-end; gap: 2px; height: 14px; }
.vcb-signal .vcb-bar { width: 3px; background: currentColor; opacity: 0.4; }
.vcb-bar-1 { height: 5px; }
.vcb-bar-2 { height: 9px; }
.vcb-bar-3 { height: 14px; }
.vcb-signal-good { color: #3ba55d; }
.vcb-signal-good .vcb-bar { opacity: 1; }
.vcb-signal-fair { color: #d7a300; }
.vcb-signal-fair .vcb-bar-1, .vcb-signal-fair .vcb-bar-2 { opacity: 1; }
.vcb-signal-poor { color: #e23b3b; }
.vcb-signal-poor .vcb-bar-1 { opacity: 1; }
```

- [ ] 4. Type-check, expect clean (UI-only; no JS runner):

```
cd /home/deez/farder/client && npx tsc --noEmit
```
Expected: no output.

- [ ] 5. **Commit Phase 3 frontend:**

```
cd /home/deez/farder && git add -A && git commit -m "voice: connection-quality signal bars + tooltip on control bar

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-review

### Spec-coverage table

| Spec requirement | Task(s) |
| --- | --- |
| PTT tap-to-toggle; gate starts closed in PTT | 1.1, 1.2, 1.3, 1.4 |
| Mode read at join, no mid-call hot-swap | 1.4, 1.5 (`voice_join` -> `join_with_config`) |
| `transmit: Arc<AtomicBool>` owned by controller, init false on join | 1.1, 1.3 |
| `GateMode::Ptt(transmit)` when PTT else `Open` (pipeline gate-construction site) | 1.2 (`gate_for_mode` + `AudioPipelineFactory::spawn`) |
| Mute always wins (independent flags) | unchanged send-path (mute checked post-gate in `send.rs`); preserved by Task 1.2 |
| `transmitting: bool` on Rust `VoiceState` + TS `VoiceState` | 1.1, 1.6 |
| `voice_set_transmitting` / `voice_toggle_transmit` command re-emits state | 1.3, 1.5 |
| `useVoice` exposes `transmitting`, `toggleTransmit` | 1.7 |
| In-app PTT keydown listener (focused, match `event.code`, ignore repeats/inputs) | 1.8 |
| Control bar transmit indicator + key hint (PTT only) | 1.8 |
| Voice settings tab: mic-mode radio + key-capture (default `Backquote`) | 1.5 (defaults), 1.8 |
| `settings.json` `voice_mode`, `ptt_key` with backward-compat defaults | 1.5 |
| Per-peer volume range 0.0–2.0 clamp, default 1.0 | 2.1, 2.2 |
| Gain applied in `mix_one_frame` (`acc[i] += frame[i] * gain`) | 2.1 |
| Runtime gain as `Arc<AtomicU32>` of `f32::to_bits`, paired per ring | 2.1, 2.2 |
| Seed gain from persisted `peer_volumes[pubkey]` at `on_peer_track_enabled` | 2.2 |
| `voice_set_peer_volume(pubkey_hex, volume)` clamps, persists, updates live | 2.2 |
| `settings.json` `peer_volumes: HashMap<String,f32>` | 1.5 (reader), 2.2 (persist) |
| Context-menu 0–200% slider wired to `onSetVolume`/`currentVolume` | 2.3 |
| `useVoice` peer-volume getter/setter | 2.3 |
| Right-click participant -> volume in `ChannelSidebar` | 2.3 |
| Stats poller on join / abort on leave, ~1s cadence | 3.2 |
| Reads `connection.stats()` (quinn 0.11.9 `path.{rtt,lost_packets,sent_packets}`) | 3.2 |
| Emits `voice://connection-quality` `{ rtt_ms, loss_pct }`; loss = lost/max(1,sent) | 3.2 (`quality_from_stats`) |
| TS tier classifier (Good/Fair/Poor boundaries) in `connectionQuality.ts` | 3.1 |
| `useVoice` subscribes to quality event (StrictMode-safe), holds latest | 3.3 |
| Signal bars colored by tier + tooltip `Ping … · Loss …`, in-call only | 3.3 |
| Theme CSS for indicator, signal bars, slider (3 themes) | 1.8, 2.3, 3.3 |
| Auto-reconnect OUT of scope | (intentionally absent) |
| Each feature independently committable in order | Phase 1 / 2 / 3 commits |

### No-placeholder / consistency note

- No `TODO`, `etc.`, "similar to above", or stubbed bodies remain in any code step; every Rust block shows complete, compiling code matching the real signatures read from `gate.rs`, `send.rs`, `mixer.rs`, `recv.rs`, `voice/mod.rs`, `state.rs`, and the `commands.rs` settings helpers.
- Type/signature consistency across tasks: `PeerRings` becomes `HashMap<SessionId, (Arc<PeerPcmRing>, Arc<AtomicU32>)>` in Task 2.1 and every reader (`mix_one_frame`, `on_peer_track_enabled`, `set_peer_volume`, `leave`'s `.clear()`, mixer tests) is updated to the tuple form. `JoinConfig` grows monotonically across Tasks 1.4 → 2.2 → 3.2 (`mode`, `peer_volumes`, `connection`), and every literal construction (back-compat `join` via `Default`, plus three controller tests) is updated when the field is added. `PipelineParams.gate` is added in 1.2 and the single `spawn` call site is updated in the same task. `VoiceState.transmitting` (Rust 1.1, TS 1.6) and the `{ rtt_ms, loss_pct }` event payload (Rust 3.2, TS `ConnectionQualityPayload` 1.6 + `ConnectionQuality` 3.1) use consistent snake_case-on-the-wire / camelCase-in-TS conventions.
