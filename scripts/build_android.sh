#!/usr/bin/env bash
# 安卓 aarch64 交叉编译 smart-dl-daemon（P1-3，关键路径）
# 产物：target/aarch64-linux-android/release/smart-dl-daemon（API 24+，bionic libc）
#
# 用法：
#   ANDROID_NDK_HOME=/path/to/android-ndk-r27c ./scripts/build_android.sh
#   # 缺省在 ANDROID_NDK_HOME / $HOME/Android/Sdk/ndk/* / /opt/android-ndk* 里找
#
# 前置：
#   rustup target add aarch64-linux-android
#   （TLS = rustls，无 openssl 依赖；ring 的 aarch64 asm 由 NDK clang 编译）
set -euo pipefail

TARGET=aarch64-linux-android
API=24

# ---- 定位 NDK ----
find_ndk() {
    if [[ -n "${ANDROID_NDK_HOME:-}" && -d "$ANDROID_NDK_HOME" ]]; then
        echo "$ANDROID_NDK_HOME"; return 0
    fi
    local cand
    for cand in \
        "$HOME/Android/Sdk/ndk/"* \
        /opt/android-ndk* \
        /usr/lib/android-sdk/ndk-bundle; do
        [[ -d "$cand" ]] && { echo "$cand"; return 0; }
    done
    return 1
}

NDK="$(find_ndk)" || {
    echo "错误: 未找到 Android NDK（设 ANDROID_NDK_HOME 后重试）" >&2
    exit 1
}
echo "NDK: $NDK"

TOOLCHAIN="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
CC="$TOOLCHAIN/${TARGET}${API}-clang"
CXX="$TOOLCHAIN/${TARGET}${API}-clang++"
AR="$TOOLCHAIN/llvm-ar"
[[ -x "$CC" ]] || { echo "错误: 缺 $CC"; exit 1; }

# cc-rs 构建脚本（ring 的 aarch64 asm 等）读 CC_<target> / AR_<target> 环境变量，
# 值用带 API 级别的 NDK clang（24+ 覆盖 termux 主流设备）。
export CC_${TARGET//-/_}="$CC"
export CXX_${TARGET//-/_}="$CXX"
export AR_${TARGET//-/_}="$AR"

# ---- cargo 目标链接器配置（生成 .cargo/config.toml，幂等）----
mkdir -p .cargo
cat > .cargo/config.toml <<EOF
# 由 scripts/build_android.sh 生成（本机 NDK 路径，不入仓语义）
[target.aarch64-linux-android]
linker = "$CC"
ar = "$AR"
EOF

# ---- 构建 ----
rustup target add "$TARGET" >/dev/null 2>&1 || true
cargo build --release -p smart-dl-daemon --target "$TARGET" "$@"

BIN="target/$TARGET/release/smart-dl-daemon"
echo
echo "构建完成: $BIN"
file "$BIN" || true
# 产物体积
du -h "$BIN"
