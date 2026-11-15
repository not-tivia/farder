# Builds a SELF-CONTAINED Windows copy of Farder (frontend baked in, so it needs
# no dev server) from a git checkout, for the automatic two-client self-test.
# It never touches your normal `tauri dev` setup: run it inside a separate
# checkout (a `git worktree`), which has its own build folder.
#
#   .\scripts\selftest\build-windows.ps1            # builds in the repo it lives in
#
# Output: client\src-tauri\target\debug\farder-client.exe (+ the server beside it).
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Set-Location $root
# Some native dependencies (cmake based) need this on newer CMake.
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"

# A test window left running from an earlier run locks the app file and the build
# cannot replace it: close the ones that belong to THIS folder (and only those).
Get-Process farder-client, farder-server -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path.StartsWith($root, [StringComparison]::OrdinalIgnoreCase) } |
    ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
Start-Sleep -Seconds 2

Write-Host "== 1/4 server (sidecar) =="
cargo build -p farder-server
if ($LASTEXITCODE -ne 0) { throw "server build failed" }
$triple = ((rustc -vV) | Select-String '^host:').ToString().Split(' ')[1]
Copy-Item "target\debug\farder-server.exe" "client\src-tauri\binaries\farder-server-$triple.exe" -Force

Write-Host "== 2/4 frontend packages =="
Set-Location (Join-Path $root "client")
if (-not (Test-Path node_modules)) { npm ci; if ($LASTEXITCODE -ne 0) { throw "npm ci failed" } }

Write-Host "== 3/4 app (frontend baked in) =="
npx tauri build --debug --no-bundle
if ($LASTEXITCODE -ne 0) { throw "tauri build failed" }

Write-Host "== 4/4 server beside the app =="
Copy-Item "$root\target\debug\farder-server.exe" "$root\client\src-tauri\target\debug\farder-server-$triple.exe" -Force
Copy-Item "$root\target\debug\farder-server.exe" "$root\client\src-tauri\target\debug\farder-server.exe" -Force
Write-Host "BUILD OK: $root\client\src-tauri\target\debug\farder-client.exe"
