# httpdl 动态分段设计（P0）

> 2026-08-27

## 1. 目标

在 `crates/httpdl` 内引入动态分段下载能力，在不改变对外 `Engine` trait 的前提下，将单文件下载从单 worker 顺序下载升级为多 worker 动态领取区间并行下载，支持续传、镜像切换、流式写盘，内存从 O(段大小) 降到 O(写缓冲)。

## 2. 现状约束

- `crates/httpdl/src/engine.rs` 暴露 `Engine` trait，现有 `HttpEngine` 基于 `reqwest::Client`。
- 单文件下载路径为 `download.rs`（或等效模块），当前按顺序读取整文件或固定分片。
- 对外 API 不变：`Engine::download(url, dest)` → `Result<()>`。
- 必须保留 RateLimiter 语义（写盘速率控制）。
- 未知长度（无 `Content-Length`）时退化为单 worker 流式下载。

## 3. 架构

新增 `crates/httpdl/src/segment_manager.rs`，重构下载路径为 worker 池模型：

```text
SegmentManager（Arc<Mutex>）          N 个 worker（tokio task）
├─ pending: 有序区间 Vec<(u64,u64)>   每个 worker 循环：
├─ done:    已完成区间集合              ① take_segment() → None 退出
└─ total/offset 续传状态               ② 对 mirrors 依次下载该段（流式写盘）
                                       ③ complete(seg) / release(seg)
                                       ④ 回到 ①
```

### 3.1 SegmentManager

```rust
pub struct SegmentManager {
    total: u64,
    offset: u64,
    done: HashSet<Segment>,
    pending: VecDeque<Segment>,
    min_split: u64,
}

impl SegmentManager {
    pub fn new(total: u64, offset: u64, min_split: u64) -> Self;
    pub fn take_segment(&mut self) -> Option<Segment>;
    pub fn complete(&mut self, seg: Segment);
    pub fn release(&mut self, seg: Segment);
    pub fn progress(&self) -> f64;
}
```

- `Segment` = `(start, end)`，左闭右开 `[start, end)`。
- `done` 使用 `HashSet<(u64,u64)>`，方便续传比对。
- `pending` 保持有序，`take_segment` 从头部取，确保 worker 领取顺序一致。

### 3.2 Worker 池

```rust
struct Worker {
    manager: Arc<Mutex<SegmentManager>>,
    mirrors: Vec<Url>,
    dest: PathBuf,
    limiter: Option<RateLimiter>,
}

impl Worker {
    async fn run(&self);
    async fn download_segment_stream(&self, seg: Segment, url: &str) -> Result<()>;
}
```

- 初始并发度 `N = clamp(total / 64MB, 2, 8)`。
- 首轮每 worker 领一段，完成后立即领下一段。
- worker 退出条件：`take_segment()` 返回 `None`（pending 空且 total 已覆盖）。

## 4. 核心行为

### 4.1 动态领取

- 粒度 = `min_split_size`（默认 16MB）。
- 最后一个区间不足粒度时整段领取，避免尾部浪费。
- 不再一次性规划全部段，worker 完成一段后立即计算下一段边界并领取。

### 4.2 流式写盘

- `download_segment_stream` 对每个源按 `Range: start-end` 请求。
- 边读边写，seek 到段起点顺序写，内存仅为写缓冲（如 64KB）。
- 保留 RateLimiter：写盘前经过 limiter，保持单线程写盘语义。

### 4.3 续传

- `SegmentManager::new(total, offset)` 将 `[0, offset)` 标记完成。
- `.part` 文件保留旧数据，只领取 `[offset, total)` 缺失段。
- 重启时读取 `.part` 文件大小作为 `offset`，若 `offset >= total` 直接完成。

### 4.4 未知长度

- `total == 0`（无 `Content-Length`）时退化为单 worker 流式下载，行为同现状。
- 不创建 `SegmentManager`，直接顺序写盘。

## 5. 失败处理

- 段在某源失败 → 自动尝试下一个 mirror。
- 全源失败 → 段归还 `pending` → 整个下载返回 `Err`（不单独重试，P1 细化）。
- 网络错误仅影响当前段，不影响其他 worker 已领区间。

## 6. 测试

### 6.1 单测（无 IO）

- `SegmentManager` 领取/归还/完成/续传跳过的纯逻辑。
- 边界：total=0、offset=total、min_split > total。

### 6.2 集成（本地 HTTP server）

- 多段并行：验证多个 worker 同时写不同区间。
- 断点续传：模拟中断后重启，验证从 `.part` 大小续传。
- 镜像切换：首段源失败，自动 fallback 到第二源。

### 6.3 运行约束

- 只跑 `cargo test -p httpdl`，不跑 workspace 全量，避免与另一条线的 seeder 跨进程锁冲突。

## 7. 实施步骤

1. 新增 `crates/httpdl/src/segment_manager.rs`（Segment / SegmentManager）。
2. 新增 `crates/httpdl/src/worker.rs`（Worker 池、download_segment_stream）。
3. 重构 `crates/httpdl/src/download.rs` 使用 SegmentManager + Worker。
4. 保留 `Engine` trait 不变，内部路由到新路径。
5. 单测 + 集成测试。
6. 更新 `docs/IMPLEMENTED.md` / `docs/BACKLOG.md`。

## 8. 风险

- 并发写盘冲突：同一文件多 worker 写不同区间，需确保 seek 和写原子性（单线程写盘或 per-segment 文件句柄）。
- 镜像认证状态：若 mirror 需要鉴权，需复用现有 `Client::send` 的认证逻辑。
- RateLimiter 并发安全：需改为 `Arc<Mutex<RateLimiter>>` 或 per-worker 分片。

## 9. 未决（P1）

- 失败段指数退避重试。
- 动态并发度调整（根据 RTT / 吞吐自动增减 worker）。
- 段完成事件上报（进度回调细化）。
