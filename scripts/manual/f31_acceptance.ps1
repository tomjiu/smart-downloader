$ErrorActionPreference = 'Stop'
$base = 'http://127.0.0.1:18790'
$candidates = @(
  'https://dl-cdn.alpinelinux.org/alpine/v3.21/releases/x86_64/alpine-virt-3.21.2-x86_64.iso.torrent',
  'https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/x86_64/alpine-virt-3.20.3-x86_64.iso.torrent',
  'https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/x86_64/alpine-virt-3.19.1-x86_64.iso.torrent'
)
$torrentBytes = $null; $usedUrl = ''
foreach ($u in $candidates) {
  try {
    Write-Host "[fetch] $u"
    $r = Invoke-WebRequest -Uri $u -TimeoutSec 25 -UseBasicParsing
    if ($r.Content.Length -gt 500) { $torrentBytes = $r.Content; $usedUrl = $u; break }
  } catch { Write-Host "  skip: $($_.Exception.Message)" }
}
if (-not $torrentBytes) { Write-Host 'ALL TORRENT SOURCES FAILED'; exit 1 }
Write-Host "[fetch] got $($torrentBytes.Length) B from $usedUrl"

$b64 = [Convert]::ToBase64String($torrentBytes)
$r = Invoke-RestMethod -Method Post -Uri "$base/tasks" -ContentType 'application/json' `
     -Body (@{ torrent_b64 = $b64 } | ConvertTo-Json) -TimeoutSec 30
$tid = if ($r.task_id) { $r.task_id } elseif ($r.id) { $r.id } else { ($r | ConvertTo-Json -Compress) }
Write-Host "[task] tid=$tid"

# 500ms 轮询：metadata 一到（total>0）立刻抢停
$paused = $false
foreach ($i in 1..240) {
  Start-Sleep -Milliseconds 500
  try { $s = Invoke-RestMethod "$base/tasks/$tid" -TimeoutSec 3 } catch { continue }
  if ([long]$s.total -gt 0) {
    $null = Invoke-RestMethod -Method Post -Uri "$base/tasks/$tid/pause" -TimeoutSec 5
    Start-Sleep -Milliseconds 300
    $logs = Invoke-RestMethod "$base/tasks/$tid/logs" -TimeoutSec 5
    $recState = $logs.state
    Write-Host "[pause] total=$($s.total) 快照=$($s.state) 记录态=$recState"
    $paused = ($recState -eq 'Paused')
    break
  }
}
if (-not $paused) {
  # 兜底判定：记录态可能已是 Seeding（抢停失败）
  $logs = Invoke-RestMethod "$base/tasks/$tid/logs" -TimeoutSec 5
  Write-Host "[pause] FAILED, 记录态=$($logs.state)"
  exit 2
}

# 触发兜底（后台长跑）
Write-Host '[fallback] firing...'
$fj = Start-Job -ScriptBlock {
  param($tid)
  try {
    Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:18790/tasks/$tid/fallback" -TimeoutSec 1500 | ConvertTo-Json -Depth 5
  } catch { 'FB-ERR: ' + $_.Exception.Message + ' | ' + $_.ErrorDetails.Message }
} -ArgumentList $tid

# 监控循环：状态 + 楔死探测（交替打 /config）
$wedged = $false
foreach ($i in 1..240) {
  Start-Sleep 5
  $snap = $null; $cfgOk = $true
  try { $snap = Invoke-RestMethod "$base/tasks/$tid" -TimeoutSec 4 } catch { }
  try { Invoke-RestMethod "$base/config" -TimeoutSec 4 | Out-Null } catch { $cfgOk = $false }
  $state = if ($snap) { $snap.state } else { '?' }
  $done = if ($snap) { $snap.done } else { -1 }
  $total = if ($snap) { $snap.total } else { -1 }
  Write-Host ("[{0:d3}s] state={1} done={2} total={3} cfgAlive={4}" -f ($i*5), $state, $done, $total, $cfgOk)
  if (-not $cfgOk -and -not $wedged) { $wedged = $true; Write-Host '!!! WEDGE DETECTED !!!' }
  if ("$state".ToLower() -in @('completed','failed')) { break }
}
Write-Host '[result] receiving fallback job...'
Wait-Job $fj -Timeout 1500 | Out-Null
Receive-Job $fj
Remove-Job $fj -Force -ErrorAction SilentlyContinue

# 最终落盘校验
Get-ChildItem 'E:\temp\f31\downloads' -Recurse -File | Select-Object FullName, Length
