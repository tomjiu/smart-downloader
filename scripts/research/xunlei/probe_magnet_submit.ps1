$ErrorActionPreference='Stop'
$CID='Xqp0kJBXWhwaTpB6'; $VER='1.92.91'; $HOSTN='pan.xunlei.com'; $UID='860599297'; $PKG='pan.xunlei.com'
$salts=@('tkPbM0TLWT+eMvAdV2FbXEEQ/Qx5QrfO895+47hmDDPdRZ98xm','7EBc6XKuI6YGw19anZHmnE4d8W18zjrJU+F','stEQvsO6eeP93DdrX7mfYA7G','edXgGCdIaqdZJZH5k','J9SB6D864S1B','xlAs2Oo28sr','21+f+kgyrbIcwUUo+xaPD4GYHkpRGv5i4wOnyHrkH4ehKti','08kltU1bp6eV5bEdlgSEU0GpzjD7/j5X3FwbiiraEzar','hX6tf7kBT/DS')
function MD5Hex([string]$s){$m=[System.Security.Cryptography.MD5]::Create();($m.ComputeHash([Text.Encoding]::UTF8.GetBytes($s))|ForEach-Object{$_.ToString('x2')})-join ''}
function CaptchaSign([string]$d,[string]$t){$s="$CID$VER$HOSTN$d$t";foreach($x in $salts){$s=MD5Hex ($s+$x)};"1.$s"}
$cred=Get-Content 'E:\Code\ai\smart-downloader\xunlei_auth_web.json' -Raw|ConvertFrom-Json
$did=-join((1..32)|ForEach-Object{'{0:x}' -f (Get-Random -Max 16)});$ts=[string]([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())
$h=@{Authorization="Bearer $($cred.access_token)";'User-Agent'='Mozilla/5.0'}
$cap=Invoke-RestMethod -Uri 'https://xluser-ssl.xunlei.com/v1/shield/captcha/init' -Method Post -ContentType 'application/json' -Headers $h -Body (@{action='POST:/drive/v1/files';captcha_token='';client_id=$CID;device_id=$did;redirect_uri='xlaccsdk01://xunlei.com/callback?state=harbor';meta=@{client_version=$VER;package_name=$PKG;user_id=$UID;timestamp=$ts;captcha_sign=(CaptchaSign $did $ts)}}|ConvertTo-Json -Depth 6) -TimeoutSec 20
Write-Host ("captcha len=" + $cap.captcha_token.Length)
$h2=@{Authorization="Bearer $($cred.access_token)";'X-Captcha-Token'=$cap.captcha_token;'X-Client-Id'=$CID;'X-Device-Id'=$did;Origin='https://pan.xunlei.com';Referer='https://pan.xunlei.com/';'User-Agent'='Mozilla/5.0'}
$magnet='magnet:?xt=urn:btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c&dn=Big+Buck+Bunny&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce'
$body=@{kind='drive#file';name='Big Buck Bunny';parent_id='';upload_type='UPLOAD_TYPE_URL';url=@{url=$magnet}}|ConvertTo-Json
$r=$null
try { $r=Invoke-WebRequest -Uri 'https://api-pan.xunlei.com/drive/v1/files' -Method Post -ContentType 'application/json' -Headers $h2 -Body $body -TimeoutSec 25 -UseBasicParsing } catch { Write-Host "HTTP-ERR body: $($_.ErrorDetails.Message)"; exit 1 }
Write-Host "HTTP $($r.StatusCode)"
Write-Host $r.Content
