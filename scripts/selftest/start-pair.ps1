# Starts TWO Farder windows (identity A and identity B) from the self-contained
# build, each with its OWN throwaway data folder and browser profile, and with a
# debugging port open so the self-test driver can click and read them.
#
#   .\scripts\selftest\start-pair.ps1 [-Exe <path>] [-DataRoot <folder>] [-Fresh]
#
# It only ever touches the folders under -DataRoot (default: ~\farder-selftest-data),
# never your real ~\.farder. It writes the process ids it started to
# <DataRoot>\pids.txt so stop-pair.ps1 can end exactly those and nothing else.
param(
    [string]$Exe = (Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) "client\src-tauri\target\debug\farder-client.exe"),
    [string]$DataRoot = (Join-Path $env:USERPROFILE "farder-selftest-data"),
    [int]$PortA = 9222,
    [int]$PortB = 9223,
    [switch]$Fresh
)
$ErrorActionPreference = "Stop"
if (-not (Test-Path $Exe)) { throw "Cannot find $Exe - run build-windows.ps1 first." }
if ($Fresh -and (Test-Path $DataRoot)) { Remove-Item -Recurse -Force $DataRoot }
New-Item -ItemType Directory -Force -Path $DataRoot | Out-Null
$pids = @()
foreach ($who in @(@("a", $PortA), @("b", $PortB))) {
    $name = $who[0]; $port = $who[1]
    $dir = Join-Path $DataRoot $name
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $env:FARDER_DATA = $dir
    $env:WEBVIEW2_USER_DATA_FOLDER = Join-Path $dir "webview"
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$port"
    $p = Start-Process -FilePath $Exe -WorkingDirectory (Split-Path $Exe) -PassThru
    $pids += $p.Id
    Write-Host "identity $name : pid $($p.Id), data $dir, debug port $port"
}
$pids | Set-Content (Join-Path $DataRoot "pids.txt")
