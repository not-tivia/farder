# One command: starts Farder as identity A (normal, `tauri dev`) and, once that
# window is up, starts identity B beside it. Run from anywhere:
#
#   .\scripts\two-identities.ps1
#
# A is your normal ~\.farder. B uses ~\.farder-b (see second-identity.ps1).
# Close both windows (and quit from the tray) when you are done.
$root = Split-Path -Parent $PSScriptRoot
$client = Join-Path $root "client"

Write-Host "Starting identity A (this can take a while on the first build)..."
Start-Process powershell -WorkingDirectory $client -ArgumentList "-NoExit", "-Command", "npm run tauri dev"

# Wait for A's program to be running, then give it a moment to open its window.
$deadline = (Get-Date).AddMinutes(15)
while (-not (Get-Process farder-client -ErrorAction SilentlyContinue)) {
    if ((Get-Date) -gt $deadline) { Write-Host "A never started; check its window for errors."; exit 1 }
    Start-Sleep -Seconds 2
}
Start-Sleep -Seconds 8

Write-Host "Starting identity B..."
& (Join-Path $PSScriptRoot "second-identity.ps1")
