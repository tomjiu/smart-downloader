$ErrorActionPreference = 'Stop'
$base = 'http://127.0.0.1:18790'
$magnet = 'magnet:?xt=urn:btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c&dn=Big+Buck+Bunny&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce'
$CID='Xqp0kJBXWhwaTpB6'; $VER='1.92.91'; $HOSTN='pan.xunlei.com'; $UID='860599297'; $PKG='pan.xunlei.com'
$salts=@('tkPbM0TLWT+eMvAdV2FbXEEQ/Qx5QrfO895+47hmDDPdRZ98xm','7EBc6XKuI6YGw19anZHmnE4d8W18zjrJU+F','stEQvsO6eeP93DdrX7mfYA7G','edXgGCdIaqdZJZH5k','J9SB6D864S1B','xlAs2Oo28sr','21+f+kgyrbIcwUUo+xaPD4GYHkpRGv5i4wOnyHrkH4ehKti','08kltU1bp6eV5bEdlgSEU0GpzjD7/j5X3FwbiiraEzar','hX6tf7kBT/DS')
function MD5Hex([string]$s){$m=[System.Security.Cryptography.MD5]::Create();($m.ComputeHash([Text.Encoding]::UTF8.GetBytes($s))|ForEach-Object{$_.ToString('x2')})-join ''}
function CaptchaSign([string]$d,[string]$t){$s="$CID$VER$HOSTN$d$t";foreach($x in $salts){$s=MD5Hex ($s+$x)};"1.$s"}
$cred=Get-Content 'E:\Code\ai\smart-downloader\xunlei_auth_web.json' -Raw|ConvertFrom-Json
$did=-join((1..32)|ForEach-Object{'{0:x}' -f (Get-Random -Max 16)});$ts=[string]([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())
$h1=@{Authorization="Bearer $($cred.access_token)";'User-Agent'='Mozilla/5.0'}
$cap=Invoke-RestMethod -Uri 'https://xluser-ssl.xunlei.com/v1/shield/captcha/init' -Method Post -ContentType 'application/json' -Headers $h1 -Body (@{action='GET:/drive/v1/files';captcha_token='';client_id=$CID;device_id=$did;redirect_uri='xlaccsdk01://xunlei.com/callback?state=harbor';meta=@{client_version=$VER;package_name=$PKG;user_id=$UID;timestamp=$ts;captcha_sign=(CaptchaSign $did $ts)}}|ConvertTo-Json -Depth 6) -TimeoutSec 20
$h2=@{Authorization="Bearer $($cred.access_token)";'X-Captcha-Token'=$cap.captcha_token;'X-Client-Id'=$CID;'X-Device-Id'=$did;Origin='https://pan.xunlei.com';Referer='https://pan.xunlei.com/'}

# 清场：删旧任务与残留文件
try { Invoke-RestMethod -Method Delete -Uri "$base/tasks/t1" -TimeoutSec 10 | Out-Null } catch {}
Get-ChildItem 'E:\temp\f31\downloads' -Recurse -File -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

Write-Host '[add] 重新提交磁力...'
$r = Invoke-RestMethod -Method Post -Uri "$base/tasks" -ContentType 'application/json' -Body (@{url=$magnet}|ConvertTo-Json) -TimeoutSec 30
$tid = if ($r.task_id) { $r.task_id } else { $r.id }
Write-Host "[add] tid=$tid"

# 等 metadata（total>0）→ 抢停；执法循环随后冻结进度
foreach ($i in 1..400) {
  Start-Sleep -Milliseconds 250
  try { $s = Invoke-RestMethod "$base/tasks/$tid" -TimeoutSec 3 } catch { continue }
  if ([long]$s.total -gt 0) {
    $null = Invoke-RestMethod -Method Post -Uri "$base/tasks/$tid/pause" -TimeoutSec 5
    Write-Host "[pause] total=$($s.total)"
    break
  }
}
Start-Sleep 3
$chk = Invoke-RestMethod "$base/tasks/$tid"
Write-Host "[verify] 暂停后 done=$($chk.done)/$($chk.total)"
if (([long]$chk.done) -gt ([long]$chk.total * 0.5)) { Write-Host '进度仍超半——放弃本轮'; exit 2 }

Write-Host '[fallback] firing...'
$fj = Start-Job -ScriptBlock {
  param($t)
  try { Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:18790/tasks/$t/fallback" -TimeoutSec 1700 | ConvertTo-Json -Depth 6 }
  catch { 'FB-ERR: ' + $_.Exception.Message + ' | ' + $_.ErrorDetails.Message }
} -ArgumentList $tid

$finalState = ''
foreach ($i in 1..300) {
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

$f = Get-ChildItem 'E:\temp\f31\downloads' -Recurse -File | Sort-Object Length -Descending | Select-Object -First 1
if ($f) {
  $lmd5 = (Get-FileHash $f.FullName -Algorithm MD5).Hash
  Write-Host "[verify] $($f.Name) $([math]::Round($f.Length/1MB,2))MB localMD5=$lmd5"
} else { Write-Host '[verify] 无落盘大文件' }

# 云端 md5_checksum 对比（按名称匹配）
$list = Invoke-RestMethod -Uri 'https://api-pan.xunlei.com/drive/v1/files?parent_id=&limit=100&with_audit=true' -Headers @{Authorization="Bearer $($cred.access_token)";'X-Captcha-Token'=($cap.captcha_token);'X-Client-Id'=$CID;'X-Device-Id'=(-join((1..32)|ForEach-Object{'{0:x}' -f (Get-Random -Max 16)}));Origin='https://pan.xunlei.com'} -TimeoutSec 20
foreach ($fi in @($list.files)) {
  if ($fi.name -like '*Bunny*' -and $fi.md5_checksum) {
    Write-Host "[cloud ] md5_checksum = $($fi.md5_checksum)"
    if ("$($fi.md5_checksum)".ToUpper() -eq $lmd5.ToUpper()) { Write-Host "`n>>> F3.1 PASS: MD5 FULLY MATCHED <<<" } else { Write-Host "`n>>> MD5 MISMATCH <<<" }
    break
  }
}
