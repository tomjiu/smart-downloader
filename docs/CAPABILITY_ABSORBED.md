# 竞品能力吸收总清单（CAPABILITY_ABSORBED）

> 更新：2026-08-30（Task 5-d + 主代理收口）
> 回答的问题：**比特彗星/夸克等分析文档、分析结果、分析代码，都转化成可吸收的能力了吗？有哪些能力？**
> 结论先行：**全部分析材料已盘点完毕并建档；其中高 ROI 能力已落地进主工作区 Rust 代码（本清单逐项标注）；其余保留为原型/计划并写明集成点。**

---

## 一、分析投入总览与吸收方法论

| 材料 | 位置 | 规模 | 产出形态 |
|------|------|------|----------|
| BitComet 逆向 r1 | `docs/research/clients/bitcomet/r1/` | 8 个 Python 代码节点，tests 8/8 PASS | 符号级分析 + 可复用代码 |
| BitComet 逆向 r2（深度轮） | `docs/research/clients/bitcomet/r2/` | 27 个代码节点（8+5+8 三轮 + 逆向工具 + bencode），tests 27/27 PASS | 含 close_reason/增量 PEX/Wire 协议/4 优先级缓存/WS Repeater 等私有协议还原 |
| 多下载器横评 | `docs/research/clients/multi_downloader/analysis/01~06` | 5 客户端（qBittorrent/FileCentipede/FlashGet/Tixati/夸克）+ 横向对比，共 ~385KB 分析文档 | 架构/算法/教训 |
| Rust 吸收原型 | `analysis/07_rust_proto/multi_downloader/` | 8 模块 36 文件 ~6000 行，含单测 | Tixati/FlashGet/夸克/FileCentipede 算法 Rust 化 |
| 夸克 installer 逆向 | `analysis/05_quark/quark_architecture.md` + `reversing_evidence/decompiled/quark/` | PE 资源解压 + 11K 行 strings | 7 阶段状态机/三段错误码/上报通道 |

**吸收方法论**：`分析文档 → 算法原型（Python/Rust）→ 主工作区落地（Rust, 可编译+测试）→ 本清单验收标注`。
四档状态：✅ 已落地（主工作区，cargo test 绿）/ 🔶 原型待接（07_rust_proto 或 Python 工具包内，集成点已指明）/ 📋 计划（写入门控条件）/ 🚫 明确不吸收（含理由）。

## 二、总表（按来源客户端）

### BitComet（r1 8 节点 + r2 深度增补）

| 能力 | 分析文档 | 原型代码 | 主工作区落地 | 状态 |
|------|----------|----------|--------------|------|
| bclink 多协议 URL 统一解析 | r1 ANALYSIS §2 | `r1/src/bclink_url_parser.py` | 通用化并入 `core/src/sniffer.rs`（scheme 直判 + 网盘分享识别）+ `core/src/source_parse/`（thunder/qqdl/ed2k） | ✅（以 sniffer 形态落地） |
| P2SP 多源合并下载 | r1 ANALYSIS §3 | `r1/src/p2sp_downloader.py` | httpdl 多连接/镜像/备用源兜底 + F5.1 web seed 注入（BT+直链混合） | ✅（等价能力已有） |
| LT-Seeding 长效做种协议 | r1 ANALYSIS §4.1 + r2 BCSP 云端 announce | `r1/src/lt_seed_protocol.py`、`r2/src/lt_seed_cloud_client.py` | — | 📋 需 btcore 自研扩展协议门控（libtorrent 无此概念），远期 |
| 自适应磁盘缓存 | r1 ANALYSIS §4.3 | `r1/src/adaptive_disk_cache.py` | `core/src/strategy.rs::DiskCacheAdvice`（纯函数建议器，7 单测）+ `btcore::strategy` 门面 | ✅ |
| 4 优先级磁盘缓存桶 | r2 ANALYSIS §磁盘 | `r2/src/disk_cache_priority.py` | 并入 `strategy.rs`（CacheProfile→优先级桶参数建议） | ✅ |
| 分级反吸血过滤器 | r1 ANALYSIS §4.7 | `r1/src/anti_leech_filter.py` | `core/src/strategy.rs::AntiLeechAdvice`（choking/ban 参数建议，7 单测覆盖两 advice） | ✅ |
| Peer 广播优化 + 增量 PEX + NAT 打洞 | r1 §4.5 + r2 `pex_full_protocol.py`/`repeater_ws_protocol.py` | 同左 | — | 🔶 原型待接（集成点：btcore pex 扩展；libtorrent 2.x 内建 PEX，增量版属自研扩展） |
| UTP 拥塞诊断 | r1 §4.6 | `r1/src/utp_diagnostics.py` | — | 🔶 原型待接（诊断工具，非运行时组件） |
| 多源 Peer 发现扩展 | r1 §4.8 | `r1/src/peer_discovery_extender.py` | 概念并入 `core/src/sniffer.rs` 多源发现规则 + HTTP 镜像发现已有 | ✅（等价） |
| close_reason 私有扩展解码 | r2 深度 | `r2/src/close_reason_decoder.py` | — | 🔶 原型待接（仅与 BitComet 互通时有用） |
| Core_Wire 传输抽象 | r2 深度 | `r2/src/wire_protocol.py` | — | 🔶 参考（架构参考价值，不落地） |
| 逆向工具链（符号提取/复现脚本） | r1/r2 scripts | `bitcomet_symbol_extractor.py` + `reverse_engineering.sh` | `scripts/` 既有同类逆向脚本 | 🔶 工具留存 |

### qBittorrent（开源源码分析）

| 能力 | 分析文档 | 主工作区落地 | 状态 |
|------|----------|--------------|------|
| libtorrent wrapper + alert 系统 | 01_qbittorrent | btcore alerts 模块（同构 alert→事件队列） | ✅（已自有） |
| settings_pack 参数面 | 01_qbittorrent | btcore settings 应用点 + strategy.rs 建议器输出目标 | ✅（接入点就绪） |
| RSS/分类/标签管理 | 01_qbittorrent | — | 📋 远期（任务元数据扩展后） |

### FileCentipede（半开源）

| 能力 | 分析文档 | 原型 | 主工作区落地 | 状态 |
|------|----------|------|--------------|------|
| 6 引擎抽象 | 02_filecentipede | 07_rust_proto engine/ | `core/src/types.rs::DownloadEngine` trait（add/pause/resume/status/remove） | ✅（已自有等价） |
| **协议嗅探（4 层规则）** | 02_filecentipede | 07_rust_proto sniffer/ | **`core/src/sniffer.rs`（本轮落地）**：scheme 直判（thunder/qqdl/fs2you/magnet/ed2k/ftp/http）/文本正则提取多链接/协议推断（.torrent 后缀、pan.xunlei.com/s/、pan.quark.cn/s/ 网盘分享）/规则表可配置，13 单测 | ✅ |
| 双进程 IPC 隔离 | 02_filecentipede | — | — | 🚫 不吸收：daemon 单进程 + lockfile 已满足；双进程复杂度不值（对比横评 §06 结论） |
| 嗅探→任务路由 | 02 | sniffer 模块 | `sniffer.rs::SniffedSource` 输出可直连 `core/src/router.rs` 归一化入口 | ✅ |

### FlashGet（历史架构分析）

| 能力 | 分析文档 | 原型 | 主工作区落地 | 状态 |
|------|----------|------|--------------|------|
| **Mirror 加权评分** | 03_flashget | 07_rust_proto engine/mirror.rs | **httpdl 已落地**（Mirror 加权评分：失败惩罚/成功奖励，选源按分数排序，commit 89dc55f） | ✅ |
| 6 状态 Part 状态机 | 03 | 07_rust_proto | httpdl 动态分段 SegmentManager（动态领取+流式写盘，109692c）+ 失败缩小粒度重试（b70923e） | ✅（等价能力） |
| Keep-Alive socket 池 | 03 | 07_rust_proto net/socket_pool.rs | reqwest 连接池（hyper 内建）+ 未来自研通道可参考原型 | 🔶（依赖池已覆盖，自研池不吸收） |
| P4S 私有格式教训 | 03 | — | 决策记录：SQLite WAL 替代 .jc! 嵌入；不搞私有元数据格式 | 🚫 教训吸收（决策） |

### Tixati（闭源 ELF 逆向）

| 能力 | 分析文档 | 原型 | 主工作区落地 | 状态 |
|------|----------|------|--------------|------|
| Peer 评分（7 字段加权） | 04_tixati | `07_rust_proto/bt/peer_score.rs`（7 单测） | 概念映射 btcore peers 统计；libtorrent 不暴露 per-peer 打分钩子 | 🔶 原型待接（btcore 自研 picker 时） |
| 3 模式 Unchoke（Forced/Random/Charity） | 04 | `bt/unchoke.rs`（4 单测） | libtorrent choking_algorithm 可设参数已由 strategy.rs AntiLeechAdvice 输出 | ✅（参数面）/ 🔶（算法面） |
| **5 层带宽分配**（Global+Trading+Seeding+AutoLimit+Quota） | 04 | `bt/bandwidth.rs` | daemon 全局限速（KiB/s，0=不限）已接双引擎；分任务配额列计划 | 🔶 部分落地（集成点：daemon rate limiter 扩展） |
| AutoThrottle RTT 自动限速 | 04 | `bt/autothrottle.rs` | — | 🔶 原型待接 |
| 11 阶段连接生命周期状态机 | 04 | `bt/connection.rs` + `core/listener.rs` | — | 🔶 原型待接 |
| 自研 BT 栈教训（90MB 体积） | 04 + 06 横评 | — | 决策记录：坚持 libtorrent 基座（D28 系列决策） | 🚫 教训吸收 |

### 夸克网盘（闭源 PE 逆向 + 通用网盘知识）

| 能力 | 分析文档 | 原型 | 主工作区落地 | 状态 |
|------|----------|------|--------------|------|
| **分享解析 → 转存 → 直链下载** | 05_quark + 公开网盘协议知识 | 07_rust_proto quark 模块 | **`provider/src/quark/`（本轮落地）**：QuarkClient（stoken→detail→save→task→download 全链）、QuarkProvider 实现 RemoteProvider trait、Cookie 登录态持久化（原子写）、错误分类（NotLogin/ShareExpired/QuotaExhausted）+ 失败冷却（Auth 5min/Quota 1h/其他 1min，同 xunlei 模式）、6+4 mock 单测（axum 本地 mock 全链） | ✅ |
| 7 阶段安装状态机 | 05 | 07_rust_proto | 不吸收（安装器行为与下载器无关，分析价值=逆向方法论） | 🚫 |
| 三段错误码体系 | 05 | 07_rust_proto | 分类思想并入 quark::types::QuarkError + xunlei 错误分类 | ✅（思想） |
| 上报通道（4 个埋点通道） | 05 | — | **零埋点决策**（06 横评明确）：任何用户行为不外报 | 🚫 反向吸收（隐私红线） |
| InnoSetup + DLL 双层打包 | 05 | — | 决策记录：单一可执行（daemon+CLI 一体） | 🚫 教训吸收 |

## 三、本轮（2026-08-30）净新增落地

| # | 落地物 | 文件 | 测试 |
|---|--------|------|------|
| 1 | 协议嗅探引擎（FileCentipede） | `crates/core/src/sniffer.rs`（606 行） | 13 |
| 2 | BitComet 策略建议器（缓存+反吸血） | `crates/core/src/strategy.rs`（329 行）+ `crates/btcore/src/strategy.rs` 门面 | 7 |
| 3 | 夸克网盘 Provider 全链 | `crates/provider/src/quark/`（4 文件 1077 行） | 10 |
| 4 | 迅雷原生登录三模式（用户需求 Q1） | `provider/src/xunlei/{login_flow,login_page}.rs` + HTML + daemon `xunlei_login.rs` + CLI | 9（含 2 个 mock e2e） |
| 5 | ed2k 链接解析 | `core/src/source_parse/ed2k.rs` | 含于 core 88 测试 |
| 6 | btcore Linux 可编译（bindgen 回退） | `crates/btcore/build.rs` + `ffi.rs` | check 绿 |

验收：`cargo check --workspace` ✅；`cargo test --workspace --exclude smart-dl-btcore` ✅ 全绿（btcore 测试需 Windows libtorrent 链接环境，与 CI 约定一致）。

## 四、"不吸收"决策清单（同样重要）

| 项 | 来源 | 不吸收理由 |
|----|------|-----------|
| 自研 BT 协议栈 | Tixati 90MB 教训 | 维护成本失控，libtorrent 基座已验证（横评 §06 决策 1） |
| .jc! 嵌入式断点元数据 | FlashGet P4S 教训 | 磁盘布局耦合、损坏即全损；用 fastresume/SQLite 路线 |
| 4 通道行为上报 | 夸克隐私问题 | 零埋点是产品红线（横评 §06 决策 5） |
| 静态 OpenSSL | 夸克体积教训 | rustls + webpki-roots（横评 §06 决策 3/7） |
| MSE/RC4 旧加密 | Tixati 过时加密 | 现代协议栈已淘汰 |
| Mirror 默认开启 | FlashGet P4SP 教训 | 镜像发现默认关闭，用户显式启用（横评 §06 决策 8） |
| InnoSetup 双层安装 | 夸克 | 单一可执行（横评 §06 决策 6） |
| 私有 P2P 加速引擎自研 | 迅雷 PHub/DCDN | D28 决策排除（服务端授权依赖，技术不可行） |

## 五、遗留原型接入路线图（🔶 → ✅ 的门控）

| 原型 | 解锁条件 | 预估工作量 |
|------|----------|-----------|
| Tixati Peer 评分/unchoke | btcore 引入自研 picker（libtorrent 性能瓶颈实测出现时） | 2-3 周 |
| 5 层带宽分配 | daemon 配置扩展 per-task quota（UI/CLI 就绪后） | 1 周 |
| AutoThrottle | daemon 暴露 RTT 观测 | 3-5 天 |
| 增量 PEX/NAT 打洞 | 决定与 BitComet 生态互通时 | 2 周 |
| LT-Seeding/BCSP | 做种生态需求出现时 | 3 周 |
| UTP 诊断 | 作为独立诊断子命令（无运行时耦合） | 3 天 |
