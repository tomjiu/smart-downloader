$ErrorActionPreference = 'Stop'
$base = 'http://127.0.0.1:18790'
$magnet = 'magnet:?xt=urn:btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c&dn=Big+Buck+Bunny&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce'

# 冷启动清场：残留完成档/fastresume 会让引擎秒变 100%（门禁正确拒绝）
Get-ChildItem 'E:\temp\f31\downloads' -Recurse -File -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
Write-Host '[clean] dest 已清空'

Write-Host '[add] 提交磁力（容忍去重收编）...'
$tid = $null
foreach ($try in 1..3) {
  try {
    $r = Invoke-RestMethod -Method Post -Uri "$base/tasks" -ContentType 'application/json' -Body (@{url=$magnet}|ConvertTo-Json) -TimeoutSec 30
    $tid = if ($r.task_id) { $r.task_id } else { $r.id }
    Write-Host "[add] tid=$tid"; break
  } catch {
    $msg = "$($_.ErrorDetails.Message)"
    if ($msg -match 'existing:\s*([^\s"]+)') { $tid = $Matches[1]; Write-Host "[add] 去重收编已有任务 $tid"; break }
    Write-Host "add err(第${try}次): $msg"; Start-Sleep 3
  }
}
if (-not $tid) { Write-Host '无法获得任务'; exit 1 }

# 等 metadata（最长 6 分钟；期间若本地自行完成则判失败退出）
$metaOk = $false
foreach ($i in 1..1440) {
  Start-Sleep -Milliseconds 250
  try { $s = Invoke-RestMethod "$base/tasks/$tid" -TimeoutSec 3 } catch { continue }
  if ("$($s.state)".ToLower() -in @('seeding','completed')) { Write-Host "本地已自行完成(state=$($s.state))——无法演练暂停门禁"; exit 3 }
  if ([long]$s.total -gt 0) { $metaOk = $true; Write-Host "[meta] total=$($s.total) @$(([int]($i*0.25)))s"; break }
}
if (-not $metaOk) { Write-Host '6 分钟未获 metadata'; exit 2 }

$null = Invoke-RestMethod -Method Post -Uri "$base/tasks/$tid/pause" -TimeoutSec 8
Start-Sleep 2
$snap = Invoke-RestMethod "$base/tasks/$tid"
Write-Host "[pause] 快照 done=$($snap.done)/$($snap.total)"
if (([long]$snap.done) -gt ([long]$snap.total * 0.5)) { Write-Host '进度已过半——放弃'; exit 2 }

Write-Host '[fallback] firing...'
$fj = Start-Job -ScriptBlock {
  param($t)
  try { Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:18790/tasks/$t/fallback" -TimeoutSec 1800 | ConvertTo-Json -Depth 6 }
  catch { 'FB-ERR: ' + $_.Exception.Message + ' | ' + $_.ErrorDetails.Message }
} -ArgumentList $tid

$finalState = ''
foreach ($i in 1..360) {
  Start-Sleep 5
  $snap = $null; $cfgOk = $true
  try { $snap = Invoke-RestMethod "$base/tasks/$tid" -TimeoutSec 4 } catch {}
  try { Invoke-RestMethod "$base/config" -TimeoutSec 4 | Out-Null } catch { $cfgOk = $false }
  $st = if ($snap) { $snap.state } else { '?' }
  $dn = if ($snap) { $snap.done } else { '?' }
  Write-Host ("[{0:d3}s] state={1} done={2} cfg={3}" -f ($i*5), $st, $dn, $cfgOk)
  if (-not $cfgOk) { Write-Host '!!! WEDGE !!!' }
  if ("$st".ToLower() -in @('completed','failed')) { $finalState = $st; break }
}

Wait-Job $fj -Timeout 1800 | Out-Null
$fb = Receive-Job $fj
Remove-Job $fj -Force -ErrorAction SilentlyContinue
Write-Host "[fallback job] $fb"
if ("$fb".StartsWith('FB-ERR')) { exit 1 }

$f = Get-ChildItem 'E:\temp\f31\downloads' -Recurse -File | Sort-Object Length -Descending | Select-Object -First 1
if ($f) {
  $lmd5 = (Get-FileHash $f.FullName -Algorithm MD5).Hash
  Write-Host "[verify] $($f.Name) $([math]::Round($f.Length/1MB,2))MB localMD5=$lmd5"
} else { Write-Host '[verify] 无落盘大文件'; exit 1 }

$cred=Get-Content 'E:\Code\ai\smart-downloader\xunlei_auth_web.json' -Raw|ConvertFrom-Json
$CID='Xqp0kJBXWhwaTpB6'; $did=-join((1..32)|ForEach-Object{'{0:x}' -f (Get-Random -Max 16)})
$cap=Invoke-RestMethod -Uri 'https://xluser-ssl.xunlei.com/v1/shield/captcha/init' -Method Post -ContentType 'application/json' -Headers @{Authorization="Bearer $($cred.access_token)"} -Body (@{action='GET:/drive/v1/files';captcha_token='';client_id=$CID;device_id=$did;redirect_uri='https://pan.xunlei.com';meta=@{client_version='1.92.91';package_name='pan.xunlei.com';user_id=$cred.user_id;timestamp=[string]([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds());captcha_sign='x'}}|ConvertTo-Json -Depth 6) -TimeoutSec 20
$list=Invoke-RestMethod -Uri 'https://api-pan.xunlei.com/drive/v1/files?parent_id=&limit=100&with_audit=true' -Headers @{Authorization="Bearer $($cred.access_token)";'X-Captcha-Token'=$cap.captcha_token;'X-Client-Id'=$CID;'X-Device-Id'=$did;Origin='https://pan.xunlei.com'} -TimeoutSec 20
foreach ($fi in @($list.files)) {
  if ($fi.name -like '*Bunny*' -and $fi.md5_checksum) {
    Write-Host "[cloud ] md5_checksum = $($fi.md5_checksum)"
    if ("$($fi.md5_checksum)".ToUpper() -eq $lmd5.ToUpper()) { Write-Host "`n>>> F3.1 PASS: MD5 FULLY MATCHED <<<" } else { Write-Host "`n>>> MD5 MISMATCH <<<" }
    break
  }
}
