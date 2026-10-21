/// daemon API 类型（对齐 crates/daemon 线格式）。
///
/// 2026-10-21（#147 §六 / #143 第一梯队）：核心线格式类型改由 ts-rs 从
/// Rust 结构体生成（`ui/src/lib/daemon/generated/`，`cargo test
/// export_bindings` 导出，随仓库提交）——字段名漂移在编译期报错，不再
/// 运行时静默失效。本文件是唯一门面：生成类型重导出 + 尚无 Rust 结构体
/// 的响应（json! 宏构建）保留手写并标注 TODO。
///
/// 仍手写（后端为 serde_json::json! 动态构建，待 DTO 化后纳入生成）：
/// - GET /settings 快照（state/settings.rs settings_snapshot）
/// - GET /rss/feeds 与 /rss/items（http.rs json! 计数聚合）
/// - GET /tasks/:id/logs（ops.rs task_logs）

// ---- ts-rs 生成类型（单一事实来源：Rust 结构体）----

export type { TaskSummary as TaskListItem } from "./generated/TaskSummary";
export type { TaskSnapshot } from "./generated/TaskSnapshot";
export type { TaskRates } from "./generated/TaskRates";
export type { FileProgress as TaskFile } from "./generated/FileProgress";
export type { DaemonStats as Stats } from "./generated/DaemonStats";
export type { GlobalLimits } from "./generated/GlobalLimits";
export type { SchedulerEvent } from "./generated/SchedulerEvent";
export type { Envelope as SchedulerEventEnvelope } from "./generated/Envelope";
export type { SettingsApplyReport } from "./generated/SettingsApplyReport";
export type { EngineKind } from "./generated/EngineKind";
export type { TaskState } from "./generated/TaskState";
export type { AddTaskReq } from "./generated/AddTaskReq";
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

/// 事件流派生的任务级速率（Speed 事件）。
/// 客户端本地形状：ts 时间戳为 UI 侧落地时刻（TTL 新鲜度判定用），
/// 非后端字段——不入生成类型。
export interface TaskRate {
  down: number;
  up: number;
  ts: number;
}

// ---- S1 设置面（GET /settings 为 json! 动态构建——待 DTO 化，见文件头）----

export interface SettingsBandwidth {
  max_download_kb_s: number;
  max_upload_kb_s: number;
  alt_enabled: boolean;
  alt_max_download_kb_s: number;
  alt_max_upload_kb_s: number;
  alt_from: string;
  alt_to: string;
  alt_days: number[];
  alt_active_now: boolean;
  effective_max_download_kb_s: number;
  effective_max_upload_kb_s: number;
}

export interface Settings {
  bandwidth: SettingsBandwidth;
  connection: {
    proxy: string;
    bt_listen_port: number;
    bt_max_connections: number;
  };
  bittorrent: {
    enable_dht: boolean;
    enable_lsd: boolean;
    enable_upnp: boolean;
    enable_pex: boolean;
    enable_utp: boolean;
    encrypt: string;
    extra_trackers: string[];
    max_share_ratio: number;
    max_seeding_time_min: number;
    /** 存储模式：true = 新任务预分配（batch5，重启生效） */
    storage_allocate: boolean;
    bt_available: boolean;
  };
  download: {
    dest_root: string;
    disk_precheck_strict: boolean;
    /** HTTP 重定向最大跳数（batch5，1..=100，重启生效） */
    max_redirects: number;
  };
  cleanup: {
    auto_remove_completed_days: number;
    auto_remove_keep_data: boolean;
  };
  post_download: { move_to: string; hook: string };
  webhook: { url: string };
  scheduler: {
    start_jitter_seconds: number;
    /** 全部任务终态后的动作（Task 46）：none/exit/shutdown/sleep/hibernate；
     * 每进程会话最多触发一次；空任务表不触发。 */
    completion_action: string;
  };
  queue: {
    max_active_bt: number;
    max_active_http: number;
    max_active_ftp: number;
  };
  meta: { persist_path: string | null };
}

// ---- RSS（GET /rss/feeds 与 /rss/items 为 json! 动态构建——待 DTO 化）----

export interface RssItem {
  feed_id: number;
  feed_title: string;
  guid: string;
  title: string;
  url: string;
  pub_date?: string | null;
  task_id?: string | null;
}

export interface RssFeed {
  id: number;
  url: string;
  title: string;
  added_at_unix: number;
  last_refresh_unix?: number | null;
  /** 每 feed 独立刷新间隔（秒；0 = 跟随全局） */
  interval_override_secs?: number;
  item_count: number;
  pending_count: number;
}

export interface RssRule {
  id: number;
  name: string;
  enabled: boolean;
  must_contain: string[];
  must_not_contain: string[];
  feed_id?: number | null;
  tags: string[];
  dest?: string | null;
  /** 关键词按正则解释（batch5） */
  use_regex?: boolean;
  /** 集数过滤，如 `1x02;1x04-1x06`（batch5） */
  episode_filter?: string | null;
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
