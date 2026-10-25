"use client";
/// daemon 客户端：同源优先（内嵌 UI / 桌面壳），开发态可经
/// NEXT_PUBLIC_DAEMON 或 localStorage 覆盖；Bearer token 可选。

import type {
  AddedCount,
  AddPeersReq,
  AddRssFeedReq,
  AddRssRuleReq,
  AddTaskReq,
  AnnounceReq,
  BansImportOut,
  BansImportReq,
  BansListOut,
  BansOut,
  BansReq,
  BatchTaskReq,
  ConnectionsReq,
  FilePriorityEntry,
  FilePrioritiesOut,
  FilePriorityReq,
  GlobalLimitReq,
  GlobalLimits,
  LimitReq,
  ListTasksQuery,
  PeerResults,
  PiecePriorityReq,
  PriorityReq,
  ProbeOut,
  ProbeReq,
  ProviderStatusDto,
  QueuePriorityOut,
  RemovedUrl,
  RenameTaskReq,
  RssFeed,
  RssFeedUpdateReq,
  RssItem,
  RssRule,
  SchedulerEventEnvelope,
  SequentialReq,
  SetProxyReq,
  Settings,
  SettingsApplyReport,
  SettingsReq,
  Stats,
  SuperSeedingReq,
  TagsReq,
  TaskListItem,
  TaskSnapshot,
  TrackerEntry,
  TrackersReq,
  WebseedReq,
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
  /// A1 修复（契约审计 2026-10-21）：返回类型改为 TaskListItem[]——后端
  /// list_tasks 实际返回 TaskSummary 轻量形状（无 done/total/rates），
  /// 旧声明 TaskSnapshot[] 是类型级谎言（结构性兼容侥幸过编译，
  /// 但误导后续消费——若读 done/total 将拿到 undefined）。
  /// #161 P3：q 支持服务端过滤/搜索（?state=&engine=&search=&tag=，
  /// 参数形状由生成的 ListTasksQuery 锁定；未提供/全空 = 全量，
  /// 行为与形状同旧）。分页（limit/offset + X-Total-Count）留待
  /// 服务端排序支持后再接（客户端排序与分页组合语义不成立）。
  listTasks(q?: ListTasksQuery): Promise<TaskListItem[]> {
    if (q) {
      const p = new URLSearchParams();
      if (q.state) p.set("state", q.state);
      if (q.engine) p.set("engine", q.engine);
      if (q.search) p.set("search", q.search);
      if (q.tag) p.set("tag", q.tag);
      if (typeof q.limit === "number") p.set("limit", String(q.limit));
      if (typeof q.offset === "number") p.set("offset", String(q.offset));
      const s = p.toString();
      if (s) return req(`/tasks?${s}`);
    }
    return req("/tasks");
  }
  getTask(id: string): Promise<TaskSnapshot> {
    return req(`/tasks/${id}`);
  }
  /// ts-rs 生成（#147 §六）：请求体类型来自 Rust 结构体——字段名漂移在
  /// 编译期报错（旧 addTask 的 Record<string, unknown> 无任何防护）。
  /// 响应：单任务 = task_ids[0]；metalink = 逐 file 展开（task_ids 多项 +
  /// count；task_id = 首个，向后兼容）。
  addTask(body: Partial<AddTaskReq>): Promise<{
    task_id?: string;
    task_ids: string[];
    count?: number;
  }> {
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
  /// P0-1 修复（Issue #115）：字段名对齐 http.rs LimitReq
  /// （down_kb_s / up_kb_s）。旧字段名 max_download_kb_s / max_upload_kb_s
  /// 被 serde 双 #[serde(default)] Option 静默丢弃 → 双 None → 空操作返 200，
  /// UI 显示「限速已应用」但限速从未生效（AddTaskReq 的 down_kb_s 是对的，
  /// 仅此处写错）。ts-rs 后此类型直接来自 Rust，同类漂移编译期即红。
  taskLimit(id: string, down?: number, up?: number) {
    const body: LimitReq = { down_kb_s: down, up_kb_s: up };
    return req(`/tasks/${id}/limit`, {
      method: "POST",
      body: JSON.stringify(body),
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
    const body: BatchTaskReq = {
      action,
      ids,
      delete_data: opts?.deleteData ?? false,
    };
    return req(`/tasks/batch`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  }
  taskSequential(id: string, on: boolean) {
    const body: SequentialReq = { sequential: on };
    return req(`/tasks/${id}/sequential`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  }
  taskConnections(id: string, maxConnections: number) {
    const body: ConnectionsReq = { max_connections: maxConnections };
    return req(`/tasks/${id}/connections`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  }
  taskProxy(id: string, proxy: string | null) {
    const body: SetProxyReq = { proxy };
    return req(`/tasks/${id}/proxy`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  }
  /// 首尾块优先（batch5 对标 qB）：每文件首/末块优先级；prio 0..=7（0=恢复）
  taskPieceFirstLast(id: string, priority: number) {
    const body: PiecePriorityReq = { priority };
    return req(`/tasks/${id}/piece-priority`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  }

  /// 重命名（E6 派生链旁路——用户显式名持久化）。响应 = 更新后快照。
  taskRename(id: string, name: string): Promise<TaskSnapshot> {
    const body: RenameTaskReq = { name };
    return req(`/tasks/${id}/name`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  }
  /// 标签设置（E18）：null = 清空；返回归一化后的标签全集。
  taskTags(id: string, tags: string[] | null): Promise<{ tags: string[] }> {
    const body: TagsReq = { tags };
    return req(`/tasks/${id}/tags`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  }

  /// 队列优先级（第二梯队）：绝对值或相对移动（top/bottom/up/down，
  /// 二选一——同给以 value 优先，见 http.rs PriorityReq 注释）。
  taskPriority(
    id: string,
    opts: { priority?: number; action?: "top" | "bottom" | "up" | "down" },
  ): Promise<QueuePriorityOut> {
    const body: PriorityReq = { priority: opts.priority, action: opts.action };
    return req(`/tasks/${id}/priority`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  }
  /// 子文件优先级（仅 BT）：priority 语义同 libtorrent——0=不下载 /
  /// 1=低 / 4=默认 / 7=最高；响应 = 各文件优先级快照（与 files 下标对齐）。
  taskFilePriority(
    id: string,
    priorities: FilePriorityEntry[],
  ): Promise<FilePrioritiesOut> {
    const body: FilePriorityReq = { priorities };
    return req(`/tasks/${id}/files/priority`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  }

  // ---- BT 种源管理（第二梯队；全部仅 BT 任务，其余 409）----
  /// tracker 表（E29）：GET 裸数组（url + tier，tier 小者优先）。
  listTrackers(id: string): Promise<TrackerEntry[]> {
    return req(`/tasks/${id}/trackers`);
  }
  /// 批量追加 tracker：逐 URL 校验，非法 400；返回成功计数。
  addTrackers(id: string, urls: string[]): Promise<AddedCount> {
    const body: TrackersReq = { urls };
    return req(`/tasks/${id}/trackers`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  }
  /// 精确删除 tracker（URL 全文匹配；无匹配 404）。
  removeTracker(id: string, url: string): Promise<RemovedUrl> {
    return req(`/tasks/${id}/trackers?url=${encodeURIComponent(url)}`, {
      method: "DELETE",
    });
  }
  /// 手动注入 peer（`ip:port`，支持 IPv6 `[::1]:6881`）：部分成功语义，
  /// 逐条回执（ok / error）。注意：409（全失败且非 BT）也会带 results 体。
  addPeers(id: string, addrs: string[]): Promise<PeerResults> {
    const body: AddPeersReq = { addrs };
    return req(`/tasks/${id}/peers`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  }
  /// 注入 web seed（云盘直链，URL 须原样带签名）。
  addWebseeds(id: string, urls: string[]): Promise<AddedCount> {
    const body: WebseedReq = { urls };
    return req(`/tasks/${id}/webseeds`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  }
  /// 超级种子开关（做种态生效）。响应 = 快照。
  taskSuperSeeding(id: string, enabled: boolean): Promise<TaskSnapshot> {
    const body: SuperSeedingReq = { enabled };
    return req(`/tasks/${id}/super-seeding`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  }
  /// 强制宣告（tracker / dht 至少一项 true，否则 400）。响应 = 快照。
  taskAnnounce(id: string, tracker: boolean, dht: boolean): Promise<TaskSnapshot> {
    const body: AnnounceReq = { tracker, dht };
    return req(`/tasks/${id}/announce`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  }
  /// 强制重新校验（全量 hash 重算，进度将回零重走）。响应 = 快照。
  taskRecheck(id: string): Promise<TaskSnapshot> {
    return req(`/tasks/${id}/recheck`, { method: "POST" });
  }
  /// 生成 magnet URI（含当前 tracker 表与 name）。
  taskMagnet(id: string): Promise<{ magnet: string }> {
    return req(`/tasks/${id}/magnet`);
  }
  /// 导出 .torrent（二进制下载：blob → a[download]，文件名取 Content-Disposition）。
  /// 独立于 req<T>（非 JSON 响应）；鉴权头同源透传。
  async exportTorrent(id: string): Promise<string> {
    const res = await fetch(url(`/tasks/${id}/export`), {
      headers: headers(),
    });
    if (!res.ok) {
      let msg = `${res.status}`;
      try {
        const body = await res.json();
        if (body?.error) msg = body.error;
      } catch {
        /* 非 JSON 错误体 */
      }
      throw new Error(msg);
    }
    const cd = res.headers.get("Content-Disposition") ?? "";
    const m = /filename="?([^";]+)"?/.exec(cd);
    const name = m?.[1] ?? `${id}.torrent`;
    const blobUrl = URL.createObjectURL(await res.blob());
    const a = document.createElement("a");
    a.href = blobUrl;
    a.download = name;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(blobUrl), 30_000);
    return name;
  }

  /// 探测预览（E31，第二梯队新建表单）：不建任务，取大小/建议名/
  /// Range 支持（headers/proxy/auth 同 add 语义随探测下发——预览即验证
  /// 反防盗链配置是否有效）。非 2xx 抛错误（req 统一错误提取）。
  probe(body: Partial<ProbeReq>): Promise<ProbeOut> {
    return req("/probe", { method: "POST", body: JSON.stringify(body) });
  }

  // ---- IP 封禁（第二梯队 Wave C；BT 引擎级黑名单）----
  /// 当前封禁表（规范化后的 IP / 区间字符串）。
  listBans(): Promise<BansListOut> {
    return req("/security/bans");
  }
  /// 批量封禁：逐条回执（非法 IP / 引擎不可用逐条报错，不整批拒绝）。
  banIps(ips: string[]): Promise<BansOut> {
    const body: BansReq = { ips };
    return req("/security/bans", { method: "POST", body: JSON.stringify(body) });
  }
  /// 批量解禁（幂等）。
  unbanIps(ips: string[]): Promise<BansOut> {
    const body: BansReq = { ips };
    return req("/security/bans", { method: "DELETE", body: JSON.stringify(body) });
  }
  /// IP 段批量导入（DAT PeerGuardian / P2P 格式文本；部分成功语义）。
  /// 注意：全失败时 400（错误体含 imported/errors）——req 统一抛错，
  /// 调用方如需部分结果须自行 fetch（UI 直接展示错误文案即可）。
  importBans(text: string): Promise<BansImportOut> {
    const body: BansImportReq = { text };
    return req("/security/bans/import", {
      method: "POST",
      body: JSON.stringify(body),
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
  /// 响应 { id, title, item_count }（201）；同名/无效源 409/400。
  /// 请求体走生成的 AddRssFeedReq（#161：字段名漂移编译期报错）。
  rssAddFeed(url: string): Promise<{
    id: number;
    title: string;
    item_count: number;
  }> {
    const body: AddRssFeedReq = { url };
    return req("/rss/feeds", { method: "POST", body: JSON.stringify(body) });
  }
  rssRemoveFeed(id: number) {
    return req(`/rss/feeds/${id}`, { method: "DELETE" });
  }
  rssUpdateFeed(id: number, intervalOverrideSecs: number) {
    const body: RssFeedUpdateReq = {
      interval_override_secs: intervalOverrideSecs,
    };
    return req(`/rss/feeds/${id}`, {
      method: "PATCH",
      body: JSON.stringify(body),
    });
  }
  rssAddRule(body: AddRssRuleReq): Promise<{ id: number }> {
    return req("/rss/rules", { method: "POST", body: JSON.stringify(body) });
  }
  rssRemoveRule(id: number) {
    return req(`/rss/rules/${id}`, { method: "DELETE" });
  }
  rssRefresh() {
    return req("/rss/refresh", { method: "POST" });
  }

  // 全局
  // ---- 全局限速总阀门（E16，运行时热改不落盘）与 Provider 状态（#161 UI 接入）----
  /// 当前生效限速（POST 双缺省 = 纯查询语义，见 http.rs GlobalLimitReq 注释）。
  globalLimits(): Promise<GlobalLimits> {
    const body: GlobalLimitReq = {};
    return req("/config/limit", { method: "POST", body: JSON.stringify(body) });
  }
  /// 热改总阀门：null/缺省 = 不调整该方向；0 = 不限；响应 = 生效值。
  /// 注意：运行时阀门，不落盘——重启回到设置页的持久化基准。请求体走
  /// 生成的 GlobalLimitReq（字段名漂移编译期报错）。
  setGlobalLimits(limits: GlobalLimitReq): Promise<GlobalLimits> {
    const body: GlobalLimitReq = {
      max_download_kb_s: limits.max_download_kb_s ?? null,
      max_upload_kb_s: limits.max_upload_kb_s ?? null,
    };
    return req("/config/limit", { method: "POST", body: JSON.stringify(body) });
  }
  /// Provider 运行态快照（健康/配额/冷却）。
  providers(): Promise<ProviderStatusDto[]> {
    return req("/providers");
  }

  stats(): Promise<Stats> {
    return req("/stats");
  }

  // S1 设置面
  settings(): Promise<Settings> {
    return req("/settings");
  }
  /// 请求体走生成的 SettingsReq（#161：PUT /settings 全 Option 静默忽略
  /// 未知键——UI 拼错字段名 = 空操作，字段名漂移必须在编译期报错）。
  putSettings(patch: SettingsReq, persist = true): Promise<SettingsApplyReport> {
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

export const client = new DaemonClient();
