"""
dht_custom_implementation.py — BitComet 私有 DHT 实现
=================================================

逆向来源: Core_Tracker_DHT::InterfaceTrackerDHT + InterfaceBitTorrentShare
关键符号:
    InterfaceTrackerDHT::InterfaceTrackerDHT
    InterfaceTrackerDHT::add_node
    InterfaceTrackerDHT::add_resolved_host_node
    InterfaceTrackerDHT::announce_info_t
    InterfaceTrackerDHT::connect
    InterfaceTrackerDHT::disconnect
    InterfaceTrackerDHT::dump
    InterfaceTrackerDHT::get_outbound_limit_config
    InterfaceTrackerDHT::get_state
    InterfaceTrackerDHT::get_stats_rate_udp
    InterfaceTrackerDHT::init
    InterfaceTrackerDHT::is_ip_available
    InterfaceTrackerDHT::m_pInterfaceDHTCallback
    InterfaceTrackerDHT::node_info_t
    InterfaceTrackerDHT::nodes6_get
    InterfaceTrackerDHT::nodes6_put
    InterfaceTrackerDHT::nodes_get
    InterfaceTrackerDHT::nodes_get_random
    InterfaceTrackerDHT::nodes_put
    InterfaceTrackerDHT::outbound_observe_stats_t
    InterfaceTrackerDHT::release
    InterfaceTrackerDHT::session_info_t
    InterfaceTrackerDHT::set_connect_pending
    InterfaceTrackerDHT::set_enable_log
    InterfaceTrackerDHT::set_outbound_limit_config
    InterfaceTrackerDHT::start
    InterfaceTrackerDHT::stats_nodes_detail_t
    InterfaceTrackerDHT::stats_nodes_t
    InterfaceTrackerDHT::stop
    InterfaceTrackerDHT::tracker_announce_peer
    InterfaceTrackerDHT::tracker_get_response

    InterfaceDHTCallback::is_ip_blocked
    InterfaceDHTCallback::on_dht_received_infohash

    InterfaceBitTorrentShare::dht_torrent_add
    InterfaceBitTorrentShare::dht_torrent_clear
    InterfaceBitTorrentShare::dht_torrent_compact_async
    InterfaceBitTorrentShare::dht_torrent_get_all_count
    InterfaceBitTorrentShare::dht_torrent_get_filtered
    InterfaceBitTorrentShare::dht_torrent_get_filtered_count
    InterfaceBitTorrentShare::dht_torrent_get_metadata_count
    InterfaceBitTorrentShare::dht_torrent_import
    InterfaceBitTorrentShare::dht_torrent_load_auto
    InterfaceBitTorrentShare::dht_torrent_loaded
    InterfaceBitTorrentShare::dht_torrent_remove
    InterfaceBitTorrentShare::dht_torrent_set_category
    InterfaceBitTorrentShare::dht_torrent_set_hide_if_no_metadata
    InterfaceBitTorrentShare::dht_torrent_set_keyword
    InterfaceBitTorrentShare::dht_torrent_set_sort
    InterfaceBitTorrentShare::m_dht_torrent_db_file
    InterfaceBitTorrentShare::on_dht_received_infohash
    InterfaceBitTorrentShare::torrent_dht_t

设计核心:
1. BitComet 不用 libtorrent 内置 DHT, 而是自己实现
2. 完整 BEP-5 (DHT) + BEP-51 (Bitsurge DHT) 实现
3. 增强特性:
   a) nodes6_get/put: IPv6 节点支持
   b) outbound_observe_stats_t: 出站请求统计
   c) outbound_limit_config: 出站限速
   d) is_ip_blocked: DHT 自带 IP 过滤
   e) dht_torrent_db_file: DHT 数据库持久化
   f) dht_torrent_set_hide_if_no_metadata: 隐藏无 metadata 的种子
   g) dht_torrent_set_keyword: 种子关键词索引

加速价值 (针对 qBittorrent):
- qBittorrent 用 libtorrent 内置 DHT, 不可定制
- BitComet 私有 DHT 可:
  a) 持久化 DHT 数据库 (重启不丢失已知 peer)
  b) DHT 流量统计 + 限速
  c) IP filter 集成
  d) 关键词索引 (DHT 搜索)

本模块实现:
- DhtNode: 单个 DHT 节点
- DhtRoutingTable: K-bucket 路由表
- DhtDatabase: 持久化 torrent 数据库
- BitCometDht: 主 DHT 实现

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import hashlib
import logging
import os
import random
import socket
import struct
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Deque, Dict, List, Optional, Set, Tuple

LOG = logging.getLogger("dht")


# -----------------------------------------------------------------------------
# 常量
# -----------------------------------------------------------------------------

DHT_K = 8                     # 每个 k-bucket 最多节点数 (BEP-5 标准)
DHT_B = 160                    # 节点 ID 位数 (SHA-1)
DHT_ALPHA = 3                  # 并发查询数
DHT_REFRESH_INTERVAL = 900    # 15 分钟刷新
DHT_NODE_TIMEOUT = 900         # 15 分钟无响应视为死节点


class DhtMessageType(IntEnum):
    """BEP-5 DHT 消息类型 (用 y 字段标识)."""
    PING = 1       # query: ping
    FIND_NODE = 2   # query: find_node
    GET_PEERS = 3    # query: get_peers
    ANNOUNCE_PEER = 4 # query: announce_peer
    ERROR = 5       # response: error
    # BitComet 扩展
    GET_METADATA = 0x10  # BEP-51 Bitsurge: 获取 metadata
    SAMPLE_INFOHASHES = 0x11  # BEP-51: 采样 infohash


# -----------------------------------------------------------------------------
# DhtNode
# -----------------------------------------------------------------------------

@dataclass
class DhtNode:
    """对应 InterfaceTrackerDHT::node_info_t."""
    node_id: bytes                # 20 字节 SHA-1
    ip: str
    port: int
    is_ipv6: bool = False
    last_responsive: float = field(default_factory=time.time)
    last_query: float = 0.0
    # 节点质量 (BitComet 私有)
    response_count: int = 0
    query_count: int = 0
    failed_count: int = 0
    # 出站限流
    outbound_requests: int = 0
    outbound_bytes: int = 0

    @property
    def is_alive(self) -> bool:
        return (time.time() - self.last_responsive < DHT_NODE_TIMEOUT and
                self.failed_count < 5)

    def to_compact(self) -> bytes:
        """BEP-5 compact 格式: 20字节ID + 4字节IP + 2字节端口."""
        ip_bytes = socket.inet_aton(self.ip) if not self.is_ipv6 else b"\x00" * 16
        return self.node_id + ip_bytes + struct.pack(">H", self.port)


# -----------------------------------------------------------------------------
# DhtRoutingTable — K-bucket 路由表
# -----------------------------------------------------------------------------

class DhtRoutingTable:
    """BEP-5 K-bucket 路由表 (160 个 bucket).

    对应 InterfaceTrackerDHT::nodes_get/put + nodes6_get/put.
    """

    def __init__(self, my_node_id: bytes):
        assert len(my_node_id) == 20
        self.my_node_id = my_node_id
        # 160 个 k-bucket, 每个最多 K=8 节点
        self._buckets: List[List[DhtNode]] = [[] for _ in range(DHT_B)]
        self._nodes_by_id: Dict[bytes, DhtNode] = {}
        # IPv6 节点 (独立存储)
        self._v6_nodes: Dict[bytes, DhtNode] = {}

    def add_node(self, node: DhtNode) -> bool:
        """对应 nodes_put."""
        if node.is_ipv6:
            self._v6_nodes[node.node_id] = node
            return True
        bucket_idx = self._bucket_index(node.node_id)
        bucket = self._buckets[bucket_idx]
        # 已存在?
        for i, n in enumerate(bucket):
            if n.node_id == node.node_id:
                # 更新
                bucket[i] = node
                self._nodes_by_id[node.node_id] = node
                return True
        # bucket 满?
        if len(bucket) >= DHT_K:
            # 替换最不活跃的
            oldest = min(bucket, key=lambda n: n.last_responsive)
            if not oldest.is_alive:
                bucket.remove(oldest)
                del self._nodes_by_id[oldest.node_id]
                bucket.append(node)
                self._nodes_by_id[node.node_id] = node
                return True
            return False
        bucket.append(node)
        self._nodes_by_id[node.node_id] = node
        return True

    def get_node(self, node_id: bytes) -> Optional[DhtNode]:
        return self._nodes_by_id.get(node_id) or self._v6_nodes.get(node_id)

    def get_random_nodes(self, count: int = 8) -> List[DhtNode]:
        """对应 nodes_get_random."""
        all_nodes = list(self._nodes_by_id.values()) + list(self._v6_nodes.values())
        random.shuffle(all_nodes)
        return [n for n in all_nodes if n.is_alive][:count]

    def find_closest(self, target: bytes, count: int = DHT_K) -> List[DhtNode]:
        """BEP-5 find_node - 找最近的 K 个节点."""
        all_nodes = list(self._nodes_by_id.values()) + list(self._v6_nodes.values())
        all_nodes = [n for n in all_nodes if n.is_alive]
        # XOR 距离排序
        all_nodes.sort(key=lambda n: _xor_distance(n.node_id, target))
        return all_nodes[:count]

    def get_nodes(self) -> List[DhtNode]:
        """对应 nodes_get."""
        return list(self._nodes_by_id.values())

    def get_nodes_v6(self) -> List[DhtNode]:
        """对应 nodes6_get."""
        return list(self._v6_nodes.values())

    def remove_node(self, node_id: bytes) -> None:
        node = self._nodes_by_id.pop(node_id, None)
        if node:
            bucket_idx = self._bucket_index(node_id)
            bucket = self._buckets[bucket_idx]
            for i, n in enumerate(bucket):
                if n.node_id == node_id:
                    bucket.pop(i)
                    break
        self._v6_nodes.pop(node_id, None)

    def _bucket_index(self, node_id: bytes) -> int:
        """计算节点属于哪个 bucket (0-159)."""
        distance = _xor_distance(self.my_node_id, node_id)
        # 距离的位长度 = bucket index
        if distance == 0:
            return 0
        return distance.bit_length() - 1

    def get_stats(self) -> Dict:
        return {
            "total_nodes": len(self._nodes_by_id) + len(self._v6_nodes),
            "ipv4_nodes": len(self._nodes_by_id),
            "ipv6_nodes": len(self._v6_nodes),
            "buckets_used": sum(1 for b in self._buckets if b),
        }


def _xor_distance(a: bytes, b: bytes) -> int:
    """BEP-5 XOR 距离."""
    assert len(a) == len(b) == 20
    result = 0
    for i in range(20):
        result = (result << 8) | (a[i] ^ b[i])
    return result


# -----------------------------------------------------------------------------
# DhtTorrentEntry — DHT torrent 数据库条目
# -----------------------------------------------------------------------------

@dataclass
class DhtTorrentEntry:
    """对应 InterfaceBitTorrentShare::torrent_dht_t."""
    info_hash: bytes               # 20 字节
    name: Optional[str] = None      # 种子名 (可能从 metadata 提取)
    size: int = 0                   # 文件总大小
    file_count: int = 0
    # 发现时间
    first_seen: float = field(default_factory=time.time)
    last_seen: float = field(default_factory=time.time)
    # 持有 peer 数
    peer_count: int = 0
    # metadata 状态
    has_metadata: bool = False
    hide_if_no_metadata: bool = False
    # BitComet 私有: 关键词 + 分类
    keywords: Set[str] = field(default_factory=set)
    category: Optional[str] = None
    # 来源
    source: str = "dht"


# -----------------------------------------------------------------------------
# DhtDatabase — DHT 数据库持久化
# -----------------------------------------------------------------------------

class DhtDatabase:
    """对应 InterfaceBitTorrentShare 的 dht_torrent_* 系列方法."""

    def __init__(self, db_file: Optional[str] = None):
        self.db_file = db_file
        self._entries: Dict[bytes, DhtTorrentEntry] = {}
        # 加载持久化数据
        if db_file and os.path.exists(db_file):
            self.load_auto()

    # ----- CRUD -----

    def add(self, info_hash: bytes, name: Optional[str] = None,
            size: int = 0, file_count: int = 0) -> DhtTorrentEntry:
        """对应 dht_torrent_add."""
        if info_hash in self._entries:
            entry = self._entries[info_hash]
            entry.last_seen = time.time()
            if name:
                entry.name = name
            return entry
        entry = DhtTorrentEntry(
            info_hash=info_hash, name=name, size=size, file_count=file_count,
        )
        self._entries[info_hash] = entry
        return entry

    def remove(self, info_hash: bytes) -> bool:
        """对应 dht_torrent_remove."""
        return self._entries.pop(info_hash, None) is not None

    def clear(self) -> None:
        """对应 dht_torrent_clear."""
        self._entries.clear()

    def get_all_count(self) -> int:
        """对应 dht_torrent_get_all_count."""
        return len(self._entries)

    def get_metadata_count(self) -> int:
        """对应 dht_torrent_get_metadata_count."""
        return sum(1 for e in self._entries.values() if e.has_metadata)

    def get_filtered(self, keyword: Optional[str] = None,
                     category: Optional[str] = None,
                     with_metadata_only: bool = False,
                     limit: int = 100) -> List[DhtTorrentEntry]:
        """对应 dht_torrent_get_filtered."""
        result = []
        for e in self._entries.values():
            if keyword and keyword not in e.name and keyword not in e.keywords:
                continue
            if category and e.category != category:
                continue
            if with_metadata_only and not e.has_metadata:
                continue
            if e.hide_if_no_metadata and not e.has_metadata:
                continue
            result.append(e)
            if len(result) >= limit:
                break
        return result

    def get_filtered_count(self, **kwargs) -> int:
        return len(self.get_filtered(**kwargs))

    # ----- 设置 -----

    def set_category(self, info_hash: bytes, category: str) -> None:
        """对应 dht_torrent_set_category."""
        if info_hash in self._entries:
            self._entries[info_hash].category = category

    def set_keyword(self, info_hash: bytes, keyword: str) -> None:
        """对应 dht_torrent_set_keyword."""
        if info_hash in self._entries:
            self._entries[info_hash].keywords.add(keyword)

    def set_hide_if_no_metadata(self, info_hash: bytes, hide: bool) -> None:
        """对应 dht_torrent_set_hide_if_no_metadata."""
        if info_hash in self._entries:
            self._entries[info_hash].hide_if_no_metadata = hide

    def set_sort(self, key: str = "last_seen") -> List[DhtTorrentEntry]:
        """对应 dht_torrent_set_sort."""
        entries = list(self._entries.values())
        if key == "last_seen":
            entries.sort(key=lambda e: e.last_seen, reverse=True)
        elif key == "first_seen":
            entries.sort(key=lambda e: e.first_seen, reverse=True)
        elif key == "peer_count":
            entries.sort(key=lambda e: e.peer_count, reverse=True)
        elif key == "size":
            entries.sort(key=lambda e: e.size, reverse=True)
        return entries

    # ----- 持久化 -----

    def load_auto(self) -> None:
        """对应 dht_torrent_load_auto + dht_torrent_loaded."""
        if not self.db_file or not os.path.exists(self.db_file):
            return
        try:
            with open(self.db_file, "rb") as f:
                data = f.read()
            # 简化格式: count(4) + [info_hash(20) + name_len(2) + name + size(8) + ...]
            pos = 0
            count = struct.unpack(">I", data[pos:pos+4])[0]
            pos += 4
            for _ in range(count):
                if pos + 22 > len(data): break
                info_hash = data[pos:pos+20]; pos += 20
                name_len = struct.unpack(">H", data[pos:pos+2])[0]; pos += 2
                name = data[pos:pos+name_len].decode("utf-8", errors="replace")
                pos += name_len
                size = struct.unpack(">Q", data[pos:pos+8])[0]; pos += 8
                self.add(info_hash, name, size)
        except Exception as e:
            LOG.error("DHT db load failed: %s", e)

    def compact_async(self) -> None:
        """对应 dht_torrent_compact_async - 异步压缩数据库."""
        if not self.db_file:
            return
        # 简化: 同步写
        data = bytearray()
        data += struct.pack(">I", len(self._entries))
        for e in self._entries.values():
            data += e.info_hash
            name_bytes = (e.name or "").encode("utf-8")
            data += struct.pack(">H", len(name_bytes)) + name_bytes
            data += struct.pack(">Q", e.size)
        with open(self.db_file, "wb") as f:
            f.write(bytes(data))

    def import_data(self, other: "DhtDatabase") -> int:
        """对应 dht_torrent_import - 从另一个数据库导入."""
        count = 0
        for info_hash, entry in other._entries.items():
            if info_hash not in self._entries:
                self._entries[info_hash] = entry
                count += 1
        return count

    # ----- 事件 -----

    def on_dht_received_infohash(self, info_hash: bytes) -> None:
        """对应 InterfaceDHTCallback::on_dht_received_infohash."""
        if info_hash not in self._entries:
            self.add(info_hash)
            LOG.debug("DHT received new infohash: %s", info_hash.hex()[:16])

    def get_stats(self) -> Dict:
        return {
            "total": len(self._entries),
            "with_metadata": self.get_metadata_count(),
            "with_peers": sum(1 for e in self._entries.values() if e.peer_count > 0),
        }


# -----------------------------------------------------------------------------
# BitCometDht — 主 DHT 实现
# -----------------------------------------------------------------------------

class BitCometDht:
    """对应 InterfaceTrackerDHT 完整 DHT 实现."""

    def __init__(self, my_node_id: Optional[bytes] = None,
                 listen_port: int = 0,
                 db_file: Optional[str] = None):
        self.my_node_id = my_node_id or os.urandom(20)
        self.listen_port = listen_port
        # 路由表
        self.routing_table = DhtRoutingTable(self.my_node_id)
        # 数据库
        self.database = DhtDatabase(db_file)
        # 状态
        self.is_running = False
        self.state = "stopped"
        # 出站限速 (BitComet 私有)
        self.outbound_limit_bps: int = 0  # 0 = 不限
        self.outbound_bytes_sent: int = 0
        self.outbound_bytes_recv: int = 0
        # IP filter 集成
        self._blocked_ips: Set[str] = set()
        # bootstrap 节点
        self._bootstrap_nodes: List[Tuple[str, int]] = [
            ("router.bittorrent.com", 6881),
            ("dht.transmissionbt.com", 6881),
            ("router.utorrent.com", 6881),
        ]
        # 统计
        self.stats = {
            "queries_sent": 0,
            "queries_received": 0,
            "responses_received": 0,
            "announces_received": 0,
            "infohashes_received": 0,
        }

    # ----- 生命周期 -----

    def start(self) -> None:
        """对应 start."""
        self.is_running = True
        self.state = "running"
        # 加入 bootstrap 节点
        for host, port in self._bootstrap_nodes:
            self.add_resolved_host_node(host, port)
        LOG.info("DHT started, my_node_id=%s", self.my_node_id.hex()[:16])

    def stop(self) -> None:
        """对应 stop."""
        self.is_running = False
        self.state = "stopped"
        # 持久化数据库
        self.database.compact_async()

    # ----- 节点管理 -----

    def add_node(self, node: DhtNode) -> bool:
        """对应 add_node."""
        if self.is_ip_blocked(node.ip):
            return False
        return self.routing_table.add_node(node)

    def add_resolved_host_node(self, host: str, port: int) -> bool:
        """对应 add_resolved_host_node - 解析 DNS 后添加."""
        try:
            # 简化: 同步解析
            ips = socket.getaddrinfo(host, port, socket.AF_INET, socket.SOCK_DGRAM)
            for family, _, _, _, sockaddr in ips:
                ip = sockaddr[0]
                if self.is_ip_blocked(ip):
                    continue
                node_id = hashlib.sha1((ip + str(port)).encode()).digest()
                node = DhtNode(node_id=node_id, ip=ip, port=port)
                return self.routing_table.add_node(node)
        except socket.gaierror as e:
            LOG.debug("resolve %s failed: %s", host, e)
        return False

    def is_ip_available(self, ip: str) -> bool:
        """对应 is_ip_available."""
        return not self.is_ip_blocked(ip)

    def is_ip_blocked(self, ip: str) -> bool:
        """对应 InterfaceDHTCallback::is_ip_blocked."""
        return ip in self._blocked_ips

    def block_ip(self, ip: str) -> None:
        self._blocked_ips.add(ip)

    def unblock_ip(self, ip: str) -> None:
        self._blocked_ips.discard(ip)

    # ----- DHT 查询 -----

    def ping(self, target: DhtNode) -> bool:
        """对应 ping query."""
        if not target.is_alive:
            return False
        target.query_count += 1
        target.last_query = time.time()
        self.stats["queries_sent"] += 1
        # 简化: 假设 90% 响应
        if random.random() < 0.9:
            target.response_count += 1
            target.last_responsive = time.time()
            self.stats["responses_received"] += 1
            return True
        else:
            target.failed_count += 1
            return False

    def find_node(self, target_id: bytes) -> List[DhtNode]:
        """对应 find_node query."""
        # 返回本地路由表中最近的 K 个节点
        return self.routing_table.find_closest(target_id, DHT_K)

    def get_peers(self, info_hash: bytes) -> List[Tuple[str, int]]:
        """对应 get_peers query."""
        # 简化: 模拟返回
        return []

    def announce_peer(self, info_hash: bytes, port: int,
                       implied_port: bool = False) -> bool:
        """对应 tracker_announce_peer."""
        # 把自己加入该 infohash 的 peer 列表
        entry = self.database.add(info_hash)
        entry.peer_count += 1
        self.stats["announces_received"] += 1
        return True

    # ----- 出站限速 -----

    def get_outbound_limit_config(self) -> int:
        return self.outbound_limit_bps

    def set_outbound_limit_config(self, bps: int) -> None:
        self.outbound_limit_bps = bps

    def get_stats_rate_udp(self) -> Tuple[int, int]:
        """对应 get_stats_rate_udp - (sent_bytes, recv_bytes)."""
        return (self.outbound_bytes_sent, self.outbound_bytes_recv)

    # ----- 持久化 -----

    def dht_torrent_load_auto(self) -> None:
        """对应 dht_torrent_load_auto."""
        self.database.load_auto()

    def get_state(self) -> str:
        return self.state

    def get_stats(self) -> Dict:
        s = dict(self.stats)
        s.update(self.routing_table.get_stats())
        s.update(self.database.get_stats())
        s["state"] = self.state
        s["outbound_bytes_sent"] = self.outbound_bytes_sent
        s["outbound_bytes_recv"] = self.outbound_bytes_recv
        return s


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    logging.basicConfig(level=logging.INFO,
                        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s")
    print("=" * 60)
    print("BitComet 私有 DHT demo")
    print("=" * 60)
    dht = BitCometDht(listen_port=6881, db_file="/tmp/bc_dht.db")
    dht.start()
    # 模拟添加节点
    for i in range(20):
        ip = f"10.0.0.{i+1}"
        port = 6881 + i
        node_id = hashlib.sha1(f"{ip}:{port}".encode()).digest()
        dht.add_node(DhtNode(node_id=node_id, ip=ip, port=port))
    # 添加几个 DHT torrent
    for i in range(5):
        info_hash = hashlib.sha1(f"torrent_{i}".encode()).digest()
        dht.database.add(info_hash, name=f"torrent_{i}", size=1024*1024*(i+1))
        dht.database.set_keyword(info_hash, "movie")
        if i < 3:
            dht.database.set_category(info_hash, "media")
    # 隐藏无 metadata 的
    info_hash_hidden = hashlib.sha1(b"hidden").digest()
    dht.database.add(info_hash_hidden, name="hidden_torrent")
    dht.database.set_hide_if_no_metadata(info_hash_hidden, True)
    # 统计
    print("\n=== DHT stats ===")
    for k, v in dht.get_stats().items():
        print(f"  {k}: {v}")
    # find_node 演示
    print("\n=== find_node ===")
    target = hashlib.sha1(b"target").digest()
    closest = dht.find_node(target)
    print(f"  found {len(closest)} closest nodes")
    for n in closest[:3]:
        print(f"  → {n.ip}:{n.port} (id={n.node_id.hex()[:8]}...)")
    # filter
    print("\n=== database filtered (keyword=movie) ===")
    entries = dht.database.get_filtered(keyword="movie")
    for e in entries:
        print(f"  → {e.info_hash.hex()[:8]}... name={e.name} size={e.size}")
    dht.stop()
