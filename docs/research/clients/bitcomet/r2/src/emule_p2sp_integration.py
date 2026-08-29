"""
emule_p2sp_integration.py — BitComet eMule + P2SP 集成层
=====================================================

逆向来源: Core_BitTorrent::BitTorrentPeerPool + BitTorrentTask
关键符号:
    BitTorrentPeerPool::on_p2sp_emule_cancel_all_other_peers
    BitTorrentPeerPool::on_p2sp_emule_piece_downloaded
    BitTorrentPeerPool::on_p2sp_emule_piece_request_remove
    BitTorrentPeerPool::on_p2sp_get_bitfield
    BitTorrentPeerPool::on_p2sp_piece_request_new
    BitTorrentPeer::on_p2sp_emule_cancel_all_other_peers
    BitTorrentPeer::on_p2sp_emule_piece_downloaded
    BitTorrentPeer::on_p2sp_emule_piece_request_remove

    BitTorrentTask::is_enable_emule
    BitTorrentTask::is_enable_p2sp
    BitTorrentTask::on_p2sp_file_no_new_request
    BitTorrentTask::get_download_source

    BitTorrentPeer::get_rate_http_download  (来自 BitTorrentPeerPool)
    BitTorrentPeer::get_rate_p2sp_udp_download

    url_helper_bclink::url_emule_t
    BitTorrentTaskWrapper::task_status_emule_t

设计核心:
1. BitComet 把 eMule (eD2k) + HTTP/FTP/P2SP 集成到 BT 任务中
2. 每个文件有多个 "下载源":
   - BT peers (来自 tracker/DHT/PEX)
   - HTTP webseeds (来自 magnet ?ws=)
   - FTP mirrors (来自用户输入)
   - eMule sources (来自 ed2k:// 链接)
   - P2SP UDP peers (来自 BitComet 云端)
   - LT-Seeds (来自 LT-Seed 协议)
3. 协调策略:
   - 每个 piece 由一个 source 独占下载 (避免重复)
   - 当某 source 速度慢, 自动切换到其他 source
   - eMule piece 下载完后, 通知所有 BT peer "我有这个 piece"
4. is_enable_emule / is_enable_p2sp 开关
5. task_status_emule_t 状态记录 (eMule 任务进度)

加速价值 (针对 qBittorrent):
- qBittorrent 仅支持 BT + HTTP webseed (BEP-19)
- BitComet 多源下载让 piece 在多个协议间分配
- 加速效果: 死种场景 eMule 救场; HTTP 镜像提速

本模块实现:
- MultiSourceTask: 多源任务统一管理
- EmuleIntegration: eMule source 集成
- P2SPIntegration: P2SP (HTTP/FTP/LT-Seed) 集成
- SourceCoordinator: 跨源 piece 协调器

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import asyncio
import hashlib
import logging
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Deque, Dict, List, Optional, Set, Tuple

LOG = logging.getLogger("emule_p2sp")


# -----------------------------------------------------------------------------
# 枚举
# -----------------------------------------------------------------------------

class DownloadSource(IntEnum):
    """对应 BitComet get_download_source 的返回值."""
    BT = 0             # BitTorrent peer
    HTTP_WEBSEED = 1   # BEP-19 HTTP webseed
    FTP = 2             # FTP mirror
    EMULE = 3           # eD2k/eMule
    P2SP_UDP = 4        # BitComet P2SP UDP
    LT_SEED = 5         # BitComet LT-Seed


class SourceHealth(IntEnum):
    """source 健康度."""
    DEAD = 0           # 无响应, 应切换
    SLOW = 1            # 慢, 可考虑切换
    NORMAL = 2
    FAST = 3


# -----------------------------------------------------------------------------
# 数据结构
# -----------------------------------------------------------------------------

@dataclass
class SourceStats:
    """单个下载源的统计."""
    source_type: DownloadSource
    endpoint: str                # URL 或 (ip, port)
    is_alive: bool = True
    bytes_downloaded: int = 0
    bytes_uploaded: int = 0
    pieces_downloaded: int = 0
    last_activity: float = field(default_factory=time.time)
    avg_speed_bps: float = 0.0
    health: SourceHealth = SourceHealth.NORMAL
    # eMule 特有
    emule_file_hash: Optional[str] = None  # 32 hex
    emule_rating: int = 0
    # FTP 特有
    ftp_supports_resume: bool = True
    # HTTP 特有
    http_supports_range: bool = True


@dataclass
class TaskStatusEmule:
    """对应 BitTorrentTaskWrapper::task_status_emule_t."""
    is_enabled: bool = False
    file_hash: Optional[str] = None  # 32 hex
    file_size: int = 0
    file_name: Optional[str] = None
    sources_count: int = 0
    completed_sources: int = 0
    bytes_downloaded: int = 0
    pieces_downloaded: int = 0
    last_speed_check: float = field(default_factory=time.time)


@dataclass
class PieceDownloadAssignment:
    """piece 下载分配记录."""
    piece_index: int
    source_type: DownloadSource
    source_endpoint: str
    started_at: float = field(default_factory=time.time)
    completed_at: Optional[float] = None
    is_completed: bool = False
    is_failed: bool = False
    bytes_downloaded: int = 0


# -----------------------------------------------------------------------------
# SourceCoordinator — 跨源 piece 协调器
# -----------------------------------------------------------------------------

class SourceCoordinator:
    """对应 BitTorrentPeerPool::on_p2sp_* 系列 + on_p2sp_emule_* 系列."""

    def __init__(self, total_pieces: int, piece_size: int = 256 * 1024):
        self.total_pieces = total_pieces
        self.piece_size = piece_size
        # piece_index → 当前下载该 piece 的 source
        self._assignments: Dict[int, PieceDownloadAssignment] = {}
        # 已完成的 piece
        self._completed: Set[int] = set()
        # source endpoint → SourceStats
        self._sources: Dict[str, SourceStats] = {}
        # 配置开关
        self.enable_emule: bool = True
        self.enable_p2sp: bool = True
        self.enable_lt_seed: bool = True
        # 慢源切换阈值
        self.slow_threshold_bps: int = 50_000  # 50 KB/s
        # 统计
        self.stats = {
            "pieces_from_bt": 0,
            "pieces_from_http": 0,
            "pieces_from_ftp": 0,
            "pieces_from_emule": 0,
            "pieces_from_p2sp": 0,
            "pieces_from_lt_seed": 0,
            "source_switches": 0,
        }

    # ----- source 注册 -----

    def add_bt_peer(self, endpoint: Tuple[str, int]) -> str:
        key = f"bt://{endpoint[0]}:{endpoint[1]}"
        if key not in self._sources:
            self._sources[key] = SourceStats(
                source_type=DownloadSource.BT, endpoint=str(endpoint)
            )
        return key

    def add_http_webseed(self, url: str) -> str:
        if url not in self._sources:
            self._sources[url] = SourceStats(
                source_type=DownloadSource.HTTP_WEBSEED, endpoint=url,
                http_supports_range=True,
            )
        return url

    def add_ftp_mirror(self, url: str) -> str:
        if url not in self._sources:
            self._sources[url] = SourceStats(
                source_type=DownloadSource.FTP, endpoint=url,
                ftp_supports_resume=True,
            )
        return url

    def add_emule_source(self, endpoint: Tuple[str, int],
                          file_hash: str, rating: int = 0) -> str:
        key = f"ed2k://{endpoint[0]}:{endpoint[1]}"
        if key not in self._sources:
            self._sources[key] = SourceStats(
                source_type=DownloadSource.EMULE, endpoint=str(endpoint),
                emule_file_hash=file_hash, emule_rating=rating,
            )
        return key

    def add_p2sp_udp_source(self, endpoint: Tuple[str, int]) -> str:
        key = f"p2sp://{endpoint[0]}:{endpoint[1]}"
        if key not in self._sources:
            self._sources[key] = SourceStats(
                source_type=DownloadSource.P2SP_UDP, endpoint=str(endpoint),
            )
        return key

    def add_lt_seed_source(self, endpoint: Tuple[str, int]) -> str:
        key = f"ltseed://{endpoint[0]}:{endpoint[1]}"
        if key not in self._sources:
            self._sources[key] = SourceStats(
                source_type=DownloadSource.LT_SEED, endpoint=str(endpoint),
            )
        return key

    # ----- piece 分配 -----

    def assign_piece(self, piece_index: int, source_key: str) -> bool:
        """对应 on_p2sp_piece_request_new - 分配 piece 给 source."""
        if piece_index in self._completed:
            return False
        if piece_index in self._assignments:
            existing = self._assignments[piece_index]
            if not existing.is_failed:
                return False  # 已分配
        source = self._sources.get(source_key)
        if not source or not source.is_alive:
            return False
        self._assignments[piece_index] = PieceDownloadAssignment(
            piece_index=piece_index,
            source_type=source.source_type,
            source_endpoint=source_key,
        )
        LOG.debug("assigned piece %d to %s (%s)",
                  piece_index, source_key, source.source_type.name)
        return True

    def on_piece_downloaded(self, piece_index: int, bytes_downloaded: int) -> None:
        """对应 on_p2sp_emule_piece_downloaded."""
        assignment = self._assignments.get(piece_index)
        if not assignment:
            return
        assignment.is_completed = True
        assignment.completed_at = time.time()
        assignment.bytes_downloaded = bytes_downloaded
        self._completed.add(piece_index)
        # 更新统计
        source = self._sources.get(assignment.source_endpoint)
        if source:
            source.bytes_downloaded += bytes_downloaded
            source.pieces_downloaded += 1
            source.last_activity = time.time()
        # 计数
        stats_key = f"pieces_from_{assignment.source_type.name.lower()}"
        if stats_key in self.stats:
            self.stats[stats_key] += 1

    def on_piece_failed(self, piece_index: int) -> None:
        """piece 下载失败, 释放让其他 source 重试."""
        assignment = self._assignments.pop(piece_index, None)
        if assignment:
            assignment.is_failed = True
            self.stats["source_switches"] += 1

    def on_emule_cancel_all_other_peers(self, piece_index: int) -> None:
        """对应 on_p2sp_emule_cancel_all_other_peers.

        eMule 接管 piece, 取消其他 source 的请求.
        """
        assignment = self._assignments.get(piece_index)
        if not assignment:
            return
        # 如果当前不是 eMule, 取消让 eMule 接管
        if assignment.source_type != DownloadSource.EMULE:
            del self._assignments[piece_index]
            LOG.info("eMule canceling other peers for piece %d", piece_index)

    def on_emule_piece_request_remove(self, piece_index: int) -> None:
        """对应 on_p2sp_emule_piece_request_remove."""
        self._assignments.pop(piece_index, None)

    # ----- source 健康度 -----

    def update_source_health(self) -> None:
        """定期更新 source 健康度."""
        for key, source in self._sources.items():
            if not source.is_alive:
                source.health = SourceHealth.DEAD
                continue
            if source.avg_speed_bps < self.slow_threshold_bps:
                source.health = SourceHealth.SLOW
            elif source.avg_speed_bps < 200_000:
                source.health = SourceHealth.NORMAL
            else:
                source.health = SourceHealth.FAST

    def select_best_source(self, piece_index: int) -> Optional[str]:
        """选择最合适的 source 给 piece."""
        # 检查 source 是否持有该 piece (BT peer 需要 bitfield)
        # 简化: 按 health 排序
        candidates = [
            (k, s) for k, s in self._sources.items()
            if s.is_alive and s.health != SourceHealth.DEAD
        ]
        if not candidates:
            return None
        # 按 health 优先 + 平均速度
        candidates.sort(key=lambda x: (-x[1].health, -x[1].avg_speed_bps))
        return candidates[0][0]

    # ----- 慢源切换 -----

    def check_slow_sources(self) -> List[str]:
        """检查慢源, 返回应切换的 source 列表."""
        to_switch = []
        for key, source in self._sources.items():
            if (source.is_alive and
                source.health == SourceHealth.SLOW and
                source.avg_speed_bps < self.slow_threshold_bps):
                to_switch.append(key)
        return to_switch

    # ----- 查询 -----

    def get_completion_percent(self) -> float:
        if self.total_pieces == 0:
            return 0.0
        return (len(self._completed) / self.total_pieces) * 100.0

    def get_stats(self) -> Dict:
        s = dict(self.stats)
        s["completion_percent"] = self.get_completion_percent()
        s["total_sources"] = len(self._sources)
        s["alive_sources"] = sum(1 for s in self._sources.values() if s.is_alive)
        s["source_health"] = {
            k: v.health.name for k, v in self._sources.items()
        }
        return s


# -----------------------------------------------------------------------------
# EmuleIntegration — eMule source 集成
# -----------------------------------------------------------------------------

class EmuleIntegration:
    """对应 is_enable_emule + task_status_emule_t."""

    def __init__(self, coordinator: SourceCoordinator):
        self.coordinator = coordinator
        self.status = TaskStatusEmule()
        # eMule 文件 hash (32 hex)
        self.file_hash: Optional[str] = None

    def enable(self, file_hash: str, file_size: int, file_name: str) -> None:
        """启用 eMule 集成."""
        self.coordinator.enable_emule = True
        self.file_hash = file_hash
        self.status.is_enabled = True
        self.status.file_hash = file_hash
        self.status.file_size = file_size
        self.status.file_name = file_name

    def disable(self) -> None:
        self.coordinator.enable_emule = False
        self.status.is_enabled = False

    def add_emule_peer(self, endpoint: Tuple[str, int], rating: int = 0) -> None:
        if not self.status.is_enabled:
            return
        self.coordinator.add_emule_source(endpoint, self.file_hash, rating)
        self.status.sources_count += 1

    def on_emule_peer_completed(self, endpoint: Tuple[str, int]) -> None:
        """eMule peer 下载完成整个文件."""
        self.status.completed_sources += 1

    def update_progress(self, bytes_downloaded: int, pieces_downloaded: int) -> None:
        self.status.bytes_downloaded = bytes_downloaded
        self.status.pieces_downloaded = pieces_downloaded
        self.status.last_speed_check = time.time()


# -----------------------------------------------------------------------------
# P2SPIntegration — P2SP (HTTP/FTP/LT-Seed) 集成
# -----------------------------------------------------------------------------

class P2SPIntegration:
    """对应 is_enable_p2sp."""

    def __init__(self, coordinator: SourceCoordinator):
        self.coordinator = coordinator
        self.http_webseeds: List[str] = []
        self.ftp_mirrors: List[str] = []
        self.p2sp_udp_peers: List[Tuple[str, int]] = []
        self.lt_seeds: List[Tuple[str, int]] = []
        self.is_enabled = False

    def enable(self) -> None:
        self.coordinator.enable_p2sp = True
        self.is_enabled = True

    def disable(self) -> None:
        self.coordinator.enable_p2sp = False
        self.is_enabled = False

    def add_http_webseed(self, url: str) -> None:
        if not self.is_enabled:
            return
        self.coordinator.add_http_webseed(url)
        self.http_webseeds.append(url)

    def add_ftp_mirror(self, url: str) -> None:
        if not self.is_enabled:
            return
        self.coordinator.add_ftp_mirror(url)
        self.ftp_mirrors.append(url)

    def add_p2sp_udp_peer(self, endpoint: Tuple[str, int]) -> None:
        if not self.is_enabled:
            return
        self.coordinator.add_p2sp_udp_source(endpoint)
        self.p2sp_udp_peers.append(endpoint)

    def add_lt_seed(self, endpoint: Tuple[str, int]) -> None:
        if not self.is_enabled:
            return
        self.coordinator.add_lt_seed_source(endpoint)
        self.lt_seeds.append(endpoint)

    def on_p2sp_file_no_new_request(self, file_index: int) -> None:
        """对应 on_p2sp_file_no_new_request - 该文件无新 piece 请求."""
        LOG.debug("P2SP file %d has no new requests", file_index)


# -----------------------------------------------------------------------------
# MultiSourceTask — 多源任务统一管理
# -----------------------------------------------------------------------------

class MultiSourceTask:
    """统一管理 BT + eMule + P2SP 任务."""

    def __init__(self, info_hash: bytes, total_pieces: int,
                 piece_size: int = 256 * 1024):
        self.info_hash = info_hash
        self.total_pieces = total_pieces
        self.piece_size = piece_size
        self.coordinator = SourceCoordinator(total_pieces, piece_size)
        self.emule = EmuleIntegration(self.coordinator)
        self.p2sp = P2SPIntegration(self.coordinator)
        # BT peers bitfield (piece_index → 持有 peer 数)
        self._bt_peer_bitfield: Dict[int, Set[Tuple[str, int]]] = defaultdict(set)

    def add_bt_peer(self, endpoint: Tuple[str, int],
                     peer_pieces: Set[int]) -> None:
        """添加 BT peer, 注入 bitfield."""
        key = self.coordinator.add_bt_peer(endpoint)
        for p in peer_pieces:
            self._bt_peer_bitfield[p].add(endpoint)

    def add_emule_source(self, endpoint: Tuple[str, int],
                          file_hash: str, file_size: int,
                          file_name: str, rating: int = 0) -> None:
        self.emule.enable(file_hash, file_size, file_name)
        self.emule.add_emule_peer(endpoint, rating)

    def add_http_webseed(self, url: str) -> None:
        self.p2sp.enable()
        self.p2sp.add_http_webseed(url)

    def add_ftp_mirror(self, url: str) -> None:
        self.p2sp.enable()
        self.p2sp.add_ftp_mirror(url)

    def add_lt_seed(self, endpoint: Tuple[str, int]) -> None:
        self.p2sp.enable()
        self.p2sp.add_lt_seed(endpoint)

    def select_piece_for_source(self, source_key: str,
                                  available_pieces: Set[int]) -> Optional[int]:
        """选择一个 piece 给指定 source 下载."""
        for p in available_pieces:
            if p in self.coordinator._completed:
                continue
            if p in self.coordinator._assignments:
                continue
            # 检查 source 类型约束
            source = self.coordinator._sources.get(source_key)
            if not source:
                continue
            # BT peer 必须持有该 piece
            if source.source_type == DownloadSource.BT:
                ep = eval(source.endpoint)  # 简化
                if ep not in self._bt_peer_bitfield[p]:
                    continue
            # HTTP/FTP/eMule/P2SP/LT-Seed 不需要 bitfield (假设全有)
            return p
        return None

    def get_completion(self) -> float:
        return self.coordinator.get_completion_percent()

    def get_stats(self) -> Dict:
        s = self.coordinator.get_stats()
        s["emule_status"] = {
            "enabled": self.emule.status.is_enabled,
            "sources": self.emule.status.sources_count,
            "completed": self.emule.status.completed_sources,
            "bytes_downloaded": self.emule.status.bytes_downloaded,
        }
        s["p2sp_status"] = {
            "enabled": self.p2sp.is_enabled,
            "http_webseeds": len(self.p2sp.http_webseeds),
            "ftp_mirrors": len(self.p2sp.ftp_mirrors),
            "p2sp_udp_peers": len(self.p2sp.p2sp_udp_peers),
            "lt_seeds": len(self.p2sp.lt_seeds),
        }
        return s


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    logging.basicConfig(level=logging.INFO,
                        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s")
    print("=" * 60)
    print("BitComet eMule + P2SP 多源集成 demo")
    print("=" * 60)
    info_hash = bytes.fromhex("abcdef" * 6 + "abcdef01")
    task = MultiSourceTask(info_hash, total_pieces=100, piece_size=256*1024)
    # 添加 BT peer (持有 piece 0-30)
    task.add_bt_peer(("1.2.3.4", 6881), set(range(31)))
    task.add_bt_peer(("5.6.7.8", 6881), set(range(20, 100)))
    # 添加 eMule source
    task.add_emule_source(
        endpoint=("9.10.11.12", 4662),
        file_hash="a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6",
        file_size=25 * 1024 * 1024, file_name="test_file.bin", rating=5,
    )
    # 添加 HTTP/FTP/P2SP/LT-Seed
    task.add_http_webseed("http://mirror1.example.com/file.bin")
    task.add_ftp_mirror("ftp://mirror2.example.com/file.bin")
    task.add_lt_seed(("13.14.15.16", 25432))
    # 分配 piece
    print("\n[1] 分配 piece 给各 source")
    for p in [0, 1, 2, 50, 51, 99]:
        # 给 BT peer
        bt_key = "bt://('1.2.3.4', 6881)"
        ok = task.coordinator.assign_piece(p, bt_key)
        print(f"  piece {p:3d} → BT peer A: {ok}")
    # eMule 接管 piece 50
    print("\n[2] eMule 接管 piece 50")
    task.coordinator.on_emule_cancel_all_other_peers(50)
    emule_key = "ed2k://('9.10.11.12', 4662)"
    task.coordinator.assign_piece(50, emule_key)
    print(f"  piece 50 reassigned to eMule")
    # 完成 piece
    print("\n[3] 完成 piece 0, 1, 50")
    task.coordinator.on_piece_downloaded(0, 256*1024)
    task.coordinator.on_piece_downloaded(1, 256*1024)
    task.coordinator.on_piece_downloaded(50, 256*1024)
    # 统计
    print("\n=== Stats ===")
    for k, v in task.get_stats().items():
        if isinstance(v, dict):
            print(f"  {k}:")
            for k2, v2 in v.items():
                print(f"    {k2}: {v2}")
        else:
            print(f"  {k}: {v}")
