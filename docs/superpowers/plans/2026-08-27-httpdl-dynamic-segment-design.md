---
AIGC:
    Label: "1"
    ContentProducer: 001191440300708461136T1XGW3
    ProduceID: 3f6b26a49c1e395dad1e662e16b1300f_552f53eba22311f1abe1525400e6dd8f
    ReservedCode1: HY7rqv803aCjo5rcKrvBrY1ww2tqMGfEFKp+2KXmErH7ECdFlrwENc5W9YPZawDt4prcL3NR+O6fRXqH7vWV9tZNqrg+k0Wcgt1t+p3b98DG35VwqS6t/D4DtMMDdwFGc461QlC9z4lQ90ft8u9sc0yrVUf0/cZnCYC0KOV7e7UsT/Mt5VpxzCu0pMk=
    ContentPropagator: 001191440300708461136T1XGW3
    PropagateID: 3f6b26a49c1e395dad1e662e16b1300f_552f53eba22311f1abe1525400e6dd8f
    ReservedCode2: HY7rqv803aCjo5rcKrvBrY1ww2tqMGfEFKp+2KXmErH7ECdFlrwENc5W9YPZawDt4prcL3NR+O6fRXqH7vWV9tZNqrg+k0Wcgt1t+p3b98DG35VwqS6t/D4DtMMDdwFGc461QlC9z4lQ90ft8u9sc0yrVUf0/cZnCYC0KOV7e7UsT/Mt5VpxzCu0pMk=
---

# httpdl 动态分段设计（P0）

日期：2026-08-27
状态：已批准（用户确认方案 A）
范围：crates/httpdl 仅限；不涉及 btcore / provider / daemon

## 1. 背景与目标

当前 httpdl 采用静态等分策略（`static_split.rs`）：

- 下载开始前一次性规划全部段：`N = clamp(total/64MB, 2, 8)`，等分覆盖 `[0, total)`；
- 每个段一个 tokio task，整段读入内存 `Vec<u8>`，读完再 seek 写入 `.part`；
- 续传时从 `offset` 等分剩余区间。

两个核心痛点：

1. **慢段拖尾**：静态等分下各段进度不均，慢段决定整体完成时间，快 worker 空闲等待；
2. **内存峰值**：段大小可达 64MB+，整段读内存造成不必要的内存占用。

目标：参考 aria2 Segment Scheduler 的"动态领取"机制，升级为 worker 池模型，并改为流式写盘。

## 2. 参考机制（aria2）

aria2 的动态分段核心（`SegmentMan.cc` / `DownloadCommand.cc` / `DefaultPieceStorage.cc`）：

- **SegmentManager 持有全部分段状态**：`usedSegmentEntries_`（在用段）、`segmentWrittenLengthMemo_`（各段进度）；
- **worker（DownloadCommand）按需领取**：段完成 → `prepareForNextSegment()` → 取下一个缺失段继续；连接空闲/失败 → `cancelSegment` 归还；
- **minSplitSize 控制领取粒度**：`getMissingPiece(minSplitSize, ...)` 决定每次领取的最小分段大小，避免粒度过碎；
- **流式写盘**：数据到达即 transform 写盘（`segment->getPositionToWrite()` 顺序写），不整段缓存。

本项目落地时只吸收"动态领取 + 流式写盘"两层；aria2 的 Piece/BitfieldMan 双层结构（面向 BT）不引入——本项目 BT 已选定 libtorrent 薄内核。

## 3. 架构

### 3.1 新模块：`crates/httpdl/src/segment_manager.rs`

```
SegmentManager
├─ total: u64            // 已知文件大小（未知长度时为 0）
├─ pending: Vec<(u64,u64)>  // 待下载区间，有序、不相交、升序
├─ done: Vec<(u64,u64)>     // 已完成区间，有序、不相交（用于 finish 判断）
└─ offset: u64              // 续传起点（[0, offset) 视为已完成）
```

接口：

- `new(total: u64, offset: u64) -> SegmentManager`
  - `offset > 0` 时 `pending = [(offset, total)]`；否则 `pending = [(0, total)]`；
  - `total == 0` 表示未知长度（见 §4.5）。
- `take_segment(min_split_size: u64) -> Option<Segment>`
  - 从第一个 pending 区间头部切出 `min(min_split_size, 区间剩余)` 的段；
  - 区间耗尽则移除；无 pending 返回 `None`。
- `complete(seg: Segment)`
  - 将段并入 `done`；若 `done` 合并后覆盖 `[offset, total)` 全区间 → `finished`。
- `release(seg: Segment)`
  - 段失败归还：插回 `pending`（按起始位置有序插入，若与相邻 pending 区间连续则合并）。
- `is_finished() -> bool`
- `progress() -> (u64, u64)` // 已下载字节 / 总字节（可选，供上层展示）

共享方式：`Arc<tokio::sync::Mutex<SegmentManager>>`。段粒度最小 1MB（`MIN_GRANULARITY`），防止极端碎片。

### 3.2 重构：`crates/httpdl/src/download.rs`

worker 池模型替代"每段一个 task"：

```
N = segment_count(total)   // 保留现有公式，作为 worker 数
启动 N 个 worker，每个 worker 循环：
  1. seg = manager.take_segment(min_split_size)   // None → 退出
  2. for m in mirrors:
       download_segment_stream(client, m, seg)    // 流式写盘
       成功 → complete(seg)，goto 1
  3. 全源失败 → release(seg) 归还，整体返回 Err
```

- `min_split_size` 默认 16MB（常量 `DEFAULT_MIN_SPLIT_SIZE`，后续可配置化）；
- 第一个 pending 区间剩余不足 `min_split_size` 时整段领取，避免尾部浪费；
- 并发 worker 数不变（与现有一致），行为差异仅在"领取时机"。

### 3.3 流式写盘：`download_segment_stream`

替代现有 `download_segment`（整段 `Vec<u8>`）：

- 打开 `.part`（预先 `set_len(total)`，语义同现状），seek 到 `seg.start`；
- 逐 chunk：`limiter.wait(len)` → `write_all(chunk)`；
- 结束时校验：响应状态必须 `206 Partial Content`；实收字节数必须等于段长；
- 段写入与段区间不相交，无文件锁（同现状）。

## 4. 行为细节

### 4.1 动态领取

- 首轮：N 个 worker 各领一段（粒度 `min_split_size`）；
- 后续：完成一段立即领取下一段，直到 `take_segment` 返回 `None`；
- 收尾：最后剩余区间不足 `min_split_size` 时整段领取，保证无遗漏。

### 4.2 续传

- 入口由 `plan_segments_from(offset, total)` 改为 `SegmentManager::new(total, offset)`；
- `.part` 已有 `offset` 字节，保留不截断；只领取 `[offset, total)` 的缺失段；
- 预分配 `set_len(total)` 语义保持。

### 4.3 失败处理

- 段在某源失败 → 尝试下一个 mirror（保持现状语义）；
- **全源失败 → 整个下载返回 `Err`**（与现状一致，不做部分成功利用）；
- 失败缩小粒度重试、部分成功利用：P1（YAGNI，不在本设计内）。

### 4.4 镜像切换

- `mirrors: &[String]` 语义不变：每段按列表顺序尝试；
- worker 内局部持有 mirror 列表副本，互不影响。

### 4.5 未知长度（无 Content-Length）

- `total == 0`：不启动多 worker，退化为单连接流式下载（同现状 `GrowSegment` 语义）；
- 由上层（engine.rs）在探测到无 Content-Length 时直接走该路径，不经过 SegmentManager。

## 5. 兼容性与移除

- `static_split.rs` 保留 `split_n`（M4b 用户用例依赖显式段数）；`segment_count` 继续作为 worker 数公式；
- `plan_segments` / `plan_segments_from` 不再被 download 路径使用，标记 deprecated 或删除（实现时按调用方清理）；
- `download_segments` 的公开签名保持不变或仅内部重构，以 engine.rs 调用方最小改动为准。

## 6. 测试策略

### 6.1 单元测试（segment_manager.rs）

- 领取：正常切段、尾部整段领取、pending 耗尽返回 None；
- 归还：插回有序、相邻区间合并；
- 完成：done 合并、finish 判定（覆盖全区间才 finished）；
- 续传：`new(total, offset)` 后 take 只会落在 `[offset, total)`。

### 6.2 集成测试（crates/httpdl/tests/）

- 本地测试 HTTP server（现有测试设施复用）：
  - 多段并行下载完整性（文件字节一致）；
  - 断点续传：先下部分，重启后只下缺失区间；
  - 镜像切换：主源失败后自动切备源；
- **只跑 `cargo test -p httpdl`，不跑 workspace 全量**（避免与 btcore resume 测试的 seeder 跨进程锁冲突，并行 AI 工作期间尤其注意）。

### 6.3 不做

- 不引入 BT piece/bitfield 结构；
- 不做失败缩小粒度重试（P1）；
- 不做动态段大小自适应（P1，可基于历史速率调整 min_split_size）。

## 7. 文档更新

- 本设计文档：`docs/superpowers/plans/2026-08-27-httpdl-dynamic-segment-design.md`；
- 实现完成后：`docs/IMPLEMENTED.md` 登记；若 CAPABILITY_MAP / BACKLOG 中相关缺口被覆盖，同步更新；
- 并行 AI 工作期间，文档更新仅限 httpdl 相关文件，避免与 btcore/provider 线冲突。

## 8. 实施顺序（供 writing-plans 细化）

1. `segment_manager.rs` + 单元测试；
2. `download.rs` 重构为 worker 池 + 流式写盘；
3. 集成测试补齐（并行/续传/镜像）；
4. `engine.rs` 接入点调整（未知长度路径、续传入口）；
5. 文档更新（IMPLEMENTED.md / BACKLOG.md）。
*（内容由AI生成，仅供参考）*
