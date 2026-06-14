# Phase D probe — WASAPI loopback (run on Windows)

Throwaway. Confirms we can capture the system's **own output audio** (what a game
plays) on your Windows box before I write the Phase D (game/screen audio) plan.
Farder's mic capture uses `cpal`, which can't do loopback — this validates the
`wasapi` crate path instead. Delete this folder once Phase D ships.

## Run it

In the Farder repo on Windows:

```powershell
git pull
cd phaseD-probe
cargo run --release
```

**Start playing some audio** (music, a YouTube video, a game) while it runs — it
captures ~3 seconds and needs sound playing to show a non-zero peak.

## What I need back

Paste the whole output. The important lines:

- `Native mix format: ... Hz, ... ch, ... bits/sample` — your device's real format
- `Frames captured: ...` and `Peak amplitude: ...` — should say **NON-SILENT** if
  audio was playing
- `PROBE OK: ...` — confirms it builds + runs

If it fails to **compile**, paste the error — that means a `wasapi` API name
needs adjusting and I'll fix it before we proceed.
