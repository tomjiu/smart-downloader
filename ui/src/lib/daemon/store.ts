"use client";
/// daemon 客户端：同源优先（内嵌 UI / 桌面壳），开发态可经
/// NEXT_PUBLIC_DAEMON 或 localStorage 覆盖；Bearer token 可选。

import type {
  RssFeed,
  RssItem,
  RssRule,
  SchedulerEventEnvelope,
  Settings,
  SettingsApplyReport,
  Stats,
  TaskSnapshot,
} from "./types";

const LS_KEY = "dl.daemon.base";
const LS_TOKEN = "dl.daemon.token";

export function getBase(): string {
  if (typeof window === "undefined") return "";
  return localStorage.getItem(LS_KEY) ?? "";
}

export function setBase(base: string) {
  if (base) localStorage.setItem(LS_KEY, base);
  else localStorage.removeItem(LS_KEY);
}

export function getToken(): string {
  if (typeof window === "undefined") return "";
  return localStorage.getItem(LS_TOKEN) ?? "";
}

export function setToken(t: string) {
  if (t) localStorage.setItem(LS_TOKEN, t);
  else localStorage.removeItem(LS_TOKEN);
}

function url(path: string): string {
  return `${getBase()}${path}`;
}

function headers(): HeadersInit {
  const t = getToken();
  return t ? { Authorization: `Bearer ${t}` } : {};
}

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const hasBody = init?.body != null;
  const res = await fetch(url(path), {
    ...init,
    headers: {
      ...headers(),
      ...(hasBody ? { "Content-Type": "application/json" } : {}),
      ...(init?.headers ?? {}),
    },
  });
  if (!res.ok) {
    let msg = `${res.status}`;
    try {
      const body = await res.json();
      if (body?.error) msg = body.error;
    } catch {
      /* 忽略非 JSON 错误体 */
    }
    throw new Error(msg);
  }
  return res.json() as Promise<T>;
}

export class DaemonClient {
  // 任务（GET /tasks 返回裸数组）
  listTasks(): Promise<TaskSnapshot[]> {
    return req("/tasks");
  }
  getTask(id: string): Promise<TaskSnapshot> {
    return req(`/tasks/${id}`);
  }
  addTask(body: Record<string, unknown>): Promise<{ task_ids: string[] }> {
    return req("/tasks", { method: "POST", body: JSON.stringify(body) });
  }
  pause(id: string) {
    return req(`/tasks/${id}/pause`, { method: "POST" });
  }
  resume(id: string) {
    return req(`/tasks/${id}/resume`, { method: "POST" });
  }
  remove(id: string, deleteData = false) {
    return req(`/tasks/${id}?delete_data=${deleteData}`, { method: "DELETE" });
  }
  /// P1 修复（Issue #115）：对齐后端实际返回（state/ops.rs task_logs——
  /// { task_id, state, source, error, events: [{ at_ms, op, detail }] }）。
  /// 旧形状 { logs: [{ ts, level, message }] } 从未与后端对过，接上即错。
  taskLogs(id: string): Promise<{
    task_id: string;
    state: string;
    source: unknown;
    error: string | null;
    events: { at_ms: number; op: string; detail: string | null }[];
  }> {
    return req(`/tasks/${id}/logs`);
  }
  /// P0-1 修复（Issue #115）：字段名对齐 http.rs LimitReq
  /// （down_kb_s / up_kb_s）。旧字段名 max_download_kb_s / max_upload_kb_s
  /// 被 serde 双 #[serde(default)] Option 静默丢弃 → 双 None → 空操作返 200，
  /// UI 显示「限速已应用」但限速从未生效（AddTaskReq 的 down_kb_s 是对的，
  /// 仅此处写错）。
  taskLimit(id: string, down?: number, up?: number) {
    return req(`/tasks/${id}/limit`, {
      method: "POST",
      body: JSON.stringify({ down_kb_s: down, up_kb_s: up }),
    });
  }
  /// 批量操作（Issue #115 Phase 1 接入；后端 POST /tasks/batch）。
  /// 契约对齐 http.rs BatchTaskReq：{ action, ids, delete_data } ——
  /// 注意 action 是字段名（非 op），合法值 pause / resume / remove；
  /// remove 可附 delete_data；响应恒 200 + 逐项结果（单项失败不短路）。
  batchTasks(
    ids: string[],
    action: "pause" | "resume" | "remove",
    opts?: { deleteData?: boolean },
  ): Promise<unknown> {
    return req(`/tasks/batch`, {
      method: "POST",
      body: JSON.stringify({
        action,
        ids,
        delete_data: opts?.deleteData ?? false,
      }),
    });
  }
  taskSequential(id: string, on: boolean) {
    return req(`/tasks/${id}/sequential`, {
      method: "POST",
      body: JSON.stringify({ sequential: on }),
    });
  }
  taskConnections(id: string, maxConnections: number) {
    return req(`/tasks/${id}/connections`, {
      method: "POST",
      body: JSON.stringify({ max_connections: maxConnections }),
    });
  }
  taskProxy(id: string, proxy: string | null) {
    return req(`/tasks/${id}/proxy`, {
      method: "POST",
      body: JSON.stringify({ proxy }),
    });
  }
  /// 首尾块优先（batch5 对标 qB）：每文件首/末块优先级；prio 0..=7（0=恢复）
  taskPieceFirstLast(id: string, priority: number) {
    return req(`/tasks/${id}/piece-priority`, {
      method: "POST",
      body: JSON.stringify({ priority }),
    });
  }

  // ---- RSS ----
  rssFeeds(): Promise<{ feeds: RssFeed[] }> {
    return req("/rss/feeds");
  }
  rssItems(feedId?: number): Promise<{ items: RssItem[] }> {
    return req(`/rss/items${feedId != null ? `?feed_id=${feedId}` : ""}`);
  }
  rssRules(): Promise<{ rules: RssRule[] }> {
    return req("/rss/rules");
  }
  rssAddFeed(url: string) {
    return req("/rss/feeds", { method: "POST", body: JSON.stringify({ url }) });
  }
  rssRemoveFeed(id: number) {
    return req(`/rss/feeds/${id}`, { method: "DELETE" });
  }
  rssUpdateFeed(id: number, intervalOverrideSecs: number) {
    return req(`/rss/feeds/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ interval_override_secs: intervalOverrideSecs }),
    });
  }
  rssAddRule(body: {
    name: string;
    enabled: boolean;
    must_contain: string[];
    must_not_contain: string[];
    feed_id: number | null;
    tags: string[];
    dest: string | null;
    use_regex: boolean;
    episode_filter: string | null;
  }): Promise<{ id: number }> {
    return req("/rss/rules", { method: "POST", body: JSON.stringify(body) });
  }
  rssRemoveRule(id: number) {
    return req(`/rss/rules/${id}`, { method: "DELETE" });
  }
  rssRefresh() {
    return req("/rss/refresh", { method: "POST" });
  }

  // 全局
  stats(): Promise<Stats> {
    return req("/stats");
  }
  config(): Promise<Record<string, unknown>> {
    return req("/config");
  }
  globalLimits(): Promise<GlobalLimitsShape> {
    return req("/config/limit");
  }
  setGlobalLimits(down: number | null, up: number | null): Promise<GlobalLimitsShape> {
    return req("/config/limit", {
      method: "POST",
      body: JSON.stringify({ max_download_kb_s: down, max_upload_kb_s: up }),
    });
  }
  version(): Promise<{ name: string; version: string; features?: string[] }> {
    return req("/version");
  }
  providers(): Promise<{ providers: { name: string; enabled: boolean; authenticated?: boolean }[] }> {
    return req("/providers");
  }
  listEvents(limit = 200): Promise<{ events: SchedulerEventEnvelope[] }> {
    return req(`/events?limit=${limit}`);
  }

  // S1 设置面
  settings(): Promise<Settings> {
    return req("/settings");
  }
  putSettings(
    patch: Record<string, unknown>,
    persist = true,
  ): Promise<SettingsApplyReport> {
    return req(`/settings?persist=${persist}`, {
      method: "PUT",
      body: JSON.stringify(patch),
    });
  }

  // 事件流（SSE，带断线重连）。审计修复（40-e P1-6/P1-13）：
  // - token 以 ?token= 查询参数传递（EventSource 规范无法设置请求头，
  //   旧实现配置 token 后 401 死循环；daemon 侧对 /events/stream 回退）
  // - 重连带 after 游标（最后收到 seq）——旧实现重连从 0 全量重放，
  //   重复 seq 导致日志重复/重复 toast
  // 交互打磨批 P0 修复：daemon SSE 帧是具名事件（`event: <type_label>`，
  // http.rs envelope_to_sse + events.rs type_label 13 变体），EventSource
  // 的 onmessage 只收「无 event 字段」的默认消息——旧实现整条事件流
  // （速率列 / 完成 toast / 日志视图）从未接到过数据。改按类型
  // addEventListener 路由（后端注释「EventSource addEventListener
  // 按类型路由」的契约）。
  subscribeEvents(
    onEvent: (env: SchedulerEventEnvelope) => void,
    onState: (connected: boolean) => void,
  ): () => void {
    let closed = false;
    let es: EventSource | null = null;
    let retry: ReturnType<typeof setTimeout> | null = null;
    let lastSeq = 0;
    /// daemon events.rs known_event_type_labels() 全集（13 种；新增变体
    /// 时此处同步——未知类型不路由，属可接受的静默丢弃口径）。
    const EVENT_TYPES = [
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
      "task_activated",
      "settings_changed",
    ];
    const handle = (m: MessageEvent) => {
      try {
        const env = JSON.parse(m.data) as SchedulerEventEnvelope;
        if (typeof env.seq === "number" && env.seq > lastSeq) lastSeq = env.seq;
        onEvent(env);
      } catch {
        /* 跳过坏帧 */
      }
    };
    const connect = () => {
      if (closed) return;
      try {
        const t = getToken();
        const q = [
          t ? `token=${encodeURIComponent(t)}` : "",
          lastSeq > 0 ? `after=${lastSeq}` : "",
        ]
          .filter(Boolean)
          .join("&");
        es = new EventSource(url(`/events/stream${q ? `?${q}` : ""}`));
        es.onopen = () => onState(true);
        for (const type of EVENT_TYPES) es.addEventListener(type, handle as EventListener);
        es.onerror = () => {
          onState(false);
          es?.close();
          retry = setTimeout(connect, 2000);
        };
      } catch {
        retry = setTimeout(connect, 2000);
      }
    };
    connect();
    return () => {
      closed = true;
      if (retry) clearTimeout(retry);
      es?.close();
    };
  }
}

type GlobalLimitsShape = { max_download_kb_s: number; max_upload_kb_s: number };

export const client = new DaemonClient();
