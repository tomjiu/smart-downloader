/// daemon API 类型（对齐 crates/daemon/src/http.rs 线格式）。

export type EngineKind = "Bt" | "Http" | "Ftp" | "Provider";

export interface TaskFile {
  path: string;
  size: number;
  done: number;
}

/// GET /tasks 列表项（轻量形状，对齐 daemon list_tasks）。
export interface TaskListItem {
  task_id: string;
  state: string;
  source: string;
  engine?: string | null;
  name: string;
}

/// GET /tasks/:id 详情快照（对齐 daemon task_snapshot）。
export interface TaskSnapshot {
  task_id: string;
  state: string;
  source: string;
  dest_root?: string;
  engine?: string | null;
  done: number;
  total: number;
  error?: string | null;
  files?: TaskFile[];
  name: string;
  /** 任务级连接数上限（S1-c，仅 BT；未设置 = undefined） */
  max_connections?: number;
  /** 顺序下载（BT/HTTP） */
  sequential?: boolean;
  /** 任务级限速 */
  limits?: { down_kb_s?: number | null; up_kb_s?: number | null };
  /** 定时启动（unix 秒；0 = 未调度） */
  start_at_unix?: number;
}

/// 事件流派生的任务级速率（Speed 事件）。
export interface TaskRate {
  down: number;
  up: number;
  ts: number;
}

export interface Stats {
  total: number;
  by_state: Record<string, number>;
  by_engine: Record<string, number>;
  down_bytes_s: number;
  up_bytes_s: number;
}

export interface GlobalLimits {
  max_download_kb_s: number;
  max_upload_kb_s: number;
}

export interface SchedulerEventEnvelope {
  seq: number;
  event: { type: string; [k: string]: unknown };
}

// ---- S1 设置面 ----

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
    bt_available: boolean;
  };
  download: { dest_root: string; disk_precheck_strict: boolean };
  cleanup: {
    auto_remove_completed_days: number;
    auto_remove_keep_data: boolean;
  };
  post_download: { move_to: string; hook: string };
  webhook: { url: string };
  scheduler: { start_jitter_seconds: number };
  queue: {
    max_active_bt: number;
    max_active_http: number;
    max_active_ftp: number;
  };
  meta: { persist_path: string | null };
}

export interface SettingsApplyReport {
  applied: string[];
  restart_required: string[];
  persisted: boolean;
  limits: GlobalLimits;
}

export const THEME_KEYS = [
  "forest-light",
  "forest-dark",
  "bee-light",
  "bee-dark",
  "mint-light",
  "mint-dark",
  "light-parchment",
  "parchment-dark",
] as const;

export type ThemeKey = (typeof THEME_KEYS)[number];

export const THEME_META: Record<ThemeKey, { label: string; accent: string; dark: boolean }> = {
  "forest-light": { label: "森林·亮", accent: "#2cb879", dark: false },
  "forest-dark": { label: "森林·暗", accent: "#62c9a8", dark: true },
  "bee-light": { label: "蜂巢·亮", accent: "#e0c65c", dark: false },
  "bee-dark": { label: "蜂巢·暗", accent: "#e6ce6a", dark: true },
  "mint-light": { label: "薄荷·亮", accent: "#3f8f78", dark: false },
  "mint-dark": { label: "薄荷·暗", accent: "#76d9b9", dark: true },
  "light-parchment": { label: "羊皮·亮", accent: "#c96442", dark: false },
  "parchment-dark": { label: "羊皮·暗", accent: "#8ee5a1", dark: true },
};
