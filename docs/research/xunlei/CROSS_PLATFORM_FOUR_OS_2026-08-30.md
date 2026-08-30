# 附录 E：跨四端支持（Win/macOS/Linux/Android）可行性定案 —— NAS 引擎实测突破

> 生成：2026-08-30。输入情报：用户提供的两个新原材料——迅雷官方安卓下载 API
> （api-hezi.xunlei.com → hezi_v1.8.14.4_320_release.apk，22MB）与 cnk3x/xunlei
> 项目（MIT，Go 实现，群晖套件容器化封装）。
> 本附录结论推翻 D.2 中「Linux 无原材料」的判定，并新增 Android 侧实证。

---

## E.1 新原材料档案

### E.1.1 迅雷官方 NAS 版 SPK（本附录核心，Linux 引擎来源）

| 项 | 值 |
|---|---|
| 下载源 | `https://down.sandai.net/nas/nasxunlei-DSM7-x86_64.spk`（26MB）/ `nasxunlei-DSM7-armv8.spk`（17MB），与 Windows 版同域名 |
| 容器格式 | POSIX tar → 内含 `package.tgz`（xz 压缩 tar） |
| 包名/版本 | `pan-xunlei-com`，x86_64 版 3.23.5-0814080017（2025-08-14 编译）；arm64 版 3.1.10 |
| 关键产物 | `bin/bin/xunlei-pan-cli.{ver}.{arch}`（主引擎 62.7MB）、`xunlei-pan-cli-launcher.{arch}`（Go 启动器 19.7MB）、`ui/index.cgi`（Go，web 入口） |
| 动态依赖 | 仅 `libm/libdl/libstdc++/libpthread/libgcc_s/libc` —— 任意 x86_64/aarch64 Linux 可跑，无 glibc 版本魔咒 |
| 安装脚本 | `scripts/service-setup` 完整暴露启动协议（见 E.2.2） |

### E.1.2 hezi 安卓 APK（v1.8.14.4_320）

- 直链：`https://api-hezi.xunlei.com/api/v1/download/an`（302 → `oss-nas-ssl.xunlei.com/hezi/apk/hezi_v1.8.14.4_320_release.apk`）。
- 包名 `com.xunlei.hezi`；native 层**无下载引擎**：全部为辅助库（梆梆加固壳 libshell-super/libshella-4.6.2.2、SMB 客户端 libsmb2/libsmb_client、aplayer 播放、Bugly、阿里号码认证、zbar）。
- 定性：**盒子/NAS 管理端（遥控器）**，非引擎载体。对「借引擎」路线无直接价值；其价值在 E.1.1 路线下作为「引擎远程控制 UI」的参照实现。
- assets 保留加固资产（t86/t86_64/tarm、o0oooOO0ooOo.dat），纯静态可挖部分已归档。

### E.1.3 cnk3x/xunlei（MIT，Go）

- 核心贡献：验证「迅雷官方 NAS 版可在非群晖 Linux 上跑」，并给出群晖模拟最小集——`/etc/synoinfo.conf` 三行（platform_name/synobios/unique）+ `authenticate.cgi` 桩（实测反汇编：C 程序，输出 `Content-Type: text/plain` + `admin` 两行）+ 环境变量组。
- 我们的独立实测（E.2.4）表明：**引擎自带 docker 平台检测，纯容器环境无需任何群晖模拟**，比 cnk3x 的路径更短。

---

## E.2 xllite 引擎深度档案（主引擎 pan-cli，内部代号 xllite）

### E.2.1 身份与云端端点（字符串实证）

| 项 | 值 |
|---|---|
| 官方源码路径（Go PN 泄漏） | `gitlab.xunlei.cn/xlppc/pan-cli`（pkg/platformdetect、pkg/commands、cmd/pan-cli） |
| 账号服务 | `https://xluser-ssl.xunlei.com`（与 Windows 侧 L1 云登录同族） |
| 云盘 API | `https://api-pan.xunlei.com`；手雷/迅雷 API `api-shoulei-ssl.xunlei.com` |
| 试用高速 | **`/device/v1/try_speed/get_info` 端点内嵌**（与 Windows 侧 TrySpeed 试用线索对齐） |
| 控制面 | gin HTTP：`DriveListen`（默认 127.0.0.1:5050，TCP）/ `LauncherListen`（5051）；unix socket 双轨可选 |
| 内部服务 | launcher↔引擎 gRPC（`drive.*` proto 族：ImportDownloadRecord/CheckImportDownload/UserFileList/AppConfig…） |
| 存储层 | SQLite + storm/bolt；VFS：os/smb/alidrive/webdav 四实现注册 |
| BT/P2P | 内嵌（ERR_BT_*、ERR_P2P_* 命令字族与 Windows 版同源风格） |

### E.2.2 启动协议（SPK service-setup + config.init dump 反推，已实测）

```sh
export DriveListen=127.0.0.1:5050   # 主 gin HTTP（TCP 可用，实测）
export LauncherListen=127.0.0.1:5051
export ConfigPath=<data_dir>        # 配置/库根
export DownloadPATH=<download_dir>  # 下载根（多目录冒号分隔）
export HOME=<data_dir>/.drive       # 引擎用户数据
export GIN_MODE=release
unset PLATFORM                      # ⚠ 外层脏 PLATFORM（如 lexar:xxx）会触发 "not exist name"
./bin/xunlei-pan-cli.<ver>.<arch> -pid <work>/engine.pid
```

配置键全集（config.init dump）：`AllowCustomPlatform:false`、`SingleTaskMaxMB:100`、
`OverallShareMaxMB:200`、`IdleMemoryUsageLimit:419430400`、`RefreshTokenDuration:1h`、
`PluginTokenDuration:5h`、`HostXluser/HostApiPan/HostShoulei/HostHighSpeedFlow` 四端点、
`DriveAuthorizationTokenPath`（token 预置口，见 E.2.3）。

### E.2.3 登录：OAuth 设备码（RFC 8628，实测抓到全参）

引擎启动即向账号服务发起设备码请求（drive/auth.go 日志实证）：

```text
POST https://xluser-ssl.xunlei.com/v1/auth/device/code
{"client_id":"X9ibISwpIp8jQ4Ya","client_secret":"BlPF2z7HEeutzH4t6zyjLw",
 "scope":"pan user profile sso offline pan/xunlei/share/create"}
→ {"device_code":"…","verification_uri_complete":"https://xluser-ssl.xunlei.com/__/auth/device/?...&user_code=…","user_code":"…"}
→ 轮询 token（RefreshTokenDuration 1h 刷新）
```

- `client_id=X9ibISwpIp8jQ4Ya` 为 docker 平台客户端（与 E.2.4 平台检测结果一致），`client_secret` 同为引擎内嵌常量。
- **token 预置路径**：`DriveAuthorizationTokenPath` 指向的文件若有效可跳过扫码（格式校准=假设区实测项，候选=L1 云登录 OAuth token JSON 同构）。
- 无 TTY 且无 token 时 `DoLogin`（login.go:48）因 `open /dev/tty` panic——生产部署需先在有终端的环境完成一次扫码，或走 token 预置。

### E.2.4 平台检测与特权集（实测）

容器内启动日志：

```text
> DetectPlatform err:env PLATFORM=lexar:… not exist name:lexar   ← PLATFORM 环境变量参与检测
> detect platform: docker … labels:
  [disableLauncherAuth withQrcodeLogin withOtherAuthLogin withPreviewPrivilege
   withPlugin allowUseConfigPathAsCachePath driveApiAllowLocalToken
   withInstallLocalPlugin withHighSpeedFlowCtrl]
```

- `disableLauncherAuth`：免群晖 authenticate.cgi 认证；`withQrcodeLogin`：设备码扫码；
  `driveApiAllowLocalToken`：本地 token 放行；`withHighSpeedFlowCtrl`：**会员高速流控开**。
- 引擎内嵌平台白名单（`PLATFORM` 环境变量可指定，`群晖`/`docker` 等在列；未知名拒绝）。
- 扫码跳转页（GetQrControlUrl）：`https://pan.xunlei.com/yc/?client_id=…&platform=docker&privilege=PLATFORM_DOCKER&user_code=…`。

### E.2.5 本环境实测记录（Debian 13 容器，非 root，无群晖模拟）

| 步骤 | 结果 |
|---|---|
| 直接执行 `xunlei-pan-cli.3.23.5.amd64 --help` | ✅ 初始化全通：插件管理/内部 API/DB/VFS 注册，横幅「欢迎使用xllite 3.23.5 6bdafe7c」，随后正常退出 |
| 按启动协议拉起（无 TTY） | ✅ 设备码请求成功（拿到 device_code/user_code）→ ❌ /dev/tty panic（预期内，见 E.2.3） |
| pty 伪终端拉起 | ✅ 进程稳定存活 60s+ 等待扫码（日志无错误）→ 登录门前 HTTP 不监听（启动顺序：登录成功后才起 gin） |
| 判定 | **Linux 端=已验证可行**；剩余门槛仅「首次登录」（扫码或 token 预置），属账号侧而非工程侧 |

试跑脚本：`scripts/research/xunlei/nas_engine_run.sh`（直跑）与 `nas_engine_pty_test.py`（pty 版）。

---

## E.3 四端可行性矩阵（定案）

| 端 | 引擎来源 | 状态 | 说明 |
|---|---|---|---|
| **Windows** | DownloadSDK.dll 全家桶（xunlei-ffi loader） | ✅ 已交付 | 免登录匿名 BT + 带身份/加速证书注入；依赖面见附录 D |
| **Linux x86_64** | 官方 NAS 版 pan-cli 3.23.5（xllite） | ✅ **本次实证可行** | 非群晖/非 root/无模拟即可跑；feature `nas` 托管模块已入库（E.4） |
| **Linux aarch64**（NAS/树莓派） | 官方 NAS 版 pan-cli 3.1.10 arm64 | ✅ 同上（未实测，依赖面同族） | arm64 引擎已取回归档 |
| **macOS** | 官方 mac 包（既有 60% 逆向档案 macos_abi_reverse.md） | 🟡 等真机/窗口 | 框架结构已归档；无新增原材料，维持原计划 |
| **Android 原生** | hezi 管理端=无引擎；手机版加固壳 | ❌ 借引擎不可行 | 定性变更：hezi 是遥控器不是引擎；原生路线=自研引擎+云协议（主线即跨平台） |
| **Android 容器**（Termux/proot） | 同 Linux aarch64（pan-cli arm64） | 🟡 高可行待实测 | Android 本质 Linux 内核；proot 里跑 arm64 引擎是既定可行架构（同 pan-xunlei-com 在 aarch64 群晖跑），待真机窗口验证 |

> 结论：用户「跨四端」诉求中 **win/linux/android 三优先端全部有路**：
> win=已交付；linux=本次实证+托管代码；android=容器路线（arm64 引擎）或自研主线
> （主线 95% 纯 Rust 本就覆盖 Android NDK 目标）。macOS 维持等真机。

---

## E.4 实施产物（本轮入库）

### E.4.1 `crates/daemon/src/nas.rs`（feature `nas`，Linux-only）

- `NasManager`：`install()`（SPK tar→xz 解包，系统 tar 零新依赖）/ `start()`（启动协议 env 组装+脏 PLATFORM 清理）/ `stop()` / `status()`（/proc 存活+HTTP 探活）。
- `nas_proxy`：`/nas/*` → `DriveListen` 透明反代（method/headers/body 透传，502 带引导信息）。
- `put_auth_token()`：token 预置文件写入（L1 云登录产物 → 引擎免扫码，格式校准待实测）。
- 管理端点：`POST /nas/install|/nas/start|/nas/stop|/nas/token`、`GET /nas/status`；全局单例经 `SD_NAS_SPK/SD_NAS_WORK/SD_NAS_DOWNLOADS/SD_NAS_DRIVE_LISTEN` 环境变量配置。
- 三配置编译零警告：默认 / `--features nas` / `--features nas,xunlei-import`。
- **标注**：登录门后 API 全链路 UNTESTED（等待真实扫码/token），全部对齐 D.3 假设区纪律。

### E.4.2 归档物料

- `scripts/research/xunlei/extracted/cross-platform/`：hezi APK 及解包件、两架构 SPK 及解包件、cnk3x 源码（作为独立第三方参考，MIT）。
- 试跑脚本：`nas_engine_run.sh`、`nas_engine_pty_test.py`。

---

## E.5 假设区增量（并入 D.3 清单）

| # | 假设 | 校准手段 |
|---|---|---|
| 8 | NAS 引擎 token 预置文件格式（DriveAuthorizationTokenPath）= xluser OAuth JSON（L1 同构） | L1 登录 token 投喂实测 |
| 9 | `DriveListen` TCP 面 API 形状（gin 路由前缀 /device/v1/*）与 drive gRPC 语义 | 扫码登录后抓取（脚本就绪） |
| 10 | try_speed get_info/apply 在 NAS 引擎的参数面（与 Windows VIP 通道对照） | 同上 + 试用票据 |
| 11 | Android proot 内 aarch64 引擎可跑性（glibc vs musl、/proc 依赖） | 真机 Termux 窗口 |
| 12 | arm64 引擎 3.1.10 与 x86 3.23.5 的协议一致性 | 双端同 token 对拍 |

---

## E.6 与既有结论的衔接

- D.2「Linux 无原材料」**作废**，更新为「Linux 原材料=官方 NAS 版（down.sandai.net/nas/），已实证」。
- D.1 分层图在 L3 借引擎层新增一条：`daemon[nas] → pan-cli(xllite) → api-pan/xluser`。
- 假设区计数：7 → 12（+E.5 五项）。
- 「#1 PHub 不可做」「#6 SDK 内登录不存在」结论不变（本附录情报反而进一步佐证：官方 Linux 引擎的账号面也全部收敛到 xluser-ssl OAuth，SDK 内无独立登录）。
