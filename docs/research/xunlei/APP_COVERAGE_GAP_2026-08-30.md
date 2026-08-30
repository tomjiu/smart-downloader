# 迅雷 App 剩余未做盘点 —— 「整个迅雷 App 还有哪些部分没做」的权威清单

> 日期：2026-08-30。回答用户原话：**「现在整个迅雷 app 还有哪部分没做了」**（附官方安装包 `XunLeiWebSetup25.0.90.1592xl11.exe`）。
> 证据基线：本日实测下载的官方安装包（MD5 `a2def5a6d92660f086277e4de0c27109`）+ 仓库逆向语料 + 已合入代码。所有结论给出处，不确定处标注「**待验证**」。
> 上游文档：`CROSS_PLATFORM_UNIVERSAL_SOLUTION.md`（通解权威矩阵 ✅24/🔶11/❌7）。本文是该矩阵的"剩余工作"视角投影。

---

## 0. 一句话结论

**「迅雷 App = 下载能力 + App 外围」。下载能力：可取得的部分已全部做完（✅24 项），可继续做的只剩 9 个有明确解除路径的 🔶 缺口（合计约 3-5 个工作日），7 项 ❌ 永不可得（服务端授权/私有协议/决策排除）；App 外围（UI、播放器、云盘同步盘、会员商店等）按 D28 决策不在项目范围内。** 你给的安装包 25.0.90.1592 经实测与已分析样本**逐字节一致**，没有产生任何新增未覆盖模块。

---

## 1. 你给的安装包实测结论（25.0.90.1592 = 已分析版本）

### 1.1 安装包指纹

| 项 | 值 |
|---|---|
| 文件 | `XunLeiWebSetup25.0.90.1592xl11.exe`（15,236,256 字节） |
| MD5 | `a2def5a6d92660f086277e4de0c27109` |
| PE | PE32+ x86-64，7 节，`.rsrc` 13.9MB（熵 7.99，内嵌压缩载荷） |
| FileVersion / ProductVersion | 25.0.90.1592（SpecialBuild 100072，Copyright 2003-2026） |
| CompanyName | 深圳市迅雷网络技术有限公司 |

### 1.2 决定性事实：载荷与已分析样本 MD5 逐字节一致

安装包内嵌两个 7z 载荷（资源 ID 203/204），与仓库既有研究样本比对：

| 新安装包资源 | 大小 | MD5 | 仓库既有样本 | 结论 |
|---|---|---|---|---|
| 资源 203（安装器 UI/引导引擎） | 6,267,558 | `aed71a6bb11a231190aff3e332be633e` | `resource_1288_1296.bin` | **完全相同** |
| 资源 204（迅雷下载 SDK 核心） | 7,443,665 | `5a9f9a1f8e1ea7ac2a8c0e1d5eb26d70` | `resource_1288_1304.bin` | **完全相同** |

**含义**：当前官方渠道分发的 25.0.90.1592 外壳，内嵌的下载 SDK 与本仓库全部逆向语料对准的是同一构建。**不存在"新版本出现导致分析过期"的问题**；反过来说，本文清单就是针对"你现在能装到的版本"的最终清单。

### 1.3 SDK 载荷 34 模块 × 项目覆盖状态对照表（r204）

| 迅雷模块 | 版本 | 功能（证据） | 项目覆盖 | 状态 |
|---|---|---|---|---|
| DownloadSDK.dll | 2.86.202.127 | 下载引擎本体（100+105 导出已考古，`sdk_export_inventory.md`） | FFI 绑定 + 真机验证 | ✅ |
| DownloadSDKProxy.dll | 2.86.202.127 | 进程外代理 stub（`pe_analyze.py` 分析对象） | loader 委托加载 | ✅ |
| DownloadSDKServer.exe | 2.86.202.127 | 命名管道服务进程（`FINAL_REPORT.md:19-23`） | 生命周期已管理 | ✅ |
| xl_thunder_sdk.dll | 12.0408.960.1 | 64 位 Downloadlib SDK（Windows FFI 直连面） | xunlei-ffi 全 11 模块 | ✅ |
| TcpImpl.dll | 0.1.202.1754 | TCP 传输实现 | 经引擎黑盒使用 | ✅（黑盒） |
| Http.dll | 0.1.202.1754 | HTTP 传输插件 | 经引擎黑盒使用 | ✅（黑盒） |
| Ftp.dll | 0.1.202.1754 | FTP 协议插件 | **项目侧未实现 FTP 客户端** | 🔶 缺口（见 §3.2-9） |
| P2PBase/P2PFramework/P2PIO/P2PStat/P2PTarget/P2PCommonObjects.dll | 0.1.202.1754 | 私有 P2P 栈（PHub 前置层，PAM 2012 同源） | ❌ 协议未公开，D28 排除 | ❌ 永不做 |
| XUdt.dll | 0.1.202.1754 | uDT 私有传输层（56 类 uTP-like，`xunlei_independence_analysis.md:417-441`） | ❌ 无公开规格 | ❌ 永不做 |
| XLLiveUDownload.dll | 2.86.202.127 | 直播/升级下载通道 | 范围外 | — |
| XLReImport.dll | 2.86.202.127 | 任务重导入 | 对应 import-xunlei fastresume | ✅（自研等效） |
| XLTaskUpgrade.dll | 2.86.202.127 | 任务升级/迁移 | 对应 fastresume 迁移 | ✅（自研等效） |
| AssistantTools.dll | 2.86.202.127 | 引擎辅助工具 | 范围外 | — |
| ProxyVerifier.dll | 2.86.202.127 | 代理校验 | httpdl 支持 proxy 配置 | ✅（自研等效） |
| XLFileAssistant.exe | 0.1.0.0 | 磁盘空间预分配助手 | 范围外（httpdl 预分配自有实现） | ✅（自研等效） |
| XLBugHandler.dll / XLBugReport.exe | 4.2.1.6 | 崩溃报告 | 范围外 | — |
| upnp.exe | 1.0.2.4 | UPnP NAT 端口映射（删口） | btcore/libtorrent 原生 UPnP | ✅（自研等效） |
| libeay32/ssleay32.dll | OpenSSL 1.0.2v | TLS | Rust lsft/rustls 栈 | ✅（自研等效，且更新） |
| libcurl.dll | 7.61.1 | HTTP | reqwest | ✅（自研等效） |
| minizip.dll / zlib1.dll / VC90 CRT / manifest / statXml.xml | — | 支撑库 | Rust 栈自带 | ✅ |
| xar/DownloadDispatcher.xta | — | 任务调度脚本（1.04MB，**已定性：XLTP 容器内嵌 Lua 5.1 字节码，未加密，熵 5.59**，见附录 C.3） | 调度语义已由 query.rs/task.rs 行为等价覆盖 | 🔶 P3 档案项（无解锁价值） |

### 1.4 安装器载荷 57 文件（r203 OnlineResource）对照

InstallEntry.dll（安装引擎）+ MainWnd.xml（DUI 布局）+ libcrypto/libssl-3-x64（OpenSSL 3）+ libcurl + libexpat + xlstat4.dll（统计上报）+ cacert.pem + 44 张 UI PNG。**全部属于"安装器外壳"**，与下载能力无关，项目无需覆盖；其中 `cacert.pem`（迅雷信任根清单）与 `xlstat4.dll`（埋点事件字段）有档案价值已随包归档，不做进一步逆向。

---

## 2. 全迅雷 App 拆解：三层下载能力 + App 外围

```
迅雷 11 PC App（25.0.90.1592）
├─ 下载能力（项目范围）
│   ├─ L0 协议解码   thunder:// qqdl:// magnet          ✅ 全做完（24 项中的 3 项 + 哈希）
│   ├─ L1 云服务     登录/网盘/离线/直链/分享/搜索      ✅ 主体做完，3 个 B 级端点未实测
│   ├─ L2 本地引擎   Windows FFI ✅ / macOS 🔶 / Android 侦察后止损
│   └─ L3 私有加速   PHub/DCDN/VIP/uDT                ❌ 永不可得（7 项）
└─ App 外围（D28 范围外）
    ├─ 主界面 UI 框架 / DUI 布局引擎 / 皮肤系统            不做
    ├─ 内置播放器（XL_LaunchPlayTask 边下边播）           不做（L3 相邻）
    ├─ 云盘同步盘 / 相册备份 / 文件扫描                    不做（L1 仅取下载相关端点）
    ├─ 会员商店 / 游戏中心 / 资讯流 / 浏览器内核            不做
    └─ 快鸟宽带提速 / 硬件加速检测                         不做（地域绑定，`SPEEDUP_SYSTEM.md:9`）
```

---

## 3. 还没做的部分 —— 三张清单

### 3.1 ❌ 永不可做（7 项，技术不可行 + D28 决策双重锁定，无需再投入）

| # | 能力 | 根因（一句话） | 依据 |
|---|---|---|---|
| 1 | PHub/SHub/DPHub 私有 P2P 自研接入 | RSA-1024 包裹每请求随机 AES key，离线派生不可能；300+ 命令字需中心服务器；非官方 peer 会被风控 ban | `p2p_research_complete.md:10-28` |
| 2 | DCDN/FreeDCDN 协议自研 | 接入协议未公开（D28 原文） | `BACKLOG.md` D 段 |
| 3 | VIP TrySpeed/SuperSpeed 加速 | 配额由服务端发放，客户端无法伪造 | `SPEEDUP_SYSTEM.md:46-53` |
| 4 | VIP DCDN token 通道 | 同 #3，服务端授权范畴 | `xunlei_independence_analysis.md:263-271` |
| 5 | uDT 私有传输层自研 | 56 类 uTP-like 无公开规格，成本"极高" | `xunlei_independence_analysis.md:417-441` |
| 6 | SDK 内账号登录 | **考古封死**：100+105 导出中 0 个登录函数 | `sdk_export_inventory.md:9-16` |
| 7 | cid_store.dat 秒传复用 | 私有二进制格式 + v1 不需要（决策） | `FINAL_REPORT.md:229` |

### 3.2 🔶 还没做完、但有明确解除路径的（9 项，按 ROI 排序 —— 这是"还能做"的全部）

| # | 缺口 | 现状 | 解除路径 | 预估成本 |
|---|---|---|---|---|
| 1 | **fs2you:// 解码** | ~~全仓 0 命中~~ → ✅ **已完成**（附录 B：`core/src/source_parse/fs2you.rs`，10+2 测） | 已解除 | — |
| 2 | **Windows 速度查询** | XL_QueryTaskFlow 3 参签名待补 | 真机 dump 法确认签名后补 `query.rs` | 0.5 天（需真机） |
| 3 | **XLTaskInfo 未知字段** | +0x38..+0x53 与 task_state=9 未知 | 补真机 dump，扩展 `query.rs` 常量表 | 0.5 天（需真机） |
| 4 | **B 级 DCDN 凭证注入** | ~~未封装~~ → 🟡 **封装完成·UNTESTED**（附录 B：四导出 Option 解析 + identity 封装） | 真机 dump 校准 c_int + VIP/试用账号 | 1 天（需真机+账号） |
| 5 | **XL_SetUserInfo 绑定修复** | bindings.rs 整型传参有 ABI 错配崩溃风险 | dump 确认真实参数类型后修绑 | 0.5 天（需真机） |
| 6 | **torrent multipart 直传** | ~~端点存疑~~ → ✅ **代码完成**（`client.rs::torrent_upload` 双通道：magnet URL 通道实测稳 + form 直传 B 级 `enable_form_upload` 开关） | 登录态实测确认 form 端点（B 级形状已实现） | 0.5 天（需登录态） |
| 7 | **云盘搜索** | ~~鉴权头未取证~~ → 🟡 **代码完成·UNTESTED**（`cloud_search.rs` 纯函数 + 网络方法 + 6 测；三要素头按 api-pan 同构推断） | 登录态实测确认鉴权头 | 0.5 天（需登录态） |
| 8 | **分享匿名取链 → 登录态取链** | ~~网络链路未通~~ → 🟡 **登录态链路代码完成·UNTESTED**（2026-08-30：`share.rs::list_with_auth`/`resolve_with_auth` + `verify_pass_code_authed`，复用实测验证过的三要素头，URL 构造提为纯函数 + 5 新测；匿名链路保留作对照） | 登录态实测（对症解匿名链路的 `400 no client info found`） | 0.5 天（需登录态） |
| 9 | **FTP 下载** | ~~项目侧无 FTP 客户端~~ → ✅ **早已完成（本文档此前标注有误，2026-08-30 核实纠正）**：`httpdl::FtpEngine`（PASV/REST 续传/421 退避/目录下载，M4c）+ daemon `--features ftp` 注册 + 路由全链路（`state.rs::parse_ftp_auth` → `EngineKind::Ftp` 槽位）+ 5 个测试文件全绿 | 无（feature 门控启用 `--features ftp`） | — |
| 附 | macOS 引擎绑定 | 静态还原 60%（`TAG_XL_TASK_INFO_EX` 卡点） | §4.1 路线：30 行 C 程序 hex dump → 1 天窗口，超时永久止损 | 1 天窗口 |
| 附 | 短信登录端到端收尾 | verify 步已验，活体收尾**待验证** | 新鲜验证码走一遍 | 0.5 天 |
| 附 | 账密登录风控 | signin 全链代码完备，被滑块 `result:review` 阻塞 | xl_al 指纹移植（**可选，风控对抗不承诺**） | 2 天+ |

> 汇总（2026-08-30 晚更新）：**9 项中 4 项已完成（#1 fs2you / #6 torrent 双通道 / #9 FTP / #8 登录态代码）**；其余 5 项全部卡在「实测输入」而非代码（真机 dump ×3、登录态 ×1、VIP 票据 ×1）——**代码面已 100% 收口，剩余为验证债**。

### 3.3 ✅ 已经做完的（24 项速览，细节见上游矩阵）

thunder:// / qqdl:// / magnet 解码、设备码扫码登录（端到端双验）、短信 send/verify、captcha_sign 三件套、网盘列表、直链提取（MD5 逐字节验证）、8 线程并行、分享纯函数、离线提交/轮询、配额档位、.torrent 上传、GCID/CID/BCID、token 刷新、Windows FFI 全套、真机 P2SP+BT 验证、匿名 FreeDCDN 开关、身份注入三件套、fastresume 迁移（piece SHA1 命中 99.1%）、web seed 混合加速、云兜底 M6、直链分类器 202 host 表。

---

## 4. 结论与建议

1. **"整个迅雷 App"里属于下载能力的部分**：能做的已全部做完；剩余 9 个 🔶 缺口中代码已全部收口（4 项完成 + 5 项 UNTESTED 待实测），汇总约 3.5 个工作日的**验证债**（需真机/账号/样本），完成后客户端可见面覆盖率从 ≈95% 提升至 ≈98%（剩余为 ❌ 服务端授权面，不属于工程欠账）。
2. **你给的安装包不改变现状**：25.0.90.1592 载荷与已分析样本 MD5 一致；SDK 引擎版本指纹 `2.86.202.127`（传输层 `0.1.202.1754` / 64 位 SDK `12.0408.960.1` / OpenSSL 1.0.2v / libcurl 7.61.1）已归档，作为未来版本漂移检测的基线。
3. **建议执行顺序**（2026-08-30 更新）：~~#1 fs2you → #9 FTP~~（均已完成）→ 代码面已收口，转入**实测解锁序列**：① 用户提供试用票据 → VIP 两项实测；② 一次登录态会话 → B 级三端点 + 短信收尾；③ 一次 Windows 真机会话 → dump 系列 3 项；④ （可选）macOS 1 天窗口（超时止损）。
4. **范围边界**：App 外围（UI/播放器/同步盘/商店）按 D28 永不进入项目；L3 私有加速 7 项永不可做。这两块的"没做"是**决策结果**，不是**工程欠账**。

---

## 附：本次实测产物索引

| 产物 | 路径 |
|---|---|
| 官方安装包 | `/home/z/my-project/xunlei-25/XunLeiWebSetup25.0.90.1592xl11.exe` |
| 资源提取脚本 | `/home/z/my-project/xunlei-25/dump_resources.py` |
| 7z 载荷解包脚本 | `/home/z/my-project/xunlei-25/extract_payload.py` |
| 载荷解包目录 | `/home/z/my-project/xunlei-25/payload/r203`（57 文件）、`/payload/r204`（34 模块） |
| 既有逆向样本（MD5 相同） | `scripts/research/xunlei/extracted/resource_1288_1296.bin` / `_1304.bin` |
| 上游权威矩阵 | `docs/research/xunlei/CROSS_PLATFORM_UNIVERSAL_SOLUTION.md` |

> 合规声明：与上游文档 §5 一致，本次分析仅针对公开分发安装包做互操作性研究（资源枚举、载荷清点、版本指纹），不逆向未公开加密、不分发专有二进制。

---

## 附录 A：❌ 7 项再审（2026-08-30 追问：「永不可做 7 项能做吗？」）

> 方法论：把「做」拆成两条独立路线——**A 自研实现**（不依赖迅雷二进制与服务端授权）与 **B 黑盒/等效用上**（复用官方引擎二进制或标准栈替代），逐项重新判定。结论与 §3.1 分级有差异的，以本附录为准。

### A.0 重审结论速览

| # | 能力 | A 自研 | B 用上/等效 | 终判 |
|---|---|:---:|:---:|---|
| 1 | PHub/SHub 私有 P2P | ❌ 三重锁死 | ✅ 标准栈等效已做 | **真死路**（唯一） |
| 2 | DCDN/FreeDCDN 自研 | ❌ 协议未公开 | ✅ **Windows 已用上**（dcdn.rs 匿名通道） | 自研死路；能力已在手 |
| 3 | VIP TrySpeed/SuperSpeed | ❌ 配额服务端发放 | 🔶 **有条件可做**（VIP 账号 + 证书注入已封装） | 可解锁（需 VIP 账号） |
| 4 | VIP DCDN token 通道 | ❌ 同上 | 🔶 **可封装**（4 个 B 级导出已定位） | 可解锁（需 VIP 账号 + dump） |
| 5 | uDT 私有传输 | ❌ 无公开规格 | ✅ 黑盒隐式已用（引擎自带 XUdt） | 自研死路；无需自研 |
| 6 | SDK 内账号登录 | **能力不存在**（非"难"，是"无"） | ✅ L1 云登录已完全替代 | 绝对死路，且需求已解决 |
| 7 | cid_store.dat | —（本地格式，非协议） | 🔶 **技术可做，可解封** | 唯一纯技术可解锁项（1-2 天） |

**一句话**：7 项中真正"绝对做不到"的只有 **#1 协议自研** 与 **#6 能力不存在** 两项；**#3/#4 只差一个 VIP 测试账号**；**#7 可直接解封开工**；#2/#5 的能力你实际上已经在用（Windows 黑盒），只是不能脱离官方二进制独立存在。

### A.1 逐项重审

**#1 PHub/SHub/DPHub 私有 P2P —— 唯一真死路**
- 自研五重锁定（加密/协议/工程/风控/先例）在 §3.1 已给全证据，无新增破解面。
- Frida 运行时窃取会话密钥（hook `XPF_RandomBytes`）技术上可探测，但违反合规边界 #2（不绕过加密与鉴权），且产出依赖常驻官方客户端 = 失去独立客户端意义 → **明确不做，永久排除**。
- 等效面已做：标准 BT swarm（与 qB/Transmission 互通）+ F5.1 web seed CDN 叠加 + M6 云兜底。损失仅"迅雷私有 swarm 加成"。

**#2 DCDN/FreeDCDN —— "不能自研"≠"用不上"**
- Windows 线 `dcdn.rs` 已封装 `XL_EnableFreeDcdn`（UserID=0 免登录），**匿名 DCDN 加速能力已在手**（这是 L2 FFI 的附带行为，非 L3 协议自研）。
- 非 Windows 平台以 BEP-19 web seed 等效。自研协议面维持 ❌（D28）。

**#3 VIP TrySpeed/SuperSpeed —— 有条件可做，差一个 VIP 账号**
- 配额（trial_left_times/trial_key）由服务端发放，伪造 = ❌（维持）。
- 但"用上"路径已半就绪：`identity.rs` **SetAccelerateCertification（A 级）已封装**，可向引擎注入加速证书；缺的是 VipSpeedUpUrl 参数形状（需一次真机会话抓官方客户端请求）。
- 合限定性：以**用户自己的 VIP 账号**走官方客户端同款接口 = 账号授权使用（非破解、非绕过）；频率友好遵循 §5-6。**"提取成免 VIP 能力"不可做也不做**。
- 解锁条件：你提供 VIP 测试账号 → 预估 1 天（抓包 + 接线 + 验证）。

**#4 VIP DCDN token 通道 —— 与 #3 同源，更近一步**
- B 级 4 导出已定位（EnableDcdnWithToken/Session/VipCert、SetTaskEquityToken），即 §3.2 缺口 #4；VIP 账号 + 参数 dump 后封装即通。
- 解锁条件同 #3；两者可同一真机会话一并完成。

**#5 uDT 私有传输 —— 自研无意义**
- 自研 ❌（56 类 uTP-like 无公开规格，成本"极高"，维持）。
- Windows 走引擎时 XUdt/TcpImpl **本来就是传输层**（黑盒隐式已用）；httpdl 为我们的独立等效替代。自研一份 uDT 换不来任何新增用户能力 → 维持不做。

**#6 SDK 内账号登录 —— 不是"难做"，是"不存在"**
- 205 个导出、0 个登录函数、0 条登录端点字符串（`sdk_export_inventory.md:9-16`）——考古已封死，任何工程投入都无法创造不存在的导出。
- 且其服务需求已被 L1 云登录**完全满足**（设备码端到端双验 + 短信 send/verify + OAuth 刷新）。此项维持 ❌，但性质应表述为"**绝对死路且无需做**"。

**#7 cid_store.dat —— 唯一纯技术可解锁项，建议解封**
- 性质澄清：这是**本地文件格式**（非协议、无服务端准入问题），与 xlbt.cfg/.bt.xltd 同族——后两者已逆向成功（TLV 结构，piece SHA1 命中 99.1%），方法论与工具链可直接复用。
- 解封后价值：导入用户旧迅雷安装的 hash 缓存 → 结合已实现的 GCID/CID/BCID 算法 → 秒传识别迁移（旧文件免重校验）。
- 前提缺口：云端 hash→资源查询端点**待验证**（可先用离线提交通道部分替代）。
- 预估 1-2 天；建议列为 **P3 待办**，需求出现即做。

### A.2 决策更新（对 §3.1 的修正）

- §3.1 表头"永不可做"精确化为：**自研路线永不可做 5 项（#1/#2/#3/#4/#5）+ 能力不存在 1 项（#6）+ 决策搁置 1 项（#7，技术可做）**。
- 新增待办：**P3-1 cid_store.dat 逆向与秒传迁移**（1-2 天）；**P3-2 VIP 通道黑盒接线**（1-2 天，**前置条件：用户提供 VIP 测试账号**；无账号则永久挂起）。
- 合规重申：Frida 密钥窃取路线不做（边界 #2）；VIP 路线以自有账号为前提（边界 #5）；不对抗风控（#3 若遇滑块即止，同账密登录处理）。

---

## 附录 B：解锁进度（2026-08-30 当日晚间批次，用户指示「能做到的可以做了」）

| 项 | 解锁状态 |
|---|---|
| 缺口 #1 fs2you 解码 | ✅ **完成**（`core/src/source_parse/fs2you.rs` + normalize 路由，10+2 测） |
| 附录 A #3 VIP TrySpeed/SuperSpeed | 🟡 **代码就位·UNTESTED**（`provider/src/xunlei/vip_speedup.rs`：check_status 形状已验 + get_info/apply/cert 形状假设，8 mock 测） |
| 附录 A #4 VIP DCDN token | 🟡 **封装完成·UNTESTED**（`xunlei-ffi` 四导出 Option 解析 + identity 封装，待 dump 校准 c_int） |
| 附录 A #7 cid_store.dat | 🟡 **假设解析器就位**（`xunlei-convert/src/cid_store.rs` 三形态自适应 + `cidstore_scan.py`，待真实样本校准） |
| 附录 A #3/#4 解锁前置条件 | 等用户提供**试用/VIP 票据**（普通账户试用即可：`trial_left_times` 配额与登录态绑定，设备码登录后即可查询） |
| 缺口 #8 分享登录态取链 | 🟡 **登录态链路代码完成·UNTESTED**（2026-08-30 晚：`share.rs::list_with_auth`/`resolve_with_auth`/`verify_pass_code_authed`，URL 构造提纯函数，provider 115 测全绿） |
| 缺口 #9 FTP | ✅ **核实为早已完成**（文档此前误标，engine/routing/测试全在，`--features ftp` 启用） |

**#1 为什么做不了（PHub）—— 三句话版本**：① 服务器用 RSA-1024 包裹**每个请求随机生成**的 AES 密钥，密钥不存在于任何本地文件，离线推导在数学上不成立；② 协议有 300+ 命令字且必须与中心服务器协同（P2P 网络由服务器准入，不是开放 swarm）；③ 即便全部解决，服务端校验 peer 身份，非官方客户端会被封禁，投入随时作废。唯一"技术可行"路径是 Frida 运行时窃取会话密钥，但这违反本项目合规边界第 2 条（不绕过加密与鉴权），明确不做。

**#6 是什么（SDK 内账号登录）—— 三句话版本**：当年假设"下载引擎 DLL 里也许藏着登录功能，逆向它就能免云 API 登录"。考古结果：两套 DLL 共 205 个导出函数里**一个登录函数都没有**，登录端点字符串零命中——迅雷的架构里登录本来就不在下载 SDK 内，而在云端 API。所以不是"难做"，是"这个功能根本不存在"；它的实际需求（登录账号）早已由 L1 云登录（设备码扫码端到端验证通过）满足。

---

## 附录 C：#1 追问——PHub 属于迅雷结构哪部分 +「能不能直接调用现成组件」（2026-08-30 深夜，导出表实证）

> 用户追问：「这部分属于迅雷结构的哪部分？我们能不能直接调用现成的组件？」
> 新增取证：① P2P 模块族导出表全量盘点（`scripts/research/xunlei/p2p_module_export_check.py`）；② `DownloadDispatcher.xta` 文件头定性。取证脚本与结论均已入库。

### C.1 PHub 在迅雷结构中的确切位置

```
迅雷下载引擎（客户端侧，全部在我们已解包的 r204 34 模块内）
└─ DownloadSDK.dll（宿主 / 引擎本体 2.86.202.127）—— 静态导入下方全部模块
    ├─ P2PBase.dll            Lua 运行时（60 导出全为 XLLRT_*：CreateRunTime/LuaState/Chunk）
    ├─ P2PFramework.dll       框架服务层（XPF_*：AddressCache/AuthenticationCache/Certification…）
    ├─ P2PTarget.dll          Lua 包加载器（XPF_LoadTargetPackage ← XLTP 容器）
    ├─ P2PIO.dll / P2PCommonObjects.dll
    ├─ 传输插件   TcpImpl.dll（TCP，9 导出）/ XUdt.dll（UDP，37 导出）/ Http.dll（26 导出）/ Ftp.dll
    │              —— 共同特征：仅 Init/Uninit/RegisterInLua，注册进 Lua 环境
    ├─ P2PStat.dll            统计上报（XLSTAT4_*，与 r203 的 xlstat4.dll 同源）
    └─ xar/DownloadDispatcher.xta   1.04MB 任务调度脚本包（XLTP 容器 + Lua 5.1 字节码，未加密）

云端侧（不在安装包内，无法从客户端取得）
└─ PHub / SHub / DPHub 中心服务器集群（peer 准入、RSA-AES 会话、300+ 命令字协商、peer list 分发）
```

**一句话**：PHub 交互逻辑 = 客户端 Lua 脚本层（DownloadDispatcher.xta）+ XPF 原生框架/加密服务 + 传输插件，宿主是 DownloadSDK.dll；对端是迅雷云端中心服务器。它属于 §2 三层拆解中的 **L3 私有加速层**，物理载体是 **L2 引擎的 P2P 插件栈**。

### C.2 「直接调用现成组件」三条路判定

| 路线 | 判定 | 证据 |
|---|:---:|---|
| **① 整栈调用官方引擎**（加载 DownloadSDK 全家桶，P2P 栈随宿主自动拉起） | ✅ **已经在用** | `xunlei-ffi/src/loader.rs`：加载 Proxy→SDK 全套；引擎静态导入自动挂载 P2P 栈 → BT/磁力任务自动参与迅雷私有 swarm；`dcdn.rs` 匿名 DCDN 通道同属此路线。局限：仅 Windows + 官方二进制齐全；P2P 是引擎内部细节，只能"受益"不能"指挥" |
| **② 单独抠出 P2P 模块直接调用** | ❌ **调用面不存在（今日实证）** | 三层证据见下 |
| **③ 脱离官方二进制（Linux/跨平台原生参与迅雷 swarm）** | ❌ | 即 #1 本意，五重锁死维持（附录 A） |

路线②的三层实证：
1. **导出面全是基础设施接口，零业务入口**：P2PBase 60 导出全为 Lua VM 管理（`XLLRT_CreateEnv`/`GetLuaState`…）；P2PFramework 全为对象生命周期（`AddRef`/`Release`/`GetClassData`）；TcpImpl 仅 9 导出（`Init`/`Uninit`/`RegisterInLua`）；Http 26 导出全为请求包字段读写器。**没有任何一个"连接 PHub / 下载 / 取 peer"级别的可调用函数**。
2. **宿主静态耦合**：DownloadSDK.dll 静态导入 P2PBase **322** 函数 + P2PFramework **263** + P2PIO 57 + XUdt 32 + P2PTarget 8——任务上下文、账号凭证、下载会话全部由宿主灌入，模块脱离宿主连初始化数据都拿不到。
3. **即便手工模拟宿主拉起栈**，下一步仍撞 PHub 协议本身（RSA-1024 每请求随机 AES key + 中心服务器准入 + 风控）——绕一圈回到五重锁死。

### C.3 新发现：P2P 栈 = Lua 脚本驱动架构（架构档案）

- `DownloadDispatcher.xta`（1,041,924 B）：魔数 `XLTP`（XunLei Target Package），熵 **5.59**（未加密、未压缩），偏移 2453 处命中 **Lua 5.1 字节码签名** `\x1bLua`——与 P2PBase（Lua 运行时）+ P2PTarget（`XPF_LoadTargetPackage`）+ 传输插件 `RegisterInLua` 互证：**迅雷的调度/命令字逻辑主要住在 Lua 脚本层**，原生层提供加密与传输原语。
- 含义①：解释了迅雷为何能热更维护 300+ 命令字（脚本层下发）。
- 含义②：理论上可用 unluac/luadec 反编译 Lua 5.1 字节码还原调度逻辑，**但对 PHub 自研无解锁价值**——加密在 XPF 原生层（`XPF_Certification`/Authentication 族）、密钥协商在服务端，知道命令字也造不出合法请求。列为 **P3 档案项**（可选，仅在需要补全架构档案时做）。

### C.4 对决策的影响

- **#1 终判不变，证据升级**：从"协议五重锁死"升级为"**协议五重锁死 + 调用面被架构设计排除**"双重锁定。用户问题的最终答案：**能调用的方式（整栈黑盒）我们已经在用；不能调用的方式（单独调用 P2P 模块）被迅雷的插件架构从导出面设计排除——不存在"还没试过的调用方式"。**
- §1.3 中 xta 行定性已同步更新（P3 档案项）。

---

## 附录 D：三连追问——依赖关系 / 跨平台根源 / 黑盒全量盘点（2026-08-30，代码实证）

> 用户追问：「DownloadSDK.dll 所以整个项目主要是依赖这个是吧？所以跨平台的问题也是出自这个对吧？其他还有没有黑盒的地方？」
> 代码证据：`crates/daemon/Cargo.toml`（`xunlei = ["smart-dl-btcore/xunlei"]` 可选 feature）、`crates/btcore/src/lib.rs:16-25`（feature 门控）、`crates/core/src/router.rs:31-53`（热度路由）、`crates/btcore/src/xunlei_engine.rs:1-6`（Windows-only 声明）。

### D.1 DownloadSDK.dll 不是脊柱，是可插拔外挂

**依赖关系与直觉相反**：项目主体是 100% 自研 Rust 栈，DownloadSDK.dll 只在 `feature = "xunlei"` 下被编译进去（Windows-only）。不开这个 feature，项目一行迅雷 DLL 都不加载，HTTP/FTP/BT/磁力/云能力完整可用。

```
smart-downloader
├─ 自研主线（跨平台，零迅雷二进制）≈ 项目 95%
│   ├─ core            协议解码/路由/任务/会话（thunder://, magnet, fs2you, ed2k, ftp…）
│   ├─ httpdl          HTTP/FTP 多线程引擎（分段/重试/断点/校验）
│   ├─ btcore          BT/磁力主引擎（libtorrent 开源内核 + 自研调度）
│   ├─ provider        云端服务（迅雷云登录/网盘/直链/离线/分享/搜索 + 夸克）
│   ├─ xunlei-convert  fastresume 迁移（xltd/xlbt_cfg/cid_store）
│   └─ daemon          HTTP/WebSocket 服务
└─ Windows 加速插件（可选 feature「xunlei」）← 唯一依赖 DownloadSDK.dll 处
    └─ xunlei-ffi → DownloadSDKProxy.dll → DownloadSDK.dll 全家桶
       匿名模式（UserID=0）借道 BT/Tracker/DHT/FreeDCDN；路由器按热度选引擎，冷门走云兜底
```

### D.2 跨平台问题的根源

- **主线跨平台没有问题**（Rust + libtorrent 天然三平台）。
- **"借迅雷加速"这层的跨平台问题 100% 出自官方二进制**：Windows PE（已绑定 ✅）、macOS dylib（静态还原 60%，等真机 1 天窗口）、Android .so（侦察后止损）；Linux 官方从不发布引擎 → 无原材料，此路不存在。
- 定性：跨平台欠账 = 「想借私有加速就必须逐平台逆向官方二进制」的欠账，不是本项目架构欠账。架构应对即 D.1 的分层：主线自研保跨平台，迅雷层做成可插拔。

### D.3 黑盒全量盘点（三类）

**A. 运行时黑盒**（仅一处）：DownloadSDK 全家桶 34 模块——内部调度（Lua 层）、P2P/XUdt/DCDN 传输、DownloadSDKServer.exe 命名管道服务。FFI 只控制输入/输出，内部不可见也无需可见。

**B. 协议黑盒**（对端不可见）：云 API 风控/配额规则（滑块阈值、TrySpeed 发放逻辑、频控）；sign/captcha 算法族已逆向自研但可能随版本漂移（"已解开但会漂移"的活黑盒）。

**C. 假设区**（已写代码、待校准，全部有 UNTESTED/待验证标注）：

| 假设区 | 校准手段 |
|---|---|
| XLTaskInfo +0x38..+0x53、task_state=9 | 真机 dump |
| XL_QueryTaskFlow 3 参签名 | 真机 dump |
| XL_SetUserInfo 绑定 ABI（崩溃风险） | 真机 dump |
| B 级 DCDN 4 导出 c_int 参数 | dump + VIP/试用账号 |
| vip_speedup get_info/apply/cert 形状（check_status 已验） | 试用票据 |
| cid_store 假设解析器 | 真实样本 |
| cloud_search 鉴权头 / share 登录态取链 / torrent 直传 form.url | 登录态抓包 |

**排除项**（不是黑盒）：httpdl 全部、btcore 主路径（libtorrent 开源）、core 协议解码、登录三链路（端到端已验）、fastresume 迁移（piece SHA1 99.1%）。`btcore/src/xudt/` 为自研 uDT 帧编解码研究件，未接入下载主路径，定位是分析/迁移工具件而非黑盒依赖。
