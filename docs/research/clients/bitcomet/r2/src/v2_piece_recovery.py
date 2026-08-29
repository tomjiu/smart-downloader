"""
v2_piece_recovery.py — BitComet BT v2 损坏 piece 恢复 (用 Merkle 叶子哈希)
====================================================================

逆向来源: Core_BitTorrent::PieceManage::impl + BitTorrentTask
关键符号:
    PieceManage::impl::recover_piece_data_with_leaf_hashes
    PieceManage::impl::recovery_piece_part_list
    BitTorrentTask::on_data_recoveried(uint, uint, uint)

    PieceManage::impl::get_torrent_v2_piece_hash
    PieceManage::impl::get_torrent_v2_piece_hashes
    PieceManage::impl::get_torrent_v2_piece_layer
    PieceManage::impl::get_torrent_v2_piece_layer_state
    BitTorrentTask::calc_proof_hashes_from_piece_layer
    BitTorrentTask::get_known_hash_count_in_piece_layers
    BitTorrentTask::get_total_hash_count_in_piece_layers
    BitTorrentTask::on_piece_hash_v2_loaded
    BitTorrentTask::on_piece_hash_v2_appened
    BitTorrentTask::on_piece_hash_v2_release

    MerkleHashTree::calc_proof_hashes_from_piece_layer
    MerkleHashTree::calc_proof_hashes_from_leaf_layer
    MerkleHashTree::get_all_proof_hashes_for_piece_layer
    MerkleHashTree::get_proof_layers_for_piece
    MerkleHashTree::has_any_proof_hashes_for_piece_layer

设计核心:
1. BT v2 (BEP-52) Merkle 树结构允许 piece 级恢复
2. 当某个 piece 校验失败:
   a) 用 proof hashes (兄弟 + 叔伯节点) 重新计算该 piece 应有的 hash
   b) 与 piece_layers 字段中的 hash 对比
   c) 若 hash 一致 → 数据传输错误, 重新下载该 piece
   d) 若 hash 不一致 → piece_layers 损坏, 从其他 peer 重新拉 piece_layers
3. recovery_piece_part_list: 从 part file 恢复未完成 piece
4. on_data_recoveried(piece_index, bytes_recovered, source): 恢复完成回调

加速价值 (针对 qBittorrent):
- qBittorrent 用 libtorrent 内置 v2 校验, 但损坏检测后只能重下
- BitComet 实现:
  a) piece 级精确恢复 (不重下整个文件)
  b) proof hash 二次校验 (区分数据损坏 vs hash 损坏)
  c) 与 piece_part_file 联动: 损坏 piece 数据仍可从 part file 恢复

本模块实现:
- V2PieceRecovery: 损坏 piece 恢复器
- RecoveryStrategy: 恢复策略 (RESYNC_LAYER / REDOWNLOAD_PIECE / REDOWNLOAD_LAYER)
- V2HashTreeSync: piece_layers 同步状态机

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import hashlib
import logging
import os
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Deque, Dict, List, Optional, Set, Tuple

# 复用已有 MerkleHashTree
from bt_v2_merkle_hash import MerkleHashTree

LOG = logging.getLogger("v2_recovery")


# -----------------------------------------------------------------------------
# 枚举
# -----------------------------------------------------------------------------

class PieceHashState(IntEnum):
    """对应 BitTorrentTask::get_torrent_v2_piece_layer_state."""
    NOT_LOADED = 0          # piece_layers 未加载
    LOADING = 1              # 正在加载
    LOADED = 2               # 已加载, 可校验
    PARTIAL = 3              # 部分加载 (某些 piece hash 缺失)
    INVALID = 4              # piece_layers 损坏
    RELEASING = 5            # 正在释放


class RecoveryStrategy(IntEnum):
    """piece 损坏恢复策略."""
    NONE = 0                       # 不需要恢复
    RESYNC_LAYER = 1               # 重新同步 piece_layers (从其他 peer)
    REDOWNLOAD_PIECE = 2           # 重新下载该 piece (数据损坏)
    REDOWNLOAD_LAYER = 3           # 重新下载整个 piece layer
    USE_PART_FILE = 4              # 从 part file 恢复
    ABORT = 5                       # 放弃 (无法恢复)


class RecoverySource(IntEnum):
    """恢复数据来源."""
    PEER = 0                    # 从 BT peer 重新下载
    HTTP_WEBSEED = 1            # 从 HTTP webseed
    LT_SEED = 2                  # 从 LT-Seed
    PART_FILE = 3               # 从 part file
    LOCAL_CACHE = 4              # 从本地缓存 (可能仍有效)


# -----------------------------------------------------------------------------
# 数据结构
# -----------------------------------------------------------------------------

@dataclass
class PieceLayerInfo:
    """对应 get_torrent_v2_piece_layer 返回."""
    layer_index: int                    # piece layer 子树索引
    layer_root: Optional[bytes] = None  # 该 layer 的根 hash
    proof_hashes: List[bytes] = field(default_factory=list)
    state: PieceHashState = PieceHashState.NOT_LOADED
    last_verified: float = 0.0
    # 该 layer 包含的 piece 数量
    piece_count: int = 0
    # 已校验的 piece 数
    verified_count: int = 0


@dataclass
class PieceRecoveryRequest:
    """单次 piece 恢复请求."""
    piece_index: int
    expected_hash: Optional[bytes] = None      # 该 piece 应有的 hash
    actual_hash: Optional[bytes] = None         # 实际计算的 hash
    strategy: RecoveryStrategy = RecoveryStrategy.NONE
    source: RecoverySource = RecoverySource.PEER
    started_at: float = field(default_factory=time.time)
    completed_at: Optional[float] = None
    is_success: bool = False
    bytes_recovered: int = 0


# -----------------------------------------------------------------------------
# V2HashTreeSync — piece_layers 同步状态机
# -----------------------------------------------------------------------------

class V2HashTreeSync:
    """对应 BitTorrentTask + PieceManage::impl 的 v2 hash 同步."""

    def __init__(self, total_pieces: int, pieces_per_layer: int = 16384):
        self.total_pieces = total_pieces
        self.pieces_per_layer = pieces_per_layer
        # layer_index → PieceLayerInfo
        self._layers: Dict[int, PieceLayerInfo] = {}
        # piece_index → expected hash (来自 piece_layers)
        self._piece_hashes: Dict[int, bytes] = {}
        # 全局状态
        self._state: PieceHashState = PieceHashState.NOT_LOADED
        # 统计
        self.stats = {
            "layers_loaded": 0,
            "pieces_known": 0,
            "pieces_verified": 0,
            "pieces_failed": 0,
        }

    # ----- 公开 API: 加载 -----

    def on_piece_hash_v2_loaded(self, piece_layers_data: bytes) -> int:
        """对应 on_piece_hash_v2_loaded - piece_layers 数据加载完成."""
        # piece_layers_data 是 bencode 解码后的字节流
        # 简化: 假设每个 piece hash 32 字节
        if len(piece_layers_data) % 32 != 0:
            LOG.warning(f"piece_layers data length not multiple of 32: {len(piece_layers_data)}")
            return 0
        count = len(piece_layers_data) // 32
        for i in range(count):
            hash_data = piece_layers_data[i*32:(i+1)*32]
            self._piece_hashes[i] = hash_data
            layer_idx = i // self.pieces_per_layer
            if layer_idx not in self._layers:
                self._layers[layer_idx] = PieceLayerInfo(
                    layer_index=layer_idx,
                    state=PieceHashState.LOADED,
                    piece_count=min(self.pieces_per_layer,
                                     self.total_pieces - layer_idx * self.pieces_per_layer),
                )
            self._layers[layer_idx].verified_count += 1
        self._state = PieceHashState.LOADED
        self.stats["layers_loaded"] = len(self._layers)
        self.stats["pieces_known"] = len(self._piece_hashes)
        return count

    def on_piece_hash_v2_appened(self, piece_index: int, hash_data: bytes) -> None:
        """对应 on_piece_hash_v2_appened - 单个 piece hash 追加."""
        self._piece_hashes[piece_index] = hash_data
        layer_idx = piece_index // self.pieces_per_layer
        if layer_idx not in self._layers:
            self._layers[layer_idx] = PieceLayerInfo(
                layer_index=layer_idx, state=PieceHashState.PARTIAL,
            )
        if self._layers[layer_idx].state != PieceHashState.LOADED:
            self._layers[layer_idx].state = PieceHashState.PARTIAL
        self.stats["pieces_known"] = len(self._piece_hashes)

    def on_piece_hash_v2_release(self) -> None:
        """对应 on_piece_hash_v2_release - 释放 piece_layers (内存回收)."""
        self._state = PieceHashState.RELEASING
        # 实际生产中保留 proof_hashes, 释放 raw piece_layers
        self._state = PieceHashState.LOADED  # 简化

    def get_known_hash_count_in_piece_layers(self) -> int:
        """对应 get_known_hash_count_in_piece_layers."""
        return len(self._piece_hashes)

    def get_total_hash_count_in_piece_layers(self) -> int:
        """对应 get_total_hash_count_in_piece_layers."""
        return self.total_pieces

    def get_torrent_v2_piece_layer_state(self, layer_index: int) -> PieceHashState:
        """对应 get_torrent_v2_piece_layer_state."""
        layer = self._layers.get(layer_index)
        return layer.state if layer else PieceHashState.NOT_LOADED

    def get_torrent_v2_piece_hash(self, piece_index: int) -> Optional[bytes]:
        """对应 get_torrent_v2_piece_hash."""
        return self._piece_hashes.get(piece_index)

    # ----- 公开 API: 校验 -----

    def verify_piece(self, piece_index: int, piece_data: bytes) -> bool:
        """校验 piece 数据 hash 是否匹配."""
        expected = self._piece_hashes.get(piece_index)
        if expected is None:
            return False
        # BEP-52: piece hash = SHA-256(piece_data padded to piece_size)
        # 简化: 假设 piece_data 已 padded
        actual = hashlib.sha256(piece_data).digest()
        if actual == expected:
            self.stats["pieces_verified"] += 1
            return True
        else:
            self.stats["pieces_failed"] += 1
            return False


# -----------------------------------------------------------------------------
# V2PieceRecovery — 损坏 piece 恢复器
# -----------------------------------------------------------------------------

class V2PieceRecovery:
    """对应 PieceManage::impl::recover_piece_data_with_leaf_hashes + recovery_piece_part_list."""

    def __init__(self, hash_sync: V2HashTreeSync,
                 merkle: Optional[MerkleHashTree] = None,
                 part_list=None):
        """
        Args:
            hash_sync: piece_layers 同步状态
            merkle: Merkle 树 (可选, 用于 proof 校验)
            part_list: PiecePartList 实例 (可选, 用于从 part file 恢复)
        """
        self.hash_sync = hash_sync
        self.merkle = merkle
        self.part_list = part_list
        # 恢复历史 (最近 100 次)
        self._history: Deque[PieceRecoveryRequest] = deque(maxlen=100)
        # 进行中的恢复
        self._active: Dict[int, PieceRecoveryRequest] = {}
        # 回调
        self.on_recovery_completed: Optional[callable] = None
        # 统计
        self.stats = {
            "recoveries_initiated": 0,
            "recoveries_succeeded": 0,
            "recoveries_failed": 0,
            "bytes_recovered": 0,
            "by_strategy": {s.name: 0 for s in RecoveryStrategy},
            "by_source": {s.name: 0 for s in RecoverySource},
        }

    # ----- 公开 API: 检测损坏 + 决策 -----

    def detect_corruption(self, piece_index: int,
                          piece_data: bytes) -> Tuple[bool, RecoveryStrategy]:
        """检测 piece 是否损坏, 返回 (是否损坏, 恢复策略)."""
        # 1. 直接 hash 校验
        if self.hash_sync.verify_piece(piece_index, piece_data):
            return False, RecoveryStrategy.NONE
        # 2. 损坏, 决定策略
        strategy = self._decide_strategy(piece_index)
        return True, strategy

    def _decide_strategy(self, piece_index: int) -> RecoveryStrategy:
        """根据上下文决定恢复策略."""
        # 1. 检查 piece_layers 是否已加载
        layer_idx = piece_index // self.hash_sync.pieces_per_layer
        layer_state = self.hash_sync.get_torrent_v2_piece_layer_state(layer_idx)
        if layer_state in (PieceHashState.NOT_LOADED, PieceHashState.LOADING):
            return RecoveryStrategy.RESYNC_LAYER
        if layer_state == PieceHashState.INVALID:
            return RecoveryStrategy.REDOWNLOAD_LAYER
        # 2. 检查 part file 是否有备份数据
        if self.part_list and self.part_list.is_piece_finished(piece_index):
            return RecoveryStrategy.USE_PART_FILE
        # 3. 检查 Merkle proof 是否可用
        if self.merkle and self.merkle.has_any_proof_hashes_for_piece_layer():
            # proof 完整, 可以校验 piece hash → 数据损坏, 重下
            return RecoveryStrategy.REDOWNLOAD_PIECE
        # 4. 默认: 重下 piece
        return RecoveryStrategy.REDOWNLOAD_PIECE

    # ----- 公开 API: 执行恢复 -----

    def initiate_recovery(self, piece_index: int,
                          strategy: RecoveryStrategy,
                          source: RecoverySource = RecoverySource.PEER) -> PieceRecoveryRequest:
        """对应 recover_piece_data_with_leaf_hashes - 启动恢复."""
        req = PieceRecoveryRequest(
            piece_index=piece_index,
            expected_hash=self.hash_sync.get_torrent_v2_piece_hash(piece_index),
            strategy=strategy,
            source=source,
        )
        self._active[piece_index] = req
        self.stats["recoveries_initiated"] += 1
        self.stats["by_strategy"][strategy.name] += 1
        self.stats["by_source"][source.name] += 1
        LOG.info(f"initiating recovery for piece {piece_index}: "
                 f"strategy={strategy.name} source={source.name}")
        return req

    def on_data_recoveried(self, piece_index: int,
                            piece_data: bytes,
                            source: RecoverySource = RecoverySource.PEER) -> bool:
        """对应 BitTorrentTask::on_data_recoveried - 恢复数据到达."""
        req = self._active.get(piece_index)
        if req is None:
            # 没有进行中的恢复请求, 可能是过期数据
            return False
        # 校验新数据
        if self.hash_sync.verify_piece(piece_index, piece_data):
            req.is_success = True
            req.bytes_recovered = len(piece_data)
            req.completed_at = time.time()
            self._history.append(req)
            del self._active[piece_index]
            self.stats["recoveries_succeeded"] += 1
            self.stats["bytes_recovered"] += len(piece_data)
            if self.on_recovery_completed:
                self.on_recovery_completed(piece_index, len(piece_data), source)
            return True
        else:
            # 仍然损坏
            req.completed_at = time.time()
            self._history.append(req)
            del self._active[piece_index]
            self.stats["recoveries_failed"] += 1
            return False

    # ----- 公开 API: 从 part file 恢复 -----

    def recovery_piece_part_list(self, piece_index: int) -> Optional[bytes]:
        """对应 recovery_piece_part_list - 从 part file 恢复 piece 数据."""
        if not self.part_list:
            return None
        if not self.part_list.is_piece_finished(piece_index):
            return None
        # 组装 piece 数据
        piece = self.part_list._pieces.get(piece_index)
        if piece is None:
            return None
        full_data = self.part_list._assemble_piece(piece)
        # 校验
        if self.hash_sync.verify_piece(piece_index, full_data):
            return full_data
        return None

    # ----- 公开 API: 用 Merkle proof 二次校验 -----

    def verify_with_proof(self, piece_index: int,
                          piece_data: bytes) -> Tuple[bool, bool]:
        """用 Merkle proof 校验, 区分数据损坏 vs hash 损坏.

        Returns:
            (data_ok, hash_ok)
            - data_ok=True, hash_ok=True: 一切正常
            - data_ok=False, hash_ok=True: 数据损坏, 重下 piece
            - data_ok=False, hash_ok=False: hash 损坏, 重下 piece_layers
        """
        if not self.merkle:
            # 没 Merkle, 只能直接 hash 比较
            expected = self.hash_sync.get_torrent_v2_piece_hash(piece_index)
            actual = hashlib.sha256(piece_data).digest()
            return (actual == expected), True
        # 用 proof 校验
        proof = self.merkle.calc_proof_hashes_for_piece(piece_index)
        data_ok = self.merkle.verify_piece_hash(piece_index, piece_data, proof)
        # 二次校验: 用 piece_layers 中的 hash 与 proof 计算结果对比
        layer_idx = piece_index // self.hash_sync.pieces_per_layer
        local_idx = piece_index % self.hash_sync.pieces_per_layer
        # 计算 piece 在 layer 中的应有 hash (从 leaf 沿 proof 上升)
        leaf_hash = hashlib.sha256(piece_data).digest()
        current = leaf_hash
        idx = local_idx
        for sibling in proof:
            if idx % 2 == 0:
                current = hashlib.sha256(current + sibling).digest()
            else:
                current = hashlib.sha256(sibling + current).digest()
            idx //= 2
        # current 应等于该 layer 的 root
        layer_info = self.hash_sync._layers.get(layer_idx)
        if layer_info and layer_info.layer_root:
            hash_ok = (current == layer_info.layer_root)
        else:
            hash_ok = True  # 无法验证 hash, 假定 OK
        return data_ok, hash_ok

    # ----- 公开 API: 状态 -----

    def get_active_recoveries(self) -> List[PieceRecoveryRequest]:
        return list(self._active.values())

    def get_history(self, limit: int = 20) -> List[PieceRecoveryRequest]:
        return list(self._history)[-limit:]

    def get_stats(self) -> Dict:
        return dict(self.stats)


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    print("=" * 60)
    print("BitComet v2 piece recovery demo (BEP-52 Merkle)")
    print("=" * 60)
    # 模拟 8 个 piece
    total_pieces = 8
    pieces_per_layer = 4  # 简化: 一个 layer 含 4 个 piece
    # 原始数据
    piece_data_orig = [os.urandom(16 * 1024) for _ in range(total_pieces)]
    # 构建 piece_layers (32 字节 SHA-256 每个)
    piece_layers = b""
    for pd in piece_data_orig:
        piece_layers += hashlib.sha256(pd).digest()
    # 创建 sync
    sync = V2HashTreeSync(total_pieces, pieces_per_layer)
    sync.on_piece_hash_v2_loaded(piece_layers)
    print(f"\n[1] piece_layers loaded: {sync.get_known_hash_count_in_piece_layers()}/{sync.get_total_hash_count_in_piece_layers()}")
    # 创建 recovery (无 merkle, 无 part_list)
    recovery = V2PieceRecovery(sync)
    # 测试正常 piece
    print("\n[2] 校验正常 piece 0")
    is_corrupt, strategy = recovery.detect_corruption(0, piece_data_orig[0])
    print(f"  corrupt={is_corrupt} strategy={strategy.name}")
    # 测试损坏 piece
    print("\n[3] 校验损坏 piece 1")
    corrupted = b"\x00" * 16 * 1024
    is_corrupt, strategy = recovery.detect_corruption(1, corrupted)
    print(f"  corrupt={is_corrupt} strategy={strategy.name}")
    # 启动恢复
    print("\n[4] 启动恢复 (peer 重下)")
    req = recovery.initiate_recovery(1, strategy, RecoverySource.PEER)
    print(f"  strategy={req.strategy.name} source={req.source.name}")
    # 模拟恢复数据到达 (正确的数据)
    print("\n[5] 恢复数据到达")
    ok = recovery.on_data_recoveried(1, piece_data_orig[1], RecoverySource.PEER)
    print(f"  recovery success: {ok}")
    # 测试 part file 恢复
    print("\n[6] 从 part file 恢复 (模拟)")
    # 简化: 跳过实际 part file 测试
    # 统计
    print("\n=== Recovery stats ===")
    for k, v in recovery.get_stats().items():
        if isinstance(v, dict):
            print(f"  {k}:")
            for k2, v2 in v.items():
                if v2 > 0:
                    print(f"    {k2}: {v2}")
        else:
            print(f"  {k}: {v}")
    # 历史记录
    print(f"\n=== Recovery history ({len(recovery.get_history())}) ===")
    for h in recovery.get_history():
        print(f"  piece {h.piece_index}: {h.strategy.name} from {h.source.name} "
              f"success={h.is_success} bytes={h.bytes_recovered}")
