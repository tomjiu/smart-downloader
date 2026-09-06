#!/usr/bin/env bash
# 桌面版 BT 引擎装配（S1-d）—— Linux 段。
#
# 桌面 sidecar（smart-dl-daemon）从 no-BT profile 升级为 --features bt,ftp,sftp。
# libtorrent 以系统包（apt libtorrent-rasterbar-dev，Ubuntu 22.04 = 2.0.8）动态链接，
# 运行期 .so 闭包随包分发（deb / AppImage 共用同一相对布局，见下）。
#
# tauri v2 实证布局（v0.1.0 产物解剖）：
#   deb       sidecar → /usr/bin/smart-dl-daemon（externalBin 安装名剥去 -<triple> 后缀）
#             资源根 → /usr/lib/Smart Downloader/
#   AppImage  sidecar → AppDir/usr/bin/smart-dl-daemon
#             资源根 → AppDir/usr/lib/Smart Downloader/
# 两格式 $ORIGIN 均为 <prefix>/bin → RUNPATH 统一写：
#   $ORIGIN/../lib/Smart Downloader/native/linux
# 资源映射（tauri.linux.conf.json）把 native/linux/* 装进资源根 native/linux/。
#
# 用法：
#   scripts/ci/desktop-bt-linux.sh setup [REPO_ROOT]   # CI：apt + 内核静态库 + fakevcpkg + GITHUB_ENV
#   scripts/ci/desktop-bt-linux.sh stage <SIDECAR> [REPO_ROOT]  # 构建后：so 闭包拷贝 + rpath 注入 + 自检
#
# setup 复用 bt-integration job 同款 bt-linux-setup.sh（rootful）——保证桌面构建与
# CI 测试矩阵的链接行为逐字一致（同一内核编译行、同一 fakevcpkg 契约）。
set -euo pipefail

CMD="${1:?用法: desktop-bt-linux.sh setup|stage ...}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${3:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

# 平台资源目录（相对 src-tauri；stage 产物落入此处随 tauri resources 打包）
SRC_TAURI="$REPO_ROOT/desktop/src-tauri"
NATIVE_DIR="$SRC_TAURI/native/linux"
# 与 tauri.linux.conf.json / stage 内 RUNPATH 三处必须同步
RUNPATH='$ORIGIN/../lib/Smart Downloader/native/linux'

emit_github_env() {
    # CI 环境写 GITHUB_ENV；本地执行时打印（手工 source 用）
    if [[ -n "${GITHUB_ENV:-}" ]]; then
        {
            echo "LT_KERNEL_LIB_DIR=$1"
            echo "LT_VCPKG_LIB_DIR=$2"
            echo "LD_LIBRARY_PATH=$3"
            echo "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=$4"
        } >> "$GITHUB_ENV"
    else
        cat <<EOF
# 本地使用：手工 source 以下导出
export LT_KERNEL_LIB_DIR="$1"
export LT_VCPKG_LIB_DIR="$2"
export LD_LIBRARY_PATH="$3"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="$4"
EOF
    fi
}

do_setup() {
    # patchelf 是 stage 段的硬依赖（CI runner 预装 git 等，patchelf 需显式装）
    apt_do="apt-get"
    if command -v sudo >/dev/null 2>&1; then apt_do="sudo apt-get"; fi
    $apt_do install -y --no-install-recommends patchelf >/dev/null
    patchelf --version

    # 与 bt integration job 完全同源的 native 环境（内核 + fakevcpkg + 链接器 wrapper）
    local dest="$HOME/bt-native"
    bash "$SCRIPT_DIR/bt-linux-setup.sh" "$dest" >/dev/null

    # shellcheck disable=SC1091
    source "$dest/env.sh"
    emit_github_env "$LT_KERNEL_LIB_DIR" "$LT_VCPKG_LIB_DIR" "${LD_LIBRARY_PATH:-}" "$CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER"
    echo "==> desktop-bt-linux setup 完成（LT_KERNEL_LIB_DIR=$LT_KERNEL_LIB_DIR）"
}

do_stage() {
    local sidecar="${1:?stage 需要 sidecar 路径}"
    [[ -f "$sidecar" ]] || { echo "FATAL: sidecar 不存在: $sidecar" >&2; exit 1; }
    command -v patchelf >/dev/null || { echo "FATAL: patchelf 不可用" >&2; exit 1; }

    rm -rf "$NATIVE_DIR"
    mkdir -p "$NATIVE_DIR"

    # 前置：ldd 必须能全解析（CI：GITHUB_ENV 的 LD_LIBRARY_PATH / rootful 系统包；
    # 本地 no-root：先 source bt-linux-setup 产出的 env.sh）。闭包收集依赖可解析。
    local unresolved
    unresolved="$(ldd "$sidecar" | grep 'not found' || true)"
    if [[ -n "$unresolved" ]]; then
        echo "FATAL: 闭包收集前 ldd 存在未解析依赖（缺 native 环境？）:" >&2
        echo "$unresolved" >&2
        echo "提示：CI 先跑本脚本 setup；本地 no-root 需 source ~/bt-native/env.sh" >&2
        exit 1
    fi

    # 闭包采集：ldd 全量 NEEDED，排除 glibc 核心基座（用户系统必有且不可搬运）。
    # libstdc++ 必须随包（libtorrent 是 C++ ABI 消费者，老系统自带版本可能不够新）。
    # ldd 行格式：「libX.so.Y => /abs/path (0x…)」；末行 loader 无箭头（so_path 为空被跳过）
    local copied=0
    while read -r _lib _arrow so_path _rest; do
        [[ -n "$so_path" && -f "$so_path" ]] || continue
        local base
        base="$(basename "$so_path")"
        case "$base" in
            linux-vdso*|ld-linux*|libc.so*|libm.so*|libpthread*|libdl*|librt.so*|libgcc_s*) continue ;;
        esac
        cp -L "$(readlink -f "$so_path")" "$NATIVE_DIR/$base"
        patchelf --set-rpath '$ORIGIN' "$NATIVE_DIR/$base"
        copied=$((copied + 1))
    done < <(ldd "$sidecar")

    # sidecar 注入 RUNPATH（deb 与 AppImage 布局同构，见文件头）
    patchelf --set-rpath "$RUNPATH" "$sidecar"

    echo "==> 闭包 $copied 个 .so → $NATIVE_DIR:"
    ls -la "$NATIVE_DIR"

    # 自检：搭一个临时「安装布局」模拟树（deb / AppImage 同构，见文件头），
    # 在 LD_LIBRARY_PATH 清空的环境里跑 ldd —— RUNPATH 必须独立完成全部解析。
    # （staging 目录下 RUNPATH 指向的安装路径不存在属预期，故必须在模拟树内验。）
    local sim
    sim="$(mktemp -d)"
    mkdir -p "$sim/usr/bin" "$sim/usr/lib/Smart Downloader/native"
    cp "$sidecar" "$sim/usr/bin/smart-dl-daemon"
    # cp -r src dst/（dst 已存在 → 落位 dst/linux/，与 RUNPATH 的 native/linux 对齐）
    cp -r "$NATIVE_DIR" "$sim/usr/lib/Smart Downloader/native/"
    local missing
    missing="$(cd "$sim" && LD_LIBRARY_PATH= ldd ./usr/bin/smart-dl-daemon | grep 'not found' || true)"
    rm -rf "$sim"
    if [[ -n "$missing" ]]; then
        echo "FATAL: 安装布局模拟下依赖解析失败（RUNPATH 未覆盖）:" >&2
        echo "$missing" >&2
        exit 1
    fi
    echo "==> stage 自检通过：安装布局 + 空 LD_LIBRARY_PATH 下 ldd 全解析（RUNPATH 生效实锤）"
}

case "$CMD" in
    setup) shift; do_setup ;;
    stage) shift; do_stage "$@" ;;
    *) echo "未知子命令: $CMD" >&2; exit 1 ;;
esac
