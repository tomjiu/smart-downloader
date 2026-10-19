# Smart Downloader UI 规范检查脚本
#
# 用途：CI 或本地检查 PR 是否符合 docs/design/UI-DESIGN-SPEC.md §9 验收清单
# 用法：pwsh -File scripts/check-ui-spec.ps1 [-Path <repo-root>] [-Stage <p0|p1|p2|all>]
#
# 退出码：0 = 全通过；1 = 有失败项

param(
    [string]$Path = ".",
    [ValidateSet("p0", "p1", "p2", "all")]
    [string]$Stage = "all"
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path $Path).Path
$ui = Join-Path $root "ui"

$script:pass = 0
$script:fail = 0
$script:warn = 0
$script:fails = @()

function Ok($id, $msg) {
    $script:pass++
    Write-Host "  [PASS] $id  $msg" -ForegroundColor Green
}
function Fail($id, $msg, $detail) {
    $script:fail++
    $script:fails += "$id $msg"
    Write-Host "  [FAIL] $id  $msg" -ForegroundColor Red
    if ($detail) { Write-Host "         $detail" -ForegroundColor DarkGray }
}
function Warn($id, $msg, $detail) {
    $script:warn++
    Write-Host "  [WARN] $id  $msg" -ForegroundColor Yellow
    if ($detail) { Write-Host "         $detail" -ForegroundColor DarkGray }
}
function Head($t) { Write-Host "`n$t" -ForegroundColor Cyan }

# 收集源文件（排除构建产物与依赖）
# Issue #115 顺手修（跨平台）：排除正则原为 Windows 分隔符（\out\），
# Linux 上 /out/ 构建产物会被误扫（渐变/间距检查误报）。改为同时
# 匹配 / 与 \ 两种分隔符。
function Get-SourceFiles($patterns) {
    if (-not (Test-Path $ui)) { return @() }
    Get-ChildItem $ui -Recurse -File -Include $patterns -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -notmatch 'node_modules|[/\\]\.next[/\\]|[/\\]out[/\\]|[/\\]public[/\\]qoder-ui[/\\]' }
}

Write-Host "`n=== Smart Downloader UI 规范检查 ===" -ForegroundColor White
Write-Host "仓库: $root"
Write-Host "阶段: $Stage`n"

# ══════════════════════════════════════════════════════════
Head "§9.1 阶段门禁 (P0)"

# G0-1 qoder-ui 目录已删除
$qoderDir = Join-Path $ui "public\qoder-ui"
if (Test-Path $qoderDir) {
    $sz = [math]::Round((Get-ChildItem $qoderDir -Recurse -File | Measure-Object Length -Sum).Sum / 1KB)
    Fail "G0-1" "qoder-ui 目录仍存在" "ui/public/qoder-ui ($sz KB) — 应删除"
} else {
    Ok "G0-1" "qoder-ui 目录已删除"
}

# G0-2 无 qoder- class 引用
$qoderRefs = @()
foreach ($f in (Get-SourceFiles @("*.tsx","*.ts","*.css"))) {
    $m = Select-String -Path $f.FullName -Pattern 'qoder-' -AllMatches -ErrorAction SilentlyContinue
    if ($m) { $qoderRefs += $m }
}
if ($qoderRefs.Count -gt 0) {
    $files = ($qoderRefs | Select-Object -ExpandProperty Path -Unique) -join ", "
    Fail "G0-2" "仍有 $($qoderRefs.Count) 处 qoder- 引用" $files
} else {
    Ok "G0-2" "无 qoder- class 残留"
}

# G0-3 / G0-4 主题定义
$tokensFile = Get-ChildItem $ui -Recurse -File -Include "tokens.css","*.css" -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -eq "tokens.css" } | Select-Object -First 1

$themeSource = $null
if ($tokensFile) { $themeSource = Get-Content $tokensFile.FullName -Raw }
else {
    # 退化：在 globals.css 中找
    $g = Join-Path $ui "src\app\globals.css"
    if (Test-Path $g) { $themeSource = Get-Content $g -Raw }
}

if (-not $themeSource) {
    Fail "G0-3" "未找到 tokens.css 或 globals.css" "无法检查主题定义"
    Fail "G0-4" "跳过（无主题源文件）"
} else {
    $themes = [regex]::Matches($themeSource, '\[data-theme="([^"]+)"\]') |
        ForEach-Object { $_.Groups[1].Value } | Select-Object -Unique
    if ($themes.Count -ge 12) {
        Ok "G0-3" "主题定义 $($themes.Count) 个（≥12）"
    } else {
        Fail "G0-3" "主题定义仅 $($themes.Count) 个（需 12）" ($themes -join ", ")
    }

    # Issue #115 催修（脚本 bug 1）：0 个主题时空集合 all() 恒真 → 假 PASS。
    # 先断言主题集合非空，再逐个校验 token。
    $need = @('--bg','--fg','--accent','--ok','--warn','--err','--line','--track')
    $bad = @()
    if ($themes.Count -eq 0) {
        Fail "G0-4" "无主题可校验（空集合假 PASS 修复）" "主题定义数为 0"
    } else {
        foreach ($t in $themes) {
            $m = [regex]::Match($themeSource, "\[data-theme=`"$([regex]::Escape($t))`"\]\s*\{[^}]*\}")
            if ($m.Success) {
                $miss = $need | Where-Object { $m.Value -notmatch [regex]::Escape($_) }
                if ($miss) { $bad += "$t 缺 $($miss -join ',')" }
            } else { $bad += "$t 解析失败" }
        }
        if ($bad.Count -gt 0) { Fail "G0-4" "主题 token 不完整" ($bad -join "; ") }
        else { Ok "G0-4" "全部 $($themes.Count) 主题 token 完整" }
    }
}

# G0-5 无 transition: all
$ta = @()
foreach ($f in (Get-SourceFiles @("*.css","*.tsx"))) {
    $m = Select-String -Path $f.FullName -Pattern 'transition:\s*all' -AllMatches -ErrorAction SilentlyContinue
    if ($m) { $ta += $m }
}
if ($ta.Count -gt 0) {
    $detail = ($ta | ForEach-Object { "$($_.Path):$($_.LineNumber)" } | Select-Object -First 5) -join "`n         "
    Fail "G0-5" "发现 $($ta.Count) 处 transition: all" $detail
} else {
    Ok "G0-5" "无 transition: all"
}

# G0-6 backdrop-filter（仅允许 modal 遮罩，最多 1 处）
$bf = @()
foreach ($f in (Get-SourceFiles @("*.css"))) {
    $m = Select-String -Path $f.FullName -Pattern 'backdrop-filter' -AllMatches -ErrorAction SilentlyContinue
    if ($m) { $bf += $m }
}
if ($bf.Count -gt 1) {
    $detail = ($bf | ForEach-Object { "$($_.Path):$($_.LineNumber)" } | Select-Object -First 5) -join "`n         "
    Fail "G0-6" "backdrop-filter $($bf.Count) 处（≤1）" $detail
} else {
    Ok "G0-6" "backdrop-filter $($bf.Count) 处"
}

# G0-7 渐变仅用于进度条
$grad = @()
foreach ($f in (Get-SourceFiles @("*.css"))) {
    $m = Select-String -Path $f.FullName -Pattern 'linear-gradient|radial-gradient' -AllMatches -ErrorAction SilentlyContinue
    if ($m) { $grad += $m }
}
if ($grad.Count -gt 3) {
    Warn "G0-7" "渐变 $($grad.Count) 处，确认仅用于进度条/背景纹理" ($grad | ForEach-Object { "$($_.Path):$($_.LineNumber)" } | Select-Object -First 5) -join "`n         "
} else {
    Ok "G0-7" "渐变 $($grad.Count) 处（合理范围）"
}

# G0-8 无错峰入场动画
$enter = @()
foreach ($f in (Get-SourceFiles @("*.css","*.tsx"))) {
    $m = Select-String -Path $f.FullName -Pattern 'animation-delay:\s*calc' -AllMatches -ErrorAction SilentlyContinue
    if ($m) { $enter += $m }
}
if ($enter.Count -gt 0) {
    Fail "G0-8" "发现 $($enter.Count) 处错峰入场动画" (($enter | ForEach-Object { "$($_.Path):$($_.LineNumber)" }) -join ", ")
} else {
    Ok "G0-8" "无错峰入场动画"
}

# G0-9 daemon API 零改动
Push-Location $root
try {
    $diff = & git diff main --name-only -- crates/ 2>$null
    if ($LASTEXITCODE -eq 0 -and $diff) {
        Fail "G0-9" "crates/ 有改动（daemon API 应零改动）" (($diff | Select-Object -First 5) -join ", ")
    } else {
        Ok "G0-9" "crates/ 无改动"
    }
} catch {
    Warn "G0-9" "无法检查 git diff" $_.Exception.Message
} finally { Pop-Location }

# ══════════════════════════════════════════════════════════
if ($Stage -in @("p1", "p2", "all")) {
    Head "§9.3 细节规范"

    $cssFiles = Get-SourceFiles @("*.css")
    $allCss = ($cssFiles | ForEach-Object { Get-Content $_.FullName -Raw }) -join "`n"

    # D4 选中态
    if ($allCss -match 'is-selected|\.selected') { Ok "D4" "行选中态已定义" }
    else { Warn "D4" "未找到行选中态样式" "期望 .is-selected" }

    # D6 cursor: not-allowed
    if ($allCss -match 'cursor:\s*not-allowed') { Ok "D6" "禁用态 cursor 正确" }
    else { Fail "D6" "禁用态缺 cursor: not-allowed" "" }

    # D8 间距为 4 的倍数
    $badSpace = @()
    foreach ($f in $cssFiles) {
        $content = Get-Content $f.FullName -Raw
        foreach ($prop in @('padding','margin','gap','row-gap','column-gap')) {
            $ms = [regex]::Matches($content, "$prop[^:]*:\s*([0-9]+)px")
            foreach ($m in $ms) {
                $v = [int]$m.Groups[1].Value
                if ($v -gt 0 -and ($v % 4) -ne 0 -and $v -notin @(1,2,6,10)) {
                    $badSpace += "$($f.Name): $prop ${v}px"
                }
            }
        }
    }
    if ($badSpace.Count -gt 0) {
        Warn "D8" "$($badSpace.Count) 处间距非 4 倍数" (($badSpace | Select-Object -Unique -First 8) -join "; ")
    } else { Ok "D8" "间距均符合 4 倍数网格" }

    # D9 tabular-nums
    if ($allCss -match 'tabular-nums') { Ok "D9" "数字列 tabular-nums 已用" }
    else { Fail "D9" "缺 tabular-nums" "速率/进度数字会抖动" }

    # D10 prefers-reduced-motion
    if ($allCss -match 'prefers-reduced-motion') { Ok "D10" "prefers-reduced-motion 保留" }
    else { Fail "D10" "缺 prefers-reduced-motion" "" }

    # D5 focus-visible
    # Issue #115 催修（脚本 bug 2）：0 处时仅 WARN 会漏放「完全没做键盘可达」
    # 的 PR——规范 §1.5.2 要求全部可交互元素覆盖，0 处即 FAIL。
    $fv = ([regex]::Matches($allCss, ':focus-visible')).Count
    if ($fv -ge 3) { Ok "D5" "focus-visible $fv 处" }
    elseif ($fv -gt 0) { Warn "D5" "focus-visible 仅 $fv 处，确认覆盖所有可交互元素" "" }
    else { Fail "D5" "focus-visible 0 处（键盘可达性缺失）" "规范 §1.5.2 要求全部可交互元素覆盖" }

    # D7 过渡时长
    $slow = @()
    foreach ($f in $cssFiles) {
        $m = Select-String -Path $f.FullName -Pattern 'transition[^;]*\b(3|4|5)00ms|transition[^;]*\b0?\.[3-9]s' -AllMatches -ErrorAction SilentlyContinue
        if ($m) { $slow += $m }
    }
    if ($slow.Count -gt 0) {
        Warn "D7" "$($slow.Count) 处过渡 >200ms" (($slow | ForEach-Object { "$($_.Path):$($_.LineNumber)" } | Select-Object -First 5) -join ", ")
    } else { Ok "D7" "无过慢过渡" }
}

# ══════════════════════════════════════════════════════════
if ($Stage -in @("p2", "all")) {
    Head "§9.5 性能"

    $tsx = Get-SourceFiles @("*.tsx","*.ts")

    # P1 JSON.stringify 全量比较
    $js = @()
    foreach ($f in $tsx) {
        $m = Select-String -Path $f.FullName -Pattern 'JSON\.stringify\(cur\)\s*===\s*JSON\.stringify' -AllMatches -ErrorAction SilentlyContinue
        if ($m) { $js += $m }
    }
    if ($js.Count -gt 0) {
        Fail "P1" "发现 $($js.Count) 处 JSON.stringify 全量比较" (($js | ForEach-Object { "$($_.Path):$($_.LineNumber)" }) -join ", ")
    } else { Ok "P1" "无 JSON.stringify 全量比较" }

    # F6 NaN 防护
    $nan = $false
    foreach ($f in $tsx) {
        if (Select-String -Path $f.FullName -Pattern 'total\s*>\s*0\s*\?' -Quiet -ErrorAction SilentlyContinue) { $nan = $true }
    }
    if ($nan) { Ok "F6" "进度计算有 total>0 防护" }
    else { Warn "F6" "未见 total>0 防护，确认无 NaN%" "" }
}

# ══════════════════════════════════════════════════════════
Write-Host "`n=== 汇总 ===" -ForegroundColor White
Write-Host "  通过: $script:pass" -ForegroundColor Green
if ($script:warn -gt 0) { Write-Host "  警告: $script:warn" -ForegroundColor Yellow }
if ($script:fail -gt 0) {
    Write-Host "  失败: $script:fail" -ForegroundColor Red
    Write-Host "`n失败项:" -ForegroundColor Red
    $script:fails | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    Write-Host ""
    exit 1
}
Write-Host "`n全部检查通过 ✅`n" -ForegroundColor Green
exit 0
