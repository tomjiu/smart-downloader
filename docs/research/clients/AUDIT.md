# 闭源客户端云解析成果审计报告

> 审计对象：`docs/research/clients/` 三包云 AI 解析结果  
> 审计人：Harness agent  
> 日期：2026-08-27  
> 依据：`docs/CAPABILITY_MAP.md` §四 审查单（行为证据 / 机制假说 / 可复刻映射 / 成本 / 立项建议）

---

## 1. 归档清单

```
docs/research/clients/
├── _zips/
│   ├── bitcomet_accel_toolkit.zip              # r1 (188 KB)
│   ├── bitcomet_accel_toolkit(1).zip           # r2 (331 KB)
│   └── multi_downloader_analysis.zip           # 5 客户端综合 (5.25 MB)
├── bitcomet/
│   ├── r1/                                     # 首轮解析（保留审计基线）
│   └── r2/                                     # 二轮增强（审计主视角）
└── multi_downloader/
    ├── README.md                               # 总览
    ├── worklog.md                              # 41 KB 协作日志
    ├── analysis/                               # 6 份架构分析
    │   ├── 01_qbittorrent/qbittorrent_architecture.md
    │   ├── 02_filecentipede/filecentipede_architecture.md
    │   ├── 03_flashget/flashget_architecture.md
    │   ├── 04_tixati/tixati_architecture.md
    │   ├── 05_quark/quark_architecture.md
    │   ├── 06_comparison/cross_client_comparison.md
    │   └── 07_rust_proto/multi_downloader/      # 36 文件 Rust 原型
    └── reversing_evidence/
        ├── quark/zipres_extracted/ + dll_extracted/
        └── strings_dump/ (tixati 1.5MB, quark 11KB, dynsym)
```

---

## 2. bitcomet r1 vs r2 去重结论

| 维度 | r1 | r2 | 结论 |
|------|----|----|------|
| 总文件数 | 22 | 42 | r2 为严格超集 |
| 源码节点 | 19 `.py` | 29 `.py` | r2 新增 10 个代码节点 |
| docs | `README.md` 10 KB | `README.md` 12 KB + `ANALYSIS.md` 73 KB + `INTEGRATION.md` 49 KB | r2 含 3 份文档，r1 仅 1 份 |
| tests | 未独立列出 | `tests/test_all.py` 27 / 27 PASS | r2 补全测试基线 |
| artifacts | `artifacts_symbols/` 未列出 | `artifacts_symbols/bitcomet_symbols.txt` 1.27 MB demangled | r2 新增逆向符号库 |
| scripts | 未列出 | `scripts/` 若干 | r2 新增自动化脚本 |

字节级比对：r1 `README.md` 与 r2 `README.md` 前 ~10 KB 内容相同；r2 在此基础上追加第四轮深度逆向（存储/过滤/恢复层）和第五轮汇总。

**审计策略：只基于 r2 出结论，r1 仅作变更基线存档。**

---

## 3. 质量评级

| 包 | 评级 | 理由 |
|----|------|------|
| **bitcomet r2** | **A** | 29 个 Python 节点全部标注逆向来源（demangled 符号 + 字符串证据 + API 端点）；ANALYSIS.md 1818 行、13 章主报告 + 十一~廿二共 12 轮深度逆向；tests 27/27 PASS；INTEGRATION.md 给出 A/B/C 三种集成模式；结论与优先级矩阵可落地。扣分点：P2SP/LT-Seed 云端协调器依赖自建服务器，P0 项实施周期 2–4 周且含外部依赖。 |
| **multi_downloader** | **A-** | 5 客户端架构分析均附 reversing_evidence（strings、dynsym、PE 资源提取）；横向对比 579 行含设计原则、技术栈、模块架构、算法摘要、安全隐私对比；Rust 原型 36 文件 / 8 模块 / 90+ unit tests，结构完整。扣分点：BT 引擎仍为 trait placeholder（未接 librqbit/libtorrent-rs）；部分分析基于公开资料而非二进制逆向（FlashGet）。 |

---

## 4. 三问覆盖表

> 三问：LT-Seeding 协议 / Torrent Exchange / 跨 torrent hash 复用

### 4.1 bitcomet r2

| 问题 | 覆盖度 | 关键证据 | 代码节点 |
|------|--------|----------|----------|
| **LT-Seeding 协议** | **Full** | `P2spLtSeedManager` 完整方法清单（`lt_query_add_one_file` / `lt_client_cancel` / `switch_to_other_file` 等 14 个方法）；ltseed 配置项 5 个；`passport-client.bitcomet.com:25476/25477` 云端协调；6 种消息类型 QUERY_SEED / RESPONSE / REQUEST_PIECE / PIECE_DATA / ANNOUNCE / HEARTBEAT；HTTP (25432) + UDP 双协议 | `lt_seed_protocol.py`, `lt_seed_cloud_client.py` |
| **Torrent Exchange** | **Partial** | `TorrentShareQueryWrapper::rest_succeed` / `TorrentShareSubmitWrapper::submit_torrent_file+content`；`HTTPShareQueryWrapper` / `HTTPShareAnnounceWrapper::announce`；API 端点 `/api/task/connections/get` 等 5 个；`peer_discovery_extender.py` 云端 peer 注入到 libtorrent。**缺口**：未形成独立 BEP-7 风格规范，exchange 语义嵌入在 P2SPClient 云端信令中，未单独解耦为可移植协议模块。 | `peer_discovery_extender.py`（含 CloudPeerAnnouncer） |
| **跨 torrent hash 复用** | **Full（LT-Seed 上下文）** | `lt_file_t::sha1_t file_hash` 定义为"整个文件的 SHA-1 (40 hex)，不是 BT 的 piece SHA-1"；`lt_seed_cloud_client.py` 按 file_hash 查询/上报，天然跨 info_hash；`P2spLtSeedManager::switch_to_other_file` 允许单 client 服务多 file。**注意**：BitComet 的 eMule 多源集成 (`emule_p2sp_integration.py`) 也体现文件级 hash 复用（ed2k hash）。 | `lt_seed_protocol.py`, `lt_seed_cloud_client.py`, `emule_p2sp_integration.py` |

### 4.2 multi_downloader

| 问题 | 覆盖度 | 说明 |
|------|--------|------|
| **LT-Seeding 协议** | **None** | 未涉及 BitComet 私有 LT-Seed 协议；Rust 原型 `engine::bt_engine` 为 placeholder，注释指向 librqbit/libtorrent-rs 集成路径。 |
| **Torrent Exchange** | **None** | 5 客户端对比中仅抽象提及"peer 发现"；Tixati 的 DHT/PEX/ introducer 属于标准 BT 拓扑发现，不等价于 BitComet TorrentShare 云端交换。 |
| **跨 torrent hash 复用** | **Indirect** | `storage::piece_store.rs` 以 SHA-256 校验 piece；`core::task.rs` 的 `DownloadTask + Slice` 按 task_id 隔离。无文件级全局 hash 索引设计，需后续在 Rust 调度层补充。 |

---

## 5. 质量红旗 /  caveats

1. **Python 节点未经真实 libtorrent 集成编译验证**：tests 为协议 round-trip + import 级，未连 lt_session 跑 alert 回调压测。需 Phase 2 做 C++ plugin 原型验证。
2. **BCSP 端点可访问性未验证**：`passport-client.bitcomet.com:25476/25477` 来自符号 + 字符串，未实测连通性；BitComet 服务端是否仍在线未知。
3. **Torrent Exchange 机制依赖 BitComet 云端**：如果云端下线，`peer_discovery_extender.py` 的 CloudPeerAnnouncer / TorrentShareQuery 链路整体失效，需 fallback 到标准 DHT+PEX。
4. **multi_downloader reversing_evidence 不完整**：夸克仅分析 installer + mini_install.dll，未覆盖主程序；Tixati 基于 strings + lief 符号，无IDA/Frida动态行为验证。
5. **LT-Seed 客户端认证双因素未闭环**：`bc_passport_protocol.py` 实现用户名密码 + device token，但云端积分体系 (`update_score`) 真实交互未演示。

---

## 6. Top 行动建议（按 ROI 排序）

| 优先级 | 行动 | 来源 | 预估投入 |
|--------|------|------|---------|
| **P0** | 把 `anti_leech_filter.py` 移植为 libtorrent plugin C++ 版本（`lt::plugin::on_alert` 中接 peer_alert） | 4.3 + INTEGRATION.md 3.3 | 1 周 |
| **P0** | 将 `lt_seed_protocol.py` 的二进制封包/解包（magic+version+msg_type+payload）翻译为 Rust，接入本项目的 BT 调度层作为"死种救场"独立通道 | 4.2 + 11.6 | 2 周 |
| **P0** | 自建轻量 LT-Seed 协调服务（REST，基于 `lt_seed_cloud_client.py` 的 12 端点子集），仅保留 submit_ltseed + query_ltseed + heartbeat，不上线积分体系 | 11.6 | 3 周（含服务端） |
| **P1** | 把 `peer_broadcast_optimizer.py` 的批量去重 + `PeerExchangeDiff` 增量 PEX 逻辑移植到 Rust BT tick loop，替代 libtorrent 默认 PEX | 4.5 + 11.2 | 1 周 |
| **P1** | 将 multi_downloader `bt::peer_score.rs`（Tixati 7 字段加权）和 `bt::unchoke.rs`（3 模式）接入 librqbit 的 peer 回调 | cross_client_comparison 3.5.1–3.5.4 | 1 周 |
| **P1** | 把 `bclink_url_parser.py` 移植为 Rust `engine::protocol` 的 URL 路由层（支持 magnet/HTTP/FTP/eD2k/bclink 7 协议分流） | 4.6 + INTEGRATION.md 4.3 | 3 天 |
| **P2** | 以 `adaptive_disk_cache.py` + `disk_cache_priority.py` 为参考，评估替换 libtorrent default_storage 的 ROI（目前看风险中等，暂不立项） | 4.7 + 11.4 | 调研 3 天 |
| **P2** | 将 `repeater_ws_protocol.py` 作为独立 sidecar 服务评估（NAT 穿透收益 vs 运维成本） | 11.5 | 调研 1 周 |
| **P3** | 把 `emule_p2sp_integration.py` 的 ed2k 多源逻辑封装为独立 eD2k source trait，作为非 BT 协议的扩展接口 | 14.8 | 2 周（需 ed2k 链接可达性） |

---

## 7. multi_downloader Rust 原型可移植摘要

| Rust 模块 | 源分析 | 建议 |
|-----------|--------|------|
| `bt::peer_score` | Tixati peer_score 7 字段加权 | **直接复用**，6 个测试已覆盖 |
| `bt::unchoke` | Forced/Random/Charity 三模式 | **直接复用**，4 个测试 |
| `bt::bandwidth` | 5 层分配（Global/Trading/Seeding/Auto/Quota） | **直接复用**，4 个测试 |
| `bt::autothrottle` | RTT-driven LEDBAT-like | **直接复用**，5 个测试 |
| `bt::connection` | 11-stage FSM | **直接复用**，4 个测试 |
| `engine::mirror` | FlashGet 加权评分 `speed×0.6 + 1/latency×0.3 + reliability×0.1` | **直接复用**，3 个测试 |
| `core::state_machine` | Quark 7-stage FSM | **直接复用**（HTTP 下载任务态） |
| `storage::resume_db` | SQLite WAL 持久化 | **直接复用**（替代 FlashGet `.jc!`） |
| `sniffer::rule_engine` | FileCentipede 3 层规则 | **直接复用**（浏览器扩展前可先用 in-process） |
| `net::tls` | rustls + aws_lc_rs + webpki-roots | **已落地**，生产就绪 |

---

## 8. 审计结论

两包均达到 **A / A-** 水准，核心行为均有可复刻的映射路径（Python 原型或 Rust 原型），不满足 CAPABILITY_MAP §四 中"立项建议"门槛的仅有：

- Torrent Exchange 的云端依赖（BitComet 服务端存续风险）
- multi_downloader 中 BT 引擎未实装（依赖 librqbit 集成节点）

其余 P0 / P1 模块均可按建议直接进入 Phase 2 移植排期。
