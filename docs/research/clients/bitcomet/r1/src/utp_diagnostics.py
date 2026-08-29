"""
utp_diagnostics.py — UTP 拥塞控制与丢包诊断
==========================================

逆向来源: BitComet `Core_Socket` + `Core_Wire` 命名空间
关键符号:
    Core_Socket::utp_connection
    Core_Socket::InterfaceSocketUTP
    Core_Socket::udp_service
    Core_Socket::udp_package_t
    Core_Socket::utp_packet_t
    Core_Socket::async_connection
    Core_Wire::InterfaceWire::get_stats_rate_udp
    Core_Wire::InterfaceWire::get_stats_rate_udp_by_caller
    Core_Wire::InterfaceWire::get_utp_recv_drop_percent
    Core_Wire::InterfaceWire::get_utp_send_drop_percent
    Core_Wire::InterfaceWire::get_stats_rate
    Core_Wire::InterfaceWire::get_stats_rate_second
    Core_Wire::InterfaceWire::get_stats_rate_max_possible
    Core_Wire::InterfaceWire::get_stats_tcp_connections
    Core_Wire::InterfaceWire::get_stats_package_size
    Core_Wire::InterfaceWire::get_stats_connecting_tracker_count
    Core_Wire::InterfaceWire::get_stats_connecting_tracker_details
    Core_Wire::InterfaceWire::get_stats_http_tracker_connections
    Core_Wire::InterfaceWire::get_stats_connection

设计核心 (从符号分析):
1. BitComet 把 uTP 实现完全自主, 不依赖 libtorrent 的 uTP
2. InterfaceWire 提供丰富的统计接口 (rx/tx rate, drop%, conn count)
3. 每秒采样 rate_second, 用于绘制速度图
4. utp_recv_drop_percent / utp_send_drop_percent: 丢包率监控
5. stats_connecting_tracker_count: tracker 连接状态实时

加速价值 (针对 qBittorrent):
- qBittorrent 用 libtorrent 内置 uTP, 不可观测内部状态
- 当 uTP 速度异常时, qBittorrent 无法诊断
- BitComet 可以实时显示每个 peer 的 uTP 拥塞窗口、RTT、丢包率

本模块实现:
- UtpStats: uTP socket 统计采集器
- UtpDiagnostics: 拥塞窗口 + 丢包率 + RTT 监控
- 不实际实现 uTP 协议, 但提供诊断接口 (作为 libtorrent 的旁路监控)

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import logging
import socket
import statistics
import struct
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from typing import Deque, Dict, List, Optional, Tuple

LOG = logging.getLogger("utp_diag")


# -----------------------------------------------------------------------------
# UtpStats — 对应 utp_packet_t / utp_connection 的统计字段
# -----------------------------------------------------------------------------

@dataclass
class UtpStats:
    """单个 uTP 连接的统计 (对应 Core_Socket::utp_connection 的状态字段)."""
    endpoint: Tuple[str, int]
    # 收发统计
    bytes_sent: int = 0
    bytes_received: int = 0
    packets_sent: int = 0
    packets_received: int = 0
    # 丢包统计
    packets_lost: int = 0
    packets_retransmitted: int = 0
    # 拥塞控制
    cwnd: int = 0              # 拥塞窗口 (字节)
    rtt_ms: float = 0.0        # 平滑 RTT
    rtt_var_ms: float = 0.0    # RTT 方差
    rto_ms: float = 1000.0     # 重传超时
    # LEDBAT-style delay
    base_delay_ms: float = 0.0
    current_delay_ms: float = 0.0
    off_target: float = 0.0    # 偏离目标延迟
    # 速度
    send_rate_bps: float = 0.0
    recv_rate_bps: float = 0.0
    # 状态
    is_alive: bool = True
    last_activity: float = field(default_factory=time.time)
    # 滚动窗口
    _send_history: Deque[Tuple[float, int]] = field(default_factory=lambda: deque(maxlen=60))
    _recv_history: Deque[Tuple[float, int]] = field(default_factory=lambda: deque(maxlen=60))


# -----------------------------------------------------------------------------
# UtpDiagnostics — 诊断与告警
# -----------------------------------------------------------------------------

class UtpDiagnostics:
    """对应 Core_Wire::InterfaceWire 的统计接口.

    每秒采样一次, 维护历史时间序列.
    """

    def __init__(self, sample_interval_sec: float = 1.0,
                 history_size: int = 300):
        """300 秒历史 = 5 分钟."""
        self.sample_interval = sample_interval_sec
        self.history_size = history_size
        # endpoint → UtpStats
        self._stats: Dict[Tuple[str, int], UtpStats] = {}
        # 全局时间序列
        self._rate_history: Deque[Tuple[float, float, float]] = deque(
            maxlen=history_size  # (ts, send_bps, recv_bps)
        )
        self._drop_history: Deque[Tuple[float, float, float]] = deque(
            maxlen=history_size  # (ts, recv_drop%, send_drop%)
        )
        # 告警回调
        self._alert_callbacks: List[callable] = []
        # 强制每个 sample 都记录 (即使 dt < interval)
        self._force_sample = False
        self._last_sample = time.time()
        self._last_bytes: Dict[Tuple[str, int], Tuple[int, int]] = {}

    # ----- 公开 API -----

    def add_socket(self, endpoint: Tuple[str, int]) -> UtpStats:
        if endpoint not in self._stats:
            self._stats[endpoint] = UtpStats(endpoint=endpoint)
        return self._stats[endpoint]

    def remove_socket(self, endpoint: Tuple[str, int]) -> None:
        self._stats.pop(endpoint, None)
        self._last_bytes.pop(endpoint, None)

    def update_socket(self, endpoint: Tuple[str, int],
                      bytes_sent: int = 0, bytes_received: int = 0,
                      packets_sent: int = 0, packets_received: int = 0,
                      packets_lost: int = 0, packets_retransmitted: int = 0,
                      cwnd: Optional[int] = None,
                      rtt_ms: Optional[float] = None,
                      rto_ms: Optional[float] = None,
                      base_delay_ms: Optional[float] = None,
                      current_delay_ms: Optional[float] = None,
                      off_target: Optional[float] = None) -> None:
        """更新 socket 统计 (从 libtorrent peer_info 转换)."""
        s = self.add_socket(endpoint)
        s.bytes_sent += bytes_sent
        s.bytes_received += bytes_received
        s.packets_sent += packets_sent
        s.packets_received += packets_received
        s.packets_lost += packets_lost
        s.packets_retransmitted += packets_retransmitted
        if cwnd is not None: s.cwnd = cwnd
        if rtt_ms is not None:
            # EWMA
            if s.rtt_ms == 0:
                s.rtt_ms = rtt_ms
                s.rtt_var_ms = rtt_ms / 2
            else:
                diff = abs(rtt_ms - s.rtt_ms)
                s.rtt_var_ms = s.rtt_var_ms * 0.75 + diff * 0.25
                s.rtt_ms = s.rtt_ms * 0.875 + rtt_ms * 0.125
            # RTO = RTT + 4*RTTVAR (RFC 6298)
            s.rto_ms = s.rtt_ms + 4 * s.rtt_var_ms
        if rto_ms is not None: s.rto_ms = rto_ms
        if base_delay_ms is not None: s.base_delay_ms = base_delay_ms
        if current_delay_ms is not None: s.current_delay_ms = current_delay_ms
        if off_target is not None: s.off_target = off_target
        s.last_activity = time.time()

    def force_sample(self) -> None:
        """强制立即采样 (用于测试或非阻塞场景)."""
        self._force_sample = True
        self.sample()

    def sample(self) -> None:
        """对应 InterfaceWire::get_stats_rate_second.

        每秒调用一次, 计算速率并记入历史.
        若距上次采样不足间隔, 默认跳过; 设 self._force_sample=True 强制采样.
        """
        now = time.time()
        if not self._force_sample and now - self._last_sample < self.sample_interval:
            return
        self._force_sample = False
        dt = max(now - self._last_sample, 0.001)
        self._last_sample = now

        total_send_bps = 0
        total_recv_bps = 0
        total_recv_drop = 0
        total_send_drop = 0
        count = 0

        for endpoint, s in self._stats.items():
            prev = self._last_bytes.get(endpoint, (0, 0))
            send_delta = s.bytes_sent - prev[0]
            recv_delta = s.bytes_received - prev[1]
            s.send_rate_bps = (send_delta * 8) / dt
            s.recv_rate_bps = (recv_delta * 8) / dt
            s._send_history.append((now, s.bytes_sent))
            s._recv_history.append((now, s.bytes_received))
            self._last_bytes[endpoint] = (s.bytes_sent, s.bytes_received)
            total_send_bps += s.send_rate_bps
            total_recv_bps += s.recv_rate_bps
            # 丢包率 (RFC 6298: loss = retrans / sent)
            if s.packets_sent > 0:
                recv_drop_pct = (s.packets_lost / max(s.packets_received + s.packets_lost, 1)) * 100
                send_drop_pct = (s.packets_retransmitted / max(s.packets_sent, 1)) * 100
            else:
                recv_drop_pct = 0
                send_drop_pct = 0
            total_recv_drop += recv_drop_pct
            total_send_drop += send_drop_pct
            count += 1
        if count > 0:
            avg_recv_drop = total_recv_drop / count
            avg_send_drop = total_send_drop / count
        else:
            avg_recv_drop = avg_send_drop = 0
        self._rate_history.append((now, total_send_bps, total_recv_bps))
        self._drop_history.append((now, avg_recv_drop, avg_send_drop))

        # 告警检查
        self._check_alerts()

    def add_alert_callback(self, callback: callable) -> None:
        self._alert_callbacks.append(callback)

    # ----- 查询接口 (对应 InterfaceWire::get_*) -----

    def get_stats_rate(self) -> Tuple[float, float]:
        """对应 get_stats_rate — 返回总速率 (send, recv) bps."""
        if not self._rate_history:
            return (0, 0)
        ts, s, r = self._rate_history[-1]
        return (s, r)

    def get_stats_rate_second(self) -> List[Tuple[float, float, float]]:
        """对应 get_stats_rate_second — 返回历史速率."""
        return list(self._rate_history)

    def get_stats_rate_max_possible(self) -> float:
        """对应 get_stats_rate_max_possible — 估算理论最大速率."""
        # 简化: 取历史最大值 * 1.2
        if not self._rate_history:
            return 0
        max_rate = max(max(s, r) for _, s, r in self._rate_history)
        return max_rate * 1.2

    def get_utp_recv_drop_percent(self) -> float:
        if not self._drop_history:
            return 0
        return self._drop_history[-1][1]

    def get_utp_send_drop_percent(self) -> float:
        if not self._drop_history:
            return 0
        return self._drop_history[-1][2]

    def get_stats_tcp_connections(self) -> int:
        """活动 socket 数 (TCP+UTP)."""
        return sum(1 for s in self._stats.values() if s.is_alive)

    def get_stats_connecting_tracker_count(self) -> int:
        """连接中的 tracker 数 (这里把所有 socket 算入)."""
        return len(self._stats)

    def get_per_socket_stats(self) -> Dict[Tuple[str, int], Dict[str, float]]:
        """每个 socket 的详细统计."""
        result = {}
        for ep, s in self._stats.items():
            result[ep] = {
                "send_rate_bps": s.send_rate_bps,
                "recv_rate_bps": s.recv_rate_bps,
                "rtt_ms": s.rtt_ms,
                "rtt_var_ms": s.rtt_var_ms,
                "rto_ms": s.rto_ms,
                "cwnd_bytes": s.cwnd,
                "bytes_sent": s.bytes_sent,
                "bytes_received": s.bytes_received,
                "packets_lost": s.packets_lost,
                "packets_retransmitted": s.packets_retransmitted,
                "base_delay_ms": s.base_delay_ms,
                "current_delay_ms": s.current_delay_ms,
                "off_target": s.off_target,
            }
        return result

    # ----- 内部: 告警 -----

    def _check_alerts(self) -> None:
        for ep, s in self._stats.items():
            # 高丢包率告警
            if s.packets_sent > 100:
                loss_pct = (s.packets_retransmitted / s.packets_sent) * 100
                if loss_pct > 10:
                    self._fire_alert({
                        "type": "HIGH_LOSS_RATE",
                        "endpoint": ep,
                        "loss_pct": loss_pct,
                        "msg": f"uTP {ep[0]}:{ep[1]} loss rate {loss_pct:.1f}%"
                    })
            # 高 RTT 告警
            if s.rtt_ms > 500:
                self._fire_alert({
                    "type": "HIGH_RTT",
                    "endpoint": ep,
                    "rtt_ms": s.rtt_ms,
                    "msg": f"uTP {ep[0]}:{ep[1]} RTT {s.rtt_ms:.0f}ms"
                })
            # 拥塞窗口过小告警
            if s.cwnd > 0 and s.cwnd < 4096:
                self._fire_alert({
                    "type": "SMALL_CWND",
                    "endpoint": ep,
                    "cwnd": s.cwnd,
                    "msg": f"uTP {ep[0]}:{ep[1]} cwnd too small ({s.cwnd} bytes)"
                })

    def _fire_alert(self, alert: Dict) -> None:
        LOG.warning("uTP alert: %s", alert)
        for cb in self._alert_callbacks:
            try:
                cb(alert)
            except Exception as e:
                LOG.error("alert callback failed: %s", e)


# -----------------------------------------------------------------------------
# LibtorrentPeerInfoAdapter — 从 libtorrent peer_info 转换
# -----------------------------------------------------------------------------

class LibtorrentPeerInfoAdapter:
    """把 libtorrent 的 peer_info 转换为本模块的 UtpStats 更新.

    使用方式:
        def on_peer_alert(alert):
            handle = alert.handle
            for peer in handle.get_peer_info():
                if peer.flags & peer.utp_socket:
                    adapter.update_from_lt_peer_info(diag, peer)
    """

    @staticmethod
    def update_from_lt_peer_info(diag: UtpDiagnostics, peer_info) -> None:
        try:
            import libtorrent as lt
        except ImportError:
            return
        ep = (peer_info.ip[0], peer_info.ip[1])
        # 计算 RTT (libtorrent 提供)
        rtt_ms = peer_info.rtt.total_milliseconds() if hasattr(peer_info.rtt, 'total_milliseconds') else 0
        diag.update_socket(
            endpoint=ep,
            bytes_sent=peer_info.total_upload,
            bytes_received=peer_info.total_download,
            packets_sent=getattr(peer_info, 'packets_sent', 0),
            packets_received=getattr(peer_info, 'packets_received', 0),
            packets_lost=getattr(peer_info, 'packets_lost', 0),
            packets_retransmitted=getattr(peer_info, 'packets_retransmitted', 0),
            rtt_ms=rtt_ms,
        )


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )

    ap = argparse.ArgumentParser(description="UTP Diagnostics demo")
    ap.add_argument("--peers", type=int, default=3, help="simulated peer count")
    ap.add_argument("--duration", type=int, default=10, help="simulation seconds")
    args = ap.parse_args()

    diag = UtpDiagnostics()
    diag.add_alert_callback(lambda a: print(f"  ⚠ ALERT: {a['msg']}"))

    # 模拟 N 个 uTP socket
    import random
    for i in range(args.peers):
        ep = (f"10.0.0.{i+1}", 6881 + i)
        diag.add_socket(ep)
        diag.update_socket(ep, cwnd=64*1024, rtt_ms=20.0 + i*5)

    print(f"simulating {args.peers} uTP sockets for {args.duration}s...\n")
    start = time.time()
    while time.time() - start < args.duration:
        # 模拟数据传输
        for ep in list(diag._stats.keys()):
            bytes = random.randint(50000, 200000)
            lost = random.randint(0, 10)
            diag.update_socket(
                ep, bytes_sent=bytes, bytes_received=bytes,
                packets_sent=100, packets_received=98,
                packets_lost=lost, packets_retransmitted=lost,
            )
        diag.sample()
        time.sleep(1)
        rate_s, rate_r = diag.get_stats_rate()
        recv_drop = diag.get_utp_recv_drop_percent()
        send_drop = diag.get_utp_send_drop_percent()
        print(f"  rate: send={rate_s/1e6:.2f} Mbps  recv={rate_r/1e6:.2f} Mbps  "
              f"recv_drop={recv_drop:.1f}%  send_drop={send_drop:.1f}%  "
              f"sockets={diag.get_stats_tcp_connections()}")

    print("\n=== Per-socket detail ===")
    for ep, stats in diag.get_per_socket_stats().items():
        print(f"  {ep[0]}:{ep[1]:5d}  rtt={stats['rtt_ms']:.0f}ms  "
              f"rto={stats['rto_ms']:.0f}ms  cwnd={stats['cwnd_bytes']}B")
