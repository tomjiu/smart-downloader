"""
libtorrent_integration.py — 与 libtorrent (qBittorrent 引擎) 集成示例

前置条件:
    pip install libtorrent aiohttp psutil

本示例展示如何把 accel_toolkit 的 4 个 P0/P1 模块
与 libtorrent Python binding 集成.

注: 如果没有 libtorrent, 示例会自动用 mock 模式运行 (用于演示).
"""
import asyncio
import logging
import os
import sys
import threading
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

# 尝试导入 libtorrent
try:
    import libtorrent as lt
    HAS_LIBTORRENT = True
except ImportError:
    HAS_LIBTORRENT = False
    print("[warn] libtorrent not installed, running in mock mode")

from anti_leech_filter import (
    AntiLeechFilter, AntiLeechLevel, AntiLeechAction,
)
from utp_diagnostics import UtpDiagnostics
from peer_broadcast_optimizer import PeerBroadcastOptimizer, BtMsg


logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
)
LOG = logging.getLogger("integration")


# -----------------------------------------------------------------------------
# Mock libtorrent (用于演示, 没有真正安装 libtorrent 时)
# -----------------------------------------------------------------------------

class MockPeerInfo:
    def __init__(self, ip, port, peer_id, total_up=0, total_down=0,
                 choked=False, snubbed=False):
        self.ip = (ip, port)
        self.pid = peer_id
        self.total_upload = total_up
        self.total_download = total_down
        self.flags = 0
        if choked: self.flags |= 1
        if snubbed: self.flags |= 2
        self.rtt = type("RTT", (), {"total_milliseconds": lambda self: 50.0})()


class MockTorrentHandle:
    def __init__(self):
        self.peers = []
        self.pieces_done = set()

    def get_peer_info(self):
        return list(self.peers)

    def add_peer(self, peer):
        self.peers.append(peer)

    def disconnect_peer(self, peer):
        self.peers.remove(peer)
        LOG.info(f"  [lt] disconnected peer {peer.ip}")

    def set_peer_upload_limit(self, peer, limit):
        LOG.info(f"  [lt] set upload limit {limit} for {peer.ip}")

    def piece_finished(self, idx):
        self.pieces_done.add(idx)


# -----------------------------------------------------------------------------
# 集成主类
# -----------------------------------------------------------------------------

class LibtorrentAccelIntegration:
    """把 accel_toolkit 集成到 libtorrent session."""

    def __init__(self, lt_session):
        self.session = lt_session

        # 1. AntiLeech 过滤器 (P0)
        self.antileech = AntiLeechFilter(level=AntiLeechLevel.LIMIT)

        # 2. UTP 诊断 (P2)
        self.utp_diag = UtpDiagnostics()
        self.utp_diag.add_alert_callback(self._on_utp_alert)

        # 3. Peer 广播优化 (P1)
        self.broadcast_opt = PeerBroadcastOptimizer(
            send_callback=self._send_via_lt,
            flush_interval_ms=100,
        )

        # 统计
        self.stats = {
            "peers_banned": 0,
            "peers_limited": 0,
            "alerts_processed": 0,
        }

    def on_peer_alert(self, alert):
        """处理 libtorrent peer_alert.

        在实际使用时:
            ses.set_alert_notify(lambda: integration.on_peer_alert(ses.pop_alerts()))
        """
        self.stats["alerts_processed"] += 1
        if not isinstance(alert, lt.peer_alert if HAS_LIBTORRENT else type):
            return
        handle = alert.handle
        try:
            peers = handle.get_peer_info()
        except Exception:
            return

        for peer in peers:
            ep = peer.ip
            # AntiLeech 决策
            if ep not in self.antileech._peers:
                pid_bytes = peer.pid.to_bytes() if hasattr(peer.pid, 'to_bytes') else bytes(peer.pid)
                self.antileech.add_peer(ep, pid_bytes)
            self.antileech.update_stats(
                ep, downloaded=peer.total_download, uploaded=peer.total_upload,
                is_choking_us=bool(peer.flags & 1),
                snubbed=bool(peer.flags & 2),
            )
            action = self.antileech.decide(ep)
            if action == AntiLeechAction.DISCONNECT:
                handle.disconnect_peer(peer)
                self.stats["peers_banned"] += 1
            elif action == AntiLeechAction.LIMIT_25:
                handle.set_peer_upload_limit(peer, 25_000)
                self.stats["peers_limited"] += 1

            # UTP 诊断更新
            self.utp_diag.update_socket(
                ep,
                bytes_sent=peer.total_upload,
                bytes_received=peer.total_download,
                rtt_ms=peer.rtt.total_milliseconds() if hasattr(peer.rtt, 'total_milliseconds') else 0,
            )

    def on_piece_finished_alert(self, alert):
        """处理 piece_finished_alert, 优化广播."""
        # 实际上 libtorrent 自己广播 HAVE, 这里只做统计
        # 如果用 PeerBroadcastOptimizer 接管, 需要 plugin
        self.broadcast_opt.broadcast_have(alert.piece_index)

    def _send_via_lt(self, endpoint, msg_type, payload):
        """通过 libtorrent raw peer message 发送 (需要 plugin)."""
        # 占位: 实际集成需要写 libtorrent plugin
        pass

    def _on_utp_alert(self, alert: dict):
        LOG.warning("[UTP] %s", alert["msg"])

    def get_stats(self):
        utp_rate = self.utp_diag.get_stats_rate()
        return {
            **self.stats,
            "utp_send_rate_bps": utp_rate[0],
            "utp_recv_rate_bps": utp_rate[1],
            "utp_recv_drop_pct": self.utp_diag.get_utp_recv_drop_percent(),
            "utp_send_drop_pct": self.utp_diag.get_utp_send_drop_percent(),
            "antileech_stats": self.antileech.get_stats(),
            "broadcast_deduped": self.broadcast_opt.stats_deduped,
        }


# -----------------------------------------------------------------------------
# 演示
# -----------------------------------------------------------------------------

async def demo():
    print("=" * 60)
    print("libtorrent + accel_toolkit 集成演示")
    print("=" * 60)

    # 创建 mock session
    handle = MockTorrentHandle()

    # 注入 5 个 peer (含迅雷 + qBittorrent)
    handle.add_peer(MockPeerInfo(
        "1.2.3.4", 6881, b"-XL0001-abcdefghij",  # 迅雷, 上传吝啬
        total_up=1000000, total_down=1000, choked=False,
    ))
    handle.add_peer(MockPeerInfo(
        "5.6.7.8", 6881, b"-qB4500-abcdefghij",  # qBittorrent, 公平
        total_up=500000, total_down=400000,
    ))
    handle.add_peer(MockPeerInfo(
        "9.10.11.12", 6881, b"-SD0001-abcdefghij",  # 迅雷 Mini
        total_up=2000000, total_down=500,
    ))
    handle.add_peer(MockPeerInfo(
        "13.14.15.16", 6881, b"-TR2000-abcdefghij",  # Transmission
        total_up=300000, total_down=300000,
    ))

    # 创建 mock alert (实际是 lt.peer_alert)
    class MockAlert:
        pass
    alert = MockAlert()
    alert.handle = handle

    # 创建集成实例
    if not HAS_LIBTORRENT:
        # 用 mock 路径
        integration = LibtorrentAccelIntegration(None)
        # 直接调用内部逻辑
        integration.on_peer_alert = lambda a: _mock_handle_alert(integration, handle)
    else:
        integration = LibtorrentAccelIntegration(lt.session())

    # 模拟 alert 触发
    print("\n[1] 模拟 peer_alert 触发...")
    if not HAS_LIBTORRENT:
        _mock_handle_alert(integration, handle)
    else:
        integration.on_peer_alert(alert)

    # 等一会, 让 UTP 诊断采样
    print("\n[2] 等待 2 秒采样 UTP 诊断...")
    integration.utp_diag.force_sample()
    await asyncio.sleep(0.5)
    integration.utp_diag.force_sample()

    # 输出统计
    print("\n" + "=" * 60)
    print("集成统计")
    print("=" * 60)
    stats = integration.get_stats()
    for k, v in stats.items():
        if isinstance(v, dict):
            print(f"  {k}:")
            for k2, v2 in v.items():
                print(f"    {k2}: {v2}")
        else:
            print(f"  {k}: {v}")


def _mock_handle_alert(integration: LibtorrentAccelIntegration, handle: MockTorrentHandle):
    """Mock 模式下直接遍历 peer, 调用 antileech."""
    for peer in handle.get_peer_info():
        ep = peer.ip
        if ep not in integration.antileech._peers:
            integration.antileech.add_peer(ep, peer.pid)
        integration.antileech.update_stats(
            ep, downloaded=peer.total_download, uploaded=peer.total_upload,
        )
        action = integration.antileech.decide(ep)
        LOG.info(f"peer {ep} {peer.pid[:8]} action={action.name}")
        if action == AntiLeechAction.DISCONNECT:
            handle.disconnect_peer(peer)
            integration.stats["peers_banned"] += 1
        elif action == AntiLeechAction.LIMIT_25:
            integration.stats["peers_limited"] += 1
        # UTP diag
        integration.utp_diag.update_socket(
            ep,
            bytes_sent=peer.total_upload,
            bytes_received=peer.total_download,
            rtt_ms=50.0,
        )


if __name__ == "__main__":
    asyncio.run(demo())
