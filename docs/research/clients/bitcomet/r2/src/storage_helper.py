"""
storage_helper.py — BitComet 存储抽象层 + 文件自动开启调度
====================================================

逆向来源: Core_BitTorrent::StorageHelperDelegate + StorageHelper
关键符号:
    StorageHelper::StorageHelper
    StorageHelper::start
    StorageHelper::stop
    StorageHelper::check_pending_read_finish
    StorageHelper::get_stats_file_auto_open
    StorageHelper::on_auto_open_one_file_finished
    StorageHelper::on_read_queue_finished
    StorageHelper::on_timer_files_open_auto
    StorageHelper::schedule_timer_once
    StorageHelperDelegate::StorageHelperDelegate

    CFileEntry::file_open
    CFileEntry::file_open_readonly
    CFileEntry::file_open_writable
    CFileEntry::file_close
    CFileEntry::file_is_open
    CFileEntry::file_is_readonly
    CFileEntry::file_set_readonly
    CFileEntry::file_read
    CFileEntry::file_write
    CFileEntry::file_flush
    CFileEntry::file_fast_allocate
    CFileEntry::file_finish_check
    CFileEntry::GetLastWriteTime
    CFileEntry::disk_allocation_rate
    CFileEntry::disk_allocation_rate_cs
    CFileEntry::complete_percent
    CFileEntry::complete_permillage
    CFileEntry::files_length_auto_correct
    CFileEntry::files_name_auto_correct
    CFileEntry::get_file_extension
    CFileEntry::get_file_path_name
    CFileEntry::get_file_path_name_with_extra_extensions
    CFileEntry::get_file_relative_path_name
    CFileEntry::is_download_completed

    FileInfoVector::init
    FileInfoVector::set_file_priority

    PieceManage::file_error_check
    PieceManage::files_change_check
    PieceManage::files_init
    PieceManage::files_init_and_auto_correct
    PieceManage::calculate_file_complete
    PieceManage::calculate_piece_required
    PieceManage::disk_read
    PieceManage::disk_write

设计核心:
1. StorageHelper 管理多文件任务的文件句柄池
2. 自动开启调度 (on_timer_files_open_auto):
   - 系统级 fd 限制 (默认 1024), 不能一次打开所有文件
   - 按需打开: 用户读取/写入某文件时才 open
   - LRU 关闭: 长时间未用的文件关闭
3. file_fast_allocate: 文件预分配 (ftruncate + sparse)
4. files_init_and_auto_correct: 启动时检测文件大小, 自动纠正
5. file_set_readonly: 完成下载后文件设为只读 (防误修改)
6. schedule_timer_once: 单次定时器 (异步任务)

加速价值 (针对 qBittorrent):
- qBittorrent 用 libtorrent 内置 file_storage, 不可定制
- BitComet 实现:
  a) fd 池管理 (避免 fd 用尽)
  b) 文件预分配 (减少碎片)
  c) 完成后只读 (防误修改)
  d) 文件名/大小自动纠正 (跨平台兼容)

本模块实现:
- FileEntry: 单文件 entry (含 fd 管理)
- StorageHelper: 文件池 + 自动开启调度
- StorageHelperDelegate: 抽象回调

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import logging
import os
import threading
import time
from collections import OrderedDict
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Callable, Dict, List, Optional, Tuple

LOG = logging.getLogger("storage")


# -----------------------------------------------------------------------------
# 枚举
# -----------------------------------------------------------------------------

class FileOpenMode(IntEnum):
    """对应 CFileEntry::file_open_*."""
    READONLY = 0
    WRITABLE = 1
    READWRITE = 2


class FileAllocateStrategy(IntEnum):
    """file_fast_allocate 策略."""
    NONE = 0                  # 不预分配
    SPARSE = 1                # ftruncate 创建 sparse file
    ZERO_FILL = 2              # 写 0 (慢但无碎片)
    AUTO = 3                   # 自动选择


# -----------------------------------------------------------------------------
# FileEntry — 单文件 entry
# -----------------------------------------------------------------------------

@dataclass
class FileEntry:
    """对应 Core_BitTorrent::CFileEntry."""
    file_path: str                  # 绝对路径
    relative_path: str              # 相对路径 (用于 torrent)
    size: int = 0                    # 期望大小
    actual_size: int = 0             # 实际大小 (磁盘上)
    # fd 管理
    _fd: Optional[int] = None       # 内部用
    _fh = None                      # file handle
    open_mode: Optional[FileOpenMode] = None
    is_readonly: bool = False
    # 元信息
    last_write_time: float = 0.0
    last_access_time: float = 0.0
    # 完成度
    bytes_completed: int = 0
    is_download_completed: bool = False
    # 磁盘分配速率
    disk_allocation_rate: float = 0.0    # bytes/sec
    # 错误
    last_error: Optional[str] = None

    # ----- 文件操作 -----

    def file_open(self, mode: FileOpenMode = FileOpenMode.READWRITE) -> bool:
        """对应 file_open."""
        if self._fh is not None:
            return True  # 已打开
        try:
            mode_str = {
                FileOpenMode.READONLY: "rb",
                FileOpenMode.WRITABLE: "wb",
                FileOpenMode.READWRITE: "r+b" if os.path.exists(self.file_path) else "w+b",
            }[mode]
            self._fh = open(self.file_path, mode_str)
            self.open_mode = mode
            self.last_access_time = time.time()
            return True
        except IOError as e:
            self.last_error = str(e)
            LOG.error(f"open {self.file_path} failed: {e}")
            return False

    def file_open_readonly(self) -> bool:
        """对应 file_open_readonly."""
        return self.file_open(FileOpenMode.READONLY)

    def file_open_writable(self) -> bool:
        """对应 file_open_writable."""
        return self.file_open(FileOpenMode.WRITABLE)

    def file_close(self) -> None:
        """对应 file_close."""
        if self._fh:
            self._fh.close()
            self._fh = None
            self.open_mode = None

    def file_is_open(self) -> bool:
        """对应 file_is_open."""
        return self._fh is not None

    def file_read(self, offset: int, length: int) -> Optional[bytes]:
        """对应 file_read."""
        if not self._fh:
            if not self.file_open(FileOpenMode.READWRITE):
                return None
        try:
            self._fh.seek(offset)
            data = self._fh.read(length)
            self.last_access_time = time.time()
            return data
        except IOError as e:
            self.last_error = str(e)
            return None

    def file_write(self, offset: int, data: bytes) -> bool:
        """对应 file_write."""
        if not self._fh:
            if not self.file_open(FileOpenMode.READWRITE):
                return False
        try:
            self._fh.seek(offset)
            self._fh.write(data)
            self.last_access_time = time.time()
            self.bytes_completed += len(data)
            return True
        except IOError as e:
            self.last_error = str(e)
            return False

    def file_flush(self) -> None:
        """对应 file_flush."""
        if self._fh:
            self._fh.flush()
            try:
                os.fsync(self._fh.fileno())
            except (OSError, AttributeError):
                pass

    def file_set_readonly(self) -> bool:
        """对应 file_set_readonly."""
        self.file_close()
        try:
            # chmod 444
            os.chmod(self.file_path, 0o444)
            self.is_readonly = True
            return True
        except OSError as e:
            self.last_error = str(e)
            return False

    def file_fast_allocate(self, strategy: FileAllocateStrategy = FileAllocateStrategy.AUTO) -> bool:
        """对应 file_fast_allocate.

        预分配文件大小, 减少碎片.
        """
        if self.size == 0:
            return True
        try:
            if strategy == FileAllocateStrategy.AUTO:
                # 小文件 (< 100MB) 用 ZERO_FILL, 大文件用 SPARSE
                strategy = (FileAllocateStrategy.ZERO_FILL if self.size < 100 * 1024 * 1024
                            else FileAllocateStrategy.SPARSE)
            if strategy == FileAllocateStrategy.SPARSE:
                # ftruncate 创建 sparse file
                with open(self.file_path, "wb") as f:
                    f.truncate(self.size)
            elif strategy == FileAllocateStrategy.ZERO_FILL:
                # 写 0 (慢但无碎片)
                with open(self.file_path, "wb") as f:
                    # 4KB 块写 0
                    chunk = b"\x00" * 4096
                    remaining = self.size
                    start = time.monotonic()
                    while remaining > 0:
                        write_size = min(4096, remaining)
                        f.write(chunk[:write_size])
                        remaining -= write_size
                    elapsed = time.monotonic() - start
                    self.disk_allocation_rate = self.size / max(elapsed, 0.001)
            self.actual_size = self.size
            return True
        except IOError as e:
            self.last_error = str(e)
            return False

    def file_finish_check(self) -> bool:
        """对应 file_finish_check - 检查文件是否完成."""
        if not os.path.exists(self.file_path):
            return False
        self.actual_size = os.path.getsize(self.file_path)
        if self.actual_size != self.size:
            return False
        self.is_download_completed = True
        return True

    def get_last_write_time(self) -> float:
        """对应 GetLastWriteTime."""
        try:
            stat = os.stat(self.file_path)
            self.last_write_time = stat.st_mtime
            return self.last_write_time
        except OSError:
            return 0.0

    def complete_percent(self) -> float:
        """对应 complete_percent."""
        if self.size == 0:
            return 100.0
        return (self.bytes_completed / self.size) * 100.0

    def complete_permillage(self) -> int:
        """对应 complete_permillage."""
        if self.size == 0:
            return 1000
        return int((self.bytes_completed / self.size) * 1000)

    def get_file_extension(self) -> str:
        """对应 get_file_extension."""
        _, ext = os.path.splitext(self.relative_path)
        return ext.lower()

    def get_file_relative_path_name(self) -> str:
        """对应 get_file_relative_path_name."""
        return self.relative_path

    @staticmethod
    def files_length_auto_correct(files: List["FileEntry"]) -> int:
        """对应 files_length_auto_correct - 纠正文件大小 (跨平台).

        Returns: 修正的文件数
        """
        corrected = 0
        for f in files:
            if os.path.exists(f.file_path):
                actual = os.path.getsize(f.file_path)
                if actual != f.actual_size:
                    f.actual_size = actual
                    corrected += 1
        return corrected

    @staticmethod
    def files_name_auto_correct(files: List["FileEntry"]) -> int:
        """对应 files_name_auto_correct - 纠正文件名 (Windows 非法字符)."""
        corrected = 0
        illegal_chars = '<>:"/\\|?*' if os.name == "nt" else ""
        for f in files:
            new_name = f.relative_path
            for c in illegal_chars:
                new_name = new_name.replace(c, "_")
            if new_name != f.relative_path:
                f.relative_path = new_name
                # 也更新 file_path
                dir_name = os.path.dirname(f.file_path)
                f.file_path = os.path.join(dir_name, new_name)
                corrected += 1
        return corrected


# -----------------------------------------------------------------------------
# FileInfoVector — 文件列表管理
# -----------------------------------------------------------------------------

class FileInfoVector:
    """对应 Core_BitTorrent::FileInfoVector."""

    def __init__(self):
        self._files: List[FileEntry] = []

    def init(self, files: List[FileEntry]) -> None:
        """对应 init."""
        self._files = list(files)

    def set_file_priority(self, file_index: int, priority: int) -> None:
        """对应 set_file_priority (简化, FileEntry 不存 priority, 跳过)."""
        pass

    def get_files(self) -> List[FileEntry]:
        return list(self._files)

    def get_file(self, index: int) -> Optional[FileEntry]:
        if 0 <= index < len(self._files):
            return self._files[index]
        return None

    def size(self) -> int:
        return len(self._files)

    def total_size(self) -> int:
        return sum(f.size for f in self._files)

    def total_completed(self) -> int:
        return sum(f.bytes_completed for f in self._files)


# -----------------------------------------------------------------------------
# StorageHelperDelegate — 抽象回调
# -----------------------------------------------------------------------------

class StorageHelperDelegate:
    """对应 Core_BitTorrent::StorageHelperDelegate."""

    def on_file_opened(self, file_entry: FileEntry) -> None:
        """文件被打开."""
        pass

    def on_file_closed(self, file_entry: FileEntry) -> None:
        """文件被关闭."""
        pass

    def on_file_completed(self, file_entry: FileEntry) -> None:
        """文件下载完成."""
        pass

    def on_file_error(self, file_entry: FileEntry, error: str) -> None:
        """文件 IO 错误."""
        pass


# -----------------------------------------------------------------------------
# StorageHelper — 主存储管理器
# -----------------------------------------------------------------------------

class StorageHelper:
    """对应 Core_BitTorrent::StorageHelper.

    功能:
    1. 多文件 fd 池管理 (LRU)
    2. 自动开启调度 (按需 open)
    3. 完成检测 + 自动只读
    4. 文件预分配
    5. 文件大小/名称自动纠正
    """

    def __init__(self, file_info_vector: FileInfoVector,
                 max_open_files: int = 256,
                 delegate: Optional[StorageHelperDelegate] = None):
        self.files = file_info_vector
        self.max_open_files = max_open_files
        self.delegate = delegate
        # LRU 缓存: file_index → last_access_time
        self._open_files: "OrderedDict[int, FileEntry]" = OrderedDict()
        self._lock = threading.RLock()
        # 状态
        self.is_running = False
        # 自动开启定时器
        self._auto_open_thread: Optional[threading.Thread] = None
        self._stop_event = threading.Event()
        # 待读取队列 (read queue)
        self._pending_reads: List[Tuple[int, int, int]] = []  # (file_idx, offset, length)
        # 统计
        self.stats = {
            "files_opened": 0,
            "files_closed": 0,
            "files_completed": 0,
            "auto_open_events": 0,
            "read_queue_size": 0,
            "fd_pool_evictions": 0,
        }

    # ----- 生命周期 -----

    def start(self) -> None:
        """对应 start."""
        self.is_running = True
        # 启动自动开启定时器
        self._auto_open_thread = threading.Thread(
            target=self._auto_open_loop, daemon=True, name="StorageHelper"
        )
        self._auto_open_thread.start()
        LOG.info(f"StorageHelper started, max_open_files={self.max_open_files}")

    def stop(self) -> None:
        """对应 stop."""
        self.is_running = False
        self._stop_event.set()
        if self._auto_open_thread:
            self._auto_open_thread.join(timeout=2)
        # 关闭所有文件
        with self._lock:
            for entry in self._open_files.values():
                entry.file_close()
            self._open_files.clear()

    # ----- 文件操作 -----

    def disk_read(self, file_index: int, offset: int, length: int) -> Optional[bytes]:
        """对应 PieceManage::disk_read."""
        entry = self._get_or_open_file(file_index)
        if entry is None:
            return None
        data = entry.file_read(offset, length)
        # 更新 LRU
        with self._lock:
            self._open_files.move_to_end(file_index)
        return data

    def disk_write(self, file_index: int, offset: int, data: bytes) -> bool:
        """对应 PieceManage::disk_write."""
        entry = self._get_or_open_file(file_index)
        if entry is None:
            return False
        ok = entry.file_write(offset, data)
        if ok:
            # 检查是否完成
            if entry.bytes_completed >= entry.size and not entry.is_download_completed:
                entry.is_download_completed = True
                self.stats["files_completed"] += 1
                if self.delegate:
                    self.delegate.on_file_completed(entry)
        # 更新 LRU
        with self._lock:
            self._open_files.move_to_end(file_index)
        return ok

    def _get_or_open_file(self, file_index: int) -> Optional[FileEntry]:
        """对应 on_auto_open_one_file_finished - 按需打开文件."""
        with self._lock:
            if file_index in self._open_files:
                return self._open_files[file_index]
            # 检查 fd 池是否满
            while len(self._open_files) >= self.max_open_files:
                # LRU 关闭最久未用的
                _, oldest = self._open_files.popitem(last=False)
                oldest.file_close()
                self.stats["files_closed"] += 1
                self.stats["fd_pool_evictions"] += 1
                if self.delegate:
                    self.delegate.on_file_closed(oldest)
            # 打开新文件
            entry = self.files.get_file(file_index)
            if entry is None:
                return None
            if not entry.file_open(FileOpenMode.READWRITE):
                if self.delegate:
                    self.delegate.on_file_error(entry, entry.last_error or "open failed")
                return None
            self._open_files[file_index] = entry
            self.stats["files_opened"] += 1
            if self.delegate:
                self.delegate.on_file_opened(entry)
            return entry

    # ----- 自动开启定时器 -----

    def _auto_open_loop(self) -> None:
        """对应 on_timer_files_open_auto."""
        while not self._stop_event.wait(1.0):
            if not self.is_running:
                break
            self._auto_open_next_files()

    def _auto_open_next_files(self) -> int:
        """预打开接下来可能用到的文件."""
        # 简化: 找出未打开的前 N 个文件
        opened = 0
        with self._lock:
            current_count = len(self._open_files)
            slots = self.max_open_files - current_count
            if slots <= 0:
                return 0
        for i in range(self.files.size()):
            if i not in self._open_files:
                entry = self.files.get_file(i)
                if entry is None:
                    continue
                if not os.path.exists(entry.file_path):
                    continue  # 还没创建
                # 打开
                if entry.file_open(FileOpenMode.READWRITE):
                    with self._lock:
                        self._open_files[i] = entry
                    self.stats["files_opened"] += 1
                    self.stats["auto_open_events"] += 1
                    opened += 1
                    if opened >= slots:
                        break
        return opened

    def schedule_timer_once(self, delay_sec: float,
                              callback: Callable[[], None]) -> None:
        """对应 schedule_timer_once - 单次定时器."""
        def _run():
            self._stop_event.wait(delay_sec)
            if not self._stop_event.is_set():
                try:
                    callback()
                except Exception as e:
                    LOG.error(f"timer callback failed: {e}")
        threading.Thread(target=_run, daemon=True).start()

    # ----- 队列 -----

    def check_pending_read_finish(self) -> int:
        """对应 check_pending_read_finish - 检查待读取队列完成情况."""
        # 简化: 返回剩余数
        self.stats["read_queue_size"] = len(self._pending_reads)
        return len(self._pending_reads)

    def on_read_queue_finished(self) -> None:
        """对应 on_read_queue_finished."""
        self._pending_reads.clear()
        self.stats["read_queue_size"] = 0

    def get_stats_file_auto_open(self) -> Dict:
        """对应 get_stats_file_auto_open."""
        return {
            "open_files": len(self._open_files),
            "max_open_files": self.max_open_files,
            "total_files": self.files.size(),
            "auto_open_events": self.stats["auto_open_events"],
            "fd_pool_evictions": self.stats["fd_pool_evictions"],
        }

    def on_auto_open_one_file_finished(self, file_index: int, success: bool) -> None:
        """对应 on_auto_open_one_file_finished."""
        if success:
            self.stats["auto_open_events"] += 1

    # ----- 文件检查 -----

    def files_init(self) -> None:
        """对应 PieceManage::files_init."""
        for entry in self.files.get_files():
            # 确保目录存在
            dir_name = os.path.dirname(entry.file_path)
            if dir_name and not os.path.exists(dir_name):
                os.makedirs(dir_name, exist_ok=True)
            # 检查文件是否存在
            if not os.path.exists(entry.file_path):
                # 预分配
                entry.file_fast_allocate(FileAllocateStrategy.AUTO)

    def files_init_and_auto_correct(self) -> int:
        """对应 files_init_and_auto_correct."""
        self.files_init()
        files = self.files.get_files()
        # 大小纠正
        corrected = FileEntry.files_length_auto_correct(files)
        # 名称纠正
        corrected += FileEntry.files_name_auto_correct(files)
        return corrected

    def files_change_check(self) -> List[int]:
        """对应 files_change_check - 检测外部修改的文件."""
        changed = []
        for i, entry in enumerate(self.files.get_files()):
            if not os.path.exists(entry.file_path):
                continue
            current_mtime = os.path.getmtime(entry.file_path)
            if current_mtime != entry.last_write_time:
                changed.append(i)
                entry.last_write_time = current_mtime
        return changed

    def file_error_check(self, file_index: int) -> Optional[str]:
        """对应 PieceManage::file_error_check."""
        entry = self.files.get_file(file_index)
        return entry.last_error if entry else None

    # ----- 完成度计算 -----

    def calculate_file_complete(self, file_index: int) -> float:
        """对应 calculate_file_complete."""
        entry = self.files.get_file(file_index)
        return entry.complete_percent() if entry else 0.0

    def calculate_piece_required(self, piece_index: int,
                                  piece_size: int, total_pieces: int) -> bool:
        """对应 calculate_piece_required - 该 piece 是否需要下载."""
        # 简化: 总是 True
        return True

    def get_stats(self) -> Dict:
        s = dict(self.stats)
        s["file_auto_open_stats"] = self.get_stats_file_auto_open()
        s["total_files"] = self.files.size()
        s["total_size"] = self.files.total_size()
        s["total_completed"] = self.files.total_completed()
        return s


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    import tempfile
    print("=" * 60)
    print("BitComet storage helper demo")
    print("=" * 60)
    # 创建临时目录 + 文件
    with tempfile.TemporaryDirectory() as tmpdir:
        files = []
        for i in range(5):
            path = os.path.join(tmpdir, f"file_{i}.bin")
            entry = FileEntry(
                file_path=path,
                relative_path=f"file_{i}.bin",
                size=1024 * 1024,  # 1 MiB
            )
            files.append(entry)
        # FileInfoVector
        fiv = FileInfoVector()
        fiv.init(files)
        # StorageHelper
        helper = StorageHelper(fiv, max_open_files=3)
        helper.files_init_and_auto_correct()
        helper.start()
        try:
            # 写入数据
            print("\n[1] 写入数据到 file 0, 1, 2 (超过 max_open_files=3)")
            for i in range(3):
                data = os.urandom(4096)
                ok = helper.disk_write(i, 0, data)
                print(f"  file {i}: write ok={ok}")
            # 再写一个, 触发 LRU 关闭
            print("\n[2] 写入 file 3, 应触发 LRU 关闭 file 0")
            ok = helper.disk_write(3, 0, os.urandom(4096))
            print(f"  file 3: write ok={ok}")
            # 读取
            print("\n[3] 读取 file 0 数据 (会被重新打开)")
            data = helper.disk_read(0, 0, 4096)
            print(f"  file 0: read {len(data) if data else 0} bytes")
            # 检查完成
            print("\n[4] 文件完成度")
            for i in range(5):
                pct = helper.calculate_file_complete(i)
                print(f"  file {i}: {pct:.1f}%")
            # 统计
            print("\n=== Stats ===")
            for k, v in helper.get_stats().items():
                if isinstance(v, dict):
                    print(f"  {k}:")
                    for k2, v2 in v.items():
                        print(f"    {k2}: {v2}")
                else:
                    print(f"  {k}: {v}")
        finally:
            helper.stop()
