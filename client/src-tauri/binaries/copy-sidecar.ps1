# Copy the farder-server binary for Tauri sidecar resolution (Windows).
# Run from the repo root in PowerShell after building the server:
#   cargo build -p farder-server
#   .\client\src-tauri\binaries\copy-sidecar.ps1
$ErrorActionPreference = "Stop"

# Host target triple, e.g. x86_64-pc-windows-msvc
$triple = ((rustc -vV) | Select-String '^host:').ToString().Split(' ')[1]
$sidecar = "farder-server-$triple.exe"
$src = "target\debug\farder-server.exe"

if (-not (Test-Path $src)) {
    throw "$src not found - run 'cargo build -p farder-server' first."
}

# Copy to binaries/ (for `tauri build`) and next to the dev exe (for `tauri dev`).
Copy-Item $src "client\src-tauri\binaries\$sidecar" -Force
New-Item -ItemType Directory -Force -Path "client\src-tauri\target\debug" | Out-Null
Copy-Item $src "client\src-tauri\target\debug\$sidecar" -Force

Write-Host "Copied sidecar binary for $triple"
