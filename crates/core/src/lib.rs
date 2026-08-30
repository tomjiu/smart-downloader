//! 核心调度模型（M2 交付）：能力模型 / 身份 / 所有权 / 状态机 / 路由 / 热度。
//! 本 crate 不 import 任何引擎，仅定义契约类型与纯逻辑。

pub mod dedup;
pub mod heat;
pub mod identity;
pub mod ownership;
pub mod registry;
pub mod router;
pub mod session;
pub mod source_parse;
pub mod state_machine;
pub mod task;
pub mod torrent_meta;
pub mod types;
pub mod bencode;
pub mod xltd;
pub mod sniffer;
pub mod strategy;

pub use dedup::{DedupIndex, DedupOutcome};
pub use heat::{heat_level, heat_score, HeatEvaluator, HeatLevel};
pub use identity::{CanonicalId, CanonicalKind, ContentIdentity, Validator};
pub use ownership::{FallbackDecision, FallbackPolicy, KeepLarger, MetadataAction};
pub use registry::{EngineRegistry, QueueOutcome, RegistryError, RoutingError, TaskQueue};
pub use router::{RouteDecision, Router};
pub use source_parse::ed2k::{parse_ed2k, Ed2kError, Ed2kLink};
pub use source_parse::magnet::{parse_magnet, MagnetError, MagnetInfo};
pub use torrent_meta::{parse_torrent, TorrentFileMeta, TorrentSummary};
pub use state_machine::{EvalPhase, InvalidTransition, StateMachine, TaskState, TransitionCtx};
pub use task::{DownloadTask, FileState, TaskFile, TaskId, TaskMetadata};
pub use types::{
    Auth, Capability, DownloadEngine, DownloadSource, EngineError, EngineKind, EngineStatus,
    EngineTaskId, FileProgress, PeerInfo,
};
