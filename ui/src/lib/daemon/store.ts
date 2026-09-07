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
  taskLogs(id: string): Promise<{ logs: { ts: string; level: string; message: string }[] }> {
    return req(`/tasks/${id}/logs`);
  }
  taskLimit(id: string, down?: number, up?: number) {
    return req(`/tasks/${id}/limit`, {
      method: "POST",
      body: JSON.stringify({ max_download_kb_s: down, max_upload_kb_s: up }),
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
  subscribeEvents(
    onEvent: (env: SchedulerEventEnvelope) => void,
    onState: (connected: boolean) => void,
  ): () => void {
    let closed = false;
    let es: EventSource | null = null;
    let retry: ReturnType<typeof setTimeout> | null = null;
    let lastSeq = 0;
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
        es.onmessage = (m) => {
          try {
            const env = JSON.parse(m.data) as SchedulerEventEnvelope;
            if (typeof env.seq === "number" && env.seq > lastSeq) lastSeq = env.seq;
            onEvent(env);
          } catch {
            /* 跳过坏帧 */
          }
        };
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
