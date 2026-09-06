#!/usr/bin/env bash
# 桌面版 BT 引擎装配（S1-d）—— macOS 段（Apple Silicon）。
#
# libtorrent 用 Homebrew 动态库（libtorrent-rasterbar 2.0.x），FFI 内核用 clang++ 编译。
# macOS 动态库以 install name（绝对路径 /opt/homebrew/...）寻址，打进 .app 前必须：
#   1) 把 otool -L 闭包内所有非系统 dylib 拷入资源目录
#   2) install_name_tool 把 sidecar 与各 dylib 的引用改写为
#      @executable_path/../Resources/native/macos/lib/<basename>
#   3) 改写后的二进制逐一 ad-hoc 重签（install_name_tool 使原签名失效）
#
# tauri v2 实证布局（v0.1.0 .app 解剖）：
#   sidecar → Contents/MacOS/smart-dl-daemon（安装名剥去 -<triple>）
#   资源根 → Contents/Resources/（tauri.macos.conf.json 映射 native/macos/ → native/macos/）
#   @executable_path = Contents/MacOS → ../Resources/native/macos/lib 即目标
#
# 用法：
#   scripts/ci/desktop-bt-macos.sh setup            # brew + 内核静态库 + fakevcpkg 别名 + GITHUB_ENV
#   scripts/ci/desktop-bt-macos.sh stage <SIDECAR>  # 闭包拷贝 + 改写 + 重签 + 自检
set -euo pipefail

CMD="${1:?用法: desktop-bt-macos.sh setup|stage ...}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

SRC_TAURI="$REPO_ROOT/desktop/src-tauri"
NATIVE_DIR="$SRC_TAURI/native/macos/lib"
BREW_PREFIX="$(brew --prefix)"

emit_github_env() {
    if [[ -n "${GITHUB_ENV:-}" ]]; then
        {
            echo "LT_KERNEL_LIB_DIR=$1"
            echo "LT_VCPKG_LIB_DIR=$2"
        } >> "$GITHUB_ENV"
    else
        cat <<EOF
export LT_KERNEL_LIB_DIR="$1"
export LT_VCPKG_LIB_DIR="$2"
EOF
    fi
}

do_setup() {
    brew install libtorrent-rasterbar >/dev/null 2>&1 || brew install libtorrent-rasterbar
    brew list --versions libtorrent-rasterbar
    LT_DYLIB="$(ls "$BREW_PREFIX"/lib/libtorrent-rasterbar.*.dylib | sort -V | tail -1)"
    [[ -f "$LT_DYLIB" ]] || { echo "FATAL: 未找到 libtorrent dylib" >&2; exit 1; }
    echo "==> libtorrent: $LT_DYLIB"

    # FFI 内核（clang++，libc++ ABI，与 brew libtorrent 同链）
    mkdir -p "$HOME/bt-native-macos/lib"
    c++ -std=c++17 -O2 -fPIC -DNDEBUG -I"$REPO_ROOT/ffi" -I"$BREW_PREFIX/include" \
        -c "$REPO_ROOT/ffi/src/lt_kernel.cpp" -o "$HOME/bt-native-macos/lib/lt_kernel.o"
    ar rcs "$HOME/bt-native-macos/lib/liblt_kernel.a" "$HOME/bt-native-macos/lib/lt_kernel.o"
    rm -f "$HOME/bt-native-macos/lib/lt_kernel.o"

    # fakevcpkg 契约：*.lib 别名（build.rs 读 stem）→ -l<stem>（ld 找 lib<stem>.dylib）
    # 库集合从 libtorrent 的 otool -L 自动推导；libc++ 必须显式进链接行
    #（rustc 用 cc 驱动，不会自动带 C++ 运行时——Linux 侧同理靠 stdc++ 别名）
    local fake="$HOME/bt-native-macos/fakevcpkg"
    mkdir -p "$fake"
    # 别名（.lib，build.rs 读 stem）+ 实名符号链接（ld64 在 -L 目录按 lib<stem>.dylib
    # 搜索；/opt/homebrew/lib 不在 ld64 默认搜索路径，必须经符号链接进入 fakevcpkg）。
    # rustc 链接驱动为 cc（clang），C++ 运行时必须手工进链接行
    #（Linux 侧同理靠 stdc++ 别名，见 bt-linux-setup.sh）
    alias_of() { # $1 = dylib 路径/安装名 → 输出 stem（剥 lib 前缀与扩展）
        local base stem
        base="$(basename "$1")"
        stem="${base%.dylib}"
        stem="${stem#lib}"
        printf '%s.lib' "$stem"
    }
    emit_alias() { # $1 = dylib 实文件（绝对路径）
        local resolved
        resolved="$(readlink -f "$1")"
        printf '' > "$fake/$(alias_of "$resolved")"
        ln -sf "$resolved" "$fake/$(basename "$resolved")"
    }
    emit_alias "$LT_DYLIB"
    while read -r dep _rest; do
        case "$dep" in
            "$BREW_PREFIX"/*|/usr/local/*) [[ -f "$dep" ]] && emit_alias "$dep" ;;
            *) continue ;;
        esac
    done < <(otool -L "$LT_DYLIB" | tail -n +2)
    # C++ 运行时（系统 libc++，系统库本身无需符号链接，只需 -lc++ 进链接行）
    printf '' > "$fake/c++.lib"
    echo "    fakevcpkg: $(ls "$fake" | tr '\n' ' ')"

    emit_github_env "$HOME/bt-native-macos/lib" "$fake"
    echo "==> desktop-bt-macos setup 完成"
}

do_stage() {
    local sidecar="${1:?stage 需要 sidecar 路径}"
    [[ -f "$sidecar" ]] || { echo "FATAL: sidecar 不存在: $sidecar" >&2; exit 1; }
    local target_base="@executable_path/../Resources/native/macos/lib"

    rm -rf "$NATIVE_DIR"
    mkdir -p "$NATIVE_DIR"

    # 1) 闭包拷贝：sidecar 引用的 brew 树 dylib（含传递闭包）→ 资源目录
    #    文件名保持 install name 基名（版本号原样），改写目标一一对应
    collect_closure() { # $1 = 二进制/dylib；$2 = 已处理集合文件
        while read -r dep _rest; do
            case "$dep" in
                "$BREW_PREFIX"/*|/usr/local/*) ;;
                *) continue ;;
            esac
            local base resolved
            base="$(basename "$dep")"
            resolved="$(readlink -f "$dep")"
            grep -qxF "$base" "$2" && continue
            echo "$base" >> "$2"
            cp "$resolved" "$NATIVE_DIR/$base"
            collect_closure "$NATIVE_DIR/$base" "$2"   # 传递闭包
        done < <(otool -L "$1" | tail -n +2)
    }
    local closure_list; closure_list="$(mktemp)"
    collect_closure "$sidecar" "$closure_list"
    echo "==> 闭包 $(wc -l < "$closure_list" | tr -d ' ') 个 dylib:"
    ls "$NATIVE_DIR"

    # 2) 改写引用：先各 dylib 内部互引，再 sidecar（ install_name_tool 逐条 -change ）
    local dep orig base
    for dylib in "$NATIVE_DIR"/*.dylib; do
        while read -r dep _rest; do
            case "$dep" in
                "$BREW_PREFIX"/*|/usr/local/*) ;;
                *) continue ;;
            esac
            base="$(basename "$dep")"
            install_name_tool -change "$dep" "$target_base/$base" "$dylib"
        done < <(otool -L "$dylib" | tail -n +2)
    done
    while read -r dep _rest; do
        case "$dep" in
            "$BREW_PREFIX"/*|/usr/local/*) ;;
            *) continue ;;
        esac
        base="$(basename "$dep")"
        install_name_tool -change "$dep" "$target_base/$base" "$sidecar"
    done < <(otool -L "$sidecar" | tail -n +2)

    # 3) ad-hoc 重签（install_name_tool 使原签名失效；bundler 随后整体签名不受影响）
    codesign --force --sign - "$sidecar" 2>/dev/null
    for dylib in "$NATIVE_DIR"/*.dylib; do
        codesign --force --sign - "$dylib" 2>/dev/null
    done

    # 4) 自检：sidecar 与每个 dylib 的引用必须只剩 @executable_path / 系统 / @rpath
    local bad
    bad="$(otool -L "$sidecar" | grep -E '^\s+/opt/homebrew|^\s+/usr/local/' || true)"
    if [[ -n "$bad" ]]; then
        echo "FATAL: sidecar 仍有绝对 brew 引用:" >&2
        echo "$bad" >&2
        exit 1
    fi
    for dylib in "$NATIVE_DIR"/*.dylib; do
        bad="$(otool -L "$dylib" | grep -E '^\s+/opt/homebrew|^\s+/usr/local/' || true)"
        if [[ -n "$bad" ]]; then
            echo "FATAL: $dylib 仍有绝对 brew 引用:" >&2; echo "$bad" >&2; exit 1
        fi
    done
    echo "==> stage 自检通过：brew 绝对引用已全部改写为 @executable_path"
}

case "$CMD" in
    setup) do_setup ;;
    stage) shift; do_stage "$@" ;;
    *) echo "未知子命令: $CMD" >&2; exit 1 ;;
esac
