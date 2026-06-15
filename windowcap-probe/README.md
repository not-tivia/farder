# Window-capture probe (run on Windows)

Throwaway. Confirms the windows-capture 2.0.0 `Window` API works on your box
before the screenshare-UX feature builds against it (window capture is new
Windows-only code that can't be compiled/run on the Linux side). Delete this
folder once the feature ships.

## Run it

In the Farder repo on Windows:

```powershell
git pull
cd windowcap-probe
cargo run --release
```

## What I need back

Paste the whole output. The important lines:

- `Found N window(s):` followed by the list (title / size / process)
- `OK: Settings::new(Window=..., ...) type-checks.`
- `PROBE OK: ...`

If it fails to **compile**, paste the error — that means a `windows-capture`
`Window` API name (e.g. `enumerate` / `title` / `Settings::new`) needs adjusting,
and I'll fix it before building the real feature.
