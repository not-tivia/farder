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
    [switch]$Fresh,
    # Which monitor to put the two windows on: "second" (the one that is not the
    # main one, the default), "primary", or "leave" to not move them.
    [ValidateSet("second","primary","leave")][string]$Monitor = "second"
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

# Put the two windows side by side on the chosen monitor, so the test never covers
# the screen you are working on.
if ($Monitor -ne "leave") {
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type @"
using System; using System.Runtime.InteropServices;
public static class FarderWin {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int hh, uint flags);
}
"@
    [FarderWin]::SetProcessDPIAware() | Out-Null
    $screens = [System.Windows.Forms.Screen]::AllScreens
    $target = if ($Monitor -eq "primary") { $screens | Where-Object Primary | Select-Object -First 1 }
              else { $screens | Where-Object { -not $_.Primary } | Select-Object -First 1 }
    if (-not $target) { $target = $screens[0]; Write-Host "only one monitor found; using it" }
    $b = $target.WorkingArea
    $half = [int]($b.Width / 2)
    for ($i = 0; $i -lt $pids.Count; $i++) {
        $proc = Get-Process -Id $pids[$i]
        for ($t = 0; $t -lt 120 -and $proc.MainWindowHandle -eq 0; $t++) { Start-Sleep -Milliseconds 500; $proc.Refresh() }
        if ($proc.MainWindowHandle -eq 0) { Write-Host "no window yet for pid $($pids[$i]); not moved"; continue }
        # SWP_NOZORDER 0x4 | SWP_SHOWWINDOW 0x40
        [FarderWin]::SetWindowPos($proc.MainWindowHandle, [IntPtr]::Zero, $b.X + $i * $half, $b.Y, $half, $b.Height, 0x44) | Out-Null
    }
    Write-Host "windows placed on monitor $($target.DeviceName)"
}
