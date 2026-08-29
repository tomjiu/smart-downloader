"""
peer_discovery_extender.py — 多源 Peer 发现扩展器
================================================

逆向来源: BitComet `Core_BitTorrent::BitTorrentPeerPool` + `Core_BCSPClient`
关键符号:
    BitTorrentPeerPool::bc_peer_list_get
    BitTorrentPeerPool::find_connecting_peer
    BitTorrentPeerPool::is_incoming_peer_acceptable
    BitTorrentPeerPool::get_request_log_string
    Core_BCSPClient::BCSPClient (BitComet Service Protocol 客户端)
    Core_P2SPClient::HTTPShareQueryWrapper
    Core_P2SPClient::HTTPShareAnnounceWrapper (HTTP 共享 announce)
    Core_P2SPClient::TorrentShareQueryWrapper (BT 共享查询)
    Core_P2SPClient::TorrentShareSubmitWrapper (BT 共享提交)

配置端点 (来自 strings):
    /api/config/bt_tracker/{get,set,query,update}
    /api/config/client_filter/* (客户端过滤)
    /api/task/connections/get
    /api/task/peers/{get,ban_ip,unban_peers}
    /api/task/servers/get

设计核心 (从符号分析):
1. BitComet 不只从 tracker/DHT 找 peer, 还从云端 (TorrentShareQuery) 找
2. 当下载完成后, 主动 announce 到云端 (TorrentShareSubmit)
3. HTTPShareAnnounce: 把 HTTP/FTP 镜像也作为 "peer source" 公布
4. 与 LT-Seed 联动: 如果有 LT-Seed, 加入 peer list

加速价值 (针对 qBittorrent):
- qBittorrent 100% 依赖 tracker + DHT
- 公网 BT 经常 tracker 失效 + DHT 没人
- BitComet 多了一个云端 peer source

本模块实现:
- MultiSourcePeerDiscovery: 同时从 tracker + DHT + HTTP webseed + 自定义源 找 peer
- CloudPeerAnnouncer: 把自己的 endpoint 上报到云端 (可选自建)
- HTTPWebseedAsPeer: 把 HTTP webseed 当作 "无限速 peer" 使用

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import asyncio
import logging
import random
import socket
import struct
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from typing import Deque, Dict, List, Optional, Set, Tuple

LOG = logging.getLogger("peer_disc")


# -----------------------------------------------------------------------------
# 数据结构
# -----------------------------------------------------------------------------

@dataclass
class PeerCandidate:
    endpoint: Tuple[str, int]
    source: str                # tracker / dht / pex / cloud / lt_seed / http_webseed
    last_seen: float = field(default_factory=time.time)
    is_ipv6: bool = False
    score: int = 100            # 默认 100, 失败递减


@dataclass
class TrackerInfo:
    url: str                   # http://tracker.example.com/announce or udp://...
    type: str                   # "http" / "udp" / "ws"
    is_working: bool = True
    last_announce: float = 0.0
    peer_count: int = 0


# -----------------------------------------------------------------------------
# MultiSourcePeerDiscovery — 主类
# -----------------------------------------------------------------------------

class MultiSourcePeerDiscovery:
    """对应 BitComet BitTorrentPeerPool + BCSPClient 多源 peer 发现."""

    def __init__(self, info_hash: str, my_listen_port: int = 6881):
        self.info_hash = info_hash.lower()
        self.my_port = my_listen_port
        # peer candidates
        self._peers: Dict[Tuple[str, int], PeerCandidate] = {}
        # 已连接 (避免重复发现)
        self._connected: Set[Tuple[str, int]] = set()
        # 各源
        self._trackers: List[TrackerInfo] = []
        self._dht_active = True
        self._pex_active = True
        self._cloud_announce_url: Optional[str] = None
        # 历史
        self._history: Deque[Tuple[float, int]] = deque(maxlen=60)  # (ts, peer_count)
        # 回调
        self._on_new_peer: Optional[callable] = None

    # ----- 配置 -----

    def set_on_new_peer_callback(self, cb: callable) -> None:
        self._on_new_peer = cb

    def add_tracker(self, url: str) -> None:
        if url.startswith("http://") or url.startswith("https://"):
            t_type = "http"
        elif url.startswith("udp://"):
            t_type = "udp"
        elif url.startswith("ws://") or url.startswith("wss://"):
            t_type = "ws"
        else:
            LOG.warning("unknown tracker scheme: %s", url)
            return
        self._trackers.append(TrackerInfo(url=url, type=t_type))
        LOG.info("added %s tracker: %s", t_type, url)

    def set_cloud_announce_url(self, url: str) -> None:
        """设置云端 announce URL (TorrentShareSubmit 的本地等价)."""
        self._cloud_announce_url = url
        LOG.info("cloud announce URL: %s", url)

    # ----- 主循环 -----

    async def run(self, interval_sec: int = 60) -> None:
        """每 interval_sec 秒执行一次完整发现."""
        while True:
            await self._discover_once()
            self._history.append((time.time(), len(self._peers)))
            await asyncio.sleep(interval_sec)

    async def _discover_once(self) -> None:
        """一次完整发现: tracker + DHT + cloud + LT-Seed."""
        tasks = []
        for tracker in self._trackers:
            if tracker.type == "http":
                tasks.append(self._discover_http_tracker(tracker))
            elif tracker.type == "udp":
                tasks.append(self._discover_udp_tracker(tracker))
        if self._cloud_announce_url:
            tasks.append(self._discover_cloud())
        # DHT 和 PEX 由 libtorrent 自动处理, 这里只补充
        await asyncio.gather(*tasks, return_exceptions=True)
        LOG.info("discovery: total %d peers from %d sources",
                 len(self._peers), len(self._trackers) + (1 if self._cloud_announce_url else 0))

    # ----- HTTP tracker -----

    async def _discover_http_tracker(self, tracker: TrackerInfo) -> None:
        try:
            import aiohttp
        except ImportError:
            return
        params = {
            "info_hash": bytes.fromhex(self.info_hash),
            "peer_id": b"-PY0001-" + random.randbytes(12),
            "port": self.my_port,
            "uploaded": 0, "downloaded": 0, "left": 0,
            "compact": 1, "numwant": 50,
        }
        try:
            async with aiohttp.ClientSession() as session:
                async with session.get(tracker.url, params=params, timeout=15) as resp:
                    if resp.status != 200:
                        tracker.is_working = False
                        return
                    data = await resp.read()
            peers = self._parse_compact_response(data)
            for peer in peers:
                self._add_peer(peer, source="tracker:" + tracker.url)
            tracker.last_announce = time.time()
            tracker.peer_count = len(peers)
            tracker.is_working = True
        except Exception as e:
            LOG.warning("http tracker %s failed: %s", tracker.url, e)
            tracker.is_working = False

    @staticmethod
    def _parse_compact_response(data: bytes) -> List[Tuple[str, int]]:
        """BEP-23 compact format: 6 bytes per peer (4 ip + 2 port)."""
        if len(data) < 20:
            return []
        # 跳过 BEncode 头 (简化处理: 找 peers key)
        idx = data.find(b"6:peers")
        if idx == -1:
            return []
        idx += len(b"6:peers")
        # length prefix
        colon = data.find(b":", idx)
        if colon == -1: return []
        try:
            length = int(data[idx:colon])
        except ValueError:
            return []
        start = colon + 1
        peers_data = data[start:start + length]
        peers = []
        for i in range(0, len(peers_data) - 5, 6):
            ip = socket.inet_ntoa(peers_data[i:i+4])
            port = struct.unpack(">H", peers_data[i+4:i+6])[0]
            if port > 0:
                peers.append((ip, port))
        return peers

    # ----- UDP tracker (BEP-15) -----

    async def _discover_udp_tracker(self, tracker: TrackerInfo) -> None:
        """BEP-15 UDP tracker protocol."""
        from urllib.parse import urlsplit
        p = urlsplit(tracker.url)
        host = p.hostname
        port = p.port or 80
        try:
            reader, writer = await asyncio.open_connection(host, port, family=socket.AF_INET)
        except Exception as e:
            LOG.warning("udp tracker %s connect failed: %s", tracker.url, e)
            tracker.is_working = False
            return
        try:
            # 1. connect request
            txn_id = random.randint(0, 0xFFFFFFFF)
            req = struct.pack(">QII", 0x41727101980, 0, txn_id)
            writer.write(req)
            await writer.drain()
            resp = await asyncio.wait_for(reader.readexactly(16), timeout=10)
            action, recv_txn, conn_id = struct.unpack(">IIQ", resp)
            if action != 0 or recv_txn != txn_id:
                LOG.warning("udp tracker handshake failed")
                tracker.is_working = False
                return
            # 2. announce
            txn_id = random.randint(0, 0xFFFFFFFF)
            info_hash_bytes = bytes.fromhex(self.info_hash)
            peer_id = b"-PY0001-" + random.randbytes(12)
            req = struct.pack(
                ">BQII20s20sQQQIIIiH",
                0, conn_id, 1, txn_id,
                info_hash_bytes, peer_id,
                0, 0, 0, 0, 0, 1, -1, self.my_port,
            )
            writer.write(req)
            await writer.drain()
            resp = await asyncio.wait_for(reader.readexactly(20), timeout=15)
            action, recv_txn, interval, leechers, seeders = struct.unpack(">IIIII", resp[:20])
            if action != 1:
                tracker.is_working = False
                return
            # 6-byte peer entries
            peer_data = await reader.read()
            peers = []
            for i in range(0, len(peer_data) - 5, 6):
                ip = socket.inet_ntoa(peer_data[i:i+4])
                port = struct.unpack(">H", peer_data[i+4:i+6])[0]
                if port > 0:
                    peers.append((ip, port))
            for peer in peers:
                self._add_peer(peer, source="tracker:" + tracker.url)
            tracker.last_announce = time.time()
            tracker.peer_count = len(peers)
            tracker.is_working = True
            LOG.info("udp tracker %s returned %d peers", tracker.url, len(peers))
        except asyncio.TimeoutError:
            LOG.warning("udp tracker %s timeout", tracker.url)
            tracker.is_working = False
        except Exception as e:
            LOG.warning("udp tracker %s failed: %s", tracker.url, e)
            tracker.is_working = False
        finally:
            writer.close()

    # ----- 云端发现 (对应 TorrentShareQuery) -----

    async def _discover_cloud(self) -> None:
        if not self._cloud_announce_url:
            return
        try:
            import aiohttp
        except ImportError:
            return
        try:
            async with aiohttp.ClientSession() as session:
                params = {"info_hash": self.info_hash}
                async with session.get(self._cloud_announce_url,
                                       params=params, timeout=10) as resp:
                    if resp.status != 200:
                        return
                    data = await resp.json()
            # 期望格式: {"peers": [{"ip": "1.2.3.4", "port": 6881}, ...]}
            for p in data.get("peers", []):
                ep = (p["ip"], p["port"])
                self._add_peer(ep, source="cloud")
        except Exception as e:
            LOG.debug("cloud discovery failed: %s", e)

    # ----- PEX / DHT 注入 (由 libtorrent 触发) -----

    def inject_pex_peer(self, endpoint: Tuple[str, int]) -> None:
        self._add_peer(endpoint, source="pex")

    def inject_dht_peer(self, endpoint: Tuple[str, int]) -> None:
        self._add_peer(endpoint, source="dht")

    def inject_lt_seed(self, endpoint: Tuple[str, int]) -> None:
        self._add_peer(endpoint, source="lt_seed")

    def inject_http_webseed(self, url: str) -> None:
        """HTTP webseed 当作 "无限速 peer" — 用 (url, 0) 标记."""
        # 实际不是 TCP endpoint, 但可以加入 source 列表
        self._add_peer((url, 0), source="http_webseed")

    # ----- 查询 -----

    def get_all_peers(self) -> List[PeerCandidate]:
        return list(self._peers.values())

    def get_unconnected_peers(self, limit: int = 50) -> List[PeerCandidate]:
        """返回还未连接的 peer."""
        result = []
        for ep, p in self._peers.items():
            if ep in self._connected:
                continue
            result.append(p)
            if len(result) >= limit:
                break
        return result

    def mark_connected(self, endpoint: Tuple[str, int]) -> None:
        self._connected.add(endpoint)

    def mark_disconnected(self, endpoint: Tuple[str, int]) -> None:
        self._connected.discard(endpoint)
        # 降低分数
        if endpoint in self._peers:
            self._peers[endpoint].score -= 5

    def get_stats(self) -> Dict[str, int]:
        """按 source 分类统计."""
        stats = defaultdict(int)
        for p in self._peers.values():
            stats[p.source] += 1
        stats["total"] = len(self._peers)
        stats["connected"] = len(self._connected)
        return dict(stats)

    # ----- 内部 -----

    def _add_peer(self, endpoint: Tuple[str, int], source: str) -> None:
        if endpoint in self._peers:
            # 已知, 更新 last_seen
            self._peers[endpoint].last_seen = time.time()
            # 新 source 信息也保留 (但 endpoint 唯一)
            return
        candidate = PeerCandidate(endpoint=endpoint, source=source)
        self._peers[endpoint] = candidate
        if self._on_new_peer:
            try:
                self._on_new_peer(candidate)
            except Exception as e:
                LOG.error("peer callback failed: %s", e)


# -----------------------------------------------------------------------------
# CloudPeerAnnouncer — 把自己的 endpoint 上报到云端
# -----------------------------------------------------------------------------

class CloudPeerAnnouncer:
    """对应 Core_P2SPClient::TorrentShareSubmitWrapper.

    当本机有可用的 BT listen port, 把它上报到云端.
    其他客户端可通过云端找到本机.
    """

    def __init__(self, announce_url: str, my_listen_port: int,
                 info_hash: str):
        self.url = announce_url
        self.port = my_listen_port
        self.info_hash = info_hash.lower()
        self.is_running = False

    async def start(self, interval_sec: int = 1800) -> None:
        """每 30 分钟 announce 一次 (避免给云端压力)."""
        self.is_running = True
        while self.is_running:
            await self._announce_once()
            await asyncio.sleep(interval_sec)

    async def stop(self) -> None:
        self.is_running = False
        await self._unannounce()

    async def _announce_once(self) -> None:
        try:
            import aiohttp
        except ImportError:
            return
        # 获取公网 IP
        try:
            async with aiohttp.ClientSession() as session:
                async with session.get("https://api.ipify.org?format=text",
                                       timeout=10) as resp:
                    my_ip = (await resp.text()).strip()
                payload = {
                    "info_hash": self.info_hash,
                    "port": self.my_port,
                    "ip": my_ip,
                    "timestamp": int(time.time()),
                }
                async with session.post(self.url, json=payload, timeout=10) as resp:
                    if resp.status == 200:
                        LOG.info("announced %s:%d for %s",
                                 my_ip, self.my_port, self.info_hash[:16])
        except Exception as e:
            LOG.debug("announce failed: %s", e)

    async def _unannounce(self) -> None:
        try:
            import aiohttp
            async with aiohttp.ClientSession() as session:
                async with session.delete(
                    self.url, json={"info_hash": self.info_hash}, timeout=10
                ) as resp:
                    LOG.info("unannounced %s", self.info_hash[:16])
        except Exception as e:
            LOG.debug("unannounce failed: %s", e)


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

async def _main():
    import argparse
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )
    ap = argparse.ArgumentParser(description="Multi-source peer discovery demo")
    ap.add_argument("--info-hash", required=True, help="40-char info_hash hex")
    ap.add_argument("--tracker", action="append", help="tracker URL (can repeat)")
    ap.add_argument("--cloud-url", help="cloud announce URL")
    ap.add_argument("--port", type=int, default=6881)
    args = ap.parse_args()

    disc = MultiSourcePeerDiscovery(args.info_hash, args.port)
    if args.cloud_url:
        disc.set_cloud_announce_url(args.cloud_url)
    for t in args.tracker or []:
        disc.add_tracker(t)
    # 回调
    disc.set_on_new_peer_callback(lambda p: print(f"  ✓ new peer: {p.endpoint} from {p.source}"))
    # 运行一次发现
    await disc._discover_once()
    print(f"\n=== Discovery stats ===")
    for k, v in disc.get_stats().items():
        print(f"  {k:20s}: {v}")


if __name__ == "__main__":
    asyncio.run(_main())
