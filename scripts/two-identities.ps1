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

# Wait for A: its dev server must be listening AND a Farder started after we began.
# (A Farder left hidden in the tray from earlier must not count as "A is up".)
$began = Get-Date
$deadline = $began.AddMinutes(15)
while ($true) {
    $up = Get-NetTCPConnection -LocalPort 1420 -State Listen -ErrorAction SilentlyContinue
    $fresh = Get-Process farder-client -ErrorAction SilentlyContinue | Where-Object { $_.StartTime -gt $began }
    if ($up -and $fresh) { break }
    if ((Get-Date) -gt $deadline) { Write-Host "A never started; check its window for errors."; exit 1 }
    Start-Sleep -Seconds 2
}
Start-Sleep -Seconds 6

Write-Host "Starting identity B..."
& (Join-Path $PSScriptRoot "second-identity.ps1")
