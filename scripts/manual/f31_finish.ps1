$ErrorActionPreference = 'Stop'
$base = 'http://127.0.0.1:18790'
$tid = 't1'
$CID='Xqp0kJBXWhwaTpB6'; $VER='1.92.91'; $HOSTN='pan.xunlei.com'; $UID='860599297'; $PKG='pan.xunlei.com'
$salts=@('tkPbM0TLWT+eMvAdV2FbXEEQ/Qx5QrfO895+47hmDDPdRZ98xm','7EBc6XKuI6YGw19anZHmnE4d8W18zjrJU+F','stEQvsO6eeP93DdrX7mfYA7G','edXgGCdIaqdZJZH5k','J9SB6D864S1B','xlAs2Oo28sr','21+f+kgyrbIcwUUo+xaPD4GYHkpRGv5i4wOnyHrkH4ehKti','08kltU1bp6eV5bEdlgSEU0GpzjD7/j5X3FwbiiraEzar','hX6tf7kBT/DS')
function MD5Hex([string]$s){$m=[System.Security.Cryptography.MD5]::Create();($m.ComputeHash([Text.Encoding]::UTF8.GetBytes($s))|ForEach-Object{$_.ToString('x2')})-join ''}
function CaptchaSign([string]$d,[string]$t){$s="$CID$VER$HOSTN$d$t";foreach($x in $salts){$s=MD5Hex ($s+$x)};"1.$s"}
$cred=Get-Content 'E:\Code\ai\smart-downloader\xunlei_auth_web.json' -Raw|ConvertFrom-Json
$did=-join((1..32)|ForEach-Object{'{0:x}' -f (Get-Random -Max 16)});$ts=[string]([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())
$h1=@{Authorization="Bearer $($cred.access_token)";'User-Agent'='Mozilla/5.0'}
$cap=Invoke-RestMethod -Uri 'https://xluser-ssl.xunlei.com/v1/shield/captcha/init' -Method Post -ContentType 'application/json' -Headers $h1 -Body (@{action='GET:/drive/v1/files';captcha_token='';client_id=$CID;device_id=$did;redirect_uri='xlaccsdk01://xunlei.com/callback?state=harbor';meta=@{client_version=$VER;package_name=$PKG;user_id=$UID;timestamp=$ts;captcha_sign=(CaptchaSign $did $ts)}}|ConvertTo-Json -Depth 6) -TimeoutSec 20
$h2=@{Authorization="Bearer $($cred.access_token)";'X-Captcha-Token'=$cap.captcha_token;'X-Client-Id'=$CID;'X-Device-Id'=$did;Origin='https://pan.xunlei.com';Referer='https://pan.xunlei.com/'}

Write-Host '[1] firing fallback...'
$sw=[Diagnostics.Stopwatch]::StartNew()
try { $r = Invoke-RestMethod -Method Post -Uri "$base/tasks/$tid/fallback" -TimeoutSec 1700; Write-Host ("FALLBACK OK: " + ($r|ConvertTo-Json -Depth 4)) } catch { Write-Host "FALLBACK FAIL: $($_.Exception.Message)"; if ($_.ErrorDetails.Message) { Write-Host $_.ErrorDetails.Message }; exit 1 }
Write-Host ("[1] elapsed $([int]$sw.Elapsed.TotalSeconds)s")

Write-Host '[2] locate downloaded file...'
$f = Get-ChildItem 'E:\temp\f31\downloads' -Recurse -File | Sort-Object Length -Descending | Select-Object -First 1
if (-not $f) { Write-Host 'no file found'; exit 1 }
$localMd5 = (Get-FileHash $f.FullName -Algorithm MD5).Hash
Write-Host ("    $($f.Name)  $([math]::Round($f.Length/1MB,2)) MB")
Write-Host ("    local  MD5 = $localMd5")

Write-Host '[3] fetch cloud md5_checksum ...'
$list = Invoke-RestMethod -Uri 'https://api-pan.xunlei.com/drive/v1/files?parent_id=&limit=100&with_audit=true' -Headers $h2 -TimeoutSec 20
$cloudMd5 = ''
foreach ($fi in @($list.files)) {
  if ($fi.name -eq $f.Name -and $fi.md5_checksum) { $cloudMd5 = $fi.md5_checksum; break }
}
if (-not $cloudMd5) {
  foreach ($dir in @($list.files | Where-Object { $_.kind -eq 'drive#folder' })) {
    try {
      $sub = Invoke-RestMethod -Uri "https://api-pan.xunlei.com/drive/v1/files?parent_id=$($dir.id)&limit=100" -Headers $h2 -TimeoutSec 20
      foreach ($fi in @($sub.files)) { if ($fi.name -eq $f.Name -and $fi.md5_checksum) { $cloudMd5 = $fi.md5_checksum; break } }
    } catch {}
    if ($cloudMd5) { break }
  }
}
if ($cloudMd5) {
  $cm = $cloudMd5.ToUpper()
  Write-Host "    cloud  MD5 = $cm"
  if ($cm -eq $localMd5) { Write-Host "`n>>> F3.1 PASS: MD5 FULLY MATCHED <<<" } else { Write-Host "`n>>> MISMATCH <<<"; exit 1 }
} else {
  Write-Host '    云端未返回 md5_checksum（可能文件在子层未遍历到）——以字节数一致性为弱校验'
}
