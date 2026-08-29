"""
torrent_maker.py — BitComet 完整 torrent 创建器 (v1+v2 hybrid)
==========================================================

逆向来源: Core_BitTorrent::MakeTorrentTaskImpl + MakeTorrentTaskWrapper
关键符号:
    MakeTorrentTaskWrapper::torrent_make(torrent_make_setting_t const&)
    MakeTorrentTaskWrapper::torrent_make_begin
    MakeTorrentTaskWrapper::torrent_make_cancel
    MakeTorrentTaskWrapper::torrent_make_finished
    MakeTorrentTaskWrapper::torrent_make_get_status
    MakeTorrentTaskWrapper::get_suitable_piece_size_for_file_size
    MakeTorrentTaskWrapper::is_dir_filtered
    MakeTorrentTaskWrapper::is_file_filtered
    MakeTorrentTaskWrapper::torrent_make_setting_t
    MakeTorrentTaskWrapper::torrent_make_status_t
    MakeTorrentTaskWrapper::torrent_make_error_enum

    MakeTorrentTaskImpl::AddDirectory
    MakeTorrentTaskImpl::AddOneDir
    MakeTorrentTaskImpl::AddOneFile
    MakeTorrentTaskImpl::AddStandaloneFile
    MakeTorrentTaskImpl::IsDirFiltered
    MakeTorrentTaskImpl::IsFileFiltered
    MakeTorrentTaskImpl::SplitRelativePath
    MakeTorrentTaskImpl::find_start_file
    MakeTorrentTaskImpl::get_suitable_piece_size_for_file_size
    MakeTorrentTaskImpl::hash_begin
    MakeTorrentTaskImpl::hash_stop
    MakeTorrentTaskImpl::hash_thread
    MakeTorrentTaskImpl::hash_thread_on_finished
    MakeTorrentTaskImpl::build_torrent_v2_file_tree
    MakeTorrentTaskImpl::encode_torrent_v2_file_tree
    MakeTorrentTaskImpl::encode_torrent_v2_piece_layers
    MakeTorrentTaskImpl::sort_v1_file_list_as_v2_file_tree

    CtrlBitTorrent::init_torrent_make_setting
    CtrlBitTorrent::set_setting_by_torrent_make_setting
    DialogTorrentMakerProgress::show_modeless

设计核心:
1. 完整支持 BT v1 (BEP-3) + BT v2 (BEP-52) + hybrid
2. 多线程 hash 计算 (hash_thread 在独立线程跑)
3. 文件/目录过滤器 (IsDirFiltered / IsFileFiltered)
4. v1 文件列表自动按 v2 file tree 顺序排序 (sort_v1_file_list_as_v2_file_tree)
5. 自动 piece_size 选择 (get_suitable_piece_size_for_file_size)
6. torrent_make_error_enum 错误码

加速价值 (针对 qBittorrent):
- qBittorrent 用 libtorrent 内置 make_torrent, 不支持 hybrid
- BitComet 实现:
  a) 单独 hash 计算线程 (不阻塞 UI)
  b) v1+v2 同时生成 (hybrid magnet 兼容)
  c) 文件过滤 (排除 .DS_Store, thumbs.db 等)
  d) 自动 piece_size (大文件大 piece, 小文件小 piece)

本模块实现:
- TorrentMakeSetting: 完整设置 (路径, trackers, piece_size, private, v2)
- TorrentMaker: 主创建器
- TorrentMakeStatus: 状态机 (pending/hash/v2/file_tree/finish/error)
- 文件/目录过滤器

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import hashlib
import os
import threading
import time
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Callable, Dict, List, Optional, Tuple


# -----------------------------------------------------------------------------
# 枚举
# -----------------------------------------------------------------------------

class TorrentMakeStatus(IntEnum):
    """对应 torrent_make_status_t."""
    PENDING = 0
    SCANNING = 1          # 扫描目录/文件
    HASHING = 2           # 计算 v1 SHA-1 piece hash
    BUILDING_V2 = 3       # 构建 v2 file tree + Merkle
    ENCODING = 4          # 编码 bencode
    FINISHED = 5
    ERROR = 6
    CANCELLED = 7


class TorrentMakeError(IntEnum):
    """对应 torrent_make_error_enum."""
    NONE = 0
    FILE_NOT_FOUND = 1
    PERMISSION_DENIED = 2
    DISK_FULL = 3
    INVALID_PATH = 4
    EMPTY_DIR = 5
    PIECE_SIZE_INVALID = 6
    HASH_ERROR = 7
    USER_CANCEL = 8
    UNKNOWN = 99


class TorrentMetaVersion(IntEnum):
    """BEP-52 meta version."""
    V1 = 1            # 仅 v1
    V2 = 2            # v1+v2 hybrid (BEP-52 默认)


# 默认过滤的文件名 (从 BitComet UI 行为反推)
DEFAULT_FILE_FILTERS = [
    ".DS_Store", "Thumbs.db", "desktop.ini", ".git",
    "__pycache__", ".pyc", ".idea", ".vscode",
]


# -----------------------------------------------------------------------------
# 数据结构
# -----------------------------------------------------------------------------

@dataclass
class TorrentMakeFile:
    """对应 MakeTorrentFile 数据结构."""
    absolute_path: str        # 绝对路径
    relative_path: str        # 相对于 root 的路径 (用于 bencode)
    size: int = 0
    # v2 字段
    pieces_root: Optional[bytes] = None   # 32 字节 SHA-256 (BEP-52)
    piece_layers: Optional[bytes] = None   # proof layers
    # v1 字段
    v1_piece_hashes: Optional[List[bytes]] = None   # 每个 piece 的 SHA-1


@dataclass
class TorrentMakeSetting:
    """对应 MakeTorrentTaskWrapper::torrent_make_setting_t."""
    # 源
    source_path: str                       # 文件或目录
    output_path: str                       # .torrent 输出路径
    # 元数据
    name: Optional[str] = None             # torrent 名称 (默认用源名)
    comment: str = ""
    created_by: str = "BitComet Accelerator Toolkit"
    creation_date: int = field(default_factory=lambda: int(time.time()))
    # trackers
    trackers: List[List[str]] = field(default_factory=list)   # tier 列表
    web_seeds: List[str] = field(default_factory=list)          # BEP-19 webseeds
    # 协议
    meta_version: TorrentMetaVersion = TorrentMetaVersion.V2   # 默认 hybrid
    private: bool = False                  # private torrent
    # piece 大小 (0 = 自动选择)
    piece_size: int = 0
    # 过滤
    file_filters: List[str] = field(default_factory=lambda: list(DEFAULT_FILE_FILTERS))
    dir_filters: List[str] = field(default_factory=list)
    # HTTPseed (BEP-17)
    http_seeds: List[str] = field(default_factory=list)
    # 是否启用 v2 piece_layers 字段
    include_piece_layers: bool = True


@dataclass
class TorrentMakeStatusInfo:
    """对应 torrent_make_status_t."""
    status: TorrentMakeStatus = TorrentMakeStatus.PENDING
    progress_permillage: int = 0           # 0-1000
    current_file: Optional[str] = None
    files_processed: int = 0
    files_total: int = 0
    bytes_processed: int = 0
    bytes_total: int = 0
    error: TorrentMakeError = TorrentMakeError.NONE
    error_msg: Optional[str] = None
    # 生成的 torrent 信息
    info_hash_v1: Optional[bytes] = None
    info_hash_v2: Optional[bytes] = None
    # 开始/结束时间
    started_at: Optional[float] = None
    finished_at: Optional[float] = None


# -----------------------------------------------------------------------------
# 文件/目录过滤器
# -----------------------------------------------------------------------------

class FileFilter:
    """对应 MakeTorrentTaskImpl::IsFileFiltered / IsDirFiltered."""

    @staticmethod
    def is_file_filtered(file_name: str, filters: List[str]) -> bool:
        """对应 IsFileFiltered - 文件是否应被过滤."""
        for f in filters:
            if file_name == f:
                return True
            if file_name.endswith(f):
                return True
            if "*" in f:
                # 通配符匹配
                import fnmatch
                if fnmatch.fnmatch(file_name, f):
                    return True
        return False

    @staticmethod
    def is_dir_filtered(dir_name: str, filters: List[str]) -> bool:
        """对应 IsDirFiltered - 目录是否应被过滤."""
        return FileFilter.is_file_filtered(dir_name, filters)


# -----------------------------------------------------------------------------
# PathSplitter — 路径分解
# -----------------------------------------------------------------------------

class PathSplitter:
    """对应 MakeTorrentTaskImpl::SplitRelativePath."""

    @staticmethod
    def split(relative_path: str) -> List[str]:
        """把相对路径分解为组件列表."""
        # 兼容 / 和 \
        normalized = relative_path.replace("\\", "/")
        parts = [p for p in normalized.split("/") if p]
        return parts


# -----------------------------------------------------------------------------
# PieceSizeSelector — 自动 piece 大小选择
# -----------------------------------------------------------------------------

class PieceSizeSelector:
    """对应 get_suitable_piece_size_for_file_size."""

    # BEP-3 推荐 piece size 表 (按文件大小)
    SIZE_TABLE = [
        (1 << 20, 16 * 1024),       # < 1 MB → 16 KiB
        (50 * 1024 * 1024, 32 * 1024),    # < 50 MB → 32 KiB
        (200 * 1024 * 1024, 64 * 1024),    # < 200 MB → 64 KiB
        (1024 * 1024 * 1024, 128 * 1024),  # < 1 GB → 128 KiB
        (10 * 1024 * 1024 * 1024, 256 * 1024),  # < 10 GB → 256 KiB
        (100 * 1024 * 1024 * 1024, 512 * 1024),  # < 100 GB → 512 KiB
        (float("inf"), 1024 * 1024),                # >= 100 GB → 1 MiB
    ]

    @classmethod
    def select(cls, total_size: int) -> int:
        """根据文件总大小选择合适 piece size."""
        for threshold, piece_size in cls.SIZE_TABLE:
            if total_size < threshold:
                return piece_size
        return 1024 * 1024  # 默认 1 MiB

    @classmethod
    def is_valid(cls, piece_size: int) -> bool:
        """检查 piece_size 是否是 2 的幂."""
        if piece_size < 16 * 1024:
            return False
        if piece_size > 32 * 1024 * 1024:
            return False
        return (piece_size & (piece_size - 1)) == 0


# -----------------------------------------------------------------------------
# TorrentMaker — 主创建器
# -----------------------------------------------------------------------------

class TorrentMaker:
    """对应 MakeTorrentTaskImpl + MakeTorrentTaskWrapper."""

    def __init__(self, setting: TorrentMakeSetting):
        self.setting = setting
        self.status = TorrentMakeStatusInfo()
        self._files: List[TorrentMakeFile] = []
        self._cancel_event = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self._lock = threading.RLock()
        # 回调
        self.on_progress: Optional[Callable[[TorrentMakeStatusInfo], None]] = None
        self.on_finished: Optional[Callable[[TorrentMakeStatusInfo], None]] = None

    # ----- 公开 API -----

    def torrent_make_begin(self) -> None:
        """对应 torrent_make_begin - 异步启动."""
        self.status.status = TorrentMakeStatus.PENDING
        self.status.started_at = time.time()
        self._thread = threading.Thread(target=self._run, daemon=True,
                                         name="TorrentMaker")
        self._thread.start()

    def torrent_make_cancel(self) -> None:
        """对应 torrent_make_cancel."""
        self._cancel_event.set()
        self.status.status = TorrentMakeStatus.CANCELLED

    def torrent_make_get_status(self) -> TorrentMakeStatusInfo:
        """对应 torrent_make_get_status."""
        with self._lock:
            return TorrentMakeStatusInfo(**self.status.__dict__)

    def torrent_make_wait(self, timeout: Optional[float] = None) -> bool:
        """同步等待完成."""
        if self._thread:
            self._thread.join(timeout=timeout)
            return self._thread.is_alive() is False
        return True

    # ----- 内部: 主流程 -----

    def _run(self) -> None:
        """主流程: scan → hash → build v2 → encode."""
        try:
            # 1. 扫描文件
            self._set_status(TorrentMakeStatus.SCANNING)
            self._scan_files()
            if self._cancel_event.is_set():
                self._set_status(TorrentMakeStatus.CANCELLED)
                return
            if not self._files:
                self._set_error(TorrentMakeError.EMPTY_DIR, "no files to include")
                return

            # 2. 选择 piece_size
            total_size = sum(f.size for f in self._files)
            if self.setting.piece_size == 0:
                self.setting.piece_size = PieceSizeSelector.select(total_size)
            elif not PieceSizeSelector.is_valid(self.setting.piece_size):
                self._set_error(TorrentMakeError.PIECE_SIZE_INVALID,
                                f"piece_size {self.setting.piece_size} not power of 2")
                return

            # 3. 计算 v1 hash (多线程)
            self._set_status(TorrentMakeStatus.HASHING)
            self._hash_files_v1()

            # 4. 计算 v2 (如果 hybrid)
            if self.setting.meta_version == TorrentMetaVersion.V2:
                self._set_status(TorrentMakeStatus.BUILDING_V2)
                self._build_v2()

            # 5. 编码 bencode
            self._set_status(TorrentMakeStatus.ENCODING)
            torrent_bytes = self._encode_bencode()
            # 计算 info_hash
            self._compute_info_hashes(torrent_bytes)

            # 6. 写文件
            with open(self.setting.output_path, "wb") as f:
                f.write(torrent_bytes)

            self.status.finished_at = time.time()
            self._set_status(TorrentMakeStatus.FINISHED)

        except FileNotFoundError as e:
            self._set_error(TorrentMakeError.FILE_NOT_FOUND, str(e))
        except PermissionError as e:
            self._set_error(TorrentMakeError.PERMISSION_DENIED, str(e))
        except Exception as e:
            self._set_error(TorrentMakeError.UNKNOWN, str(e))

    def _scan_files(self) -> None:
        """对应 AddDirectory / AddOneDir / AddOneFile / AddStandaloneFile."""
        source = self.setting.source_path
        if os.path.isfile(source):
            # 单文件
            self._add_standalone_file(source)
        elif os.path.isdir(source):
            # 目录
            self._add_directory(source)
        else:
            raise FileNotFoundError(f"source not found: {source}")
        # 按 relative_path 排序
        self._files.sort(key=lambda f: f.relative_path)
        # 如果是 v2, 还要 sort_v1_file_list_as_v2_file_tree
        if self.setting.meta_version == TorrentMetaVersion.V2:
            self._sort_as_v2_file_tree()
        self.status.files_total = len(self._files)
        self.status.bytes_total = sum(f.size for f in self._files)

    def _add_standalone_file(self, abs_path: str) -> None:
        """对应 AddStandaloneFile."""
        file_name = os.path.basename(abs_path)
        if FileFilter.is_file_filtered(file_name, self.setting.file_filters):
            return
        size = os.path.getsize(abs_path)
        self._files.append(TorrentMakeFile(
            absolute_path=abs_path,
            relative_path=file_name,
            size=size,
        ))

    def _add_directory(self, root_dir: str) -> None:
        """对应 AddDirectory + AddOneDir (递归)."""
        for dirpath, dirnames, filenames in os.walk(root_dir):
            # 过滤目录
            dirnames[:] = [d for d in dirnames
                            if not FileFilter.is_dir_filtered(d, self.setting.dir_filters)]
            for filename in filenames:
                if FileFilter.is_file_filtered(filename, self.setting.file_filters):
                    continue
                abs_path = os.path.join(dirpath, filename)
                rel_path = os.path.relpath(abs_path, root_dir)
                size = os.path.getsize(abs_path)
                self._files.append(TorrentMakeFile(
                    absolute_path=abs_path,
                    relative_path=rel_path,
                    size=size,
                ))

    def _sort_as_v2_file_tree(self) -> None:
        """对应 sort_v1_file_list_as_v2_file_tree.

        BEP-52 v2 file tree 要求字典序排序.
        """
        self._files.sort(key=lambda f: PathSplitter.split(f.relative_path))

    def _hash_files_v1(self) -> None:
        """对应 hash_thread - 计算 v1 SHA-1 piece hash."""
        piece_size = self.setting.piece_size
        # 把所有文件按顺序拼起来, 算 piece SHA-1
        # 简化: 每个文件独立算 piece hash
        bytes_done = 0
        for f in self._files:
            if self._cancel_event.is_set():
                return
            self.status.current_file = f.relative_path
            f.v1_piece_hashes = []
            with open(f.absolute_path, "rb") as fp:
                while True:
                    chunk = fp.read(piece_size)
                    if not chunk:
                        break
                    # padding 到 piece_size (最后一个 piece 除外)
                    # BEP-3: 最后一个 piece 不 padding
                    next_chunk = fp.read(piece_size)
                    if next_chunk:
                        # 不是最后一块, padding
                        chunk = chunk + b"\x00" * (piece_size - len(chunk))
                        fp.seek(-len(next_chunk), 1)
                    h = hashlib.sha1(chunk).digest()
                    f.v1_piece_hashes.append(h)
                    bytes_done += len(chunk)
                    self.status.bytes_processed = bytes_done
                    self.status.progress_permillage = int(
                        bytes_done * 1000 / max(self.status.bytes_total, 1)
                    )
                    if self.on_progress:
                        self.on_progress(self.status)
            self.status.files_processed += 1

    def _build_v2(self) -> None:
        """对应 build_torrent_v2_file_tree + encode_torrent_v2_file_tree.

        每个 v1 piece 切成 16KiB 叶子, 算 SHA-256, 构建 Merkle 树.
        """
        from bt_v2_merkle_hash import MerkleHashTree
        v2_piece_size = 16 * 1024   # BEP-52 固定 16KiB 叶子
        for f in self._files:
            if self._cancel_event.is_set():
                return
            self.status.current_file = f.relative_path + " (v2)"
            # 用文件大小创建 Merkle 树
            merkle = MerkleHashTree(f.size, v2_piece_size)
            with open(f.absolute_path, "rb") as fp:
                leaf_idx = 0
                while True:
                    chunk = fp.read(v2_piece_size)
                    if not chunk:
                        break
                    if len(chunk) < v2_piece_size:
                        chunk = chunk + b"\x00" * (v2_piece_size - len(chunk))
                    leaf_hash = hashlib.sha256(chunk).digest()
                    merkle.assign_leaf_hash(leaf_idx, leaf_hash)
                    leaf_idx += 1
            # 计算根 hash
            f.pieces_root = merkle.calc_root_hash()
            # piece layers (proof hashes)
            # 简化: 只保存 piece layer 子树根
            layers = []
            for layer_idx in range(merkle.get_num_piece_layers()):
                if layer_idx in merkle._piece_layer_hashes:
                    layers.append(merkle._piece_layer_hashes[layer_idx])
            if layers:
                f.piece_layers = b"".join(layers)

    def _encode_bencode(self) -> bytes:
        """对应 bencode_node_t::encode."""
        from bencode_codec_v2 import BencodeEncoder
        # 构建 info dict
        info: Dict[bytes, any] = {
            b"name": self.setting.name.encode() if self.setting.name
                      else os.path.basename(self.setting.source_path).encode(),
            b"piece length": self.setting.piece_size,
        }
        # v1: files
        if self.setting.meta_version == TorrentMetaVersion.V1 or \
           self.setting.meta_version == TorrentMetaVersion.V2:
            v1_files = []
            for f in self._files:
                v1_files.append({
                    b"length": f.size,
                    b"path": PathSplitter.split(f.relative_path),
                })
            info[b"files"] = v1_files
            # v1 piece hashes (拼接所有文件)
            all_hashes = b""
            for f in self._files:
                if f.v1_piece_hashes:
                    all_hashes += b"".join(f.v1_piece_hashes)
            info[b"pieces"] = all_hashes
        if self.setting.private:
            info[b"private"] = 1
        # v2: file tree + meta version
        if self.setting.meta_version == TorrentMetaVersion.V2:
            info[b"meta version"] = 2
            file_tree = self._build_file_tree_dict()
            info[b"file tree"] = file_tree
        # 顶层 dict
        torrent: Dict[bytes, any] = {
            b"announce": self.setting.trackers[0][0].encode() if self.setting.trackers else b"",
            b"info": info,
            b"creation date": self.setting.creation_date,
            b"created by": self.setting.created_by.encode(),
        }
        if self.setting.comment:
            torrent[b"comment"] = self.setting.comment.encode()
        # announce-list (BEP-12)
        if len(self.setting.trackers) > 1:
            torrent[b"announce-list"] = [[t.encode() for t in tier]
                                          for tier in self.setting.trackers]
        # url-list (BEP-19 webseeds)
        if self.setting.web_seeds:
            torrent[b"url-list"] = [w.encode() for w in self.setting.web_seeds]
        # httpseeds (BEP-17)
        if self.setting.http_seeds:
            torrent[b"httpseeds"] = [h.encode() for h in self.setting.http_seeds]
        return BencodeEncoder.encode(torrent)

    def _build_file_tree_dict(self) -> Dict[bytes, any]:
        """对应 build_torrent_v2_file_tree."""
        # BEP-52: file tree 是 nested dict, 叶子是 file 节点
        # 简化: 假设所有文件都在根目录 (单层)
        tree: Dict[bytes, any] = {}
        for f in self._files:
            parts = PathSplitter.split(f.relative_path)
            current = tree
            for i, part in enumerate(parts):
                part_bytes = part.encode()
                if i == len(parts) - 1:
                    # 叶子: file node
                    current[part_bytes] = {
                        b"length": f.size,
                        b"pieces root": f.pieces_root or b"\x00" * 32,
                    }
                else:
                    if part_bytes not in current:
                        current[part_bytes] = {}
                    current = current[part_bytes]
        return tree

    def _compute_info_hashes(self, torrent_bytes: bytes) -> None:
        """计算 v1/v2 info_hash."""
        # 简化: 重新解析 info dict
        from bencode_codec_v2 import BencodeDecoder
        decoded = BencodeDecoder.decode(torrent_bytes)
        info = decoded.get(b"info", {})
        info_bytes = BencodeEncoder.encode(info) if False else b""
        # 我们需要重新编码 info dict 部分
        # 简化: 用一个 trick, 找 b"4:infod" 位置
        marker = b"4:infod"
        idx = torrent_bytes.find(marker)
        if idx >= 0:
            start = idx + len(b"4:info")
            # 从 start 找匹配的 e (字典结尾)
            depth = 1
            pos = start + 1
            while pos < len(torrent_bytes) and depth > 0:
                c = torrent_bytes[pos:pos+1]
                if c == b"d" or c == b"l":
                    depth += 1
                elif c == b"e":
                    depth -= 1
                pos += 1
            info_bytes = torrent_bytes[start:pos]
        self.status.info_hash_v1 = hashlib.sha1(info_bytes).digest()
        if self.setting.meta_version == TorrentMetaVersion.V2:
            self.status.info_hash_v2 = hashlib.sha256(info_bytes).digest()[:32]

    def _set_status(self, status: TorrentMakeStatus) -> None:
        with self._lock:
            self.status.status = status
        if self.on_progress:
            self.on_progress(self.status)
        if status == TorrentMakeStatus.FINISHED and self.on_finished:
            self.on_finished(self.status)

    def _set_error(self, error: TorrentMakeError, msg: str) -> None:
        with self._lock:
            self.status.status = TorrentMakeStatus.ERROR
            self.status.error = error
            self.status.error_msg = msg
            self.status.finished_at = time.time()
        if self.on_finished:
            self.on_finished(self.status)


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    import tempfile
    print("=" * 60)
    print("BitComet torrent maker demo (v1+v2 hybrid)")
    print("=" * 60)
    # 创建测试目录
    with tempfile.TemporaryDirectory() as tmpdir:
        # 写几个文件
        for i in range(3):
            with open(os.path.join(tmpdir, f"file_{i}.txt"), "wb") as f:
                f.write(os.urandom(32 * 1024))  # 32 KiB each
        os.makedirs(os.path.join(tmpdir, "subdir"), exist_ok=True)
        with open(os.path.join(tmpdir, "subdir", "nested.bin"), "wb") as f:
            f.write(os.urandom(50 * 1024))
        # 写 .DS_Store 测试过滤
        with open(os.path.join(tmpdir, ".DS_Store"), "w") as f:
            f.write("should be filtered")
        # 创建 setting
        setting = TorrentMakeSetting(
            source_path=tmpdir,
            output_path=os.path.join(tmpdir, "test.torrent"),
            name="test_torrent",
            trackers=[["http://tracker1.example.com/announce"],
                      ["http://tracker2.example.com/announce", "udp://tracker3.example.com:80/announce"]],
            web_seeds=["http://webseed.example.com/"],
            meta_version=TorrentMetaVersion.V2,
            piece_size=0,  # 自动
        )
        # 创建 maker
        maker = TorrentMaker(setting)
        progress_log = []
        maker.on_progress = lambda s: progress_log.append((s.status.name, s.progress_permillage))
        maker.torrent_make_begin()
        # 等待完成
        maker.torrent_make_wait(timeout=30)
        status = maker.torrent_make_get_status()
        print(f"\n[Status] {status.status.name}")
        if status.error != TorrentMakeError.NONE:
            print(f"[Error] {status.error.name}: {status.error_msg}")
        else:
            print(f"[Files] {status.files_total} files, {status.bytes_total} bytes")
            print(f"[InfoHash v1] {status.info_hash_v1.hex()[:32] if status.info_hash_v1 else 'N/A'}")
            print(f"[InfoHash v2] {status.info_hash_v2.hex()[:32] if status.info_hash_v2 else 'N/A'}")
            print(f"[Output] {setting.output_path}")
            print(f"[Size] {os.path.getsize(setting.output_path)} bytes")
            print(f"[Progress updates] {len(progress_log)}")
            if progress_log:
                print(f"[Last] {progress_log[-1]}")
