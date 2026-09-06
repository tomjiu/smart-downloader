"use client";
/// daemon 客户端：同源优先（内嵌 UI / 桌面壳），开发态可经
/// NEXT_PUBLIC_DAEMON 或 localStorage 覆盖；Bearer token 可选。

import type {
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
  taskProxy(id: string, proxy: string | null) {
    return req(`/tasks/${id}/proxy`, {
      method: "POST",
      body: JSON.stringify({ proxy }),
    });
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

  // 事件流（SSE，带断线重连）
  subscribeEvents(
    onEvent: (env: SchedulerEventEnvelope) => void,
    onState: (connected: boolean) => void,
  ): () => void {
    let closed = false;
    let es: EventSource | null = null;
    let retry: ReturnType<typeof setTimeout> | null = null;
    const connect = () => {
      if (closed) return;
      try {
        es = new EventSource(url("/events/stream"));
        es.onopen = () => onState(true);
        es.onmessage = (m) => {
          try {
            onEvent(JSON.parse(m.data));
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
