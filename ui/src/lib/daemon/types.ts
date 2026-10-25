/// daemon API 类型（对齐 crates/daemon 线格式）。
///
/// 2026-10-21（#147 §六 / #143 第一梯队）：核心线格式类型改由 ts-rs 从
/// Rust 结构体生成（`ui/src/lib/daemon/generated/`，`cargo test
/// export_bindings` 导出，随仓库提交）——字段名漂移在编译期报错，不再
/// 运行时静默失效。本文件是唯一门面：生成类型重导出 + 尚无 Rust 结构体
/// 的响应（json! 宏构建）保留手写并标注 TODO。
///
/// 2026-10-21（第二批）：GET /settings 快照、RSS feeds/items/rules 列表、
/// GET /tasks/:id/logs 三处原 serde_json::json! 动态构建的响应已 DTO 化
/// （SettingsSnapshot / Rss*Dto / TaskLogs）——手写类型全部退役。

// ---- ts-rs 生成类型（单一事实来源：Rust 结构体）----

export type { TaskSummary as TaskListItem } from "./generated/TaskSummary";
export type { TaskSnapshot } from "./generated/TaskSnapshot";
export type { TaskRates } from "./generated/TaskRates";
export type { FileProgress as TaskFile } from "./generated/FileProgress";
export type { DaemonStats as Stats } from "./generated/DaemonStats";
export type { GlobalLimits } from "./generated/GlobalLimits";
export type { ProviderStatusDto } from "./generated/ProviderStatusDto";
export type { SchedulerEvent } from "./generated/SchedulerEvent";
export type { Envelope as SchedulerEventEnvelope } from "./generated/Envelope";
export type { SettingsApplyReport } from "./generated/SettingsApplyReport";
export type { EngineKind } from "./generated/EngineKind";
export type { TaskState } from "./generated/TaskState";
export type { AddTaskReq } from "./generated/AddTaskReq";
export type { ListTasksQuery } from "./generated/ListTasksQuery";
export type { LimitReq } from "./generated/LimitReq";
export type { SequentialReq } from "./generated/SequentialReq";
export type { ConnectionsReq } from "./generated/ConnectionsReq";
export type { SetProxyReq } from "./generated/SetProxyReq";
export type { RenameTaskReq } from "./generated/RenameTaskReq";
export type { TagsReq } from "./generated/TagsReq";
export type { PiecePriorityReq } from "./generated/PiecePriorityReq";
export type { BatchTaskReq } from "./generated/BatchTaskReq";
export type { BatchSelect } from "./generated/BatchSelect";
export type { GlobalLimitReq } from "./generated/GlobalLimitReq";
export type { RssFeedUpdateReq } from "./generated/RssFeedUpdateReq";
export type { AddRssRuleReq } from "./generated/AddRssRuleReq";
export type { TaskLogs } from "./generated/TaskLogs";
export type { TaskEvent } from "./generated/TaskEvent";
export type { SettingsSnapshot as Settings } from "./generated/SettingsSnapshot";
export type { SettingsReq } from "./generated/SettingsReq";
export type { MagnetMetaReq } from "./generated/MagnetMetaReq";
export type { SettingsBandwidthDto as SettingsBandwidth } from "./generated/SettingsBandwidthDto";
export type { RssFeedDto as RssFeed } from "./generated/RssFeedDto";
export type { RssItemDto as RssItem } from "./generated/RssItemDto";
export type { AddRssFeedReq } from "./generated/AddRssFeedReq";
export type { RssRule } from "./generated/RssRule";
export type { RssFeedsList } from "./generated/RssFeedsList";
export type { RssItemsList } from "./generated/RssItemsList";
export type { RssRulesList } from "./generated/RssRulesList";
// ---- 第二梯队（种源管理 / 队列优先级 / 文件优先级）----
export type { TrackerEntry } from "./generated/TrackerEntry";
export type { TrackersReq } from "./generated/TrackersReq";
export type { RemoveTrackerQuery } from "./generated/RemoveTrackerQuery";
export type { AddedCount } from "./generated/AddedCount";
export type { RemovedUrl } from "./generated/RemovedUrl";
export type { AddPeersReq } from "./generated/AddPeersReq";
export type { PeerOutcome } from "./generated/PeerOutcome";
export type { PeerResults } from "./generated/PeerResults";
export type { WebseedReq } from "./generated/WebseedReq";
export type { SuperSeedingReq } from "./generated/SuperSeedingReq";
export type { AnnounceReq } from "./generated/AnnounceReq";
export type { PriorityReq } from "./generated/PriorityReq";
export type { QueuePriorityOut } from "./generated/QueuePriorityOut";
export type { FilePriorityReq } from "./generated/FilePriorityReq";
export type { FilePriorityEntry } from "./generated/FilePriorityEntry";
export type { FilePrioritiesOut } from "./generated/FilePrioritiesOut";
export type { BansReq } from "./generated/BansReq";
// ---- 第二梯队 Wave B（新建任务全字段表单 + 探测预览）----
export type { ProbeReq } from "./generated/ProbeReq";
export type { ProbeOut } from "./generated/ProbeOut";
// ---- 第二梯队 Wave C（IP 封禁页）----
export type { BansListOut } from "./generated/BansListOut";
export type { BanOutcome } from "./generated/BanOutcome";
export type { BansOut } from "./generated/BansOut";
export type { BansImportReq } from "./generated/BansImportReq";
export type { BansImportOut } from "./generated/BansImportOut";

/// 事件流派生的任务级速率（Speed 事件）。
/// 客户端本地形状：ts 时间戳为 UI 侧落地时刻（TTL 新鲜度判定用），
/// 非后端字段——不入生成类型。
export interface TaskRate {
  down: number;
  up: number;
  ts: number;
}

/// 12 主题（Issue #115 Phase 1：nord/carbon/gruvbox/catppuccin/solarized/mono
/// × 明暗）。键名与 tokens.css 的 [data-theme] 完全一致（与
/// theme-preview.html / UI-DESIGN-SPEC.md §4 对齐）。
export const THEME_KEYS = [
  "nord-dark",
  "nord-light",
  "carbon-dark",
  "carbon-light",
  "gruvbox-dark",
  "gruvbox-light",
  "catppuccin-dark",
  "catppuccin-light",
  "solarized-dark",
  "solarized-light",
  "mono-dark",
  "mono-light",
] as const;

export type ThemeKey = (typeof THEME_KEYS)[number];

export const DEFAULT_THEME: ThemeKey = "nord-dark";

export const THEME_META: Record<ThemeKey, { label: string; accent: string; dark: boolean }> = {
  "nord-dark": { label: "Nord 暗", accent: "#88c0d0", dark: true },
  "nord-light": { label: "Nord 亮", accent: "#5e81ac", dark: false },
  "carbon-dark": { label: "Carbon 暗", accent: "#0f62fe", dark: true },
  "carbon-light": { label: "Carbon 亮", accent: "#0f62fe", dark: false },
  "gruvbox-dark": { label: "Gruvbox 暗", accent: "#83a598", dark: true },
  "gruvbox-light": { label: "Gruvbox 亮", accent: "#076678", dark: false },
  "catppuccin-dark": { label: "Catppuccin 暗", accent: "#89b4fa", dark: true },
  "catppuccin-light": { label: "Catppuccin 亮", accent: "#1e66f5", dark: false },
  "solarized-dark": { label: "Solarized 暗", accent: "#268bd2", dark: true },
  "solarized-light": { label: "Solarized 亮", accent: "#268bd2", dark: false },
  "mono-dark": { label: "极简灰 暗", accent: "#e8a33d", dark: true },
  "mono-light": { label: "极简灰 亮", accent: "#b8721f", dark: false },
};
