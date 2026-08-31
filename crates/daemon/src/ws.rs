//! WsHub（§12 D36）：事件发布中枢——monotonic seq + 有界队列（默认 256）+ 背压
//! 丢最旧非关键；drain 全量重同步（掉队客户端重连）；snapshot_upto 跳号补拉。
//! 真 WS 升级端点在 http 层；本模块是纯逻辑，测试直连。

use crate::events::{Envelope, SchedulerEvent};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

/// 默认事件队列上限（D36：队列 256）。
pub const DEFAULT_CAPACITY: usize = 256;

pub struct WsHub {
    seq: AtomicU64,
    cap: usize,
    queue: Mutex<VecDeque<Envelope>>,
}

impl Default for WsHub {
    fn default() -> Self {
        WsHub::new()
    }
}

impl WsHub {
    pub fn new() -> Self {
        WsHub::with_capacity(DEFAULT_CAPACITY)
    }

    pub fn with_capacity(cap: usize) -> Self {
        WsHub {
            seq: AtomicU64::new(0),
            cap,
            queue: Mutex::new(VecDeque::with_capacity(cap)),
        }
    }

    /// 发布事件：seq 严格递增；队列满 → 丢最旧非关键事件；队内全关键时
    /// 关键事件绝不移出（新非关键事件丢弃；新关键事件丢队头兜底防死锁）。
    pub fn publish(&self, event: SchedulerEvent) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let mut q = self.queue.lock();
        if q.len() >= self.cap {
            match q.iter().position(|e| !e.event.is_critical()) {
                Some(i) => {
                    q.remove(i);
                    q.push_back(Envelope { seq, event });
                }
                None if event.is_critical() => {
                    q.pop_front();
                    q.push_back(Envelope { seq, event });
                }
                None => {
                    // 队内全关键 + 新事件非关键 → 丢新事件，关键全部保留
                }
            }
        } else {
            q.push_back(Envelope { seq, event });
        }
    }

    /// 消费全量（幂等：调用后队列清空）——掉队客户端重连重同步入口。
    pub fn drain(&self) -> Vec<Envelope> {
        let mut q = self.queue.lock();
        q.drain(..).collect()
    }

    /// 补拉 seq > last_seen 的事件（客户端发现跳号 → GET /tasks/:id 前先补事件）。
    pub fn snapshot_upto(&self, last_seen: u64) -> Vec<Envelope> {
        self.queue
            .lock()
            .iter()
            .filter(|e| e.seq > last_seen)
            .cloned()
            .collect()
    }

    /// 已发布的最大 seq。
    pub fn last_seq(&self) -> u64 {
        self.seq.load(Ordering::SeqCst)
    }

    /// 当前队列长度（背压观测）。
    pub fn len(&self) -> usize {
        self.queue.lock().len()
    }

    /// 队列是否为空。
    pub fn is_empty(&self) -> bool {
        self.queue.lock().is_empty()
    }
}

/// 1s 快照节流器（D36）：Progress/Speed 属高频率状态快照，按任务合并为最新值，
/// 每 ≥1s 批量 flush 一次；其他事件（关键/状态/新建等）不入节流、直通。
/// 被合并的事件 seq 跳号 → 客户端按 D36 用 GET /tasks/:id 拉快照补齐。
pub struct Throttler {
    pending: std::collections::HashMap<(String, &'static str), Envelope>,
}

impl Default for Throttler {
    fn default() -> Self {
        Self::new()
    }
}

impl Throttler {
    pub fn new() -> Self {
        Throttler {
            pending: std::collections::HashMap::new(),
        }
    }

    /// 是否属可节流的快照类事件（Progress/Speed）。
    pub fn is_throttlable(event: &SchedulerEvent) -> bool {
        matches!(
            event,
            SchedulerEvent::Progress { .. } | SchedulerEvent::Speed { .. }
        )
    }

    fn slot(event: &SchedulerEvent) -> Option<(String, &'static str)> {
        match event {
            SchedulerEvent::Progress { task_id, .. } => Some((task_id.clone(), "progress")),
            SchedulerEvent::Speed { task_id, .. } => Some((task_id.clone(), "speed")),
            _ => None,
        }
    }

    /// 合并：同一 (task, kind) 槽位保留最新事件（覆盖旧值）。
    pub fn upsert(&mut self, env: Envelope) {
        if let Some(key) = Self::slot(&env.event) {
            self.pending.insert(key, env);
        }
    }

    /// 取走全部积压（flush 后清空）。
    pub fn drain_pending(&mut self) -> Vec<Envelope> {
        self.pending.drain().map(|(_, env)| env).collect::<Vec<_>>()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::SchedulerEvent;

    fn env(seq: u64, event: SchedulerEvent) -> Envelope {
        Envelope { seq, event }
    }

    #[test]
    fn progress_and_speed_throttlable_others_not() {
        assert!(Throttler::is_throttlable(&SchedulerEvent::Progress {
            task_id: "t1".into(),
            done: 1,
            total: 2,
        }));
        assert!(Throttler::is_throttlable(&SchedulerEvent::Speed {
            task_id: "t1".into(),
            down_rate: 1,
            up_rate: 0,
        }));
        assert!(!Throttler::is_throttlable(&SchedulerEvent::TaskCreated {
            task_id: "t1".into(),
        }));
        assert!(!Throttler::is_throttlable(&SchedulerEvent::Completed {
            task_id: "t1".into(),
        }));
    }

    #[test]
    fn same_slot_keeps_latest() {
        let mut t = Throttler::new();
        t.upsert(env(
            1,
            SchedulerEvent::Progress {
                task_id: "t1".into(),
                done: 10,
                total: 100,
            },
        ));
        t.upsert(env(
            2,
            SchedulerEvent::Progress {
                task_id: "t1".into(),
                done: 99,
                total: 100,
            },
        ));
        let flushed = t.drain_pending();
        assert_eq!(flushed.len(), 1);
        match &flushed[0].event {
            SchedulerEvent::Progress { done, .. } => assert_eq!(*done, 99),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn distinct_tasks_keep_separate_slots() {
        let mut t = Throttler::new();
        t.upsert(env(
            1,
            SchedulerEvent::Progress {
                task_id: "t1".into(),
                done: 1,
                total: 2,
            },
        ));
        t.upsert(env(
            2,
            SchedulerEvent::Progress {
                task_id: "t2".into(),
                done: 3,
                total: 4,
            },
        ));
        // 同任务 Progress 与 Speed 也各自有槽
        t.upsert(env(
            3,
            SchedulerEvent::Speed {
                task_id: "t1".into(),
                down_rate: 5,
                up_rate: 0,
            },
        ));
        assert_eq!(t.drain_pending().len(), 3);
    }

    #[test]
    fn non_throttlable_never_queued() {
        let mut t = Throttler::new();
        t.upsert(env(
            1,
            SchedulerEvent::TaskCreated {
                task_id: "t1".into(),
            },
        ));
        assert!(t.is_empty());
    }
}
