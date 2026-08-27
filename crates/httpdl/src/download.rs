//! 动态分段下载器（P0，方案A）：worker 池经 SegmentManager 按 FIFO 动态领取
//! 段，段内流式写盘（seek+write，段不相交 → 并发写无锁）。
//! 失败语义：任一段全 mirror 失败 → 整体 Err（不做部分成功利用，P0 约定）。

use crate::rate::RateLimiter;
use crate::segment_manager::SegmentManager;
use crate::static_split::segment_count;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// 动态分段下载：worker 数 N=clamp(total/64MB, 2, 8)，段粒度 min_split。
/// `offset` = 续传起点（跳过 [0, offset)，由调用方续传决策给出）。
/// 任一段全源失败 → Err（调用方决定重试/报错）。`limiter` 跨段共享（0 = 不限）。
pub async fn download_dynamic(
    client: &reqwest::Client,
    part: &Path,
    total: u64,
    offset: u64,
    min_split: u64,
    mirrors: &[String],
    limiter: Arc<RateLimiter>,
) -> Result<(), String> {
    // 预分配 .part（续传场景：旧 .part 保留，只写缺失段；不截断）
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(part)
        .map_err(|e| e.to_string())?;
    f.set_len(total).map_err(|e| e.to_string())?;
    drop(f);

    let manager = Arc::new(Mutex::new(SegmentManager::new(total, offset, min_split)));
    let n_workers = segment_count(total);
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..n_workers {
        let client = client.clone();
        let part = part.to_path_buf();
        let mirrors = mirrors.to_vec();
        let limiter = limiter.clone();
        let manager = manager.clone();
        workers.spawn(async move {
            loop {
                let seg = {
                    let mut m = manager.lock().unwrap();
                    match m.take_segment() {
                        Some(s) => s,
                        None => return Ok::<(), String>(()),
                    }
                };
                let mut ok = false;
                for url in &mirrors {
                    match download_segment_with_retry(&client, url, &part, seg, &limiter).await {
                        Ok(()) => {
                            ok = true;
                            break;
                        }
                        Err(_) => continue, // 下一 mirror（粒度已缩到最小仍失败）
                    }
                }
                if !ok {
                    return Err(format!(
                        "all mirrors failed for segment [{}, {}]",
                        seg.start, seg.end
                    ));
                }
                manager.lock().unwrap().complete(seg);
            }
        });
    }
    // 任一 worker 失败 → 整体失败，取消其余
    while let Some(res) = workers.join_next().await {
        match res {
            Ok(Ok(())) => {}
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
    Ok(())
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
    limiter: &RateLimiter,
) -> Result<(), String> {
    let mut stack: Vec<crate::segment_manager::Segment> = vec![seg];
    while let Some(cur) = stack.pop() {
        match download_segment_streaming(client, url, part, cur, limiter).await {
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
async fn download_segment_streaming(
    client: &reqwest::Client,
    url: &str,
    part: &Path,
    seg: crate::segment_manager::Segment,
    limiter: &RateLimiter,
) -> Result<(), String> {
    let mut resp = client
        .get(url)
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
