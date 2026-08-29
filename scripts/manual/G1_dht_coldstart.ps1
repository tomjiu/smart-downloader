# G1: BT DHT real cold-start verification (manual, needs real internet).
# Usage:
#   pwsh -File G1_dht_coldstart.ps1 -Magnet "magnet:?xt=urn:btih:<HASH>"            # DHT on
#   pwsh -File G1_dht_coldstart.ps1 -Magnet "<same magnet>" -ControlRun             # all-off control
# PASS criteria:  DHT-on run finds num_peers>0;  control run stays 0 peers.
param(
  [Parameter(Mandatory = $true)][string]$Magnet,
  [int]$TimeoutSec = 180,
  [int]$Port = 18787,
  [switch]$ControlRun
)
$ErrorActionPreference = 'Stop'
$repo = 'E:\Code\ai\smart-downloader'
$env:Path = "$repo\ffi\vcpkg_installed\x64-windows\bin;" + $env:Path

$cfgDir = Join-Path $env:TEMP ("g1cfg_" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $cfgDir | Out-Null
$dht = if ($ControlRun) { 'false' } else { 'true' }
$cfg = "[server]`naddr = `"127.0.0.1:$Port`"`n`n[bt]`nenable_dht = $dht`nenable_lsd = false`nenable_upnp = false`n"
$cfgPath = Join-Path $cfgDir 'config.toml'
Set-Content -Path $cfgPath -Value $cfg -Encoding ASCII

Write-Host "[G1] building smart-dl-daemon (--features bt) ..."
cargo build --offline --manifest-path "$repo\Cargo.toml" -p smart-dl-daemon --features bt 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) { Write-Host '[G1] BUILD FAILED'; exit 1 }

$exe = Join-Path $repo 'target\debug\smart-dl-daemon.exe'
$log = Join-Path $cfgDir 'daemon.log'
$p = Start-Process -FilePath $exe -ArgumentList 'serve', '--config', $cfgPath `
    -RedirectStandardOutput $log -RedirectStandardError $log -PassThru -WindowStyle Hidden
try {
  $base = "http://127.0.0.1:$Port"
  $up = $false
  foreach ($i in 1..30) {
    try { Invoke-RestMethod "$base/config" -TimeoutSec 2 | Out-Null; $up = $true; break } catch { Start-Sleep 1 }
  }
  if (-not $up) { Write-Host '[G1] daemon did not come up; log tail:'; Get-Content $log -Tail 20; exit 1 }
  Write-Host "[G1] daemon up (enable_dht=$dht)"

  $body = @{ url = $Magnet } | ConvertTo-Json
  $resp = Invoke-RestMethod -Method Post -Uri "$base/tasks" -ContentType 'application/json' -Body $body
  $tid = if ($resp.task_id) { $resp.task_id } elseif ($resp.id) { $resp.id } else { $resp.ToString() }
  Write-Host "[G1] task=$tid ; polling up to ${TimeoutSec}s ..."

  $sw = [Diagnostics.Stopwatch]::StartNew(); $peers = -1; $state = ''
  while ($sw.Elapsed.TotalSeconds -lt $TimeoutSec) {
    Start-Sleep 3
    $snap = Invoke-RestMethod "$base/tasks/$tid"
    $state = "$($snap.state)"
    foreach ($pr in @($snap.PSObject.Properties | Where-Object { $_.Name -like '*num_peers*' })) {
      if ([int]$pr.Value -gt $peers) { $peers = [int]$pr.Value }
    }
    if ($state.ToLower() -in @('completed', 'failed')) { break }
  }
  Write-Host "[G1] result: state=$state max_num_peers=$peers elapsed=$([int]$sw.Elapsed.TotalSeconds)s"
  if ($ControlRun) {
    if ($peers -le 0) { Write-Host '[G1] PASS (control: no discovery without DHT)'; exit 0 }
    else { Write-Host '[G1] FAIL (control discovered peers without DHT!)'; exit 1 }
  } else {
    if ($peers -gt 0) { Write-Host '[G1] PASS (DHT cold start discovered peers)'; exit 0 }
    else { Write-Host '[G1] INCONCLUSIVE: 0 peers in timeout (check network/NAT/bootstrap connectivity)'; exit 2 }
  }
} finally {
  if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force }
}
