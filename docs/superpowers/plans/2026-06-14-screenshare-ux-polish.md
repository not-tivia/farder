# Screenshare UX Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a sharer pick a window *or* screen from a Share popover, see a self "LIVE" indicator and a self-preview of what they're sending, and let viewers opt in to watching via a Join button that opens a roomy main-area stage (retiring the cramped sidebar tiles).

**Architecture:** Reuses the A–E pipeline. `display_wgc` enumerates windows alongside monitors (windows-capture `Window` API) and `start_capture` branches on a `monitor:`/`window:` id prefix. The controller's encode loop — which already drives the network `VideoSender` — additionally emits each encoded frame to a local `voice://self-video-frame` event for the self-preview (no second capture). The UI replaces the inline voice-bar dropdowns with a **Share popover** (grouped screen/window source list + audio picker + Go Live), shows a self LIVE state, and renders the single active stream (your self-preview, or a peer you Joined) in a new **`ScreenShareStage`** mounted in the main content area; the sidebar shows LIVE + a Join button. The new Windows-only window-capture code is de-risked by a throwaway probe first.

**Tech Stack:** Rust (`windows-capture` 2.0.0 `Window` API, the voice controller, the existing `DisplayBackend`/encode loop), Tauri events, React/TypeScript + the existing WebCodecs decoder, per-theme CSS.

**Spec:** `docs/superpowers/specs/2026-06-14-screenshare-ux-polish-design.md`.

**Branch:** create `screenshare-ux` from `main` before Task 1. Finish with ff-merge + push, then delete `windowcap-probe/`.

**Scope note:** Monitor + window capture; one sharer per channel (so the stage shows at most one stream); themed dropdown/popover (not the OS WGC chooser). Out: multi-stream view, server-side one-sharer guard, persisted game-audio volume.

---

## Verified codebase facts (read 2026-06-14 — exact)

- **windows-capture 2.0.0 `Window`** (`windows_capture::window::Window`): `Window::enumerate() -> Result<Vec<Window>, Error>`; `window.title() -> Result<String, Error>`; `width()/height() -> Result<i32, Error>`; `process_name() -> Result<String, Error>`. `Window` implements the same capture-item trait `Monitor` does, so `Settings::new(window, …)` + `FrameHandler::start_free_threaded(settings)` work identically (the resulting `CaptureControl<FrameHandler, Error>` type is the same regardless of monitor vs window — it's generic over the handler, not the item).
- **DisplaySource** (`client/src-tauri/src/display.rs:15-28`): `enum DisplaySourceKind { Screen, Window }` (`#[derive(Debug, Clone, Copy, serde::Serialize)]`), `struct DisplaySource { id: String, kind: DisplaySourceKind, label: String, width: u32, height: u32 }`. `DisplayBackend` trait (`:46-55`): `enumerate_sources() -> Result<Vec<DisplaySource>, String>`, `start_capture(source_id: &str, format) -> Result<mpsc::Receiver<VideoFrame>, String>`, `stop_capture`, `backend_name`.
- **WGC backend** (`client/src-tauri/src/display_wgc.rs`): `enumerate_sources` (~140-160) loops `Monitor::enumerate()` → `id "monitor:{i+1}"`. `start_capture` (~162-219) parses `strip_prefix("monitor:")` → `Monitor::from_index(idx as usize)`, builds `Settings::new(monitor, CursorCaptureSettings::Default, DrawBorderSettings::Default, SecondaryWindowSettings::Default, MinimumUpdateIntervalSettings::Default, DirtyRegionSettings::Default, ColorFormat::Rgba8, flags)`, then `FrameHandler::start_free_threaded(settings)`. `flags: CaptureFlags { sink, stop, started }` is moved into Settings. `imports` at top include `monitor::Monitor`; add `window::Window`.
- **Encode sink** (`client/src-tauri/src/voice/mod.rs:1429-1442`, inside `start_screen_share`): spawns `std::thread` building the `H264Encoder` (it's !Send) + a `VideoSender`, then `crate::screenshare::run_encode_loop(rx, encoder, stop_t, force_t, move |enc| { sender.send(&enc, |b| { let _ = server_t.send_datagram(b); }); })`. `EncodedFrame { data: Vec<u8>, is_keyframe: bool, timestamp_ms: u64 }` (`video_encoder.rs:12-19`).
- **Emitter** (`voice/mod.rs:425`): `emitter: Arc<dyn VoiceEventEmitter>` — `Clone + Send + Sync` (it's an `Arc`); `VoiceEventEmitter::emit(&self, event: &str, payload: serde_json::Value)`. Test double records emitted events (used by Phase E's `voice://peer-video-sharing` test).
- **Base64 emit pattern** (`screenshare.rs:93-100`): `let b64 = base64::engine::general_purpose::STANDARD.encode(&enc.data);` then `emit("...", json!({ "data": b64, "key": enc.is_keyframe, "ts": enc.timestamp_ms }))`.
- **WebCodecs decoder** (`client/src/components/PeerVideoTiles.tsx:15-92`): `PeerDecoder` class (configure `avc1.42E01E`, key-gated, `EncodedVideoChunk`, draw to canvas) + `FramePayload { session, pubkey, data, key, seq }`, listens `voice://peer-video-frame`. `ScreensharePreview.tsx:30-52` is the simpler single-stream variant listening `screenshare:frame` `{ data, key, ts }`. `H264_CODEC = "avc1.42E01E"`, `b64ToBytes` helper.
- **ChatPanel** (`client/src/components/ChatPanel.tsx:119-181`): the channel main view; `<div className="message-list">` at :150. Does NOT use `useVoice` yet. Mount `<ScreenShareStage />` between the header (`:121-149`) and the message-list (`:150`). Find how ChatPanel gets `serverId`/`channelId` (props) to pass context if needed.
- **VoiceControlBar** (`client/src/components/VoiceControlBar.tsx:64-112`): the `vcb-share-setup` block (two `.vcb-source-select`s shown when `!isSharing`) — REMOVE; the `vcb-buttons` row with the 🖥 Share button (`:104-110`, `disabled={someoneElseSharing && !isSharing}`, `onClick` toggles start/stop).
- **useVoice** (`client/src/hooks/useVoice.ts:13-42` interface; `startShare` :148-154 = `await api.voiceStartScreenShare(30,1280,720,sourceId,audioDeviceId); setIsSharing(true)`; listener `useEffect` :108-146 with `safePush`; `applyState` reset :91-106). Current members incl. `isSharing/startShare/stopShare/displaySources/sourceId/setSourceId/audioDevices/audioDeviceId/setAudioDeviceId/sharingPeers/someoneElseSharing/watching/toggleWatch/setGameAudioVolume`.
- **ChannelSidebar** (`client/src/components/ChannelSidebar.tsx`): `<PeerVideoTiles watching=… onClose=… onSetGain=… />` mounted at :535 (REMOVE — retired). Participant row :414-445 renders the `voice-live-badge` button when `voice.sharingPeers.has(p.publicKey)`; `ownPk`/`isSelf` are computed there.
- **list_display_sources** (`commands.rs:3002-3004`): `make_display_backend().enumerate_sources()`. Bridge `listDisplaySources()` in `tauri-bridge.ts`.

---

### Task 1: Window-capture probe (throwaway, owner runs on Windows)

**Files:** Create `windowcap-probe/Cargo.toml`, `windowcap-probe/src/main.rs`, `windowcap-probe/README.md`

This validates the windows-capture 2.0.0 `Window` API on the owner's box BEFORE Task 2 builds against it (the WGC + WASAPI paths both had API surprises only visible on Windows). EXECUTION PAUSES here for the owner to run it and paste output.

- [ ] **Step 1: Cargo.toml** (detached standalone crate, Windows-only dep):
```toml
[package]
name = "windowcap-probe"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[target.'cfg(windows)'.dependencies]
windows-capture = "2.0.0"
```

- [ ] **Step 2: `src/main.rs`** — enumerate windows, print titles + sizes, and confirm a capture `Settings` can be built against a `Window` (compile-checks the trait):
```rust
//! THROWAWAY probe: validate windows-capture 2.0.0 Window enumeration + that a
//! capture Settings can be constructed against a Window (same trait as Monitor).
//! Delete once window capture is folded into the screenshare feature.

#[cfg(not(windows))]
fn main() { eprintln!("Windows-only probe. Run on the Windows box."); std::process::exit(2); }

#[cfg(windows)]
fn main() {
    use windows_capture::window::Window;
    let windows = match Window::enumerate() {
        Ok(w) => w,
        Err(e) => { eprintln!("Window::enumerate failed: {e}"); std::process::exit(1); }
    };
    println!("Found {} window(s):", windows.len());
    for (i, w) in windows.iter().enumerate() {
        let title = w.title().unwrap_or_else(|_| "<no title>".into());
        let proc = w.process_name().unwrap_or_else(|_| "<?>".into());
        let (ww, hh) = (w.width().unwrap_or(-1), w.height().unwrap_or(-1));
        println!("[{i}] {ww}x{hh}  \"{title}\"  ({proc})");
    }
    // Compile-check: a Window satisfies the capture-item trait Settings::new wants.
    // (We don't actually start a capture here — enumerate + the type-check is the
    // goal; full capture is exercised by the real app on the owner's run.)
    if let Some(w) = windows.into_iter().find(|w| w.title().map(|t| !t.is_empty()).unwrap_or(false)) {
        use windows_capture::settings::{
            ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
            MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
        };
        let _settings = Settings::new(
            w,
            CursorCaptureSettings::Default,
            DrawBorderSettings::Default,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Rgba8,
            (), // flags: unit is fine for a type-check
        );
        println!("OK: Settings::new(Window, ...) type-checks.");
    }
    println!("PROBE OK: window enumeration + Settings-against-Window compile/run.");
    println!("Paste this output back.");
}
```
NOTE: if `Settings::new`'s last arg can't be `()` (flags type bound), the implementer adapts — the goal is to confirm `Window` is accepted as the first arg; if the flags bound fights, drop the Settings block and keep just enumeration (the real app supplies real flags). Report which.

- [ ] **Step 3: README** (`windowcap-probe/README.md`): "Run on Windows: `git pull; cd windowcap-probe; cargo run --release`. Paste the output (window list + PROBE OK, or the compile error)."

- [ ] **Step 4: Commit + push so the owner can pull:**
```bash
git add windowcap-probe/ && git commit -m "Add throwaway window-capture probe (validate windows-capture Window API)"
git push origin screenshare-ux   # or main if executing on main; owner pulls + runs
```

- [ ] **Step 5: PAUSE.** Owner runs it; paste the window list + `PROBE OK` (or the compile error). Confirm `Window::enumerate()`, `.title()`, and `Settings::new(Window, …)` work. If an API name differs, lock the corrected names into Task 2 before building it.

---

### Task 2: Backend — window source enumeration + capture + self-preview event

**Files:** Modify `client/src-tauri/src/display_wgc.rs` (cfg-windows), `client/src-tauri/src/voice/mod.rs`

- [ ] **Step 1: Enumerate windows in `display_wgc.rs`.** Add `use windows_capture::window::Window;` to the imports. In `enumerate_sources`, AFTER the monitor loop (keep it), append windows:
```rust
        // Windows (single-window capture). Skip empty-title windows (tool/hidden).
        if let Ok(windows) = Window::enumerate() {
            for (i, w) in windows.into_iter().enumerate() {
                let title = match w.title() {
                    Ok(t) if !t.trim().is_empty() => t,
                    _ => continue,
                };
                let width = w.width().unwrap_or(0).max(0) as u32;
                let height = w.height().unwrap_or(0).max(0) as u32;
                out.push(DisplaySource {
                    id: format!("window:{}", i),
                    kind: DisplaySourceKind::Window,
                    label: format!("\u{1FA9F} {}", title), // 🪟 prefix
                    width,
                    height,
                });
            }
        }
        Ok(out)
```
(Adapt `Window::enumerate`/`title`/`width`/`height` to the EXACT names the Task-1 probe confirmed.)

- [ ] **Step 2: Branch `start_capture` on the id prefix.** Refactor the monitor-only parse into a `monitor:`/`window:` branch. Both branches construct their own `Settings` + call `start_free_threaded` and yield the same `CaptureControl<FrameHandler, _>`. Build the `flags` once (it's moved into whichever branch runs — fine, branches are exclusive):
```rust
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel::<VideoFrame>(4);
        let now = Instant::now();
        let flags = CaptureFlags { sink: tx, stop: stop.clone(), started: now };

        let control = if let Some(idx) = source_id.strip_prefix("monitor:") {
            let idx: u32 = idx.parse().map_err(|_| format!("bad source_id: {source_id}"))?;
            if idx == 0 { return Err("monitor index must be >= 1".into()); }
            let monitor = Monitor::from_index(idx as usize).map_err(|e| format!("monitor {idx}: {e}"))?;
            let settings = Settings::new(
                monitor,
                CursorCaptureSettings::Default, DrawBorderSettings::Default,
                SecondaryWindowSettings::Default, MinimumUpdateIntervalSettings::Default,
                DirtyRegionSettings::Default, ColorFormat::Rgba8, flags,
            );
            FrameHandler::start_free_threaded(settings).map_err(|e| format!("start capture: {e:?}"))?
        } else if let Some(idx) = source_id.strip_prefix("window:") {
            let idx: usize = idx.parse().map_err(|_| format!("bad source_id: {source_id}"))?;
            let window = Window::enumerate().map_err(|e| format!("enumerate windows: {e}"))?
                .into_iter().nth(idx)
                .ok_or_else(|| "that window is no longer available — refresh".to_string())?;
            let settings = Settings::new(
                window,
                CursorCaptureSettings::Default, DrawBorderSettings::Default,
                SecondaryWindowSettings::Default, MinimumUpdateIntervalSettings::Default,
                DirtyRegionSettings::Default, ColorFormat::Rgba8, flags,
            );
            FrameHandler::start_free_threaded(settings).map_err(|e| format!("start capture: {e:?}"))?
        } else {
            return Err(format!("bad source_id: {source_id}"));
        };

        *control_slot = Some(control);
        *self.stop.lock().map_err(|e| e.to_string())? = Some(stop);
        *self.started.lock().map_err(|e| e.to_string())? = Some(now);
        Ok(rx)
```
Keep the existing `format`-validation + `control_slot.is_some()` guard at the top. NOTE: `window:{idx}` re-resolves by index at capture time; the popover refreshes the list right before Go Live to keep indices fresh (the "no longer available" error surfaces the rare race).

- [ ] **Step 3: Emit the self-preview frame in `start_screen_share`'s encode sink** (`voice/mod.rs`). Clone the emitter into the spawned thread and emit each encoded frame locally alongside the network send:
```rust
        let stop_t = stop.clone();
        let force_t = force_keyframe.clone();
        let server_t = server.clone();
        let emitter_t = self.emitter.clone();
        let thread = std::thread::spawn(move || {
            let encoder = match crate::video_encoder::H264Encoder::new() {
                Ok(e) => e,
                Err(e) => { eprintln!("[voice] video encoder init failed: {e}"); return; }
            };
            let mut sender = crate::voice::send_video::VideoSender::new(video_key, my_session_id, my_pk_bytes);
            crate::screenshare::run_encode_loop(rx, encoder, stop_t, force_t, move |enc| {
                // Self-preview: emit the same encoded frame to a local event so the
                // sharer's stage shows exactly what's being sent (no 2nd capture).
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&enc.data);
                emitter_t.emit(
                    "voice://self-video-frame",
                    serde_json::json!({ "data": b64, "key": enc.is_keyframe, "seq": enc.timestamp_ms }),
                );
                sender.send(&enc, |b| { let _ = server_t.send_datagram(b); });
            });
        });
```

- [ ] **Step 4: Test the self-frame emit** (controller test, headless via the mock display backend + the recording emitter). Mirror the Phase C2 `start_then_stop_screen_share_*` test setup (FakeServerSession + `FARDER_DISPLAY_BACKEND=mock`), then assert the emitter recorded a `voice://self-video-frame` shortly after start:
```rust
    #[tokio::test]
    async fn sharing_emits_self_video_frames() {
        std::env::set_var("FARDER_DISPLAY_BACKEND", "mock");
        // build ctrl + FakeServerSession (1 member) + a recording emitter — copy the
        // exact construction from start_then_stop_screen_share_offers_enables_and_disables_video.
        // ctrl.join(...).await.unwrap();
        // ctrl.start_screen_share(15, 320, 240, None, None).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(400)).await; // let the mock capture + encode produce a frame
        let saw_self_frame = /* recorded events */.iter().any(|(name, _)| name == "voice://self-video-frame");
        assert!(saw_self_frame, "sharing must emit voice://self-video-frame for the self-preview");
        // ctrl.stop_screen_share().await.unwrap();
        std::env::remove_var("FARDER_DISPLAY_BACKEND");
    }
```
ADAPT to the actual test-double emitter accessor (read how the Phase E `voice://peer-video-sharing` test reads recorded events). If timing makes it flaky, raise the sleep or assert via a short poll loop. If the recording emitter isn't wired into the controller these tests build, extend the test harness minimally to capture events (it already is, per the Phase E test). NOTE what you did.

- [ ] **Step 5: Build + test.**
```
cd /home/deez/farder/client/src-tauri && cargo build && cargo test voice:: -- --test-threads=1 2>&1 | grep -E "test result|error\["
```
Clean + green (display_wgc is cfg-windows, not compiled on Linux — verify no error originates outside it; the self-frame test runs via the mock).

- [ ] **Step 6: Commit:**
```bash
git add client/src-tauri/src/display_wgc.rs client/src-tauri/src/voice/mod.rs
git commit -m "client: enumerate+capture windows; emit voice://self-video-frame for self-preview"
```

---

### Task 3: Frontend — Share popover + self LIVE indicator

**Files:** Create `client/src/components/ShareSetupPopover.tsx`; modify `client/src/hooks/useVoice.ts`, `client/src/components/VoiceControlBar.tsx`, theme CSS ×3

- [ ] **Step 1: useVoice — a refresh + keep startShare.** Add a `refreshDisplaySources` that re-fetches the source list (windows change), and expose it. In the hook:
```ts
  const refreshDisplaySources = useCallback(async () => {
    try {
      const s = await api.listDisplaySources();
      setDisplaySources(s);
      setSourceId((cur) => (cur && s.some((x) => x.id === cur)) ? cur : (s[0]?.id ?? null));
    } catch { /* ignore */ }
  }, []);
```
Add `refreshDisplaySources: () => Promise<void>` to the `UseVoice` interface + returned object. (`startShare`, `sourceId`, `setSourceId`, `audioDevices`, `audioDeviceId`, `setAudioDeviceId` already exist.)

- [ ] **Step 2: Create `ShareSetupPopover.tsx`** — the source/audio picker + Go Live:
```tsx
import { useEffect } from "react";
import type { UseVoice } from "../hooks/useVoice";

export default function ShareSetupPopover({ voice, onClose }: { voice: UseVoice; onClose: () => void }) {
  useEffect(() => { void voice.refreshDisplaySources(); }, []); // fresh window list each open

  const screens = voice.displaySources.filter((s) => s.kind === "Screen");
  const windows = voice.displaySources.filter((s) => s.kind === "Window");

  return (
    <div className="share-popover" role="dialog" aria-label="Start sharing">
      <div className="share-popover-head">
        <span>Start sharing</span>
        <button className="share-popover-refresh" title="Refresh sources" onClick={() => void voice.refreshDisplaySources()}>&#x21BB;</button>
      </div>
      <div className="share-popover-sources">
        {screens.length > 0 && <div className="share-popover-group">&#x1F4FA; Screens</div>}
        {screens.map((s) => (
          <button key={s.id} className={`share-popover-item${voice.sourceId === s.id ? " selected" : ""}`} onClick={() => voice.setSourceId(s.id)}>{s.label}</button>
        ))}
        {windows.length > 0 && <div className="share-popover-group">&#x1FA9F; Windows</div>}
        {windows.map((s) => (
          <button key={s.id} className={`share-popover-item${voice.sourceId === s.id ? " selected" : ""}`} onClick={() => voice.setSourceId(s.id)}>{s.label}</button>
        ))}
      </div>
      <label className="share-popover-audio">
        <span>&#x1F50A; Game audio</span>
        <select value={voice.audioDeviceId ?? ""} onChange={(e) => voice.setAudioDeviceId(e.target.value)}>
          {voice.audioDevices.map((d) => (
            <option key={d.id} value={d.id}>{d.name}{d.is_default ? " (default)" : ""}</option>
          ))}
        </select>
      </label>
      <div className="share-popover-actions">
        <button className="share-popover-cancel" onClick={onClose}>Cancel</button>
        <button
          className="share-popover-go"
          disabled={!voice.sourceId}
          onClick={() => { void voice.startShare(); onClose(); }}
        >Go Live</button>
      </div>
    </div>
  );
}
```

- [ ] **Step 3: VoiceControlBar — replace the inline dropdowns with the popover + a self LIVE indicator.** Remove the `{!voice.isSharing && (<div className="vcb-share-setup">…</div>)}` block. Add popover state + render. When `isSharing`, show a LIVE indicator + Stop in place of the Share button:
```tsx
  const [sharePopover, setSharePopover] = useState(false);
```
Replace the Share button in `.vcb-buttons` with:
```tsx
        {voice.isSharing ? (
          <button className="vcb-btn active vcb-live" title="Stop sharing your screen" onClick={() => { void voice.stopShare(); }}>
            <span className="vcb-live-dot" /> LIVE
          </button>
        ) : (
          <button
            className="vcb-btn"
            title={voice.someoneElseSharing ? "Someone else is already sharing" : "Share your screen"}
            disabled={voice.someoneElseSharing}
            onClick={() => setSharePopover((v) => !v)}
          ><span>&#x1F5A5;</span></button>
        )}
```
And render the popover (anchored above the bar) when `sharePopover && !voice.isSharing`:
```tsx
      {sharePopover && !voice.isSharing && (
        <ShareSetupPopover voice={voice} onClose={() => setSharePopover(false)} />
      )}
```
(Import `ShareSetupPopover` + `useState`. Place the popover render inside the `.voice-control-bar` root so CSS can position it.)

- [ ] **Step 4: Theme CSS** — add to all 3 `client/src/themes/*/theme.css`: `.share-popover` (absolutely positioned above the bar, panel bg, border, shadow, z-index, ~240px), `.share-popover-head` (flex, title + refresh button), `.share-popover-refresh`, `.share-popover-sources` (column, max-height + scroll), `.share-popover-group` (tiny muted heading), `.share-popover-item` (full-width button; `.selected` highlighted via accent var), `.share-popover-audio` (label + select, the select `min-width:0; flex:1`), `.share-popover-actions` (flex end, Cancel + Go Live), `.share-popover-go` (accent bg, disabled state), `.vcb-live` + `.vcb-live-dot` (the LIVE pill — red dot via `--xp-live`/`--xp-bow-red`, matching the badge from Phase E). All colors via theme vars (the `.vcb-source-select` rule from the prior fix can be removed since the inline dropdowns are gone — or left unused; removing is cleaner). Verify vars per theme.

- [ ] **Step 5: tsc + grep:**
```
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
grep -l "share-popover" client/src/themes/*/theme.css   # 3 files
```

- [ ] **Step 6: Commit:**
```bash
git add client/src/components/ShareSetupPopover.tsx client/src/components/VoiceControlBar.tsx client/src/hooks/useVoice.ts client/src/themes/*/theme.css
git commit -m "client ui: Share popover (screen/window picker) + self LIVE indicator"
```

---

### Task 4: Frontend — main-area stage + Join button + retire sidebar tiles

**Files:** Create `client/src/components/ScreenShareStage.tsx`; modify `client/src/components/ChatPanel.tsx`, `client/src/components/ChannelSidebar.tsx`, theme CSS ×3; delete `client/src/components/PeerVideoTiles.tsx`

- [ ] **Step 1: Create `ScreenShareStage.tsx`.** One large pane showing the single active stream: your self-preview (`voice://self-video-frame`) when `isSharing`, else the joined peer (`voice://peer-video-frame`, the one in `watching`) — with a game-audio slider for peers and a ✕/Stop. Reuse the `PeerDecoder` decode pattern:
```tsx
import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import type { UseVoice } from "../hooks/useVoice";

const H264_CODEC = "avc1.42E01E";
function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64); const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// A single-canvas H.264 player; key-gated, error self-healing.
function useStreamPlayer(canvasRef: React.RefObject<HTMLCanvasElement>, eventName: string,
                         match: (p: any) => boolean, active: boolean) {
  useEffect(() => {
    if (!active) return;
    let decoder: VideoDecoder | null = null;
    let gotKey = false;
    const ensure = () => {
      const canvas = canvasRef.current;
      if (!canvas || decoder) return;
      const ctx = canvas.getContext("2d")!;
      decoder = new VideoDecoder({
        output: (frame) => { canvas.width = frame.displayWidth; canvas.height = frame.displayHeight; ctx.drawImage(frame, 0, 0, canvas.width, canvas.height); frame.close(); },
        error: () => { try { decoder?.close(); } catch {} decoder = null; gotKey = false; },
      });
      decoder.configure({ codec: H264_CODEC, optimizeForLatency: true });
    };
    const un = listen<any>(eventName, (e) => {
      const p = e.payload;
      if (!match(p)) return;
      ensure();
      if (!decoder) return;
      if (!gotKey && !p.key) return;
      if (p.key) gotKey = true;
      try { decoder.decode(new EncodedVideoChunk({ type: p.key ? "key" : "delta", timestamp: p.seq ?? 0, data: b64ToBytes(p.data) })); } catch { /* drop */ }
    });
    return () => { un.then((u) => u()); try { decoder?.close(); } catch {} };
  }, [eventName, active]); // eslint-disable-line react-hooks/exhaustive-deps
}

export default function ScreenShareStage({ voice }: { voice: UseVoice }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  // One sharer per channel: show self-preview when sharing, else the joined peer.
  const watchedPubkey = [...voice.watching][0] ?? null;
  const showSelf = voice.isSharing;
  const showPeer = !showSelf && watchedPubkey != null;

  useStreamPlayer(canvasRef, "voice://self-video-frame", () => true, showSelf);
  useStreamPlayer(canvasRef, "voice://peer-video-frame", (p) => p.pubkey === watchedPubkey, showPeer);

  if (!showSelf && !showPeer) return null;
  return (
    <div className="screen-stage">
      <div className="screen-stage-head">
        <span className="screen-stage-title">{showSelf ? "You're sharing" : `${watchedPubkey?.slice(0, 8)}… is sharing`}</span>
        {showPeer && watchedPubkey && (
          <input className="screen-stage-vol" type="range" min={0} max={2} step={0.05} defaultValue={1}
            title="Game audio volume" onChange={(e) => voice.setGameAudioVolume(watchedPubkey, Number(e.target.value))} />
        )}
        <button className="screen-stage-close" title={showSelf ? "Stop sharing" : "Stop watching"}
          onClick={() => { if (showSelf) void voice.stopShare(); else if (watchedPubkey) voice.toggleWatch(watchedPubkey); }}>&#x2715;</button>
      </div>
      <canvas ref={canvasRef} className="screen-stage-canvas" />
    </div>
  );
}
```
(The `match` for self is `() => true` since the self event is unambiguous. The peer player only runs when `showPeer`. Both target the same canvas but only ONE is `active` at a time given one-sharer-per-channel, so they don't fight.)

- [ ] **Step 2: Mount in ChatPanel.** In `client/src/components/ChatPanel.tsx`: `import { useVoice } from "../hooks/useVoice";` and `import ScreenShareStage from "./ScreenShareStage";`. Add `const voice = useVoice();` in the component body. Between the channel header (`:149`) and `<div className="message-list">` (`:150`), render:
```tsx
        <ScreenShareStage voice={voice} />
```
(`ScreenShareStage` returns null when nothing is active, so it's safe to always mount.)

- [ ] **Step 3: Sidebar — Join button + self LIVE badge; remove PeerVideoTiles.** In `ChannelSidebar.tsx`: delete the `<PeerVideoTiles … />` mount (`:535`) and its import. In the participant row (`:414-445`), change the LIVE affordance: for a REMOTE sharing peer show a LIVE badge + a Join button; for SELF (when `voice.isSharing`) show a LIVE badge:
```tsx
        {isSelf && voice.isSharing ? (
          <span className="voice-live-badge" title="You're sharing">LIVE</span>
        ) : (!isSelf && voice.sharingPeers.has(p.publicKey)) ? (
          <button
            className="voice-live-badge"
            title={voice.watching.has(p.publicKey) ? "Stop watching" : "Join screen share"}
            onClick={() => voice.toggleWatch(p.publicKey)}
          >{voice.watching.has(p.publicKey) ? "WATCHING" : "JOIN"}</button>
        ) : null}
```
(`isSelf`/`ownPk` are already computed in this row. `ChannelSidebar` already has `voice`.)

- [ ] **Step 4: Delete the retired component.**
```bash
git rm client/src/components/PeerVideoTiles.tsx
```
Confirm nothing else imports it: `grep -rn "PeerVideoTiles" client/src` → only the removed sidebar line (now gone).

- [ ] **Step 5: Theme CSS** — add to all 3 themes: `.screen-stage` (block in the main column, margin, border `var(--xp-border)`, rounded), `.screen-stage-head` (flex row: title + volume + close, small, padding, `var(--xp-text-muted)`), `.screen-stage-title`, `.screen-stage-vol` (`accent-color` via accent var, width ~120px), `.screen-stage-close` (icon button), `.screen-stage-canvas` (`width:100%; max-height:60vh; background:#000; object-fit:contain`). `#000` letterbox is the allowed literal; everything else via vars. Verify per theme.

- [ ] **Step 6: tsc + grep:**
```
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
grep -l "screen-stage" client/src/themes/*/theme.css   # 3 files
grep -rn "PeerVideoTiles" client/src || echo "retired (good)"
```

- [ ] **Step 7: Commit:**
```bash
git add client/src/components/ScreenShareStage.tsx client/src/components/ChatPanel.tsx client/src/components/ChannelSidebar.tsx client/src/themes/*/theme.css
git commit -m "client ui: main-area screen-share stage + Join button; retire sidebar tiles"
```

---

### Task 5: Docs + verification gate + delete probe

**Files:** Modify `docs/modules/voice-video-transport.md`, `docs/modules/tauri-bridge.md`, `docs/modules/frontend-state.md`, `ARCHITECTURE.md`; delete `windowcap-probe/`

- [ ] **Step 1: Docs.** `voice-video-transport.md`: window capture (`window:` source ids, `Window::enumerate`/capture in `display_wgc`), the `voice://self-video-frame` event (self-preview, emitted from the encode loop), and the UX (Share popover, self LIVE, main-area `ScreenShareStage`, Join flow, retired `PeerVideoTiles`). `tauri-bridge.md`: the new `voice://self-video-frame` event (payload `{data, key, seq}`, consumed by `ScreenShareStage`). `frontend-state.md`: `useVoice.refreshDisplaySources`; note the picker/stage moved out of the sidebar. `ARCHITECTURE.md`: one line — screensharing supports window + screen sources with a self-preview and a main-area viewer.

- [ ] **Step 2: Full gate:**
```bash
cd /home/deez/farder && cargo test --workspace 2>&1 | grep -E "^test result" | tail -25
cd /home/deez/farder/client/src-tauri && cargo build && cargo test -- --test-threads=1 2>&1 | grep -E "^test result"
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
```
All green (client single-threaded; the `mock_capture_emits_frames_at_expected_fps` bound was widened earlier). If any voice/media test fails for a real reason, STOP and report.

- [ ] **Step 3: Delete the probe:**
```bash
cd /home/deez/farder && git rm -r windowcap-probe && git commit -m "chore: remove window-capture probe (folded into the feature)"
```

- [ ] **Step 4: Commit docs:**
```bash
git add docs/ ARCHITECTURE.md && git commit -m "docs: window capture + self-preview + main-area stage (screenshare UX)"
```

- [ ] **Step 5: Owner two-client runtime verification (report, not code).** Rebuild both clients (client-only; no sidecar change). With two clients in a voice channel: client A clicks 🖥 Share → the **popover** lists Screens AND Windows → A picks a **window** + the physical audio device → **Go Live**. A sees a **🔴 LIVE** indicator in the voice bar, a **LIVE badge on themselves**, and a **self-preview** of that window in the **main area**. Client B sees A with **LIVE + a [JOIN] button**; clicking JOIN shows A's window **in B's main area** with a working **game-audio slider**; B's ✕ stops watching (A keeps sharing). A's Stop ends it (B's stage closes, badges clear). Repeat sharing a **whole screen**, and over a **direct** server.

---

## Self-review notes (done at plan time)

- **Spec coverage:** window+screen source via `display_wgc` window enumeration + `window:` capture branch (Task 2) behind a probe (Task 1); Share popover with grouped screen/window picker + audio + Go Live (Task 3) replacing the inline dropdowns; self LIVE indicator (Task 3) + self-badge (Task 4); self-preview via the tapped encode loop → `voice://self-video-frame` → `ScreenShareStage` (Tasks 2+4); Join model + main-area stage + game-audio slider + retired sidebar tiles (Task 4). Probe-first de-risking (Task 1). Owner runtime (Task 5).
- **Type consistency:** `DisplaySource{id,kind:Screen|Window,label,…}` ↔ TS `DisplaySource` (`kind: "Screen"|"Window"`); `window:{idx}` / `monitor:{n}` id scheme matches enumerate↔start_capture; `voice://self-video-frame {data,key,seq}` emitted (Task 2) ↔ consumed by `ScreenShareStage` (Task 4); `voice_start_screen_share(fps,maxW,maxH,source_id,audio_device_id)` unchanged (Phase E); `refreshDisplaySources` added to `UseVoice` (Task 3) used by the popover; `ScreenShareStage({voice})` mounted in ChatPanel (Task 4).
- **Proven-path risk:** Task 2 ADDS window enumeration + a capture branch (the monitor branch is byte-for-byte the old path) and ADDS one emit to the encode sink (network send unchanged); Tasks 3-4 are frontend. The full voice suite gates Task 2. The new cfg(windows) window code is probe-validated (Task 1) and owner-run (Task 5) — never compiled on Linux.
- **Known judgment calls:** `window:{index}` re-resolves by enumerate order (popover refresh-before-share keeps it fresh; "no longer available" error covers the race) — the probe may reveal a stabler handle the implementer can swap in; the stage targets one canvas with two players but only one is `active` given one-sharer-per-channel; the self-preview reuses the outbound frames (faithful, zero extra capture). Game-audio volume stays ephemeral.
