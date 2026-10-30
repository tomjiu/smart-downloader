//! 动态分段下载器（P0，方案A）：worker 池经 SegmentManager 按 FIFO 动态领取
//! 段，段内流式写盘（seek+write，段不相交 → 并发写无锁）。
//! 失败语义：任一段全 mirror 失败 → 整体 Err（不做部分成功利用，P0 约定）。
//!
//! P4 演进：
//! - **段账本**：每段完成后原子写 `<part>.progress`（已完成段区间），
//!   断点续传真源与下载过程同生共死（进程崩溃最多丢一个在飞段）。
//! - **真暂停**：`pause` 标志在段边界检查——置位后不再领取新段，在飞段
//!   收尾后返回 `Paused`（调用方不得对 Paused 结果做校验/落位）。
//! - **进度回调**：每段完成回报全局已下载字节（引擎透传到 status）。

use crate::ledger::{self, Ledger};
use crate::rate::RateLimiter;
use crate::segment_manager::{SegmentManager, MIN_RETRY_GRANULARITY};
use crate::static_split::segment_count;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Mirror 评分 clamp 边界（防单个坏源被无限惩罚/好源无限膨胀）。
/// `pub(crate)`：engine 校验失败隔离试错的归因中毒评分复用同一口径。
const SCORE_MAX: i64 = 4;
pub(crate) const SCORE_MIN: i64 = -4;

/// 顺序下载（边下边播）在飞段窗口：限制同时在飞的段数，前缀完成速率不再被
/// 后段乱序完成拖累（FIFO 领取语义不变，只是收紧 lookahead）。窗口 2 =
/// 在单连接流式与多连接吞吐间取平衡（TCP 窗口填充仍有余量）。
pub const SEQUENTIAL_WINDOW: usize = 2;

/// 段账本句柄：恢复已完成段 + 下载中逐段持久化进度。
#[derive(Clone, Debug)]
pub struct DynamicLedger {
    /// 账本落盘路径（`<part>.progress`）。
    pub path: PathBuf,
    /// 内容一致性 token（随每笔账本写入持久化，供下次 add 决策）。
    pub etag: Option<String>,
    /// 内容指纹备援（E26）：Last-Modified 原始串（随账本持久化）。
    pub last_modified: Option<String>,
    /// 本 session 段粒度（恢复场景必须沿用账本记录的粒度，保证段对齐）。
    pub min_split: u64,
    /// 已完成段（恢复起点；闭区间升序）。
    pub done: Vec<(u64, u64)>,
}

/// download_dynamic 结局：`Completed` = 全部落位（可校验/落位）；
/// `Paused` = 暂停退出（在飞段已收尾并记账，调用方不得 finalize）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynamicOutcome {
    Completed,
    Paused,
}

/// 段数据落盘策略（fsync 性能开关，PR #114 审查 §3.3 跟进项）。
///
/// - `per_segment = true`（默认）：每段 fsync 后才记账——P0-4 WAL 语义的
///   最严粒度。SSD 推荐（~13ms fsync 摊在 16MB 段上占比可忽略）。
/// - `per_segment = false`（批量模式）：累计 `batch_segments` 段或距上次
///   落盘超过 `batch_interval`（先到者）才做一次「数据 fsync + 账本落盘」。
///   机械盘场景的吞吐救星：段级 fsync 把机械盘吞吐支配到个位数 MB/s
///   （PR #114 审查实测 2022 MB/s → 6.4 MB/s），批量化把 fsync 成本
///   摊销到 N 段一次。
///
/// 两模式共享同一 WAL 不变量：**段数据 fsync 先于含该段的账本落盘**。
/// 批量模式只是把不变量粒度从「段」放宽到「批」——崩溃时账本至多落后
/// 于数据（重启重下少量段，代价可接受），绝不超前（不产生 P0-4 修复的
/// 「账本已记、数据未落盘」→ 空洞文件问题）。批量模式下 `min_split`
/// 调小（1-4MB）时 fsync 频率自然上升 4-16 倍的注意事项同样成立，
/// 批量阈值是唯一对冲手段（审查 §3.3 建议 3）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentSyncPolicy {
    /// true = 每段 fsync（默认）；false = 批量模式。
    pub per_segment: bool,
    /// 批量模式触发阈值一：自上次落盘累计完成段数（默认 8）。
    pub batch_segments: u32,
    /// 批量模式触发阈值二：距上次落盘的时长（默认 2s）。任一先到即触发。
    pub batch_interval: std::time::Duration,
}

impl Default for SegmentSyncPolicy {
    fn default() -> Self {
        Self {
            per_segment: true,
            batch_segments: 8,
            batch_interval: std::time::Duration::from_secs(2),
        }
    }
}

impl SegmentSyncPolicy {
    /// 批量触发判定（纯函数，可单测）：`pending` = 自上次落盘累计完成段数，
    /// `since_last` = 距上次落盘（或批量计时起点）的时长。任一阈值先到
    /// 即 true。`pending = 0` 恒 false（无脏数据不空转 fsync）。
    pub fn should_flush(&self, pending: u32, since_last: std::time::Duration) -> bool {
        pending > 0 && (pending >= self.batch_segments || since_last >= self.batch_interval)
    }
}

/// 批量模式落盘状态（worker 间共享；只在 manager 锁内访问，
/// 锁序恒为 manager → batch，无反向路径）。
#[derive(Debug)]
struct BatchSyncState {
    /// 自上次落盘累计完成段数。
    pending: u32,
    /// 有未落盘的完成段（final flush 判据）。
    dirty: bool,
    /// 上次落盘时刻（初始 = 批量计时起点）。
    last_flush: std::time::Instant,
}

/// 批量模式冲刷：数据 fsync 先行 → 账本快照落盘（WAL 顺序保持，
/// 粒度从「段」放宽到「批」）。调用方必须已持有 manager 锁与 batch 锁
/// （快照与计数在同一临界区内，账本视图与内存一致——与既有每段
/// `ledger::save` 锁内口径相同）。fsync 失败按段级 fsync 同口径上抛；
/// `ledger::save` 失败内部仅告警（既有语义：进度丢失 = 下次重下）。
fn flush_batch(
    batch: &mut BatchSyncState,
    part: &Path,
    manager: &SegmentManager,
    ledger_handle: &Option<DynamicLedger>,
    total: u64,
) -> Result<(), String> {
    // 数据先行：任一 handle fsync 均冲刷 inode 脏页（段写入各自的
    // 短命 handle 已关闭，此处新开一次即可覆盖全部待落盘段）。
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(part)
        .map_err(|e| format!("batch flush open: {e}"))?;
    f.sync_data()
        .map_err(|e| format!("batch flush fsync: {e}"))?;
    drop(f);
    // 账本随后：快照含全部已完成段（含触发本批的段——complete 已先于
    // flush 调用在 manager 内执行）。
    if let Some(l) = ledger_handle {
        let snapshot = Ledger {
            version: ledger::LEDGER_VERSION,
            total,
            min_split: l.min_split,
            etag: l.etag.clone(),
            last_modified: l.last_modified.clone(),
            done: manager.done_ranges().to_vec(),
        };
        ledger::save(&l.path, &snapshot);
    }
    batch.pending = 0;
    batch.dirty = false;
    batch.last_flush = std::time::Instant::now();
    Ok(())
}

/// worker 退出原因。
enum WorkerExit {
    /// 队列领空（正常收工）。
    Drained,
    /// 暂停标志置位（段边界退出）。
    Paused,
}

/// worker 镜像偏好轮转（E24）：起点 = worker_no % len（确定性均匀分摊）。
/// 单元素表恒等返回克隆（零变化）。供 download_dynamic spawn 时调用与单测。
fn rotate_mirrors(mirrors: &[String], worker_no: usize) -> Vec<String> {
    if mirrors.len() <= 1 {
        return mirrors.to_vec();
    }
    let k = worker_no % mirrors.len();
    mirrors[k..]
        .iter()
        .chain(mirrors[..k].iter())
        .cloned()
        .collect()
}

/// 动态分段下载：worker 数 N=clamp(total/64MB, 2, 8)，段粒度 min_split。
/// `ledger` = Some 时从已完成段恢复并逐段持久化进度（None = 一次性下载，
/// 不落账本）。`pause` = Some 时段边界检查暂停标志。`progress` = 每段完成
/// 后回报全局已下载字节（含恢复凭据折算）。
/// 任一段全源失败 → Err（调用方决定重试/报错）。`limiter` 跨段共享（0 = 不限）。
/// `scores` = 可选 Mirror 加权评分表（None = 不评分，纯按 mirrors 顺序）。
/// `sequential` = 顺序下载：在飞段数收紧到 `SEQUENTIAL_WINDOW`（false = 不限）。
/// `headers` = 任务级自定义头（H-8 修复）：随每个段请求下发（与探测一致语义），
/// 否则鉴权型源（Cookie/Token）段请求 403 不可用。任务头中的 `range`（大小写
/// 不敏感）跳过——段区间由段参数生成，任务头不得劫持/制造重复 Range。
/// 参数均为同层语义字段，聚合结构体反而增加调用方样板 → 允许 14 参。
#[allow(clippy::too_many_arguments)]
pub async fn download_dynamic(
    client: &reqwest::Client,
    part: &Path,
    total: u64,
    min_split: u64,
    mirrors: &[String],
    headers: &[(String, String)],
    limiter: Arc<RateLimiter>,
    scores: Option<Arc<Mutex<HashMap<String, i64>>>>,
    sequential: bool,
    ledger_handle: Option<DynamicLedger>,
    pause: Option<Arc<AtomicBool>>,
    progress: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    sync: SegmentSyncPolicy,
    max_workers: usize,
) -> Result<DynamicOutcome, String> {
    // 预分配 .part（续传场景：旧 .part 保留，只写缺失段；不截断）
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(part)
        .map_err(|e| e.to_string())?;
    f.set_len(total).map_err(|e| e.to_string())?;
    drop(f);

    // 段管理器：账本恢复（沿用账本粒度，跳过已完成段）或全新计划
    let manager = Arc::new(Mutex::new(match &ledger_handle {
        Some(l) => SegmentManager::new_with_done(total, l.min_split, &l.done),
        None => SegmentManager::new(total, 0, min_split),
    }));

    // 进度初报：恢复凭据折算的已完成字节立即可见（daemon 轮询无需等首段）
    if let Some(p) = &progress {
        p(manager.lock().done_bytes());
    }

    // 顺序模式在飞闸门：permit 从领取前持有到 complete 后释放（RAII），
    // 失败/panic 退出路径同样随作用域释放，无泄漏。
    let seq_gate: Option<Arc<tokio::sync::Semaphore>> = if sequential {
        Some(Arc::new(tokio::sync::Semaphore::new(SEQUENTIAL_WINDOW)))
    } else {
        None
    };
    // B2（#214）：单任务并发上限——0 = 自动公式；>0 = clamp(公式, cap)
    // （1 = 强制单连接；上限不放大公式值，防小文件打满连接）。
    let n_workers = if max_workers == 0 {
        segment_count(total)
    } else {
        segment_count(total).min(max_workers).max(1)
    };
    // 批量落盘状态（fsync 性能开关）：仅在 !sync.per_segment 时使用；
    // 锁序恒为 manager → batch（只在 manager 锁内的完成块/收尾冲刷中访问）。
    let batch_state = Arc::new(Mutex::new(BatchSyncState {
        pending: 0,
        dirty: false,
        last_flush: std::time::Instant::now(),
    }));
    let mut workers = tokio::task::JoinSet::new();
    for worker_no in 0..n_workers {
        let client = client.clone();
        let part = part.to_path_buf();
        let mirrors = mirrors.to_vec();
        // E24 多源并行：worker 按序号轮转 mirror 起点偏好（w%len），
        // 多源表在 worker 池上摊平（真并行分摊而非全部挤首选源）；
        // 段失败仍按轮转后的顺序逐源回退。单源表轮转恒等（零变化）。
        // 评分排序（download_loop）已把健康源排前，轮转在其上做均匀分摊。
        let mirrors = rotate_mirrors(&mirrors, worker_no);
        let headers = headers.to_vec();
        let limiter = limiter.clone();
        let manager = manager.clone();
        let scores = scores.clone();
        let seq_gate = seq_gate.clone();
        let ledger_handle = ledger_handle.clone();
        let pause = pause.clone();
        let progress = progress.clone();
        let batch_state = batch_state.clone();
        // sync 为 Copy：worker 闭包按迭代自动拷贝（段写入跳过/保留 fsync 的依据）
        workers.spawn(async move {
            loop {
                // 真暂停：段边界检查——置位后不再领取新段（在飞段收尾即退出）
                if pause.as_ref().is_some_and(|p| p.load(Ordering::SeqCst)) {
                    return Ok::<WorkerExit, String>(WorkerExit::Paused);
                }
                // 顺序模式：先拿 permit 再领取段，保证「在飞段数 ≤ 窗口」
                // （先领后等会导致窗口外表内的段已占用 FIFO 游标）。
                let _permit = match &seq_gate {
                    Some(g) => Some(
                        g.clone()
                            .acquire_owned()
                            .await
                            .map_err(|_| "sequential gate closed".to_string())?,
                    ),
                    None => None,
                };
                let seg = {
                    let mut m = manager.lock();
                    match m.take_segment() {
                        Some(s) => s,
                        None => return Ok::<WorkerExit, String>(WorkerExit::Drained),
                    }
                };
                let mut ok = false;
                for url in &mirrors {
                    match download_segment_with_retry(
                        &client, url, &part, seg, &headers, &limiter, sync,
                    )
                    .await
                    {
                        Ok(()) => {
                            if let Some(sc) = &scores {
                                update_score(sc, url, 1);
                            }
                            ok = true;
                            break;
                        }
                        Err(_) => {
                            // 该 mirror 对此段失败（粒度已缩到最小仍失败）→ 惩罚
                            if let Some(sc) = &scores {
                                update_score(sc, url, -2);
                            }
                        }
                    }
                }
                if !ok {
                    return Err(format!(
                        "all mirrors failed for segment [{}, {}]",
                        seg.start, seg.end
                    ));
                }
                // 段完成：记账 + 账本原子落盘 + 进度回报（锁内一并完成，
                // 保证账本视图与计数一致）。落盘节奏由 SegmentSyncPolicy 决定：
                // 每段（P0-4 默认）或批量（fsync 性能开关，WAL 顺序不变）。
                {
                    let mut m = manager.lock();
                    m.complete(seg);
                    if sync.per_segment {
                        if let Some(l) = &ledger_handle {
                            let snapshot = Ledger {
                                version: ledger::LEDGER_VERSION,
                                total,
                                min_split: l.min_split,
                                etag: l.etag.clone(),
                                last_modified: l.last_modified.clone(),
                                done: m.done_ranges().to_vec(),
                            };
                            ledger::save(&l.path, &snapshot);
                        }
                    } else {
                        // 批量模式：记账在内存，fsync+账本落盘推迟到批边界
                        //（锁序 manager → batch，与收尾冲刷同序）。
                        let mut b = batch_state.lock();
                        b.pending += 1;
                        b.dirty = true;
                        if sync.should_flush(b.pending, b.last_flush.elapsed()) {
                            flush_batch(&mut b, &part, &m, &ledger_handle, total)?;
                        }
                    }
                    if let Some(p) = &progress {
                        p(m.done_bytes());
                    }
                }
                drop(_permit); // 显式释放：permit 生命周期必须覆盖 complete
            }
        });
    }
    // 任一 worker 失败 → 整体失败，取消其余；暂停退出优先于正常收工判定。
    // 批量模式收尾冲刷（fsync 性能开关）：三出口统一先冲脏批——
    // Completed/Paused 需完整 resume 凭据；Err 出口 best-effort（冲刷
    // 失败不覆盖原错误，仅告警）。锁序与完成块一致（manager → batch）。
    let flush_final = |policy: SegmentSyncPolicy| -> Result<(), String> {
        if policy.per_segment {
            return Ok(());
        }
        let m = manager.lock();
        let mut b = batch_state.lock();
        if !b.dirty {
            return Ok(());
        }
        flush_batch(&mut b, part, &m, &ledger_handle, total)
    };
    let mut paused = false;
    while let Some(res) = workers.join_next().await {
        match res {
            Ok(Ok(WorkerExit::Drained)) => {}
            Ok(Ok(WorkerExit::Paused)) => paused = true,
            Ok(Err(e)) => {
                workers.abort_all();
                // best-effort 冲刷：已写数据尽量入账（失败仅告警，
                // 不覆盖原错误——账本落后数据是安全方向）
                if let Err(fe) = flush_final(sync) {
                    tracing::warn!("批量收尾冲刷失败（忽略，原错误优先）: {fe}");
                }
                return Err(e);
            }
            Err(e) => {
                workers.abort_all();
                if let Err(fe) = flush_final(sync) {
                    tracing::warn!("批量收尾冲刷失败（忽略，原错误优先）: {fe}");
                }
                return Err(format!("worker panicked: {e}"));
            }
        }
    }
    // 正常收工（Completed/Paused）：冲刷失败按错误传播（与段级 fsync
    // 失败同口径——磁盘故障应被看见，而非静默交出一个未落盘的完成态）
    flush_final(sync)?;
    if paused {
        return Ok(DynamicOutcome::Paused);
    }
    Ok(DynamicOutcome::Completed)
}

/// 更新 mirror 评分（成功 +delta / 失败惩罚，clamp [SCORE_MIN, SCORE_MAX]）。
/// `pub(crate)`：engine 的 update_sources 探测评分播种复用同一 clamp 口径。
pub(crate) fn update_score(scores: &Mutex<HashMap<String, i64>>, url: &str, delta: i64) {
    let mut m = scores.lock();
    let s = m.entry(url.to_string()).or_insert(0);
    *s = (*s + delta).clamp(SCORE_MIN, SCORE_MAX);
}

/// 段下载 + 失败退避重试（UNWIRED #1 接线，P1 粒度缩小的前置换 D25 退避）：
/// 可重试错误（408/429/5xx/传输/截断）先指数退避重试同段（Retry-After
/// 优先，1/2/4s ×4 次，jitter 防惊群）；耗尽或不可重试且可拆
///（len/2 >= MIN_RETRY_GRANULARITY）→ 拆半重试（对 5xx/传输仍有用）；
/// 429 永不拆分（旧实现 429 → 拆半 → 2× 请求 → 再 429 → 再拆半，
/// 正反馈放大请求量——UNWIRED #1 的核心病灶）。
///
/// 总时延有界设计（task_proxy e2e 暴露的放大缺陷）：退避阶梯只在「根段」
/// 生效；拆半子段单发不退避（= 旧口径）。若子段也带满额阶梯，等待时间
/// 随拆分深度 2^n 放大（4 次 × 7s × 2^depth）；根段 4 次退避 + 子段树
/// 零等待快速失败，总时延上界 ≈ 单段阶梯 7s + 请求开销。
/// 迭代式拆分栈（避免 async 递归装箱）；栈元素 = (段, 是否根段)。
async fn download_segment_with_retry(
    client: &reqwest::Client,
    url: &str,
    part: &Path,
    seg: crate::segment_manager::Segment,
    headers: &[(String, String)],
    limiter: &RateLimiter,
    sync: SegmentSyncPolicy,
) -> Result<(), String> {
    let backoff = crate::retry::Backoff::default();
    let mut stack: Vec<(crate::segment_manager::Segment, bool)> = vec![(seg, true)];
    while let Some((cur, is_root)) = stack.pop() {
        let max_attempts = if is_root { SEGMENT_MAX_ATTEMPTS } else { 1 };
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            match download_segment_streaming(client, url, part, cur, headers, limiter, sync).await {
                Ok(()) => break,
                Err(e) if attempt < max_attempts && e.is_retryable(attempt) => {
                    let delay = e
                        .retry_after_hint()
                        .unwrap_or_else(|| backoff.next_delay(attempt));
                    let wait = delay + backoff_jitter();
                    tracing::warn!(
                        "段 [{},{}] 第 {attempt} 次失败（{e}），{:?} 后重试",
                        cur.start,
                        cur.end,
                        wait
                    );
                    tokio::time::sleep(wait).await;
                }
                Err(e) if e.may_split() && cur.len() / 2 >= MIN_RETRY_GRANULARITY => {
                    let mid = cur.start + cur.len() / 2;
                    // 先压 right 再压 left → 先处理 left，与递归顺序一致；
                    // 子段 is_root=false：单发不退避（总时延有界设计）
                    stack.push((
                        crate::segment_manager::Segment {
                            start: mid,
                            end: cur.end,
                        },
                        false,
                    ));
                    stack.push((
                        crate::segment_manager::Segment {
                            start: cur.start,
                            end: mid - 1,
                        },
                        false,
                    ));
                    break;
                }
                Err(e) => return Err(format!("segment [{}, {}]: {e}", cur.start, cur.end)),
            }
        }
    }
    Ok(())
}

/// 段级错误分类（UNWIRED #1）：状态码/Retry-After 在段边界不再被 String
/// 抹平——旧实现 `Err(String)` 丢弃状态码后，429 与 5xx/传输错误同走
/// 拆半路径，重试退避（retry.rs，已实现但零调用）无从接入。
#[derive(Debug, Clone)]
enum SegmentError {
    /// HTTP 状态码（非 206/200）+ Retry-After 秒数（若服务器提供且可解析）。
    Status(u16, Option<u64>),
    /// 网络/IO/长度截断（可重试可拆分：连接中断重连、磁盘瞬态、体截断）。
    Transport(String),
    /// 206 Content-Range 与请求区间错位（不可重试；拆半保留旧口径）。
    RangeMismatch(String),
}

/// 同段重试上限（D25 ×4：4 次尝试，间以 1/2/4s 退避）。
const SEGMENT_MAX_ATTEMPTS: u32 = 4;

/// Retry-After 钉制上限：恶意/异常大值（如 86400）会把任务睡眠挂死，
/// 超过郎按缺省退避处理。
const RETRY_AFTER_CAP_SECS: u64 = 60;

impl SegmentError {
    /// 是否值得同段重试（D25：408/429/5xx；403/404 终态）。
    fn is_retryable(&self, attempt: u32) -> bool {
        match self {
            SegmentError::Status(code, _) => {
                crate::retry::should_retry(*code, attempt, SEGMENT_MAX_ATTEMPTS)
            }
            SegmentError::Transport(_) => attempt < SEGMENT_MAX_ATTEMPTS,
            SegmentError::RangeMismatch(_) => false,
        }
    }

    /// 服务器指定等待（Retry-After，已钳制）；无则 None → 调用方用指数退避。
    fn retry_after_hint(&self) -> Option<std::time::Duration> {
        match self {
            SegmentError::Status(_, Some(secs)) => Some(std::time::Duration::from_secs(
                (*secs).min(RETRY_AFTER_CAP_SECS),
            )),
            _ => None,
        }
    }

    /// 耗尽重试后是否允许拆半：429 拆半 = 请求量正反馈放大（核心病灶），
    /// 永不拆；其余错误保留旧拆半口径（5xx/传输对更小区间仍可能成功）。
    fn may_split(&self) -> bool {
        !matches!(self, SegmentError::Status(429, _))
    }

    fn transport(e: impl std::fmt::Display) -> Self {
        SegmentError::Transport(e.to_string())
    }
}

impl std::fmt::Display for SegmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SegmentError::Status(code, Some(ra)) => {
                write!(f, "status {code} (Retry-After: {ra}s)")
            }
            SegmentError::Status(code, None) => write!(f, "status {code}"),
            SegmentError::Transport(e) => write!(f, "transport: {e}"),
            SegmentError::RangeMismatch(e) => write!(f, "range mismatch: {e}"),
        }
    }
}

/// 退避抖动（0-500ms）：SystemTime 亚秒纳秒取模——httpdl 生产依赖零新增
///（dev 侧的 rand 仅供测试）。防并发 worker 同步重试惊群。
fn backoff_jitter() -> std::time::Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    std::time::Duration::from_nanos((nanos % 500_000_000) as u64)
}

/// 解析 `Retry-After` 响应头（仅数字秒形式；HTTP-date 形式解析失败 → None
/// → 调用方回落指数退避）。钳制见 RETRY_AFTER_CAP_SECS。
fn retry_after_secs(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|s| s.min(RETRY_AFTER_CAP_SECS))
}

/// 单段流式下载：Range: bytes=start-end，chunk 边收边写 .part
/// （段内顺序写，seek 一次定位；段不相交 → 与其它 worker 无写冲突）。
/// `headers` = 任务级自定义头（H-8）：逐个追加；同名 `range` 头（大小写
/// 不敏感）跳过——段区间由段参数生成，任务头不得劫持/制造重复 Range。
async fn download_segment_streaming(
    client: &reqwest::Client,
    url: &str,
    part: &Path,
    seg: crate::segment_manager::Segment,
    headers: &[(String, String)],
    limiter: &RateLimiter,
    sync: SegmentSyncPolicy,
) -> Result<(), SegmentError> {
    let mut req = client.get(url);
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("range") {
            continue;
        }
        req = req.header(k, v);
    }
    let mut resp = req
        .header(
            reqwest::header::RANGE,
            format!("bytes={}-{}", seg.start, seg.end),
        )
        .send()
        .await
        .map_err(SegmentError::transport)?;
    // RFC 7233 §2.1：服务器可忽略 Range 返回 200 全量体。容错路径：
    // 200 → 丢弃前 seg.start 字节后顺序写 seg.len 字节（正确但浪费带宽，
    // 仅非 206 服务器触发的兜底路径；206 主路径零浪费）。写满即弃流
    // （drop resp 断开连接），语义与账本/进度完全兼容。
    let skip_start = match resp.status() {
        reqwest::StatusCode::PARTIAL_CONTENT => {
            // batch3-P1：206 必须校验 Content-Range 首字节 == 段起点。旧实现
            // 无条件信任 206 与请求区间一致——服务器/中间缓存错位应答（含
            // 「bytes 0-N/…」归一化劣化）会把数据写到错误偏移，账本/长度全部
            // 自洽，仅哈希校验能事后兜底。不符按失败处理（走段重试/降粒度）。
            let cr_start = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range_start);
            match cr_start {
                Some(st) if st == seg.start => 0u64,
                // 缺失头宽容放行（保持旧行为：RFC 要求 206 必带，但部分轻量
                // 服务器省略；只拒绝「有头但与请求区间不符」的明确错位）。
                None => 0u64,
                other => {
                    return Err(SegmentError::RangeMismatch(format!(
                        "request start {}, got {other:?}",
                        seg.start
                    )))
                }
            }
        }
        reqwest::StatusCode::OK => seg.start,
        // UNWIRED #1：状态码 + Retry-After 在段边界保留（旧实现抹平成
        // String，重试决策无从区分 429/5xx/终态）。
        other => {
            return Err(SegmentError::Status(
                other.as_u16(),
                retry_after_secs(&resp),
            ))
        }
    };
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(part)
        .map_err(|e| SegmentError::Transport(format!("part open: {e}")))?;
    f.seek(SeekFrom::Start(seg.start))
        .map_err(SegmentError::transport)?;
    let mut written: u64 = 0;
    let mut skipped: u64 = 0;
    loop {
        let chunk = resp.chunk().await.map_err(SegmentError::transport)?;
        let Some(chunk) = chunk else { break };
        if skipped < skip_start {
            let take = ((skip_start - skipped) as usize).min(chunk.len());
            skipped += take as u64;
            if take < chunk.len() {
                let rest = &chunk[take..];
                let n = rest.len().min((seg.len() - written) as usize);
                limiter.wait(n as u64).await;
                f.write_all(&rest[..n]).map_err(SegmentError::transport)?;
                written += n as u64;
            }
        } else {
            let n = chunk.len().min((seg.len() - written) as usize);
            limiter.wait(n as u64).await;
            f.write_all(&chunk[..n]).map_err(SegmentError::transport)?;
            written += n as u64;
        }
        if written >= seg.len() {
            break; // 写满即停：200 全量体的剩余部分（若有）主动弃流
        }
    }
    if written != seg.len() {
        // 传输截断：可重试（连接中断重连后同段重下）
        return Err(SegmentError::Transport(format!(
            "segment length {} != expected {}",
            written,
            seg.len()
        )));
    }
    // P0-4 修复（崩溃一致性层）：段数据 fsync 后才允许调用方记账——
    // 原顺序「write_all（页缓存）→ 账本 save」在进程崩溃时产生空洞：
    // 账本已记段完成但页缓存数据未落盘 → 重启 resume 跳过该段 →
    // 交付长度正确但内容全零的文件且状态 Completed。WAL 语义要求
    // 数据先于账本落盘。sync_data（不含 metadata：段内 seek+write
    // 不改文件大小，set_len 已在循环外完成）。
    //
    // fsync 性能开关（PR #114 审查 §3.3）：批量模式（per_segment=false）
    // 下本处跳过——数据 fsync 与记账一起推迟到批边界（flush_batch），
    // WAL 顺序不变（数据 fsync 先于账本落盘），仅粒度从段放宽到批。
    // 机械盘上每段 ~13ms 的 fsync 支配吞吐，批量化将其摊销到 N 段一次。
    if sync.per_segment {
        f.sync_data()
            .map_err(|e| SegmentError::Transport(format!("segment fsync: {e}")))?;
    }
    Ok(())
}

/// batch3-P1：解析 `Content-Range: bytes START-END/TOTAL` 的 START。
/// `bytes */TOTAL`（unsatisfied）返回 None。
pub(crate) fn parse_content_range_start(v: &str) -> Option<u64> {
    // 兼容 RFC 7233 标准格式 `bytes START-END/TOTAL`（空格分隔）与常见
    // 非标准写法 `bytes=START-END/TOTAL`（等号紧贴）。旧实现只认等号格式，
    // 标准格式的 Content-Range 恒解析为 None → 错位 206 被宽容放行。
    let rest = v.trim().strip_prefix("bytes")?.trim();
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    let unit = rest.split('/').next()?.trim();
    let start = unit.split('-').next()?.trim();
    start.parse().ok()
}

#[cfg(test)]
mod sync_policy_tests {
    use super::*;

    #[test]
    fn default_is_per_segment_wal_strict() {
        // 默认必须保持 P0-4 语义（每段 fsync）：机械盘用户显式改配置才降级
        let p = SegmentSyncPolicy::default();
        assert!(p.per_segment);
        assert_eq!(p.batch_segments, 8);
        assert_eq!(p.batch_interval, std::time::Duration::from_secs(2));
    }

    #[test]
    fn should_flush_segment_threshold() {
        let p = SegmentSyncPolicy {
            per_segment: false,
            batch_segments: 3,
            batch_interval: std::time::Duration::from_secs(3600),
        };
        // 计时远未到：纯段数驱动
        let t0 = std::time::Duration::from_millis(1);
        assert!(!p.should_flush(1, t0));
        assert!(!p.should_flush(2, t0));
        assert!(p.should_flush(3, t0), "达到段阈值必须触发");
        assert!(p.should_flush(7, t0), "超过阈值恒触发");
    }

    #[test]
    fn should_flush_interval_threshold() {
        let p = SegmentSyncPolicy {
            per_segment: false,
            batch_segments: 1000,
            batch_interval: std::time::Duration::from_millis(500),
        };
        let t_short = std::time::Duration::from_millis(100);
        let t_over = std::time::Duration::from_millis(600);
        assert!(!p.should_flush(1, t_short));
        assert!(
            p.should_flush(1, t_over),
            "间隔超时即使只有 1 段也必须触发（防脏数据无限滞留页缓存）"
        );
    }

    #[test]
    fn should_flush_never_fires_on_clean_state() {
        let p = SegmentSyncPolicy::default();
        // pending=0（无脏段）恒不触发：不空转 fsync
        assert!(!p.should_flush(0, std::time::Duration::from_secs(999)));
    }

    #[test]
    fn per_segment_mode_should_flush_is_unused_but_consistent() {
        // per_segment 模式不走 should_flush 路径；语义一致性锚：
        // 批量阈值定义独立于模式位，纯函数口径不随模式漂移
        let p = SegmentSyncPolicy::default();
        assert!(p.should_flush(p.batch_segments, std::time::Duration::ZERO));
    }
}

#[cfg(test)]
mod rotation_tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn single_mirror_rotation_is_identity() {
        let m = v(&["a"]);
        for w in 0..4 {
            assert_eq!(rotate_mirrors(&m, w), m);
        }
        // 空表（防御）恒空
        assert!(rotate_mirrors(&[], 3).is_empty());
    }

    #[test]
    fn dual_mirror_rotation_spreads_workers() {
        let m = v(&["a", "b"]);
        assert_eq!(rotate_mirrors(&m, 0), v(&["a", "b"]));
        assert_eq!(rotate_mirrors(&m, 1), v(&["b", "a"]));
        assert_eq!(rotate_mirrors(&m, 2), v(&["a", "b"]));
        assert_eq!(rotate_mirrors(&m, 3), v(&["b", "a"]));
        // 三源：起点 0/1/2 循环
        let m3 = v(&["a", "b", "c"]);
        assert_eq!(rotate_mirrors(&m3, 1), v(&["b", "c", "a"]));
        assert_eq!(rotate_mirrors(&m3, 2), v(&["c", "a", "b"]));
        // 轮转结果恒为原表重排（无丢失/重复）
        for w in 0..6 {
            let mut r = rotate_mirrors(&m3, w);
            r.sort();
            assert_eq!(r, m3);
        }
    }
}

#[cfg(test)]
mod segment_error_tests {
    use super::*;

    #[test]
    fn status_429_retryable_then_never_splits() {
        // UNWIRED #1 核心：429 可重试（退避）但永不拆半
        let e = SegmentError::Status(429, None);
        assert!(e.is_retryable(1));
        assert!(e.is_retryable(3), "第 3 次失败仍可重试（共 4 次）");
        assert!(!e.is_retryable(4), "第 4 次用尽 → 不再重试");
        assert!(!e.may_split(), "429 永不拆半（正反馈放大病灶）");
    }

    #[test]
    fn status_5xx_retryable_and_splittable_after_exhaustion() {
        let e = SegmentError::Status(503, None);
        assert!(e.is_retryable(1));
        assert!(!e.is_retryable(4));
        assert!(e.may_split(), "5xx 耗尽后拆半仍有效（旧口径保留）");
    }

    #[test]
    fn terminal_statuses_fail_fast() {
        // 403/404：终态——不重试（mirror 轮换/任务失败由上层处理）
        for code in [400u16, 401, 403, 404, 416] {
            let e = SegmentError::Status(code, None);
            assert!(!e.is_retryable(1), "{code} 不应重试");
        }
    }

    #[test]
    fn transport_retryable_and_splittable() {
        let e = SegmentError::Transport("connection reset".into());
        assert!(e.is_retryable(1));
        assert!(!e.is_retryable(4));
        assert!(e.may_split());
    }

    #[test]
    fn range_mismatch_is_never_retried() {
        let e = SegmentError::RangeMismatch("offset drifted".into());
        assert!(!e.is_retryable(1), "错位重试同请求无意义");
        assert!(e.may_split(), "拆半口径保留（batch3-P1 设计）");
    }

    #[test]
    fn retry_after_hint_capped_at_60s() {
        // 恶意/异常大 Retry-After 钳制：86400 → 60s（不挂死任务）
        let e = SegmentError::Status(429, Some(86400));
        assert_eq!(
            e.retry_after_hint(),
            Some(std::time::Duration::from_secs(60))
        );
        let e2 = SegmentError::Status(429, Some(2));
        assert_eq!(
            e2.retry_after_hint(),
            Some(std::time::Duration::from_secs(2))
        );
        // 无 Retry-After → None（调用方走指数退避）
        assert_eq!(SegmentError::Status(429, None).retry_after_hint(), None);
        assert_eq!(SegmentError::Transport("x".into()).retry_after_hint(), None);
    }

    #[test]
    fn jitter_stays_within_500ms() {
        for _ in 0..200 {
            let j = backoff_jitter();
            assert!(j <= std::time::Duration::from_millis(500));
        }
    }

    #[test]
    fn display_carries_status_and_retry_after() {
        let e = SegmentError::Status(429, Some(3));
        let s = format!("{e}");
        assert!(s.contains("429") && s.contains("3"), "Display: {s}");
    }
}
