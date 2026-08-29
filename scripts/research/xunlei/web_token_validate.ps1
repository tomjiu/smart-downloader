$ErrorActionPreference = 'Stop'
$CID = 'Xqp0kJBXWhwaTpB6'; $VER = '1.92.91'; $HOSTN = 'pan.xunlei.com'
$UID = '860599297'; $PKG = 'pan.xunlei.com'
$salts = @(
  'tkPbM0TLWT+eMvAdV2FbXEEQ/Qx5QrfO895+47hmDDPdRZ98xm',
  '7EBc6XKuI6YGw19anZHmnE4d8W18zjrJU+F',
  'stEQvsO6eeP93DdrX7mfYA7G',
  'edXgGCdIaqdZJZH5k',
  'J9SB6D864S1B',
  'xlAs2Oo28sr',
  '21+f+kgyrbIcwUUo+xaPD4GYHkpRGv5i4wOnyHrkH4ehKti',
  '08kltU1bp6eV5bEdlgSEU0GpzjD7/j5X3FwbiiraEzar',
  'hX6tf7kBT/DS'
)
function MD5Hex([string]$s) {
  $md5 = [System.Security.Cryptography.MD5]::Create()
  ($md5.ComputeHash([Text.Encoding]::UTF8.GetBytes($s)) | ForEach-Object { $_.ToString('x2') }) -join ''
}
function CaptchaSign([string]$did, [string]$tsMs) {
  $s = "$CID$VER$HOSTN$did$tsMs"
  foreach ($salt in $salts) { $s = MD5Hex ($s + $salt) }
  return "1.$s"
}
function PostJson([string]$url, [object]$bodyObj, [hashtable]$headers) {
  $json = $bodyObj | ConvertTo-Json -Depth 6
  Invoke-RestMethod -Uri $url -Method Post -ContentType 'application/json' -Body $json -Headers $headers -TimeoutSec 20
}

$cred = Get-Content 'E:\Code\ai\smart-downloader\xunlei_auth_web.json' -Raw | ConvertFrom-Json
$did = -join ((1..32) | ForEach-Object { '{0:x}' -f (Get-Random -Max 16) })
$ts = [string]([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())
$sign = CaptchaSign $did $ts
Write-Host "[*] did=$did"; Write-Host "[*] sign=$sign"

$h1 = @{ Authorization = "Bearer $($cred.access_token)"; 'User-Agent' = 'Mozilla/5.0' }
$capBody = @{
  action = 'POST:/drive/v1/files'; captcha_token = ''; client_id = $CID
  device_id = $did; redirect_uri = 'xlaccsdk01://xunlei.com/callback?state=harbor'
  meta = @{
    client_version = $VER; package_name = $PKG; user_id = $UID
    timestamp = $ts; captcha_sign = $sign
  }
}
Write-Host '[1] captcha/init (meta 带 user_id+captcha_sign)...'
$cap = PostJson 'https://xluser-ssl.xunlei.com/v1/shield/captcha/init' $capBody $h1
Write-Host ("    OK token len=" + $cap.captcha_token.Length)

$h2 = @{
  Authorization = "Bearer $($cred.access_token)"; 'X-Captcha-Token' = $cap.captcha_token
  'X-Client-Id' = $CID; 'X-Device-Id' = $did
  Origin = 'https://pan.xunlei.com'; Referer = 'https://pan.xunlei.com/'
  'User-Agent' = 'Mozilla/5.0'
}
$url = 'https://api-pan.xunlei.com/drive/v1/files?parent_id=&usage=DISPLAY&with_audit=true&thumbnail_size=SIZE_SMALL&limit=5'
Write-Host '[2] drive/v1/files ...'
$r = Invoke-RestMethod -Uri $url -Headers $h2 -TimeoutSec 20
Write-Host ("    200, files=" + $r.files.Count)
foreach ($f in @($r.files) | Select-Object -First 3) { Write-Host ("    - " + $f.name + "  id=" + $f.id) }

if (@($r.files).Count -gt 0) {
  # 先找根目录下的真实文件；没有则遍历各文件夹子层
  $fid = $null; $fname = ''
  foreach ($f in @($r.files)) {
    if ($f.kind -ne 'drive#folder') { $fid = $f.id; $fname = $f.name; break }
  }
  if (-not $fid) {
    foreach ($dir in @($r.files)) {
      Write-Host ("[3] list folder: " + $dir.name)
      try {
        $r2 = Invoke-RestMethod -Uri "https://api-pan.xunlei.com/drive/v1/files?parent_id=$($dir.id)&limit=20" -Headers $h2 -TimeoutSec 20
      } catch { continue }
      foreach ($f in @($r2.files)) {
        if ($f.kind -ne 'drive#folder') { $fid = $f.id; $fname = $f.name; break }
      }
      if ($fid) { break }
    }
    if (-not $fid) { Write-Host 'all folders empty of files'; exit 2 }
  }
  Write-Host "[3] PLAY link for [$fname] id=$fid ..."
  $play = Invoke-RestMethod -Uri "https://api-pan.xunlei.com/drive/v1/files/$fid`?space=&usage=PLAY" -Headers $h2 -TimeoutSec 20
  $link = $play.web_content_link
  if (-not $link) { Write-Host 'web_content_link empty'; exit 2 }
  Write-Host ('    LINK: ' + $link.Substring(0, [Math]::Min(90, $link.Length)) + '...')
  # 实测 Range 下载首块
  $req2 = [System.Net.HttpWebRequest]::Create($link)
  $req2.AddRange(0, 1023); $req2.Timeout = 20000
  $resp2 = $req2.GetResponse()
  $st2 = $resp2.GetResponseStream(); $buf = New-Object byte[] 1024; $got = $st2.Read($buf, 0, 1024)
  $st2.Close(); $resp2.Close()
  Write-Host ("    Range download OK: HTTP " + [int]$resp2.StatusCode + " got $got bytes")
  Write-Host '>>> FULL CHAIN VERIFIED: token->captcha->list->PLAY->range-download <<<'
  exit 0
} else { Write-Host 'no files in drive (empty account?) - chain still OK up to list'; exit 0 }
