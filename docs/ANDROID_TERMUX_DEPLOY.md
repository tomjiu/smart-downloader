# Android（Termux）部署指南 —— smart-dl-daemon aarch64

> 收尾计划 P1-4（2026-08-31）。前置：P1-3 交叉编译已落地
> （`scripts/build_android.sh`，ELF aarch64 / Android 24+ / bionic linker64）。
> 本指南给出两条部署路线、身份档位选择理由、功能矩阵与已知边界。

## 0. 为什么安卓可以用「一台 PC」的身份

服务端视角的平台身份 = 参数集，不是硬件：

| 身份要素 | 来源 | 安卓上的取值 |
|---------|------|-------------|
| client_id | 编译期常量（`tier.rs` 档位表） | `web` 档 `Xqp0kJBXWhwaTpB6` |
| device_id | 软件生成，随 AuthState 落盘 | 本地随机 32 位 hex，无硬件证明 |
| token | RFC 8628 设备码流 | 与 PC 同一登录协议 |

安卓上跑的 daemon 对云端就是**一台装了网页版客户端的 PC**：无 90120 限制、
无每日 3 次配额闸门（那是 docker/pan-cli 档的云端裁剪，A6_PREP §8），
下载直连 CDN 不经云端闸门。唯一例外是 L3 私有加速（本仓永不实现，红线）。

## 1. 路线 A：部署预编译产物（推荐，5 分钟）

```bash
# 1) 传二进制到手机（adb / termux-openssl-get / 网盘均可）
#    产物名：smart-dl-daemon-android-aarch64（13MB，未 strip）
adb push smart-dl-daemon-android-aarch64 /data/local/tmp/
# 或在 termux 里直接 curl 你的下载源

# 2) Termux 内安装
pkg install -y tsu   # 可选，仅 root 拷贝需要
mkdir -p ~/bin
cp /data/local/tmp/smart-dl-daemon-android-aarch64 ~/bin/smart-dl-daemon
chmod +x ~/bin/smart-dl-daemon

# 3) 验证可执行
~/bin/smart-dl-daemon --help
```

> 产物是 bionic 动态链接（interpreter `/system/bin/linker64`），**不是**
> termux 自己的 bionic 前缀（`/data/data/com.termux/files/`）。直接运行若报
> `library liblog.so not found`，用 `termux-fix-shebang` 无效（非脚本问题），
> 改用路线 B 或静态重链（`RUSTFLAGS="-C target-feature=+crt-static"`，
> 需接受 getaddrinfo 线程局部性损失）。

## 2. 路线 B：Termux 端上原生构建（30 分钟，产物与 termux libc 同前缀）

```bash
pkg update
pkg install -y rust git binutils
git clone <repo-url> smart-downloader && cd smart-downloader

# Termux 自带 clang 就是本机编译器，无交叉，直接 build（release）
cargo build --release -p smart-dl-daemon
# 产物：target/release/smart-dl-daemon
```

路线 B 产物链接 termux 的 bionic，无路线 A 的动态库前缀问题；代价是端上
编译时长（中端机型 10-20 分钟）与 ~1.5GB 存储。

## 3. 登录（身份档位）

```bash
# web 档（推荐：PC 同款身份，能力面最全）
./smart-dl-daemon xunlei-login --tier web --qr
# 登录态写入 ./xunlei_auth.json

# nas 档（群晖同款身份；多账号/多档并存实验用）
./smart-dl-daemon xunlei-login --tier nas --qr
# 登录态写入 ./xunlei_auth_nas.json（独立 device_id，与 web 档不互踢）
```

扫码用手机迅雷 App（本机就是手机，直接「扫一扫」屏幕上另一个设备展示的
二维码，或把授权页链接发到桌面浏览器打开后再扫）。

## 4. 启动 daemon

```toml
# ~/smart-dl.toml
[server]
addr = "127.0.0.1:8787"     # 只听本机；需要局域网访问改 0.0.0.0（自担风险）

[download]
dest_root = "/data/data/com.termux/files/home/downloads"

[provider]
enabled = true

[provider_xunlei]
enabled = true
tier = "web"                 # P1-1 档位；env SMART_DL_XUNLEI_TIER 可覆盖
token_path = "/data/data/com.termux/files/home/xunlei_auth.json"

[bt]
enabled = false              # 安卓不编 BT 引擎（见 §6 边界）
```

```bash
./smart-dl-daemon serve --config ~/smart-dl.toml
```

常驻：`pkg install termux-services` 后 `sv-enable smart-dl`，或用
`termux-wake-lock` + nohup。Android 12+ 的 phantom process killer 可能杀
后台进程，`adb shell settings put global settings_enable_monitor_phantom_procs false`
（需 adb，一次即可）。

## 5. 功能矩阵（安卓）

| 功能 | 状态 | 说明 |
|------|------|------|
| HTTP/HTTPS 多线程下载 | ✅ | httpdl 动态分片，rustls TLS |
| 迅雷云盘离线提交（磁力/HTTP） | ✅ | provider_xunlei，web/nas 档 |
| 离线完成自动取直链落盘 | ✅ | submit→status→resolve 全链 |
| 断点续传/限速/代理 | ✅ | 与 Linux 桌面同代码 |
| FTP | ✅ | feature `ftp` |
| BT/磁力本地 BT 引擎 | ❌ | libtorrent 交叉成本高，磁力走云端离线替代 |
| 迅雷 SDK 原生引擎（Win 同款） | ❌ | 平台闭源 DLL，仅 Windows |
| L3 私有加速 | ❌（永不） | 收尾红线 |

## 6. 已知边界

1. **BT 引擎不在安卓**：默认构建无 `bt` feature（CI 基线同构）。磁力链接
   走迅雷云盘离线（`provider_xunlei`）拿到直链后再落盘，体验等效。
2. **通知/前台服务**：daemon 是纯 HTTP 服务，无 Android 组件封装；后台
   存活靠 termux-wake-lock，息屏长任务建议插电。
3. **存储路径**：termux 只能写自己的 app 目录与 `termux-setup-storage`
   授权的共享目录；`dest_root` 必须落在可写路径。
4. **TLS**：rustls（P1-3 切换），无 openssl 依赖；ring 在 aarch64 有
   asm 加速。系统时间不准会导致 TLS 握手失败，报错先看时间。
