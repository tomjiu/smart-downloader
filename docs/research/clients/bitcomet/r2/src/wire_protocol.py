"""
wire_protocol.py — BitComet Core_Wire 传输抽象层协议
==================================================

逆向来源: Core_Wire 命名空间 (929 个符号)
关键符号:
    Core_Wire::InterfaceWire           (抽象接口)
    Core_Wire::WireLinkLayer           (链路层)
    Core_Wire::WireLinkPool            (连接池)
    Core_Wire::WireLinkGroup           (分组)
    Core_Wire::UDPPool                 (UDP socket 池)
    Core_Wire::UDPBuffer               (UDP 缓冲区)
    Core_Wire::UDPBufferVector         (UDP 缓冲区向量)
    Core_Wire::WireBuffer              (Wire 缓冲区)
    Core_Wire::pending_queue_key_t     (待发送队列键)
    Core_Wire::protocol_enum           (上层协议枚举)
    Core_Wire::tracker_host_bucket_t   (tracker 主机分桶)
    Core_Wire::wire_group_t            (分组结构)

完整方法清单 (从 nm -C 提取):
    InterfaceWire::init
    InterfaceWire::dump / protocol_dump
    InterfaceWire::protocol_attach_wire / protocol_attach_wire_i
    InterfaceWire::protocol_detach_wire / protocol_detach_wire_i
    InterfaceWire::get_settings_connection / get_settings_proxy / get_settings_rate_max
    InterfaceWire::get_stats_connecting_tracker_count
    InterfaceWire::get_stats_connecting_tracker_details
    InterfaceWire::get_stats_connection
    InterfaceWire::get_stats_http_tracker_connections
    InterfaceWire::get_stats_package_size
    InterfaceWire::get_stats_rate / get_stats_rate_second / get_stats_rate_max_possible
    InterfaceWire::get_stats_rate_udp / get_stats_rate_udp_by_caller
    InterfaceWire::get_stats_tcp_connections
    InterfaceWire::get_utp_recv_drop_percent / get_utp_send_drop_percent
    InterfaceWire::is_multi_thread_callback
    InterfaceWire::is_stopped
    InterfaceWire::log_id
    InterfaceWire::protocol_set_close_reason / protocol_set_close_reason_i
    InterfaceWire::protocol_get_remote_close_reason / protocol_get_remote_close_reason_i
    WireLinkLayer::socket_recv / socket_send_end / wire_need_recv / wire_need_send
    WireLinkLayer::wire_set_close_reason / wire_get_remote_close_reason
    WireLinkPool::vector_push / vector_erase / wirelink_pending_insert / wirelink_pending_erase
    WireLinkGroup::group_add_wire / group_remove_wire

设计核心:
1. Wire 是 BitComet 自有的传输抽象, 独立于 libtorrent 的 asio
2. 上层有 4 种协议挂在 Wire 上 (InterfaceWireCallbackTemplate 实例化):
   - InterfaceBitTorrentProtocol   (BT 主协议)
   - InterfaceHTTPClientProtocol   (HTTP 客户端)
   - InterfaceHTTPServerProtocol   (HTTP 服务端, WebUI)
   - InterfaceSOAPClientProtocol    (SOAP, 云端通信)
   - InterfaceFTPClientProtocol    (FTP)
   - InterfaceFTPDataProtocol       (FTP 数据)
   - InterfaceSOAPHold / InterfaceSOAPDrop  (SOAP 长连接 / 短连接)
3. Wire 统一管理:
   - 多线程回调 (g_multi_thread_callback)
   - 限速 (get_settings_rate_max)
   - 代理 (get_settings_proxy)
   - 关闭原因透传 (wire_set_close_reason / wire_get_remote_close_reason)
4. tracker_host_bucket_t: 按 tracker host 分桶, 实现负载均衡
5. pending_queue_key_t: 待发送队列键, 支持按优先级发送

加速价值 (针对 qBittorrent):
- qBittorrent 直接依赖 libtorrent 内部 socket, 不可定制
- 多协议任务无法共享连接池 (BT peer / HTTP webseed / FTP 各自独立)
- BitComet Wire 层让所有协议共享:
  a) UDP socket 池 (避免每个协议各开一个 socket)
  b) 限速统一管理
  c) 关闭原因透传 (调试友好)
  d) tracker 分桶 (避免单 tracker 过载)

本模块实现:
- WireLinkLayer: 链路层抽象 (含 close_reason 透传)
- UDPPool: UDP socket 共享池
- TrackerHostBucket: tracker 负载均衡
- PendingSendQueue: 优先级发送队列

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import logging
import socket
import struct
import threading
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Callable, Deque, Dict, List, Optional, Tuple

LOG = logging.getLogger("wire")


# -----------------------------------------------------------------------------
# 枚举
# -----------------------------------------------------------------------------

class ProtocolEnum(IntEnum):
    """对应 Core_Wire::protocol_enum — 挂在 Wire 上的上层协议."""
    BITTORRENT = 0
    HTTP_CLIENT = 1
    HTTP_SERVER = 2
    SOAP_CLIENT = 3
    SOAP_SERVER = 4
    FTP_CLIENT = 5
    FTP_DATA = 6
    BCIP_CLIENT = 7         # BitComet NAT 探测协议
    P2SP_UDP_CLIENT = 8      # P2SP UDP 客户端
    P2SP_UDP_SERVER = 9      # P2SP UDP 服务端
    TRACKER_DHT = 10         # DHT
    TRACKER_CLIENT = 11      # tracker 客户端
    UTP = 12                 # uTP


class PendingQueuePriority(IntEnum):
    """对应 pending_queue_key_t 的优先级字段."""
    LOW = 0          # HTTP webseed, FTP
    NORMAL = 1       # BT piece request
    HIGH = 2         # BT handshake, peer exchange
    URGENT = 3       # 关闭原因, 错误通知


# -----------------------------------------------------------------------------
# 数据结构
# -----------------------------------------------------------------------------

@dataclass
class WireSettings:
    """对应 InterfaceWire::get_settings_*."""
    max_connections: int = 200
    max_rate_per_socket_bps: int = 0   # 0 = 不限
    proxy_host: Optional[str] = None
    proxy_port: Optional[int] = None
    proxy_type: str = "http"          # http / socks5 / socks4


@dataclass
class WireStats:
    """对应 InterfaceWire::get_stats_*."""
    # 速率统计
    rate_bps_send: float = 0.0
    rate_bps_recv: float = 0.0
    rate_udp_bps_send: float = 0.0
    rate_udp_bps_recv: float = 0.0
    # 连接数
    tcp_connections: int = 0
    udp_connections: int = 0
    # 丢包率
    utp_recv_drop_pct: float = 0.0
    utp_send_drop_pct: float = 0.0
    # 包大小统计
    avg_package_size: int = 0
    # tracker 统计
    connecting_tracker_count: int = 0
    connecting_tracker_details: List[Tuple[str, int]] = field(default_factory=list)
    http_tracker_connections: int = 0
    # 历史速率 (1s 采样)
    rate_history_second: Deque[Tuple[float, float, float]] = field(
        default_factory=lambda: deque(maxlen=300)
    )


@dataclass
class WireLinkLayer:
    """对应 Core_Wire::WireLinkLayer — 单个连接的链路层状态."""
    endpoint: Tuple[str, int]
    protocol: ProtocolEnum
    # 关闭原因 (本端 + 对端)
    local_close_reason: int = 0
    remote_close_reason: int = 0
    # 收发统计
    bytes_sent: int = 0
    bytes_received: int = 0
    # 状态
    is_alive: bool = True
    last_activity: float = field(default_factory=time.time)
    # 所属 group
    group_id: Optional[str] = None


@dataclass
class PendingSendKey:
    """对应 pending_queue_key_t — 用于按优先级排队."""
    priority: PendingQueuePriority
    timestamp: float
    seq: int


# -----------------------------------------------------------------------------
# TrackerHostBucket — tracker 负载均衡
# -----------------------------------------------------------------------------

class TrackerHostBucket:
    """对应 Core_Wire::tracker_host_bucket_t.

    BitComet 把所有 tracker URL 按 host 分桶, 同 host 的请求串行化,
    避免单个 tracker 因并发过高被 ban.
    """

    def __init__(self, max_per_host: int = 5, ban_threshold: int = 10,
                 ban_duration_sec: float = 300.0):
        self.max_per_host = max_per_host
        self.ban_threshold = ban_threshold
        self.ban_duration = ban_duration_sec
        # host → 当前并发数
        self._active: Dict[str, int] = defaultdict(int)
        # host → 失败次数
        self._failures: Dict[str, int] = defaultdict(int)
        # host → 解除 ban 时间
        self._banned_until: Dict[str, float] = {}

    def acquire(self, host: str) -> bool:
        """尝试获取一个 host 的 slot."""
        # 检查 ban
        if host in self._banned_until:
            if time.time() < self._banned_until[host]:
                return False
            else:
                del self._banned_until[host]
                self._failures[host] = 0
        # 检查并发
        if self._active[host] >= self.max_per_host:
            return False
        self._active[host] += 1
        return True

    def release(self, host: str, success: bool = True) -> None:
        if self._active[host] > 0:
            self._active[host] -= 1
        if success:
            self._failures[host] = 0
        else:
            self._failures[host] += 1
            if self._failures[host] >= self.ban_threshold:
                self._banned_until[host] = time.time() + self.ban_duration
                LOG.warning("tracker host %s banned for %ds (failed %d times)",
                            host, int(self.ban_duration), self._failures[host])

    def is_banned(self, host: str) -> bool:
        return (host in self._banned_until and
                time.time() < self._banned_until[host])

    def get_stats(self) -> Dict[str, Dict[str, int]]:
        return {
            host: {
                "active": self._active[host],
                "failures": self._failures[host],
                "banned": 1 if self.is_banned(host) else 0,
            }
            for host in set(self._active) | set(self._banned_until)
        }


# -----------------------------------------------------------------------------
# PendingSendQueue — 优先级发送队列
# -----------------------------------------------------------------------------

class PendingSendQueue:
    """对应 Core_Wire 的 pending_queue (按 priority + timestamp 排序)."""

    def __init__(self):
        # priority → deque[(key, payload)]
        self._queues: Dict[PendingQueuePriority, Deque[Tuple[PendingSendKey, bytes]]] = {
            p: deque() for p in PendingQueuePriority
        }
        self._lock = threading.Lock()
        self._seq = 0

    def push(self, priority: PendingQueuePriority, payload: bytes) -> PendingSendKey:
        with self._lock:
            self._seq += 1
            key = PendingSendKey(
                priority=priority,
                timestamp=time.time(),
                seq=self._seq,
            )
            self._queues[priority].append((key, payload))
            return key

    def pop(self) -> Optional[Tuple[PendingSendKey, bytes]]:
        """按优先级取 (URGENT > HIGH > NORMAL > LOW)."""
        with self._lock:
            for p in sorted(PendingQueuePriority, reverse=True):
                if self._queues[p]:
                    return self._queues[p].popleft()
            return None

    def size(self) -> int:
        with self._lock:
            return sum(len(q) for q in self._queues.values())


# -----------------------------------------------------------------------------
# UDPPool — 共享 UDP socket 池
# -----------------------------------------------------------------------------

class UDPPool:
    """对应 Core_Wire::UDPPool.

    BitComet 让所有 UDP-based 协议 (uTP / DHT / UDP tracker / BCIP / LT-Seed UDP)
    共享同一个 UDP socket, 避免每个协议各开一个 socket.
    """

    def __init__(self, bind_port: int = 0):
        self.bind_port = bind_port
        self._socket: Optional[socket.socket] = None
        # 分发: (caller_id) → callback
        self._callbacks: Dict[str, Callable[[bytes, Tuple[str, int]], None]] = {}
        self._lock = threading.Lock()
        self._running = False
        self._thread: Optional[threading.Thread] = None
        # 统计
        self.stats = {
            "bytes_sent": 0, "bytes_received": 0,
            "packets_sent": 0, "packets_received": 0,
            "dispatch_errors": 0,
        }

    def register(self, caller_id: str,
                  callback: Callable[[bytes, Tuple[str, int]], None]) -> None:
        with self._lock:
            self._callbacks[caller_id] = callback

    def unregister(self, caller_id: str) -> None:
        with self._lock:
            self._callbacks.pop(caller_id, None)

    def start(self) -> None:
        if self._running:
            return
        self._socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._socket.bind(("0.0.0.0", self.bind_port))
        self.bind_port = self._socket.getsockname()[1]
        self._socket.settimeout(0.5)
        self._running = True
        self._thread = threading.Thread(target=self._recv_loop, daemon=True,
                                         name=f"UDPPool-{self.bind_port}")
        self._thread.start()
        LOG.info("UDPPool listening on port %d", self.bind_port)

    def stop(self) -> None:
        self._running = False
        if self._socket:
            self._socket.close()
        if self._thread:
            self._thread.join(timeout=2)

    def send(self, data: bytes, target: Tuple[str, int],
              caller_id: str = "default") -> int:
        if not self._socket:
            raise RuntimeError("UDPPool not started")
        try:
            n = self._socket.sendto(data, target)
            self.stats["bytes_sent"] += n
            self.stats["packets_sent"] += 1
            return n
        except OSError as e:
            LOG.error("UDPPool send failed: %s", e)
            return 0

    def _recv_loop(self) -> None:
        while self._running:
            try:
                data, addr = self._socket.recvfrom(65535)
            except socket.timeout:
                continue
            except OSError:
                break
            self.stats["bytes_received"] += len(data)
            self.stats["packets_received"] += 1
            # 分发: 前 2 字节是 caller_id hash (简化)
            if len(data) < 2:
                self.stats["dispatch_errors"] += 1
                continue
            caller_id_hash = struct.unpack(">H", data[:2])[0]
            # 找到对应 caller (简化: 用 hash 直接匹配)
            caller_id = self._find_caller_by_hash(caller_id_hash)
            if not caller_id:
                self.stats["dispatch_errors"] += 1
                continue
            cb = self._callbacks.get(caller_id)
            if cb:
                try:
                    cb(data[2:], addr)
                except Exception as e:
                    LOG.error("UDPPool dispatch callback error: %s", e)
                    self.stats["dispatch_errors"] += 1

    def _find_caller_by_hash(self, h: int) -> Optional[str]:
        """简化版 hash 匹配 (实际 BitComet 用 protocol_enum 作 caller_id)."""
        # 取第一个注册的 caller
        with self._lock:
            if not self._callbacks:
                return None
            # 用 hash % len 取一个
            keys = list(self._callbacks.keys())
            return keys[h % len(keys)]


# -----------------------------------------------------------------------------
# WireLinkLayerManager — Wire 链路层管理 (主入口)
# -----------------------------------------------------------------------------

class WireLinkLayerManager:
    """对应 Core_Wire::WireLinkLayer + WireLinkPool.

    统一管理所有 Wire 连接, 提供:
    - 多协议共享连接
    - close_reason 透传
    - 速率统计
    - tracker 分桶
    """

    def __init__(self, settings: Optional[WireSettings] = None):
        self.settings = settings or WireSettings()
        # endpoint → WireLinkLayer
        self._links: Dict[Tuple[str, int], WireLinkLayer] = {}
        # 待发送队列 (按 endpoint 分组)
        self._pending: Dict[Tuple[str, int], PendingSendQueue] = defaultdict(PendingSendQueue)
        # tracker 分桶
        self._tracker_bucket = TrackerHostBucket()
        # UDP 池 (共享)
        self._udp_pool = UDPPool()
        # 统计
        self.stats = WireStats()
        # 锁
        self._lock = threading.RLock()
        # 回调: 收到对端 close_reason
        self._on_remote_close: Optional[Callable] = None

    # ----- 公开 API: 连接管理 -----

    def attach(self, endpoint: Tuple[str, int], protocol: ProtocolEnum) -> WireLinkLayer:
        """对应 protocol_attach_wire."""
        with self._lock:
            if endpoint in self._links:
                return self._links[endpoint]
            link = WireLinkLayer(endpoint=endpoint, protocol=protocol)
            self._links[endpoint] = link
            self.stats.tcp_connections = len(self._links)
            LOG.info("attached Wire link: %s protocol=%s", endpoint, protocol.name)
            return link

    def detach(self, endpoint: Tuple[str, int]) -> None:
        """对应 protocol_detach_wire."""
        with self._lock:
            link = self._links.pop(endpoint, None)
            if link:
                link.is_alive = False
                self.stats.tcp_connections = len(self._links)

    def set_close_reason(self, endpoint: Tuple[str, int], reason: int) -> None:
        """对应 WireLinkLayer::wire_set_close_reason."""
        with self._lock:
            link = self._links.get(endpoint)
            if link:
                link.local_close_reason = reason
                LOG.info("set close_reason %d for %s", reason, endpoint)

    def get_remote_close_reason(self, endpoint: Tuple[str, int]) -> int:
        """对应 WireLinkLayer::wire_get_remote_close_reason."""
        with self._lock:
            link = self._links.get(endpoint)
            return link.remote_close_reason if link else 0

    def set_remote_close_reason(self, endpoint: Tuple[str, int], reason: int) -> None:
        """对端报告的 close_reason (收到对端 ut_close)."""
        with self._lock:
            link = self._links.get(endpoint)
            if link:
                link.remote_close_reason = reason
                if self._on_remote_close:
                    self._on_remote_close(endpoint, reason)

    # ----- 公开 API: 发送队列 -----

    def enqueue_send(self, endpoint: Tuple[str, int], payload: bytes,
                      priority: PendingQueuePriority = PendingQueuePriority.NORMAL) -> None:
        """对应 WireLinkLayer::wire_need_send 的入队."""
        self._pending[endpoint].push(priority, payload)

    def dequeue_send(self, endpoint: Tuple[str, int]) -> Optional[bytes]:
        """对应 WireLinkLayer::wire_need_send_i 的实际发送."""
        q = self._pending.get(endpoint)
        if not q:
            return None
        item = q.pop()
        return item[1] if item else None

    # ----- 公开 API: 统计 -----

    def update_stats(self) -> None:
        """对应 InterfaceWire::get_stats_rate_second.

        每秒采样一次, 维护历史时间序列.
        """
        now = time.time()
        total_send = sum(l.bytes_sent for l in self._links.values())
        total_recv = sum(l.bytes_received for l in self._links.values())
        self.stats.rate_history_second.append((now, total_send, total_recv))

    def get_stats_rate(self) -> Tuple[float, float]:
        """对应 get_stats_rate — 返回 (send_bps, recv_bps)."""
        if len(self.stats.rate_history_second) < 2:
            return (0, 0)
        t1, s1, r1 = self.stats.rate_history_second[-2]
        t2, s2, r2 = self.stats.rate_history_second[-1]
        dt = max(t2 - t1, 0.001)
        return ((s2 - s1) * 8 / dt, (r2 - r1) * 8 / dt)

    def get_stats(self) -> WireStats:
        return self.stats

    def get_stats_connecting_tracker_details(self) -> List[Tuple[str, int]]:
        """对应 get_stats_connecting_tracker_details."""
        return self._tracker_bucket.get_stats().__class__ and list(
            (host, info["active"]) for host, info in self._tracker_bucket.get_stats().items()
        )

    # ----- 公开 API: tracker 分桶 -----

    def acquire_tracker_slot(self, host: str) -> bool:
        return self._tracker_bucket.acquire(host)

    def release_tracker_slot(self, host: str, success: bool = True) -> None:
        self._tracker_bucket.release(host, success)

    # ----- 公开 API: UDP 池 -----

    def start_udp_pool(self, port: int = 0) -> int:
        """启动共享 UDP socket, 返回实际绑定端口."""
        self._udp_pool.bind_port = port
        self._udp_pool.start()
        return self._udp_pool.bind_port

    def stop_udp_pool(self) -> None:
        self._udp_pool.stop()

    def udp_send(self, data: bytes, target: Tuple[str, int],
                  caller_id: str = "default") -> int:
        return self._udp_pool.send(data, target, caller_id)

    def udp_register(self, caller_id: str,
                      callback: Callable[[bytes, Tuple[str, int]], None]) -> None:
        self._udp_pool.register(caller_id, callback)

    # ----- 回调 -----

    def set_remote_close_callback(self, cb: Callable[[Tuple[str, int], int], None]) -> None:
        self._on_remote_close = cb


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    logging.basicConfig(level=logging.INFO,
                        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s")
    ap = argparse.ArgumentParser(description="Core_Wire 协议层 demo")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p_udp = sub.add_parser("udp", help="启动共享 UDP socket")
    p_udp.add_argument("--port", type=int, default=0)

    p_t = sub.add_parser("tracker", help="演示 tracker 分桶")
    p_t.add_argument("--host", default="tracker.example.com")
    p_t.add_argument("--count", type=int, default=10)

    args = ap.parse_args()
    mgr = WireLinkLayerManager()

    if args.cmd == "udp":
        port = mgr.start_udp_pool(args.port)
        print(f"UDP pool started on port {port}")
        # 注册一个 caller
        mgr.udp_register("test", lambda data, addr: print(f"  recv {len(data)} bytes from {addr}"))
        # 发个包给自己
        import struct as s
        # data: hash(2) + payload
        data = s.pack(">H", 0) + b"hello"
        mgr.udp_send(data, ("127.0.0.1", port), "test")
        import time
        time.sleep(0.5)
        mgr.stop_udp_pool()
        print(f"stats: {mgr._udp_pool.stats}")

    elif args.cmd == "tracker":
        print(f"acquiring {args.count} slots for {args.host}...")
        for i in range(args.count):
            ok = mgr.acquire_tracker_slot(args.host)
            print(f"  [{i+1}] acquire: {ok}")
        print(f"\nstats: {mgr._tracker_bucket.get_stats()}")
        # 模拟失败
        for i in range(15):
            mgr.release_tracker_slot(args.host, success=False)
        print(f"\nafter 15 failures:")
        print(f"stats: {mgr._tracker_bucket.get_stats()}")
        print(f"is banned: {mgr._tracker_bucket.is_banned(args.host)}")
