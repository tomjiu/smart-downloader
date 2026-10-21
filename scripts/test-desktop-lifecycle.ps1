# scripts/test-desktop-lifecycle.ps1
# 进程/资源生命周期实机验证（Issue #121 维度 C-2）
#
# 当前仓库形态：daemon 独立进程 + Web UI（无桌面壳）。
# 本脚本验证 **daemon 进程生命周期**（REALTEST-PLAN C-2 的等价物）：
#   1. 启动 -> /health 200
#   2. 同 storage 第二实例被 lockfile 拒绝（单实例互斥）
#   3. 优雅关闭（Ctrl+C 语义）-> 进程退出
#   4. 端口可立即复用
#
# 桌面壳（tauri/wails 类）上线后，在 §桌面壳场景 追加：
#   - 普通关窗 -> daemon 退出（Job Object 子进程清理）
#   - 强杀壳进程（Stop-Process -Force）-> daemon 自动退出
#
# CI 化说明：unix 侧等价验证已常驻为
# crates/daemon/tests/lifecycle_process.rs（cargo test 自动跑，
# SIGTERM/lockfile/端口复用断言）；本脚本用于 Windows 手工实机验证。

param(
    [string]$Exe = "target\release\smart-dl-daemon.exe",
    [int]$Port = 18788,
    [string]$WorkDir = "$env:TEMP\sd-lifecycle-$(Get-Random)"
)

$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Path $WorkDir -Force | Out-Null
$cfg = Join-Path $WorkDir "daemon.toml"
@"
[server]
addr = "127.0.0.1:$Port"
[storage]
tasks_path = "$($WorkDir.Replace('\', '\\'))\\tasks.json"
[lock]
path = "$($WorkDir.Replace('\', '\\'))\\daemon.lock"
"@ | Set-Content -Encoding UTF8 $cfg

Write-Host "=== 1. 启动 daemon（$Exe serve --config $cfg） ==="
$app = Start-Process $Exe -ArgumentList "serve", "--config", $cfg -PassThru
$healthy = $false
foreach ($i in 1..25) {
    Start-Sleep -Milliseconds 400
    try {
        $r = Invoke-WebRequest "http://127.0.0.1:$Port/health" -UseBasicParsing -TimeoutSec 2
        if ($r.StatusCode -eq 200) { $healthy = $true; break }
    } catch {}
}
if (-not $healthy) { throw "[FAIL] daemon 10s 内未就绪（/health）" }
Write-Host "[OK] daemon 已就绪 PID=$($app.Id)"

Write-Host "=== 2. 同 storage 第二实例：lockfile 互斥 ==="
$second = Start-Process $Exe -ArgumentList "serve", "--config", $cfg -PassThru -WindowStyle Hidden
$second.WaitForExit(15000) | Out-Null
if (-not $second.HasExited) { $second.Kill(); throw "[FAIL] 第二实例未被拒绝（单实例互斥失效）" }
if ($second.ExitCode -eq 0) { throw "[FAIL] 第二实例退出码为 0（应非 0）" }
Write-Host "[OK] 第二实例被 lockfile 拒绝（exit=$($second.ExitCode)）"

Write-Host "=== 3. 优雅关闭（Ctrl+C 语义） ==="
# GenerateConsoleCtrlEvent 需要 Windows API；手工验证时直接在控制台 Ctrl+C。
# 脚本化用 taskkill /pid（graceful 语义由 daemon 的 ctrl_c handler 承接）。
taskkill /pid $app.Id 2>$null | Out-Null
$app.WaitForExit(15000) | Out-Null
if (-not $app.HasExited) { $app.Kill(); throw "[FAIL] 关闭后 daemon 未退出（15s 宽限）" }
Write-Host "[OK] daemon 已退出"

Write-Host "=== 4. 端口可立即复用 ==="
$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
try { $listener.Start(); Write-Host "[OK] 端口 $Port 立即可复用" }
catch { throw "[FAIL] 端口 $Port 未释放" }
finally { $listener.Stop() }

Write-Host ""
Write-Host "全部通过：启动/互斥/退出/端口复用 ✓"
Write-Host "桌面壳场景（Job Object 强杀）待桌面端上线后补充。"
