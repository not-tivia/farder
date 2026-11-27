# Opens a SECOND Farder with its own identity, beside the one you already have.
#
# Run it while `npm run tauri dev` is going (that one stays identity A). This
# starts the same program again but with its own data folder, so it has a
# separate identity, contacts and settings. Your real ~/.farder is never touched.
#
#   .\scripts\second-identity.ps1          # identity B  (folder ~\.farder-b)
#   .\scripts\second-identity.ps1 -Name c  # identity C  (folder ~\.farder-c)
#
# To start from nothing again, delete that folder while the window is closed.
param([string]$Name = "b")

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "client\src-tauri\target\debug\farder-client.exe"
if (-not (Test-Path $exe)) {
    Write-Host "Cannot find $exe"
    Write-Host "Start the normal copy once with 'npm run tauri dev' first (that builds it)."
    exit 1
}
$dir = Join-Path $env:USERPROFILE ".farder-$Name"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$env:FARDER_DATA = $dir
# Its own browser profile too, so nothing the page stores (localStorage, caches)
# is shared with identity A.
$env:WEBVIEW2_USER_DATA_FOLDER = Join-Path $dir "webview"
Write-Host "Starting identity '$Name' with data in $dir"
Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe)
