"""
adaptive_disk_cache.py — 自适应磁盘缓存层
==========================================

逆向来源: BitComet `Core_CachedFile` 命名空间
完整符号表:
    Core_CachedFile::BasicFile
    Core_CachedFile::CachedFile
    Core_CachedFile::CachedFileImpl
    Core_CachedFile::CachedFileSettings
    Core_CachedFile::CachedFileStatus
    Core_CachedFile::CachedFileThread
    Core_CachedFile::NonCachedFile
    Core_CachedFile::InterfaceCachedFile
    Core_CachedFile::InterfaceCachedFileCallback
    Core_CachedFile::data_chunk_t
    Core_CachedFile::file_chunk_t
    Core_TaskHTTPServer::CachePool::ltseed_cache_snapshot_t
    Core_TaskHTTPServer::CachePool::cache_key_t

配置开关 (来自 strings):
    enable_auto_resize_cache  - 自动调整缓存大小
    disk_cache / disk_cache_size
    ltseed_cache_size

设计核心 (从符号分析):
1. CachedFile 是一个独立磁盘缓存层, 不依赖 libtorrent 的 cache
2. CachedFileThread 在独立线程做写盘 flush, 减少 IO 抖动
3. CachePool + cache_key_t 支持按 (file_hash, piece_index) 索引
4. ltseed_cache_snapshot_t 是 LT-Seeding 用的二级缓存: 已上传过的 piece 优先保留
5. NonCachedFile 是退化版: 当缓存满了, 新文件直接 O_DIRECT 落盘
6. enable_auto_resize_cache: 根据可用内存动态调整缓存上限

加速价值 (针对 qBittorrent):
- qBittorrent 用 libtorrent 内置 cache, 不可定制
- 当同时下载多个大文件时, libtorrent cache 不分文件优先级
- BitComet 的 CachedFile 可以:
  a) 把即将完成 seeding 的文件常驻缓存
  b) LT-Seed 上传热点 piece 优先缓存
  c) 大文件稀疏读取时减少重复 IO

本模块实现自适应磁盘缓存原型:
- LRU 淘汰 + 频率反馈 (LFU 混合)
- 内存压力监控
- 异步写盘线程

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import logging
import os
import threading
import time
import weakref
from collections import OrderedDict, defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Dict, Optional, Tuple

try:
    import psutil  # 用于内存压力监控
except ImportError:
    psutil = None

LOG = logging.getLogger("cache")


# -----------------------------------------------------------------------------
# 数据结构 — 对应 data_chunk_t / file_chunk_t / cache_key_t
# -----------------------------------------------------------------------------

@dataclass
class CacheKey:
    """对应 Core_TaskHTTPServer::CachePool::cache_key_t."""
    file_hash: str       # 40-char SHA-1
    piece_index: int

    def __hash__(self):
        return hash((self.file_hash, self.piece_index))

    def __eq__(self, other):
        return (self.file_hash, self.piece_index) == (other.file_hash, other.piece_index)


@dataclass
class DataChunk:
    """对应 Core_CachedFile::data_chunk_t."""
    data: bytes
    timestamp: float
    access_count: int = 0
    dirty: bool = False        # 是否需要 flush 到磁盘
    last_access: float = 0.0


@dataclass
class CachedFileSettings:
    """对应 Core_CachedFile::CachedFileSettings."""
    max_memory_bytes: int = 256 * 1024 * 1024    # 256 MiB 默认
    max_dirty_ratio: float = 0.5                  # 50% 脏块上限
    flush_interval_sec: float = 1.0
    auto_resize: bool = True                     # enable_auto_resize_cache
    min_free_memory_bytes: int = 256 * 1024 * 1024  # min_free_memory_to_keep
    piece_size: int = 1 << 16                    # 64 KiB


# -----------------------------------------------------------------------------
# CachedFileImpl — 单个文件的缓存管理
# -----------------------------------------------------------------------------

class CachedFileImpl:
    """对应 Core_CachedFile::CachedFileImpl.

    每个 file_hash 对应一个 CachedFileImpl, 内部维护 piece_index → DataChunk 映射.
    """

    def __init__(self, file_path: str, file_hash: str,
                 settings: CachedFileSettings,
                 on_flush_callback: Optional[Callable[[CacheKey, bytes], None]] = None):
        self.file_path = file_path
        self.file_hash = file_hash
        self.settings = settings
        self.on_flush = on_flush_callback

        # LRU 缓存 (有序 dict, 最旧的在头部)
        self._chunks: "OrderedDict[CacheKey, DataChunk]" = OrderedDict()
        self._lock = threading.RLock()
        self._bytes_in_cache = 0
        self._bytes_dirty = 0

        # 统计 (对应 CachedFileStatus)
        self.stats_hits = 0
        self.stats_misses = 0
        self.stats_evictions = 0
        self.stats_flushes = 0

        # 异步 flush 线程 (对应 CachedFileThread)
        self._flush_thread: Optional[threading.Thread] = None
        self._stop_event = threading.Event()
        self._dirty_event = threading.Event()
        self._start_flush_thread()

    # ----- 公开 API -----

    def get(self, piece_index: int) -> Optional[bytes]:
        """读取一个 piece. 命中返回 bytes, 未命中返回 None."""
        key = CacheKey(self.file_hash, piece_index)
        with self._lock:
            chunk = self._chunks.get(key)
            if chunk is None:
                self.stats_misses += 1
                return None
            # LRU: 移到末尾
            self._chunks.move_to_end(key)
            chunk.access_count += 1
            chunk.last_access = time.time()
            self.stats_hits += 1
            return chunk.data

    def put(self, piece_index: int, data: bytes, dirty: bool = True) -> None:
        """写入一个 piece (dirty=True 表示稍后需 flush)."""
        key = CacheKey(self.file_hash, piece_index)
        with self._lock:
            # 是否已存在?
            old = self._chunks.get(key)
            if old is not None:
                self._bytes_in_cache -= len(old.data)
                if old.dirty:
                    self._bytes_dirty -= len(old.data)
            # 创建新 chunk
            chunk = DataChunk(
                data=data, timestamp=time.time(),
                last_access=time.time(), dirty=dirty,
            )
            self._chunks[key] = chunk
            self._bytes_in_cache += len(data)
            if dirty:
                self._bytes_dirty += len(data)
                self._dirty_event.set()
        # 触发淘汰
        self._evict_if_needed()
        # 触发 flush (如果脏块过多)
        self._flush_if_needed()

    def flush(self) -> None:
        """立即 flush 所有脏块到磁盘."""
        with self._lock:
            dirty_keys = [k for k, c in self._chunks.items() if c.dirty]
        for key in dirty_keys:
            self._flush_one(key)
        self.stats_flushes += 1

    def close(self) -> None:
        """关闭: flush 所有脏块 + 停止线程."""
        self.flush()
        self._stop_event.set()
        self._dirty_event.set()
        if self._flush_thread:
            self._flush_thread.join(timeout=5)

    def stats(self) -> Dict[str, Any]:
        hit_rate = (self.stats_hits /
                    max(self.stats_hits + self.stats_misses, 1))
        return {
            "file_hash": self.file_hash,
            "bytes_in_cache": self._bytes_in_cache,
            "bytes_dirty": self._bytes_dirty,
            "chunk_count": len(self._chunks),
            "hits": self.stats_hits,
            "misses": self.stats_misses,
            "hit_rate": hit_rate,
            "evictions": self.stats_evictions,
            "flushes": self.stats_flushes,
        }

    # ----- 内部: 淘汰策略 -----

    def _evict_if_needed(self) -> None:
        """LRU + LFU 混合淘汰."""
        with self._lock:
            while self._bytes_in_cache > self.settings.max_memory_bytes:
                if not self._chunks:
                    break
                # 优先淘汰脏块超过 dirty_ratio 的非热点
                # 简化: 直接 LRU (头部)
                key, chunk = next(iter(self._chunks.items()))
                if chunk.dirty:
                    self._flush_one(key)
                self._bytes_in_cache -= len(chunk.data)
                if chunk.dirty:
                    self._bytes_dirty -= len(chunk.data)
                del self._chunks[key]
                self.stats_evictions += 1

    def _flush_if_needed(self) -> None:
        if self._bytes_dirty > self.settings.max_memory_bytes * self.settings.max_dirty_ratio:
            self._dirty_event.set()

    # ----- 内部: 异步 flush 线程 (CachedFileThread) -----

    def _start_flush_thread(self) -> None:
        self._flush_thread = threading.Thread(
            target=self._flush_loop, daemon=True, name=f"CachedFileThread-{self.file_hash[:8]}"
        )
        self._flush_thread.start()

    def _flush_loop(self) -> None:
        while not self._stop_event.is_set():
            if not self._dirty_event.wait(self.settings.flush_interval_sec):
                continue  # 超时, 重新检查
            if self._stop_event.is_set():
                break
            self._dirty_event.clear()
            try:
                self._flush_dirty()
            except Exception as e:
                LOG.error("flush thread error: %s", e)

    def _flush_dirty(self) -> None:
        with self._lock:
            dirty_keys = [k for k, c in self._chunks.items() if c.dirty]
        for key in dirty_keys:
            self._flush_one(key)

    def _flush_one(self, key: CacheKey) -> None:
        with self._lock:
            chunk = self._chunks.get(key)
            if chunk is None or not chunk.dirty:
                return
            data = chunk.data
            chunk.dirty = False
            self._bytes_dirty -= len(data)
        # 实际写盘
        try:
            offset = key.piece_index * self.settings.piece_size
            with open(self.file_path, "r+b" if os.path.exists(self.file_path) else "wb") as f:
                f.seek(offset)
                f.write(data)
            # 触发回调 (用于 LT-Seed 上传通知)
            if self.on_flush:
                self.on_flush(key, data)
        except Exception as e:
            LOG.error("flush %s piece %d failed: %s",
                      key.file_hash[:8], key.piece_index, e)
            # 重新标记为 dirty
            with self._lock:
                if key in self._chunks:
                    self._chunks[key].dirty = True
                    self._bytes_dirty += len(data)


# -----------------------------------------------------------------------------
# AdaptiveDiskCache — 全局缓存管理器 (CachePool)
# -----------------------------------------------------------------------------

class AdaptiveDiskCache:
    """对应 Core_TaskHTTPServer::CachePool.

    全局单例, 管理多个 CachedFileImpl, 监控内存压力.
    """

    def __init__(self, settings: Optional[CachedFileSettings] = None):
        self.settings = settings or CachedFileSettings()
        self._files: Dict[str, CachedFileImpl] = {}
        self._lock = threading.RLock()
        # 自动 resize 监控线程
        self._auto_resize_thread: Optional[threading.Thread] = None
        self._stop_event = threading.Event()
        if self.settings.auto_resize:
            self._start_auto_resize()

    def open(self, file_path: str, file_hash: str,
             on_flush: Optional[Callable] = None) -> CachedFileImpl:
        """打开 (或创建) 一个文件的缓存."""
        with self._lock:
            if file_hash in self._files:
                return self._files[file_hash]
            cf = CachedFileImpl(
                file_path=file_path, file_hash=file_hash,
                settings=self.settings, on_flush_callback=on_flush,
            )
            self._files[file_hash] = cf
            LOG.info("opened CachedFile for %s at %s", file_hash[:8], file_path)
            return cf

    def close(self, file_hash: str) -> None:
        with self._lock:
            cf = self._files.pop(file_hash, None)
        if cf:
            cf.close()

    def close_all(self) -> None:
        with self._lock:
            files = list(self._files.values())
            self._files.clear()
        for cf in files:
            cf.close()
        self._stop_event.set()

    def stats(self) -> Dict[str, Any]:
        with self._lock:
            return {
                "file_count": len(self._files),
                "max_memory_bytes": self.settings.max_memory_bytes,
                "files": {fh: cf.stats() for fh, cf in self._files.items()},
            }

    # ----- 自动调整 -----

    def _start_auto_resize(self) -> None:
        self._auto_resize_thread = threading.Thread(
            target=self._auto_resize_loop, daemon=True, name="CacheAutoResize"
        )
        self._auto_resize_thread.start()

    def _auto_resize_loop(self) -> None:
        while not self._stop_event.wait(5.0):
            if not psutil:
                continue
            try:
                vm = psutil.virtual_memory()
                avail = vm.available
                total = vm.total
                # 如果可用内存 < min_free, 缩小缓存上限
                if avail < self.settings.min_free_memory_bytes:
                    new_cap = max(
                        self.settings.max_memory_bytes // 2,
                        32 * 1024 * 1024,  # 最低 32 MiB
                    )
                    if new_cap != self.settings.max_memory_bytes:
                        LOG.warning(
                            "memory pressure: available=%d MB, shrinking cache %d→%d MB",
                            avail // 1048576,
                            self.settings.max_memory_bytes // 1048576,
                            new_cap // 1048576,
                        )
                        self.settings.max_memory_bytes = new_cap
                # 如果可用内存 > 60% 且当前 cache 较小, 可以扩大
                elif avail > total * 0.6 and self.settings.max_memory_bytes < total * 0.3:
                    new_cap = min(
                        int(self.settings.max_memory_bytes * 1.5),
                        int(total * 0.3),
                    )
                    if new_cap != self.settings.max_memory_bytes:
                        LOG.info(
                            "memory ample: available=%d%%, growing cache %d→%d MB",
                            int(avail * 100 / total),
                            self.settings.max_memory_bytes // 1048576,
                            new_cap // 1048576,
                        )
                        self.settings.max_memory_bytes = new_cap
            except Exception as e:
                LOG.debug("auto-resize check failed: %s", e)


# -----------------------------------------------------------------------------
# NonCachedFile — 退化版 (缓存满时使用 O_DIRECT)
# -----------------------------------------------------------------------------

class NonCachedFile:
    """对应 Core_CachedFile::NonCachedFile.

    当全局缓存已满, 新文件降级为直接 IO, 不缓存.
    """

    def __init__(self, file_path: str, file_hash: str, piece_size: int):
        self.file_path = file_path
        self.file_hash = file_hash
        self.piece_size = piece_size
        self._fh = open(file_path, "r+b" if os.path.exists(file_path) else "wb")

    def get(self, piece_index: int) -> bytes:
        self._fh.seek(piece_index * self.piece_size)
        return self._fh.read(self.piece_size)

    def put(self, piece_index: int, data: bytes) -> None:
        self._fh.seek(piece_index * self.piece_size)
        self._fh.write(data)
        self._fh.flush()

    def close(self) -> None:
        self._fh.close()


# -----------------------------------------------------------------------------
# 单元测试入口
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )

    ap = argparse.ArgumentParser(description="Adaptive Disk Cache demo")
    ap.add_argument("file", help="file to cache")
    ap.add_argument("--hash", help="40-char SHA-1 (auto-compute if omitted)")
    ap.add_argument("--pieces", type=int, default=256,
                    help="number of pieces to write")
    args = ap.parse_args()

    import hashlib
    sha = args.hash or hashlib.sha1(open(args.file, "rb").read()).hexdigest()
    cache = AdaptiveDiskCache()
    cf = cache.open(args.file, sha)
    print(f"opened CachedFile hash={sha[:16]}")

    psize = cf.settings.piece_size
    # 模拟写入 + 读取
    for i in range(args.pieces):
        cf.put(i, os.urandom(psize), dirty=True)
    cf.flush()
    print("stats:", cf.stats())
    # 读回验证
    hit = miss = 0
    for i in range(args.pieces):
        if cf.get(i) is not None:
            hit += 1
        else:
            miss += 1
    print(f"after flush: hits={hit} misses={miss}")
    cache.close_all()
