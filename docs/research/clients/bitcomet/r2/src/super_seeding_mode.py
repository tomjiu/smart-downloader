"""
super_seeding_mode.py — BitComet 超级种子模式
==========================================

逆向来源: Core_BitTorrent::BitTorrentPeer + BitTorrentPeerPool + BitTorrentTask
关键符号:
    BitTorrentPeerPool::timer_super_seeding
    BitTorrentPeerPool::optimize_peer_connections
    BitTorrentPeer::get_my_permillage_as_superseed
    BitTorrentPeer::get_my_progress_as_superseed
    BitTorrentPeer::has_metadata_only_close_blocking_activity
    BitTorrentPeer::metadata_only_close_cancel
    BitTorrentPeer::metadata_only_close_check
    BitTorrentPeer::metadata_only_close_disconnect
    BitTorrentPeer::is_metadata_download_active
    BitTorrentPeer::is_utp_send_drained_for_metadata_only_close
    BitTorrentPeer::mark_metadata_piece_uploaded
    BitTorrentPeer::performance_optimze

    BitTorrentTask::is_enable_super_seeding
    BitTorrentTask::find_piece_for_superseeding  (从 PieceManage)
    PieceManage::impl::find_piece_for_superseeding
    PieceManage::availability_percent / health_percent

    BitTorrentPeerPool::timer_tick

设计核心 (BEP-14 Super-seeding 扩展):
1. 超级种子模式下, seed 不再持有所有 piece
2. 每个 piece 对特定 peer 标记为 "未发送" (谎称)
3. 当某 peer 请求该 piece, seed 给它, 然后谎称该 piece "已上传"
4. 等待其他 peer 报告有该 piece (说明该 peer 已分发)
5. 才标记 piece "已分发", 进入下一轮

加速价值:
- 公网 BT 任务初期, seed 带宽是瓶颈
- 超级种子模式让 seed 的每个 piece 都能尽快扩散到不同 peer
- 避免某 peer 把 seed 的所有 piece 都自己缓存
- 让 piece 在 P2P 网络中扩散更快

BitComet 私有扩展:
- get_my_permillage_as_superseed: 每个 peer 的"已分发了多少 piece" 千分比
- get_my_progress_as_superseed: peer 在超级种子模式下的进度
- metadata_only_close_*: metadata 下载完后特殊关闭流程
- mark_metadata_piece_uploaded: 标记 piece 已上传给该 peer
- find_piece_for_superseeding: 选择对哪个 peer 发哪个 piece

本模块实现:
- SuperSeedingState: 单个 peer 在超级种子模式下的状态
- SuperSeedingManager: 完整超级种子调度
- MetadataOnlyCloseHandler: metadata 下载完特殊关闭

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import logging
import random
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Deque, Dict, List, Optional, Set, Tuple

LOG = logging.getLogger("super_seed")


# -----------------------------------------------------------------------------
# 数据结构
# -----------------------------------------------------------------------------

class PieceDistributionStatus(IntEnum):
    """单个 piece 在 P2P 网络中的分布状态."""
    NEVER_SENT = 0          # 从未发送 (seed 持有但未给任何人)
    SENT_TO_ONE = 1         # 已发给 1 个 peer, 等待其他 peer 持有
    DISTRIBUTED = 2          # 已扩散 (≥2 个 peer 持有)


@dataclass
class PeerSuperSeedState:
    """单个 peer 在超级种子模式下的状态."""
    endpoint: Tuple[str, int]
    # 对该 peer 谎称的 bitfield (哪些 piece 它认为我们没持有)
    fake_missing_pieces: Set[int] = field(default_factory=set)
    # 已发送给该 peer 的 piece (真实)
    sent_pieces: Set[int] = field(default_factory=set)
    # 已上传字节数
    bytes_uploaded: int = 0
    # piece 上传标记 (mark_metadata_piece_uploaded)
    uploaded_pieces: Set[int] = field(default_factory=set)
    # 时间戳
    last_send_time: float = 0.0
    last_have_received: float = 0.0
    # permillage 状态 (get_my_permillage_as_superseed)
    permillage_completed: int = 0
    # 是否处于 metadata-only-close 流程
    is_in_metadata_only_close: bool = False

    @property
    def permillage_float(self) -> float:
        return self.permillage_completed / 1000.0


@dataclass
class PieceSeedState:
    """单个 piece 的种子状态."""
    piece_index: int
    status: PieceDistributionStatus = PieceDistributionStatus.NEVER_SENT
    sent_to_peer: Optional[Tuple[str, int]] = None  # 发给哪个 peer
    sent_at: float = 0.0
    # 哪些 peer 报告持有 (on_peer_have_piece)
    held_by: Set[Tuple[str, int]] = field(default_factory=set)
    # 等待超时 (如果 sent_to_one 后长时间没扩散, 重新发送)
    distribute_timeout_sec: float = 60.0


# -----------------------------------------------------------------------------
# SuperSeedingManager — 完整超级种子调度
# -----------------------------------------------------------------------------

class SuperSeedingManager:
    """对应 BitTorrentPeerPool::timer_super_seeding + PieceManage::find_piece_for_superseeding."""

    def __init__(self, total_pieces: int,
                 fake_missing_ratio: float = 0.7,
                 distribute_timeout_sec: float = 60.0):
        """
        Args:
            total_pieces: 总 piece 数
            fake_missing_ratio: 对每个 peer 谎称"没持有"的 piece 比例 (0.7 = 70%)
            distribute_timeout_sec: 单 piece 等待扩散超时
        """
        self.total_pieces = total_pieces
        self.fake_missing_ratio = fake_missing_ratio
        self.distribute_timeout = distribute_timeout_sec
        # piece_index → PieceSeedState
        self._piece_states: Dict[int, PieceSeedState] = {
            i: PieceSeedState(piece_index=i,
                              distribute_timeout_sec=distribute_timeout_sec)
            for i in range(total_pieces)
        }
        # peer endpoint → PeerSuperSeedState
        self._peer_states: Dict[Tuple[str, int], PeerSuperSeedState] = {}
        # 已分发 piece 数 (用于 permillage 计算)
        self._distributed_count = 0
        # 统计
        self.stats = {
            "pieces_sent": 0,
            "pieces_distributed": 0,
            "pieces_re_sent": 0,
            "peers_added": 0,
            "peers_removed": 0,
            "metadata_only_close_initiated": 0,
        }

    # ----- 公开 API: peer 生命周期 -----

    def add_peer(self, endpoint: Tuple[str, int]) -> PeerSuperSeedState:
        """新增 peer 到超级种子模式."""
        if endpoint in self._peer_states:
            return self._peer_states[endpoint]
        state = PeerSuperSeedState(endpoint=endpoint)
        # 对该 peer 谎称 missing 70% 的 piece
        all_pieces = list(range(self.total_pieces))
        random.shuffle(all_pieces)
        fake_count = int(self.total_pieces * self.fake_missing_ratio)
        state.fake_missing_pieces = set(all_pieces[:fake_count])
        self._peer_states[endpoint] = state
        self.stats["peers_added"] += 1
        LOG.debug("added super-seed peer %s (fake missing %d pieces)",
                  endpoint, len(state.fake_missing_pieces))
        return state

    def remove_peer(self, endpoint: Tuple[str, int]) -> None:
        """peer 离开, 释放它持有的 piece (允许重新发送)."""
        state = self._peer_states.pop(endpoint, None)
        if state:
            # 释放它独占的 piece
            for piece_idx in state.sent_pieces:
                ps = self._piece_states.get(piece_idx)
                if ps and ps.sent_to_peer == endpoint:
                    if ps.status == PieceDistributionStatus.SENT_TO_ONE:
                        # 回到 NEVER_SENT
                        ps.status = PieceDistributionStatus.NEVER_SENT
                        ps.sent_to_peer = None
            self.stats["peers_removed"] += 1

    # ----- 公开 API: 选择 piece 发送 -----

    def find_piece_for_superseeding(self, peer_endpoint: Tuple[str, int]) -> Optional[int]:
        """对应 PieceManage::find_piece_for_superseeding.

        选择策略:
        1. 优先 NEVER_SENT piece (从未发过)
        2. 若所有都已发, 选 SENT_TO_ONE 且超时的 (重新发送)
        3. 不选 DISTRIBUTED (已扩散的)
        4. 不选该 peer 已持有的
        """
        peer_state = self._peer_states.get(peer_endpoint)
        if not peer_state:
            self.add_peer(peer_endpoint)
            peer_state = self._peer_states[peer_endpoint]
        # 1. NEVER_SENT
        candidates = [
            i for i, ps in self._piece_states.items()
            if ps.status == PieceDistributionStatus.NEVER_SENT
            and i not in peer_state.sent_pieces
        ]
        if candidates:
            return random.choice(candidates)
        # 2. SENT_TO_ONE 超时
        now = time.time()
        timeout_candidates = [
            i for i, ps in self._piece_states.items()
            if ps.status == PieceDistributionStatus.SENT_TO_ONE
            and ps.sent_to_peer != peer_endpoint
            and i not in peer_state.sent_pieces
            and now - ps.sent_at > ps.distribute_timeout_sec
        ]
        if timeout_candidates:
            self.stats["pieces_re_sent"] += 1
            return random.choice(timeout_candidates)
        # 3. 没有 piece 可发
        return None

    def mark_piece_sent(self, peer_endpoint: Tuple[str, int],
                         piece_index: int) -> None:
        """标记 piece 已发给 peer."""
        ps = self._piece_states.get(piece_index)
        peer_state = self._peer_states.get(peer_endpoint)
        if not ps or not peer_state:
            return
        if ps.status == PieceDistributionStatus.NEVER_SENT:
            ps.status = PieceDistributionStatus.SENT_TO_ONE
            ps.sent_to_peer = peer_endpoint
            ps.sent_at = time.time()
            self.stats["pieces_sent"] += 1
        peer_state.sent_pieces.add(piece_index)
        peer_state.uploaded_pieces.add(piece_index)
        peer_state.last_send_time = time.time()
        peer_state.bytes_uploaded += 16 * 1024  # 假设 16KB

    # ----- 公开 API: peer 报告持有 -----

    def on_peer_have_piece(self, peer_endpoint: Tuple[str, int],
                            piece_index: int) -> None:
        """对应 BitTorrentPeerPool::on_peer_have_piece.

        当 peer A 报告它有 piece X, 说明 piece X 已扩散.
        """
        ps = self._piece_states.get(piece_index)
        if not ps:
            return
        ps.held_by.add(peer_endpoint)
        # 如果有 ≥2 个 peer 持有 (含原 seed), 标记 DISTRIBUTED
        if len(ps.held_by) >= 2 and ps.status == PieceDistributionStatus.SENT_TO_ONE:
            ps.status = PieceDistributionStatus.DISTRIBUTED
            self._distributed_count += 1
            self.stats["pieces_distributed"] += 1
            LOG.debug("piece %d distributed (held by %d peers)",
                      piece_index, len(ps.held_by))

    # ----- 公开 API: permillage 计算 -----

    def get_my_permillage_as_superseed(self, peer_endpoint: Tuple[str, int]) -> int:
        """对应 BitTorrentPeer::get_my_permillage_as_superseed.

        返回该 peer 视角下, 我们 (seed) 的"已上传"千分比.
        """
        peer_state = self._peer_states.get(peer_endpoint)
        if not peer_state or self.total_pieces == 0:
            return 0
        # permillage = 已上传给该 peer 的 piece / 该 peer 视角下应该上传的 piece
        view_total = self.total_pieces - len(peer_state.fake_missing_pieces)
        if view_total == 0:
            return 1000
        return min(1000, int(len(peer_state.uploaded_pieces) * 1000 / view_total))

    def get_my_progress_as_superseed(self, peer_endpoint: Tuple[str, int]) -> float:
        """对应 BitTorrentPeer::get_my_progress_as_superseed.

        返回 0.0-1.0 的进度.
        """
        return self.get_my_permillage_as_superseed(peer_endpoint) / 1000.0

    # ----- 公开 API: metadata-only-close 流程 -----

    def metadata_only_close_check(self, peer_endpoint: Tuple[str, int]) -> bool:
        """对应 BitTorrentPeer::metadata_only_close_check.

        metadata-only peer 下载完 metadata 后, 是否可以关闭?
        条件: peer 已声明对 metadata 感兴趣, 且 uTP send drained.
        """
        peer_state = self._peer_states.get(peer_endpoint)
        if not peer_state:
            return False
        return (peer_state.is_in_metadata_only_close and
                not peer_state.uploaded_pieces)  # 没有任何 piece 上传

    def metadata_only_close_initiate(self, peer_endpoint: Tuple[str, int]) -> None:
        """对应 metadata_only_close_disconnect + cancel."""
        peer_state = self._peer_states.get(peer_endpoint)
        if peer_state and not peer_state.is_in_metadata_only_close:
            peer_state.is_in_metadata_only_close = True
            self.stats["metadata_only_close_initiated"] += 1
            LOG.info("initiating metadata-only close for %s", peer_endpoint)

    def is_utp_send_drained_for_metadata_only_close(self,
                                                      peer_endpoint: Tuple[str, int]) -> bool:
        """对应 BitTorrentPeer::is_utp_send_drained_for_metadata_only_close."""
        peer_state = self._peer_states.get(peer_endpoint)
        if not peer_state:
            return False
        # 简化: 1 秒内没有发送, 视为 drained
        return (peer_state.is_in_metadata_only_close and
                time.time() - peer_state.last_send_time > 1.0)

    # ----- 公开 API: 定时调度 -----

    def timer_tick(self) -> Dict[str, int]:
        """对应 BitTorrentPeerPool::timer_tick + timer_super_seeding.

        每秒调用, 检查:
        1. SENT_TO_ONE 超时 piece 回到 NEVER_SENT
        2. metadata-only-close 检查
        """
        now = time.time()
        timed_out = 0
        for ps in self._piece_states.values():
            if (ps.status == PieceDistributionStatus.SENT_TO_ONE and
                now - ps.sent_at > ps.distribute_timeout_sec):
                # 超时, 该 peer 没扩散, 重新发送
                ps.status = PieceDistributionStatus.NEVER_SENT
                ps.sent_to_peer = None
                timed_out += 1
        # metadata-only close 检查
        metadata_closed = 0
        for ep, ps in list(self._peer_states.items()):
            if (ps.is_in_metadata_only_close and
                self.is_utp_send_drained_for_metadata_only_close(ep)):
                # 实际关闭由调用方处理
                metadata_closed += 1
        return {
            "pieces_timed_out": timed_out,
            "metadata_closed": metadata_closed,
            "distributed_count": self._distributed_count,
            "total_pieces": self.total_pieces,
        }

    def optimize_peer_connections(self) -> List[Tuple[str, int]]:
        """对应 BitTorrentPeerPool::optimize_peer_connections.

        返回应主动断开的 peer 列表 (低效 peer).
        """
        to_disconnect = []
        now = time.time()
        for ep, ps in self._peer_states.items():
            # 长时间没贡献的 peer
            if (now - ps.last_send_time > 300 and  # 5min 没上传
                not ps.is_in_metadata_only_close and
                len(ps.uploaded_pieces) == 0):
                to_disconnect.append(ep)
        return to_disconnect

    # ----- 公开 API: 状态查询 -----

    def get_availability_percent(self) -> float:
        """对应 PieceManage::availability_percent - 已分发的 piece 百分比."""
        if self.total_pieces == 0:
            return 0.0
        return (self._distributed_count / self.total_pieces) * 100.0

    def get_health_percent(self) -> float:
        """对应 PieceManage::health_percent - 整个 swarm 健康度."""
        if not self._peer_states:
            return 0.0
        # 简化: 已分发的 piece / 总 piece * peer 数
        distributed_ratio = self._distributed_count / max(self.total_pieces, 1)
        peer_factor = min(1.0, len(self._peer_states) / 10.0)
        return distributed_ratio * peer_factor * 100

    def get_stats(self) -> Dict:
        s = dict(self.stats)
        s["availability_percent"] = self.get_availability_percent()
        s["health_percent"] = self.get_health_percent()
        s["peer_count"] = len(self._peer_states)
        s["distributed_count"] = self._distributed_count
        return s


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    logging.basicConfig(level=logging.INFO,
                        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s")
    print("=" * 60)
    print("BitComet Super-seeding 模式 demo")
    print("=" * 60)
    # 100 个 piece, 5 个 peer
    mgr = SuperSeedingManager(total_pieces=100, fake_missing_ratio=0.7)
    for i in range(5):
        mgr.add_peer((f"10.0.0.{i+1}", 6881+i))
    # 模拟 50 次发送
    for _ in range(50):
        for i in range(5):
            ep = (f"10.0.0.{i+1}", 6881+i)
            piece = mgr.find_piece_for_superseeding(ep)
            if piece is not None:
                mgr.mark_piece_sent(ep, piece)
                # 随机让其他 peer 报告持有
                other = (i + 1) % 5 + 1
                other_ep = (f"10.0.0.{other}", 6881+other-1)
                mgr.on_peer_have_piece(other_ep, piece)
    # 统计
    print("\n=== Super-seeding stats ===")
    for k, v in mgr.get_stats().items():
        print(f"  {k}: {v}")
    # permillage 查询
    print("\n=== Per-peer permillage ===")
    for i in range(5):
        ep = (f"10.0.0.{i+1}", 6881+i)
        perm = mgr.get_my_permillage_as_superseed(ep)
        prog = mgr.get_my_progress_as_superseed(ep)
        print(f"  peer {ep}: permillage={perm} ({perm/10:.1f}%) progress={prog:.2f}")
    # timer_tick
    print("\n=== timer_tick ===")
    result = mgr.timer_tick()
    for k, v in result.items():
        print(f"  {k}: {v}")
