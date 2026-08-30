# Phase 2 交付说明（2026-08-30）

> 本压缩包 = smart-downloader 完整代码 + 全部文档（本轮 Phase 2 增量 + 历史积累）。
> 对应提交：`9fed968 Phase2: xunlei native login (3 modes) + capability absorption ...`
> 验收基线：`cargo check --workspace` ✅（Linux）；`cargo test --workspace --exclude smart-dl-btcore` **472 通过 / 0 失败**
> （btcore 测试需 Windows + libtorrent 链接环境，属既有 CI 约定）

---

## 一、你上轮三个问题的直接答案

### Q1 迅雷登录原生（OAuth 式 / App 一致页面）

**已全部实现**，三种模式，命令 `smart-dl-daemon xunlei-login`：

| 模式 | 命令 | 体验 |
|------|------|------|
| 本地登录页（默认） | `xunlei-login` | 浏览器打开本地 `127.0.0.1` 页面——深蓝渐变+白卡片+“迅雷”标志+三 Tab（扫码/密码/短信），视觉复刻迅雷 App 登录页 |
| 官方页跳转 | `xunlei-login --browser` | **系统浏览器直接跳转迅雷官方授权页** `pan.xunlei.com/yc/?client_id=…&user_code=…` |
| 终端二维码 | `xunlei-login --qr` | 命令行直出二维码，手机 App 扫码 |

- 设备码 client_id 已对齐实测通过值 `Xqp0kJBXWhwaTpB6`（原 `XW5SkOhLDjnOZP7J` 为已知失败值，已修正+防回归单测）。
- 登录态落盘 `xunlei_auth.json`（0600），与 `XunleiProvider` 完全互通、自动续期。
- 代码：`crates/provider/src/xunlei/{login_flow,login_page}.rs` + `login_page.html` + `crates/daemon/src/xunlei_login.rs`；测试含两个 mock 全链 e2e。
- 手册：`docs/research/xunlei/NATIVE_LOGIN_GUIDE.md`。

### Q2 迅雷跨平台通解 / 下载能力能否完全取下来

**权威回答见 `docs/research/xunlei/CROSS_PLATFORM_UNIVERSAL_SOLUTION.md`**（301 行，逐条带仓库证据）。TL;DR：

- **通解成立，但分层**：L0 协议解码层（thunder:// qqdl:// magnet 等）与 L1 云服务层（OAuth 登录/网盘 API/分享/离线/直链）= 纯 Rust「同一份代码跑遍全平台」的真通解 ✅；L2 本地引擎层 = 分平台等效通解（Windows 迅雷引擎 FFI 全套真机验证 ✅；其余平台 libtorrent + 云直链兜底 ✅）；L3 私有加速层（VIP 通道 / PHub / DCDN）❌ 永不通解。
- **「完全取下来了吗」≈ 95% 客户端可见面已取下**（✅24 项清单）；永远取不到的是**服务端授权面**（VIP 配额 + 私有 P2P 准入，不在客户端二进制里，D28 决策 + 技术不可行双重锁定，❌7 项）；macOS/Android 为 🔶11 项（附解除路径路线图）。

### Q3 比特彗星/夸克等是否已转化为可吸收能力

**已全量建档并落地高 ROI 项，见 `docs/CAPABILITY_ABSORBED.md`**。四档状态：✅ 已落地 / 🔶 原型待接 / 📋 计划 / 🚫 明确不吸收。本轮净新增落地 6 项：

1. **FileCentipede 协议嗅探引擎** → `crates/core/src/sniffer.rs`（scheme 直判/文本提取/网盘分享识别/规则表可配，13 测）
2. **BitComet 策略建议器** → `crates/core/src/strategy.rs`（自适应磁盘缓存 + 分级反吸血 → libtorrent 参数建议，7 测）
3. **夸克网盘 Provider 全链** → `crates/provider/src/quark/`（分享解析→转存→直链，RemoteProvider trait + 失败冷却，10 测）
4. ed2k 链接解析 → `crates/core/src/source_parse/ed2k.rs`
5. 迅雷原生登录（Q1 的三模式）
6. btcore Linux 可编译（bindgen 回退）

另：此前已落地的 FlashGet Mirror 加权评分（httpdl）、HTTP 动态分段、备用源兜底等在清单中标 ✅；Tixati Peer 评分/5 层带宽等保留为原型并写明接入门控；**不吸收清单**（自研 BT 栈/私有上报/旧加密等 8 项）同样重要，附理由。

## 二、本轮主线增强（通用下载器）

- `cargo check --workspace` 在 Linux 从**挂**到**全绿**（xunlei-ffi 全量 cfg 门控；btcore build.rs 无 libclang 自动回退已提交 bindings 并剥离平台断言）。
- 全 workspace 测试修复后 **472/0**。
- ed2k 链接解析进主线（解析→结构化元数据→明确路由错误，完整 eMule 引擎仍列远期）。
-迅雷离线下载 API（submit/list/progress + 配额档位）+ torrent 字节直传通道（Phase 1 已有，本轮纳入文档与验收）。

## 三、快速上手

```bash
# 构建（Linux/任意平台）
cargo check --workspace
cargo test --workspace --exclude smart-dl-btcore

# 迅雷登录（三选一）
cargo run -p smart-dl-daemon -- xunlei-login            # 本地 App 同款登录页
cargo run -p smart-dl-daemon -- xunlei-login --browser  # 跳转官方授权页
cargo run -p smart-dl-daemon -- xunlei-login --qr       # 终端二维码

# 启动 daemon + 添加任务
cargo run -p smart-dl-daemon -- serve
cargo run -p smart-dl-daemon -- add "https://example.com/file.iso" -o ./downloads

# 网盘能力（需先登录）
#   迅雷：分享解析/云盘列表/直链下载/离线提交 —— crates/provider/src/xunlei
#   夸克：分享解析/直链 Provider —— crates/provider/src/quark
```

## 四、压缩包内容与排除项

| 内容 | 说明 |
|------|------|
| `crates/`（7 个） | core / httpdl / btcore / daemon / provider / xunlei-ffi / xunlei-convert |
| `docs/` | 全部研究文档（迅雷逆向/登录/跨平台通解/BitComet/5 客户端横评/能力地图/吸收清单/状态三件套） |
| `scripts/`、`tools/`、`tests/`、`ffi/` | 逆向脚本、工具、集成测试、libtorrent C 接口 |
| `spike/`、`.github/`、`Cargo.*`、`tasks.json` | spike 对照实验、CI 配置、构建清单 |

**已排除**（体积/合规）：`.git`（168M）、`target/`（构建产物）、`out/`（构建输出 83M）、`research_bin/`（69M 专有软件研究二进制——保留在你的 GitHub 仓库/Git LFS 中，不在可分发压缩包里复制）。

## 五、合规声明

本项目为互操作性研究与学习用途：仅调用公开 Web/API 端点、不复制专有代码、不分发破解物、不绕过加密与鉴权；登录凭证仅存本机。迅雷私有 P2P 加速引擎（PHub/DCDN）维持 D28 决策排除。请遵守当地法律法规。
