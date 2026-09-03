//! WS 事件协议（§12 D36）：9 类事件 + ProviderStatus，每事件带 monotonic seq；
//! 队列 256，满丢最旧非关键事件；客户端跳号 → snapshot_upto 补拉。

use serde::{Deserialize, Serialize};
use smart_dl_core::state_machine::TaskState;
use smart_dl_provider::ProviderRuntime;

use crate::health::HealthEventKind;

/// 调度器事件（D36 9 类 + ProviderStatus）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SchedulerEvent {
    TaskCreated {
        task_id: String,
    },
    StateChanged {
        task_id: String,
        from: TaskState,
        to: TaskState,
    },
    Progress {
        task_id: String,
        done: u64,
        total: u64,
    },
    Speed {
        task_id: String,
        down_rate: u64,
        up_rate: u64,
    },
    HealthEvent {
        task_id: String,
        kind: HealthEventKind,
    },
    Error {
        task_id: String,
        message: String,
    },
    Completed {
        task_id: String,
    },
    Failed {
        task_id: String,
        reason: String,
    },
    DuplicateRejected {
        task_id: String,
        existing: String,
    },
    /// Provider 运行态快照（§13/D5）。
    ProviderStatus {
        provider: String,
        runtime: ProviderRuntime,
    },
    /// 全局限速总阀门变更（E16）：daemon 级事件（无 task_id）。
    GlobalLimitsChanged {
        max_download_kb_s: u32,
        max_upload_kb_s: u32,
    },
}

impl SchedulerEvent {
    /// 关键事件（满队时优先保留）：终态/错误/去重拒绝。
    pub fn is_critical(&self) -> bool {
        matches!(
            self,
            SchedulerEvent::Completed { .. }
                | SchedulerEvent::Failed { .. }
                | SchedulerEvent::Error { .. }
                | SchedulerEvent::DuplicateRejected { .. }
        )
    }

    pub fn task_id(&self) -> Option<&str> {
        match self {
            SchedulerEvent::TaskCreated { task_id }
            | SchedulerEvent::StateChanged { task_id, .. }
            | SchedulerEvent::Progress { task_id, .. }
            | SchedulerEvent::Speed { task_id, .. }
            | SchedulerEvent::HealthEvent { task_id, .. }
            | SchedulerEvent::Error { task_id, .. }
            | SchedulerEvent::Completed { task_id }
            | SchedulerEvent::Failed { task_id, .. }
            | SchedulerEvent::DuplicateRejected { task_id, .. } => Some(task_id),
            SchedulerEvent::ProviderStatus { .. } | SchedulerEvent::GlobalLimitsChanged { .. } => {
                None
            }
        }
    }

    /// 事件类型标签（E10）：与 serde `tag = "type"` `rename_all =
    /// snake_case` 的线格式一致——match 全变体且无通配臂，新增变体漏标
    /// 由编译期拦截（对齐 E7 known_state_labels 防漂移模式）。
    pub fn type_label(&self) -> &'static str {
        match self {
            SchedulerEvent::TaskCreated { .. } => "task_created",
            SchedulerEvent::StateChanged { .. } => "state_changed",
            SchedulerEvent::Progress { .. } => "progress",
            SchedulerEvent::Speed { .. } => "speed",
            SchedulerEvent::HealthEvent { .. } => "health_event",
            SchedulerEvent::Error { .. } => "error",
            SchedulerEvent::Completed { .. } => "completed",
            SchedulerEvent::Failed { .. } => "failed",
            SchedulerEvent::DuplicateRejected { .. } => "duplicate_rejected",
            SchedulerEvent::ProviderStatus { .. } => "provider_status",
            SchedulerEvent::GlobalLimitsChanged { .. } => "global_limits_changed",
        }
    }
}

/// 合法事件类型标签全集（E10 `GET /events?type=` 校验输入；顺序与枚举
/// 声明序一致，返回形状对齐 E7 known_state_labels）。一致性由测试锁定：
/// 与 `type_label()` 全变体映射逐项相等。
pub fn known_event_type_labels() -> Vec<String> {
    [
        "task_created",
        "state_changed",
        "progress",
        "speed",
        "health_event",
        "error",
        "completed",
        "failed",
        "duplicate_rejected",
        "provider_status",
        "global_limits_changed",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// 带 monotonic seq 的事件信封（D36）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub seq: u64,
    pub event: SchedulerEvent,
}
