#!/usr/bin/env bash
# BT 集成测试 native 环境搭建（libtorrent 2.0.x，Linux）。
#
# 背景：ffi/src 按 libtorrent 2.1 API 编写，Linux 发行版系统包均为 2.0.x
# （Debian trixie 2.0.11 / Ubuntu noble 2.0.10），版本兼容由源内
# `#if LIBTORRENT_VERSION_NUM >= 20100` 守卫解决（见 lt_kernel.cpp / seed_main.cpp）。
# 本脚本负责其余三件事：
#   1) 准备 libtorrent 头文件与动态库（rootful 直接 apt 安装；no-root 解包到本地前缀）
#   2) 编译 FFI 内核静态库 liblt_kernel.a 与 e2e seeder seed_main
#   3) 仿真 build.rs 的 vcpkg 契约（LT_VCPKG_LIB_DIR：*.lib 别名 + lib*.so 实名符号链接）
#
# 用法：
#   scripts/ci/bt-linux-setup.sh <DEST>              # rootful：apt 安装系统包（GitHub Actions）
#   scripts/ci/bt-linux-setup.sh --no-root <DEST>    # 无 root：apt-get download + dpkg -x 本地前缀
#
# 产物布局：
#   <DEST>/lib/liblt_kernel.a    FFI 内核静态库          → LT_KERNEL_LIB_DIR=<DEST>/lib
#   <DEST>/lib/seed_main         e2e seeder 可执行       → SEED_MAIN=<DEST>/lib/seed_main
#   <DEST>/fakevcpkg/            vcpkg 契约仿真          → LT_VCPKG_LIB_DIR=<DEST>/fakevcpkg
#   <DEST>/prefix/               （仅 no-root）解包前缀
#   <DEST>/env.sh                环境变量快照（source 后可直接 cargo test）
#
# 验证（宿主已装 rust）：
#   source <DEST>/env.sh
#   cargo test -p smart-dl-btcore
#   cargo test -p smart-dl-daemon --features bt
set -euo pipefail

NO_ROOT=0
if [[ "${1:-}" == "--no-root" ]]; then
    NO_ROOT=1
    shift
fi
DEST="${1:?用法: bt-linux-setup.sh [--no-root] <DEST>}"
DEST="$(mkdir -p "$DEST" && cd "$DEST" && pwd)"
LIB_DIR="$DEST/lib"
FAKEVCPKG="$DEST/fakevcpkg"
PREFIX="$DEST/prefix"
mkdir -p "$LIB_DIR" "$FAKEVCPKG"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FFI_SRC="$REPO_ROOT/ffi/src"

# ---------------------------------------------------------------------------
# 1) libtorrent 头文件 + 动态库就位
# ---------------------------------------------------------------------------
download_deb() {
    # apt-get download 递归依赖集到 $1（无 root 的包源）
    local dir="$1"
    mkdir -p "$dir"
    (cd "$dir"
    local pkgs
    mapfile -t pkgs < <(apt-cache depends --recurse --no-recommends --no-suggests \
        --no-conflicts --no-breaks --no-replaces --no-enhances \
        libtorrent-rasterbar-dev 2>/dev/null \
        | grep -oE '^[[:space:]]*[a-z0-9][a-z0-9.+-]*$' | sed 's/^[[:space:]]*//' | sort -u)
    apt-get download "${pkgs[@]}")
}

if [[ "$NO_ROOT" == "1" ]]; then
    echo "==> [no-root] 下载并解包 libtorrent-rasterbar-dev 依赖集"
    mkdir -p "$PREFIX"
    download_deb "$DEST/debs"
    for deb in "$DEST/debs"/*.deb; do
        dpkg -x "$deb" "$PREFIX"
    done
    SYS_LIB="$(dirname "$(find "$PREFIX" -name 'libtorrent-rasterbar.so.*' | head -1)")"
    INC_FLAGS="-I$PREFIX/usr/include"
    export LD_LIBRARY_PATH="$SYS_LIB:${LD_LIBRARY_PATH:-}"
else
    echo "==> [rootful] apt 安装 libtorrent-rasterbar-dev"
    apt_do="apt-get"
    if command -v sudo >/dev/null 2>&1; then apt_do="sudo apt-get"; fi
    $apt_do update -y
    $apt_do install -y --no-install-recommends libtorrent-rasterbar-dev g++
    SYS_LIB="/usr/lib/$( (g++ -dumpmachine 2>/dev/null || echo x86_64-linux-gnu) )"
    [[ -d "$SYS_LIB" ]] || SYS_LIB="/usr/lib/x86_64-linux-gnu"
    INC_FLAGS=""
fi

LT_SO=""
for cand in "$SYS_LIB"/libtorrent-rasterbar.so.*; do
    if [[ -e "$cand" ]]; then LT_SO="$(readlink -f "$cand")"; break; fi
done
[[ -e "$LT_SO" ]] || { echo "FATAL: 未找到 libtorrent-rasterbar 动态库 ($SYS_LIB)" >&2; exit 1; }
echo "==> libtorrent: $LT_SO ($(grep -oE '[0-9]+\.[0-9]+\.[0-9]+' "$SYS_LIB"/pkgconfig/libtorrent-rasterbar.pc 2>/dev/null | head -1 || echo '版本未知'))"

# ---------------------------------------------------------------------------
# 2) fakevcpkg：build.rs 契约（每个 *.lib → cargo:rustc-link-lib=dylib=<stem>）
#    库集合从 libtorrent 动态库的 NEEDED 依赖自动推导，不硬编码。
# ---------------------------------------------------------------------------
echo "==> 生成 fakevcpkg（NEEDED 依赖推导）"
emit_alias() {
    local so_file="$1"  # 实际动态库文件（libX.so.Y[.Z]，绝对路径）
    local base stem soname
    base="$(basename "$so_file")"    # libX.so.Y.Z
    stem="${base%%.so*}"             # libX
    soname="${stem}.so"              # libX.so
    stem="${stem#lib}"               # X（build.rs 的 link-lib=dylib=<stem>）
    # .lib 别名（build.rs 读 stem）+ libX.so 实名符号链接（ld 搜索用）
    printf '' > "$FAKEVCPKG/${stem}.lib"
    ln -sf "$so_file" "$FAKEVCPKG/$soname"
}
# 主库自身
emit_alias "$LT_SO"
# NEEDED 依赖（boost/ssl/… ）：ld 解析 libtorrent NEEDED 时需要能在 -L 路径找到
mapfile -t needed_list < <(objdump -p "$LT_SO" | awk '/NEEDED/ {print $2}')
for needed in "${needed_list[@]}"; do
    case "$needed" in
        libtorrent-rasterbar*) continue ;;
        # 注意：libstdc++ 不能排除 —— rustc 链接传 -nodefaultlibs，C++ 运行时
        # 不会被自动补上，必须经 fakevcpkg 的 stdc++.lib → -lstdc++ 进入链接行
        libc.so*|libm.so*|libgcc_s.so*|libpthread.so*|librt.so*|ld-linux*) continue ;;
    esac
    if [[ -e "$SYS_LIB/$needed" || -L "$SYS_LIB/$needed" ]]; then
        emit_alias "$(readlink -f "$SYS_LIB/$needed")"
    fi
done
echo "    fakevcpkg: $(ls "$FAKEVCPKG" | tr '\n' ' ')"

# ---------------------------------------------------------------------------
# 3) 编译 FFI 内核静态库 + e2e seeder
# ---------------------------------------------------------------------------
echo "==> 编译 lt_kernel（静态库）"
g++ -std=c++17 -O2 -fPIC -DNDEBUG $INC_FLAGS -I"$REPO_ROOT/ffi" \
    -c "$FFI_SRC/lt_kernel.cpp" -o "$LIB_DIR/lt_kernel.o"
ar rcs "$LIB_DIR/liblt_kernel.a" "$LIB_DIR/lt_kernel.o"
rm -f "$LIB_DIR/lt_kernel.o"

echo "==> 编译 seed_main（e2e seeder）"
g++ -std=c++17 -O2 -DNDEBUG $INC_FLAGS -I"$REPO_ROOT/ffi" \
    "$FFI_SRC/seed_main.cpp" -o "$LIB_DIR/seed_main" \
    -L"$FAKEVCPKG" -ltorrent-rasterbar

echo "==> 编译自检：ldd seed_main 解析"
( export LD_LIBRARY_PATH="${SYS_LIB}:${LD_LIBRARY_PATH:-}"; ldd "$LIB_DIR/seed_main" | grep -E 'not found' && {
    echo "FATAL: seed_main 依赖解析失败" >&2; exit 1; } || true )

# ---------------------------------------------------------------------------
# 4) 环境变量快照
# ---------------------------------------------------------------------------
# rustc 1.98 链接布局规避（no-root 本地前缀场景实测必踩，rootful 无害冗余）：
# 1) 剥 -fuse-ld=lld（系统无 ld.lld → collect2 "cannot find 'ld'"）
# 2) 剥 -B<...>/gcc-ld（rustc 强制 rust-lld）→ g++ 默认 GNU ld（传统 -L 语义）
# 3) 剥 -nodefaultlibs（g++ 默认 C++ 闭包尾部兜底 liblt_kernel.a 的 DSO missing）
# 4) 本地前缀 -L 注入首个 -l 之前（rustc 1.98 把 native -L 排在 -l 之后，
#    ld/lld 单遍扫描解析失败 → unable to find library -ltorrent-rasterbar）
cat > "$DEST/linker-wrap.sh" <<WEOF
#!/bin/bash
out=(); injected=0
for a in "\$@"; do
  case "\$a" in
    -fuse-ld=*|-nodefaultlibs) continue ;;
    -B*) continue ;;
  esac
  if [[ "\$a" == -l* && \$injected -eq 0 ]]; then
    out+=(-L"$LIB_DIR" -L"$FAKEVCPKG")
    injected=1
  fi
  out+=("\$a")
done
exec g++ "\${out[@]}"
WEOF
chmod +x "$DEST/linker-wrap.sh"

cat > "$DEST/env.sh" <<EOF
# bt-linux-setup.sh 产物 —— source 本文件后可直接跑 BT 集成测试
export LT_KERNEL_LIB_DIR="$LIB_DIR"
export LT_VCPKG_LIB_DIR="$FAKEVCPKG"
export SEED_MAIN="$LIB_DIR/seed_main"
export LD_LIBRARY_PATH="$SYS_LIB:\${LD_LIBRARY_PATH:-}"
# 新版 rustc（>=1.90）默认 linker=rust-lld，不自动链 C++ 运行时；
# FFI 静态库含 C++ 符号 → g++ 驱动 wrapper（自动带 libstdc++/libgcc，
# 并规避 rustc 1.98 的 -L/-l 排序与 lld 强制问题，见 linker-wrap.sh 头注）
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="$DEST/linker-wrap.sh"
EOF

cat <<EOF

============================================================
native 环境就绪: $DEST
  LT_KERNEL_LIB_DIR=$LIB_DIR
  LT_VCPKG_LIB_DIR=$FAKEVCPKG
  SEED_MAIN=$LIB_DIR/seed_main

验证：
  source $DEST/env.sh
  cargo test -p smart-dl-btcore
  cargo test -p smart-dl-daemon --features bt
============================================================
EOF
