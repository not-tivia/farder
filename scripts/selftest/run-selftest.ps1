# One command for the whole automatic self-test (after build-windows.ps1 has run):
#   .\scripts\selftest\run-selftest.ps1
# Starts two fresh throwaway clients on the second monitor, runs the first-run chat
# scenario, quits and relaunches them with the same data, runs the relaunch scenario,
# and stops them. Results and screenshots land in <DataRoot>\results and \shots.
param([string]$DataRoot = (Join-Path $env:USERPROFILE "farder-selftest-data"))
$here = $PSScriptRoot
$root = Split-Path -Parent (Split-Path -Parent $here)
$env:SHOTS = Join-Path $DataRoot "shots"
$env:STATE = Join-Path $DataRoot "state.json"
Set-Location $root
$bad = 0
& "$here\stop-pair.ps1" -DataRoot $DataRoot | Out-Null
& "$here\start-pair.ps1" -DataRoot $DataRoot -Fresh | Out-Null
$env:RESULTS = Join-Path $DataRoot "results-core.json"
node "$here\driver.mjs" core; if ($LASTEXITCODE -ne 0) { $bad++ }
$env:RESULTS = Join-Path $DataRoot "results-dm.json"
node "$here\driver.mjs" dm; if ($LASTEXITCODE -ne 0) { $bad++ }
$env:RESULTS = Join-Path $DataRoot "results-buddy.json"
node "$here\driver.mjs" buddy; if ($LASTEXITCODE -ne 0) { $bad++ }
$env:RESULTS = Join-Path $DataRoot "results-edit.json"
node "$here\driver.mjs" edit; if ($LASTEXITCODE -ne 0) { $bad++ }
$env:RESULTS = Join-Path $DataRoot "results-direct.json"
node "$here\driver.mjs" direct; if ($LASTEXITCODE -ne 0) { $bad++ }
foreach ($extra in @("unread", "popup", "attach", "garble", "admin-garble", "powers", "voice", "phone", "popup-size", "strip", "sound", "everyday", "themes")) {
    $env:RESULTS = Join-Path $DataRoot "results-$extra.json"
    node "$here\driver.mjs" $extra; if ($LASTEXITCODE -ne 0) { $bad++ }
}
& "$here\stop-pair.ps1" -DataRoot $DataRoot | Out-Null
& "$here\start-pair.ps1" -DataRoot $DataRoot | Out-Null
$env:RESULTS = Join-Path $DataRoot "results-relaunch.json"
node "$here\driver.mjs" relaunch; if ($LASTEXITCODE -ne 0) { $bad++ }
$env:RESULTS = Join-Path $DataRoot "results-direct-relaunch.json"
node "$here\driver.mjs" direct-relaunch; if ($LASTEXITCODE -ne 0) { $bad++ }
# Last, because it hides B's window to the tray and nothing here clicks the tray back.
$env:DATAROOT = $DataRoot
$env:RESULTS = Join-Path $DataRoot "results-tray.json"
node "$here\driver.mjs" tray; if ($LASTEXITCODE -ne 0) { $bad++ }
& "$here\stop-pair.ps1" -DataRoot $DataRoot | Out-Null
if ($bad) { Write-Host "SELF-TEST FAILED ($bad scenario(s))"; exit 1 } else { Write-Host "SELF-TEST PASSED" }
