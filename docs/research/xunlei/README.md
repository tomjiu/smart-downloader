# 迅雷 BT 逆向研究 — 项目集成索引

> **来源**: 独立研究 agent 的完整产出,2026-08-16 从沙箱迁移入库。
> **完整合集**: `xunlei_research_complete.md`(824KB,含全部原始报告/反汇编 JSON/历史报告)。
> **状态**: 研究完成 · PoC 合成验证通过 · 真实样本验证待用户提供

---

## 结论摘要(对 smart-downloader 的意义)

1. **迅雷 BT 协议层 100% 标准**:BEP-3/5/6/8/9/10/11 全覆盖,piece SHA1 + infohash + piece length 与标准 BT 一致(A 级证据)。
2. **迅雷 BT 占位文件不通用**:`.bt.xltd`(纯 piece 数据 sparse file,B 级)+ `.xlbt.cfg`(私有任务配置,头部 A 级、section 映射 C/D 级)。
3. **路径 A = 主引擎**:纯 libtorrent,完全无黑盒依赖。→ 即本项目的 BT engine(base)。
4. **路径 D = 用户迁移工具**:迅雷任务 → libtorrent fastresume 转换器(PoC 已通过合成样本端到端验证,piece hash 匹配率 1.0)。
   - 决策记录见 `DECISIONS.md`(D-2026-08-16-01/02/04)。
5. **剩余唯一门槛 = 真实样本验证**(用户操作,见 `sample_collection_guide.md`)。

---

## 目录结构

```
docs/research/xunlei/
├── README.md                        ← 本索引
├── xunlei_research_complete.md      ← 完整合集(原始归档,824KB)
├── FINAL_REPORT.md                  ← 最终报告(核心结论,分层证据)
├── SPEC 格式规范 / 推断清单
│   ├── spec_pending_validation.md   ← .xlbt.cfg/.bt.xltd 格式规范(置信度 A/B/C/D)
│   └── OPEN_QUESTIONS.md
├── 研究状态
│   ├── RESEARCH_STATE.md
│   ├── FINDINGS.md
│   └── DECISIONS.md
├── p2p_research_complete.md        ← P2P 接入逆向·最新完整版(11:32,P2 协议文档化 +
│                                      P3 路径评估 + 16 PoC 失败→真实抓包建议)
├── p2p_research_complete_v1.md     ← 前版(含独立 Http.dll 反汇编报告章节)
├── p2p_recon_complete.md           ← 早期合集(RESEARCH_STATE + PROGRESS v2/v3)
├── xunlei_independence_analysis.md ← 黑盒独立可行性分析(路径 A 采纳依据,08-16)
├── xunlei_engine_research.md       ← 迅雷本地引擎逆向(被拒方案调研记录,08-16)
├── p2p_recon/                      ← P2P 侦察中间产物补齐(08-17 晚,见 REVIEW §6)
│   ├── REVIEW.md                   ← 审查记录(采纳 P3-C;证据分级;补齐归档说明)
│   ├── FINAL_REPORT.md / PROGRESS_REPORT_v2/v3.md / PUBLIC_INTEL_REPORT.md
│   ├── xbtpackage_vtables.json / phub_shub_cmd_analysis.json
│   ├── alist_src/*.go              ← alist AGPL-3.0 节选,仅研究参考
│   └── scripts/*.py                ← 反汇编/PoC 脚本(capstone/ghidra)
├── NEXT_ACTION.md                   ← 下一步(两个决策点 + 真实样本验证)
└── sample_collection_guide.md       ← 用户采集真实样本的手册(5-10 分钟)

tools/xunlei-migrate/                ← 路径 D 转换器工具集(Python, 无第三方依赖)
├── xunlei_to_libtorrent_converter.py   ← 转换器(默认 DIAGNOSTIC, --convert 才写文件)
├── validate_xunlei_sample.py           ← 8 项验证器(真实样本验证入口)
├── e2e_test_converter.py               ← 端到端测试(合成样本,已 100% 通过)
├── parse_xlbt_cfg.py                   ← .xlbt.cfg 解析器
├── gen_synthetic_full_cfg.py           ← 合成 cfg 生成器(完整版)
└── gen_synthetic_cfg.py                ← 合成 cfg 生成器(简单版)
```

---

## 现状与验收门槛

| 项 | 状态 |
|---|---|
| 协议层标准性 / 引擎架构 | ✅ A 级证据 |
| .xlbt.cfg 头部(40B + 20B entry) | ✅ A 级反汇编证据 |
| .bt.xltd 纯数据 sparse 布局 | ⚠ B 级(多证据支持,未真实验证) |
| section_id → 内容映射 | ❌ C/D 级猜测(**禁止写死进生产代码**) |
| 转换器 PoC | ✅ 合成样本 e2e 通过(pieces_hash_match_rate=1.0, fastresume 271B 合法, .part 正确) |
| 真实样本验证 | ⛔ 待用户提供(唯一外部依赖) |

**铁律**(来自 spec):C/D 级推断(尤其 section_id 映射)禁止硬编码进生产代码;转换器默认诊断模式,验证通过前不产生任何文件变更。

---

## 如何运行

### 1. 本地复跑合成 e2e(验证工具链完整)

```powershell
# 依赖: 仅 e2e 与合成生成需要 python libtorrent 绑定 (转换器/验证器本体是纯 stdlib)
python -m pip install libtorrent   # 一次性

python tools/xunlei-migrate/e2e_test_converter.py
```

预期:端到端报告显示 pieces_hash_match_rate = 1.0、fastresume/.part 校验通过(于本机实测通过)。

> 注:`gen_synthetic_full_cfg.py` 会写 1GB 的 `.bt.xltd`(Linux 下为 sparse),Windows 上谨慎运行;
> `gen_synthetic_cfg.py` 为小样本快速自检。

### 2. 真实样本验证(样本到手后)

```powershell
# 验证(输出 verification.json)
python tools/xunlei-migrate/validate_xunlei_sample.py `
  --torrent <样本>.torrent --bt-xltd <样本>.bt.xltd --cfg <样本>.xlbt.cfg `
  --report verification.json

# 全部 8 项验证通过后 → 转换
python tools/xunlei-migrate/xunlei_to_libtorrent_converter.py `
  --torrent <样本>.torrent --bt-xltd <样本>.bt.xltd --cfg <样本>.xlbt.cfg `
  --output-dir output --convert
```

验证通过后:把 `spec_pending_validation.md` 中 C/D 级升级为 A 级,解锁转换器为正式工具。

### 3. 采集真实样本(需要你做,5-10 分钟)

见 `sample_collection_guide.md`。要点:

1. 迅雷 v25.x 下载任意 100MB-1GB BT 任务到 **30-50%** 时暂停
2. **完全退出迅雷**(托盘退出 + 任务管理器结束 XLUE/Thunder/DownloadSDKServer)
3. 复制三件套:`<任务名>.bt.xltd` + `<任务名>.xlbt.cfg` + 原始 `.torrent`
4. zip 打包(bt.xltd 是 sparse,压缩后很小)提供给我
5. 隐私注意:不要提供 `cid_store.dat`;`.xlbt.cfg` 可能含 device_id,介意可先 hex 检查

---

## 与主设计的关系

- 主引擎保持不变:**单 libtorrent 基座**(路径 A),不含任何迅雷黑盒组件(design v0.6 §3)。
- 路径 D 转换器是**可选的用户迁移工具**(v1 增量),产出 libtorrent fastresume + .part,可被 BT engine 直接续传。
- 迅雷云盘 Provider(O5')已确认不做;`thunder_offline_research.md`(迅雷云盘 API 调研)另见 `docs/research/2026-08-16-thunder-offline-research.md`。

## 结论

- **研究线程已关闭**:所有在当前环境可达的结论与实验均已完成(下称"在当前条件下完成")。
- **唯一剩余动作在用户侧**:提供真实样本后 1 小时内可完成全部 C/D 级升级验证。
- 真实样本验证**不阻塞** M0-M7 主线(路径 A 完全独立)。

## 研究二进制资产（GitHub Release）

因 GitHub 仓库对单文件 100MB 硬限制，`thunder_5.80.7.66659.dmg`（108.77MB）已发布为 Release 附件：

- **Release 页面**: https://github.com/tomjiu/smart-downloader/releases/tag/v0.1.0-assets
- **附件名**: `thunder_5.80.7.66659.dmg`
- **用途**: macOS DownloadKit.framework / MacXLSDKs / DownloadService.xpc 提取源
- **校验**: 仓库内 `research_bin/installers/` 仅保留 Android `x-player-guanwang.apk`（77MB，通过 Git LFS 存储）；DMG 需从 Release 单独下载

仓库内已包含的分析二进制（无需 Release）：
- `research_bin/windows/`: `DownloadSDKProxy.dll` / `DownloadSDK.dll` / `DownloadSDKServer.exe`
- `research_bin/macos/`: `DownloadKit` / `DownloadKit_arm64.bin` / `xlcommon` / `MacXLSDKs` / `DownloadService`
- `research_bin/android/`: `libxl_thunder_sdk.so`

云分析工作区建议：
1. 从 Release 下载 `thunder_5.80.7.66659.dmg`
2. 挂载/解包后提取 `DownloadKit.framework`、`xlcommon.framework`、`MacXLSDKs.framework`、`DownloadService.xpc`
3. 对照 `docs/research/xunlei/macos_abi_reverse.md` 继续逆向后 4 个未完成结构体

## 跨平台取证原材料（GitHub Release）

Phase 3 四端可行性定案的原始证据（APK 解包 + NAS 引擎 + cnk3x 源码），因体积较大（181MB）单独归档：

- **Release 页面**: https://github.com/tomjiu/smart-downloader/releases/tag/v0.1.0-cross-platform
- **附件名**: `cross-platform-evidence.zip`
- **MD5**: `0fa3103e9234b8d95dd67716bdc54a26`
- **用途**: 
  - `spk-x64/payload/bin/bin/xunlei-pan-cli.3.23.5.amd64` — Linux x86_64 官方引擎（xllite）
  - `spk-arm/payload/bin/bin/xunlei-pan-cli.3.1.10.arm64` — Linux ARM64 官方引擎
  - `apk-unpacked/` — hezi APK 解包（确认无引擎，为管理端）
  - `cnk3x/` — 群晖模拟最小集参考实现（MIT）
- **文档**: `docs/research/xunlei/CROSS_PLATFORM_FOUR_OS_2026-08-30.md`（附录 E）
## Phase 3b（B 档）：NAS 引擎集成骨架 + 统一身份层

在附录 E 四端定案基础上的工程落地（本 PR 分支）：

- **B-1 FTP 引擎**：`httpdl::FtpEngine` 早已交付（15 测试全绿，`--features ftp`），状态核实后从待办表移除
- **B-2 NasRemoteEngine**：`crates/daemon/src/nas_remote.rs` —— `DownloadEngine` 适配器，经 `DriveListen`（默认 `127.0.0.1:5050`）TCP 面探活 + 能力声明；下载端点表按假设区 #9 占位（UNTESTED，待扫码实测后校准）
- **B-3 L1→xllite 身份桥**：`crates/daemon/src/nas.rs::sync_l1_token` —— L1 云盘登录态（`xunlei_auth.json`）自动映射为 xllite 引擎预置 token；`serve.rs` 第 4e 步启动时自动同步（`SD_L1_TOKEN` 可覆盖路径）。格式已定案（2026-08-30 扫码实测）：引擎按原生 9 字段形读取预置文件，桥已对齐原生形（缺字段不伪造，宽容度待 A2 engine 步）
- **B-4 Android Termux 一键部署**：`scripts/nas/android-termux-setup.sh` —— proot-distro Debian + arm64 SPK 引擎下载解包 + 启动协议包装（`DriveListen/LauncherListen/ConfigPath/HOME`，unset PLATFORM）
- **A 档扫码守护**：`scripts/nas/nas_qr_daemon.py` —— RFC 8628 设备码自动续发（120s/码，运行 2h），token 到手自动落盘 `~/.nas-engine-test/data/.drive/auth_token.json`（引擎预置路径）+ 取证归档；零第三方依赖
- **core**：`EngineKind::XunleiNas` 新枚举值

原始证据（SPK/APK 解包/cnk3x）见上方 Release 附件；引擎实测记录见 `docs/research/xunlei/CROSS_PLATFORM_FOUR_OS_2026-08-30.md` 附录 E。
