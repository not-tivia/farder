# NVENC probe (run on Windows, after your restart)

Throwaway. Confirms NVIDIA hardware H.264 encoding (NVENC) works on your box
before I wire it into the screen share. Needs only your installed NVIDIA driver
(no CUDA toolkit / SDK install). Delete this folder once NVENC ships.

## Run it

In the Farder repo on Windows (on the `screenshare-ux` branch):

```powershell
git pull
cd nvenc-probe
cargo run --release
```

The first build pulls the NVENC/cudarc crates — it may take a minute.

## What I need back

Paste the whole output. The key lines:

- `CUDA context created on GPU 0.`
- `NVENC encoder initialised.`
- `codec[i] input formats (...): [...]` — tells me whether NVENC wants NV12 or
  can take RGBA/ARGB directly (decides whether we need a color-convert step).
- `PROBE OK: ...`

If it fails to **compile**, paste the error — most likely a `cudarc` feature
flag needs adjusting for your CUDA/driver version (e.g. the `cuda-12020` line in
`Cargo.toml`), which I'll fix. If it builds but **CUDA/NVENC init fails**, paste
that error too — it tells us about the driver/GPU.
