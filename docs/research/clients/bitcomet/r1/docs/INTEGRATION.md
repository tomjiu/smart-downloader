# 集成到 qBittorrent-based 自研下载器指南

本指南说明如何把本工具包的 8 个加速设计集成到基于 qBittorrent 引擎（libtorrent-rasterbar）的自研下载器中。

## 1. 集成架构概览

```
自研下载器
├── UI 层 (Qt6 Widgets / WebUI)
│   └── 调用 accel_toolkit 的 Python 接口
├── accel_toolkit (本工具包, Python)
│   ├── bclink_url_parser         ← 用户输入 URL 预处理
│   ├── anti_leech_filter         ← libtorrent peer_alert 回调 hook
│   ├── peer_broadcast_optimizer  ← 替换 libtorrent 默认广播
│   ├── peer_discovery_extender   ← 注入额外 peer
│   ├── utp_diagnostics           ← 旁路监控
│   ├── adaptive_disk_cache       ← 替换 libtorrent 默认 cache (可选)
│   ├── p2sp_downloader            ← 处理非 BT 协议 (HTTP/FTP/eD2k)
│   └── lt_seed_protocol          ← 死种救场协议
└── libtorrent-rasterbar (引擎, 不修改)
    └── 通过 libtorrent Python binding 调用
```

## 2. 三种集成模式

### 模式 A：Python 包嵌入（推荐快速原型）

把整个 toolkit 作为 Python 包嵌入：

```python
# 在自研下载器入口
import sys
sys.path.insert(0, '/path/to/bitcomet_accel_toolkit/src')

from bclink_url_parser import parse as parse_url
from anti_leech_filter import AntiLeechFilter, AntiLeechLevel
from utp_diagnostics import UtpDiagnostics
# ... 其他模块
```

**优点**：零成本启动，所有功能可用
**缺点**：Python ↔ C++ 桥接有性能损耗（每次 alert 回调约 10μs）

### 模式 B：libtorrent 原生扩展（推荐长期方案）

把 Python 原型翻译成 C++，作为 libtorrent 的 plugin：

```cpp
// 自研 plugin 头文件
class BitCometAccelPlugin : public lt::plugin {
    void on_alert(lt::alert const* alert) override {
        if (auto pa = lt::alert_cast<lt::peer_alert>(alert)) {
            // 调用 AntiLeechFilter 的 C++ 版本
            auto action = m_antileech.decide(pa->endpoint, pa->pid);
            if (action == AntiLeechAction::DISCONNECT) {
                pa->handle.disconnect_peer(pa->endpoint);
            }
        }
    }
    AntiLeechFilter m_antileech;
    PeerBroadcastOptimizer m_broadcast;
    UtpDiagnostics m_utp_diag;
};

// 注册
lt::session_params params;
params.extensions.push_back(std::make_shared<BitCometAccelPlugin>());
lt::session ses(std::move(params));
```

**优点**：原生性能，零 alert 桥接损耗
**缺点**：需要 C++ 重新实现

### 模式 C：sidecar 进程（推荐分离部署）

Toolkit 作为独立 Python 进程，通过 IPC（ZMQ / HTTP）与下载器通信：

```
┌─────────────────────┐         ┌─────────────────────┐
│  下载器主进程 (C++)  │ <IPC>  │ accel_toolkit sidecar │
│  Qt6 + libtorrent    │ ─────> │  Python async loop    │
└─────────────────────┘         └─────────────────────┘
```

**优点**：崩溃隔离，可独立更新
**缺点**：IPC 序列化开销

## 3. P0 模块集成步骤

### 3.1 P2SP 多源合并下载

**何时使用**：用户从论坛复制多个镜像 URL 时

```python
from p2sp_downloader import P2SPDownloader, BasicDownloadStrategy

# 用户输入 4 个镜像
urls = [
    "http://mirror1.example.com/file.iso",
    "http://mirror2.example.com/file.iso",
    "ftp://mirror3.example.com/file.iso",
    "https://mirror4.example.com/file.iso",
]

async def download_with_p2sp(output_path):
    dl = P2SPDownloader(
        output_path=output_path,
        strategy=BasicDownloadStrategy(piece_size=1 << 20),  # 1 MiB
        max_concurrent_sources=4,
    )
    stats = await dl.download(urls)
    return stats
```

**集成位置**：在自研下载器的"添加任务"对话框，如果用户输入多个 URL，自动走 P2SP 路径。

### 3.2 LT-Seeding 协议

**何时使用**：
1. 用户下载完成一个文件后，自动启动 LT-Seed 服务端
2. 当下载卡死时（速度 < 10KB/s 持续 5 分钟），自动启用 LT-Seed 客户端

```python
from lt_seed_protocol import LtSeedServer, LtSeedClient, compute_file_sha1

# 启动服务端 (用户下载完成后)
async def on_torrent_completed(torrent_handle):
    file_path = torrent_handle.status().save_path
    server = LtSeedServer(listen_port=25432)
    file_hash = server.add_file(file_path)
    await server.start()
    # 把 file_hash 上报到云端协调器
    await announce_to_cloud(file_hash, my_endpoint=(my_ip, 25432))

# 启动客户端 (死种救场)
async def on_speed_too_low(torrent_handle):
    file_hash = await lookup_lt_seed_hash(torrent_handle.info_hash())
    if not file_hash:
        return
    client = LtSeedClient(seed_servers=[("passport-client.bitcomet.com", 25476)])
    seeds = await client.query_seeds(file_hash)
    if seeds:
        # 从 LT-Seed 取分片
        for piece_index in missing_pieces(torrent_handle):
            data = await client.fetch_piece(file_hash, piece_index, seeds)
            if data:
                torrent_handle.add_piece(piece_index, data)
```

**集成位置**：torrent 完成事件回调 + 速度监控定时器。

### 3.3 AntiLeech 过滤器

**何时使用**：libtorrent peer_alert 回调

```python
import libtorrent as lt
from anti_leech_filter import (
    AntiLeechFilter, AntiLeechLevel, AntiLeechAction,
    LibtorrentPeerInfoAdapter,
)

antileech = AntiLeechFilter(level=AntiLeechLevel.LIMIT)

def on_peer_alert(alert):
    if not isinstance(alert, lt.peer_alert):
        return
    handle = alert.handle
    try:
        peers = handle.get_peer_info()
    except Exception:
        return
    for peer in peers:
        # 1. AntiLeech 决策
        action = LibtorrentPeerInfoAdapter.update_from_lt_peer_info(
            antileech, peer
        )
        if action == AntiLeechAction.DISCONNECT:
            handle.disconnect_peer(peer)
        elif action == AntiLeechAction.LIMIT_25:
            handle.set_peer_upload_limit(peer, default_limit // 4)
            handle.set_peer_download_limit(peer, default_limit // 4)
        elif action == AntiLeechAction.BAN_NEW_REQUESTS:
            handle.set_peer_upload_limit(peer, 0)

# 注册到 libtorrent session
ses.set_alert_notify(on_peer_alert)
```

**集成位置**：libtorrent alert dispatcher。

## 4. P1 模块集成步骤

### 4.1 多源 Peer 发现

```python
from peer_discovery_extender import (
    MultiSourcePeerDiscovery, CloudPeerAnnouncer,
)

# 启动发现
disc = MultiSourcePeerDiscovery(
    info_hash=torrent_handle.info_hash().to_bytes().hex(),
    my_listen_port=6881,
)
disc.set_cloud_announce_url("https://my-cloud.example.com/api/peers")
for tracker_url in torrent_handle.trackers():
    disc.add_tracker(tracker_url.url)

# 回调: 发现新 peer 时, 注入到 libtorrent
def on_new_peer(candidate):
    try:
        torrent_handle.connect_peer(
            lt.tcp_endpoint(candidate.endpoint[0], candidate.endpoint[1])
        )
    except Exception:
        pass

disc.set_on_new_peer_callback(on_new_peer)

# 后台运行
import asyncio
asyncio.create_task(disc.run(interval_sec=60))
```

### 4.2 Peer 广播优化

```python
from peer_broadcast_optimizer import (
    PeerBroadcastOptimizer, BtMsg,
)

# 创建优化器
broadcast_opt = PeerBroadcastOptimizer(
    send_callback=lambda ep, mt, payload: send_via_libtorrent(ep, mt, payload),
    flush_interval_ms=100,
)

# 在 libtorrent piece_finished 回调中
def on_piece_finished(alert):
    broadcast_opt.broadcast_have(alert.piece_index)
    broadcast_opt.flush()  # 批量发送
```

### 4.3 多协议 URL 解析

```python
from bclink_url_parser import parse, UrlProtocol, is_valid

def on_user_paste_url(url):
    if not is_valid(url):
        show_error("不支持的 URL 格式")
        return
    parts = parse(url)
    if parts.protocol == UrlProtocol.MAGNET:
        add_bt_task(magnet=parts.raw)
    elif parts.protocol == UrlProtocol.HTTP:
        # 多源合并? 询问用户
        if user_wants_p2sp():
            add_p2sp_task([parts.raw])
        else:
            add_http_task(parts.raw)
    elif parts.protocol == UrlProtocol.ED2K:
        add_ed2k_task(parts.name, parts.size, parts.file_hash)
    elif parts.protocol == UrlProtocol.BCLINK:
        # bc:// 解码后通常是 magnet
        if parts.info_hash:
            add_bt_task(magnet=build_magnet_from_info_hash(parts.info_hash))
```

## 5. P2 模块集成步骤

### 5.1 自适应磁盘缓存

**注意**：本模块替换 libtorrent 默认 cache，需要谨慎。

```python
from adaptive_disk_cache import AdaptiveDiskCache, CachedFileSettings

settings = CachedFileSettings(
    max_memory_bytes=512 * 1024 * 1024,  # 512 MiB
    auto_resize=True,
    min_free_memory_bytes=512 * 1024 * 1024,
)
disk_cache = AdaptiveDiskCache(settings=settings)

# 每个 torrent 启动时打开 cache
def on_torrent_added(torrent_handle):
    file_path = torrent_handle.status().save_path
    file_hash = torrent_handle.info_hash().to_bytes().hex()
    cf = disk_cache.open(file_path, file_hash)
    # libtorrent piece 写入时调用 cf.put, 读取时调用 cf.get
```

### 5.2 UTP 拥塞诊断

```python
from utp_diagnostics import UtpDiagnostics, LibtorrentPeerInfoAdapter

utp_diag = UtpDiagnostics()

# 告警回调
utp_diag.add_alert_callback(lambda a: log_warning(a["msg"]))

# 在 peer_alert 中更新统计
def on_peer_alert(alert):
    for peer in alert.handle.get_peer_info():
        LibtorrentPeerInfoAdapter.update_from_lt_peer_info(utp_diag, peer)

# 后台采样
import threading
def sample_loop():
    while True:
        utp_diag.sample()
        time.sleep(1.0)
threading.Thread(target=sample_loop, daemon=True).start()

# 在 UI 中显示
def get_utp_stats_for_ui():
    return utp_diag.get_per_socket_stats()
```

## 6. 一键启动所有加速模块

```python
"""自研下载器集成 accel_toolkit 的入口."""
import asyncio
import libtorrent as lt
from pathlib import Path

# 引入 toolkit
import sys
sys.path.insert(0, '/path/to/bitcomet_accel_toolkit/src')
from bclink_url_parser import parse as parse_url
from anti_leech_filter import AntiLeechFilter, AntiLeechLevel, LibtorrentPeerInfoAdapter
from peer_broadcast_optimizer import PeerBroadcastOptimizer, BtMsg
from peer_discovery_extender import MultiSourcePeerDiscovery
from utp_diagnostics import UtpDiagnostics
from adaptive_disk_cache import AdaptiveDiskCache, CachedFileSettings


class AccelToolkit:
    """集中管理所有加速模块."""

    def __init__(self, lt_session: lt.session, listen_port: int = 6881):
        self.lt = lt_session
        self.listen_port = listen_port

        # 初始化各模块
        self.antileech = AntiLeechFilter(level=AntiLeechLevel.LIMIT)
        self.utp_diag = UtpDiagnostics()
        self.utp_diag.add_alert_callback(self._on_utp_alert)
        self.broadcast_opt = PeerBroadcastOptimizer(
            send_callback=self._send_via_libtorrent,
        )
        self.disk_cache = AdaptiveDiskCache(
            settings=CachedFileSettings(max_memory_bytes=512*1024*1024)
        )
        # 各 torrent 的 peer 发现器
        self._discoveries = {}

        # 启动 alert 处理线程
        self._alert_thread = None
        self._stop = False

    def attach_to_torrent(self, torrent_handle, info_hash: str, trackers: list):
        """为一个新 torrent 启用所有加速模块."""
        # 多源 peer 发现
        disc = MultiSourcePeerDiscovery(info_hash, self.listen_port)
        for tr in trackers:
            disc.add_tracker(tr)
        disc.set_on_new_peer_callback(
            lambda p: torrent_handle.connect_peer(
                lt.tcp_endpoint(p.endpoint[0], p.endpoint[1])
            )
        )
        asyncio.create_task(disc.run(interval_sec=60))
        self._discoveries[info_hash] = disc

    def start(self):
        """启动 alert 处理循环."""
        import threading
        self._alert_thread = threading.Thread(target=self._alert_loop, daemon=True)
        self._alert_thread.start()

    def _alert_loop(self):
        while not self._stop:
            if not self.lt.wait_for_alert(500):  # 500ms
                continue
            alerts = self.lt.pop_alerts()
            for alert in alerts:
                self._handle_alert(alert)

    def _handle_alert(self, alert):
        # peer_alert: AntiLeech + UTP diag
        if isinstance(alert, lt.peer_alert):
            handle = alert.handle
            try:
                peers = handle.get_peer_info()
            except Exception:
                return
            for peer in peers:
                action = LibtorrentPeerInfoAdapter.update_from_lt_peer_info(
                    self.antileech, peer
                )
                if action.name == "DISCONNECT":
                    handle.disconnect_peer(peer)
                elif action.name == "LIMIT_25":
                    handle.set_peer_upload_limit(peer, 100_000)  # 100 KB/s
                LibtorrentPeerInfoAdapter.update_from_lt_peer_info(
                    self.utp_diag, peer
                )

        # piece_finished: 广播优化
        elif isinstance(alert, lt.piece_finished_alert):
            self.broadcast_opt.broadcast_have(alert.piece_index)
            self.broadcast_opt.flush()

    def _send_via_libtorrent(self, endpoint, msg_type, payload):
        """把优化器发出的消息通过 libtorrent 发送."""
        # 这里需要 libtorrent 的 raw peer message API
        # 实际使用时, 可能需要写 libtorrent plugin
        pass

    def _on_utp_alert(self, alert: dict):
        print(f"[UTP] {alert['msg']}")

    def stop(self):
        self._stop = True
        self.disk_cache.close_all()


# 使用
if __name__ == "__main__":
    ses = lt.session()
    ses.listen_on(6881, 6891)

    accel = AccelToolkit(ses)
    accel.start()
    print("✓ accel_toolkit 已启动")
```

## 7. 测试与监控

集成后的监控指标：

```python
def get_accel_stats(accel: AccelToolkit):
    return {
        "antileech": accel.antileech.get_stats(),
        "utp_diag": {
            "sockets": accel.utp_diag.get_stats_tcp_connections(),
            "rate_send_bps, rate_recv_bps": accel.utp_diag.get_stats_rate(),
            "recv_drop_%": accel.utp_diag.get_utp_recv_drop_percent(),
            "send_drop_%": accel.utp_diag.get_utp_send_drop_percent(),
        },
        "disk_cache": accel.disk_cache.stats(),
        "discoveries": {
            ih: disc.get_stats() for ih, disc in accel._discoveries.items()
        },
        "broadcast_opt": {
            "sent": accel.broadcast_opt.stats_sent,
            "deduped": accel.broadcast_opt.stats_deduped,
            "skipped": accel.broadcast_opt.stats_skipped,
        },
    }
```

## 8. 性能基准建议

集成后建议跑以下基准：

1. **AntiLeech ROI 测试**
   - 跑 24h 公网 BT 任务
   - 对比启用前后：本机上传带宽 / 下载完成度 / 上传完成度

2. **P2SP 多源测试**
   - 找 4 个镜像同一文件
   - 对比单源 vs 多源下载时间

3. **LT-Seed 救场测试**
   - 找一个 tracker 失效的死种
   - 启用 LT-Seed 客户端，看能否取到分片

4. **Peer 广播优化测试**
   - 1000 peers 场景
   - 对比 HAVE 消息包数（优化前 vs 优化后）

5. **UTP 诊断告警测试**
   - 模拟高丢包网络
   - 验证告警触发率
