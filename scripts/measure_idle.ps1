<#
.SYNOPSIS
  Measure how much CPU and memory Farder uses while it just sits there.

.DESCRIPTION
  Samples the Farder client, every WebView2 process it started (the window's
  browser engine), and the farder-server sidecar if one is running. Reports the
  CPU used (as a share of ONE core, and of the whole machine), memory, thread and
  handle counts. Read-only: it only looks, it changes nothing.

  Run it with Farder open, signed in, and left ALONE (no typing, no clicking)
  for the duration. Try it twice: once with the window in front and once with it
  minimised -- the second is the number that matters for an app you leave open.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\measure_idle.ps1 -Seconds 120

.PARAMETER ProcessName
  The client's process name without .exe (default farder-client).
.PARAMETER Seconds
  How long to watch (default 120).
.PARAMETER Interval
  Seconds between samples (default 5).
#>
param(
  [string]$ProcessName = 'farder-client',
  [int]$Seconds = 120,
  [int]$Interval = 5
)

$ErrorActionPreference = 'Stop'
$cores = [Environment]::ProcessorCount

function Get-Tree([int[]]$rootIds) {
  # Every descendant of the given processes (WebView2 children, helpers).
  $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name
  $found = New-Object System.Collections.Generic.HashSet[int]
  $queue = New-Object System.Collections.Generic.Queue[int]
  foreach ($r in $rootIds) { [void]$found.Add($r); $queue.Enqueue($r) }
  while ($queue.Count -gt 0) {
    $p = $queue.Dequeue()
    foreach ($c in ($all | Where-Object { $_.ParentProcessId -eq $p })) {
      if ($found.Add([int]$c.ProcessId)) { $queue.Enqueue([int]$c.ProcessId) }
    }
  }
  return ,$found
}

$roots = @(Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
if ($roots.Count -eq 0) {
  Write-Output "Process '$ProcessName' is not running. Start Farder, sign in, leave it alone, then run this again."
  exit 2
}
$sidecars = @(Get-Process -Name 'farder-server' -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$ids = Get-Tree ($roots + $sidecars)

function Take-Sample {
  $rows = @()
  foreach ($id in $ids) {
    $p = Get-Process -Id $id -ErrorAction SilentlyContinue
    if ($null -eq $p) { continue }
    $rows += [pscustomobject]@{
      Id = $p.Id; Name = $p.ProcessName
      CpuSec = $p.TotalProcessorTime.TotalSeconds
      WorkingMB = $p.WorkingSet64 / 1MB
      PrivateMB = $p.PrivateMemorySize64 / 1MB
      Threads = $p.Threads.Count; Handles = $p.HandleCount
    }
  }
  return $rows
}

Write-Output ("Watching {0} process(es) for {1}s, every {2}s. Leave Farder alone." -f $ids.Count, $Seconds, $Interval)
$first = Take-Sample
$t0 = Get-Date
$prev = $first; $prevT = $t0
$cpuPct = @(); $mem = @()
$steps = [math]::Floor($Seconds / $Interval)
for ($i = 0; $i -lt $steps; $i++) {
  Start-Sleep -Seconds $Interval
  $now = Take-Sample; $nowT = Get-Date
  $dt = ($nowT - $prevT).TotalSeconds
  $prevMap = @{}; foreach ($r in $prev) { $prevMap[$r.Id] = $r.CpuSec }
  $delta = 0.0
  foreach ($r in $now) { if ($prevMap.ContainsKey($r.Id)) { $delta += ($r.CpuSec - $prevMap[$r.Id]) } }
  $cpuPct += ($delta / $dt) * 100.0
  $mem += ($now | Measure-Object -Property PrivateMB -Sum).Sum
  $prev = $now; $prevT = $nowT
}
$last = $prev

$total = ((Get-Date) - $t0).TotalSeconds
$cpuAvg = ($cpuPct | Measure-Object -Average).Average
$cpuMax = ($cpuPct | Measure-Object -Maximum).Maximum
$busy = @($cpuPct | Where-Object { $_ -ge 1.0 }).Count
Write-Output ""
Write-Output "Per process (end of run):"
$last | Sort-Object PrivateMB -Descending |
  Format-Table Id, Name, @{n='Private MB';e={[math]::Round($_.PrivateMB,1)}}, @{n='Working MB';e={[math]::Round($_.WorkingMB,1)}}, Threads, Handles -AutoSize |
  Out-String -Width 160
Write-Output "Whole Farder (all processes together):"
Write-Output ("  Seconds observed        : {0:N0}" -f $total)
Write-Output ("  CPU, average            : {0:N2}% of one core  ({1:N3}% of this {2}-core machine)" -f $cpuAvg, ($cpuAvg / $cores), $cores)
Write-Output ("  CPU, worst {0}s sample    : {1:N2}% of one core" -f $Interval, $cpuMax)
Write-Output ("  Samples at/above 1% CPU : {0} of {1}  (an idle app should have almost none)" -f $busy, $cpuPct.Count)
Write-Output ("  Private memory          : {0:N0} MB at start, {1:N0} MB at end" -f $mem[0], $mem[$mem.Count - 1])
Write-Output ("  Memory drift            : {0:+0.0;-0.0} MB over the run (a steady rise means a leak)" -f ($mem[$mem.Count - 1] - $mem[0]))
