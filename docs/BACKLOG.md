# 未实现清单（整体）— 迅雷 + 通用 + 未来愿景

> 更新：2026-08-27。已完成主线（thunder 解码 / HTTP 断点续传 / BT / fastresume / 热重载 / 状态推进）+ C 类通用缺口中的**代理 + 引擎层限速**（3fac8e3）+ **M6 云兜底调度接线**（c19313a）+ 2026-08-25 四卡落地：**FTP 目录下载**（httpdl 引擎 + daemon 路由）、**BT DHT/LSD/UPnP 配置开关**（FFI `lt_apply_discovery` 全链路）、**.torrent 多文件空间预检**、**TaskSnapshot.files 透出** + 2026-08-27：**httpdl 动态分段（P0，方案A）**（109692c）。
> **迅雷云盘线现状一句话**：F2/F2.1 的「私有流格式」前提已被考古证伪推翻，F3/F5 PoC 均已通过，主线进入 **Rust 实装 + 活票验收**阶段。
> **已完成事项的集中档案见 [`IMPLEMENTED.md`](IMPLEMENTED.md)**（行为契约/配置/API/验证证据）。
> 本文档 = 一切**未实现**事项的总清单。排序：迅雷云盘线（当前主线）→ 研究尾巴 → 通用缺口 → 明确排除 → 未来愿景。

## A. 迅雷云盘线（当前主线，按依赖排序）

> 2026-08-25 大修：F2/F2.1 的「私有流格式」前提被推翻——铁证见 [`NOTES_F3_DIRECT_DOWNLOAD_POC.md`](../scripts/research/cloud_delivery/NOTES_F3_DIRECT_DOWNLOAD_POC.md) §2（26MB 全量 MD5 与云端逐字节一致 = 原始文件，「任何加密/转封装/私有容器假设被彻底排除」），[`RECON_PRIVATE_FORMAT.md`](../scripts/research/cloud_delivery/RECON_PRIVATE_FORMAT.md) 交叉印证（前置侦察）。旧 NOTES_STREAM_FORMAT 的「私有容器实锤」判定作废，样本随档移入 `_ARCHIVE/obsolete_docs/`。

| # | 项 | 状态 | 前置/证据 |
|---|---|---|---|
| F2 | 浏览器 cookie 方案验证（播放流类型：若 HLS 则避开私有格式）| ✅ **已由 F3 PoC 覆盖关闭**：`usage=PLAY` → `web_content_link` 即原始直链，无私有流；多线程 Range 有效（8 线程 823KB/s）| [NOTES_F3](../scripts/research/cloud_delivery/NOTES_F3_DIRECT_DOWNLOAD_POC.md) |
| F2.1 | 私有流格式逆向（`FFmpeg.Service` 容器解密——云盘直下硬前置）| ✅ **以否定结论关闭**：不存在私有容器；证据 = NOTES_F3 §2 + [RECON_PRIVATE_FORMAT](../scripts/research/cloud_delivery/RECON_PRIVATE_FORMAT.md)（旧判定已证伪）| — |
| F3 | 云盘直下 PoC（网页 API 路径：列表→PLAY→下载→校验）| ✅ **PoC 已通过（2026-08-19）**：26MB 全量 MD5 逐字节一致；单连接 ≈155KB/s、8 并发 ≈823KB/s、16 并发部分被拒（免费档饱和 ≈1MB/s）| [NOTES_F3](../scripts/research/cloud_delivery/NOTES_F3_DIRECT_DOWNLOAD_POC.md) |
| F3.1 | Provider 实装活票验收（resolve → 直链传输端到端）| ✅ **已收尾**：Bug A（暂停不持久）已修复并实测通过；fallback 功能点已由手动触发验证（provider 返回 transferred=1）；当日迅雷配额耗尽（error_code 11），标准化脚本的 fallback+MD5 段待后续配额刷新后补跑 | F3 |
| F3.2 | 云盘在线解压探测（`/decompress/v1/{list,decompress,progress}`，S1-G2 发现；「云端预览/选择性下载」能力候选）| 📋 **只读探测已做（2026-08-25）**：端点形状已从 m_134.js 还原（list 为 POST + 新发现第 4 端点 `/download`）；但活体探测全变体 404——路由未挂载在 api-pan/api-gateway-pan 两已知网关，真实网关待浏览器抓包。端点形状/参数猜测表/实录见 [`DECOMPRESS_API.md`](research/xunlei/DECOMPRESS_API.md)（**禁调 `/decompress` 写操作**）| S1 扫描 |
| F4 | 云盘直下（原生取流 API 路径，备选）| ❌ 未做（维持备选不变）| F3.1 失败才考虑 |
| F5 | P2SP 混合加速 PoC（libtorrent BT + 云盘直链并行）| ✅ **PoC 已通过（Python 层，2026-08-19）**：BEP-19 web seed 与迅雷 CDN 完全兼容、BT + 多 web seed 速度叠加（100+155=255KB/s 实测）、零自研调度器；直链 URL query 含防篡改签名（`at=`）**禁止改动**——多源只能靠多次取链，不能复制改参 | [NOTES_F5](../scripts/research/cloud_delivery/NOTES_F5_P2SP_HYBRID_POC.md) |
| F5.1 | Rust 实装：`POST /tasks/:id/webseeds` 注入端点（web seed 注入 + 在线换链）| ✅ **已完成（2026-08-25）**：端点 + `BtEngine::add_url_seed` → FFI `lt_add_url_seed` 全链 + e2e 本地 HTTP 源测试（详见 [`IMPLEMENTED.md`](IMPLEMENTED.md) 2026-08-25 批次 #4）；实装要点仍有效：直链时效 ≈1h 需定时/403 触发换链（NOTES_F5 §4）；客户端链需 UA 含 `Thunder` 才放行（NOTES_F5 §8）。旧注「进行中（另一代理并行开发中）」已过时 | F5 |

## B. 迅雷研究尾巴

- EOF 补验（有效直链 + 真实完整下载场景；当前有效直链 206 干净未复现）— 📌 留档
- E80630C5 身份（本地 11 视频 GCID 全不匹配，无法确证）— 📌 归档
- 离线创建配额 privilege 端点实测备忘：`CREATE_OFFLINE_TASK_LIMIT` 档位表 **free=3 / ordinary=100 / platinum=100 / super=500 / super.year=1000**（来源：实测响应 JSON，2026-08-25）；同日观测**今日配额烧尽事件 `error_code 11`**——做离线任务配额管理/降级策略时直接引用 — 📌 归档
- ~~PHub 加密文档收口（v2 MD5 公式作废，v3 RSA-wrapped random AES key 勘误 + XUDT 密钥结论并入主文档）~~ **已完成（2026-08-27）**：`p2p_recon_complete.md` / `p2p_research_complete.md` / `PROGRESS_REPORT_v3.md` / `RESEARCH_STATE.md` 顶部加勘误条；`xunlei_engine_research.md` 新增「XUDT 加密密钥」段落 + PHub 加密说明

## C. 通用下载器缺口（今天核实）

| 缺口 | 说明 |
|---|---|
| ~代理支持~ | ~~无（HTTP/BT 均无代理配置项）~~ **已完成**：HTTP（reqwest Proxy）+ BT（lt_apply_network）双引擎接线，启动时生效（见 3fac8e3）|
| ~引擎层限速~ | ~~无（BT 的 libtorrent 速率上限未接线；HTTP 无限速）~~ **已完成**：全局下载/上传限速（KiB/s；0=不限），HTTP 跨段共享 RateLimiter（见 3fac8e3）|
| ~云兜底调度接线~ | ~~FallbackCoordinator（M2 设计）仅在 provider crate 测试里使用，daemon 无调度入口~~ **已完成（M6）**：`POST /tasks/:id/fallback` 手动兜底——BT 任务暂停且进度 <50% → 选 provider → 直链 → HttpEngine 传输 → 任务 Completed；`[provider]` 配置段（mock 占位，真实 provider 待迅雷线落地）|
| ~Provider 探活失败自动降级~ | ~~Provider 探活失败会阻塞主链路 / 手动兜底失败后无自动切换~~ **已完成（2026-08-27）**：`XunleiProvider` 内部失败冷却（Auth 5 分钟 / Quota 1 小时 / 其他 1 分钟）；`FallbackCoordinator::begin_fallback` 支持多 provider 依次尝试；`RemoteProvider::probe()` 轻量探活（默认 `Ok(())`）|
| FTP 目录下载 | ~~无（仅单文件）~~ **已完成（2026-08-25）**：httpdl 引擎目录递归展开 + daemon `add_ftp_task` 路由打通（`POST /tasks` 可达 `ftp://` 目录 URL），目录任务按多文件下发 |
| ~快照 files 字段透出~ | ~~多文件任务快照只见总量不见明细~~ **已完成（2026-08-25）**：`TaskSnapshot.files` 透出每个子文件的路径/大小/进度 |
| ~xunlei-import 端到端测试~ | ~~`POST /tasks/xunlei-import` 代码存在但无 e2e 测试~~ **已完成（2026-08-27）**：新增 `crates/daemon/tests/xunlei_import_api.rs`，覆盖合法样本导入、bad base64、xltd 数量不匹配 |
| ed2k 协议 | 明确不支持（解码出的 ed2k 链接 → 路由拒绝，报"ed2k 不支持"）——**挪远期**：完整实现 = eMule/eDonkey 客户端协议（数周级），并入"跨协议"远期专项（见 F 段）|

已有（防重复列）：并发队列（BT≤3/HTTP·FTP≤8）、HTTP 多连接并行/镜像/换源、**HTTP 动态分段（SegmentManager 动态领取 + 流式写盘，`109692c`）**、**失败缩小粒度重试（`b70923e`）**、**backup_url/backup_md5 备用源兜底（`963f9dd`）**、sha256 可选校验、BT 校验/做种停止、事件队列背压、全局代理 + 双引擎限速（启动时生效）。

### 手动验证待办（脚本已备，待人工在真实网络执行）

> 2026-08-25 四卡（DHT/LSD/UPnP 开关、FTP 目录路由等）的自动化单测均已绿；以下两项依赖真实互联网/本地代理环境，留人工收尾。

| # | 项 | 脚本 | PASS 标准 | 状态 |
|---|---|---|---|---|
| G1 | magnet DHT 冷启（enable_dht on vs 全关对照）| `scripts/manual/G1_dht_coldstart.ps1` | DHT-on run 找到 `num_peers > 0`；全关对照 run 保持 0 peer | ☐ 待人工跑（需真实互联网）|
| G2 | 代理 live 转发（任务流量走指定代理出口）| `scripts/manual/G2_proxy_live.ps1` | 任务到达 Completed 且代理客户端侧可见对测试主机的会话 | ☐ 待人工跑（需本地代理）|

## D. 明确排除（决策，不开发）

- 迅雷私有 P2P 加速引擎（D28：纯 libtorrent；且 PHub 离线不可实现、DCDN 接入协议未公开）

## E. 未来愿景（仅记录，不讨论设计）

> 用户 2026-08-18：之后要做**夸克网盘**、**百度网盘（112 链接）**的转换支持；它们**貌似也有 BT 下载能力**（现在不做）。
> 架构要求：**设计留出空间**——未来可通过**其他服务商发现更多节点**来加速下载（多源节点发现）。
> 当前主线不变：仍以迅雷（云盘处理）为主。

- 夸克网盘转换
- 百度网盘 112 链接转换
- 多服务商节点发现 → 加速下载（架构预留扩展点）

### F0. 能力地图与客户端分析总纲

> 全量分档、净增量四项、云解析审查单见 [CAPABILITY_MAP.md](CAPABILITY_MAP.md)。BitComet 三问挂靠其 §三-B 已派条目。

### F. 远期专项：BitComet 冷门能力复刻（排在迅雷云盘线 + 主线能力完成后）

> 2026-08-18 官方资料审计结论（已确认级）：BitComet 冷门优势不来自核心 BT 协议（标准互通），
> 而来自 **① LT-Seeding（私有协议 + BitComet 中心服务器 + 文件级 LT-hash 发现 peer，普通 libtorrent 完全无法利用）
> ② Torrent Exchange（用户间交换 torrent 列表，私有生态）③ eMule/Kad 跨协议**（同一任务混源）。
> 完整复刻 LT-Seeding 不现实（中心服务器 + 私有协议 + 生态积累 + ToS/隐私风险）；现实路径 =
> eMule/Kad 开源库集成 + magnet/DHT metadata 加强 + 按内容 hash 跨种子复用（类似 BiglyBT Swarm Merging 方向）。

- 开发时第一件事：**审查官方文档**（BitComet changelog / Wiki / 源码 / 协议实现），把 LT-Seeding 与 Torrent Exchange 的具体要点考证清楚后写进本文档（当前不深挖，审计方也未给出协议细节）
- 待考证 3 问（开发时查证）：
  1. LT-Seeding 完整协议流程（LT-hash 算法/发现服务器/请求参数/peer 认证/handshake/piece 请求/部分文件/跨 torrent）
  2. Torrent Exchange 机制（谁↔谁、协议载体、交换的是 metadata 还是索引）
  3. 文件级 hash 跨 torrent 内容复用是否真实（若真 → Content Swarm + Smart Scheduler 架构机会）
- 前置条件：迅雷云盘线（F2-F5）+ C 类通用缺口补齐后启动

## 环境限制（非功能缺口）

- Kaspersky 锁 daemon lib 单测 exe → 只能用集成测试（`--test X`）
- bt_api 并行偶发 flaky（libtorrent 多 session 并行）→ 重跑即绿
- http_api 曾偶发 1 用例失败：重负载窗口（连续重建 + 杀软扫描新 exe）把轮询测试的 10s 等待击穿。**已修复**：三处等待护栏 10s→60s（快照/list/事件；语义不变，仅抗进程级停顿；已实测 6 轮强制 rebuild 首跑 + 50+ 次运行全绿）
