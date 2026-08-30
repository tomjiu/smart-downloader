# 项目交接状态总报告

> 生成时间：2026-08-29
> 仓库：https://github.com/tomjiu/smart-downloader
> 用途：让云/其他人接手时，一眼看清四条主线的现状、卡点、下一步

---

## 一、通用下载器主能力开发（smart-dl daemon + core）

### 已实现

| 能力 | 状态 | 说明 |
|------|------|------|
| HTTP/HTTPS 下载 | ✅ | `crates/httpdl`，多连接、Range、backup_url/backup_md5 备用源 |
| FTP 下载 | ✅ | `crates/httpdl/src/protocol/ftp.rs`，含目录遍历 |
| BT 引擎（libtorrent 基座） | ✅ | `crates/btcore`，M0  matured，支持 magnet/torrent/resume |
| 任务生命周期 | ✅ | add/pause/resume/remove/list/status，daemon HTTP API |
| 迅雷链接解码 | ✅ | `thunder://` / `qqdl://` 解码为 HTTP URL |
| 迅雷网盘分享解析 | ✅ | `pan.xunlei.com/s/xxx?pwd=yyy` 解析 |
| 配置系统 | ✅ | TOML 配置，BT/HTTP/Proxy/Xunlei/Provider 分块 |
| CLI 客户端 | ✅ | `smart-dl add/pause/resume/remove/list/status/logs/fallback` |
| 导入迅雷任务 | ✅ | `import-xunlei` 命令（xlbt.cfg + .bt.xltd + .torrent → fastresume） |

### 未完成 / 待增强

| 能力 | 状态 | 说明 |
|------|------|------|
| Magnet 直链解析 | 🔶 部分 | 有 `bencode_info_hash`，但缺少完整 magnet → torrent 抓取 |
| BT 子文件选择 | 🔶 deferred | `XL_BtSelectSubTask` 在 Windows 绑定已暴露，但上层 UI 未接线 |
| 速度精确查询 | 🔶 deferred | `XL_QueryTaskFlow` 字段 + `XLGetGlobalDownloadSpeed` 签名待补 |
| 断点续传校验 | 🔶 partial | fastresume 有，但跨引擎统一续传协议待定 |
| 多源调度可视化 | 🔶 backlog | Provider fallback 有骨架，无 UI |

### 关键文件

- `crates/daemon/src/main.rs` — 服务/客户端入口
- `crates/daemon/src/cli.rs` — 命令集（含 `import-xunlei`）
- `crates/core/src/types.rs` — `DownloadEngine` trait（M3/M5/M6 消费）
- `crates/core/src/source_parse/normalize.rs` — 链接归一化
- `docs/CAPABILITY_MAP.md` — 远期能力地图（BiglyBT/aria2/eMule 对标）

---

## 二、迅雷跨平台 SDK 开发（Windows / macOS / Android）

### Windows SDK（✅ 完成）

| 项目 | 状态 | 文件 |
|------|------|------|
| DLL 加载 | ✅ | `crates/xunlei-ffi/src/loader.rs` |
| 类型绑定 | ✅ | `bindings.rs`（XLInitParam / XLBTTaskParamV2 / XLP2spParam / XLTaskInfo） |
| 错误码 | ✅ | `error.rs` |
| 生命周期 | ✅ | `handle.rs`（XL_Init / XL_UnInit） |
| 任务创建 | ✅ | `task.rs`（Magnet / BT / P2SP） |
| 状态查询 | ✅ | `query.rs`（TaskState enum + TaskInfo） |
| Peer / Tracker / DCDN | ✅ | `peer.rs` / `tracker.rs` / `dcdn.rs` |
| 身份注入 | ✅ | `identity.rs`（SetTokenMode / SetAppGuid / SetAccelerateCertification） |
| 真机验证 | ✅ | `scripts/research/xunlei/verify_bt_download.py` / `verify_p2sp_lifecycle.py` |

**已知限制（不影响主线）**
- `XL_QueryTaskFlow` 速度字段 3 参数签名待补
- task_state=9 / +0x38..+0x53 未知字段 deferred

### macOS SDK（🔶 进行中）

| 项目 | 状态 | 说明 |
|------|------|------|
| 二进制提取 | ✅ | `DownloadKit` fat / `DownloadKit_arm64.bin` / `xlcommon` / `MacXLSDKs` / `DownloadService.xpc` |
| C 导出定位 | ✅ | 14 个关键函数地址（CreateBtTask / GetTaskInfo / Init 等） |
| `TAG_TASK_PARAM_BT` | ✅ | 核心字段还原（字符串 + 标志） |
| `XLInitParam` | ✅ | 完整布局（148 字节） |
| mangled 名提取 | ✅ | 101 个 DownloadLib 内部函数签名 |
| `TAG_XL_TASK_INFO_EX` | ❌ 卡住 | 虚函数表多级调度，静态分析未突破 |
| 完整 C 导出列表 | 🔶 半完成 | ~30 个关键地址，非完整 153 个 |
| Rust FFI 绑定 | ❌ 未开始 | `crates/xunlei-ffi-macos/` 尚未创建 |

**推荐下一步（按 ROI）**
1. 写 30 行 C 测试程序（macOS 真机）调用 `XLGetTaskInfo` 后 hex dump — 最快拿到 `TAG_XL_TASK_INFO_EX` ground truth
2. 或用 `machotools` / `ghidra` 做更高级的符号恢复（比 capstone 手扫高效）

### Android SDK（🔶 侦察完成，绑定未开始）

| 项目 | 状态 | 说明 |
|------|------|------|
| .so 提取 | ✅ | `libxl_thunder_sdk.so`（8.5MB，arm64） |
| 字符串侦察 | ✅ | 159 个 XL 函数 via 字符串 |
| JNI 边界 | ✅ | 确认 JNI-internal，C 导出不可直接用 |
| Rust FFI 绑定 | ❌ 未开始 | 需通过 JNI 或直接调用 .so 内部函数 |

---

## 三、迅雷登录的原生解决办法

### 最终确定的方案（2026-08-25 定稿）

| 步骤 | 状态 | 说明 |
|------|------|------|
| 设备码二维码 | ✅ 端到端验证 | `pan.xunlei.com/yc/?client_id=Xqp0...&user_code=...` |
| 手机 App 扫码 | ✅ 8/22 + 8/25 成功 | 用户手机迅雷 App 确认授权 |
| token 获取 | ✅ | access_token + refresh_token（12h 自动续期） |
| captcha/init | ✅ | 带 meta（user_id + client_version + package_name + timestamp）+ captcha_sign |
| list_files 全链 | ✅ | 三件套（Bearer + x-client-id + x-device-id + x-captcha-token）同源验证通过 |

### 代码现状

| 文件 | 状态 |
|------|------|
| `crates/provider/src/xunlei/auth.rs` | ✅ `AuthState` + `load/save` + JWT 解析 + web credentials 兼容 |
| `crates/provider/src/xunlei/device.rs` | ✅ `DeviceAuthFlow` 状态机（start → poll_once → Done/Failed） |
| `crates/provider/src/xunlei/client.rs` | ✅ `request_device_code` / `poll_device_token` / `refresh_captcha` / `resolve_link` / `list_files` |
| `crates/provider/src/xunlei/provider.rs` | ✅ `store_login`（token + 自动 refresh captcha_token） |
| `crates/provider/src/xunlei/login_flow.rs` | ✅ 2026-08-30 三模式编排（browser/page/qr）+ 本地 QR URL 构造 |
| `crates/provider/src/xunlei/login_page.rs` + `login_page.html` | ✅ 2026-08-30 本地 App 同款登录页（axum，127.0.0.1，扫码/密码/短信三 Tab） |
| `crates/daemon/src/xunlei_login.rs` + cli.rs | ✅ 2026-08-30 `smart-dl-daemon xunlei-login [--browser|--page|--qr] [--token] [--port]` |
| `docs/research/xunlei/NATIVE_LOGIN_GUIDE.md` | ✅ 2026-08-30 用户手册（三模式/时序/复刻清单/合规） |

### 待对齐（重要）

| 问题 | 状态（2026-08-30 收口） |
|------|--------------------------|
| `DEVICE_CLIENT_ID` | ✅ 已对齐 `Xqp0kJBXWhwaTpB6`（client.rs 常量注释留档 + 防回归单测） |
| QR 构造 | ✅ 本地 `pan.xunlei.com/yc/?client_id=…&user_code=…` 模板构造（`device_code_qr_url`） |
| examples | ✅ 6 个示例在库，`xunlei_qr_login.rs` 已切本地 QR 构造；`cargo check --examples` 全过 |

### 凭证存储（安全约定）

- 活体 token **严禁入库**（`.gitignore` 已排除）
- `xunlei_auth.json` / `xunlei_auth_web.json` / `xunlei_fresh_token.txt` 仅本地
- 文档只记录前 12 字符 + 长度 + exp，完整值存本地 json

---

## 四、对比特彗星等材料的吸收处理借鉴

### 已归档材料

| 材料 | 位置 | 状态 |
|------|------|------|
| BitComet 逆向 | `docs/research/clients/bitcomet/r1/` | ✅ 符号/API/协议已提取 |
| 多下载器分析包 | `docs/research/clients/_zips/` | ✅ 原始 zip 归档 |
| 迅雷 P2P 侦察 | `docs/research/xunlei/p2p_research_complete.md`（255KB） | ✅ PHub/SHub/FreeDCDN 协议文档化 |
| 迅雷引擎逆向 | `docs/research/xunlei/xunlei_engine_research.md` | ✅ 被拒方案调研记录 |
| 云上传扫描 | `docs/research/xunlei/_cloud_upload_scan.json` | ✅ 端点清单 |
| 网关扫描 | `docs/research/xunlei/_gateway_scan.json` | ✅ 端点清单 |
| 解压 API | `docs/research/xunlei/DECOMPRESS_API.md` | ✅ 在线解压接口 |

### 能力地图（CAPABILITY_MAP.md）中的对标计划

| 对标对象 | 抽什么 | 对接点 | 门控 |
|----------|--------|--------|------|
| **BiglyBT** | Swarm Merging（多 torrent piece 合并） | Source Pool + Piece Manager | F3.1 后 |
| **aria2** | 多协议 Source/Segment Scheduler | `httpdl` 升级 | 主线后 |
| **Transmission** | piece picker / 请求窗口 / choking | 对照实验 | N4 时 |
| **Deluge** | libtorrent 上层策略 | 隔离引擎贡献 | 与 Transmission 并行 |
| **eMule/MLDonkey** | Kad / Source Exchange / AICH | `trait SourceProvider` | N2 时 |
| **WebTorrent** | WebRTC transport | 远期 | — |
| **μTorrent** | uTP 参数 / 连接配比 / 磁盘缓存 | btcore 内核开关 | N4 时 |
| **Tixati** | Peer 质量评分 / 带宽分配 | 长期 | — |
| **FlashGet** | 多线程/镜像发现 | httpdl 对照 | C 档最后 |

### 云解析队列（待派/已派）

| 状态 | 对象 | 必答考题 |
|------|------|----------|
| 📥 已派 | **BitComet** | LT-Seeding 协议 / Torrent Exchange / HTTP-FTP P2P / Anti-Leech / 磁盘缓存 |
| ☁️ 待派 | **μTorrent / BT Classic** | uTP / choke / piece 选择 / 上传槽位 |
| ☁️ 待派 | **Tixati** | Peer 评分 / 带宽分配 / 连接生命周期 |
| ☁️ 待派 | **FlashGet** | 多线程/镜像发现 |
| ☁️ 待派 | **文件蜈蚣** | 协议嗅探（C 档，最后） |
| ✅ 本地已覆盖 | **迅雷本体** | 登录/云盘/加速/配额已完成 |

### 能力吸收总清单（2026-08-30 新增）

**全部竞品分析材料已盘点建档 → `docs/CAPABILITY_ABSORBED.md`**（✅ 已落地 / 🔶 原型待接 / 📋 计划 / 🚫 明确不吸收，逐项标注）。本轮净新增落地：FileCentipede 嗅探引擎（`core/src/sniffer.rs`）、BitComet 策略建议器（`core/src/strategy.rs`）、夸克网盘 Provider（`provider/src/quark/`，RemoteProvider 全链）。

### 可操作结论

- **BitComet 结果**若拿到：优先映射到 `SourceProvider` trait + `PieceManager` 策略
- **aria2 scheduler**：直接对照 `crates/httpdl/src/protocol/ftp.rs` + HTTP 段调度逻辑升级
- **eMule Kad**：抽象为 `trait SourceProvider { async fn discover_sources(...) }`，不直接耦合协议
- **Super Seeding / 磁盘缓存**：需 btcore 内核暴露开关时再议，当前 libtorrent 基座不直接支持

---

## 五、云端工作区使用指南

### 仓库结构

```
smart-downloader/
├── crates/
│   ├── core/          — 类型定义 + DownloadEngine trait + source_parse
│   ├── btcore/        — libtorrent 引擎封装
│   ├── httpdl/        — HTTP/FTP 下载器
│   ├── daemon/        — 服务 + CLI
│   ├── provider/      — 云兜底 Provider（XunleiProvider 在内）
│   └── xunlei-ffi/    — Windows SDK FFI（仅 Windows）
├── ffi/               — libtorrent C 接口
├── docs/
│   ├── CAPABILITY_MAP.md          — 能力地图 + 对标计划
│   └── research/xunlei/           — 迅雷逆向全文档
├── research_bin/      — 关键分析二进制（见下）
└── scripts/research/xunlei/       — 逆向脚本
```

### 关键二进制位置

| 文件 | 路径 | 大小 |
|------|------|------|
| Windows SDK | `research_bin/windows/DownloadSDK.dll` + `Proxy.dll` + `Server.exe` | ~5MB |
| macOS DownloadKit | `research_bin/macos/DownloadKit` (fat) + `DownloadKit_arm64.bin` | ~33MB |
| macOS xlcommon | `research_bin/macos/xlcommon` | 0.5MB |
| macOS MacXLSDKs | `research_bin/macos/MacXLSDKs` | 3.6MB |
| macOS DownloadService | `research_bin/macos/DownloadService` | 17.5MB |
| Android libxl | `research_bin/android/libxl_thunder_sdk.so` | 8.5MB |
| Android APK | Git LFS 仓库内 | 77MB |
| macOS DMG | **Release 附件**（108MB，超 GitHub 100MB 限制） | — |

Release 页面：https://github.com/tomjiu/smart-downloader/releases/tag/v0.1.0-assets

### 云分析推荐工作流

1. **通用下载器主线**：从 `crates/core/src/types.rs` 的 `DownloadEngine` trait 开始，理解 add/pause/resume/status/remove/peers 统一抽象
2. **Windows 迅雷绑定**：`crates/xunlei-ffi/src/` 全套，已验证可编译运行
3. **macOS 逆向**：`docs/research/xunlei/macos_abi_reverse.md` + `research_bin/macos/` 二进制
4. **登录方案**：`docs/research/xunlei/CREDENTIAL_HUNTING.md` + `crates/provider/src/xunlei/client.rs`
5. **BitComet 吸收**：`docs/research/clients/bitcomet/r1/` + `docs/CAPABILITY_MAP.md`

---

## 六、最高优先级 TODO（接手后建议先做）

1. ~~Windows 登录对齐~~ **✅ 2026-08-30 完成**（client_id 对齐 + QR 本地构造 + 三种登录模式，见 `NATIVE_LOGIN_GUIDE.md`）
2. **macOS `TAG_XL_TASK_INFO_EX`**：写 C 测试程序 hex dump，或 Ghidra 高级反编译（未变，路线图见 `CROSS_PLATFORM_UNIVERSAL_SOLUTION.md`）
3. ~~BitComet/竞品解析结果审计~~ **✅ 2026-08-30 完成**（`CAPABILITY_ABSORBED.md` 全量建档，高 ROI 项已落地）
4. **主线 F3.1 验收**：BT 引擎 matured + Bug B/C 关闭，解锁 CAPABILITY_MAP 第一波（未变）
5. ~~Rust 编译检查~~ **✅ 2026-08-30 完成**：`cargo check --workspace` Linux 全绿（xunlei-ffi cfg 门控 + btcore bindgen 回退）；`cargo test --workspace --exclude smart-dl-btcore` 全绿

---

## 七、已知风险与约束

| 风险 | 影响 | 缓解 |
|------|------|------|
| macOS SDK 静态分析陷入虚函数表泥潭 | 绑定进度缓慢 | 改用 C 测试程序 / Ghidra / 找 xunlei 开源闭源替代 |
| GitHub LFS 配额 | 大文件上传可能超 | 关键二进制已精简，DMG 改 Release |
| Windows SDK 无源码 | FFI 绑定靠反汇编 | 已有真机验证脚本兜底 |
| 迅雷登录 client 白名单 | 非白名单 client 被拒 | 已确认 Xqp0 可用，需改代码 |
| 凭证安全 |  token 泄露风险 | .gitignore 已收紧，文档只记录前缀 |
