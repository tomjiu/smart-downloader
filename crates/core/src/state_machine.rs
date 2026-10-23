//! 状态机（§10）：TaskState 枚举 + 转换表。
//! PausingAwait/Stalled 不是枚举值（D32）：stall 由 TransitionCtx.stalled 表达。

use crate::ownership::FallbackPolicy;
use crate::types::EngineKind;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 评估阶段（§10 三阶段评估 D7）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum EvalPhase {
    MetadataPending,
    PeerDiscovery,
    HeatEvaluating,
}

/// 对外任务状态（§10，D32：无 PausingAwait/Stalled 值）。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum TaskState {
    Queued,
    Evaluating(EvalPhase),
    Downloading(EngineKind),
    Paused,
    FallbackProvider,
    Transferring,
    Completed,
    Stopped,
    Seeding,
    Failed,
}

/// 转换上下文（转换表条件；§10）。
#[derive(Clone, Debug)]
pub struct TransitionCtx {
    pub quota_ok: bool,
    pub metadata_received: bool,
    pub heat: Option<f64>,
    pub bt_progress: f64,
    pub stalled: bool,
    pub policy: FallbackPolicy,
    pub seeding_enabled: bool,
}

impl Default for TransitionCtx {
    fn default() -> Self {
        TransitionCtx {
            quota_ok: true,
            metadata_received: false,
            heat: None,
            bt_progress: 0.0,
            stalled: false,
            policy: FallbackPolicy::default(),
            seeding_enabled: false,
        }
    }
}

/// 非法转换错误。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InvalidTransition {
    #[error("transition forbidden: {0:?} → {1:?}")]
    Forbidden(TaskState, TaskState),
}

/// 纯函数转换表（§10）。签名是 M3/M5/M6 的基础，不得改动。
pub struct StateMachine;

impl StateMachine {
    pub fn can_transition(&self, from: &TaskState, to: &TaskState, ctx: &TransitionCtx) -> bool {
        match (from, to) {
            (TaskState::Queued, TaskState::Evaluating(EvalPhase::MetadataPending)) => ctx.quota_ok,
            (
                TaskState::Evaluating(EvalPhase::MetadataPending),
                TaskState::Evaluating(EvalPhase::PeerDiscovery),
            ) => ctx.metadata_received,
            (
                TaskState::Evaluating(EvalPhase::PeerDiscovery),
                TaskState::Evaluating(EvalPhase::HeatEvaluating),
            ) => true,
            (
                TaskState::Evaluating(EvalPhase::HeatEvaluating),
                TaskState::Downloading(EngineKind::Bt),
            ) => ctx.heat.is_some_and(|h| h >= 0.3),
            (TaskState::Evaluating(EvalPhase::HeatEvaluating), TaskState::FallbackProvider) => {
                ctx.heat.is_some_and(|h| h < 0.3)
            }
            // Downloading → Paused：必须经过 stall（§10 转换表）
            (TaskState::Downloading(_), TaskState::Paused) => ctx.stalled,
            // Downloading(Bt) → FallbackProvider：stall 且 BT < 策略阈值（串行兜底）
            (TaskState::Downloading(EngineKind::Bt), TaskState::FallbackProvider) => {
                ctx.stalled && ctx.bt_progress < ctx.policy.bt_ratio_to_continue
            }
            (TaskState::Downloading(_), TaskState::Completed) => true,
            (TaskState::FallbackProvider, TaskState::Transferring) => true,
            (TaskState::Transferring, TaskState::Completed) => true,
            // resume
            (TaskState::Paused, TaskState::Downloading(_)) => true,
            // 完成与做种分离（D24）：Completed → Seeding 需配置开启
            (TaskState::Completed, TaskState::Stopped) => true,
            (TaskState::Completed, TaskState::Seeding) => ctx.seeding_enabled,
            // * → Failed（重试超上限 / 无可用源 / Ed2k）
            (_, TaskState::Failed) => true,
            _ => false,
        }
    }

    pub fn transition(
        &self,
        from: &TaskState,
        to: &TaskState,
        ctx: &TransitionCtx,
    ) -> Result<TaskState, InvalidTransition> {
        if self.can_transition(from, to, ctx) {
            Ok(to.clone())
        } else {
            Err(InvalidTransition::Forbidden(from.clone(), to.clone()))
        }
    }
}
