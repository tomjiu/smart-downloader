# 迅雷收尾总计划（2026-08-31）

> 目标（用户原话）：「争取一把按照计划全做完，给迅雷收尾，争取迅雷插件在
> win/mac/linux/安卓都可用」。
> 前置状态：上游 PR #3 已合并（tomjiu/main @ 337a14d，维护者追加安全加固
> 3015a88）；NAS 线归档（NAS_REMOTE_ENGINE.md 头部横幅）；本计划为其后继。

## Definition of Done（判定）

**四平台跑通同一条端到端链：登录 → 提交 → 下载 → 进度 → 完成。**
逐平台判据见 `docs/FOUR_PLATFORM_ACCEPTANCE_PLAYBOOK.md`（P4-1，S0-S7）。

平台判定矩阵（随执行更新）：

| 平台 | 判定 | 依据 |
|------|------|------|
| Windows | ✅ 达成 | XunleiEngine（FFI SDK）+ provider，L0-L2 全链既有实测 |
| Linux   | ✅ 达成 | httpdl/btcore + provider；P4-2：543 单测全绿 + CLI 冒烟 |
| macOS   | 🔶 等效必达 | provider/httpdl 纯 Rust 理论直通；原生 XunleiEngine 绑定冲刺（Phase 3，止损线写死） |
| Android | ✅→真机项 | **P1-3 翻转**：aarch64 NDK 交叉产物产出（bionic API24+）；端到端待真机（P4-5） |

## 阶段划分

### Phase 0：文档收口（本提交完成）
- P0-1 ✅ PR #3 合并核实 + 安全口径接纳（3015a88 红线入工作纪律）
- P0-2 ✅ NAS_REMOTE_ENGINE.md 归档横幅（主线回归 Win/Mac Xqp0 档）
- P0-3 ✅ 本计划入库
- P0-4 ✅ worklog 32-a/32-b 记录
- P0-5 ✅ 分支 push + 上游 PR（PAT 瞬时注入）

### Phase 1：通用能力（一把做完）
- P1-1 ✅ **平台身份多 profile**：`tier.rs` 档位注册表（web/nas）；
  Client 持有 `&'static Tier`，全部请求体/三要素头随档位；
  `tier_authorize_url` 档位授权页；`XunleiProvider::with_tier`；
  serve `provider_xunlei.tier` + env `SMART_DL_XUNLEI_TIER`（未知档拒绝启动）；
  `xunlei-login --tier`（登录态按档分文件 = 独立 device_id 防互踢）；
  share/cloud_search 显式钉死 web 档。
- P1-2 ✅ fs2you:// 解码（2026-08-30 已落地：`source_parse/fs2you.rs`
  10 单测 + normalize 路由；无需重做）
- P1-3 ✅ **NDK 交叉编译（关键路径）**：reqwest 切 rustls（openssl 依赖树
  清零）；`scripts/build_android.sh`（NDK r27c，API24）；产物
  `smart-dl-daemon-android-aarch64`（13MB bionic）；Linux 回归 543 单测全绿
- P1-4 ✅ `docs/ANDROID_TERMUX_DEPLOY.md`（路线 A 产物直部署 / B 端上原生
  构建 / web 档身份理由 / 功能矩阵 / 常驻 / 边界）
- P1-5 ◻ 活体账号项（需用户配合）：S3 云端离线链活体复验、多档互踢观测、
  微信扫码流程活体确认

### Phase 2：Windows dump 级增强（需真机，不阻塞 DoD）
XL_QueryTaskFlow 签名、XLTaskInfo 尾部 task_state=9 + 0x38..0x53、
DCDN 凭证注入 4 函数参数、XL_SetUserInfo ABI。

### Phase 3：macOS 原生绑定（止损线写死）
TAG_XL_TASK_INFO_EX 输出布局（1 硬卡点 + 3 欠账）。
**止损线**：1 日无 dump → 降级绑定（provider/httpdl 等效路径，即判定矩阵
「等效必达」）；5 日无端到端 → 永久止损。

### Phase 4：四平台端到端验收
- P4-1 ✅ `docs/FOUR_PLATFORM_ACCEPTANCE_PLAYBOOK.md`（S0-S7 × 四平台判据）
- P4-2 ✅ Linux 本环境验收：构建 + 543 单测 + CLI 冒烟（`--tier nas` 真实
  设备码请求 200 = tier 全链活体）
- P4-3 ◻ Windows 真机（用户配合）
- P4-4 ◻ macOS 真机（用户配合）
- P4-5 ◻ Android 真机（用户配合；产物已就绪）

## 「一把做完」可行性裁定

| 项 | 本环境可做 | 依据 |
|----|-----------|------|
| Phase 0 全部 | ✅ | 纯文档 |
| P1-1/P1-2 | ✅ | 纯 Rust，无外部依赖 |
| P1-3/P1-4 | ✅ | NDK r27c 已在手；无实弹 |
| P4-1/P4-2 | ✅ | 本环境 Linux + 单测基线 |
| P1-5 / P4-3..5 | ❌ | 活体账号 / 真机，需用户配合 |
| Phase 2 / Phase 3 | ❌ | dump 采集需真机；止损线已写死 |

## 不做清单（红线）

1. **L3 私有加速永不通解**（RSA-1024 每请求随机密钥；vip_speedup 之外不碰）
2. eMule/ed2k 引擎不新建（白名单解析已有，引擎不做）
3. Wine 跑 Windows DLL 不做（脏路线，维护成本无穷大）
4. 不追迅雷客户端版本更新（校准对象锁 3.23.5 / web 1.92.91；漂移另立项）
5. 凭证 0 落仓（3015a88 红线）：证据只留形状摘要（前 12 字符 + 长度 + 过期）

## 风险与对策

| 风险 | 对策 |
|------|------|
| nas 档 captcha 盐链未实弹验证 | tier.rs 假设区显式标注；实弹路径走引擎二进制（NAS 线归档） |
| 服务端风控对非常规 client_id 收紧 | 多档独立 device_id + 参数严格匹配档位；异常即回 web 档 |
| 账号设备数上限 | 多档 = 多设备观感；文档明示风险，P1-5 活体观测 |
| ring/arm64 asm 构建漂移 | build_android.sh 固定 NDK r27c + API24；升级另测 |
