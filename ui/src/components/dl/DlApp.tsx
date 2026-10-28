"use client";
/// 应用壳（2026-10-19 布局重设计）：侧栏(216px 可折叠) + 右列
/// [视图工具栏(48px) + 内容(flex:1) + 状态栏(28px)]。
/// 信息架构：导航归侧栏（结构性），动作归工具栏（上下文性）。
/// - 数据轮询：列表 + stats（1s）+ 任务详情快照（1s 并发，表格进度/ETA 数据源）
///   #161 P3：列表过滤/搜索改服务端执行（?state=&search=，防抖 300ms），
///   侧栏分段计数与状态栏计数改由 /stats 全量聚合派生
/// - SSE 事件流：速率 / 完成 / 失败 toast
/// - 快捷键：Ctrl/Cmd+1..5 切视图、/ 聚焦搜索（Slack/GitHub 惯例）
/// - 侧栏折叠：localStorage["dl.side"] 持久化；≤900px 初始自动图标栏
/// 性能修复（审计 P1）：JSON.stringify 全量比较 → 浅层引用比较（长度 + 逐项
/// 引用相等即可判断「新数据」——fetch 每次返回新对象，引用比较足够触发更新，
/// 无需深比较语义；列表项字段级比较留给 React 渲染 diff）。
/// 交互打磨批：
/// - E8：loaded 标志区分「加载中」与「暂无任务」（首帧数据未到显示骨架）
/// - 删除（单行/批量）走 ConfirmDialog 确认（危险操作不可裸触发）
/// - 表头排序状态在此持有（排序后传表格渲染；Shift 范围选择按显示序）
/// - toast 支持手动关闭；新建条 Esc 收起

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  ListTasksQuery,
  SchedulerEventEnvelope,
  Stats,
  TaskListItem,
  TaskRate,
  TaskSnapshot,
} from "@/lib/daemon/types";
import { client, getBase, getToken, HttpError } from "@/lib/daemon/store";
import TasksTable, {
  displayName,
  type SortKey,
  type TaskAction,
  type TableSort,
} from "./TasksTable";
import StatsView from "./StatsView";
import LogsView from "./LogsView";
import ProvidersView from "./ProvidersView";
import RssView from "./RssView";
import SettingsView from "./SettingsView";
import SecurityView from "./SecurityView";
import TaskDetail from "./TaskDetail";
import NewTaskForm from "./NewTaskForm";
import Toolbar, { TASK_FILTERS, type TaskFilterKey, type ViewKey } from "./Toolbar";
import Sidebar from "./Sidebar";
import StatusBar from "./StatusBar";
import { ConfirmDialog, DaemonDown, TableSkeleton } from "./Common";
import { Icon } from "./Icon";

type SpeedPoint = { down: number; up: number };

/// 服务端列表过滤（#161 P3）的分段 → 状态标签映射：过滤/搜索改在
/// daemon 侧执行（GET /tasks?state=&search=），浏览器不再每秒拉全量后
/// 自行过滤。标签与 state.rs known_state_labels / state_label 口径一致
/// （即 TaskState 变体名）；漂移会在请求期 400 暴露（非静默）。
/// 注：Seeding 同时属「进行中」（上传中）与「已完成」（下完整做种）两段
/// —— any-of 语义下两段均含，与旧客户端过滤口径一致。
const SEGMENT_STATES: Record<Exclude<TaskFilterKey, "all">, string> = {
  active: "Downloading,Seeding,Transferring,FallbackProvider",
  queued: "Queued,Evaluating",
  paused: "Paused",
  done: "Completed,Stopped,Seeding",
  failed: "Failed",
};

/// SSE 速率新鲜度 TTL（与 TasksTable.RATE_TTL_MS 同口径；DlApp 侧的
/// 过期清理 interval 消费）。
const RATE_TTL_MS = 5000;

/// R15 视图状态持久化（qB 式「记住我的列表」）：sort / filter / view
/// 三态存 localStorage；搜索框不持久化（陈旧搜索会在重载后造成「列表
/// 为何是空的」困惑——qB 同款决策）。恢复值全部校验（旧版本/损坏/
/// 手改的键值 → 回落默认），typeof window 守卫防 SSG 预渲染崩溃；
/// 恢复走挂载后 useEffect 而非 lazy initializer——避免服务端渲染
/// 默认态与客户端恢复态的 hydration 不匹配（与 dl.side 同模式）。
const VIEW_KEYS: ViewKey[] = ["tasks", "stats", "rss", "logs", "plugins", "security", "settings"];
const SORT_KEYS = ["name", "state", "progress", "speed", "size", "eta"] as const;

function restoreView(): ViewKey {
  const raw = localStorage.getItem("dl.view");
  return VIEW_KEYS.includes(raw as ViewKey) ? (raw as ViewKey) : "tasks";
}

function restoreFilter(): TaskFilterKey {
  const raw = localStorage.getItem("dl.filter");
  const keys = TASK_FILTERS.map((f) => f.key) as readonly string[];
  return keys.includes(raw ?? "") ? (raw as TaskFilterKey) : "all";
}

function restoreSort(): TableSort {
  const raw = localStorage.getItem("dl.sort");
  if (!raw) return null;
  try {
    const v = JSON.parse(raw) as { key?: unknown; dir?: unknown };
    if (
      (SORT_KEYS as readonly string[]).includes(v.key as string) &&
      (v.dir === "asc" || v.dir === "desc")
    ) {
      return { key: v.key as SortKey, dir: v.dir };
    }
  } catch {
    /* 损坏 JSON → 默认 */
  }
  return null;
}

/// 活跃（非终态）任务状态集（P1-5-C：全终态时冻结快照轮询的判定口径；
/// 与 Common.tsx StateBadge 的 live 集一致 + Queued/Evaluating/Paused
/// ——Paused 仍是可恢复的进行中语义）。
const ACTIVE_STATES = [
  "Downloading",
  "Seeding",
  "Transferring",
  "FallbackProvider",
  "Evaluating",
  "Queued",
  "Paused",
];

export default function DlApp() {
  const [view, setView] = useState<ViewKey>("tasks");
  /// 侧栏折叠（2026-10-19：localStorage 持久化；未存过且窄屏 → 图标栏）。
  const [sideCollapsed, setSideCollapsed] = useState(false);
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const [tasks, setTasks] = useState<TaskListItem[]>([]);
  const [snapshots, setSnapshots] = useState<Record<string, TaskSnapshot>>({});
  /// 快照最新引用（refreshSnapshots 引用稳定化用；避免 state 依赖重建回调）
  const snapshotsRef = useRef<Record<string, TaskSnapshot>>({});
  snapshotsRef.current = snapshots;
  const [rates, setRates] = useState<Record<string, TaskRate>>({});
  const [stats, setStats] = useState<Stats | null>(null);
  const [events, setEvents] = useState<SchedulerEventEnvelope[]>([]);
  const [connected, setConnected] = useState(false);
  const [detail, setDetail] = useState<string | null>(null);
  /// R17 焦点归还（WAI-ARIA 对话框模式）：开抽屉前的触发元素（行），
  /// 关闭后归还焦点（键盘用户不被扔回 body；虚拟滚动已回收行则放弃）。
  const detailTriggerRef = useRef<HTMLElement | null>(null);
  const openDetail = useCallback((id: string) => {
    detailTriggerRef.current =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setDetail(id);
  }, []);
  useEffect(() => {
    if (detail !== null) return;
    const el = detailTriggerRef.current;
    if (!el) return;
    detailTriggerRef.current = null;
    if (el.isConnected) el.focus();
  }, [detail]);
  const [toasts, setToasts] = useState<{ id: number; kind: string; text: string }[]>([]);
  const [filter, setFilter] = useState<TaskFilterKey>("all");
  const [query, setQuery] = useState("");
  /// 搜索防抖（#161 P3 服务端过滤配套）：输入停顿 300ms 才发起请求，
  /// 避免逐键打点 daemon。
  const [debouncedQuery, setDebouncedQuery] = useState("");
  useEffect(() => {
    const h = setTimeout(() => setDebouncedQuery(query.trim()), 300);
    return () => clearTimeout(h);
  }, [query]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  /// R24：挂载后捕获连接目标（localStorage 仅客户端可读；面板晚于首拍
  /// 渲染，彼时必就绪——首拍骨架与 SSR HTML 一致，无 hydration 风险）。
  useEffect(() => {
    setDd({ base: getBase(), hasToken: !!getToken() });
  }, []);
  const [adding, setAdding] = useState(false);
  /// E8：首帧数据是否已到（区分「加载中」骨架与「暂无任务」空态）。
  const [loaded, setLoaded] = useState(false);
  /// R24：首轮轮询是否已决（区分「首次请求在途」与「daemon 不可达」）。
  /// 门控 DaemonDown 面板：正常加载首拍（请求在飞、connected 仍 false）
  /// 必须显示骨架而非面板——否则每次健康加载都会闪「无法连接」。
  const [polled, setPolled] = useState(false);
  /// R25：全败且拒绝原因是 HTTP 401/403——daemon 可达但拒绝（令牌错/
  /// 白名单未放行），面板切换认证失败变体（文案/指引与网络不可达分道）。
  const [authFailed, setAuthFailed] = useState(false);
  /// R24：面板要展示的连接目标（localStorage 仅客户端可读，挂载后捕获；
  /// 面板只在轮询决出后渲染，彼时已就绪）。
  const [dd, setDd] = useState<{ base: string; hasToken: boolean } | null>(null);
  /// 表头排序（null = 后端默认序）。
  const [sort, setSort] = useState<TableSort>(null);
  /// 危险操作确认（删除任务/批量删除）。
  const [confirmState, setConfirmState] = useState<{
    title: string;
    body: string;
    confirmText: string;
    danger: boolean;
    run: () => Promise<void>;
  } | null>(null);
  /// 速率曲线历史（不可变更新：新数组引用驱动 SpeedChart useMemo 重算）。
  /// 修复：旧实现 historyRef 原地 push + historyTick 驱动重渲染——但
  /// StatsView 拿到的 history 引用永不变，SpeedChart 的 useMemo([data])
  /// 引用相等短路，曲线在挂载后永久冻结（切走再切回才重算）。
  /// 无变化返旧引用：速率未变时跳过重渲染（保留原优化意图）。
  const [history, setHistory] = useState<SpeedPoint[]>([]);

  const toast = useCallback((t: { kind: string; text: string }) => {
    const id = Date.now() + Math.random();
    // R16：并发上限 3（最旧挤掉）——批量完成风暴不把 UI 埋进 toast 堆
    // （120 任务导入后重载曾被 120 个「任务完成」toast 连环遮挡按钮）。
    setToasts((cur) => [...cur, { id, ...t }].slice(-3));
    setTimeout(() => setToasts((cur) => cur.filter((x) => x.id !== id)), 4200);
  }, []);

  /// 侧栏初始折叠态：用户持久化选择优先；未存过 → ≤900px 视口自动图标栏
  /// （一次性判断，不在 resize 时反复切换——不打架用户选择；VS Code 同样策略）。
  const toggleSide = useCallback(() => {
    setSideCollapsed((cur) => {
      localStorage.setItem("dl.side", cur ? "0" : "1");
      return !cur;
    });
  }, []);

  useEffect(() => {
    const saved = localStorage.getItem("dl.side");
    if (saved === "1" || saved === "0") setSideCollapsed(saved === "1");
    else if (window.innerWidth <= 900) setSideCollapsed(true);
  }, []);

  /// R15：视图状态恢复（挂载后一次；校验过的合法值才生效）。首拍列表
  /// 请求以默认序发出，恢复的 sort 生效后 listParams 变化 → 即时重拉
  /// 带排序的新序（两拍收敛，无需专门等待）。
  useEffect(() => {
    setView(restoreView());
    setFilter(restoreFilter());
    setSort(restoreSort());
  }, []);

  /// R15：变更即持久化（sort=null 清键——回默认序后重载不再恢复旧序）。
  useEffect(() => {
    localStorage.setItem("dl.view", view);
  }, [view]);
  useEffect(() => {
    localStorage.setItem("dl.filter", filter);
  }, [filter]);
  useEffect(() => {
    if (sort) localStorage.setItem("dl.sort", JSON.stringify(sort));
    else localStorage.removeItem("dl.sort");
  }, [sort]);

  /// 全局快捷键（Slack/GitHub 惯例）：Ctrl/Cmd+1..6 切视图、/ 聚焦搜索。
  /// 输入控件内不拦截（除 Ctrl 组合——数字组合在输入框内无编辑语义冲突）。
  useEffect(() => {
    const ORDER: ViewKey[] = ["tasks", "stats", "rss", "logs", "plugins", "security", "settings"];
    const isTyping = (t: EventTarget | null) =>
      t instanceof HTMLElement &&
      (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable);
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key >= "1" && e.key <= "6") {
        e.preventDefault();
        setView(ORDER[Number(e.key) - 1]);
        return;
      }
      if (e.key === "/" && !isTyping(e.target)) {
        e.preventDefault();
        searchInputRef.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  /// 当前视图的列表请求参数（#161 P3）：null = 未过滤全量视图（同旧
  /// 行为）；过滤/搜索由 daemon 执行，浏览器只收所见集。参数变化 →
  /// refresh 重建 → 轮询 effect 重跑（即时拉新视图，不等下一拍）。
  const listParams = useMemo<ListTasksQuery | null>(() => {
    const state = filter !== "all" ? SEGMENT_STATES[filter] : undefined;
    const search = debouncedQuery || undefined;
    // R14：排序下推服务端（SortKey 与服务端 sort 键一一对应；
    // null = 默认创建序）。服务端为排序唯一权威——表头切换 →
    // listParams 重建 → refresh 重建 → 轮询 effect 即时重拉新序。
    const sortQ =
      sort !== null ? { sort: sort.key, order: sort.dir } : undefined;
    return state === undefined && search === undefined && sortQ === undefined
      ? null
      : { state, search, ...sortQ };
  }, [filter, debouncedQuery, sort]);
  const filtered = listParams !== null;

  const refresh = useCallback(async () => {
    // 审计修复（40-e P1-4）：失败保留旧数据——双请求都失败才判断连。
    // R25：allSettled 捕获拒绝原因——全败时区分「认证失败」（HttpError
    // 401/403：daemon 活着、令牌/白名单问题）与「网络不可达」（fetch
    // TypeError：拒绝/超时），面板给出完全不同的文案与排障指引。
    const [tRes, sRes] = await Promise.allSettled([
      client.listTasks(listParams ?? undefined),
      client.stats(),
    ]);
    const t = tRes.status === "fulfilled" ? tRes.value : null;
    const s = sRes.status === "fulfilled" ? sRes.value : null;
    if (t !== null) {
      setLoaded(true);
      // P1-5-A 修复（Issue #127）：内容签名去重 + 项级引用稳定。
      // 旧实现每秒无条件 setTasks(新数组)：1000+ 任务时整树每秒重渲染，
      // onClick 被排队到下一空闲帧 → 点击滞后 100-300ms。签名覆盖
      // TaskListItem 全部字段（id/state/source/engine/name），内容不变
      // 则跳过 set；变更时内容未变的项沿用旧对象引用（行级 memo 生效）。
      let sig = "";
      const prev = new Map(tasksRef.current.map((x) => [x.task_id, x]));
      const reused = t.map((x) => {
        // A2：签名纳入限定字段（start_at/next_retry/retries）——定时到期激活、
        // 重试计数变化都会驱动徽章刷新；tags 同步入签（列表尚未展示，防未来
        // 遗漏）。内容不变则跳过 set；变更时内容未变的项沿用旧对象引用。
        sig += `${x.task_id}\u0001${x.state}\u0001${x.source}\u0001${x.engine ?? ""}\u0001${x.name ?? ""}\u0001${x.start_at_unix ?? 0}\u0001${x.next_retry_at_unix ?? 0}\u0001${x.retries ?? 0}\u0001${(x.tags ?? []).join("\u0003")}\u0002`;
        const p = prev.get(x.task_id);
        return p &&
          p.state === x.state &&
          p.name === x.name &&
          p.source === x.source &&
          p.engine === x.engine &&
          p.start_at_unix === x.start_at_unix &&
          p.next_retry_at_unix === x.next_retry_at_unix &&
          p.retries === x.retries
          ? p
          : x;
      });
      if (sig !== taskSigRef.current) {
        taskSigRef.current = sig;
        setTasks(reused);
      }
    }
    if (s) {
      setStats(s);
      // 不可变更新：新点 → 新数组引用（SpeedChart memo 重算）；
      // 速率未变 → 返回旧引用（跳过重渲染）。上限 180 点滑动窗口。
      setHistory((h) => {
        const last = h[h.length - 1];
        if (last && last.down === s.down_bytes_s && last.up === s.up_bytes_s) {
          return h;
        }
        const next = [...h, { down: s.down_bytes_s, up: s.up_bytes_s }];
        return next.length > 180 ? next.slice(next.length - 180) : next;
      });
    }
    setConnected(t !== null || s !== null);
    // R25：全败时分类——任一拒绝原因是 HttpError 401/403 → 认证/白名单
    // 面板（daemon 可达）；否则网络不可达面板。恢复同拍自动（1s 轮询）。
    if (t === null && s === null) {
      const reasons = [tRes, sRes]
        .filter((r): r is PromiseRejectedResult => r.status === "rejected")
        .map((r) => r.reason);
      setAuthFailed(
        reasons.some(
          (e) => e instanceof HttpError && (e.status === 401 || e.status === 403),
        ),
      );
    } else {
      setAuthFailed(false);
    }
    // R25：首轮决出标记（DaemonDown 门控——首拍在飞不算不可达）。
    setPolled(true);
  }, [listParams]);

  /// 详情快照轮询：仅对当前列表中的任务并发拉取（表格进度/大小/ETA 数据源）。
  /// 节拍 2s 独立于列表轮询（1s）：任务量大时减半请求压力；tasks 最新引用
  /// 经 ref 传递（避免每秒新数组引用重置 interval 计时）。
  /// P1-5-C 修复（Issue #127）：全终态冻结——无进行中任务且任务集合未变时
  /// 停拉（终值的快照已入账，继续拉只产生每 2s 的解析+重渲染压力）；
  /// 出现新任务/活跃任务即自动恢复。快照项引用稳定：内容未变沿用旧对象
  /// （配合行级 memo，速率列不变时不重渲染行）。
  const tasksRef = useRef<TaskListItem[]>([]);
  tasksRef.current = tasks;
  const taskSigRef = useRef<string>("");
  const snapIdsRef = useRef<string>("");
  const refreshSnapshots = useCallback(async () => {
    const list = tasksRef.current;
    if (list.length === 0) {
      setSnapshots((cur) => (Object.keys(cur).length === 0 ? cur : {}));
      snapIdsRef.current = "";
      return;
    }
    const hasActive = list.some((t) => ACTIVE_STATES.includes(t.state));
    const idsSig = list.map((x) => x.task_id).join(",");
    if (!hasActive && idsSig === snapIdsRef.current) {
      return; // 全终态 + 集合未变 → 冻结（终值已入账）
    }
    snapIdsRef.current = idsSig;
    const prev = snapshotsRef.current;
    const next: Record<string, TaskSnapshot> = {};
    await Promise.all(
      list.map(async (t) => {
        try {
          const fresh = await client.getTask(t.task_id);
          // 引用稳定：表格消费的字段（done/total/state/error/name/limits/
          // rates/累计统计）未变 → 沿用旧快照对象（行级 memo 浅比较生效）。
          // 审计修复（2026-10-21，PR #146）+ #147 §一勘误：速率数据是双路
          // —— SSE Speed 事件为新鲜时主源（#117 起有生产者：BT 做种分支
          // 只发 Speed、HTTP Downloading 分支发 Speed+Progress；setRates
          // 每事件新建对象，不受引用复用影响）；本快照轮询是 SSE 过期/
          // 断连后的回落源，且是 TaskDetail 累计上传统计的唯一源。旧比较
          // 漏 rates/limits/累计字段 → 快照侧新值被引用复用静默丢弃（回落
          // 源冻结 + 累计统计冻结）。做种速率列不可见的另一半根因（渲染
          // 门 down>0）见 TasksTable 速率单元格注释，随 #147 一并修复。
          const sameRates = (a?: { down_bytes_s: number; up_bytes_s: number } | null, b?: { down_bytes_s: number; up_bytes_s: number } | null) =>
            (a?.down_bytes_s ?? 0) === (b?.down_bytes_s ?? 0) &&
            (a?.up_bytes_s ?? 0) === (b?.up_bytes_s ?? 0);
          const p = prev[t.task_id];
          next[t.task_id] =
            p &&
            p.done === fresh.done &&
            p.total === fresh.total &&
            p.state === fresh.state &&
            p.error === fresh.error &&
            p.name === fresh.name &&
            sameRates(p.rates, fresh.rates) &&
            (p.limits?.down_kb_s ?? 0) === (fresh.limits?.down_kb_s ?? 0) &&
            (p.limits?.up_kb_s ?? 0) === (fresh.limits?.up_kb_s ?? 0) &&
            p.total_downloaded === fresh.total_downloaded &&
            p.total_uploaded === fresh.total_uploaded
              ? p
              : fresh;
        } catch {
          /* 任务可能已移除；保留缺失（表格显示 —） */
        }
      }),
    );
    if (Object.keys(next).length > 0) setSnapshots(next);
  }, []);

  /// 列表轮询（1s）：refresh 依赖 listParams——过滤/搜索切换即重拉新视图
  ///（重置节拍，无额外等待）。
  useEffect(() => {
    refresh();
    const iv = setInterval(refresh, 1000);
    return () => clearInterval(iv);
  }, [refresh]);

  /// SSE 事件流（速率/完成/失败 toast）：与轮询分离——过滤/搜索切换
  /// 只重置轮播节拍，事件流不重连。
  /// R16 live 边界：先 GET /events 取 latest_seq + 最近 200 条日志种子，
  /// 再以 after=latest_seq 订阅增量——重载不再全量重放（历史完成事件
  /// 对用户是历史而非现场，不该弹「任务完成」toast；曾致批量导入后
  /// 重载被 toast 洪泛遮挡按钮）。REST 失败 → 保持 after=0 全量重放
  /// （降级 = 旧行为，toast 照常）。
  useEffect(() => {
    const onEnv = (env: SchedulerEventEnvelope) => {
      setEvents((cur) => {
        const next = [...cur, env];
        return next.length > 500 ? next.slice(next.length - 500) : next;
      });
      const type = env.event.type;
      if (type === "speed") {
        const tid = String(env.event.task_id ?? "");
        if (tid) {
          // ts-rs 精确联合（#147 §六）：speed 变体字段为必填 number；
          // 取值须在 setRates 回调外完成——属性路径 narrowing 不进闭包。
          const { down_rate, up_rate } = env.event;
          setRates((cur) => ({
            ...cur,
            [tid]: {
              down: Number(down_rate ?? 0),
              up: Number(up_rate ?? 0),
              ts: Date.now(),
            },
          }));
        }
      }
      if (type === "completed") toast({ kind: "success", text: "任务完成" });
      if (type === "failed") toast({ kind: "error", text: "任务失败" });
      // 审计修复（40-e P2-22）：终态事件清理速率表条目
      // （事件类型全集见 events.rs type_label——无 removed 变体，
      // 删除场景由渲染侧 5s TTL 衰减兜底）
      if (type === "completed" || type === "failed") {
        const tid = String(env.event.task_id ?? "");
        if (tid) {
          setRates((cur) => {
            if (!(tid in cur)) return cur;
            const next = { ...cur };
            delete next[tid];
            return next;
          });
        }
      }
    };
    // 先降级订阅（全量重放）；REST 引导成功后热切换为 live 增量流。
    // disposed 守卫：卸载后迟到的 REST 解析不得再换流（EventSource 泄漏）。
    let disposed = false;
    let unsub = client.subscribeEvents(onEnv, setConnected);
    client
      .listEvents({ limit: 1 })
      .then((page) => {
        const latest = page.latest_seq ?? 0;
        return client
          .listEvents({ after: Math.max(0, latest - 200), limit: 200 })
          .then((seed) => {
            if (disposed) return;
            setEvents(seed.events.slice(-500));
            unsub();
            unsub = client.subscribeEvents(onEnv, setConnected, latest);
          });
      })
      .catch(() => {
        /* REST 不可达：保持降级全量重放流（与旧行为一致） */
      });
    return () => {
      disposed = true;
      unsub();
    };
  }, [toast]);

  /// 快照轮询独立节拍（2s）：挂载一次，读 ref 最新列表。
  useEffect(() => {
    refreshSnapshots();
    const iv = setInterval(refreshSnapshots, 2000);
    return () => clearInterval(iv);
  }, [refreshSnapshots]);

  /// P1-5（Issue #127）速率 TTL 过期清理：旧实现靠每秒无条件 setTasks
  /// 触发整树重算来「刷新」SSE 速率的新鲜度判定；签名去重后静默期无重渲染，
  /// 过期清理必须自驱动（仅在实际有过期项时 setState——频率 ≤ 1 次/秒）。
  useEffect(() => {
    const iv = setInterval(() => {
      setRates((cur) => {
        const now = Date.now();
        let changed = false;
        const next: Record<string, TaskRate> = {};
        for (const k of Object.keys(cur)) {
          const v = cur[k];
          if (now - v.ts < RATE_TTL_MS) next[k] = v;
          else changed = true;
        }
        return changed ? next : cur;
      });
    }, 1000);
    return () => clearInterval(iv);
  }, []);

  /// 选集随任务列表收缩（已移除任务的选中项清除）。#161 P3 起列表可能是
  /// 过滤子集——无法区分「不在视图」与「已删除」，不能据此外推；仅在
  /// 未过滤视图修剪，显式删除路径（onAction/onBatch 的 remove）单点反选
  /// 兜底，跨视图残留选中由批量 API 逐项结果容错。
  useEffect(() => {
    if (filtered) return;
    setSelected((cur) => {
      const alive = new Set(tasks.map((t) => t.task_id));
      const next = new Set([...cur].filter((id) => alive.has(id)));
      return next.size === cur.size ? cur : next;
    });
  }, [tasks, filtered]);

  /// 过滤分段计数（侧栏导航项徽章）：#161 P3 起列表请求已被过滤，不能
  /// 再当计数源——改由 /stats 全量聚合（by_state 键同 TaskListItem.state
  /// 口径）派生；stats 与列表同拍轮询，新鲜度等同。Seeding 同属两段的
  /// 口径与 SEGMENT_STATES 一致。
  const counts = useMemo(() => {
    const bs = stats?.by_state ?? {};
    const seg = (ks: string[]) => ks.reduce((a, k) => a + (bs[k] ?? 0), 0);
    return {
      all: stats?.total ?? 0,
      active: seg(["Downloading", "Seeding", "Transferring", "FallbackProvider"]),
      queued: seg(["Queued", "Evaluating"]),
      paused: bs["Paused"] ?? 0,
      done: seg(["Completed", "Stopped", "Seeding"]),
      failed: bs["Failed"] ?? 0,
    } as Record<TaskFilterKey, number>;
  }, [stats]);

  /// 显示集 = 服务端过滤 + 排序后的列表（#161 P3 过滤下推 + R14 排序下推：
  /// 两者均由 daemon 执行，客户端不再二次过滤/二次排序——避免双重口径
  /// 遮蔽服务端语义漂移；列表轮询 1s 携带 sort/order 重拉，序随引擎快照
  /// 缓存更新，新鲜度与旧客户端实时重排等同）。显示序 = 用户所见序，
  /// Shift 范围选择按此序。

  const act = useCallback(
    async (fn: () => Promise<unknown>) => {
      setBusy(true);
      try {
        await fn();
        await refresh();
      } catch (e) {
        toast({ kind: "error", text: `操作失败：${String(e)}` });
      } finally {
        setBusy(false);
      }
    },
    [refresh, toast],
  );

  const onAction = useCallback(
    (id: string, a: TaskAction) => {
      if (a === "remove") {
        const name = displayName(tasks.find((t) => t.task_id === id) ?? { task_id: id });
        setConfirmState({
          title: "删除任务",
          body: `确定删除「${name}」吗？任务记录将从列表移除（已下载文件默认保留）。`,
          confirmText: "删除",
          danger: true,
          /// #161 P3：列表可能为过滤子集，选集修剪不再全能——删除时
          /// 单点反选，避免跨视图残留幽灵选中。
          run: async () => {
            await act(async () => {
              await client.remove(id);
              setSelected((cur) => {
                if (!cur.has(id)) return cur;
                const next = new Set(cur);
                next.delete(id);
                return next;
              });
            });
          },
        });
        return;
      }
      act(() => (a === "resume" ? client.resume(id) : client.pause(id)));
    },
    [act, tasks],
  );

  const onBatch = useCallback(
    (op: "pause" | "resume" | "remove") => {
      const ids = [...selected];
      if (ids.length === 0) return;
      if (op === "remove") {
        setConfirmState({
          title: `批量删除 ${ids.length} 个任务`,
          body: `确定删除选中的 ${ids.length} 个任务吗？任务记录将从列表移除（已下载文件默认保留）。`,
          confirmText: `删除 ${ids.length} 项`,
          danger: true,
          run: async () => {
            await act(async () => {
              await client.batchTasks(ids, op);
              setSelected(new Set());
            });
          },
        });
        return;
      }
      act(async () => {
        await client.batchTasks(ids, op);
        // UX-1 修复（体检 #182）：pause/resume 保留选择集——「批量暂停→
        // 批量恢复」是连续操作，清空选择迫使用户重新全选（qBittorrent 等
        // 惯例均保留）。remove 路径任务已删除，仍清空（上方 run 内）。
      });
    },
    [act, selected],
  );

  /// 新建任务成功回调（Wave B：表单组件自理提交/校验/错误取景，
  /// 这里只负责收尾刷新与 toast——metalink 多任务展示 count）。
  const onTaskAdded = useCallback(
    async ({ count }: { count: number }) => {
      toast({ kind: "success", text: count > 1 ? `已添加 ${count} 个任务（metalink 展开）` : "任务已添加" });
      await refresh();
    },
    [refresh, toast],
  );

  const toggleSelect = useCallback((id: string) => {
    setSelected((cur) => {
      const next = new Set(cur);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const toggleAll = useCallback(
    (on: boolean) => {
      setSelected(on ? new Set(tasks.map((t) => t.task_id)) : new Set());
    },
    [tasks],
  );

  const onSelectRow = useCallback(
    (id: string) => {
      setSelected((cur) => {
        // 单击行 = 唯一选中（Ctrl/Shift 语义在表格行内处理）
        return cur.size === 1 && cur.has(id) ? new Set() : new Set([id]);
      });
    },
    [],
  );

  /// Shift 范围加选（不剔除——精确剔除用 checkbox 单点）。
  const onSelectRange = useCallback((ids: string[]) => {
    setSelected((cur) => {
      const next = new Set(cur);
      let changed = false;
      for (const id of ids) {
        if (!next.has(id)) {
          next.add(id);
          changed = true;
        }
      }
      return changed ? next : cur;
    });
  }, []);

  return (
    <div className="dl-root">
      <Sidebar
        view={view}
        onView={setView}
        filter={filter}
        onFilter={setFilter}
        counts={counts}
        stats={stats}
        connected={connected}
        collapsed={sideCollapsed}
        onToggleCollapsed={toggleSide}
      />

      <div className="dl-shell">
        <Toolbar
          view={view}
          query={query}
          onQuery={setQuery}
          selectedCount={selected.size}
          onBatch={onBatch}
          onNew={() => setAdding((v) => !v)}
          adding={adding}
          busy={busy}
          searchInputRef={searchInputRef}
        />

        <div className="dl-body">
          <main className="dl-main">
          {view === "tasks" && (
            <>
              {/* Wave B（#143 第二梯队）：全字段新建表单（链接/.torrent/metalink
                  + 探测预览 + 高级项）。常驻挂载 hidden 切换——收起不丢已填
                  内容；Esc 不收表单（防误触丢稿），收起走「取消」/工具栏。 */}
              <NewTaskForm open={adding} onOpen={setAdding} onAdded={onTaskAdded} />
              {/* R24/R25：首帧未到时三分支——首轮未决（请求在飞）或连接正常 =
                  骨架（与 SSR HTML 一致，无闪变）；首轮已决且连接失败 =
                  daemon 不可达面板（网络层）或认证失败变体（HTTP 401/403，
                  daemon 活着但拒绝）。connected 翻转零交互自愈（1s 轮询）。 */}
              {!loaded ? (
                polled && !connected ? (
                  <DaemonDown
                    base={dd?.base ?? ""}
                    hasToken={dd?.hasToken ?? false}
                    mode={authFailed ? "auth" : "unreachable"}
                  />
                ) : (
                  <TableSkeleton />
                )
              ) : (
                <TasksTable
                  tasks={tasks}
                  snapshots={snapshots}
                  rates={rates}
                  selected={selected}
                  onToggleSelect={toggleSelect}
                  onToggleAll={toggleAll}
                  onSelect={onSelectRow}
                  onSelectRange={onSelectRange}
                  onOpen={openDetail}
                  onAction={onAction}
                  busy={busy}
                  query={query}
                  sort={sort}
                  onSort={setSort}
                />
              )}
            </>
          )}
          {view === "stats" && <StatsView stats={stats} history={history} connected={connected} />}
          {view === "rss" && <RssView onToast={toast} />}
          {view === "logs" && <LogsView events={events} />}
          {view === "plugins" && <ProvidersView onToast={toast} />}
          {view === "settings" && <SettingsView onToast={toast} />}
          {view === "security" && <SecurityView onToast={toast} />}
          </main>
        </div>

        <StatusBar stats={stats} selectedCount={selected.size} />
      </div>

      {detail && (
        <TaskDetail id={detail} onClose={() => setDetail(null)} onChanged={refresh} />
      )}

      {confirmState && (
        <ConfirmDialog
          title={confirmState.title}
          body={confirmState.body}
          confirmText={confirmState.confirmText}
          danger={confirmState.danger}
          onConfirm={() => {
            const run = confirmState.run;
            setConfirmState(null);
            run().catch((e) => toast({ kind: "error", text: `操作失败：${String(e)}` }));
          }}
          onCancel={() => setConfirmState(null)}
        />
      )}

      <div className="dl-toasts" role="status" aria-live="polite">
        {toasts.map((t) => (
          <div key={t.id} className={`dl-toast dl-toast--${t.kind}`}>
            <span className="dl-toast-text">{t.text}</span>
            <button
              type="button"
              className="dl-toast-close"
              aria-label="关闭提示"
              title="关闭提示"
              onClick={() => setToasts((cur) => cur.filter((x) => x.id !== t.id))}
            >
              <Icon name="close" size={12} />
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}
