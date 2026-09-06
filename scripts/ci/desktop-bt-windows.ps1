# 桌面版 BT 引擎装配（S1-d）—— Windows 段。
#
# libtorrent 走 vcpkg 动态 triplet（x64-windows，与 btcore build.rs 的 vcpkg 契约
# 原生同构——该契约最初即为 Windows 设计）。FFI 内核 lt_kernel.cpp 用 MSVC（cl）
# 编译为静态库，/MD 与 vcpkg / Rust msvc 默认 CRT 一致。
#
# 运行期 DLL 随包分发：vcpkg installed/x64-windows/bin 全量 + MSVC CRT 三件套，
# 经 tauri.windows.conf.json 资源映射落安装根（= exe 目录，DLL 搜索路径首位命中）。
#
# 用法：
#   desktop-bt-windows.ps1 -Cmd setup            # vcpkg 装 libtorrent + cl 编内核 + GITHUB_ENV
#   desktop-bt-windows.ps1 -Cmd stage            # DLL 闭包 → src-tauri/native/win/
param(
    [Parameter(Mandatory = $true, Position = 0)][ValidateSet("setup", "stage")][string]$Cmd,
    [string]$RepoRoot = (Join-Path $PSScriptRoot ".." | Join-Path -ChildPath "..")
)
$ErrorActionPreference = "Stop"

$Installed = $null   # <vcpkg>\installed\x64-windows

function Add-GhEnv([string]$k, [string]$v) {
    if ($env:GITHUB_ENV) { Add-Content -Path $env:GITHUB_ENV -Value "$k=$v" }
    else { Write-Host "  (local) $k=$v" }
}

function Invoke-Setup {
    $vcpkgRoot = $env:VCPKG_INSTALLATION_ROOT
    if (-not $vcpkgRoot) { throw "VCPKG_INSTALLATION_ROOT 未设置（非 GitHub runner？）" }
    $vcpkg = Join-Path $vcpkgRoot "vcpkg.exe"

    # 二进制缓存：actions/cache 挂仓库 .vcpkg-cache（内容寻址，镜像换代自动失配重建）
    $binCache = Join-Path $RepoRoot ".vcpkg-cache"
    New-Item -ItemType Directory -Force -Path $binCache | Out-Null
    $env:VCPKG_DEFAULT_BINARY_CACHE = $binCache
    $env:VCPKG_DISABLE_METRICS = "1"

    Write-Host "==> vcpkg install libtorrent (x64-windows)…"
    $log = Join-Path $env:TEMP "vcpkg-libtorrent.log"
    & $vcpkg install libtorrent --triplet x64-windows 2>&1 | Tee-Object -FilePath $log | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Get-Content $log -Tail 60
        throw "vcpkg install libtorrent 失败（日志 $log）"
    }
    Write-Host "    vcpkg install 完成（日志 $($log)，$((Get-Item $log).Length) 字节）"

    $script:Installed = Join-Path $vcpkgRoot "installed\x64-windows"
    $libDir = Join-Path $Installed "lib"
    $torrentLib = Get-ChildItem $libDir -Filter "*torrent*.lib" | Select-Object -First 1
    if (-not $torrentLib) { throw "vcpkg 产物缺 libtorrent 导入库（$libDir）" }
    $torrentDll = Get-ChildItem (Join-Path $Installed "bin") -Filter "*torrent*.dll" | Select-Object -First 1
    if (-not $torrentDll) { throw "vcpkg 产物缺 libtorrent DLL" }
    Write-Host "    libtorrent: $($torrentDll.Name) / $($torrentLib.Name)"

    # FFI 内核静态库（MSVC /MD，与 vcpkg、Rust msvc CRT 对齐）
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vsPath = & $vswhere -latest -products * `
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
        -property installationPath
    if (-not $vsPath) { throw "vswhere 未定位到 VS（缺 VC.Tools.x86.x64 组件？）" }
    $vcvars = Join-Path $vsPath "VC\Auxiliary\Build\vcvars64.bat"

    $buildDir = Join-Path $env:USERPROFILE "bt-native-windows\build"
    New-Item -ItemType Directory -Force -Path $buildDir | Out-Null
    $ffi = Join-Path $RepoRoot "ffi"

    # vcvars（供 Windows SDK 的 INCLUDE/LIB）+ cl 编译 + lib 打静态库：
    # 走临时 .cmd 批处理执行，规避 cmd /c 长串内嵌引号剥层问题
    $bat = Join-Path $buildDir "_build_kernel.cmd"
    # 构建态宏一致性：vcpkg libtorrent 带 openssl 时（libssl.lib 存在），消费者编译
    # 必须同定义 TORRENT_USE_OPENSSL（2.1.x RTC 强制校验；2.0.x 内联/模板实例化
    # 同样需要一致），否则链接期 undefined symbols
    $sslDef = ""
    if (Test-Path (Join-Path $Installed "lib\libssl.lib")) { $sslDef = "/DTORRENT_USE_OPENSSL=1 " }
    # ⚠ 批处理行必须单行拼死：@() 元素里 "+ 换行" 续接不生效，片段会各自成行
    # （二轮实证：cl 行腰斩 → D8003 缺源文件 + '/c' 被当命令执行）
    $inc = Join-Path $Installed 'include'
    $src = "$ffi\src\lt_kernel.cpp"
    $out = Join-Path $buildDir 'lt_kernel.lib'
    @(
        "call `"$vcvars`" >NUL",
        "cd /d `"$buildDir`"",
        "cl /nologo /std:c++17 /O2 /MD /EHsc /DNDEBUG $sslDef/I`"$inc`" /I`"$ffi`" /c `"$src`"",
        "if errorlevel 1 exit /b 1",
        "lib /nologo /OUT:`"$out`" lt_kernel.obj",
        "if errorlevel 1 exit /b 1"
    ) | Set-Content -Path $bat
    # PowerShell 直调 .cmd（内部即 cmd /c，无引号折叠问题；cmd /c "`"$bat`"" 形式
    # 会把 /c 折进参数串导致 "'/c' is not recognized"——首跑实证）
    & $bat
    if ($LASTEXITCODE -ne 0) { throw "lt_kernel.lib 编译失败（$bat）" }
    if (-not (Test-Path (Join-Path $buildDir "lt_kernel.lib"))) { throw "lt_kernel.lib 未生成" }
    Write-Host "    lt_kernel.lib 就绪：$buildDir"

    Add-GhEnv "LT_KERNEL_LIB_DIR" $buildDir
    Add-GhEnv "LT_VCPKG_LIB_DIR" $libDir
    Write-Host "==> desktop-bt-windows setup 完成"
}

function Invoke-Stage {
    if (-not $Installed) {
        # stage 可能在独立 step 执行：从 GITHUB_ENV 注入的变量还原 installed 根
        $libDir = $env:LT_VCPKG_LIB_DIR
        if (-not $libDir) { throw "LT_VCPKG_LIB_DIR 未设置（须先跑 setup）" }
        $script:Installed = Split-Path $libDir -Parent
    }
    $nativeDir = Join-Path $RepoRoot "desktop\src-tauri\native\win"
    if (Test-Path $nativeDir) { Remove-Item -Recurse -Force $nativeDir }
    New-Item -ItemType Directory -Force -Path $nativeDir | Out-Null

    # vcpkg bin 全量 = libtorrent + 传递闭包（openssl/zlib/boost…），装进安装根
    Copy-Item (Join-Path $Installed "bin\*.dll") $nativeDir

    # MSVC CRT 三件套（vcpkg 与 Rust 均为 /MD；随包携带，用户机器免装 VC Redist）
    foreach ($dll in @("msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll")) {
        $src = Join-Path $env:SystemRoot "System32\$dll"
        if (Test-Path $src) { Copy-Item $src $nativeDir }
        else { Write-Host "    警告：System32 缺 $dll" }
    }

    Write-Host ("==> DLL 闭包 {0} 个 → {1}" -f (Get-ChildItem $nativeDir).Count, $nativeDir)
    Get-ChildItem $nativeDir | ForEach-Object { Write-Host ("    {0}  {1:N0} B" -f $_.Name, $_.Length) }
}

switch ($Cmd) {
    "setup" { Invoke-Setup }
    "stage" { Invoke-Stage }
}
