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
use crate::segment_manager::SegmentManager;
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
/// 参数均为同层语义字段，聚合结构体反而增加调用方样板 → 允许 12 参。
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
    let n_workers = segment_count(total);
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
                    match download_segment_with_retry(&client, url, &part, seg, &headers, &limiter)
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
                // 保证账本视图与计数一致）
                {
                    let mut m = manager.lock();
                    m.complete(seg);
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
                    if let Some(p) = &progress {
                        p(m.done_bytes());
                    }
                }
                drop(_permit); // 显式释放：permit 生命周期必须覆盖 complete
            }
        });
    }
    // 任一 worker 失败 → 整体失败，取消其余；暂停退出优先于正常收工判定
    let mut paused = false;
    while let Some(res) = workers.join_next().await {
        match res {
            Ok(Ok(WorkerExit::Drained)) => {}
            Ok(Ok(WorkerExit::Paused)) => paused = true,
            Ok(Err(e)) => {
                workers.abort_all();
                return Err(e);
            }
            Err(e) => {
                workers.abort_all();
                return Err(format!("worker panicked: {e}"));
            }
        }
    }
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

/// 失败缩小粒度重试的最小粒度（P1）：低于该粒度不再拆分（对齐设计 §3.1 的 1MB 防碎片）。
const MIN_RETRY_GRANULARITY: u64 = 1024 * 1024;

/// 段下载 + 失败缩小粒度重试（P1）：整段全 mirror 失败时，若可拆（len/2 >= MIN_RETRY_GRANULARITY）
/// 则拆半重试；左右子段都成功才视为成功。子段写入各自区间（与整段写入等价）；
/// 已成功子段的字节不回收（后续重试覆盖写，语义无害）。迭代式拆分栈（避免 async 递归装箱）。
async fn download_segment_with_retry(
    client: &reqwest::Client,
    url: &str,
    part: &Path,
    seg: crate::segment_manager::Segment,
    headers: &[(String, String)],
    limiter: &RateLimiter,
) -> Result<(), String> {
    let mut stack: Vec<crate::segment_manager::Segment> = vec![seg];
    while let Some(cur) = stack.pop() {
        match download_segment_streaming(client, url, part, cur, headers, limiter).await {
            Ok(()) => {}
            Err(_) if cur.len() / 2 >= MIN_RETRY_GRANULARITY => {
                let mid = cur.start + cur.len() / 2;
                // 先压 right 再压 left → 先处理 left，与递归顺序一致
                stack.push(crate::segment_manager::Segment {
                    start: mid,
                    end: cur.end,
                });
                stack.push(crate::segment_manager::Segment {
                    start: cur.start,
                    end: mid - 1,
                });
            }
            Err(e) => return Err(format!("segment [{}, {}]: {e}", cur.start, cur.end)),
        }
    }
    Ok(())
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
) -> Result<(), String> {
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
        .map_err(|e| e.to_string())?;
    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(format!("segment status {}", resp.status()));
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(part)
        .map_err(|e| format!("part open: {e}"))?;
    f.seek(SeekFrom::Start(seg.start))
        .map_err(|e| e.to_string())?;
    let mut written: u64 = 0;
    loop {
        let chunk = resp.chunk().await.map_err(|e| e.to_string())?;
        let Some(chunk) = chunk else { break };
        limiter.wait(chunk.len() as u64).await;
        f.write_all(&chunk).map_err(|e| e.to_string())?;
        written += chunk.len() as u64;
    }
    if written != seg.len() {
        return Err(format!(
            "segment length {} != expected {}",
            written,
            seg.len()
        ));
    }
    Ok(())
}
