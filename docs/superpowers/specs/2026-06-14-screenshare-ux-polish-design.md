# Screenshare UX Polish — Design

**Date:** 2026-06-14
**Status:** Approved (owner), pending spec review
**Builds on:** Screensharing Phases A–E (capture → H.264 → E2EE datagram transport → WebCodecs decode + WASAPI-loopback game audio, all shipped on `main`).

## Problem

Post-Phase-E owner testing surfaced four gaps in the share/watch experience:

1. **Screens only.** You can share a whole monitor but not a single window.
2. **No self-indicator.** Nothing tells you *you* are currently sharing.
3. **No self-preview.** You can't see what you're actually sharing.
4. **No clear "Join".** Being in the voice channel shouldn't auto-show someone's
   screen — viewers need a deliberate, visible way to opt in to watching, and the
   current sidebar `LIVE` badge / auto-tiles weren't a clear "join the stream"
   affordance.

Additionally, the always-visible source/audio dropdowns crammed into the narrow
voice sidebar were a layout problem (already partially fixed, but adding windows
makes a permanent dropdown untenable).

## Goals

- Share a **specific window** or a **whole screen**, chosen from a combined list.
- A clear **self "LIVE" indicator** while sharing.
- A **self-preview** showing exactly what's being sent.
- A deliberate, opt-in **Join/Watch** flow for viewers.
- A roomy **main-area viewer**, retiring the cramped sidebar tiles.

Non-goals (explicitly out): multi-stream simultaneous viewing (one sharer per
channel makes this moot for v1); server-side one-sharer enforcement (still
client-side); OS-native WGC picker dialog (we render our own list); stereo /
multi-window capture; persisting game-audio volume.

## Design

### 1. Source selection — screens + windows

`DisplaySource { id, kind: Screen|Window, label, width, height }` already exists
(the `kind` field was added in Phase E but only `Screen` was populated).

- **Backend (`display_wgc.rs`, cfg(windows)):** `enumerate_sources()` additionally
  enumerates open windows via the windows-capture `Window` API, emitting
  `DisplaySource { id: "window:<index>", kind: Window, label: <window title>, ... }`
  alongside the existing `id: "monitor:<n>"` screens. `start_capture(source_id)`
  branches on the `monitor:` / `window:` prefix: `monitor:` → `Monitor::from_index`
  (today's path, unchanged); `window:` → resolve the window and build the WGC
  `Settings` against it instead of a monitor. The mock backend (non-Windows) keeps
  returning its single `mock-display` Screen so the seam is testable headlessly.
- **Exact window-capture API is validated by a probe first** (see §5) — like the
  WASAPI loopback probe, because this is new cfg(windows) code untestable on Linux.

### 2. Share initiation — the "Start sharing" popover

The two always-visible dropdowns in the voice bar are **removed**. Instead:

- The **🖥 Share button** opens a small **"Start sharing" popover** anchored to the
  voice bar, containing:
  - A **source list** grouped into **📺 Screens** and **🪟 Windows** (from
    `list_display_sources`), each row selectable; a small **↻ Refresh** re-queries
    (windows open/close during a session).
  - The **game-audio device picker** (the Phase D output-device list).
  - A **Go Live** button that starts the share with the chosen source + audio
    device, and a **Cancel**.
- The Share button is **disabled when someone else is already sharing** (the
  existing one-sharer guard), with a tooltip explaining why.
- Closing/cancelling the popover does nothing; **Go Live** is the commit.

This makes sharing a deliberate action, removes the sidebar cramping permanently,
and scales to many windows.

### 3. Self experience while sharing

- **Voice bar:** while `isSharing`, the Share button is replaced by a **🔴 LIVE —
  Sharing** indicator (showing the chosen source label, truncated) with a **Stop**
  button.
- **Participant list:** you get a **LIVE badge on yourself** (the same badge style
  as remote sharers).
- **Self-preview:** the main-area stage (see §5) shows **what you're sending**. The
  controller's encode loop, which already feeds the network `VideoSender`, ALSO
  emits each encoded H.264 frame to a local **`voice://self-video-frame`** event
  (`{ data: base64, key: bool, seq }`) while sharing. The stage decodes it with the
  same WebCodecs path used for peers. No second capture/encode — the preview is
  literally the outbound frames, so it's always faithful. Emission is gated on the
  share being active and torn down with it.

### 4. Watching others — the Join model

- A peer who is sharing shows, in the sidebar participant row, a **LIVE badge + a
  [Join] button**. (`sharingPeers`, fed by the existing `voice://peer-video-sharing`
  event, already tracks who's live.)
- Clicking **Join** adds that peer to the `watching` set → the **main-area stage**
  starts decoding and shows their screen. Being in voice does **not** auto-join.
- Because it's **one sharer per channel**, the stage shows at most one remote
  stream. A **✕ / Leave** on the stage removes them from `watching` (stops local
  decode); the sharer keeps broadcasting for anyone else.

### 5. The main-area "stage"

- A new component (`ScreenShareStage`) mounts in the **main content area** (above
  the message list, within the channel view), not the sidebar.
- It renders the single active stream large:
  - **Your self-preview** when you're sharing (fed by `voice://self-video-frame`),
    labeled "You're sharing — <source>".
  - **A joined peer's stream** when watching (fed by `voice://peer-video-frame`),
    labeled with the sharer's name, plus the **game-audio volume slider** (drives
    `voice_set_screen_audio_gain`, Phase E) and a **✕ Leave**.
  - Nothing (renders null) when neither applies.
- The cramped sidebar tiles component (`PeerVideoTiles`) is **retired**; its
  WebCodecs decoder logic (key-gated, error self-heal, race-free create) is reused
  in the stage.

### 6. Window-capture probe (de-risk first)

Before building §1, a throwaway `windowcap-probe/` (cfg(windows), standalone) on
the owner's Windows box validates the windows-capture 2.0.0 **Window** API:
`Window::enumerate()` (and its title accessor), and that a capture `Settings` can
be constructed against a `Window` (compiles + runs). This mirrors the WASAPI
probe, which caught real API mismatches before they reached the implementation.
The probe is deleted once the API is locked into the feature.

## Architecture / data flow

```
Share popover ─ Go Live ─▶ voice_start_screen_share(source_id="window:…"/"monitor:…", audio_device_id)
   start_screen_share ─▶ display backend.start_capture(source_id) ─▶ capture→encode loop
        encode sink ─┬─▶ VideoSender ─▶ datagram ─▶ relay/server ─▶ peers
                     └─▶ emit voice://self-video-frame ─▶ ScreenShareStage (self-preview)

remote peer enables Video ─▶ voice://peer-video-sharing{sharing:true} ─▶ sidebar LIVE + [Join]
   click Join ─▶ watching.add(pubkey) ─▶ ScreenShareStage decodes voice://peer-video-frame
        game-audio slider ─▶ voice_set_screen_audio_gain(pubkey, gain) ─▶ screen_audio_rings gain
```

E2EE, transport, mixing, and the one-sharer guard are all unchanged from A–E;
this round is capture-source breadth + the surrounding UI/UX.

## Error handling

- **Window vanished** between enumerate and capture: `start_capture` returns an
  error; the popover surfaces a friendly "that window is no longer available —
  refresh" and stays open. Voice is unaffected.
- **Capture/permission failure:** the existing best-effort behavior — the share
  fails cleanly, voice continues; the popover shows the error.
- **Self-preview decode error:** the stage's decoder self-heals (existing logic);
  worst case a brief blank, recovered on the next keyframe.
- **Join with no key yet / peer stops mid-watch:** existing teardown — a peer
  going un-live (`sharing:false`) removes them from `watching` and closes the stage.

## Testing

- **Headless (Linux):** `enumerate_sources` window branch via the mock; the
  `source_id` prefix routing; the self-frame emit gating (controller test that
  sharing emits `voice://self-video-frame` and stop ceases it); the stage's
  watch-gating + decoder lifecycle (tsc + reasoned, as in Phase E).
- **Windows probe:** the Window API validation (owner runs it).
- **Owner two-client runtime:** share a **window** (not just a screen) → see your
  **self-preview** + your own **LIVE**; the other client sees **LIVE + Join**,
  clicks **Join** → your screen appears **in their main area** with a working
  game-audio slider; ✕ leaves; Stop ends it. Repeat sharing a **screen** and over a
  **direct** server.

## Phasing

One spec, but the plan will sequence: **(0)** window-capture probe → **(1)** backend
window source + self-preview event → **(2)** share-setup popover + self LIVE
indicator → **(3)** main-area stage + Join button + retire sidebar tiles → **(4)**
docs + gate + owner runtime verification.
