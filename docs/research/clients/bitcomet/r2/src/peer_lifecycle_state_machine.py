"""
peer_lifecycle_state_machine.py — BitComet Peer 6 状态机
==================================================

逆向来源: Core_BitTorrent::PeerPoolBase + BitTorrentPeer + peer_active_t
关键符号:
    PeerPoolBase::peer_put_into_new              ← 状态: NEW
    PeerPoolBase::peer_put_into_connecting       ← 状态: CONNECTING
    PeerPoolBase::peer_put_into_connected        ← 状态: CONNECTED
    PeerPoolBase::peer_put_into_dead             ← 状态: DEAD
    PeerPoolBase::peer_put_into_banned           ← 状态: BANNED
    PeerPoolBase::peer_put_into_seen             ← 状态: SEEN
    PeerPoolBase::peer_add
    PeerPoolBase::peer_add_for_connect
    PeerPoolBase::peer_abort_connecting
    PeerPoolBase::peer_remove
    PeerPoolBase::peer_remove_and_merge
    PeerPoolBase::peer_remove_and_put_into
    PeerPoolBase::peer_remove_dead_auto
    PeerPoolBase::peer_remove_from_connected
    PeerPoolBase::peer_remove_from_connecting
    PeerPoolBase::peer_remove_from_waiting_list
    PeerPoolBase::peer_ban
    PeerPoolBase::peer_ban_waiting_by_ipfilter
    PeerPoolBase::peer_unban
    PeerPoolBase::peer_unban_all
    PeerPoolBase::peer_disconnect
    PeerPoolBase::peer_disconnect_all
    PeerPoolBase::peer_is_valid
    PeerPoolBase::protocol_attach
    PeerPoolBase::protocol_detach
    PeerPoolBase::protocol_handshake_passed
    PeerPoolBase::protocol_outgoing_connected
    PeerPoolBase::protocol_outgoing_connecting_started
    PeerPoolBase::protocol_outgoing_failed
    PeerPoolBase::save
    PeerPoolBase::is_loaded
    PeerPoolBase::num_peers
    PeerPoolBase::clear
    PeerPoolBase::load

    BitTorrentPeer::on_connected
    BitTorrentPeer::on_disconnected
    BitTorrentPeer::on_send_buffer_empty
    BitTorrentPeer::peer_auto_connect
    BitTorrentPeer::peer_auto_disconnect
    BitTorrentPeer::peer_connect_by_holepunch
    BitTorrentPeer::peer_create_from_incoming
    BitTorrentPeer::peer_reconnect_use_DHE
    BitTorrentPeer::is_failed_relay_peer
    BitTorrentPeer::is_hole_punching_failed
    BitTorrentPeer::is_holepunch_accomplishable
    BitTorrentPeer::is_holepunch_supported
    BitTorrentPeer::is_holepunch_unsupported
    BitTorrentPeer::is_incoming_connection
    BitTorrentPeer::is_TCP_connection
    BitTorrentPeer::is_UDP_hole_punching
    BitTorrentPeer::is_uTP_connection
    BitTorrentPeer::is_using_uTP

    peer_active_t::on_connect_failed
    peer_active_t 结构 (peer 状态记录)

设计核心 (6 状态):
    NEW ──> CONNECTING ──> CONNECTED ──> DEAD
     │         │              │           ▲
     │         ▼              ▼           │
     └──> SEEN <──── BANNED <─┘ (timeout) │
                ▲                          │
                └────── IP filter ─────────┘

状态转换:
    NEW → CONNECTING       (peer_add_for_connect)
    CONNECTING → CONNECTED (protocol_handshake_passed)
    CONNECTING → DEAD     (protocol_outgoing_failed / on_connect_failed)
    CONNECTING → BANNED   (peer_ban_waiting_by_ipfilter)
    CONNECTED → DEAD      (on_disconnected)
    CONNECTED → BANNED    (peer_ban)
    ANY → SEEN            (peer_remove_and_put_into SEEN)
    SEEN → CONNECTING     (peer_auto_connect)
    BANNED → CONNECTING   (peer_unban 后回 SEEN)
    DEAD → (auto-remove)  (peer_remove_dead_auto)

加速价值 (针对 qBittorrent):
- qBittorrent 用 libtorrent 内置 peer list, 状态隐含 (active/queued/banned)
- BitComet 6 状态显式管理, 让上层可定制:
  a) BANNED 状态保留 ban 计时器 (到期自动 unban)
  b) SEEN 状态避免重复连接尝试
  c) peer_remove_dead_auto 自动清理, 避免列表膨胀

本模块实现:
- PeerState: 6 状态枚举
- PeerLifecycleStateMachine: 状态机管理器
- PeerRecord: 单个 peer 完整记录
- PeerReaper: 死 peer 自动清理

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import logging
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Dict, List, Optional, Set, Tuple

LOG = logging.getLogger("peer_fsm")


# -----------------------------------------------------------------------------
# 状态枚举
# -----------------------------------------------------------------------------

class PeerState(IntEnum):
    """对应 peer_state_enum + peer_put_into_* 系列."""
    NEW = 0           # peer_put_into_new: 刚发现, 未尝试连接
    CONNECTING = 1     # peer_put_into_connecting: 正在 TCP/uTP 握手
    CONNECTED = 2      # peer_put_into_connected: 握手通过, 可通信
    DEAD = 3           # peer_put_into_dead: 失败, 等待清理
    BANNED = 4         # peer_put_into_banned: 被 ban, 等到期 unban
    SEEN = 5           # peer_put_into_seen: 见过但当前不连


class PeerProtocolVersion(IntEnum):
    """对应 BitTorrentPeer 构造函数的 PeerProtocolVersion 参数."""
    V1 = 1                # BT v1 (info_hash)
    V2 = 2                # BT v2 (info_hash_v2)
    V1_V2_HYBRID = 3      # v1+v2 混合


class PeerTransportType(IntEnum):
    """对应 BitTorrentPeer::is_TCP_connection / is_uTP_connection / is_UDP_hole_punching."""
    TCP_DIRECT = 0          # 直连 TCP
    UTP_DIRECT = 1           # 直连 uTP
    UTP_HOLEPUNCH = 2       # uTP + NAT 打洞
    RELAY = 3                # WebSocket repeater 中继


# -----------------------------------------------------------------------------
# PeerRecord
# -----------------------------------------------------------------------------

@dataclass
class PeerRecord:
    """对应 peer_active_t + PeerBase 状态."""
    endpoint: Tuple[str, int]
    peer_id: Optional[bytes] = None
    state: PeerState = PeerState.NEW
    # 协议版本
    protocol_version: PeerProtocolVersion = PeerProtocolVersion.V1_V2_HYBRID
    # 传输类型
    transport: PeerTransportType = PeerTransportType.TCP_DIRECT
    # 时间戳
    first_seen: float = field(default_factory=time.time)
    last_seen: float = field(default_factory=time.time)
    last_connect_attempt: float = 0.0
    last_state_change: float = field(default_factory=time.time)
    # 统计
    connect_attempts: int = 0
    connect_failures: int = 0
    bytes_uploaded: int = 0
    bytes_downloaded: int = 0
    # ban 信息
    ban_reason: Optional[str] = None
    ban_until: float = 0.0
    # uTP 信息
    is_utp_supported: bool = True
    is_utp_unsupported: bool = False
    is_holepunch_supported: bool = False
    is_holepunch_accomplishable: bool = False
    # BitComet passport 认证状态
    is_bitcomet_auth_passed: bool = False
    # relay peer
    is_failed_relay_peer: bool = False
    # 上次错误
    last_error: Optional[str] = None

    @property
    def is_banned(self) -> bool:
        return (self.state == PeerState.BANNED and
                time.time() < self.ban_until)


# -----------------------------------------------------------------------------
# PeerLifecycleStateMachine — 主状态机
# -----------------------------------------------------------------------------

class PeerLifecycleStateMachine:
    """对应 PeerPoolBase 完整状态机."""

    def __init__(self,
                 max_peers_total: int = 200,
                 max_connecting: int = 50,
                 max_connected: int = 100,
                 ban_duration_sec: float = 3600.0,
                 dead_auto_remove_sec: float = 600.0,
                 seen_reconnect_interval_sec: float = 300.0):
        self.max_total = max_peers_total
        self.max_connecting = max_connecting
        self.max_connected = max_connected
        self.ban_duration = ban_duration_sec
        self.dead_remove_after = dead_auto_remove_sec
        self.seen_reconnect_after = seen_reconnect_interval_sec
        # endpoint → PeerRecord
        self._peers: Dict[Tuple[str, int], PeerRecord] = {}
        # 按状态分桶 (用于快速查询)
        self._state_buckets: Dict[PeerState, Set[Tuple[str, int]]] = {
            s: set() for s in PeerState
        }
        # IP filter ban 等待队列 (host → ban 时间)
        self._ipfilter_bans: Dict[str, float] = {}
        # 统计
        self.stats = {
            "transitions": 0,
            "connect_attempts": 0,
            "connect_success": 0,
            "connect_failures": 0,
            "auto_removed_dead": 0,
            "bans_issued": 0,
            "bans_expired": 0,
            "unbans": 0,
        }

    # ----- 公开 API: 添加 peer -----

    def peer_add(self, endpoint: Tuple[str, int],
                  peer_id: Optional[bytes] = None) -> PeerRecord:
        """对应 PeerPoolBase::peer_add."""
        if endpoint in self._peers:
            rec = self._peers[endpoint]
            rec.last_seen = time.time()
            return rec
        # 检查 IP filter ban
        host = endpoint[0]
        if host in self._ipfilter_bans:
            if time.time() < self._ipfilter_bans[host]:
                # 创建 BANNED 状态的 record
                rec = PeerRecord(
                    endpoint=endpoint, peer_id=peer_id,
                    state=PeerState.BANNED,
                    ban_reason="ipfilter",
                    ban_until=self._ipfilter_bans[host],
                )
                self._peers[endpoint] = rec
                self._state_buckets[PeerState.BANNED].add(endpoint)
                return rec
            else:
                del self._ipfilter_bans[host]
        # 新 peer
        rec = PeerRecord(endpoint=endpoint, peer_id=peer_id, state=PeerState.NEW)
        self._peers[endpoint] = rec
        self._state_buckets[PeerState.NEW].add(endpoint)
        return rec

    def peer_add_for_connect(self, endpoint: Tuple[str, int]) -> PeerRecord:
        """对应 peer_add_for_connect - 直接进 CONNECTING."""
        rec = self.peer_add(endpoint)
        self._transition(rec, PeerState.CONNECTING)
        rec.connect_attempts += 1
        rec.last_connect_attempt = time.time()
        self.stats["connect_attempts"] += 1
        return rec

    def peer_create_from_incoming(self, endpoint: Tuple[str, int],
                                    peer_id: bytes) -> PeerRecord:
        """对应 peer_create_from_incoming - 入站 peer 直接 CONNECTED."""
        rec = self.peer_add(endpoint, peer_id)
        self._transition(rec, PeerState.CONNECTED)
        rec.is_bitcomet_auth_passed = True  # 简化: 假设已通过
        return rec

    # ----- 公开 API: 状态转换 -----

    def protocol_outgoing_connecting_started(self, endpoint: Tuple[str, int]) -> None:
        """对应 protocol_outgoing_connecting_started."""
        rec = self._peers.get(endpoint)
        if rec:
            self._transition(rec, PeerState.CONNECTING)
            rec.last_connect_attempt = time.time()

    def protocol_outgoing_connected(self, endpoint: Tuple[str, int]) -> None:
        """对应 protocol_outgoing_connected - TCP 连接建立."""
        rec = self._peers.get(endpoint)
        if rec and rec.state == PeerState.CONNECTING:
            # 仍需握手通过才进 CONNECTED
            pass

    def protocol_outgoing_failed(self, endpoint: Tuple[str, int],
                                   error: Optional[str] = None) -> None:
        """对应 protocol_outgoing_failed."""
        rec = self._peers.get(endpoint)
        if rec:
            rec.connect_failures += 1
            rec.last_error = error
            self.stats["connect_failures"] += 1
            self._transition(rec, PeerState.DEAD)
            rec.last_state_change = time.time()

    def protocol_handshake_passed(self, endpoint: Tuple[str, int]) -> None:
        """对应 protocol_handshake_passed - 握手成功, 进 CONNECTED."""
        rec = self._peers.get(endpoint)
        if rec and rec.state == PeerState.CONNECTING:
            self._transition(rec, PeerState.CONNECTED)
            self.stats["connect_success"] += 1

    def on_disconnected(self, endpoint: Tuple[str, int],
                         reason: Optional[str] = None) -> None:
        """对应 BitTorrentPeer::on_disconnected."""
        rec = self._peers.get(endpoint)
        if rec:
            if rec.state == PeerState.CONNECTED:
                self._transition(rec, PeerState.DEAD)
                rec.last_error = reason

    def peer_ban(self, endpoint: Tuple[str, int],
                  reason: str = "manual",
                  duration: Optional[float] = None) -> None:
        """对应 peer_ban."""
        rec = self._peers.get(endpoint)
        if not rec:
            rec = self.peer_add(endpoint)
        duration = duration or self.ban_duration
        rec.ban_reason = reason
        rec.ban_until = time.time() + duration
        self._transition(rec, PeerState.BANNED)
        self.stats["bans_issued"] += 1
        LOG.info("banned peer %s for %s (reason=%s)",
                 endpoint, reason, int(duration))

    def peer_ban_waiting_by_ipfilter(self, host: str,
                                       duration: Optional[float] = None) -> None:
        """对应 peer_ban_waiting_by_ipfilter - 把整个 host 的 peer 全部 ban."""
        duration = duration or self.ban_duration
        self._ipfilter_bans[host] = time.time() + duration
        # 找所有该 host 的 peer, 转 BANNED
        for ep, rec in list(self._peers.items()):
            if ep[0] == host and rec.state != PeerState.BANNED:
                rec.ban_reason = "ipfilter"
                rec.ban_until = self._ipfilter_bans[host]
                self._transition(rec, PeerState.BANNED)

    def peer_unban(self, endpoint: Tuple[str, int]) -> None:
        """对应 peer_unban."""
        rec = self._peers.get(endpoint)
        if rec and rec.state == PeerState.BANNED:
            self._transition(rec, PeerState.SEEN)
            self.stats["unbans"] += 1

    def peer_unban_all(self) -> None:
        """对应 peer_unban_all."""
        for ep in list(self._state_buckets[PeerState.BANNED]):
            self.peer_unban(ep)

    def peer_remove(self, endpoint: Tuple[str, int]) -> None:
        """对应 peer_remove."""
        rec = self._peers.pop(endpoint, None)
        if rec:
            self._state_buckets[rec.state].discard(endpoint)

    def peer_remove_and_put_into(self, endpoint: Tuple[str, int],
                                  new_state: PeerState) -> None:
        """对应 peer_remove_and_put_into - 状态转换的统一入口."""
        rec = self._peers.get(endpoint)
        if rec:
            self._transition(rec, new_state)

    def peer_disconnect(self, endpoint: Tuple[str, int]) -> None:
        """对应 peer_disconnect - 主动断开."""
        rec = self._peers.get(endpoint)
        if rec and rec.state == PeerState.CONNECTED:
            self._transition(rec, PeerState.DEAD)

    def peer_disconnect_all(self) -> None:
        """对应 peer_disconnect_all."""
        for ep in list(self._state_buckets[PeerState.CONNECTED]):
            self.peer_disconnect(ep)

    def peer_abort_connecting(self, endpoint: Tuple[str, int]) -> None:
        """对应 peer_abort_connecting - 中断 CONNECTING."""
        rec = self._peers.get(endpoint)
        if rec and rec.state == PeerState.CONNECTING:
            self._transition(rec, PeerState.DEAD)

    # ----- 公开 API: 自动清理 -----

    def peer_remove_dead_auto(self) -> int:
        """对应 peer_remove_dead_auto - 清理超时的 DEAD peer."""
        now = time.time()
        removed = 0
        for ep in list(self._state_buckets[PeerState.DEAD]):
            rec = self._peers.get(ep)
            if rec and now - rec.last_state_change > self.dead_remove_after:
                self.peer_remove(ep)
                removed += 1
                self.stats["auto_removed_dead"] += 1
        # 同时检查 ban 到期
        for ep in list(self._state_buckets[PeerState.BANNED]):
            rec = self._peers.get(ep)
            if rec and now > rec.ban_until:
                self.peer_unban(ep)
                self.stats["bans_expired"] += 1
        if removed > 0:
            LOG.debug("auto-removed %d dead peers", removed)
        return removed

    # ----- 公开 API: 查询 -----

    def num_peers(self) -> int:
        """对应 PeerPoolBase::num_peers."""
        return len(self._peers)

    def get_state_count(self, state: PeerState) -> int:
        return len(self._state_buckets[state])

    def get_connected_peers(self) -> List[PeerRecord]:
        return [self._peers[ep] for ep in self._state_buckets[PeerState.CONNECTED]]

    def get_connecting_peers(self) -> List[PeerRecord]:
        return [self._peers[ep] for ep in self._state_buckets[PeerState.CONNECTING]]

    def get_new_peers_for_connect(self, limit: int = 10) -> List[PeerRecord]:
        """获取 NEW 状态 peer 供 peer_auto_connect 使用."""
        new_peers = list(self._state_buckets[PeerState.NEW])
        return [self._peers[ep] for ep in new_peers[:limit]]

    def get_seen_peers_for_reconnect(self, limit: int = 10) -> List[PeerRecord]:
        """获取 SEEN 状态 peer (超过 reconnect interval) 供重连."""
        now = time.time()
        result = []
        for ep in self._state_buckets[PeerState.SEEN]:
            rec = self._peers.get(ep)
            if rec and now - rec.last_seen > self.seen_reconnect_after:
                result.append(rec)
                if len(result) >= limit:
                    break
        return result

    def is_peer_valid(self, endpoint: Tuple[str, int]) -> bool:
        """对应 peer_is_valid."""
        rec = self._peers.get(endpoint)
        return rec is not None and rec.state not in (PeerState.DEAD, PeerState.BANNED)

    def save(self) -> Dict:
        """对应 PeerPoolBase::save - 序列化状态."""
        return {
            "peers": [
                {
                    "endpoint": list(ep),
                    "state": int(rec.state),
                    "peer_id": rec.peer_id.hex() if rec.peer_id else None,
                    "last_seen": rec.last_seen,
                }
                for ep, rec in self._peers.items()
            ],
            "ipfilter_bans": {h: t for h, t in self._ipfilter_bans.items()},
        }

    def load(self, data: Dict) -> None:
        """对应 PeerPoolBase::load."""
        for p in data.get("peers", []):
            ep = tuple(p["endpoint"])
            rec = self.peer_add(ep)
            rec.state = PeerState(p["state"])
            rec.last_seen = p.get("last_seen", time.time())
        self._ipfilter_bans = data.get("ipfilter_bans", {})

    def get_stats(self) -> Dict:
        s = dict(self.stats)
        s["state_counts"] = {st.name: len(self._state_buckets[st]) for st in PeerState}
        s["ipfilter_bans"] = len(self._ipfilter_bans)
        return s

    # ----- 内部 -----

    def _transition(self, rec: PeerRecord, new_state: PeerState) -> None:
        if rec.state == new_state:
            return
        old = rec.state
        self._state_buckets[old].discard(rec.endpoint)
        self._state_buckets[new_state].add(rec.endpoint)
        rec.state = new_state
        rec.last_state_change = time.time()
        self.stats["transitions"] += 1
        LOG.debug("peer %s: %s → %s", rec.endpoint, old.name, new_state.name)


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    logging.basicConfig(level=logging.INFO,
                        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s")
    print("=" * 60)
    print("BitComet Peer 6-state lifecycle demo")
    print("=" * 60)
    fsm = PeerLifecycleStateMachine(max_peers_total=100)

    # 模拟 10 个 peer 加入
    for i in range(10):
        ep = (f"10.0.0.{i+1}", 6881+i)
        fsm.peer_add(ep)

    # 尝试连接 5 个
    for i in range(5):
        ep = (f"10.0.0.{i+1}", 6881+i)
        fsm.peer_add_for_connect(ep)

    # 3 个握手成功
    for i in range(3):
        ep = (f"10.0.0.{i+1}", 6881+i)
        fsm.protocol_handshake_passed(ep)

    # 2 个连接失败
    for i in range(3, 5):
        ep = (f"10.0.0.{i+1}", 6881+i)
        fsm.protocol_outgoing_failed(ep, "timeout")

    # 1 个被 ban
    fsm.peer_ban(("10.0.0.1", 6881), reason="leech")
    # 整个 host ban
    fsm.peer_ban_waiting_by_ipfilter("10.0.0.9", duration=3600)

    print("\n=== State counts ===")
    for st in PeerState:
        print(f"  {st.name:12s}: {fsm.get_state_count(st)}")
    print(f"\n=== Stats ===")
    for k, v in fsm.get_stats().items():
        if isinstance(v, dict):
            print(f"  {k}:")
            for k2, v2 in v.items():
                print(f"    {k2}: {v2}")
        else:
            print(f"  {k}: {v}")

    # unban
    print("\n[unban peer 1]")
    fsm.peer_unban(("10.0.0.1", 6881))
    print(f"  state: {fsm._peers[('10.0.0.1', 6881)].state.name}")
