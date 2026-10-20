# scripts/test-security.ps1
# 安全实机手工验证（Issue #121 维度 D，REALTEST-PLAN §5）
#
# CI 化说明：全部 6+3 项攻击已常驻为 crates/daemon/tests/security_live.rs
#（真实 daemon + 裸 TCP/reqwest 攻击探针，cargo test 自动跑）。
# 本脚本用于**手工实机**复核（对任意已部署的 daemon 实例），
# 与 CI 测试同口径：Host/Origin 同源守卫、token 401 矩阵、
# 路径穿越、source 脱敏、tasks.json 权限。

param(
    [string]$Base = "http://127.0.0.1:8788",
    [string]$Token = ""   # daemon 配置了 http_token 时填；空 = tokenless 模式
)

$ErrorActionPreference = "Stop"
$fail = 0

function Check($name, $ok, $detail = "") {
    if ($ok) { Write-Host "[OK]   $name" }
    else { Write-Host "[FAIL] $name $detail"; $script:fail++ }
}

# ── D-1 #3：DNS rebinding（Host: evil.com）→ 403（tokenless 模式）──
if (-not $Token) {
    $r = curl.exe -s -o NUL -w "%{http_code}" -H "Host: evil.com" "$Base/tasks" 2>$null
    Check "D1-3 Host:evil.com -> 403" ($r -eq "403") "(实际 $r)"

    # ── D-1 #4：跨源 POST → 403 ──
    $r = curl.exe -s -o NUL -w "%{http_code}" -X POST -H "Origin: https://evil.com" `
        -H "Content-Type: application/json" -d '{"url":"http://127.0.0.1:1/x"}' "$Base/tasks" 2>$null
    Check "D1-4 Origin:evil.com -> 403" ($r -eq "403") "(实际 $r)"

    # ── D-1 #6：Origin null（沙箱 iframe）→ 403 ──
    $r = curl.exe -s -o NUL -w "%{http_code}" -X POST -H "Origin: null" `
        -H "Content-Type: application/json" -d '{"url":"http://127.0.0.1:1/x"}' "$Base/tasks" 2>$null
    Check "D1-6 Origin:null -> 403" ($r -eq "403") "(实际 $r)"

    # ── D-1 #5：Host 缺失（HTTP/1.0 手改报文）→ 403（fail-closed）──
    $raw = "GET /tasks HTTP/1.0`r`n`r`n"
    $client = [System.Net.Sockets.TcpClient]::new("127.0.0.1", [int]($Base -replace '\D*(\d+)$', '$1'))
    $stream = $client.GetStream()
    $bytes = [System.Text.Encoding]::ASCII.GetBytes($raw)
    $stream.Write($bytes, 0, $bytes.Length)
    $buf = New-Object byte[] 1024
    $n = $stream.Read($buf, 0, 1024)
    $resp = [System.Text.Encoding]::ASCII.GetString($buf, 0, $n)
    $client.Close()
    Check "D1-5 无 Host HTTP/1.0 -> 403" ($resp -match "403") "(首行 $($resp.Split("`n")[0]))"
} else {
    # ── D-1 #1/#2：token 模式 401 矩阵 ──
    $r = curl.exe -s -o NUL -w "%{http_code}" "$Base/tasks" 2>$null
    Check "D1-1 无 token -> 401" ($r -eq "401") "(实际 $r)"

    $r = curl.exe -s -o NUL -w "%{http_code}" -H "Authorization: Bearer wrong" "$Base/tasks" 2>$null
    Check "D1-2 错 token -> 401" ($r -eq "401") "(实际 $r)"

    $r = curl.exe -s -o NUL -w "%{http_code}" -H "Authorization: Bearer $Token" "$Base/tasks" 2>$null
    Check "D1 对照 正确 token -> 200" ($r -eq "200") "(实际 $r)"
}

# ── D-2：路径穿越（daemon 侧拒 400；201 = 攻击成功）──
$evils = @("../../../etc", "a/../../b", "/etc")
foreach ($e in $evils) {
    $body = @{ url = "http://127.0.0.1:1/x"; dest = $e } | ConvertTo-Json -Compress
    if ($Token) {
        $r = curl.exe -s -o NUL -w "%{http_code}" -X POST -H "Content-Type: application/json" `
            -H "Authorization: Bearer $Token" -d $body "$Base/tasks" 2>$null
    } else {
        $r = curl.exe -s -o NUL -w "%{http_code}" -X POST -H "Content-Type: application/json" `
            -d $body "$Base/tasks" 2>$null
    }
    Check "D2 dest=$e 被拒（非 201）" ($r -ne "201") "(实际 $r)"
}

# ── D-4 #1：tasks.json 权限（0600，unix；daemon 所在机执行）──
Write-Host ""
Write-Host "D-4 tasks.json 0600 权限与 source 脱敏：见 CI security_live.rs（d4_*）断言。"

Write-Host ""
if ($fail -eq 0) { Write-Host "全部通过 ✓" } else { Write-Host "$fail 项失败"; exit 1 }
