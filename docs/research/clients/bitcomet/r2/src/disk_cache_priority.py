"""
disk_cache_priority.py — BitComet CachePool piece 优先级算法
==========================================================

逆向来源: Core_TaskHTTPServer::CachePool + Core_CachedFile
关键符号:
    Core_TaskHTTPServer::CachePool::ltseed_cache_snapshot_t
    Core_TaskHTTPServer::CachePool::cache_key_t
    Core_CachedFile::CachedFileImpl
    Core_CachedFile::CachedFileSettings
    Core_CachedFile::CachedFileStatus
    Core_CachedFile::CachedFileThread
    Core_CachedFile::NonCachedFile
    Core_CachedFile::data_chunk_t
    Core_CachedFile::file_chunk_t

确认字符串:
    enable_auto_resize_cache
    disk_cache / disk_cache_size
    ltseed_cache_size
    min_free_memory_to_keep

设计核心 (从符号分析):
1. CachePool 不只做 LRU, 还有"作用域优先级"
2. ltseed_cache_snapshot_t: LT-Seed 上传热点 piece 优先保留
3. cache_key_t = (file_hash, piece_index), 但内部加了 priority 字段
4. CachedFileThread 异步 flush, 不阻塞主下载线程
5. enable_auto_resize_cache: 根据可用内存动态调整上限
6. NonCachedFile: 缓存满时降级 O_DIRECT, 不污染其他文件缓存

加速价值 (针对 qBittorrent):
- qBittorrent 用 libtorrent 内置 cache, LRU 单一策略
- 大文件场景: LT-Seed 上传 piece 被新下载的 piece 挤出, 重新读盘
- BitComet 分桶策略:
  a) piece_priority = DOWNLOAD_HOT (正在被请求)
  b) piece_priority = LT_SEED_HOT (正在被 LT-Seed 上传)
  c) piece_priority = NORMAL (普通 piece)
  d) piece_priority = COLD (老 piece)
- 淘汰顺序: COLD > NORMAL > LT_SEED_HOT > DOWNLOAD_HOT

本模块实现:
- PriorityCacheKey: 4 级优先级
- PriorityDiskCache: 分优先级的 LRU 缓存
- LTSeedHotTracker: 跟踪 LT-Seed 上传热点 piece
- MemoryPressureController: 自动 resize 上限

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import logging
import os
import threading
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Callable, Deque, Dict, List, Optional, Set, Tuple

try:
    import psutil
except ImportError:
    psutil = None

LOG = logging.getLogger("cache_pri")


# -----------------------------------------------------------------------------
# 优先级
# -----------------------------------------------------------------------------

class PiecePriority(IntEnum):
    """对应 CachePool 内部的 piece 优先级 (从符号反推).

    优先级越高, 越不容易被淘汰.
    """
    COLD = 0          # 老 piece, 已下载很久没人访问
    NORMAL = 1         # 普通 piece, 默认
    LT_SEED_HOT = 2    # 正在被 LT-Seed 上传的 piece
    DOWNLOAD_HOT = 3   # 正在被多个 peer 请求的 piece


@dataclass
class PriorityCacheKey:
    """对应 Core_TaskHTTPServer::CachePool::cache_key_t.

    结构: file_hash(20) + piece_index(4) + priority(1) + reserved(3)
    """
    file_hash: str           # 40-char SHA-1 hex
    piece_index: int
    priority: PiecePriority = PiecePriority.NORMAL

    def __hash__(self):
        return hash((self.file_hash, self.piece_index))

    def __eq__(self, other):
        return (self.file_hash, self.piece_index) == (other.file_hash, other.piece_index)


@dataclass
class PriorityChunk:
    """对应 data_chunk_t (含优先级)."""
    data: bytes
    timestamp: float = field(default_factory=time.time)
    last_access: float = field(default_factory=time.time)
    access_count: int = 0
    dirty: bool = False
    priority: PiecePriority = PiecePriority.NORMAL
    # LT-Seed 跟踪
    ltseed_upload_count: int = 0
    ltseed_last_upload: float = 0.0


@dataclass
class PriorityCacheSettings:
    """对应 CachedFileSettings + 全局策略."""
    max_memory_bytes: int = 512 * 1024 * 1024
    auto_resize: bool = True
    min_free_memory_bytes: int = 512 * 1024 * 1024
    # 各优先级的预留比例 (总和 = 1.0)
    quota_cold: float = 0.20          # 20% 给 COLD
    quota_normal: float = 0.40        # 40% 给 NORMAL
    quota_lt_seed_hot: float = 0.25   # 25% 给 LT_SEED_HOT
    quota_download_hot: float = 0.15  # 15% 给 DOWNLOAD_HOT
    # 淘汰检查间隔
    eviction_check_interval_sec: float = 1.0


# -----------------------------------------------------------------------------
# LTSeedHotTracker — 跟踪 LT-Seed 上传热点
# -----------------------------------------------------------------------------

class LTSeedHotTracker:
    """对应 ltseed_cache_snapshot_t.

    维护每个 piece 在过去 N 秒内被 LT-Seed 上传的次数.
    超过阈值的 piece 升级为 LT_SEED_HOT.
    """

    def __init__(self, window_sec: int = 60, hot_threshold: int = 3):
        self.window_sec = window_sec
        self.hot_threshold = hot_threshold
        # (file_hash, piece_index) → [(timestamp, bytes_uploaded)]
        self._history: Dict[Tuple[str, int], Deque[Tuple[float, int]]] = defaultdict(
            lambda: deque(maxlen=100)
        )
        self._lock = threading.Lock()

    def record_upload(self, file_hash: str, piece_index: int,
                      bytes_uploaded: int) -> None:
        with self._lock:
            key = (file_hash, piece_index)
            self._history[key].append((time.time(), bytes_uploaded))
            # 清理过期
            cutoff = time.time() - self.window_sec
            while self._history[key] and self._history[key][0][0] < cutoff:
                self._history[key].popleft()

    def get_upload_count(self, file_hash: str, piece_index: int) -> int:
        with self._lock:
            key = (file_hash, piece_index)
            return len(self._history.get(key, []))

    def is_hot(self, file_hash: str, piece_index: int) -> bool:
        return self.get_upload_count(file_hash, piece_index) >= self.hot_threshold

    def get_hot_pieces(self) -> List[Tuple[str, int]]:
        """获取所有 hot piece 列表."""
        with self._lock:
            return [
                key for key, hist in self._history.items()
                if len(hist) >= self.hot_threshold
            ]

    def cleanup_stale(self) -> None:
        """清理长期没上传的 piece."""
        with self._lock:
            cutoff = time.time() - self.window_sec
            for key in list(self._history.keys()):
                while self._history[key] and self._history[key][0][0] < cutoff:
                    self._history[key].popleft()
                if not self._history[key]:
                    del self._history[key]


# -----------------------------------------------------------------------------
# PriorityDiskCache — 分优先级 LRU
# -----------------------------------------------------------------------------

class PriorityDiskCache:
    """对应 CachePool (4 优先级桶).

    每个优先级有独立的 LRU 队列, 淘汰时按优先级从低到高.
    """

    def __init__(self, settings: Optional[PriorityCacheSettings] = None,
                 on_flush: Optional[Callable[[PriorityCacheKey, bytes], None]] = None):
        self.settings = settings or PriorityCacheSettings()
        self.on_flush = on_flush
        self.ltseed_tracker = LTSeedHotTracker()
        # 4 个 LRU 桶 (按优先级分)
        self._buckets: Dict[PiecePriority, Dict[PriorityCacheKey, PriorityChunk]] = {
            p: {} for p in PiecePriority
        }
        self._lock = threading.RLock()
        self._bytes_total = 0
        self._bytes_dirty = 0
        # 异步 flush 线程
        self._flush_thread: Optional[threading.Thread] = None
        self._stop_event = threading.Event()
        self._dirty_event = threading.Event()
        self._start_flush_thread()
        # 自动 resize
        self._auto_resize_thread: Optional[threading.Thread] = None
        if self.settings.auto_resize:
            self._start_auto_resize()
        # 统计
        self.stats = {
            "hits": 0, "misses": 0, "evictions": 0,
            "promotions": 0, "demotions": 0, "flushes": 0,
            "max_memory_bytes": self.settings.max_memory_bytes,
        }

    # ----- 公开 API -----

    def get(self, file_hash: str, piece_index: int) -> Optional[bytes]:
        """读取 piece."""
        key = PriorityCacheKey(file_hash=file_hash, piece_index=piece_index)
        with self._lock:
            for prio in reversed(PiecePriority):
                chunk = self._buckets[prio].get(key)
                if chunk is not None:
                    chunk.last_access = time.time()
                    chunk.access_count += 1
                    self.stats["hits"] += 1
                    return chunk.data
            self.stats["misses"] += 1
            return None

    def put(self, file_hash: str, piece_index: int, data: bytes,
            dirty: bool = True,
            priority: PiecePriority = PiecePriority.NORMAL) -> None:
        """写入 piece.

        如果该 piece 被 LT-Seed 频繁上传, 自动升级到 LT_SEED_HOT.
        """
        # LT-Seed 热点检测
        if self.ltseed_tracker.is_hot(file_hash, piece_index):
            if priority < PiecePriority.LT_SEED_HOT:
                priority = PiecePriority.LT_SEED_HOT
                self.stats["promotions"] += 1

        key = PriorityCacheKey(file_hash=file_hash, piece_index=piece_index,
                                priority=priority)
        with self._lock:
            # 检查是否已存在 (可能在其他优先级桶)
            for prio in PiecePriority:
                if key in self._buckets[prio]:
                    old = self._buckets[prio].pop(key)
                    self._bytes_total -= len(old.data)
                    if old.dirty:
                        self._bytes_dirty -= len(old.data)
                    if prio != priority:
                        # 优先级变化
                        if priority > prio:
                            self.stats["promotions"] += 1
                        else:
                            self.stats["demotions"] += 1
                    break
            # 插入新 chunk
            chunk = PriorityChunk(
                data=data, dirty=dirty, priority=priority,
            )
            self._buckets[priority][key] = chunk
            self._bytes_total += len(data)
            if dirty:
                self._bytes_dirty += len(data)
                self._dirty_event.set()
        # 触发淘汰
        self._evict_if_needed()
        # 触发 flush
        self._flush_if_needed()

    def set_priority(self, file_hash: str, piece_index: int,
                      new_priority: PiecePriority) -> None:
        """手动调整 piece 优先级."""
        key = PriorityCacheKey(file_hash=file_hash, piece_index=piece_index)
        with self._lock:
            for prio in PiecePriority:
                chunk = self._buckets[prio].get(key)
                if chunk is not None and prio != new_priority:
                    del self._buckets[prio][key]
                    key.priority = new_priority
                    chunk.priority = new_priority
                    self._buckets[new_priority][key] = chunk
                    if new_priority > prio:
                        self.stats["promotions"] += 1
                    else:
                        self.stats["demotions"] += 1
                    return

    def record_ltseed_upload(self, file_hash: str, piece_index: int,
                              bytes_uploaded: int) -> None:
        """记录 LT-Seed 上传事件, 自动升级 hot piece."""
        self.ltseed_tracker.record_upload(file_hash, piece_index, bytes_uploaded)
        # 检查是否需要升级
        if self.ltseed_tracker.is_hot(file_hash, piece_index):
            self.set_priority(file_hash, piece_index, PiecePriority.LT_SEED_HOT)

    def flush(self) -> None:
        """flush 所有脏块."""
        with self._lock:
            dirty_keys = []
            for prio, bucket in self._buckets.items():
                for key, chunk in bucket.items():
                    if chunk.dirty:
                        dirty_keys.append((prio, key, chunk))
        for prio, key, chunk in dirty_keys:
            self._flush_one(prio, key, chunk)
        self.stats["flushes"] += 1

    def close(self) -> None:
        self.flush()
        self._stop_event.set()
        self._dirty_event.set()
        if self._flush_thread:
            self._flush_thread.join(timeout=5)

    def stats_summary(self) -> Dict[str, int]:
        with self._lock:
            hit_rate = (self.stats["hits"] /
                        max(self.stats["hits"] + self.stats["misses"], 1))
            return {
                **self.stats,
                "bytes_total": self._bytes_total,
                "bytes_dirty": self._bytes_dirty,
                "bucket_sizes": {
                    p.name: len(self._buckets[p]) for p in PiecePriority
                },
                "hit_rate": hit_rate,
                "hot_pieces_ltseed": len(self.ltseed_tracker.get_hot_pieces()),
            }

    # ----- 内部: 淘汰 -----

    def _evict_if_needed(self) -> None:
        """LRU + 优先级混合淘汰.

        策略:
        1. 如果某优先级桶超过其配额, 淘汰该桶最旧的
        2. 全局超额时, 先淘汰 COLD, 再 NORMAL, 不淘汰 LT_SEED_HOT / DOWNLOAD_HOT
        """
        with self._lock:
            # 各优先级配额
            quotas = {
                PiecePriority.COLD: int(self.settings.max_memory_bytes * self.settings.quota_cold),
                PiecePriority.NORMAL: int(self.settings.max_memory_bytes * self.settings.quota_normal),
                PiecePriority.LT_SEED_HOT: int(self.settings.max_memory_bytes * self.settings.quota_lt_seed_hot),
                PiecePriority.DOWNLOAD_HOT: int(self.settings.max_memory_bytes * self.settings.quota_download_hot),
            }
            # 优先级桶大小
            bucket_bytes = {
                p: sum(len(c.data) for c in bucket.values())
                for p, bucket in self._buckets.items()
            }
            # 1. 检查每个桶是否超额
            for prio in PiecePriority:
                while bucket_bytes[prio] > quotas[prio] and self._buckets[prio]:
                    # 淘汰最旧 (timestamp 最小)
                    oldest_key = min(self._buckets[prio],
                                      key=lambda k: self._buckets[prio][k].timestamp)
                    chunk = self._buckets[prio].pop(oldest_key)
                    if chunk.dirty:
                        self._flush_one(prio, oldest_key, chunk)
                    self._bytes_total -= len(chunk.data)
                    if chunk.dirty:
                        self._bytes_dirty -= len(chunk.data)
                    bucket_bytes[prio] -= len(chunk.data)
                    self.stats["evictions"] += 1
            # 2. 全局超额, 跨优先级淘汰 (从 COLD 开始)
            while self._bytes_total > self.settings.max_memory_bytes:
                evicted = False
                for prio in [PiecePriority.COLD, PiecePriority.NORMAL]:
                    if self._buckets[prio]:
                        oldest_key = min(self._buckets[prio],
                                          key=lambda k: self._buckets[prio][k].timestamp)
                        chunk = self._buckets[prio].pop(oldest_key)
                        if chunk.dirty:
                            self._flush_one(prio, oldest_key, chunk)
                        self._bytes_total -= len(chunk.data)
                        if chunk.dirty:
                            self._bytes_dirty -= len(chunk.data)
                        bucket_bytes[prio] -= len(chunk.data)
                        self.stats["evictions"] += 1
                        evicted = True
                        break
                if not evicted:
                    break

    def _flush_if_needed(self) -> None:
        if self._bytes_dirty > self.settings.max_memory_bytes * 0.5:
            self._dirty_event.set()

    # ----- 内部: flush 线程 -----

    def _start_flush_thread(self) -> None:
        self._flush_thread = threading.Thread(
            target=self._flush_loop, daemon=True, name="PriorityCacheFlush"
        )
        self._flush_thread.start()

    def _flush_loop(self) -> None:
        while not self._stop_event.is_set():
            if not self._dirty_event.wait(self.settings.eviction_check_interval_sec):
                continue
            if self._stop_event.is_set():
                break
            self._dirty_event.clear()
            try:
                self._flush_dirty()
            except Exception as e:
                LOG.error("flush thread error: %s", e)

    def _flush_dirty(self) -> None:
        with self._lock:
            dirty = []
            for prio, bucket in self._buckets.items():
                for key, chunk in bucket.items():
                    if chunk.dirty:
                        dirty.append((prio, key, chunk))
        for prio, key, chunk in dirty:
            self._flush_one(prio, key, chunk)

    def _flush_one(self, prio: PiecePriority, key: PriorityCacheKey,
                    chunk: PriorityChunk) -> None:
        with self._lock:
            chunk.dirty = False
            self._bytes_dirty -= len(chunk.data)
        if self.on_flush:
            try:
                self.on_flush(key, chunk.data)
            except Exception as e:
                LOG.error("flush callback failed: %s", e)
                # 重新标记 dirty
                with self._lock:
                    chunk.dirty = True
                    self._bytes_dirty += len(chunk.data)

    # ----- 内部: 自动 resize -----

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
                if avail < self.settings.min_free_memory_bytes:
                    new_cap = max(
                        self.settings.max_memory_bytes // 2,
                        64 * 1024 * 1024,
                    )
                    if new_cap != self.settings.max_memory_bytes:
                        LOG.warning(
                            "memory pressure: avail=%d MB, shrinking cache %d→%d MB",
                            avail // 1048576,
                            self.settings.max_memory_bytes // 1048576,
                            new_cap // 1048576,
                        )
                        self.settings.max_memory_bytes = new_cap
                        self.stats["max_memory_bytes"] = new_cap
                        self._evict_if_needed()
                elif avail > total * 0.5 and self.settings.max_memory_bytes < total * 0.3:
                    new_cap = min(
                        int(self.settings.max_memory_bytes * 1.5),
                        int(total * 0.3),
                    )
                    if new_cap != self.settings.max_memory_bytes:
                        LOG.info(
                            "memory ample: avail=%d%%, growing cache %d→%d MB",
                            int(avail * 100 / total),
                            self.settings.max_memory_bytes // 1048576,
                            new_cap // 1048576,
                        )
                        self.settings.max_memory_bytes = new_cap
                        self.stats["max_memory_bytes"] = new_cap
            except Exception as e:
                LOG.debug("auto-resize check failed: %s", e)


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    logging.basicConfig(level=logging.INFO,
                        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s")
    ap = argparse.ArgumentParser(description="Priority Disk Cache demo")
    ap.add_argument("--files", type=int, default=1, help="模拟多少个文件")
    ap.add_argument("--pieces", type=int, default=100, help="每个文件多少 piece")
    ap.add_argument("--piece-size", type=int, default=64*1024, help="piece 大小 (字节)")
    args = ap.parse_args()

    cache = PriorityDiskCache(
        settings=PriorityCacheSettings(
            max_memory_bytes=args.files * args.pieces * args.piece_size // 4,
            auto_resize=False,
        )
    )
    print(f"cache capacity: {cache.settings.max_memory_bytes // 1048576} MiB")
    print(f"writing {args.files} files * {args.pieces} pieces * {args.piece_size // 1024} KiB = "
          f"{args.files * args.pieces * args.piece_size // 1048576} MiB total")

    # 写入 (模拟 4 倍容量, 触发淘汰)
    for fi in range(args.files):
        file_hash = f"{fi:040d}"
        for pi in range(args.pieces):
            cache.put(file_hash, pi, os.urandom(args.piece_size), dirty=True)
            # 偶尔读
            if pi % 10 == 0:
                cache.get(file_hash, pi // 2)

    # 模拟 LT-Seed 频繁上传某些 piece, 触发升级
    print("\nsimulating LT-Seed uploads...")
    for _ in range(10):
        cache.record_ltseed_upload(f"{0:040d}", 5, 64 * 1024)
        cache.record_ltseed_upload(f"{0:040d}", 10, 64 * 1024)
        cache.record_ltseed_upload(f"{0:040d}", 15, 64 * 1024)

    cache.flush()
    print(f"\n=== Stats ===")
    for k, v in cache.stats_summary().items():
        if isinstance(v, dict):
            print(f"  {k}:")
            for k2, v2 in v.items():
                print(f"    {k2}: {v2}")
        else:
            print(f"  {k}: {v}")
    cache.close()
