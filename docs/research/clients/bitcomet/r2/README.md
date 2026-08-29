# BitComet 加速设计逆向分析与实现工具包

> 基于 BitComet 2.21.2 Linux x86_64 deb 包完整逆向，识别并实现 **8 个可移植到 qBittorrent-based 自研下载器** 的加速设计。

## 1. 项目背景

我们正在基于 qBittorrent 的引擎（libtorrent-rasterbar）自研下载器。本工具包通过逆向 BitComet 2.21.2 闭源客户端，识别出 qBittorrent 缺失但 **对下载加速有益** 的独特设计，并用 Python 实现了可直接复用的代码节点。

每个代码节点都：
- 标注了逆向来源（demangled 符号 + 配置字符串）
- 实现了核心算法
- 提供了 CLI 入口或可集成 API
- 通过了单元测试

## 2. 工具包结构

```
bitcomet_accel_toolkit/
├── src/                                # 27 个核心代码节点 (8 + 5 + 8 三轮深度逆向 + 1 逆向工具 + 1 bencode)
│   ├── bclink_url_parser.py            # 多协议 URL 统一解析器
│   ├── p2sp_downloader.py              # P2SP 多源合并下载器
│   ├── lt_seed_protocol.py             # BitComet LT-Seeding 协议
│   ├── adaptive_disk_cache.py          # 自适应磁盘缓存
│   ├── anti_leech_filter.py            # 分级反吸血过滤器
│   ├── peer_broadcast_optimizer.py     # Peer 广播优化 + 增量 PEX + NAT 打洞
│   ├── utp_diagnostics.py              # UTP 拥塞控制诊断
│   ├── peer_discovery_extender.py      # 多源 Peer 发现扩展
│   ├── close_reason_decoder.py         # [深度] BitComet 私有 close_reason 扩展
│   ├── pex_full_protocol.py            # [深度] 完整 seq/ack 增量 PEX
│   ├── wire_protocol.py                # [深度] Core_Wire 传输抽象层
│   ├── disk_cache_priority.py           # [深度] 4 优先级磁盘缓存
│   ├── repeater_ws_protocol.py          # [深度] WebSocket Repeater NAT 穿透
│   ├── lt_seed_cloud_client.py         # [深度] LT-Seed 云端 announce (BCSP 协议)
│   └── bitcomet_symbol_extractor.py    # 自动化逆向工具 (复现本次工作)
├── tests/
│   └── test_all.py                     # 全部测试 (27/27 PASS)
├── docs/
│   ├── ANALYSIS.md                     # 完整分析报告 (13 章)
│   └── INTEGRATION.md                  # 集成到 qBittorrent 的指南
├── scripts/
│   └── reverse_engineering.sh          # 复现完整逆向流程
├── examples/
│   ├── p2sp_demo.py                    # 多源下载演示
│   ├── lt_seed_server_demo.py          # LT-Seed 服务端
│   └── libtorrent_integration.py       # libtorrent 集成示例
└── README.md
```

## 3. 快速开始

```bash
# 安装可选依赖
pip install aiohttp psutil

# 运行所有测试
python3 tests/test_all.py

# 自动化逆向 (复现本次工作)
python3 src/bitcomet_symbol_extractor.py \
    --deb BitComet-2.21.2-x86_64.deb \
    --qbittorrent-src /path/to/qBittorrent \
    -o ./analysis_output

# P2SP 多源下载示例
python3 src/p2sp_downloader.py \
    -o output.bin \
    http://mirror1.example.com/file.bin \
    http://mirror2.example.com/file.bin \
    ftp://mirror3.example.com/file.bin

# LT-Seed 服务端 (作为种子源暴露文件)
python3 src/lt_seed_protocol.py serve /path/to/my_file.bin --port 25432

# AntiLeech 演示
python3 src/anti_leech_filter.py --level 4
```

## 4. 14 个加速设计 - 优先级矩阵

| # | 模块 | 加速效果 | 实现难度 | 移植优先级 | qBittorrent 缺失原因 |
|---|------|---------|---------|------------|-------------------|
| 1 | **P2SP 多源合并** | ⭐⭐⭐⭐⭐ | 中 | P0 | qBittorrent 仅 BT+HTTP webseed，无 FTP/多镜像合并 |
| 2 | **LT-Seeding 协议** | ⭐⭐⭐⭐⭐ | 高 | P0 | qBittorrent 无 P2P-CDN，死种场景无法救场 |
| 3 | **AntiLeech 过滤器** | ⭐⭐⭐⭐ | 低 | P0 | qBittorrent 不识别迅雷等吸血客户端 |
| 4 | **多源 Peer 发现** | ⭐⭐⭐⭐ | 中 | P1 | qBittorrent 仅依赖 tracker+DHT，无云端 peer 源 |
| 5 | **Peer 广播优化** | ⭐⭐⭐ | 中 | P1 | qBittorrent 默认 HAVE 逐条发送，无批量/去重 |
| 6 | **多协议 URL 解析** | ⭐⭐ | 低 | P1 | qBittorrent 不支持 ed2k/bc:// 等链接 |
| 7 | **自适应磁盘缓存** | ⭐⭐⭐ | 中 | P2 | qBittorrent 用 libtorrent 内置缓存，不可定制 |
| 8 | **UTP 诊断** | ⭐⭐ | 低 | P2 | qBittorrent 无法观测 uTP 内部状态 |
| 9 | **close_reason 扩展** | ⭐⭐ | 低 | P1 | BEP-14 标准仅 6 个原因，调试不友好 |
| 10 | **完整 seq/ack 增量 PEX** | ⭐⭐⭐⭐ | 中 | P0 | qBittorrent PEX 流量大，无重传 |
| 11 | **Core_Wire 传输层** | ⭐⭐⭐ | 中 | P1 | qBittorrent 无统一传输抽象 |
| 12 | **4 优先级磁盘缓存** | ⭐⭐⭐ | 中 | P1 | qBittorrent 用单一 LRU，无 LT-Seed hot piece 优先 |
| 13 | **WebSocket Repeater NAT** | ⭐⭐⭐⭐ | 高 | P0 | qBittorrent 对称 NAT 后无法远程访问 |
| 14 | **LT-Seed 云端 announce** | ⭐⭐⭐⭐ | 中 | P0 | qBittorrent 无账户系统，无法云端协调 |

> **P0** = 立即移植，对下载速度影响显著
> **P1** = 短期移植，提升 P2P 体验
> **P2** = 长期观察，可定制化优化

## 5. 与 libtorrent (qBittorrent 引擎) 集成方式

本工具包的设计原则是 **不修改 libtorrent 本身**，而是作为旁路加速层叠加在 qBittorrent 之上：

```
┌────────────────────────────────────────────────────────┐
│  自研下载器 UI 层 (Qt6 / WebUI)                         │
├────────────────────────────────────────────────────────┤
│  bitcomet_accel_toolkit (本工具包)                       │
│  ┌──────────────────────────────────────────────────┐  │
│  │ bclink_url_parser    ← 用户输入 URL 预处理         │  │
│  │ anti_leech_filter    ← peer_alert 回调 hook       │  │
│  │ peer_broadcast_opt   ← have/cancel/batch_send    │  │
│  │ peer_discovery_ext   ← 注入额外 peer 给 libtorrent│  │
│  │ utp_diagnostics      ← 旁路监控 uTP socket        │  │
│  │ adaptive_disk_cache  ← 替换 libtorrent 默认 cache │  │
│  │ p2sp_downloader      ← 替代部分 HTTP webseed 逻辑 │  │
│  │ lt_seed_protocol     ← 死种救场协议 (可选)        │  │
│  └──────────────────────────────────────────────────┘  │
├────────────────────────────────────────────────────────┤
│  libtorrent-rasterbar (qBittorrent 引擎, 不修改)        │
└────────────────────────────────────────────────────────┘
```

**集成示例** (libtorrent 集成模式)：

```python
import libtorrent as lt
from bitcomet_accel_toolkit import (
    AntiLeechFilter, AntiLeechLevel,
    PeerBroadcastOptimizer, BtMsg,
    UtpDiagnostics, LibtorrentPeerInfoAdapter,
)

# 创建 libtorrent session
ses = lt.session()
ses.listen_on(6881, 6891)

# 1. AntiLeech 过滤器
antileech = AntiLeechFilter(level=AntiLeechLevel.LIMIT)

# 2. Peer 广播优化器
broadcast_opt = PeerBroadcastOptimizer(
    send_callback=lambda ep, mt, payload: ses.broadcast_piece_message(ep, mt, payload)
)

# 3. UTP 诊断
utp_diag = UtpDiagnostics()
utp_diag.add_alert_callback(lambda a: print(f"[UTP Alert] {a['msg']}"))

# 4. peer_alert 回调 hook
def on_peer_alert(alert):
    handle = alert.handle
    for peer_info in handle.get_peer_info():
        # AntiLeech 决策
        action = LibtorrentPeerInfoAdapter.update_from_lt_peer_info(
            antileech, peer_info
        )
        if action.name == "DISCONNECT":
            handle.disconnect_peer(peer_info)
        # UTP 诊断
        LibtorrentPeerInfoAdapter.update_from_lt_peer_info(
            utp_diag, peer_info
        )

# 注册 alert handler
ses.set_alert_callback(on_peer_alert)

# 5. 定期采样 UTP
import threading
def sample_loop():
    while True:
        utp_diag.sample()
        time.sleep(1.0)
threading.Thread(target=sample_loop, daemon=True).start()
```

## 6. 逆向方法学

本工具包不是凭空设计的，而是基于 BitComet 2.21.2 二进制的系统性逆向：

1. **下载验证**：35,426,504 字节，与 HTTP `Content-Length` 完全匹配
2. **解压**：`dpkg-deb -R` 提取得到 70MB GUI + 49MB daemon + 资源
3. **关键发现**：二进制 **未 strip 且带 debug_info**
4. **符号提取**：`nm -C BitComet` 提取 109,727 个 demangled 符号
5. **分类**：识别 2,929 个 C++ 命名空间，16167 个 BitComet 专属符号
6. **字符串提取**：`strings BitComet` 提取 110 个 REST API 端点 + 50+ 配置项
7. **代码节点实现**：每个节点对应明确的命名空间和符号

完整复现命令：
```bash
bash scripts/reverse_engineering.sh
```

## 7. 测试覆盖

```
============================================================
BitComet Accelerator Toolkit — Full Test Suite (含深度逆向节点)
============================================================
[1/14] Import test:                14/14 modules OK
[2/14] bclink_url_parser:          5/5 protocols parsed
[3/14] p2sp_downloader:            piece planning OK
[4/14] lt_seed_protocol:           encode/decode roundtrip OK
[5/14] adaptive_disk_cache:        LRU + flush OK, 100% hit rate
[6/14] anti_leech_filter:          XunLei detected + banned
[7/14] peer_broadcast_optimizer:   5 peers got HAVE message
[8/14] utp_diagnostics:            rate sampling OK
[9/14] close_reason_decoder:       4 BitComet private + BEP-14 compat OK
[10/14] pex_full_protocol:         100 added → 0 diff → 10 dropped, seq/ack OK
[11/14] wire_protocol:            attach/close_reason/queue/tracker bucket OK
[12/14] disk_cache_priority:       LT-Seed hot piece 自动升级 LT_SEED_HOT
[13/14] repeater_ws_protocol:      encode/decode + 3-mode punch decision OK
[14/14] lt_seed_cloud_client:      REST_Package encode/decode + endpoint map OK
All tests passed (14/14).
============================================================
```

## 8. 详细分析报告

完整的逆向分析、设计原理、对比表、移植建议请见 **[docs/ANALYSIS.md](docs/ANALYSIS.md)**。

集成到 qBittorrent 的具体步骤请见 **[docs/INTEGRATION.md](docs/INTEGRATION.md)**。

## 9. 已知限制

| 限制 | 说明 |
|------|------|
| libtorrent fork | BitComet 的 `Core_BitTorrent::libtorrent` 是私有 fork，**不**包含在本工具包；自研下载器继续使用上游 libtorrent |
| WebKit2GTK 依赖 | BitComet 用 wxWidgets+WebKit2GTK；建议自研下载器继续用 Qt6 |
| CometID 账户体系 | 需要服务端基础设施，本工具包仅提供客户端协议层 |
| LT-Seed 云端 | 本工具包提供客户端+服务端协议，但需要自建协调服务器 |

## 10. 许可与归属

- 逆向分析：基于 BitComet 2.21.2 (Apache License 2.0 类似, 商业可用)
- 代码实现：Z.ai BitComet Reverse Engineering Team, MIT License
- 上游对比：qBittorrent (GPLv2/GPLv3)

工具包不包含 BitComet 的源代码或反编译产物，仅包含基于公开符号表设计的独立实现。
