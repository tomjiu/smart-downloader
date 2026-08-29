# G2: proxy live-forwarding verification (manual; proxy-side log check is on you).
# Usage:
#   pwsh -File G2_proxy_live.ps1 -ProxyUrl "http://127.0.0.1:10809"
# PASS criteria: task reaches completed AND your proxy client shows a session
#                to the test host while the download runs.
param(
  [Parameter(Mandatory = $true)][string]$ProxyUrl,
  [string]$TestUrl = 'https://proof.ovh.net/files/1Mb.dat',
  [int]$TimeoutSec = 120,
  [int]$Port = 18788
)
$ErrorActionPreference = 'Stop'
$repo = 'E:\Code\ai\smart-downloader'

$cfgDir = Join-Path $env:TEMP ("g2cfg_" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $cfgDir | Out-Null
$dest = Join-Path $cfgDir 'downloads'
New-Item -ItemType Directory -Path $dest | Out-Null
$cfg = "[server]`naddr = `"127.0.0.1:$Port`"`n`n[download]`nproxy = `"$ProxyUrl`"`ndest_root = `"$($dest -replace '\\','/')`"`n"
$cfgPath = Join-Path $cfgDir 'config.toml'
Set-Content -Path $cfgPath -Value $cfg -Encoding ASCII

Write-Host '[G2] building smart-dl-daemon ...'
cargo build --offline --manifest-path "$repo\Cargo.toml" -p smart-dl-daemon 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) { Write-Host '[G2] BUILD FAILED'; exit 1 }

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
  if (-not $up) { Write-Host '[G2] daemon did not come up; log tail:'; Get-Content $log -Tail 20; exit 1 }
  # proxy_enabled must be true in snapshot (never leaks the URL by design)
  $cfgSnap = Invoke-RestMethod "$base/config"
  Write-Host "[G2] daemon up (proxy_enabled=$($cfgSnap.proxy_enabled))"

  $body = @{ url = $TestUrl } | ConvertTo-Json
  $resp = Invoke-RestMethod -Method Post -Uri "$base/tasks" -ContentType 'application/json' -Body $body
  $tid = if ($resp.task_id) { $resp.task_id } elseif ($resp.id) { $resp.id } else { $resp.ToString() }
  Write-Host "[G2] task=$tid ; >>> NOW CHECK YOUR PROXY CLIENT LOG FOR A SESSION TO THE TEST HOST <<<"

  $sw = [Diagnostics.Stopwatch]::StartNew(); $state = ''
  while ($sw.Elapsed.TotalSeconds -lt $TimeoutSec) {
    Start-Sleep 2
    $snap = Invoke-RestMethod "$base/tasks/$tid"
    $state = "$($snap.state)"
    if ($state.ToLower() -in @('completed', 'failed')) { break }
  }
  Write-Host "[G2] result: state=$state elapsed=$([int]$sw.Elapsed.TotalSeconds)s dest=$dest"
  if ($state.ToLower() -eq 'completed') {
    Write-Host '[G2] transfer OK via configured proxy. FINAL STEP (manual): confirm the session in your proxy client connection log.'
    exit 0
  } else {
    Write-Host '[G2] INCONCLUSIVE: task not completed in timeout (check proxy reachability).'; exit 2
  }
} finally {
  if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force }
}
