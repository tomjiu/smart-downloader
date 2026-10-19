"use client";
/// 应用壳（2026-10-19 布局重设计）：侧栏(216px 可折叠) + 右列
/// [视图工具栏(48px) + 内容(flex:1) + 状态栏(28px)]。
/// 信息架构：导航归侧栏（结构性），动作归工具栏（上下文性）。
/// - 数据轮询：列表 + stats（1s）+ 任务详情快照（1s 并发，表格进度/ETA 数据源）
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
  SchedulerEventEnvelope,
  Stats,
  TaskListItem,
  TaskRate,
  TaskSnapshot,
} from "@/lib/daemon/types";
import { client } from "@/lib/daemon/store";
import TasksTable, {
  displayName,
  taskSortValue,
  type TaskAction,
  type TableSort,
} from "./TasksTable";
import StatsView from "./StatsView";
import LogsView from "./LogsView";
import RssView from "./RssView";
import SettingsView from "./SettingsView";
import TaskDetail from "./TaskDetail";
import Toolbar, { TASK_FILTERS, type TaskFilterKey, type ViewKey } from "./Toolbar";
import Sidebar from "./Sidebar";
import StatusBar from "./StatusBar";
import { ConfirmDialog, TableSkeleton } from "./Common";
import { Icon } from "./Icon";

type SpeedPoint = { down: number; up: number };

function matchFilter(t: TaskListItem, f: TaskFilterKey): boolean {
  switch (f) {
    case "active":
      return ["Downloading", "Seeding", "Transferring", "FallbackProvider"].includes(t.state);
    case "queued":
      return ["Queued", "Evaluating"].includes(t.state);
    case "paused":
      return t.state === "Paused";
    case "done":
      return ["Completed", "Stopped", "Seeding"].includes(t.state);
    case "failed":
      return t.state === "Failed";
    default:
      return true;
  }
}

export default function DlApp() {
  const [view, setView] = useState<ViewKey>("tasks");
  /// 侧栏折叠（2026-10-19：localStorage 持久化；未存过且窄屏 → 图标栏）。
  const [sideCollapsed, setSideCollapsed] = useState(false);
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const [tasks, setTasks] = useState<TaskListItem[]>([]);
  const [snapshots, setSnapshots] = useState<Record<string, TaskSnapshot>>({});
  const [rates, setRates] = useState<Record<string, TaskRate>>({});
  const [stats, setStats] = useState<Stats | null>(null);
  const [events, setEvents] = useState<SchedulerEventEnvelope[]>([]);
  const [connected, setConnected] = useState(false);
  const [detail, setDetail] = useState<string | null>(null);
  const [toasts, setToasts] = useState<{ id: number; kind: string; text: string }[]>([]);
  const [filter, setFilter] = useState<TaskFilterKey>("all");
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [adding, setAdding] = useState(false);
  const [newUrl, setNewUrl] = useState("");
  /// E8：首帧数据是否已到（区分「加载中」骨架与「暂无任务」空态）。
  const [loaded, setLoaded] = useState(false);
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
  const historyRef = useRef<SpeedPoint[]>([]);
  const [historyTick, setHistoryTick] = useState(0);

  const toast = useCallback((t: { kind: string; text: string }) => {
    const id = Date.now() + Math.random();
    setToasts((cur) => [...cur, { id, ...t }]);
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

  /// 全局快捷键（Slack/GitHub 惯例）：Ctrl/Cmd+1..5 切视图、/ 聚焦搜索。
  /// 输入控件内不拦截（除 Ctrl 组合——数字组合在输入框内无编辑语义冲突）。
  useEffect(() => {
    const ORDER: ViewKey[] = ["tasks", "stats", "rss", "logs", "settings"];
    const isTyping = (t: EventTarget | null) =>
      t instanceof HTMLElement &&
      (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable);
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key >= "1" && e.key <= "5") {
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

  const refresh = useCallback(async () => {
    // 审计修复（40-e P1-4）：失败保留旧数据——双请求都失败才判断连。
    const [t, s] = await Promise.all([
      client.listTasks().catch(() => null),
      client.stats().catch(() => null),
    ]);
    if (t !== null) {
      setTasks(t);
      setLoaded(true);
    }
    if (s) {
      setStats(s);
      const h = historyRef.current;
      const last = h[h.length - 1];
      if (!last || last.down !== s.down_bytes_s || last.up !== s.up_bytes_s) {
        h.push({ down: s.down_bytes_s, up: s.up_bytes_s });
        if (h.length > 180) h.splice(0, h.length - 180);
        setHistoryTick((x) => x + 1);
      }
    }
    setConnected(t !== null || s !== null);
  }, []);

  /// 详情快照轮询：仅对当前列表中的任务并发拉取（表格进度/大小/ETA 数据源）。
  /// 节拍 2s 独立于列表轮询（1s）：任务量大时减半请求压力；tasks 最新引用
  /// 经 ref 传递（避免每秒新数组引用重置 interval 计时）。
  const tasksRef = useRef<TaskListItem[]>([]);
  tasksRef.current = tasks;
  const refreshSnapshots = useCallback(async () => {
    const list = tasksRef.current;
    if (list.length === 0) {
      setSnapshots((cur) => (Object.keys(cur).length === 0 ? cur : {}));
      return;
    }
    const next: Record<string, TaskSnapshot> = {};
    await Promise.all(
      list.map(async (t) => {
        try {
          next[t.task_id] = await client.getTask(t.task_id);
        } catch {
          /* 任务可能已移除；保留缺失（表格显示 —） */
        }
      }),
    );
    if (Object.keys(next).length > 0) setSnapshots(next);
  }, []);

  useEffect(() => {
    refresh();
    const iv = setInterval(refresh, 1000);
    const unsub = client.subscribeEvents(
      (env) => {
        setEvents((cur) => {
          const next = [...cur, env];
          return next.length > 500 ? next.slice(next.length - 500) : next;
        });
        const type = env.event.type;
        if (type === "speed") {
          const tid = String(env.event.task_id ?? "");
          if (tid) {
            setRates((cur) => ({
              ...cur,
              [tid]: {
                down: Number(env.event.down_rate ?? 0),
                up: Number(env.event.up_rate ?? 0),
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
      },
      setConnected,
    );
    return () => {
      clearInterval(iv);
      unsub();
    };
  }, [refresh, toast]);

  /// 快照轮询独立节拍（2s）：挂载一次，读 ref 最新列表。
  useEffect(() => {
    refreshSnapshots();
    const iv = setInterval(refreshSnapshots, 2000);
    return () => clearInterval(iv);
  }, [refreshSnapshots]);

  /// 选集随任务列表收缩（已移除任务的选中项清除）。
  useEffect(() => {
    setSelected((cur) => {
      const alive = new Set(tasks.map((t) => t.task_id));
      const next = new Set([...cur].filter((id) => alive.has(id)));
      return next.size === cur.size ? cur : next;
    });
  }, [tasks]);

  /// 过滤分段计数（侧栏导航项徽章）。
  const counts = useMemo(() => {
    const c = Object.fromEntries(TASK_FILTERS.map((f) => [f.key, 0])) as Record<TaskFilterKey, number>;
    for (const t of tasks) {
      for (const f of TASK_FILTERS) {
        if (matchFilter(t, f.key)) c[f.key] += 1;
      }
    }
    return c;
  }, [tasks]);

  const shown = useMemo(
    () =>
      tasks.filter(
        (t) =>
          matchFilter(t, filter) &&
          (!query ||
            displayName(t).toLowerCase().includes(query.toLowerCase()) ||
            t.source.toLowerCase().includes(query.toLowerCase())),
      ),
    [tasks, filter, query],
  );

  /// 排序在过滤/搜索之后应用（显示序 = 用户所见序；Shift 范围选择按此序）。
  const sorted = useMemo(() => {
    if (!sort) return shown;
    const dir = sort.dir === "asc" ? 1 : -1;
    return [...shown].sort((a, b) => {
      const va = taskSortValue(a, snapshots[a.task_id], rates[a.task_id], sort.key);
      const vb = taskSortValue(b, snapshots[b.task_id], rates[b.task_id], sort.key);
      if (typeof va === "string" || typeof vb === "string") {
        return String(va).localeCompare(String(vb), "zh-Hans-CN") * dir;
      }
      return (va - vb) * dir;
    });
  }, [shown, sort, snapshots, rates]);

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
          run: () => act(() => client.remove(id)),
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
        setSelected(new Set());
      });
    },
    [act, selected],
  );

  const add = async () => {
    if (busy || !newUrl.trim()) return;
    setBusy(true);
    try {
      await client.addTask({ url: newUrl.trim() });
      setNewUrl("");
      setAdding(false);
      await refresh();
    } catch (e) {
      toast({ kind: "error", text: `添加失败：${String(e)}` });
    } finally {
      setBusy(false);
    }
  };

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
      setSelected(on ? new Set(sorted.map((t) => t.task_id)) : new Set());
    },
    [sorted],
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
              {adding && (
                <div className="dl-card dl-add-bar" id="dl-add-bar">
                  <input
                    className="dl-input dl-add-input"
                    placeholder="http(s):// · ftp(s):// · sftp:// · magnet:?xt=… · .m3u8 · .meta4"
                    aria-label="下载链接"
                    value={newUrl}
                    autoFocus
                    onChange={(e) => setNewUrl(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") add();
                      if (e.key === "Escape") {
                        e.stopPropagation();
                        setAdding(false);
                      }
                    }}
                  />
                  <button className="dl-btn dl-btn--primary" disabled={busy || !newUrl.trim()} onClick={add}>
                    添加
                  </button>
                  {/* Batch A 槽位：torrent 文件 / metalink / headers / sha256
                      （POST /tasks 全字段）—— 后续批次填 */}
                </div>
              )}
              {!loaded ? (
                <TableSkeleton />
              ) : (
                <TasksTable
                  tasks={sorted}
                  snapshots={snapshots}
                  rates={rates}
                  selected={selected}
                  onToggleSelect={toggleSelect}
                  onToggleAll={toggleAll}
                  onSelect={onSelectRow}
                  onSelectRange={onSelectRange}
                  onOpen={setDetail}
                  onAction={onAction}
                  busy={busy}
                  query={query}
                  sort={sort}
                  onSort={setSort}
                />
              )}
            </>
          )}
          {view === "stats" && (
            <StatsView stats={stats} history={historyRef.current} tasks={tasks} />
          )}
          {view === "rss" && <RssView onToast={toast} />}
          {view === "logs" && <LogsView events={events} />}
          {view === "settings" && <SettingsView onToast={toast} />}
          {/* historyTick 驱动曲线重绘 */}
          <span hidden>{historyTick}</span>
          </main>
        </div>

        <StatusBar tasks={tasks} stats={stats} selectedCount={selected.size} />
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
