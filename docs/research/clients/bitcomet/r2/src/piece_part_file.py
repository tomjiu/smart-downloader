"""
piece_part_file.py — BitComet piece-part 临时文件管理 (断电恢复)
=====================================================

逆向来源: Core_BitTorrent::PiecePartList + PiecePartFile
关键符号:
    PiecePartList::PiecePartList
    PiecePartList::PiecePartVector
    PiecePartList::PiecePart_t
    PiecePartList::SlicePart_t
    PiecePartList::clear
    PiecePartList::clear_piece
    PiecePartList::dump_list_info
    PiecePartList::dump_piece_info
    PiecePartList::empty
    PiecePartList::get_download_request
    PiecePartList::is_download_need
    PiecePartList::is_in_list
    PiecePartList::is_piece_finished
    PiecePartList::is_piece_need_save
    PiecePartList::is_piece_saved
    PiecePartList::is_slice_finished
    PiecePartList::loaded_slice_data_check
    PiecePartList::on_data_downloaded
    PiecePartList::rebuild_list
    PiecePartList::save_piece_from_download_files_to_part_file
    PiecePartList::save_piece_from_part_file_to_download_files
    PiecePartList::task_piece_size

    PiecePartFile::PiecePartFile
    PiecePartFile::piece_record_t
    PiecePartFile::slice_record_t
    PiecePartFile::load
    PiecePartFile::load_list
    PiecePartFile::safe_read_int8 / int16 / int32 / int64
    PiecePartFile::safe_read_string
    PiecePartFile::safe_write_int8 / int16 / int32 / int64
    PiecePartFile::safe_write_string
    PiecePartFile::save

设计核心:
1. piece-part 临时文件存储已下载但未完成的 piece
2. 当 piece 部分下载完成 (某些 slice 已收到), 写入 .bc! 文件
3. 应用程序崩溃/断电后, 重启时:
   a) load_list 从 .bc! 文件加载所有 piece 记录
   b) loaded_slice_data_check 校验 slice 数据
   c) save_piece_from_part_file_to_download_files 把已完成的 piece 写回主文件
4. piece_record_t: 持久化 piece 索引 + slice 列表
5. slice_record_t: 持久化单个 slice (offset + length + data)
6. safe_read/write_*: 类型安全的二进制 IO (检测 EOF + 损坏)

加速价值 (针对 qBittorrent):
- qBittorrent 用 libtorrent 内置 part_file, 但:
  a) 单文件管理, 不可定制
  b) 损坏检测不严格 (无 safe_read/write)
- BitComet 实现:
  a) 多 torrent 共享 part_file
  b) 类型安全 IO (防文件损坏)
  c) loaded_slice_data_check 二次校验
  d) rebuild_list 重建内存索引

本模块实现:
- PiecePartFile: 持久化 .bc! 文件 (含 safe_read/write)
- PiecePartList: 内存索引 + 写回主文件
- SliceRecord / PieceRecord: 持久化数据结构

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import hashlib
import logging
import os
import struct
import time
from collections import defaultdict
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Dict, List, Optional, Tuple

LOG = logging.getLogger("partfile")


# -----------------------------------------------------------------------------
# 常量
# -----------------------------------------------------------------------------

PART_FILE_MAGIC = b"BCPP"   # BitComet Piece Part
PART_FILE_VERSION = 1
SLICE_SIZE = 16 * 1024        # 16 KiB (BEP-3 标准 sub-piece)


# -----------------------------------------------------------------------------
# 数据结构
# -----------------------------------------------------------------------------

@dataclass
class SliceRecord:
    """对应 PiecePartFile::slice_record_t."""
    offset: int           # 在 piece 内偏移
    length: int            # slice 长度 (通常 SLICE_SIZE)
    data: bytes            # slice 数据
    crc32: int = 0          # 数据校验


@dataclass
class PieceRecord:
    """对应 PiecePartFile::piece_record_t."""
    piece_index: int
    piece_size: int                 # piece 总大小
    slice_count: int                # slice 数
    slices: List[SliceRecord] = field(default_factory=list)
    # 元信息
    created_at: float = field(default_factory=time.time)
    last_modified: float = field(default_factory=time.time)
    # 校验
    piece_data_hash: Optional[bytes] = None    # 已下载数据的 SHA-1 (用于校验)
    is_complete: bool = False                  # 所有 slice 是否到齐


# -----------------------------------------------------------------------------
# PiecePartFile — 持久化 .bc! 文件
# -----------------------------------------------------------------------------

class PiecePartFile:
    """对应 Core_BitTorrent::PiecePartFile.

    文件格式 (二进制, 大端序):
        magic(4) "BCPP"
        version(1)
        torrent_hash(20)              # torrent info_hash
        piece_count(4)
        [piece_record_t * piece_count]
            piece_index(4)
            piece_size(4)
            slice_count(4)
            created_at(8)
            last_modified(8)
            piece_data_hash_present(1)
            [piece_data_hash(20) if present]
            is_complete(1)
            [slice_record_t * slice_count]
                offset(4)
                length(4)
                crc32(4)
                data(length bytes)
    """

    def __init__(self, file_path: str):
        self.file_path = file_path
        self._fh = None

    # ----- 打开/关闭 -----

    def open(self, mode: str = "r+b") -> None:
        if "b" not in mode:
            mode = mode + "b"
        self._fh = open(self.file_path, mode)

    def close(self) -> None:
        if self._fh:
            self._fh.close()
            self._fh = None

    def __enter__(self):
        self.open()
        return self

    def __exit__(self, *args):
        self.close()

    # ----- safe write (类型安全) -----

    def safe_write_int8(self, value: int) -> None:
        """对应 safe_write_int8."""
        self._fh.write(struct.pack(">B", value & 0xFF))

    def safe_write_int16(self, value: int) -> None:
        self._fh.write(struct.pack(">H", value & 0xFFFF))

    def safe_write_int32(self, value: int) -> None:
        self._fh.write(struct.pack(">I", value & 0xFFFFFFFF))

    def safe_write_int64(self, value: int) -> None:
        self._fh.write(struct.pack(">Q", value))

    def safe_write_string(self, data: bytes) -> None:
        """字符串格式: length(4) + data."""
        self.safe_write_int32(len(data))
        self._fh.write(data)

    # ----- safe read (类型安全, 检测 EOF + 损坏) -----

    def safe_read_int8(self) -> Optional[int]:
        """对应 safe_read_int8. 返回 None 表示 EOF/损坏."""
        data = self._fh.read(1)
        if len(data) != 1:
            return None
        return struct.unpack(">B", data)[0]

    def safe_read_int16(self) -> Optional[int]:
        data = self._fh.read(2)
        if len(data) != 2:
            return None
        return struct.unpack(">H", data)[0]

    def safe_read_int32(self) -> Optional[int]:
        data = self._fh.read(4)
        if len(data) != 4:
            return None
        return struct.unpack(">I", data)[0]

    def safe_read_int64(self) -> Optional[int]:
        data = self._fh.read(8)
        if len(data) != 8:
            return None
        return struct.unpack(">Q", data)[0]

    def safe_read_string(self) -> Optional[bytes]:
        length = self.safe_read_int32()
        if length is None or length > 16 * 1024 * 1024:  # 防止损坏导致大分配
            return None
        data = self._fh.read(length)
        if len(data) != length:
            return None
        return data

    # ----- 高层 API: 保存 -----

    def save(self, torrent_hash: bytes, pieces: List[PieceRecord]) -> None:
        """对应 save - 完整保存所有 piece 记录."""
        if not self._fh:
            self.open("wb")
        self._fh.seek(0)
        self._fh.truncate()
        # header
        self._fh.write(PART_FILE_MAGIC)
        self.safe_write_int8(PART_FILE_VERSION)
        self._fh.write(torrent_hash)
        self.safe_write_int32(len(pieces))
        # pieces
        for p in pieces:
            self._write_piece_record(p)

    def _write_piece_record(self, p: PieceRecord) -> None:
        self.safe_write_int32(p.piece_index)
        self.safe_write_int32(p.piece_size)
        self.safe_write_int32(len(p.slices))
        self.safe_write_int64(int(p.created_at))
        self.safe_write_int64(int(p.last_modified))
        # piece_data_hash (可选)
        if p.piece_data_hash is not None:
            self.safe_write_int8(1)
            self._fh.write(p.piece_data_hash)
        else:
            self.safe_write_int8(0)
        self.safe_write_int8(1 if p.is_complete else 0)
        # slices
        for s in p.slices:
            self._write_slice_record(s)

    def _write_slice_record(self, s: SliceRecord) -> None:
        self.safe_write_int32(s.offset)
        self.safe_write_int32(s.length)
        # 计算 CRC32
        if s.crc32 == 0:
            import zlib
            s.crc32 = zlib.crc32(s.data) & 0xFFFFFFFF
        self.safe_write_int32(s.crc32)
        self._fh.write(s.data)

    # ----- 高层 API: 加载 -----

    def load(self) -> Tuple[Optional[bytes], List[PieceRecord]]:
        """对应 load - 完整加载所有 piece 记录.

        Returns:
            (torrent_hash, pieces) 或 (None, []) 如果文件损坏
        """
        if not self._fh:
            self.open("rb")
        self._fh.seek(0)
        # header
        magic = self._fh.read(4)
        if magic != PART_FILE_MAGIC:
            LOG.error(f"bad magic: {magic!r}")
            return None, []
        version = self.safe_read_int8()
        if version != PART_FILE_VERSION:
            LOG.error(f"unsupported version: {version}")
            return None, []
        torrent_hash = self._fh.read(20)
        if len(torrent_hash) != 20:
            return None, []
        piece_count = self.safe_read_int32()
        if piece_count is None or piece_count > 100000:
            LOG.error(f"bad piece_count: {piece_count}")
            return None, []
        # pieces
        pieces = []
        for _ in range(piece_count):
            p = self._read_piece_record()
            if p is None:
                break
            pieces.append(p)
        return torrent_hash, pieces

    def _read_piece_record(self) -> Optional[PieceRecord]:
        piece_index = self.safe_read_int32()
        if piece_index is None:
            return None
        piece_size = self.safe_read_int32()
        if piece_size is None:
            return None
        slice_count = self.safe_read_int32()
        if slice_count is None or slice_count > 1024:
            return None
        created_at = self.safe_read_int64()
        if created_at is None:
            return None
        last_modified = self.safe_read_int64()
        if last_modified is None:
            return None
        has_hash = self.safe_read_int8()
        if has_hash is None:
            return None
        piece_data_hash = None
        if has_hash == 1:
            piece_data_hash = self._fh.read(20)
            if len(piece_data_hash) != 20:
                return None
        is_complete = self.safe_read_int8()
        if is_complete is None:
            return None
        # slices
        slices = []
        for _ in range(slice_count):
            s = self._read_slice_record()
            if s is None:
                break
            slices.append(s)
        return PieceRecord(
            piece_index=piece_index,
            piece_size=piece_size,
            slice_count=slice_count,
            slices=slices,
            created_at=float(created_at),
            last_modified=float(last_modified),
            piece_data_hash=piece_data_hash,
            is_complete=bool(is_complete),
        )

    def _read_slice_record(self) -> Optional[SliceRecord]:
        offset = self.safe_read_int32()
        if offset is None:
            return None
        length = self.safe_read_int32()
        if length is None or length > 64 * 1024:
            return None
        crc32_val = self.safe_read_int32()
        if crc32_val is None:
            return None
        data = self._fh.read(length)
        if len(data) != length:
            return None
        # CRC32 校验
        import zlib
        actual_crc = zlib.crc32(data) & 0xFFFFFFFF
        if actual_crc != crc32_val:
            LOG.warning(f"slice {offset} CRC mismatch: {crc32_val} != {actual_crc}")
            return None
        return SliceRecord(
            offset=offset, length=length, data=data, crc32=crc32_val,
        )


# -----------------------------------------------------------------------------
# PiecePartList — 内存索引
# -----------------------------------------------------------------------------

class PiecePartList:
    """对应 Core_BitTorrent::PiecePartList.

    内存中维护 piece → slice 列表映射, 持久化到 PiecePartFile.
    """

    def __init__(self, torrent_hash: bytes, piece_size: int,
                 part_file_path: Optional[str] = None):
        self.torrent_hash = torrent_hash
        self.piece_size = piece_size
        self.part_file_path = part_file_path
        # piece_index → PieceRecord
        self._pieces: Dict[int, PieceRecord] = {}
        # 加载持久化
        if part_file_path and os.path.exists(part_file_path):
            self.load_list()

    # ----- 公开 API: 添加 slice -----

    def on_data_downloaded(self, piece_index: int, offset: int,
                            data: bytes) -> bool:
        """对应 on_data_downloaded - 收到 slice 数据."""
        if not self._validate_slice(piece_index, offset, data):
            return False
        piece = self._pieces.get(piece_index)
        if piece is None:
            piece = PieceRecord(
                piece_index=piece_index,
                piece_size=self.piece_size,
                slice_count=0,
            )
            self._pieces[piece_index] = piece
        # 检查重复
        for s in piece.slices:
            if s.offset == offset:
                # 已存在, 检查数据是否一致
                if s.data == data:
                    return True  # 重复, OK
                else:
                    LOG.warning(f"slice {piece_index}:{offset} data mismatch")
                    return False
        # 添加
        slice_record = SliceRecord(offset=offset, length=len(data), data=data)
        piece.slices.append(slice_record)
        piece.slice_count = len(piece.slices)
        piece.last_modified = time.time()
        # 检查是否完成
        if self.is_piece_finished(piece_index):
            piece.is_complete = True
            piece.piece_data_hash = hashlib.sha1(self._assemble_piece(piece)).digest()
        return True

    def _validate_slice(self, piece_index: int, offset: int, data: bytes) -> bool:
        if offset % SLICE_SIZE != 0:
            return False
        if len(data) > SLICE_SIZE:
            return False
        if piece_index < 0:
            return False
        return True

    # ----- 公开 API: 状态查询 -----

    def is_in_list(self, piece_index: int) -> bool:
        """对应 is_in_list."""
        return piece_index in self._pieces

    def is_piece_finished(self, piece_index: int) -> bool:
        """对应 is_piece_finished - 所有 slice 是否到齐."""
        piece = self._pieces.get(piece_index)
        if piece is None:
            return False
        # 计算应有的 slice 数
        expected_slices = (self.piece_size + SLICE_SIZE - 1) // SLICE_SIZE
        if len(piece.slices) < expected_slices:
            return False
        # 检查每个 slice 偏移
        offsets = sorted(s.offset for s in piece.slices)
        for i, offset in enumerate(offsets):
            if offset != i * SLICE_SIZE:
                return False
        return True

    def is_slice_finished(self, piece_index: int, offset: int) -> bool:
        """对应 is_slice_finished."""
        piece = self._pieces.get(piece_index)
        if piece is None:
            return False
        return any(s.offset == offset for s in piece.slices)

    def is_piece_saved(self, piece_index: int) -> bool:
        """对应 is_piece_saved - piece 是否已写回主文件."""
        # 简化: is_complete 即视为 saved
        piece = self._pieces.get(piece_index)
        return piece is not None and piece.is_complete

    def is_piece_need_save(self, piece_index: int) -> bool:
        """对应 is_piece_need_save - piece 是否需要写回主文件."""
        piece = self._pieces.get(piece_index)
        return (piece is not None and piece.is_complete
                and not getattr(piece, "_written_to_main", False))

    def is_download_need(self, piece_index: int) -> bool:
        """对应 is_download_need - piece 是否还需要下载."""
        piece = self._pieces.get(piece_index)
        if piece is None:
            return True  # 不在列表, 需下载
        return not piece.is_complete

    def empty(self) -> bool:
        """对应 empty."""
        return len(self._pieces) == 0

    def clear(self) -> None:
        """对应 clear."""
        self._pieces.clear()

    def clear_piece(self, piece_index: int) -> None:
        """对应 clear_piece."""
        self._pieces.pop(piece_index, None)

    # ----- 公开 API: 持久化 -----

    def save(self) -> None:
        """对应 save - 持久化到 .bc! 文件."""
        if not self.part_file_path:
            return
        with PiecePartFile(self.part_file_path) as pf:
            pf.save(self.torrent_hash, list(self._pieces.values()))

    def load_list(self) -> int:
        """对应 load_list - 从 .bc! 文件加载."""
        if not self.part_file_path or not os.path.exists(self.part_file_path):
            return 0
        with PiecePartFile(self.part_file_path) as pf:
            torrent_hash, pieces = pf.load()
        if torrent_hash != self.torrent_hash:
            LOG.error("part file torrent hash mismatch")
            return 0
        count = 0
        for p in pieces:
            self._pieces[p.piece_index] = p
            count += 1
        # 校验 slice 数据
        self.loaded_slice_data_check()
        return count

    def rebuild_list(self) -> None:
        """对应 rebuild_list - 重建内存索引."""
        self._pieces.clear()
        self.load_list()

    def loaded_slice_data_check(self) -> int:
        """对应 loaded_slice_data_check - 校验已加载的 slice 数据."""
        invalid_count = 0
        for piece_index, piece in list(self._pieces.items()):
            for s in piece.slices[:]:
                # 长度 + 偏移校验
                if s.offset < 0 or s.length <= 0 or s.length > SLICE_SIZE:
                    piece.slices.remove(s)
                    invalid_count += 1
                    continue
                # offset 必须是 SLICE_SIZE 倍数
                if s.offset % SLICE_SIZE != 0:
                    piece.slices.remove(s)
                    invalid_count += 1
        return invalid_count

    # ----- 公开 API: 写回主文件 -----

    def save_piece_from_part_file_to_download_files(self, piece_index: int,
                                                      main_file_path: str,
                                                      piece_offset_in_file: int = 0) -> bool:
        """对应 save_piece_from_part_file_to_download_files.

        把已完成的 piece 写回主下载文件.
        """
        piece = self._pieces.get(piece_index)
        if piece is None or not piece.is_complete:
            return False
        # 组装完整 piece 数据
        full_data = self._assemble_piece(piece)
        # 写入主文件
        try:
            with open(main_file_path, "r+b" if os.path.exists(main_file_path) else "wb") as f:
                f.seek(piece_offset_in_file)
                f.write(full_data)
        except IOError as e:
            LOG.error(f"write main file failed: {e}")
            return False
        # 标记已写回
        piece._written_to_main = True
        # 从 part list 移除 (可选)
        # self.clear_piece(piece_index)
        return True

    def save_piece_from_download_files_to_part_file(self, piece_index: int,
                                                      main_file_path: str,
                                                      piece_offset_in_file: int = 0) -> bool:
        """对应 save_piece_from_download_files_to_part_file.

        反向: 从主文件读取 piece 数据, 存到 part file (用于 task 暂停时).
        """
        try:
            with open(main_file_path, "rb") as f:
                f.seek(piece_offset_in_file)
                data = f.read(self.piece_size)
        except IOError:
            return False
        if len(data) == 0:
            return False
        # 切成 slices
        for i in range(0, len(data), SLICE_SIZE):
            slice_data = data[i:i+SLICE_SIZE]
            if len(slice_data) < SLICE_SIZE:
                slice_data = slice_data + b"\x00" * (SLICE_SIZE - len(slice_data))
            self.on_data_downloaded(piece_index, i, slice_data)
        return True

    # ----- 公开 API: dump 调试 -----

    def dump_piece_info(self, piece_index: int) -> str:
        """对应 dump_piece_info."""
        piece = self._pieces.get(piece_index)
        if piece is None:
            return f"piece {piece_index}: not in list"
        lines = [
            f"piece {piece_index}:",
            f"  size: {piece.piece_size}",
            f"  slices: {piece.slice_count}/{(self.piece_size + SLICE_SIZE - 1) // SLICE_SIZE}",
            f"  is_complete: {piece.is_complete}",
            f"  created_at: {piece.created_at}",
            f"  last_modified: {piece.last_modified}",
        ]
        if piece.piece_data_hash:
            lines.append(f"  data_hash: {piece.piece_data_hash.hex()[:16]}...")
        for s in piece.slices:
            lines.append(f"    slice off={s.offset} len={s.length} crc={s.crc32:08x}")
        return "\n".join(lines)

    def dump_list_info(self) -> str:
        """对应 dump_list_info."""
        lines = [
            f"PiecePartList:",
            f"  torrent_hash: {self.torrent_hash.hex()[:16]}...",
            f"  piece_size: {self.piece_size}",
            f"  total_pieces: {len(self._pieces)}",
            f"  completed: {sum(1 for p in self._pieces.values() if p.is_complete)}",
            f"  partial: {sum(1 for p in self._pieces.values() if not p.is_complete)}",
        ]
        return "\n".join(lines)

    # ----- 内部 -----

    def _assemble_piece(self, piece: PieceRecord) -> bytes:
        """组装完整 piece 数据."""
        # 按 offset 排序
        sorted_slices = sorted(piece.slices, key=lambda s: s.offset)
        # 拼接
        result = b""
        for s in sorted_slices:
            result += s.data
        # padding 到 piece_size
        if len(result) < piece.piece_size:
            result += b"\x00" * (piece.piece_size - len(result))
        return result

    def get_stats(self) -> Dict:
        return {
            "total_pieces": len(self._pieces),
            "completed": sum(1 for p in self._pieces.values() if p.is_complete),
            "partial": sum(1 for p in self._pieces.values() if not p.is_complete),
            "total_slices": sum(p.slice_count for p in self._pieces.values()),
            "total_bytes": sum(
                len(s.data) for p in self._pieces.values() for s in p.slices
            ),
        }


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    import tempfile
    print("=" * 60)
    print("BitComet piece-part file demo (断电恢复)")
    print("=" * 60)
    torrent_hash = b"\xab" * 20
    piece_size = 256 * 1024   # 256 KiB
    # 创建临时 part file
    with tempfile.NamedTemporaryFile(suffix=".bc!", delete=False) as tmp:
        part_file = tmp.name
    try:
        ppl = PiecePartList(torrent_hash, piece_size, part_file)
        # 模拟下载 piece 0 的 16 个 slice
        print("\n[1] 下载 piece 0 的 slice 数据")
        for i in range(16):
            slice_data = os.urandom(SLICE_SIZE)
            ok = ppl.on_data_downloaded(0, i * SLICE_SIZE, slice_data)
            if not ok:
                print(f"  slice {i} failed")
        print(f"  piece 0 complete: {ppl.is_piece_finished(0)}")
        # 部分下载 piece 1
        print("\n[2] 部分下载 piece 1 (8 个 slice)")
        for i in range(8):
            ppl.on_data_downloaded(1, i * SLICE_SIZE, os.urandom(SLICE_SIZE))
        print(f"  piece 1 complete: {ppl.is_piece_finished(1)}")
        # 持久化
        print("\n[3] 持久化到 part file")
        ppl.save()
        print(f"  file size: {os.path.getsize(part_file)} bytes")
        # 模拟断电: 重新加载
        print("\n[4] 模拟断电重启: 重新加载")
        ppl2 = PiecePartList(torrent_hash, piece_size, part_file)
        stats = ppl2.get_stats()
        print(f"  loaded pieces: {stats['total_pieces']}")
        print(f"  completed: {stats['completed']}")
        print(f"  partial: {stats['partial']}")
        print(f"  total_slices: {stats['total_slices']}")
        # 写回主文件
        print("\n[5] 写回主文件 (piece 0)")
        with tempfile.NamedTemporaryFile(delete=False) as main_tmp:
            main_file = main_tmp.name
        try:
            ok = ppl2.save_piece_from_part_file_to_download_files(0, main_file, 0)
            print(f"  write back: {ok}")
            print(f"  main file size: {os.path.getsize(main_file)} bytes")
        finally:
            os.unlink(main_file)
        # dump 调试
        print("\n[6] dump 调试信息")
        print(ppl2.dump_list_info())
        print()
        print(ppl2.dump_piece_info(0))
    finally:
        if os.path.exists(part_file):
            os.unlink(part_file)
