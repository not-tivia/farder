# Screensharing Phase E — Polished Share UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish screensharing's UX: a video-source picker (choose which monitor to share), a LIVE badge on the sharing participant, click-to-watch viewer tiles (instead of auto-show), a viewer-side game-audio volume slider that drives the per-peer ScreenAudio gain, and a client-side one-sharer-per-channel guard — all themed.

**Architecture:** Reuses surfaces already built in B/C/D. The video-source picker surfaces the existing `DisplayBackend::enumerate_sources()` (monitors) through a new `list_display_sources` command and threads a `source_id: Option<String>` through `start_screen_share` (replacing the hardcoded `sources.first()`), mirroring Phase D's audio-device picker. A new `voice://peer-video-sharing` event (emitted when a peer's Video track enables/disables) lets `useVoice` track which pubkeys are live, driving the LIVE badge. `PeerVideoTiles` becomes click-to-watch: tiles render only for sessions the user opted into via the badge, and a viewer pane adds an enlarge control + a game-audio slider that calls a new `voice_set_screen_audio_gain` command (modeled on `voice_set_peer_volume`, but writing the `screen_audio_rings` gain). No new native code; the real WGC capture path is unchanged from Phase B/C.

**Tech Stack:** Rust (existing `DisplayBackend`, the voice controller, Tauri commands/events), React/TypeScript + the existing WebCodecs viewer, per-theme CSS.

**Spec:** `docs/superpowers/specs/2026-06-12-screensharing-design.md` (Phase E — the UI section).

**Branch:** create `screenshare-phaseE` from `main` before Task 1. Finish with ff-merge + push.

**Scope note:** Phase E ships a **monitor** source picker (what `display_wgc` enumerates today and Phase B validated on Windows). **Single-window** capture is deferred (it needs a separate WGC window-enumeration path not yet built) — noted as a follow-up, not in E. The picker is a themed `<select>` (consistent with the Phase D audio picker), not the WinRT OS chooser dialog. This completes the 5-phase screensharing arc.

---

## Verified codebase facts (read 2026-06-13 — exact)

- **DisplayBackend** (`client/src-tauri/src/display.rs:46-55`): `fn enumerate_sources(&self) -> Result<Vec<DisplaySource>, String>`; `fn start_capture(&self, source_id: &str, format: DisplayFormat) -> Result<mpsc::Receiver<VideoFrame>, String>`. `DisplaySource { id: String, kind: DisplaySourceKind, label: String, width: u32, height: u32 }` (`display.rs:21-28`). WGC backend enumerates monitors as `id = "monitor:{i+1}"`, `label = "Display {i+1}: {name}"` (`display_wgc.rs:137-157`). Mock returns one source `id:"mock-display"`, `label:"Mock Display 1280×720"` (`display.rs:159-167`).
- **start_screen_share source pick** (`voice/mod.rs:1378-1381`): `let backend = make_display_backend(); let sources = backend.enumerate_sources()?; let source_id = sources.first().map(|s| s.id.clone()).ok_or("no capture source")?; let rx = backend.start_capture(&source_id, DisplayFormat{fps,max_width,max_height})?;`. Signature is `start_screen_share(&self, fps, max_width, max_height, audio_device_id: Option<String>)` (`voice/mod.rs:1355`).
- **voice_start_screen_share command** (`commands.rs:2993-3001`): params `fps, max_width, max_height, audio_device_id: Option<String>`. TS bridge `voiceStartScreenShare(fps,maxWidth,maxHeight,audioDeviceId)` (`tauri-bridge.ts:467`).
- **set_peer_volume (the gain model)** (`voice/mod.rs:910-924`): iterates `call.peer_rings`, finds the ring whose `call.peers.get(sid).pubkey.to_string() == pubkey_hex`, `gain.store(clamped.to_bits(), Ordering::Release)`. Command `voice_set_peer_volume(pubkey_hex, volume)` (`commands.rs:2975-2983`, clamps 0..2, persists). Bridge `voiceSetPeerVolume(pubkeyHex, volume)` (`tauri-bridge.ts:496`).
- **screen_audio gain** (`voice/mod.rs:1105-1129` `on_peer_screen_audio_track_enabled`): inserts `(ring, Arc<AtomicU32>=1.0f32.to_bits())` into `call.screen_audio_rings` (keyed by SessionId). `PeerRings = Arc<Mutex<HashMap<SessionId,(Arc<PeerPcmRing>,Arc<AtomicU32>)>>>` (`mixer.rs:23`). **No setter command yet.** `_peer_pubkey` param is currently unused — Phase E will use it. `call.peer_pubkeys: HashMap<SessionId,PublicKey>` (`voice/mod.rs:454`) maps session→pubkey; helper `peer_pubkey_for(&session_id)` (`voice/mod.rs:589`).
- **peer-video-frame event** (`voice/mod.rs:1080-1091`): emits `voice://peer-video-frame {session: hex, pubkey: hex(vk_…), data: b64, key: bool, seq}`. `on_peer_video_track_enabled(session_id, peer_pubkey)` (`voice/mod.rs:~1015`) spawns the recv; `on_peer_track_disabled(session, Video)` (`voice/mod.rs:~1062`) + `on_peer_stream_left` tear it down.
- **Frontend peer state** (`useVoice.ts:6-11`): `VoiceUiPeer { pubkey, speaking, muted, deafened }` (no `isSharing`). `peers` updated by `voice://state-changed` (`useVoice.ts:97`) and incrementally by `voice://peer-speaking` (`useVoice.ts:100-105`). `pubkey` is `publicKeyToString(...)` = `vk_<hex>`.
- **Participant list** (`ChannelSidebar.tsx:362,414-438`): `const liveByPk = new Map(voice.peers.map(p => [p.pubkey, p]));` then per participant `const live = liveByPk.get(p.publicKey)`; renders avatar/name + mute/deafen emoji. `p.publicKey` is the member's key string. No sharing badge.
- **PeerVideoTiles** (`PeerVideoTiles.tsx:15,49-101`): `FramePayload { session, pubkey, data, key, seq }`. State `sessions: Record<string,{pubkey}>`, auto-adds a session on first frame, reaps after 3s idle. Decoder per session (race-free ref-callback create, key-gated, error self-heal). Mounted in `ChannelSidebar` above the voice bar.
- **Audio-device picker precedent (Phase D)** (`useVoice.ts` + `VoiceControlBar.tsx`): `audioDevices`/`audioDeviceId`/`setAudioDeviceId` in the hook (loaded via `listAudioOutputDevices()` in a `useEffect`), a `.vcb-audio-source` `<select>` in the voice bar. The video-source picker mirrors this exactly.

---

### Task 1: Backend — `list_display_sources` command + `source_id` on start-share

**Files:** Modify `client/src-tauri/src/voice/mod.rs`, `client/src-tauri/src/commands.rs`, `client/src-tauri/src/main.rs`, `client/src/lib/tauri-bridge.ts`

- [ ] **Step 1: Add `source_id` to `start_screen_share`.** Change the signature (`voice/mod.rs:1355`) to:
```rust
    pub async fn start_screen_share(&self, fps: u32, max_width: u32, max_height: u32, source_id: Option<String>, audio_device_id: Option<String>) -> Result<(), String> {
```
And replace the source pick (lines 1378-1381):
```rust
        let backend = crate::display::make_display_backend();
        let source_id = match source_id {
            Some(id) => id,
            None => {
                let sources = backend.enumerate_sources()?;
                sources.first().map(|s| s.id.clone()).ok_or("no capture source")?
            }
        };
        let rx = backend.start_capture(&source_id, crate::display::DisplayFormat { fps, max_width, max_height })?;
```

- [ ] **Step 2: Add the enumerate command + update the start command** in `commands.rs`. Add:
```rust
#[tauri::command]
pub async fn list_display_sources() -> Result<Vec<crate::display::DisplaySource>, String> {
    crate::display::make_display_backend().enumerate_sources()
}
```
(Confirm `DisplaySource` derives `serde::Serialize` — if not, add `#[derive(..., serde::Serialize)]` to it in `display.rs`, and `DisplaySourceKind` too.) Update `voice_start_screen_share` (commands.rs:2993) to take + forward `source_id`:
```rust
pub async fn voice_start_screen_share(
    voice: tauri::State<'_, std::sync::Arc<crate::voice::VoiceController>>,
    fps: u32,
    max_width: u32,
    max_height: u32,
    source_id: Option<String>,
    audio_device_id: Option<String>,
) -> Result<(), String> {
    voice.start_screen_share(fps, max_width, max_height, source_id, audio_device_id).await
}
```
(Match the exact state-extraction the current command uses.)

- [ ] **Step 3: Register `list_display_sources`** in `generate_handler![...]` in `main.rs` (next to `list_audio_output_devices`).

- [ ] **Step 4: Bridge** in `tauri-bridge.ts`. Add:
```ts
export type DisplaySourceKind = "Screen" | "Window";
export interface DisplaySource { id: string; kind: DisplaySourceKind; label: string; width: number; height: number; }
export async function listDisplaySources(): Promise<DisplaySource[]> {
  return invoke<DisplaySource[]>("list_display_sources");
}
```
Update `voiceStartScreenShare` to add `sourceId`:
```ts
export async function voiceStartScreenShare(fps: number, maxWidth: number, maxHeight: number, sourceId: string | null, audioDeviceId: string | null): Promise<void> {
  return invoke<void>("voice_start_screen_share", { fps, maxWidth, maxHeight, sourceId, audioDeviceId });
}
```
Update the existing caller in `useVoice.ts` (`startShare`) to pass `null` for `sourceId` for now (Task 4 wires the real selection): `voiceStartScreenShare(30,1280,720, null, audioDeviceId)`.

- [ ] **Step 5: Build + seam + tsc + voice tests.**
```
cd /home/deez/farder/client/src-tauri && cargo build && cargo test voice:: -- --test-threads=1 2>&1 | grep -E "test result|error\["
cd /home/deez/farder && grep -q "list_display_sources" client/src-tauri/src/main.rs && grep -q '"list_display_sources"' client/src/lib/tauri-bridge.ts && echo "SEAM OK" || echo "SEAM MISSING"
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
```
(Update any in-file test callers of `start_screen_share` to pass the new `None` source arg.) Expected: green, SEAM OK, TSC_OK.

- [ ] **Step 6: Commit:**
```bash
git add client/src-tauri/src/voice/mod.rs client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src-tauri/src/display.rs client/src/lib/tauri-bridge.ts client/src/hooks/useVoice.ts
git commit -m "client: list_display_sources command + source_id on start-share"
```

---

### Task 2: Backend — `voice://peer-video-sharing` event (who is live)

**Files:** Modify `client/src-tauri/src/voice/mod.rs`

The frontend needs to know which peer (by pubkey) currently has an active Video track, to show the LIVE badge. Emit an event on enable + disable.

- [ ] **Step 1: Emit on video-track enable.** In `on_peer_video_track_enabled` (after it sets up the recv, near the existing setup — `voice/mod.rs:~1015-1056`), emit:
```rust
        self.emitter.emit(
            "voice://peer-video-sharing",
            serde_json::json!({ "pubkey": peer_pubkey.to_string(), "sharing": true }),
        );
```
(Use the same `emitter` field the other emits use; place it after the `video_peers.insert`.)

- [ ] **Step 2: Emit on video-track disable + teardown.** In `on_peer_track_disabled`'s `Video` arm (`voice/mod.rs:~1062`), when an entry was actually removed, emit `sharing:false`. The handler has `session_id`; get the pubkey from `peer_pubkeys` (or the removed entry):
```rust
                    if let Some(pk) = call.peer_pubkeys.get(&session_id).cloned() {
                        // capture pk before dropping the lock; emit after.
                    }
```
Concretely: inside the Video teardown branch, after removing the `video_peers` entry, capture `let shed_pubkey = call.peer_pubkeys.get(&session_id).map(|p| p.to_string());`, then after releasing the inner lock emit when `Some`:
```rust
        if let Some(pubkey) = shed_pubkey {
            self.emitter.emit("voice://peer-video-sharing", serde_json::json!({ "pubkey": pubkey, "sharing": false }));
        }
```
Adapt to the exact lock structure of `on_peer_track_disabled` (read it — it may already release the lock before a tail section; emit there). Since `on_peer_stream_left` now calls `on_peer_track_disabled(session, Video)`, a peer leaving also emits `sharing:false` automatically — no extra code needed there.

- [ ] **Step 3: Test.** Add a controller test that `on_peer_video_track_enabled` causes a `voice://peer-video-sharing {sharing:true}` emit and `on_peer_track_disabled(Video)` a `{sharing:false}` emit. Use the existing test emitter capture pattern (find how other tests assert emitted events — e.g. a `Vec<(String, Value)>` recording emitter; mirror it). If video-track-enabled is heavy to drive in a test (needs a key offer first), assert the narrower observable you can reach, or rely on the existing video enable test + add only the disable-emit assertion. Note which.

- [ ] **Step 4: Build + test:**
```
cd /home/deez/farder/client/src-tauri && cargo build && cargo test voice:: -- --test-threads=1 2>&1 | grep -E "test result|error\["
```
Green.

- [ ] **Step 5: Commit:**
```bash
git add client/src-tauri/src/voice/mod.rs
git commit -m "client: emit voice://peer-video-sharing on video track enable/disable"
```

---

### Task 3: Backend — `voice_set_screen_audio_gain` command

**Files:** Modify `client/src-tauri/src/voice/mod.rs`, `client/src-tauri/src/commands.rs`, `client/src-tauri/src/main.rs`, `client/src/lib/tauri-bridge.ts`

The viewer's game-audio slider sets the per-peer ScreenAudio ring gain (the independent gain Phase D left at 1.0). Model on `set_peer_volume` but write `screen_audio_rings`, matching the peer by pubkey via `peer_pubkeys`.

- [ ] **Step 1: Controller method.** In `voice/mod.rs` (near `set_peer_volume`):
```rust
    /// Set a peer's GAME-AUDIO (ScreenAudio) volume, independent of their voice.
    pub async fn set_screen_audio_gain(&self, pubkey_hex: String, gain: f32) -> Result<(), String> {
        let clamped = gain.clamp(0.0, 2.0);
        let inner = self.inner.lock().await;
        if let Some(call) = inner.active.as_ref() {
            let rings = call.screen_audio_rings.lock().expect("screen_audio_rings poisoned");
            for (sid, (_, g)) in rings.iter() {
                if let Some(pk) = call.peer_pubkeys.get(sid) {
                    if pk.to_string() == pubkey_hex {
                        g.store(clamped.to_bits(), Ordering::Release);
                    }
                }
            }
        }
        Ok(())
    }
```

- [ ] **Step 2: Command** in `commands.rs` (mirror `voice_set_peer_volume`, but NO persistence — game-audio volume is per-session/ephemeral for v1):
```rust
#[tauri::command]
pub async fn voice_set_screen_audio_gain(
    voice: tauri::State<'_, std::sync::Arc<crate::voice::VoiceController>>,
    pubkey_hex: String,
    gain: f32,
) -> Result<(), String> {
    voice.set_screen_audio_gain(pubkey_hex, gain).await
}
```
(Match the exact state-extraction `voice_set_peer_volume` uses.)

- [ ] **Step 3: Register** in `generate_handler![...]` in `main.rs`.

- [ ] **Step 4: Bridge** in `tauri-bridge.ts`:
```ts
export async function voiceSetScreenAudioGain(pubkeyHex: string, gain: number): Promise<void> {
  return invoke<void>("voice_set_screen_audio_gain", { pubkeyHex, gain });
}
```

- [ ] **Step 5: Test.** Add a controller test: seed a ScreenAudio peer (offer key + `on_peer_track_enabled(sid, pk, ScreenAudio)` — mirror Task-6/Phase-D's `peer_stream_left_tears_down_screen_audio_ring` seeding), call `set_screen_audio_gain(pk.to_string(), 0.5)`, and assert the ring's gain AtomicU32 reads back `0.5f32.to_bits()` (reach into `ctrl.inner.lock().await` → `active.screen_audio_rings`). Assert clamping (e.g. `3.0` clamps to `2.0`).

- [ ] **Step 6: Build + seam + tsc:**
```
cd /home/deez/farder/client/src-tauri && cargo build && cargo test voice:: -- --test-threads=1 2>&1 | grep -E "test result|error\["
cd /home/deez/farder && grep -q "voice_set_screen_audio_gain" client/src-tauri/src/main.rs && grep -q '"voice_set_screen_audio_gain"' client/src/lib/tauri-bridge.ts && echo "SEAM OK" || echo "SEAM MISSING"
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
```
Green, SEAM OK, TSC_OK.

- [ ] **Step 7: Commit:**
```bash
git add client/src-tauri/src/voice/mod.rs client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src/lib/tauri-bridge.ts
git commit -m "client: voice_set_screen_audio_gain command (per-peer game-audio volume)"
```

---

### Task 4: Frontend — video-source picker + one-sharer guard

**Files:** Modify `client/src/hooks/useVoice.ts`, `client/src/components/VoiceControlBar.tsx`, theme CSS ×3

- [ ] **Step 1: Source state in useVoice** (mirror the Phase D `audioDevices` block):
```ts
  const [displaySources, setDisplaySources] = useState<api.DisplaySource[]>([]);
  const [sourceId, setSourceId] = useState<string | null>(null);
  useEffect(() => {
    api.listDisplaySources().then((s) => {
      setDisplaySources(s);
      setSourceId((cur) => cur ?? s[0]?.id ?? null);
    }).catch(() => {});
  }, []);
```
Change `startShare` to pass the selected source: `await api.voiceStartScreenShare(30, 1280, 720, sourceId, audioDeviceId);` and add `sourceId` to the deps. Add `displaySources`, `sourceId`, `setSourceId` to the `UseVoice` interface + returned object.

- [ ] **Step 2: One-sharer-per-channel guard.** Add a derived `someoneElseSharing` from the peer-sharing set (Task 5 adds `sharingPeers`). For THIS task, add to the `UseVoice` interface a boolean the Share button disables on. If Task 5 isn't done yet, default it false; Task 5 wires it. Expose `someoneElseSharing: boolean` (computed in Task 5 from `sharingPeers.size > 0`).

- [ ] **Step 3: Picker + guarded Share button in VoiceControlBar.** Add a `<select className="vcb-video-source">` next to the audio picker (only useful before sharing), and disable the Share button when `voice.someoneElseSharing`:
```tsx
        <select
          className="vcb-video-source"
          title="Screen to share (monitor)"
          value={voice.sourceId ?? ""}
          onChange={(e) => voice.setSourceId(e.target.value)}
          disabled={voice.isSharing}
        >
          {voice.displaySources.map((s) => (
            <option key={s.id} value={s.id}>{s.label}</option>
          ))}
        </select>
```
And on the existing Share button add `disabled={voice.someoneElseSharing && !voice.isSharing}` plus a title hint when disabled ("Someone else is already sharing").

- [ ] **Step 4: Theme CSS.** Add `.vcb-video-source` to all 3 `client/src/themes/*/theme.css` using the SAME rule as `.vcb-audio-source` (reuse its exact properties/vars per theme — copy that rule and rename the selector; if you prefer, make both share a comma selector `.vcb-audio-source, .vcb-video-source`). Verify the theme vars used exist in each file.

- [ ] **Step 5: tsc + grep:**
```
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
grep -l "vcb-video-source" client/src/themes/*/theme.css   # 3 files
```

- [ ] **Step 6: Commit:**
```bash
git add client/src/hooks/useVoice.ts client/src/components/VoiceControlBar.tsx client/src/themes/*/theme.css
git commit -m "client ui: video-source picker + one-sharer-per-channel guard"
```

---

### Task 5: Frontend — LIVE badge + sharing-peer tracking

**Files:** Modify `client/src/hooks/useVoice.ts`, `client/src/components/ChannelSidebar.tsx`, theme CSS ×3

- [ ] **Step 1: Track sharing peers in useVoice.** Add a `sharingPeers` set fed by the new event:
```ts
  const [sharingPeers, setSharingPeers] = useState<Set<string>>(new Set());
  // inside the listen() setup effect:
  listen<{ pubkey: string; sharing: boolean }>("voice://peer-video-sharing", (e) => {
    setSharingPeers((prev) => {
      const next = new Set(prev);
      if (e.payload.sharing) next.add(e.payload.pubkey); else next.delete(e.payload.pubkey);
      return next;
    });
  }).then(safePush);
```
Reset `setSharingPeers(new Set())` when a call ends (in `applyState`'s `!inCall` block). Compute `someoneElseSharing = sharingPeers.size > 0` (or excluding self if self can appear — self never emits peer-video-sharing for itself, so `size > 0` is fine while not sharing; when `isSharing` the button shows Stop anyway). Expose `sharingPeers`, `someoneElseSharing`, and a `watchPeer`/viewer toggle (Task 6) on the interface + returned object.

- [ ] **Step 2: LIVE badge in ChannelSidebar.** In the participant row (`ChannelSidebar.tsx:414-438`), compute `const isSharing = voice.sharingPeers.has(p.publicKey);` and render a clickable badge when true:
```tsx
            {isSharing ? (
              <button
                className="voice-live-badge"
                title="Watch screen share"
                onClick={() => voice.toggleWatch(p.publicKey)}
              >LIVE</button>
            ) : null}
```
(`toggleWatch` is added in Task 6; for THIS task it can be a no-op placeholder on the interface so the badge renders + tsc passes. If Task 6 isn't done, wire `toggleWatch` as `() => {}` in the hook and replace in Task 6.)

- [ ] **Step 3: Theme CSS.** Add `.voice-live-badge` to all 3 themes — a small pill: red/live accent background, white text, tiny font, rounded, clickable cursor. Colors via theme vars where possible; a `LIVE` red is acceptable as a theme var if one exists (e.g. `var(--xp-danger)`/`var(--xp-red)`), else use the theme's strongest accent var. Per CLAUDE.md, no raw hex except if a theme genuinely lacks any red — then add a `--xp-live` var to that theme. Verify the var exists in each file.

- [ ] **Step 4: tsc + grep:**
```
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
grep -l "voice-live-badge" client/src/themes/*/theme.css   # 3 files
```

- [ ] **Step 5: Commit:**
```bash
git add client/src/hooks/useVoice.ts client/src/components/ChannelSidebar.tsx client/src/themes/*/theme.css
git commit -m "client ui: LIVE badge on the sharing participant"
```

---

### Task 6: Frontend — click-to-watch viewer pane + game-audio slider

**Files:** Modify `client/src/hooks/useVoice.ts`, `client/src/components/PeerVideoTiles.tsx`, theme CSS ×3

Convert `PeerVideoTiles` from auto-show to click-to-watch and add the per-peer game-audio slider + an enlarge control.

- [ ] **Step 1: Watch-set state in useVoice.** Add a set of pubkeys the user is watching + the toggle:
```ts
  const [watching, setWatching] = useState<Set<string>>(new Set());
  const toggleWatch = useCallback((pubkey: string) => {
    setWatching((prev) => {
      const next = new Set(prev);
      if (next.has(pubkey)) next.delete(pubkey); else next.add(pubkey);
      return next;
    });
  }, []);
```
Reset `setWatching(new Set())` on call end. When a peer stops sharing (`voice://peer-video-sharing {sharing:false}`), also remove them from `watching` (in that listener). Expose `watching`, `toggleWatch` on the interface + returned object (replace the Task-5 placeholder `toggleWatch`).

- [ ] **Step 2: Gate PeerVideoTiles on the watch set.** `PeerVideoTiles` currently auto-renders a tile per session. Change it to accept the watching set + render tiles ONLY for sessions whose `pubkey` is in `watching`. Pass `watching` (and `onSetGain`) as props from where `PeerVideoTiles` is mounted (ChannelSidebar) — or read them from a context/hook the component already has access to. Simplest: make `PeerVideoTiles` take props `{ watching: Set<string>; onSetGain: (pubkey: string, gain: number) => void }`. In the render, filter `entries` to `info.pubkey ∈ watching`. Keep the decoder lifecycle as-is (decoders still created on frames; only the VISIBLE tiles are gated — or gate decoder creation too, to save CPU: only create a decoder when watched). Minimal: gate the RENDER (`entries.filter(([, info]) => watching.has(info.pubkey))`) and the decoder-create in the frame listener on `watching.has(p.pubkey)` so unwatched streams don't decode. Read the current frame-listener + ref-callback decoder-create and add the `watching` guard.

- [ ] **Step 3: Game-audio slider + enlarge in each tile.** In the tile JSX (the watched ones), add below the canvas:
```tsx
          <div className="peer-video-controls">
            <button className="peer-video-close" title="Stop watching" onClick={() => onClose(info.pubkey)}>✕</button>
            <input
              type="range" min={0} max={2} step={0.05} defaultValue={1}
              className="peer-video-volume"
              title="Game audio volume"
              onChange={(e) => onSetGain(info.pubkey, Number(e.target.value))}
            />
          </div>
```
`onSetGain` calls `api.voiceSetScreenAudioGain(pubkey, gain)` (wire it from the hook: a `setGameAudioVolume` callback exposed by useVoice that calls the bridge). `onClose` calls `toggleWatch(pubkey)` (removes from watching → tile hides; the sharer keeps broadcasting). For "enlarge/fullscreen", add a button that toggles a `peer-video-tile--large` class on the tile (CSS makes it bigger / spans the pane). Keep it simple — a CSS size toggle, not real OS fullscreen.

- [ ] **Step 4: Mount wiring.** Update `ChannelSidebar` (where `<PeerVideoTiles />` is mounted) to pass `watching={voice.watching}`, `onSetGain={voice.setGameAudioVolume}`, `onClose={voice.toggleWatch}` (add `setGameAudioVolume` to useVoice: `const setGameAudioVolume = useCallback((pk, g) => { void api.voiceSetScreenAudioGain(pk, g); }, []);`).

- [ ] **Step 5: Theme CSS.** Add `.peer-video-controls`, `.peer-video-close`, `.peer-video-volume`, `.peer-video-tile--large` to all 3 themes (vars-driven; the existing `.peer-video-*` classes from C2 are the style baseline — match them). The range input can be left mostly native but give it a sensible width + the accent via `accent-color: var(--xp-blue)` (or the theme's accent var).

- [ ] **Step 6: tsc + grep:**
```
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
grep -l "peer-video-controls" client/src/themes/*/theme.css   # 3 files
```

- [ ] **Step 7: Commit:**
```bash
git add client/src/hooks/useVoice.ts client/src/components/PeerVideoTiles.tsx client/src/components/ChannelSidebar.tsx client/src/themes/*/theme.css
git commit -m "client ui: click-to-watch viewer pane + game-audio volume slider"
```

---

### Task 7: Docs + verification gate

**Files:** Modify `docs/modules/voice-video-transport.md`, `docs/modules/tauri-commands.md`, `docs/modules/frontend-state.md` (if it documents useVoice), `ARCHITECTURE.md`

- [ ] **Step 1: Docs.** `voice-video-transport.md`: a "Phase E — share UI" note — `list_display_sources` + `source_id` on start-share (monitor picker; window capture deferred), the `voice://peer-video-sharing` event, `voice_set_screen_audio_gain` (per-peer game-audio volume driving the `screen_audio_rings` gain), and the frontend (video-source picker, LIVE badge, click-to-watch `PeerVideoTiles` + game-audio slider, one-sharer guard). `tauri-commands.md`: `list_display_sources`, `voice_set_screen_audio_gain`, and the new `source_id` param on `voice_start_screen_share` (+ bridge fns). `frontend-state.md` (if present): the new `useVoice` members (`displaySources`/`sourceId`/`setSourceId`, `sharingPeers`/`someoneElseSharing`, `watching`/`toggleWatch`/`setGameAudioVolume`). `ARCHITECTURE.md`: one line — screensharing is feature-complete (all 5 phases): pick a monitor + game-audio device → share → peers see a LIVE badge, click to watch with an independent game-audio volume.

- [ ] **Step 2: Full gate:**
```bash
cd /home/deez/farder && cargo test --workspace 2>&1 | grep -E "^test result" | tail -25
cd /home/deez/farder/client/src-tauri && cargo build && cargo test -- --test-threads=1 2>&1 | grep -E "^test result"
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
cd /home/deez/farder && for c in list_display_sources voice_set_screen_audio_gain voice_start_screen_share; do grep -q "$c" client/src-tauri/src/main.rs && echo "OK $c" || echo "MISSING $c"; done
```
All green (client single-threaded; the `mock_capture_emits_frames_at_expected_fps` flake re-runs alone). If any voice/media test fails for a real reason, STOP and report.

- [ ] **Step 3: Commit docs:**
```bash
git add docs/ ARCHITECTURE.md
git commit -m "docs: share UI — source picker, LIVE badge, viewer + game-audio slider (Phase E)"
```

- [ ] **Step 4: Owner two-client runtime verification (report, not code).** UNVERIFIED until the owner's Windows run. Rebuild both clients (and the sidecar if any server change — Phase E is client-only, but ensure both clients are on the same build). Two clients in a voice channel: A sees a monitor dropdown + game-audio dropdown next to Share; A picks a monitor + the physical audio device, clicks Share. B sees a **LIVE** badge on A in the participant list; B clicks it → a video tile appears showing A's screen, with a **game-audio volume slider** that changes how loud A's game is (independent of A's voice). B closes the tile (✕) → stops watching but A keeps sharing for others. While A shares, B's own Share button is disabled (one-sharer guard). Confirm Stop ends A's share + clears the badge + closes B's tile. Repeat over a DIRECT server. This completes the screensharing feature.

---

## Self-review notes (done at plan time)

- **Spec coverage (Phase E UI):** Share button + source picker (Task 1+4 — monitor picker via `list_display_sources`/`source_id`; OS chooser dialog intentionally replaced by a themed dropdown for consistency + testability; window capture deferred, noted); Stop Sharing (already the C2 toggle); LIVE badge clickable (Task 2 event + Task 5 badge); click-to-watch viewer pane with game-audio slider (Task 3 gain command + Task 6 viewer); one-sharer-per-channel client guard (Task 4/5); themed in all three (each UI task adds CSS ×3). The "enlarge/fullscreen" is a CSS size toggle (Task 6), not OS fullscreen — a reasonable v1.
- **Type consistency:** `start_screen_share(fps,max_width,max_height,source_id,audio_device_id)` ↔ `voice_start_screen_share(...,source_id,audio_device_id)` ↔ `voiceStartScreenShare(30,1280,720,sourceId,audioDeviceId)`; `DisplaySource{id,kind,label,width,height}` ↔ TS `DisplaySource`; `voice_set_screen_audio_gain(pubkey_hex,gain)` ↔ `voiceSetScreenAudioGain(pubkeyHex,gain)` ↔ `set_screen_audio_gain` writing `screen_audio_rings`; event `voice://peer-video-sharing {pubkey,sharing}` ↔ useVoice `sharingPeers`; `watching: Set<pubkey>` ↔ `toggleWatch`/`PeerVideoTiles` filter.
- **Proven-path risk:** Task 1 changes `start_screen_share`'s source pick (None→first preserves current behavior); Task 2/3 only ADD an event + a gain setter (no change to send/recv/mix); the mic-voice + video + screen-audio data paths are untouched. The full voice suite gates Tasks 1-3.
- **No new native code / no probe:** Phase E reuses `enumerate_sources` (Phase-B-validated monitors), the existing recv/mix/gain, and the WebCodecs viewer. Everything except the real WGC capture (owner's Windows run, unchanged from B/C/D) is headless-testable via the mock backend.
- **Known judgment calls:** monitor-only source picker (window capture deferred — display_wgc has no window enumeration yet); game-audio volume is ephemeral (not persisted like voice peer volume) for v1; the LIVE-badge `someoneElseSharing` guard is client-side only (the spec accepts a server-side guard as a non-required add); "fullscreen" is a CSS size toggle. `PeerVideoTiles` gates decoder creation on the watch set so unwatched streams don't burn CPU decoding.
