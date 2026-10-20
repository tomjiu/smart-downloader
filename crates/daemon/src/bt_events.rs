//! BT alert 事件流（feature `bt`）：后台轮询 btcore alert → 推进任务状态 + WS 事件广播。
//!
//! v1 范围：状态**主动推进**（State+Finished → Seeding，State+Error → Failed）+ 事件广播
//! （`Completed` / `Failed` / `StateChanged`）。进度/速度字段不在 alert 内，仍由快照
//! 实时拉取引擎状态提供——事件流只负责"被动拉取看不到的"终态推进与推送。
//!
//! 匹配键：`alert.ih` 对 `TaskRecord.engine_tid`（内核返回的原始大小写，比较时归一化）。

use std::sync::Arc;
use std::time::Duration;

use smart_dl_btcore::{Alert, AlertKind, StateSubKind};
use smart_dl_core::state_machine::TaskState;
use smart_dl_core::types::EngineKind;

use crate::events::SchedulerEvent;
use crate::state::DaemonState;

/// alert → 状态迁移的纯函数（不触碰共享状态，单测友好）。
/// 返回 `(from, to)`；无迁移返回 `None`。
pub fn transition_for(now: &TaskState, a: &Alert) -> Option<(TaskState, TaskState)> {
    match (&a.kind, a.state_subkind()) {
        // Bug C 修复：BT 任务在记录态 Paused 下若引擎实际完成（Bug A 复活后跑完），
        // 允许迁移到 Seeding，避免记录态与引擎态永久错位导致 fallback 误判进度。
        (AlertKind::State, StateSubKind::Finished)
            if matches!(
                now,
                TaskState::Downloading(EngineKind::Bt) | TaskState::Queued | TaskState::Paused
            ) =>
        {
            Some((now.clone(), TaskState::Seeding))
        }
        (AlertKind::State, StateSubKind::Error) if *now != TaskState::Failed => {
            Some((now.clone(), TaskState::Failed))
        }
        _ => None,
    }
}

/// 启动 BT alert 消费循环：每 `interval` 弹一批（≤128）→ 应用迁移 → 广播事件。
/// 会话 pop 失败（内核不可用）静默跳过；任务已移除/无迁移的 alert 丢弃。
///
/// `guard`（Bug A，调度层压制）：BtEngine 句柄，Metadata alert 到达且该任务
/// 带暂停意图时立即二次 pause，对冲 auto_managed 队列的自动恢复。
///
/// P0-3 根因修复：`guard` 收紧为必传（原 Option 的 None 分支虽在生产不可达
/// ——serve.rs 装配保证 bt_core=Some ⟺ bt_typed=Some——但该分支会在不置位
/// `alert_loop_active` 的情况下常驻 pop_alerts，令 save_fastresume 恒走
/// 自消费路径形成双消费者互吞）。同时：
/// - 旗标置位移出任何条件守卫，循环启动即置位；
/// - RAII 复位：循环 future 被 abort/正常结束时 Drop 自动清旗标，
///   save_fastresume 立即回到自 poll 模式（防「先 abort 后保存」的未来回归）。
pub fn spawn_alert_loop(
    state: Arc<DaemonState>,
    guard: Arc<crate::bt::BtEngine>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    // batch3-P1 + P0-3：置位常驻消费者旗标 → save_fastresume 纯等分发不自行 pop
    guard.mark_alert_loop_active();
    tokio::spawn(async move {
        // P0-3：RAII 复位——future 被 abort（serve.rs 退出序列）或正常结束时，
        // 旗标自动清零，后续 save_fastresume 不再空等已死分发者（每任务 3s）。
        struct ResetOnDrop(Arc<crate::bt::BtEngine>);
        impl Drop for ResetOnDrop {
            fn drop(&mut self) {
                self.0.clear_alert_loop_active();
            }
        }
        let _reset = ResetOnDrop(guard.clone());
        loop {
            // 审计修复（P2-8）：先工作后 sleep——原「先 sleep 后工作」在启动
            // 后留下一个 interval 的盲区（恢复任务的终态 alert 延迟首轮）。
            let alerts = guard.pop_alerts_enforcing_pause(128);
            for alert in &alerts {
                let Some(effect) = state.apply_bt_alert(alert) else {
                    continue;
                };
                let hub = state.hub();
                hub.publish(SchedulerEvent::StateChanged {
                    task_id: effect.task_id.clone(),
                    from: effect.from,
                    to: effect.to.clone(),
                });
                match &effect.to {
                    TaskState::Seeding => {
                        // E17：完成事件统一出口（广播 + Webhook；BT 下载完成 = 进做种）
                        state.publish_task_completed(&effect.task_id);
                    }
                    TaskState::Failed => {
                        hub.publish(SchedulerEvent::Failed {
                            task_id: effect.task_id.clone(),
                            reason: effect.message.clone(),
                        });
                    }
                    _ => {}
                }
            }
            tokio::time::sleep(interval).await;
        }
    })
}

/// 子文件优先级重放循环：周期性收敛「恢复时 metadata 未就绪」挂起的优先级
/// 任务（`DaemonState::replay_pending_file_priorities`）。pending 为空时为
/// 纯空转检查（锁一次即返回），默认 2s 粒度对恢复场景足够。
/// 审计修复（P2-8）：先工作后 sleep——恢复挂起的优先级任务首轮即收敛。
pub fn spawn_file_priority_replay_loop(
    state: Arc<DaemonState>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            state.replay_pending_file_priorities().await;
            tokio::time::sleep(interval).await;
        }
    })
}
