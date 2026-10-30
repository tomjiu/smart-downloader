//! ABBA 死锁回归（Issue #121 B-1 修复判别锚，repro/round23 实函数版）。
//!
//! PR #114 催修 P0-2：`attach_reserved_handle` 原实现「先锁
//! slot_reservations 再锁 tasks」与 `gate_or_enqueue`（tasks →
//! slot_reservations）方向相反——并发 add 时构成 ABBA 环。
//!
//! 本测试与 repro/round23 的区别：round23 复刻 State 结构自证；
//! 本处直接压测 **DaemonState 的真实函数**（`gate_or_enqueue` ↔
//! `attach_reserved_handle` 并发交错），锁序回归即在此显形。
//!
//! 判别（已双向验证）：把 attach 的锁序临时改回修复前反序
//!（持 slot_reservations 时取 tasks），本测试在数秒内死锁超时；
//! 修复后锁序下任意迭代次数全部完成。
//!
//! 并发正确性的完整故事（三个互补层次）：
//! 1. 本测试：真实函数微基准（锁窗口密度对齐 repro——判别力最强）
//! 2. tests/concurrency_deadlock.rs b1：HTTP 层实机并发（真实请求
//!    时序，窗口窄但形态真——守恒断言 + 超时护栏）
//! 3. queue_gate_tests：超卖/递补的业务语义（配额不变量）

use super::*;
use std::sync::Arc;
use std::time::Duration;

/// 最小 DownloadTask（gate/attach 只读 id/state/dest 等字段，引擎零参与）。
fn task_of(id: &str) -> DownloadTask {
    DownloadTask {
        id: id.into(),
        canonical_id: CanonicalId {
            kind: CanonicalKind::Http,
            identity: format!("https://x/{id}.bin"),
            validator: None,
            token_sensitive: false,
        },
        source: DownloadSource::Http {
            url: format!("https://x/{id}.bin"),
            headers: vec![],
            auth: None,
            backup_url: None,
            proxy: None,
        },
        identity: ContentIdentity::SingleFile {
            size: 0,
            etag: None,
            sha256: None,
            sha1: None,
            md5: None,
            backup_md5: None,
        },
        dest_root: PathBuf::from("."),
        files: vec![],
        acquisitions: vec![],
        aggregate: Default::default(),
        state: TaskState::Queued,
        retry: RetryState {
            retries: 0,
            max_retries: 0,
        },
        created_at: std::time::Instant::now(),
        file_priorities: None,
        sequential: false,
        queue_priority: 0,
        metadata: TaskMetadata {
            name: None,
            added_at_unix: 0,
            added_at_ms: 0,
            tags: Vec::new(),
            finished_at_unix: 0,
            start_at_unix: 0,
            next_retry_at_unix: 0,
        },
        limits: None,
        max_connections: None,
        max_uploads: None,
    }
}

/// 真实函数并发压测：gate_or_enqueue（tasks → slot）与
/// attach_reserved_handle（修复后同序；修复前反序）8 线程 × 高频交错。
///
/// 实现口径（repro/round23 同构）：**std::thread** 而非 tokio task——
/// parking_lot 死锁是 OS 级线程阻塞，若压测任务占满 tokio worker，
/// runtime timer 饿死、`tokio::time::timeout` 永不触发（实测复现）。
/// 进度计数 + 主线程 deadline 轮询使死锁变成「失败」而非挂死
///（原则 3）；死锁线程泄漏给测试进程（round23 同款处置）。
#[test]
fn abba_gate_attach_real_functions_no_deadlock() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake, vec![]);
    let state = Arc::new(state);

    const THREADS: usize = 8;
    const ITERS: usize = 500;
    let progress = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let total = THREADS * ITERS;

    for t in 0..THREADS {
        let s = state.clone();
        let p = progress.clone();
        // attach_reserved_handle 是 async fn：std 线程内用最小 current_thread
        // runtime 驱动（每次迭代一个块级 runtime，死锁阻塞在线程上而非
        // 测试主线程——进度停滞由主线程 deadline 轮询检出）
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            for k in 0..ITERS {
                let id = format!("t{t}-{k}");
                let task = task_of(&id);
                // 路径 A：gate（tasks → slot_reservations）
                if s.gate_or_enqueue(&task, EngineKind::Http) {
                    // 路径 B：attach（修复后 tasks → slot_reservations 同序；
                    // 修复前反序 slot → tasks —— 与 A 并发即 ABBA 环）
                    rt.block_on(s.attach_reserved_handle(
                        &id,
                        format!("et-{t}-{k}"),
                        EngineKind::Http,
                        &TaskState::Queued,
                    ));
                    // 收尾：摘除占位（下一轮不膨胀 tasks 表）
                    s.tasks.lock().remove(&id);
                }
                p.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });
    }

    // 主线程 deadline 轮询：死锁 → 进度停滞 → 超时失败（不挂死）
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let done = progress.load(std::sync::atomic::Ordering::Relaxed);
        if done >= total {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "压测超时 —— 疑似死锁（gate/attach 锁序回归？）完成 {done}/{total}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 守恒不变量（超卖防线）：并发 gate+attach 全程配额不破。
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn abba_concurrent_gate_never_oversells_quota() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake, vec![]).with_queue_cfg(crate::config::QueueCfg {
        max_active_bt: 0,
        max_active_http: 3,
        max_active_ftp: 0,
    });
    let state = Arc::new(state);

    let mut handles = Vec::new();
    for t in 0..8usize {
        let s = state.clone();
        handles.push(tokio::spawn(async move {
            for k in 0..60usize {
                let id = format!("q{t}-{k}");
                let task = task_of(&id);
                if s.gate_or_enqueue(&task, EngineKind::Http) {
                    s.attach_reserved_handle(
                        &id,
                        format!("et-{t}-{k}"),
                        EngineKind::Http,
                        &TaskState::Queued,
                    )
                    .await;
                }
                // 守恒：任意时刻「已接引擎 + 已预留」≤ 配额 3
                let counts = s.active_slot_counts();
                assert!(
                    counts[1] <= 3,
                    "HTTP 槽位超卖：{} > 3（t={t} k={k}）",
                    counts[1]
                );
            }
        }));
    }
    for r in futures::future::join_all(handles).await {
        r.expect("worker 不得 panic");
    }
}
