# Moves the two test windows (the ones start-pair.ps1 recorded) side by side onto
# one monitor. Safe to run again any time, e.g. if a window was dragged away.
#   .\scripts\selftest\place-pair.ps1 [-Monitor second|primary]
param(
    [string]$DataRoot = (Join-Path $env:USERPROFILE "farder-selftest-data"),
    [ValidateSet("second","primary")][string]$Monitor = "second"
)
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System; using System.Text; using System.Collections.Generic; using System.Runtime.InteropServices;
public static class FarderWin {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int hh, uint flags);
  // Visible, titled top-level windows of one process.
  public static List<IntPtr> Of(uint pid) {
    var l = new List<IntPtr>();
    EnumWindows((h, _) => { uint p; GetWindowThreadProcessId(h, out p); if (p == pid && IsWindowVisible(h) && GetWindowTextLength(h) > 0) l.Add(h); return true; }, IntPtr.Zero);
    return l;
  }
}
"@
[FarderWin]::SetProcessDPIAware() | Out-Null
$f = Join-Path $DataRoot "pids.txt"
if (-not (Test-Path $f)) { throw "no pids.txt: start-pair.ps1 has not run" }
$pids = @(Get-Content $f)
$screens = [System.Windows.Forms.Screen]::AllScreens
$target = if ($Monitor -eq "primary") { $screens | Where-Object Primary | Select-Object -First 1 }
          else { $screens | Where-Object { -not $_.Primary } | Select-Object -First 1 }
if (-not $target) { $target = $screens[0]; Write-Host "only one monitor found; using it" }
$b = $target.WorkingArea
$half = [int]($b.Width / 2)
for ($i = 0; $i -lt $pids.Count; $i++) {
    $h = $null
    for ($t = 0; $t -lt 120 -and -not $h; $t++) {
        $h = [FarderWin]::Of([uint32]$pids[$i]) | Select-Object -First 1
        if (-not $h) { Start-Sleep -Milliseconds 500 }
    }
    if (-not $h) { Write-Host "no visible window for pid $($pids[$i])"; continue }
    [FarderWin]::ShowWindow($h, 9) | Out-Null   # restore if maximised/minimised
    # SWP_NOZORDER 0x4 | SWP_SHOWWINDOW 0x40
    [FarderWin]::SetWindowPos($h, [IntPtr]::Zero, $b.X + $i * $half, $b.Y, $half, $b.Height, 0x44) | Out-Null
}
Write-Host "windows placed on monitor $($target.DeviceName) at x=$($b.X)"
