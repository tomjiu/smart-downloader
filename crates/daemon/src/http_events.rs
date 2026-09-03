//! 引擎状态轮询循环（state 推进 + 速率缓存）：daemon 侧定期轮询活跃任务引擎状态 →
//! 记录状态机推进（Queued → Downloading，→ Completed/Failed）+ 事件广播；
//! 同时把引擎快照写入 `engine_status` 缓存（E11——`/stats` 聚合速率数据源）。
//!
//! 背景：v1 HTTP 引擎为薄接入（add 后 fire-and-forget，无 alert 回调），任务记录 state
//! 此前停在 Queued——`list` 与 `status` 不一致（status 实时查引擎、list 读记录）。
//! 本循环补上推进路径：权威 = 引擎实时状态（`DaemonState::poll_engine_states`）。

use std::sync::Arc;
use std::time::Duration;

use smart_dl_core::state_machine::TaskState;

use crate::events::SchedulerEvent;
use crate::state::DaemonState;

/// 启动 HTTP 状态推进循环：每 `interval` 轮询一批 → 应用迁移 → 广播事件。
/// 引擎不可用/任务已移除静默跳过。
pub fn spawn_http_events(
    state: Arc<DaemonState>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            for effect in state.poll_engine_states().await {
                let hub = state.hub();
                hub.publish(SchedulerEvent::StateChanged {
                    task_id: effect.task_id.clone(),
                    from: effect.from,
                    to: effect.to.clone(),
                });
                match &effect.to {
                    TaskState::Completed => {
                        hub.publish(SchedulerEvent::Completed {
                            task_id: effect.task_id,
                        });
                    }
                    TaskState::Failed => {
                        hub.publish(SchedulerEvent::Failed {
                            task_id: effect.task_id,
                            reason: effect.message,
                        });
                    }
                    _ => {}
                }
            }
        }
    })
}
