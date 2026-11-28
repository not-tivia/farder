# Ends the windows (and their servers) that start-pair.ps1 started, and nothing else.
param([string]$DataRoot = (Join-Path $env:USERPROFILE "farder-selftest-data"))
$f = Join-Path $DataRoot "pids.txt"
$ids = if (Test-Path $f) { Get-Content $f } else { Write-Host "no app pids recorded"; @() }
foreach ($id in $ids) {
    # the app first, then anything it started (its server children)
    Get-CimInstance Win32_Process -Filter "ParentProcessId=$id" -ErrorAction SilentlyContinue | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
}
# Servers the app started from the self-test folders (they can outlive the app).
Get-CimInstance Win32_Process -Filter "Name='farder-server.exe'" -ErrorAction SilentlyContinue |
    Where-Object { $_.CommandLine -like "*$DataRoot*" } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
# Their web views (they hold files in the data folder open, so a -Fresh start could not clear it).
Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" -ErrorAction SilentlyContinue |
    Where-Object { $_.CommandLine -like "*$DataRoot*" } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
Remove-Item $f -ErrorAction SilentlyContinue
Write-Host "stopped"
