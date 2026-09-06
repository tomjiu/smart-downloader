#!/bin/bash
# BT native 链接环境（无 root 前缀方案；Task 35 重建版）
# 配方：apt-get download libtorrent-rasterbar-dev libtorrent-rasterbar2.0t64
#       + dpkg -x 到 ~/bt-native/prefix（boost1.83-dev/openssl/zlib 等系统已有）
# 内核：g++ -std=c++17 -O2 -fPIC -Iffi -I$PREFIX_INC ffi/src/lt_kernel.cpp → ar rcs lt_kernel.lib
# fakevcpkg：xxx.lib 别名 + libxxx.so 实名符号链接喂 build.rs 的 dylib 枚举
export LT_KERNEL_LIB_DIR="$HOME/bt-native/build"
export LT_VCPKG_LIB_DIR="$HOME/bt-native/fakevcpkg"
export LD_LIBRARY_PATH="$HOME/bt-native/prefix/usr/lib/x86_64-linux-gnu:${LD_LIBRARY_PATH:-}"
export SEED_MAIN="$HOME/bt-native/build/seed_main"
export PATH="$HOME/.cargo/bin:$PATH"
