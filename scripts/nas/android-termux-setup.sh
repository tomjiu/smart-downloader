#!/data/data/com.termux/files/usr/bin/bash
# =============================================================
# smart-dl NAS 引擎 · Android Termux 一键部署（B-4）
# 目标：在 Android（aarch64）上跑迅雷官方 arm64 引擎 xunlei-pan-cli
# 原理：引擎为 glibc 动态链接 ELF，Termux 原生 Bionic 无法加载，
#       故经 proot-distro(Debian) 提供标准 Linux 用户态（附录 E 假设区 #11）。
# 用法（Termux 内）：
#   pkg install -y curl bash && bash android-termux-setup.sh
# 可选环境变量：SPK_URL（armv8 SPK 直链）/ SPK_FILE（本机 spk 路径）
# =============================================================
set -euo pipefail

SPK_URL="${SPK_URL:-https://down.sandai.net/nas/nasxunlei-DSM7-armv8.spk}"
SPK_FILE="${SPK_FILE:-}"
WORK="${HOME}/nas-engine"          # Termux 侧根（proot 挂载回去）
DISTRO="debian"

say() { echo -e "\033[1;32m[setup]\033[0m $*"; }

# 0. 环境自检
if ! uname -o 2>/dev/null | grep -qi android; then
  say "⚠ 未检测到 Android（uname -o != *Android*）。本脚本面向 Termux；纯 Linux 请用 daemon feature nas。"
fi
[ "$(uname -m)" = "aarch64" ] || say "⚠ 非 aarch64（$(uname -m)）；SPK 默认下载 armv8 包，其他架构请改 SPK_URL。"

# 1. 依赖
say "安装 proot-distro / 依赖…"
pkg install -y proot-distro tar xz-utils curl >/dev/null 2>&1 || pkg install -y proot-distro tar xz-utils curl

# 2. Debian 发行版
if ! proot-distro list 2>/dev/null | grep -q "^${DISTRO}\$"; then
  say "安装 proot Debian（约 30MB，首次较慢）…"
  proot-distro install "$DISTRO"
fi

# 3. 取 SPK（宿主侧下载，进 proot 后解包）
mkdir -p "${WORK}/spk"
if [ -n "${SPK_FILE}" ]; then
  cp -f "${SPK_FILE}" "${WORK}/spk/engine.spk"
else
  say "下载 armv8 SPK…"
  curl -L --fail -o "${WORK}/spk/engine.spk" "${SPK_URL}"
fi
du -h "${WORK}/spk/engine.spk"

# 4. 写 proot 内引导脚本（解包+启动协议，与 Linux 侧一致）
mkdir -p "${WORK}/data/.drive" "${WORK}/downloads" "${WORK}/logs"
cat > "${WORK}/inside.sh" <<'EOS'
#!/bin/bash
set -euo pipefail
WORK="$1"
DEST="${WORK}/target"
mkdir -p "$DEST"
[ -f "$DEST/bin/bin/version" ] || {
  echo "[inside] 解包 SPK…"
  tar -xf "${WORK}/spk/engine.spk" -C "$DEST"
  tar -xJf "$DEST/package.tgz" -C "$DEST"
}
VER=$(cat "$DEST/bin/bin/version")
ARCH=$(uname -m); [ "$ARCH" = "aarch64" ] && XLARCH=arm64 || XLARCH=amd64
ENGINE="$DEST/bin/bin/xunlei-pan-cli.${VER}.${XLARCH}"
[ -x "$ENGINE" ] || { echo "[!] 引擎不存在: $ENGINE"; exit 1; }
unset PLATFORM
export DriveListen=127.0.0.1:5050 LauncherListen=127.0.0.1:5051
export ConfigPath="${WORK}/data" DownloadPATH="${WORK}/downloads"
export HOME="${WORK}/data/.drive" GIN_MODE=release
cd "$(dirname "$ENGINE")"
echo "[inside] 启动 xllite ${VER}（首次需扫码，见日志）…"
exec "$ENGINE" -pid "${WORK}/engine.pid"
EOS
chmod +x "${WORK}/inside.sh"

# 5. 生成宿主侧启动/停止包装（登录日志看 tail）
cat > "${WORK}/start.sh" <<EOS
#!/data/data/com.termux/files/usr/bin/bash
proot-distro login ${DISTRO} --bind ${WORK}:/nas-engine -- /nas-engine/inside.sh /nas-engine \\
  >> ${WORK}/logs/engine.log 2>&1 &
echo \$! > ${WORK}/wrapper.pid
echo "已后台启动（pid=\$(cat ${WORK}/wrapper.pid)）。日志: tail -f ${WORK}/logs/engine.log"
echo "扫码完成后 web 面板: http://127.0.0.1:5050/"
EOS
cat > "${WORK}/stop.sh" <<EOS
#!/data/data/com.termux/files/usr/bin/bash
proot-distro login ${DISTRO} -- /bin/kill -TERM \$(cat ${WORK}/engine.pid 2>/dev/null) 2>/dev/null || true
echo "已发送停止信号"
EOS
chmod +x "${WORK}/start.sh" "${WORK}/stop.sh"

say "部署完成。"
echo "  启动:  bash ~/nas-engine/start.sh"
echo "  日志:  tail -f ~/nas-engine/logs/engine.log   （首次启动日志里有扫码短链）"
echo "  停止:  bash ~/nas-engine/stop.sh"
echo "  面板:  http://127.0.0.1:5050/  （登录成功后）"
echo ""
say "提示：首次启动需在日志中找设备码短链，用迅雷 App 扫码或复制到手机浏览器授权；"
say "      token 落盘后（${WORK}/data/.drive）后续启动免扫码。"
say "      Android 术语：Android 11+ 限制下如遇 /proc 读取异常，改用 proot-distro login --fix-low-ports 或 Termux:API 提权路径（假设区 #11 待实测项）。"
