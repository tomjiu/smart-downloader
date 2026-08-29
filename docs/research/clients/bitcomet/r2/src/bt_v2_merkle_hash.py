"""
bt_v2_merkle_hash.py — BitComet BEP-52 BT v2 Merkle 哈希树实现
=========================================================

逆向来源: Core_BitTorrent::MerkleHashTree + Core_BitTorrent::BitTorrentTask
关键符号 (完整方法表):
    MerkleHashTree::MerkleHashTree
    MerkleHashTree::assign_hash / assign_leaf_hash / assign_root_hash
    MerkleHashTree::assign_piece_hash_proof_layers / assign_proof_hash
    MerkleHashTree::calc_proof_hashes_from_leaf_layer
    MerkleHashTree::calc_proof_hashes_from_piece_layer
    MerkleHashTree::get_all_proof_hashes_for_piece_layer
    MerkleHashTree::get_hash_index_in_piece_layer
    MerkleHashTree::get_hash_index_in_tree_from_leaf_index
    MerkleHashTree::get_hash_index_in_tree_from_piece_index
    MerkleHashTree::get_leaf_hash
    MerkleHashTree::get_num_assigned_leaf_hashes / get_num_leaf_hashes
    MerkleHashTree::get_num_leaves_in_piece
    MerkleHashTree::get_num_piece_hashes / get_num_proof_hashes_for_piece_layer
    MerkleHashTree::get_padding_leaf_hash / get_padding_piece_hash
    MerkleHashTree::get_parent_hash_index_in_tree
    MerkleHashTree::get_piece_count_for_file_size
    MerkleHashTree::get_piece_index_in_task
    MerkleHashTree::get_proof_hashes_for_leaf_hashes / get_proof_hashes_for_piece_hashes
    MerkleHashTree::get_proof_layers_for_leaf / get_proof_layers_for_piece
    MerkleHashTree::get_root_hash / get_root_hash_for_hashes / get_root_hash_for_hashes_auto
    MerkleHashTree::get_root_hash_for_piece_hashes
    MerkleHashTree::get_sibling_hash_index_in_tree
    MerkleHashTree::get_tree_layer_of_leaf_hashes / get_tree_layer_of_piece_hashes
    MerkleHashTree::get_uncle_hash_index_in_tree
    MerkleHashTree::has_any_proof_hashes_for_piece_layer
    MerkleHashTree::has_leaf_hash / has_leaf_layer / has_piece_layer / has_root_hash
    MerkleHashTree::bit_length

    BitTorrentTask::calc_proof_hashes_from_piece_layer
    BitTorrentTask::encode_torrent_v2_piece_hash_proof_layers
    BitTorrentTask::encode_torrent_v2_piece_hashes
    BitTorrentTask::encode_torrent_v2_piece_layers
    BitTorrentTask::get_known_hash_count_in_piece_layers
    BitTorrentTask::get_total_hash_count_in_piece_layers
    BitTorrentTask::get_torrent_v2_piece_layer_state
    BitTorrentTask::on_piece_hash_v2_loaded
    BitTorrentTask::on_piece_hash_v2_appened
    BitTorrentTask::on_piece_hash_v2_release

    BitTorrentPeer::upgrade_bittorrent_protocol_v1_to_v2
    BitTorrentProtocolInterface::protocol_bittorrent_upgrade_v1_to_v2
    BitTorrentProtocolInterface::protocol_bittorrent_has_infohash_v2
    BitTorrentProtocolInterface::protocol_bittorrent_my_infohash_v2

    MakeTorrentTaskImpl::build_torrent_v2_file_tree
    MakeTorrentTaskImpl::encode_torrent_v2_file_tree
    MakeTorrentTaskImpl::encode_torrent_v2_piece_layers
    MakeTorrentTaskImpl::sort_v1_file_list_as_v2_file_tree

    PieceManage::torrent_read_piece_layers
    PieceManage::impl::init_pieces_hash_v2
    PieceManage::impl::hash_check_task_v2

    Core_Common::TorrentFileV2Decode::is_in_file_tree
    Core_Common::TorrentFileV2Decode::process_file_tree_dict_enter/leave

设计核心 (BEP-52 BT v2):
1. 每个 piece (默认 16 KiB) 的 SHA-256 是叶子节点
2. 叶子按 2^14 (16384) 个一组, 构成一棵 Merkle 子树
3. 子树根 = "piece hash" (放在 piece layers 字段)
4. 文件级 Merkle 树根 = "file tree" 节点的 hash

BitComet 完整实现了:
- Merkle hash 树计算
- Proof layers 生成
- v1 → v2 协议升级
- v2 hybrid 兼容 (同时支持 v1 info_hash + v2 info_hash_v2)

加速价值 (针对 qBittorrent):
- qBittorrent (master) 已支持 BEP-52, 但实现深埋在 libtorrent 内部
- BitComet 的独立实现可移植到任何 BT 客户端
- 自研下载器可用此模块做 v1/v2 hybrid 创建/校验

本模块实现:
- MerkleHashTree: 完整 BEP-52 Merkle 树
- BtV2TorrentBuilder: v2 torrent 创建
- BtV2HashVerifier: v2 hash 校验
- BtProtocolUpgrader: v1 → v2 协议升级

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import hashlib
import os
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Tuple


# -----------------------------------------------------------------------------
# BEP-52 常量
# -----------------------------------------------------------------------------

# BEP-52 默认 piece 大小: 16 KiB (叶子 hash 单位)
PIECE_SIZE_V2 = 16 * 1024
# 默认 piece layers 大小: 2^14 个 piece (即 16384 个叶子)
# 这意味着一个 piece hash 子树覆盖 16KiB * 16384 = 256 MiB
PIECES_PER_PIECE_LAYER = 1 << 14
# SHA-256 输出长度
SHA256_LEN = 32
# 填充用全 0
PADDING_LEAF_HASH = b"\x00" * SHA256_LEN


# -----------------------------------------------------------------------------
# MerkleHashTree — 完整 BEP-52 Merkle 树
# -----------------------------------------------------------------------------

class MerkleHashTree:
    """对应 Core_BitTorrent::MerkleHashTree.

    BEP-52 树结构:
        root
        ├─ piece_layer_root_1 (覆盖 16384 个 piece)
        │  ├─ piece_hash_1 (子树根, 覆盖 16384 piece = 256 MiB)
        │  ├─ piece_hash_2
        │  └─ ...
        └─ file_layer (file_tree, BEP-52 v2 file tree)

    每个 piece (16KiB) 的 hash 是叶子, 16384 个叶子组成一棵子树.
    """

    def __init__(self, file_size: int, piece_size: int = PIECE_SIZE_V2):
        self.file_size = file_size
        self.piece_size = piece_size
        # 叶子数 = ceil(file_size / piece_size)
        self.num_leaves = (file_size + piece_size - 1) // piece_size
        # 一个 piece layer 子树能覆盖多少 piece
        self.pieces_per_layer = min(PIECES_PER_PIECE_LAYER, max(self.num_leaves, 1))
        # 叶子 hash 数组 (index 0..num_leaves-1)
        self._leaf_hashes: Dict[int, bytes] = {}
        # piece layer 子树根 (index 0..num_layers-1)
        self._piece_layer_hashes: Dict[int, bytes] = {}
        # proof hashes (用于验证单个 piece, 不需要整棵树)
        self._proof_layers: Dict[int, List[bytes]] = {}
        # 根 hash
        self._root_hash: Optional[bytes] = None
        # 树深度
        self._tree_depth = self._calc_tree_depth()

    # ----- 公开 API: 计算 -----

    def assign_leaf_hash(self, leaf_index: int, hash_data: bytes) -> None:
        """对应 assign_leaf_hash."""
        assert len(hash_data) == SHA256_LEN
        self._leaf_hashes[leaf_index] = hash_data

    def assign_piece_hash_proof_layers(self, piece_index: int,
                                        proof_hashes: List[bytes]) -> None:
        """对应 assign_piece_hash_proof_layers."""
        self._proof_layers[piece_index] = proof_hashes

    def calc_root_hash(self) -> bytes:
        """对应 get_root_hash_for_hashes / get_root_hash_for_hashes_auto.

        从所有叶子计算根 hash.
        """
        if self._root_hash is not None:
            return self._root_hash
        # 1. 先算每个 piece layer 子树的根
        for layer_idx in range(self._get_num_piece_layers()):
            self._calc_piece_layer_hash(layer_idx)
        # 2. 再算总根
        layer_hashes = list(self._piece_layer_hashes.values())
        if not layer_hashes:
            self._root_hash = PADDING_LEAF_HASH
        elif len(layer_hashes) == 1:
            self._root_hash = layer_hashes[0]
        else:
            # piece layer 之间再做 Merkle 组合
            self._root_hash = self._merkle_combine(layer_hashes)
        return self._root_hash

    def calc_proof_hashes_for_piece(self, piece_index: int) -> List[bytes]:
        """对应 calc_proof_hashes_from_piece_layer.

        给定一个 piece index, 返回它的 proof path (从叶子到根的兄弟节点 hash).
        """
        if piece_index not in self._leaf_hashes:
            return []
        layer_idx = piece_index // self.pieces_per_layer
        local_idx = piece_index % self.pieces_per_layer
        # 在该 layer 子树内的 proof path
        proof = []
        current_level = self._build_full_level(layer_idx)
        idx = local_idx
        while len(current_level) > 1:
            sibling_idx = idx ^ 1
            if sibling_idx < len(current_level):
                proof.append(current_level[sibling_idx])
            else:
                proof.append(PADDING_LEAF_HASH)
            # 上升一层
            current_level = self._merkle_level_up(current_level)
            idx //= 2
        return proof

    def verify_piece_hash(self, piece_index: int, piece_data: bytes,
                           proof: List[bytes]) -> bool:
        """用 proof 验证单个 piece 的 hash 是否正确."""
        leaf_hash = hashlib.sha256(piece_data).digest()
        layer_idx = piece_index // self.pieces_per_layer
        local_idx = piece_index % self.pieces_per_layer
        # 沿 proof 上升
        current = leaf_hash
        idx = local_idx
        for sibling in proof:
            if idx % 2 == 0:
                current = hashlib.sha256(current + sibling).digest()
            else:
                current = hashlib.sha256(sibling + current).digest()
            idx //= 2
        # 最终应等于该 layer 的根
        return current == self._piece_layer_hashes.get(layer_idx)

    # ----- 公开 API: 查询 -----

    def get_leaf_hash(self, leaf_index: int) -> Optional[bytes]:
        return self._leaf_hashes.get(leaf_index)

    def get_num_leaf_hashes(self) -> int:
        return len(self._leaf_hashes)

    def get_num_assigned_leaf_hashes(self) -> int:
        return len(self._leaf_hashes)

    def get_num_piece_hashes(self) -> int:
        return len(self._piece_layer_hashes)

    def get_num_piece_layers(self) -> int:
        return self._get_num_piece_layers()

    def get_piece_count_for_file_size(self, file_size: int) -> int:
        return (file_size + self.piece_size - 1) // self.piece_size

    def get_padding_leaf_hash(self) -> bytes:
        """对应 get_padding_leaf_hash - BEP-52 用全 0 作填充."""
        return PADDING_LEAF_HASH

    def get_padding_piece_hash(self) -> bytes:
        """对应 get_padding_piece_hash - 填充 piece 子树根."""
        # 计算全 0 叶子构成的子树根
        leaves = [PADDING_LEAF_HASH] * self.pieces_per_layer
        return self._merkle_combine(leaves)

    def get_root_hash(self) -> Optional[bytes]:
        return self._root_hash

    def get_root_hash_for_piece_hashes(self, piece_hashes: List[bytes]) -> bytes:
        """从 piece hashes (子树根列表) 算根."""
        if not piece_hashes:
            return PADDING_LEAF_HASH
        return self._merkle_combine(piece_hashes)

    def has_root_hash(self) -> bool:
        return self._root_hash is not None

    def has_leaf_hash(self, leaf_index: int) -> bool:
        return leaf_index in self._leaf_hashes

    def has_leaf_layer(self) -> bool:
        return len(self._leaf_hashes) > 0

    def has_piece_layer(self) -> bool:
        return len(self._piece_layer_hashes) > 0

    def has_any_proof_hashes_for_piece_layer(self) -> bool:
        return len(self._proof_layers) > 0

    def get_hash_index_in_piece_layer(self, piece_index: int) -> int:
        """对应 get_hash_index_in_piece_layer."""
        return piece_index % self.pieces_per_layer

    def get_hash_index_in_tree_from_leaf_index(self, leaf_index: int) -> int:
        """对应 get_hash_index_in_tree_from_leaf_index."""
        # 完整树中位置: layer_index * pieces_per_layer + local
        layer_idx = leaf_index // self.pieces_per_layer
        local = leaf_index % self.pieces_per_layer
        return layer_idx * self.pieces_per_layer + local

    def get_hash_index_in_tree_from_piece_index(self, piece_index: int) -> int:
        return self.get_hash_index_in_tree_from_leaf_index(piece_index)

    def get_parent_hash_index_in_tree(self, hash_index: int) -> int:
        """对应 get_parent_hash_index_in_tree."""
        return hash_index // 2

    def get_sibling_hash_index_in_tree(self, hash_index: int) -> int:
        """对应 get_sibling_hash_index_in_tree."""
        return hash_index ^ 1

    def get_uncle_hash_index_in_tree(self, hash_index: int) -> int:
        """对应 get_uncle_hash_index_in_tree (parent's sibling)."""
        parent = self.get_parent_hash_index_in_tree(hash_index)
        return parent ^ 1

    def get_tree_layer_of_leaf_hashes(self) -> int:
        """叶子层所在深度 (0 是根)."""
        return self._tree_depth

    def get_tree_layer_of_piece_hashes(self) -> int:
        """piece layer (子树根层) 所在深度."""
        # 叶子深度 14, 所以 piece layer 深度 = 14 - log2(pieces_per_layer) = 0
        # 但实际 BEP-52 中 piece layer 是叶子层之上的一层
        return max(0, self._tree_depth - 14)

    def get_num_leaves_in_piece(self) -> int:
        """每个 piece 包含多少叶子 (BEP-52 默认 1)."""
        return 1  # 1 leaf per piece (piece_size = leaf_size = 16KiB)

    def bit_length(self) -> int:
        """对应 bit_length - 树的位深度."""
        return self._tree_depth

    # ----- 内部 -----

    def _calc_tree_depth(self) -> int:
        if self.num_leaves <= 1:
            return 0
        depth = 0
        n = self.num_leaves
        while n > 1:
            n = (n + 1) // 2
            depth += 1
        return depth

    def _get_num_piece_layers(self) -> int:
        return (self.num_leaves + self.pieces_per_layer - 1) // self.pieces_per_layer

    def _calc_piece_layer_hash(self, layer_idx: int) -> bytes:
        """计算某个 piece layer 子树的根."""
        if layer_idx in self._piece_layer_hashes:
            return self._piece_layer_hashes[layer_idx]
        start = layer_idx * self.pieces_per_layer
        end = min(start + self.pieces_per_layer, self.num_leaves)
        leaves = []
        for i in range(start, end):
            leaves.append(self._leaf_hashes.get(i, PADDING_LEAF_HASH))
        # 补齐到 2^14
        while len(leaves) < self.pieces_per_layer:
            leaves.append(PADDING_LEAF_HASH)
        root = self._merkle_combine(leaves)
        self._piece_layer_hashes[layer_idx] = root
        return root

    def _build_full_level(self, layer_idx: int) -> List[bytes]:
        """构造某 layer 子树的叶子层 (含填充)."""
        start = layer_idx * self.pieces_per_layer
        end = min(start + self.pieces_per_layer, self.num_leaves)
        leaves = []
        for i in range(start, end):
            leaves.append(self._leaf_hashes.get(i, PADDING_LEAF_HASH))
        while len(leaves) < self.pieces_per_layer:
            leaves.append(PADDING_LEAF_HASH)
        return leaves

    @staticmethod
    def _merkle_combine(leaves: List[bytes]) -> bytes:
        """Merkle 树层叠: pairs → hash, 直到只剩一个根."""
        if not leaves:
            return PADDING_LEAF_HASH
        level = list(leaves)
        # 如果数量不是 2 的幂, 补齐
        while len(level) & (len(level) - 1):  # not power of 2
            level.append(PADDING_LEAF_HASH)
        while len(level) > 1:
            new_level = []
            for i in range(0, len(level), 2):
                new_level.append(hashlib.sha256(level[i] + level[i+1]).digest())
            level = new_level
        return level[0]

    @staticmethod
    def _merkle_level_up(level: List[bytes]) -> List[bytes]:
        new_level = []
        for i in range(0, len(level), 2):
            sibling = level[i+1] if i+1 < len(level) else PADDING_LEAF_HASH
            new_level.append(hashlib.sha256(level[i] + sibling).digest())
        return new_level


# -----------------------------------------------------------------------------
# BtV2TorrentBuilder — v2 torrent 创建
# -----------------------------------------------------------------------------

class BtV2TorrentBuilder:
    """对应 MakeTorrentTaskImpl 的 v2 部分."""

    def __init__(self, piece_size: int = PIECE_SIZE_V2):
        self.piece_size = piece_size
        # file_tree 字段: dict { "": <root node> }
        self.file_tree: Dict = {}
        # piece_layers 字段: dict { piece_hash_root: <proof layers bytes> }
        self.piece_layers: Dict[bytes, bytes] = {}
        # 文件列表
        self.files: List[Dict] = []

    def add_standalone_file(self, name: str, file_path: str) -> Dict:
        """对应 AddStandaloneFile.

        BEP-52 v2 file tree 节点结构:
        {
            "": <root>,
            "<file_name>": {
                "length": <int>,
                "pieces root": <32-byte SHA-256 root>
            }
        }
        """
        file_size = os.path.getsize(file_path)
        # 计算 piece root
        merkle = MerkleHashTree(file_size, self.piece_size)
        with open(file_path, "rb") as f:
            piece_idx = 0
            while True:
                data = f.read(self.piece_size)
                if not data:
                    break
                # BEP-52: padding 到 piece_size 用 0
                if len(data) < self.piece_size:
                    data = data + b"\x00" * (self.piece_size - len(data))
                leaf_hash = hashlib.sha256(data).digest()
                merkle.assign_leaf_hash(piece_idx, leaf_hash)
                piece_idx += 1
        root = merkle.calc_root_hash()
        node = {
            "length": file_size,
            "pieces root": root,
        }
        self.file_tree[name] = node
        self.files.append({"name": name, "path": file_path, "size": file_size,
                            "merkle": merkle, "root": root})
        return node

    def encode_torrent_v2_file_tree(self) -> bytes:
        """对应 encode_torrent_v2_file_tree.

        BEP-52 v2 bencode:
            d8:meta versioni2e9:file tree<file_tree_dict>...
        """
        # 简化版 bencode
        result = b"d12:meta versioni2e9:file treed"
        for name, node in self.file_tree.items():
            result += f"{len(name)}:{name}d".encode()
            result += f"6:lengthi{node['length']}e".encode()
            result += b"11:pieces root32:" + node["pieces root"]
            result += b"e"
        result += b"e"
        return result

    def encode_torrent_v2_piece_layers(self) -> bytes:
        """对应 encode_torrent_v2_piece_layers.

        BEP-52 piece layers 字段:
            d<pieces_root_str>base32:<proof_layers_bytes>e
        """
        result = b"d"
        for f in self.files:
            merkle: MerkleHashTree = f["merkle"]
            # 收集所有 piece layer hashes
            layers = []
            for layer_idx in range(merkle.get_num_piece_layers()):
                if layer_idx in merkle._piece_layer_hashes:
                    layers.append(merkle._piece_layer_hashes[layer_idx])
            if layers:
                proof_bytes = b"".join(layers)
                key = f["root"]
                # BEP-52 用 base32 编码 key
                import base64
                key_b32 = base64.b32encode(key).decode().rstrip("=").lower()
                result += f"{len(key_b32)}:{key_b32}".encode()
                result += f"{len(proof_bytes)}:".encode() + proof_bytes
        result += b"e"
        return result

    @staticmethod
    def get_suitable_piece_size_for_file_size(file_size: int) -> int:
        """对应 get_suitable_piece_size_for_file_size.

        BEP-52 推荐: piece_size 使 piece_layers 不超过 1MB
        """
        # 简化: 总是 16 KiB (BEP-52 标准)
        return PIECE_SIZE_V2


# -----------------------------------------------------------------------------
# BtV2HashVerifier — v2 piece 校验
# -----------------------------------------------------------------------------

class BtV2HashVerifier:
    """对应 BitTorrentTask + PieceManage v2 校验."""

    def __init__(self, merkle: MerkleHashTree):
        self.merkle = merkle

    def verify_piece(self, piece_index: int, piece_data: bytes) -> bool:
        """校验单个 piece, 用 proof."""
        proof = self.merkle.calc_proof_hashes_for_piece(piece_index)
        return self.merkle.verify_piece_hash(piece_index, piece_data, proof)

    def verify_all_pieces(self, piece_data_iter) -> Tuple[int, int]:
        """校验所有 pieces, 返回 (passed, failed)."""
        passed = failed = 0
        for i, data in enumerate(piece_data_iter):
            if self.verify_piece(i, data):
                passed += 1
            else:
                failed += 1
        return passed, failed


# -----------------------------------------------------------------------------
# BtProtocolUpgrader — v1 → v2 协议升级
# -----------------------------------------------------------------------------

class BtProtocolUpgrader:
    """对应 BitTorrentPeer::upgrade_bittorrent_protocol_v1_to_v2."""

    def __init__(self, file_path: str, v1_info_hash: bytes):
        self.file_path = file_path
        self.v1_info_hash = v1_info_hash
        self.v2_root_hash: Optional[bytes] = None

    def upgrade(self) -> bytes:
        """从 v1 torrent 升级到 v2, 返回 v2 root hash."""
        size = os.path.getsize(self.file_path)
        builder = BtV2TorrentBuilder()
        builder.add_standalone_file(os.path.basename(self.file_path), self.file_path)
        self.v2_root_hash = builder.files[0]["root"]
        return self.v2_root_hash

    def build_hybrid_magnet(self) -> str:
        """生成 hybrid magnet 链 (同时含 v1 + v2 hash)."""
        if not self.v2_root_hash:
            self.upgrade()
        import base64
        v2_b32 = base64.b32encode(self.v2_root_hash).decode().rstrip("=").lower()
        return (f"magnet:?xt=urn:btih:{self.v1_info_hash.hex()}"
                f"&xt=urn:btmh:1220{v2_b32}")


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser(description="BitComet BT v2 Merkle hash demo")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p_demo = sub.add_parser("demo", help="演示 Merkle 树 + v2 校验")
    p_demo.add_argument("--size", type=int, default=64 * 1024, help="文件大小 (字节)")
    p_demo.add_argument("--piece-size", type=int, default=16 * 1024)

    args = ap.parse_args()

    if args.cmd == "demo":
        # 模拟一个文件
        data = os.urandom(args.size)
        # 创建 Merkle 树
        merkle = MerkleHashTree(args.size, args.piece_size)
        # 填充叶子 hash
        for i in range(0, args.size, args.piece_size):
            piece = data[i:i+args.piece_size]
            if len(piece) < args.piece_size:
                piece = piece + b"\x00" * (args.piece_size - len(piece))
            merkle.assign_leaf_hash(i // args.piece_size, hashlib.sha256(piece).digest())
        # 计算根
        root = merkle.calc_root_hash()
        print(f"file size   : {args.size} bytes")
        print(f"piece size  : {args.piece_size} bytes")
        print(f"num leaves  : {merkle.num_leaves}")
        print(f"num layers  : {merkle.get_num_piece_layers()}")
        print(f"tree depth  : {merkle.bit_length()}")
        print(f"root hash   : {root.hex()[:32]}...")

        # Proof 验证
        piece_idx = 2
        piece_data = data[piece_idx*args.piece_size:(piece_idx+1)*args.piece_size]
        proof = merkle.calc_proof_hashes_for_piece(piece_idx)
        print(f"\npiece {piece_idx} proof path length: {len(proof)}")
        ok = merkle.verify_piece_hash(piece_idx, piece_data, proof)
        print(f"verify piece {piece_idx}: {'PASS' if ok else 'FAIL'}")

        # 损坏数据验证应失败
        bad_data = b"\x00" * args.piece_size
        bad = merkle.verify_piece_hash(piece_idx, bad_data, proof)
        print(f"verify corrupted piece: {'PASS (BUG!)' if bad else 'FAIL (correct)'}")
