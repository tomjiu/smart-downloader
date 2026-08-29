"""
piece_request_scheduler.py — BitComet Piece 调度器 (分离模式 + 优先级)
================================================================

逆向来源: Core_BitTorrent::BitTorrentPeer + BitTorrentPeerPool + PieceManage
关键符号:
    BitTorrentPeer::queue_download_add
    BitTorrentPeer::queue_download_cancel
    BitTorrentPeer::queue_download_clear
    BitTorrentPeer::queue_download_existed
    BitTorrentPeer::queue_download_recv
    BitTorrentPeer::queue_download_timeout_check
    BitTorrentPeer::queue_download_valid_check
    BitTorrentPeer::queue_upload_send
    BitTorrentPeer::queue_upload_send2

    BitTorrentPeerPool::broadcast_queue_download_valid_check
    BitTorrentPeerPool::broadcast_queue_upload_send
    BitTorrentPeerPool::on_peer_check_download_request_valid_in_slice_map
    BitTorrentPeerPool::on_peer_load_slice
    BitTorrentPeerPool::on_peer_save_slice
    BitTorrentPeerPool::on_peer_separate_mode_piece_failed
    BitTorrentPeerPool::on_peer_separate_mode_piece_passed
    BitTorrentPeerPool::on_peer_slice_request_new
    BitTorrentPeerPool::on_peer_slice_request_remove
    BitTorrentPeerPool::on_p2sp_piece_request_new

    BitTorrentTask::on_separate_downloaded_piece_failed
    BitTorrentTask::on_separate_downloaded_piece_passed
    BitTorrentTask::on_separate_downloaded_piece_start
    BitTorrentTask::on_p2sp_file_no_new_request
    BitTorrentTask::get_file_index_for_sequential_download
    BitTorrentTask::is_piece_hash_ready_for_file

    PieceManage::find_piece_for_superseeding
    PieceManage::get_file_index_for_sequential_download
    PieceManage::set_file_priority
    PieceManage::impl::overlapped_piece_priority
    PieceManage::impl::check_pending_read_finish
    PieceManage::aligned_slice_t
    PieceManage::availability_percent
    PieceManage::health_percent

    BitTorrentPeer::on_p2sp_emule_cancel_all_other_peers
    BitTorrentPeer::on_p2sp_emule_piece_downloaded
    BitTorrentPeer::on_p2sp_emule_piece_request_remove

设计核心:
1. BitComet 实现 "分离模式" piece 调度:
   - 普通 BT peer 走 piece_request 标准流程
   - P2SP/HTTP/eMule source 走 "separate" 流程
   - 两路 piece 互不冲突 (避免重复下载)
2. aligned_slice_t: piece 对齐切片 (16KiB 标准块)
3. overlapped_piece_priority: piece 跨多文件时的优先级合并
4. sequential_download: 顺序下载 (流媒体预览)
5. broadcast_queue_download_valid_check: 批量校验 piece 请求有效性

加速价值 (针对 qBittorrent):
- qBittorrent 用 libtorrent 内置 piece picker (rarest-first)
- BitComet 的"分离模式"允许:
  a) HTTP/FTP 源下载 piece A, BT peer 同时下载 piece B (不重叠)
  b) 视频文件顺序下载 (前几 piece 优先)
  c) 多文件任务的 file priority 实时调整

本模块实现:
- PieceScheduler: 完整 piece 调度器 (含分离模式)
- SliceRequestQueue: sub-piece 请求队列
- FilePriorityManager: 多文件优先级管理
- SeparateModePieceTracker: 分离模式 piece 跟踪

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import logging
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Deque, Dict, List, Optional, Set, Tuple

LOG = logging.getLogger("piece_sched")


# -----------------------------------------------------------------------------
# 常量
# -----------------------------------------------------------------------------

# BEP-3 标准 sub-piece (slice) 大小: 16 KiB
SLICE_SIZE = 16 * 1024
# 默认 piece 超时 (peer 请求 30s 未响应)
PIECE_REQUEST_TIMEOUT = 30.0
# 同 piece 最大并发请求 peer 数 (避免重复)
MAX_PEERS_PER_PIECE = 4


class PieceSource(IntEnum):
    """piece 下载来源."""
    BT_PEER = 0
    HTTP_WEBSEED = 1
    FTP_MIRROR = 2
    LT_SEED = 3
    EMULE = 4
    P2SP = 5


class PieceState(IntEnum):
    """piece 状态."""
    NOT_DOWNLOADED = 0
    DOWNLOADING = 1
    DOWNLOADED = 2      # 已下载, 待校验
    VERIFIED = 3        # 已校验
    FAILED = 4           # 校验失败, 需重下


@dataclass
class SliceRequest:
    """对应 BitTorrentPeer::queue_download_add 的 sub-piece 请求."""
    piece_index: int
    offset: int           # 在 piece 内的偏移 (16KiB 倍数)
    length: int           # slice 长度 (通常 16KiB)
    peer_endpoint: Tuple[str, int]   # 请求哪个 peer
    source: PieceSource = PieceSource.BT_PEER
    requested_at: float = field(default_factory=time.time)
    is_separate_mode: bool = False   # 是否分离模式 (HTTP/FTP/LT-Seed)


@dataclass
class PieceInfo:
    """单个 piece 的状态."""
    index: int
    state: PieceState = PieceState.NOT_DOWNLOADED
    # 哪个 source 在下载 (None = 无人下载)
    downloading_source: Optional[PieceSource] = None
    downloading_peer: Optional[Tuple[str, int]] = None
    # 已下载的 slice (按 offset 索引)
    downloaded_slices: Dict[int, bytes] = field(default_factory=dict)
    # 已发出的请求 (用于超时检测)
    pending_requests: List[SliceRequest] = field(default_factory=list)
    # 失败次数
    failure_count: int = 0
    # 优先级 (0=normal, >0=高优先)
    priority: int = 0
    # 文件归属 (跨文件 piece)
    file_indices: List[int] = field(default_factory=list)


# -----------------------------------------------------------------------------
# SeparateModePieceTracker — 分离模式 piece 跟踪
# -----------------------------------------------------------------------------

class SeparateModePieceTracker:
    """对应 BitTorrentTask::on_separate_downloaded_piece_*.

    分离模式: HTTP/FTP/LT-Seed source 独立下载某些 piece, 不与 BT peer 重叠.
    """

    def __init__(self, total_pieces: int):
        self.total_pieces = total_pieces
        # piece_index → source (分离模式下载中)
        self._separate_downloading: Dict[int, PieceSource] = {}
        # 已分离下载完成的 piece
        self._separate_done: Set[int] = set()
        # 失败的分离 piece (需回退到 BT)
        self._separate_failed: Set[int] = set()
        # 统计
        self.stats = {
            "separate_started": 0,
            "separate_passed": 0,
            "separate_failed": 0,
        }

    def start_separate(self, piece_index: int,
                       source: PieceSource) -> bool:
        """对应 on_separate_downloaded_piece_start."""
        if piece_index in self._separate_downloading:
            return False
        if piece_index in self._separate_done:
            return False
        self._separate_downloading[piece_index] = source
        self.stats["separate_started"] += 1
        return True

    def on_separate_passed(self, piece_index: int) -> None:
        """对应 on_separate_downloaded_piece_passed."""
        self._separate_downloading.pop(piece_index, None)
        self._separate_done.add(piece_index)
        self.stats["separate_passed"] += 1

    def on_separate_failed(self, piece_index: int) -> None:
        """对应 on_separate_downloaded_piece_failed."""
        self._separate_downloading.pop(piece_index, None)
        self._separate_failed.add(piece_index)
        self.stats["separate_failed"] += 1

    def is_separate_downloading(self, piece_index: int) -> bool:
        return piece_index in self._separate_downloading

    def is_separate_done(self, piece_index: int) -> bool:
        return piece_index in self._separate_done

    def is_separate_failed(self, piece_index: int) -> bool:
        return piece_index in self._separate_failed

    def get_separate_pieces(self) -> Set[int]:
        return set(self._separate_downloading.keys())


# -----------------------------------------------------------------------------
# FilePriorityManager — 多文件优先级管理
# -----------------------------------------------------------------------------

class FilePriority:
    """对应 priority_enum."""
    SKIP = 0
    NORMAL = 1
    HIGH = 2
    MAX = 3


class FilePriorityManager:
    """对应 PieceManage::set_file_priority + BitTorrentTask::get_file_index_for_sequential_download."""

    def __init__(self, files: List[Dict]):
        """
        Args:
            files: [{name, size, first_piece, last_piece, piece_count}]
        """
        self.files = files
        # file_index → priority
        self._priorities: Dict[int, int] = {i: FilePriority.NORMAL for i in range(len(files))}
        # 顺序下载游标 (用于 sequential download)
        self._sequential_cursor: Dict[int, int] = {}
        # 统计
        self.stats = {"priority_changes": 0}

    def set_file_priority(self, file_index: int, priority: int) -> None:
        """对应 set_file_priority."""
        if file_index < 0 or file_index >= len(self.files):
            return
        old = self._priorities.get(file_index, FilePriority.NORMAL)
        if old != priority:
            self._priorities[file_index] = priority
            self.stats["priority_changes"] += 1
            LOG.info("file %s priority: %d → %d",
                     self.files[file_index]["name"], old, priority)

    def get_file_priority(self, file_index: int) -> int:
        return self._priorities.get(file_index, FilePriority.NORMAL)

    def get_overlapped_piece_priority(self, piece_index: int) -> int:
        """对应 overlapped_piece_priority - 跨多文件的 piece 取最高优先级."""
        max_prio = FilePriority.NORMAL
        for fi, f in enumerate(self.files):
            if f["first_piece"] <= piece_index <= f["last_piece"]:
                prio = self._priorities.get(fi, FilePriority.NORMAL)
                if prio == FilePriority.SKIP:
                    continue  # skip 文件不下载
                max_prio = max(max_prio, prio)
        return max_prio

    def get_file_index_for_sequential_download(self, piece_index: int) -> Optional[int]:
        """对应 get_file_index_for_sequential_download.

        找出该 piece 属于哪个文件 (用于顺序下载).
        """
        for fi, f in enumerate(self.files):
            if f["first_piece"] <= piece_index <= f["last_piece"]:
                return fi
        return None

    def get_next_sequential_piece(self, file_index: int) -> Optional[int]:
        """获取文件 file_index 的下一个顺序下载 piece."""
        f = self.files[file_index]
        cursor = self._sequential_cursor.get(file_index, f["first_piece"])
        if cursor > f["last_piece"]:
            return None
        return cursor

    def advance_sequential_cursor(self, file_index: int) -> None:
        f = self.files[file_index]
        cursor = self._sequential_cursor.get(file_index, f["first_piece"])
        self._sequential_cursor[file_index] = cursor + 1

    def get_stats(self) -> Dict:
        s = dict(self.stats)
        s["files"] = {f["name"]: self._priorities[i] for i, f in enumerate(self.files)}
        return s


# -----------------------------------------------------------------------------
# SliceRequestQueue — sub-piece 请求队列
# -----------------------------------------------------------------------------

class SliceRequestQueue:
    """对应 BitTorrentPeer::queue_download_add/cancel/recv 系列."""

    def __init__(self, piece_size: int = 16 * 1024 * 16):  # 16 slices per piece
        self.piece_size = piece_size
        # piece_index → SliceRequest list
        self._requests: Dict[int, List[SliceRequest]] = defaultdict(list)
        # 超时
        self.timeout_sec = PIECE_REQUEST_TIMEOUT

    def add(self, piece_index: int, offset: int, length: int,
             peer_endpoint: Tuple[str, int],
             source: PieceSource = PieceSource.BT_PEER,
             is_separate_mode: bool = False) -> SliceRequest:
        """对应 queue_download_add."""
        req = SliceRequest(
            piece_index=piece_index, offset=offset, length=length,
            peer_endpoint=peer_endpoint, source=source,
            is_separate_mode=is_separate_mode,
        )
        self._requests[piece_index].append(req)
        return req

    def cancel(self, piece_index: int, offset: int,
               peer_endpoint: Tuple[str, int]) -> bool:
        """对应 queue_download_cancel."""
        if piece_index not in self._requests:
            return False
        reqs = self._requests[piece_index]
        for i, r in enumerate(reqs):
            if r.offset == offset and r.peer_endpoint == peer_endpoint:
                reqs.pop(i)
                return True
        return False

    def existed(self, piece_index: int, offset: int,
                 peer_endpoint: Tuple[str, int]) -> bool:
        """对应 queue_download_existed."""
        if piece_index not in self._requests:
            return False
        return any(r.offset == offset and r.peer_endpoint == peer_endpoint
                    for r in self._requests[piece_index])

    def recv(self, piece_index: int, offset: int,
              peer_endpoint: Tuple[str, int]) -> Optional[SliceRequest]:
        """对应 queue_download_recv - 收到响应, 移除请求."""
        if piece_index not in self._requests:
            return None
        reqs = self._requests[piece_index]
        for i, r in enumerate(reqs):
            if r.offset == offset and r.peer_endpoint == peer_endpoint:
                return reqs.pop(i)
        return None

    def timeout_check(self) -> List[SliceRequest]:
        """对应 queue_download_timeout_check - 检查超时请求."""
        now = time.time()
        timed_out = []
        for piece_index, reqs in list(self._requests.items()):
            for r in reqs[:]:
                if now - r.requested_at > self.timeout_sec:
                    timed_out.append(r)
                    reqs.remove(r)
        return timed_out

    def valid_check(self, piece_index: int, offset: int) -> bool:
        """对应 broadcast_queue_download_valid_check - 验证请求是否有效."""
        if piece_index not in self._requests:
            return False
        return any(r.offset == offset for r in self._requests[piece_index])

    def clear(self, piece_index: int) -> None:
        """对应 queue_download_clear."""
        self._requests.pop(piece_index, None)

    def size(self) -> int:
        return sum(len(reqs) for reqs in self._requests.values())

    def get_pending_for_peer(self, peer_endpoint: Tuple[str, int]) -> List[SliceRequest]:
        return [r for reqs in self._requests.values() for r in reqs
                if r.peer_endpoint == peer_endpoint]


# -----------------------------------------------------------------------------
# PieceScheduler — 主调度器
# -----------------------------------------------------------------------------

class PieceScheduler:
    """对应 PieceManage + BitTorrentPeerPool 的 piece 调度."""

    def __init__(self, total_pieces: int, piece_size: int = 256 * 1024,
                 files: Optional[List[Dict]] = None):
        self.total_pieces = total_pieces
        self.piece_size = piece_size
        # piece 状态
        self._pieces: Dict[int, PieceInfo] = {
            i: PieceInfo(index=i) for i in range(total_pieces)
        }
        # 分离模式跟踪
        self.separate_tracker = SeparateModePieceTracker(total_pieces)
        # 文件优先级
        self.file_priority = FilePriorityManager(files or [])
        # sub-piece 请求队列
        self.slice_queue = SliceRequestQueue(piece_size)
        # piece 完成度 (用于 availability 计算)
        self._completed_pieces: Set[int] = set()
        # piece 持有 peer 数 (rarest-first)
        self._piece_peer_count: Dict[int, int] = defaultdict(int)
        # 统计
        self.stats = {
            "pieces_downloaded": 0,
            "pieces_verified": 0,
            "pieces_failed": 0,
            "duplicate_slices_ignored": 0,
        }

    # ----- 公开 API: piece 状态 -----

    def is_piece_needed(self, piece_index: int) -> bool:
        """对应 is_download_need."""
        if piece_index in self._completed_pieces:
            return False
        piece = self._pieces[piece_index]
        return piece.state in (PieceState.NOT_DOWNLOADED, PieceState.FAILED)

    def is_piece_finished(self, piece_index: int) -> bool:
        """对应 is_finished (piece 级)."""
        return piece_index in self._completed_pieces

    # ----- 公开 API: piece 选择 (rarest-first) -----

    def select_rarest_piece(self, peer_has_pieces: Set[int],
                             peer_endpoint: Tuple[str, int]) -> Optional[int]:
        """选择最稀有的 piece (BEP-3 标准)."""
        # 1. peer 有 + 我们需要
        candidates = [
            p for p in peer_has_pieces
            if self.is_piece_needed(p)
            and not self.separate_tracker.is_separate_downloading(p)
        ]
        if not candidates:
            return None
        # 2. 选持有 peer 最少的 (rarest)
        candidates.sort(key=lambda p: self._piece_peer_count.get(p, 0))
        return candidates[0]

    def select_sequential_piece(self, file_index: int) -> Optional[int]:
        """对应 get_file_index_for_sequential_download - 顺序下载."""
        return self.file_priority.get_next_sequential_piece(file_index)

    # ----- 公开 API: slice 请求 -----

    def request_slice(self, piece_index: int, offset: int,
                       peer_endpoint: Tuple[str, int],
                       source: PieceSource = PieceSource.BT_PEER) -> Optional[SliceRequest]:
        """对应 on_peer_slice_request_new."""
        # 允许 DOWNLOADING 状态继续请求 slice (piece 在下载中)
        piece = self._pieces[piece_index]
        if piece.state in (PieceState.VERIFIED,):
            return None  # 已完成, 不再请求
        if piece.state == PieceState.NOT_DOWNLOADED:
            piece.state = PieceState.DOWNLOADING
            piece.downloading_source = source
            piece.downloading_peer = peer_endpoint
        # 检查是否已存在
        if self.slice_queue.existed(piece_index, offset, peer_endpoint):
            self.stats["duplicate_slices_ignored"] += 1
            return None
        # 加入队列
        return self.slice_queue.add(
            piece_index, offset, SLICE_SIZE, peer_endpoint, source
        )

    def on_slice_received(self, piece_index: int, offset: int,
                           data: bytes, peer_endpoint: Tuple[str, int]) -> bool:
        """对应 on_peer_load_slice."""
        # 移除请求
        req = self.slice_queue.recv(piece_index, offset, peer_endpoint)
        if not req:
            return False
        # 保存数据
        piece = self._pieces[piece_index]
        piece.downloaded_slices[offset] = data
        # 检查 piece 是否完整
        if len(piece.downloaded_slices) * SLICE_SIZE >= self.piece_size:
            self._on_piece_complete(piece_index)
        return True

    def _on_piece_complete(self, piece_index: int) -> None:
        """对应 on_piecemanage_hash_check_finished."""
        piece = self._pieces[piece_index]
        piece.state = PieceState.DOWNLOADED
        # 标记分离模式完成
        if piece.downloading_source and piece.downloading_source != PieceSource.BT_PEER:
            self.separate_tracker.on_separate_passed(piece_index)
        self.stats["pieces_downloaded"] += 1
        # 简化: 假设总是通过 hash check
        piece.state = PieceState.VERIFIED
        self._completed_pieces.add(piece_index)
        self.stats["pieces_verified"] += 1

    # ----- 公开 API: 分离模式 -----

    def request_separate_piece(self, piece_index: int,
                                source: PieceSource) -> bool:
        """对应 on_p2sp_piece_request_new - 分离模式请求."""
        if not self.is_piece_needed(piece_index):
            return False
        if self.separate_tracker.is_separate_downloading(piece_index):
            return False
        return self.separate_tracker.start_separate(piece_index, source)

    def on_separate_piece_failed(self, piece_index: int) -> None:
        """对应 on_separate_downloaded_piece_failed - 分离 piece 失败."""
        self.separate_tracker.on_separate_failed(piece_index)
        # 回退到 BT 重新请求
        piece = self._pieces[piece_index]
        piece.state = PieceState.NOT_DOWNLOADED
        piece.failure_count += 1
        self.stats["pieces_failed"] += 1

    # ----- 公开 API: P2SP/eMule 协调 -----

    def on_p2sp_emule_cancel_all_other_peers(self, piece_index: int) -> None:
        """对应 on_p2sp_emule_cancel_all_other_peers.

        当 P2SP/eMule 接管 piece, 取消其他 BT peer 的请求.
        """
        for req in self.slice_queue._requests.get(piece_index, [])[:]:
            if req.source != PieceSource.BT_PEER:
                continue
            self.slice_queue.cancel(piece_index, req.offset, req.peer_endpoint)

    # ----- 公开 API: 超时 + 统计 -----

    def timeout_check(self) -> List[SliceRequest]:
        """对应 queue_download_timeout_check."""
        timed_out = self.slice_queue.timeout_check()
        for req in timed_out:
            piece = self._pieces.get(req.piece_index)
            if piece:
                piece.failure_count += 1
        return timed_out

    def on_peer_have_piece(self, peer_endpoint: Tuple[str, int],
                            piece_index: int) -> None:
        """对应 on_peer_have_piece - peer 报告持有."""
        self._piece_peer_count[piece_index] += 1

    def on_peer_lost_piece(self, peer_endpoint: Tuple[str, int],
                            piece_index: int) -> None:
        """对应 on_me_lost_piece."""
        if self._piece_peer_count[piece_index] > 0:
            self._piece_peer_count[piece_index] -= 1

    def get_availability_percent(self) -> float:
        """对应 availability_percent - 已下载 piece 百分比."""
        if self.total_pieces == 0:
            return 0.0
        return (len(self._completed_pieces) / self.total_pieces) * 100.0

    def get_health_percent(self) -> float:
        """对应 health_percent - 整体 swarm 健康度."""
        # 简化: 平均每个 piece 有多少 peer 持有
        if not self._piece_peer_count:
            return 0.0
        avg = sum(self._piece_peer_count.values()) / max(len(self._piece_peer_count), 1)
        return min(100.0, avg * 10)  # 10 peer 平均 = 100% health

    def get_stats(self) -> Dict:
        s = dict(self.stats)
        s["availability_percent"] = self.get_availability_percent()
        s["health_percent"] = self.get_health_percent()
        s["separate_stats"] = self.separate_tracker.stats
        s["pending_slices"] = self.slice_queue.size()
        s["file_priority_stats"] = self.file_priority.stats
        return s


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    logging.basicConfig(level=logging.INFO,
                        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s")
    print("=" * 60)
    print("BitComet Piece 调度器 demo")
    print("=" * 60)
    # 100 个 piece, 2 个文件
    files = [
        {"name": "video.mp4", "size": 50 * 256 * 1024,
         "first_piece": 0, "last_piece": 49, "piece_count": 50},
        {"name": "sub.srt", "size": 50 * 256 * 1024,
         "first_piece": 50, "last_piece": 99, "piece_count": 50},
    ]
    sched = PieceScheduler(total_pieces=100, piece_size=256*1024, files=files)
    # 设置优先级
    sched.file_priority.set_file_priority(0, FilePriority.HIGH)
    sched.file_priority.set_file_priority(1, FilePriority.NORMAL)
    # 模拟 peer A 持有 piece 0-30
    peer_a = ("1.2.3.4", 6881)
    for i in range(31):
        sched.on_peer_have_piece(peer_a, i)
    # peer B 持有 piece 20-99
    peer_b = ("5.6.7.8", 6881)
    for i in range(20, 100):
        sched.on_peer_have_piece(peer_b, i)
    # 选 piece (rarest)
    print("\n[1] Rarest-first 选择")
    peer_a_has = set(range(31))
    piece = sched.select_rarest_piece(peer_a_has, peer_a)
    print(f"  → piece {piece} (peer_count={sched._piece_peer_count.get(piece, 0)})")
    # 请求 slice
    print("\n[2] Slice 请求")
    req = sched.request_slice(piece, 0, peer_a)
    print(f"  → slice request: piece={req.piece_index} offset={req.offset} peer={req.peer_endpoint}")
    # 模拟分离模式 (P2SP 接管 piece 50)
    print("\n[3] 分离模式 (P2SP 接管 piece 50)")
    ok = sched.request_separate_piece(50, PieceSource.P2SP)
    print(f"  → separate start: {ok}")
    # BT peer 不应再请求 piece 50
    piece_50 = sched.select_rarest_piece({50}, peer_a)
    print(f"  → select piece 50: {piece_50} (应为 None)")
    # 完成 piece 50
    sched.on_slice_received(50, 0, b"\x00" * SLICE_SIZE, peer_a)
    print(f"  → piece 50 state: {sched._pieces[50].state.name}")
    # 超时检查
    print("\n[4] 超时检查")
    timed_out = sched.timeout_check()
    print(f"  → timed out: {len(timed_out)} requests")
    # 统计
    print("\n=== Stats ===")
    for k, v in sched.get_stats().items():
        if isinstance(v, dict):
            print(f"  {k}:")
            for k2, v2 in v.items():
                print(f"    {k2}: {v2}")
        else:
            print(f"  {k}: {v}")
